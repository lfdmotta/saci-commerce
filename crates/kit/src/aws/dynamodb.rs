//! [`DocumentStore`] sobre uma tabela DynamoDB do módulo `dynamodb-table` da
//! plataforma (chave `pk`/`sk` texto, TTL em `expires_at`, Streams opcional).
//!
//! Atributos de um item:
//!
//! | Atributo | Tipo | Conteúdo |
//! | --- | --- | --- |
//! | `pk`, `sk` | S | Chave |
//! | `doc` | S | O documento JSON |
//! | `v` | N | Versão (concorrência otimista) |
//! | `expires_at` | N | TTL, quando há |
//!
//! Eventos de *outbox* viram itens `pk = OUTBOX#<evt>`, `sk = EVENT`, com o
//! envelope em `outbox_event` e TTL de 24 h: exatamente o formato que
//! `saci_outbox::relay` lê do stream da tabela.

use std::collections::HashMap;

pub use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::error::SdkError;
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::types::{AttributeValue, ConditionCheck, Delete, Put, TransactWriteItem};

use crate::BoxFut;
use crate::store::{DocumentStore, Expect, Item, Page, Query, StoreError, WriteOp};

type Attrs = HashMap<String, AttributeValue>;

/// Tabela de um serviço.
#[derive(Debug, Clone)]
pub struct DynamoStore {
    client: Client,
    table: String,
}

impl DynamoStore {
    /// Cria o adaptador.
    #[must_use]
    pub fn new(client: Client, table: impl Into<String>) -> Self {
        Self {
            client,
            table: table.into(),
        }
    }

    /// Lê o nome da tabela de uma variável de ambiente (`TABLE`) e monta o adaptador.
    pub fn from_env(config: &aws_config::SdkConfig, var: &str) -> Result<Self, String> {
        Ok(Self::new(Client::new(config), crate::env(var)?))
    }
}

fn s(v: impl Into<String>) -> AttributeValue {
    AttributeValue::S(v.into())
}

fn n(v: u64) -> AttributeValue {
    AttributeValue::N(v.to_string())
}

fn key(pk: &str, sk: &str) -> Attrs {
    HashMap::from([("pk".into(), s(pk)), ("sk".into(), s(sk))])
}

fn backend(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

fn to_item(attrs: &Attrs) -> Result<Item, StoreError> {
    let text = |name: &str| attrs.get(name).and_then(|v| v.as_s().ok()).cloned();
    let num = |name: &str| {
        attrs
            .get(name)
            .and_then(|v| v.as_n().ok())
            .and_then(|v| v.parse().ok())
    };
    let doc = text("doc").unwrap_or_else(|| "null".into());
    Ok(Item {
        pk: text("pk").ok_or_else(|| StoreError::Serialization("item sem pk".into()))?,
        sk: text("sk").unwrap_or_default(),
        doc: serde_json::from_str(&doc).map_err(|e| StoreError::Serialization(e.to_string()))?,
        version: num("v").unwrap_or(1),
        expires_at: num("expires_at"),
    })
}

fn to_attrs(item: &Item) -> Attrs {
    let mut attrs = key(&item.pk, &item.sk);
    attrs.insert("doc".into(), s(item.doc.to_string()));
    attrs.insert("v".into(), n(item.version));
    if let Some(t) = item.expires_at {
        attrs.insert("expires_at".into(), n(t));
    }
    attrs
}

/// Expressão de condição e valores para um [`Expect`]. O DynamoDB não tem TTL
/// imediato: item vencido conta como ausente, como no adaptador em memória.
fn condition(expect: Expect, now: u64) -> Option<(String, Attrs)> {
    let now_attr = || HashMap::from([(":now".to_owned(), n(now))]);
    match expect {
        Expect::Any => None,
        Expect::Absent => Some((
            "attribute_not_exists(pk) OR (attribute_exists(expires_at) AND expires_at <= :now)"
                .into(),
            now_attr(),
        )),
        Expect::Present => Some(("attribute_exists(pk)".into(), HashMap::new())),
        Expect::Version(v) => Some((
            "attribute_exists(pk) AND v = :v".into(),
            HashMap::from([(":v".to_owned(), n(v))]),
        )),
    }
}

fn now() -> u64 {
    crate::clock::Clock::epoch(&crate::clock::SystemClock)
}

impl DynamoStore {
    fn transact_item(&self, op: WriteOp, now: u64) -> Result<TransactWriteItem, StoreError> {
        let built = match op {
            WriteOp::Put { item, expect } => {
                let mut put = Put::builder()
                    .table_name(&self.table)
                    .set_item(Some(to_attrs(&item)));
                if let Some((expr, values)) = condition(expect, now) {
                    put = put.condition_expression(expr);
                    if !values.is_empty() {
                        put = put.set_expression_attribute_values(Some(values));
                    }
                }
                TransactWriteItem::builder()
                    .put(put.build().map_err(backend)?)
                    .build()
            }
            WriteOp::Delete { pk, sk, expect } => {
                let mut del = Delete::builder()
                    .table_name(&self.table)
                    .set_key(Some(key(&pk, &sk)));
                if let Some((expr, values)) = condition(expect, now) {
                    del = del.condition_expression(expr);
                    if !values.is_empty() {
                        del = del.set_expression_attribute_values(Some(values));
                    }
                }
                TransactWriteItem::builder()
                    .delete(del.build().map_err(backend)?)
                    .build()
            }
            WriteOp::Check { pk, sk, expect } => {
                let (expr, values) = condition(expect, now).unwrap_or_else(|| {
                    (
                        "attribute_exists(pk) OR attribute_not_exists(pk)".into(),
                        HashMap::new(),
                    )
                });
                let mut check = ConditionCheck::builder()
                    .table_name(&self.table)
                    .set_key(Some(key(&pk, &sk)))
                    .condition_expression(expr);
                if !values.is_empty() {
                    check = check.set_expression_attribute_values(Some(values));
                }
                TransactWriteItem::builder()
                    .condition_check(check.build().map_err(backend)?)
                    .build()
            }
            WriteOp::Publish(event) => {
                let body = serde_json::to_string(&event)
                    .map_err(|e| StoreError::Serialization(e.to_string()))?;
                let attrs = HashMap::from([
                    (
                        "pk".to_owned(),
                        s(format!("{}{}", saci_outbox::PK_PREFIX, event.id)),
                    ),
                    ("sk".to_owned(), s("EVENT")),
                    (saci_outbox::EVENT_ATTRIBUTE.to_owned(), s(body)),
                    (
                        "expires_at".to_owned(),
                        n(now + saci_outbox::RETENTION_SECS),
                    ),
                ]);
                let put = Put::builder()
                    .table_name(&self.table)
                    .set_item(Some(attrs))
                    .condition_expression("attribute_not_exists(pk)")
                    .build()
                    .map_err(backend)?;
                TransactWriteItem::builder().put(put).build()
            }
        };
        Ok(built)
    }
}

impl DocumentStore for DynamoStore {
    fn get<'a>(&'a self, pk: &'a str, sk: &'a str) -> BoxFut<'a, Result<Option<Item>, StoreError>> {
        Box::pin(async move {
            let out = self
                .client
                .get_item()
                .table_name(&self.table)
                .set_key(Some(key(pk, sk)))
                .consistent_read(true)
                .send()
                .await
                .map_err(|e| backend(e.into_service_error()))?;
            match out.item {
                Some(attrs) => {
                    let item = to_item(&attrs)?;
                    Ok(item.expires_at.is_none_or(|t| t > now()).then_some(item))
                }
                None => Ok(None),
            }
        })
    }

    fn query(&self, query: Query) -> BoxFut<'_, Result<Page, StoreError>> {
        Box::pin(async move {
            let now = now();
            let mut values = HashMap::from([(":pk".to_owned(), s(&query.pk))]);
            let mut expr = "pk = :pk".to_owned();
            if !query.sk_prefix.is_empty() {
                expr.push_str(" AND begins_with(sk, :prefix)");
                values.insert(":prefix".into(), s(&query.sk_prefix));
            }
            let mut req = self
                .client
                .query()
                .table_name(&self.table)
                .key_condition_expression(expr)
                .set_expression_attribute_values(Some(values))
                .scan_index_forward(!query.descending);
            if query.limit > 0 {
                req = req.limit(i32::try_from(query.limit + 1).unwrap_or(i32::MAX));
            }
            if let Some(after) = &query.after {
                req = req.set_exclusive_start_key(Some(key(&query.pk, after)));
            }
            let out = req
                .send()
                .await
                .map_err(|e| backend(e.into_service_error()))?;
            let mut items = Vec::new();
            for attrs in out.items() {
                let item = to_item(attrs)?;
                if item.expires_at.is_none_or(|t| t > now) {
                    items.push(item);
                }
            }
            let last = if query.limit > 0 && items.len() > query.limit {
                items.truncate(query.limit);
                items.last().map(|i| i.sk.clone())
            } else if query.limit == 0 {
                // Sem limite: o DynamoDB pagina em 1 MB; quem chama segue `last`.
                out.last_evaluated_key()
                    .and_then(|k| k.get("sk"))
                    .and_then(|v| v.as_s().ok())
                    .cloned()
            } else {
                None
            };
            Ok(Page { items, last })
        })
    }

    fn write(&self, ops: Vec<WriteOp>) -> BoxFut<'_, Result<(), StoreError>> {
        Box::pin(async move {
            if ops.is_empty() {
                return Ok(());
            }
            let now = now();
            // Uma escrita simples sai mais barata fora de transação.
            if let [WriteOp::Put { item, expect }] = ops.as_slice() {
                let mut put = self
                    .client
                    .put_item()
                    .table_name(&self.table)
                    .set_item(Some(to_attrs(item)));
                if let Some((expr, values)) = condition(*expect, now) {
                    put = put.condition_expression(expr);
                    if !values.is_empty() {
                        put = put.set_expression_attribute_values(Some(values));
                    }
                }
                return match put.send().await {
                    Ok(_) => Ok(()),
                    Err(SdkError::ServiceError(e))
                        if e.err().is_conditional_check_failed_exception() =>
                    {
                        Err(StoreError::Conflict)
                    }
                    Err(e) => Err(backend(e.into_service_error())),
                };
            }
            let items = ops
                .into_iter()
                .map(|op| self.transact_item(op, now))
                .collect::<Result<Vec<_>, _>>()?;
            match self
                .client
                .transact_write_items()
                .set_transact_items(Some(items))
                .send()
                .await
            {
                Ok(_) => Ok(()),
                Err(SdkError::ServiceError(e)) => match e.err() {
                    TransactWriteItemsError::TransactionCanceledException(c)
                        if c.cancellation_reasons()
                            .iter()
                            .any(|r| r.code() == Some("ConditionalCheckFailed")) =>
                    {
                        Err(StoreError::Conflict)
                    }
                    other => Err(backend(other)),
                },
                Err(e) => Err(backend(e.into_service_error())),
            }
        })
    }
}
