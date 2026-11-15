//! `support-api`: os chamados (proposta 5.11).
//!
//! | Rota | Acesso |
//! | --- | --- |
//! | `GET /v1/support/me/tickets` | `SELLER:atendimento` (`status`, `cursor`), sem mensagens |
//! | `GET /v1/support/me/tickets/{protocol}` | `SELLER:atendimento`, com mensagens |
//! | `POST /v1/support/me/tickets/{protocol}/messages` | `SELLER:atendimento` + `Idempotency-Key` (409 se encerrado) |
//! | `POST /v1/support/me/tickets/{protocol}/resolve` | `SELLER:atendimento` |
//!
//! O chamado é aberto por dentro, pelo evento `assistant.handoff.requested.v1`
//! (a Caipora passa a conversa para a vitrine). Regras de S-23: os textos de
//! quem compra são guardados **mascarados** (CPF, contato, cartão), o resumo
//! da Caipora é texto puro (a interface mostra sem links), e o nome de quem
//! responde vem da sessão no servidor, nunca do corpo.
//!
//! Tabela `saci-<env>-support`:
//!
//! | `pk` | `sk` | Documento |
//! | --- | --- | --- |
//! | `TKT#<chamado>` | `META` | [`TicketRecord`] |
//! | `TKT#<chamado>` | `MSG#<data>#<mensagem>` | mensagem |
//! | `PROTO#<protocolo>` | `META` | chamado de um protocolo |
//! | `STF#<vitrine>` | `TKT#<chamado>` | situação e data (lista do painel) |

use std::sync::Arc;

use saci_commerce_kit::auth::{self, Section};
use saci_commerce_kit::clock::Clock;
use saci_commerce_kit::cursor::CursorSigner;
use saci_commerce_kit::events::{self, ConsumeError, Consumer, Event, data};
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::idempotency::{self, StoreIdempotency};
use saci_commerce_kit::internal::{
    self, InternalClient, InternalHandler, payload, reply, unknown_op,
};
use saci_commerce_kit::problem::{invalid, not_found, problem};
use saci_commerce_kit::store::{DocumentStore, Expect, Item, StoreError, StoreExt, WriteOp};
use saci_commerce_kit::text::redact;
use saci_commerce_kit::types::Page;
use saci_commerce_kit::{BoxFut, Problem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Chamados por página.
const PAGE: usize = 20;

/// Dependências do serviço.
#[derive(Debug, Clone)]
pub struct Support {
    /// Tabela.
    pub store: Arc<dyn DocumentStore>,
    /// `identity` (nome de quem responde) e `assistant` (dados do repasse).
    pub internal: Arc<dyn InternalClient>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Cursores.
    pub cursors: CursorSigner,
}

/// Um chamado guardado (o `Ticket` do contrato, sem as mensagens).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketRecord {
    /// Id (`tkt_`).
    pub id: String,
    /// Protocolo (leitura humana).
    pub protocol: String,
    /// Vitrine.
    pub storefront_id: String,
    /// Assunto.
    pub subject: String,
    /// `open`, `waiting_customer` ou `resolved`.
    pub status: String,
    /// Abertura.
    pub opened_at: String,
    /// Última mudança.
    pub updated_at: String,
    /// Primeiro nome, cidade e UF de quem pediu ajuda.
    pub customer: Value,
    /// Pedido ligado.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Value>,
    /// Resumo escrito pela Caipora (texto puro).
    pub handoff_summary: String,
    /// Mensagens não lidas pela vitrine.
    pub unread: u32,
    /// Conversa da Caipora de onde veio.
    #[serde(default)]
    pub conversation_id: Option<String>,
}

impl TicketRecord {
    fn public(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            for k in ["id", "storefront_id", "conversation_id"] {
                o.remove(k);
            }
        }
        v
    }
}

/// Uma mensagem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    /// Id.
    pub id: String,
    /// `customer`, `vitrine`, `caipora` ou `system`.
    pub author: String,
    /// Nome de quem escreveu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    /// Texto (mascarado quando é de quem compra).
    pub text: String,
    /// Quando.
    pub created_at: String,
}

fn ticket_pk(id: &str) -> String {
    format!("TKT#{id}")
}

impl HttpService for Support {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

impl Support {
    async fn route(&self, req: &Request) -> HttpResult {
        match (req.method.as_str(), req.segments().as_slice()) {
            ("GET", ["v1", "support", "me", "tickets"]) => self.list(req).await,
            ("GET", ["v1", "support", "me", "tickets", protocol]) => self.get(req, protocol).await,
            ("POST", ["v1", "support", "me", "tickets", protocol, "messages"]) => {
                self.reply(req, protocol).await
            }
            ("POST", ["v1", "support", "me", "tickets", protocol, "resolve"]) => {
                self.resolve(req, protocol).await
            }
            _ => Err(not_found()),
        }
    }

    async fn list(&self, req: &Request) -> HttpResult {
        let stf = auth::section(req, Section::Atendimento)?.to_owned();
        let status = req.query_enum("status", &["open", "waiting_customer", "resolved"])?;
        let index = self
            .store
            .query_all(format!("STF#{stf}"), "TKT#".into())
            .await?;
        let mut entries: Vec<Value> = index.iter().map(|i| i.doc.clone()).collect();
        if let Some(s) = status {
            entries.retain(|e| e["status"] == s);
        }
        entries.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
        let scope = format!("{stf}:tickets:{}", status.unwrap_or("*"));
        let start = match req.query("cursor") {
            Some(c) => self
                .cursors
                .decode(&scope, c)?
                .parse::<usize>()
                .map_err(|_| invalid("cursor", "Cursor inválido."))?,
            None => 0,
        };
        let mut items = Vec::new();
        for e in entries.iter().skip(start).take(PAGE) {
            if let Some((t, _)) = self
                .store
                .load::<TicketRecord>(
                    &ticket_pk(e["ticket_id"].as_str().unwrap_or_default()),
                    "META",
                )
                .await?
            {
                items.push(t.public());
            }
        }
        let next_cursor = (start + PAGE < entries.len())
            .then(|| self.cursors.encode(&scope, &(start + PAGE).to_string()));
        Ok(Response::ok(&Page { items, next_cursor }))
    }

    async fn owned(
        &self,
        storefront: &str,
        protocol: &str,
    ) -> Result<(TicketRecord, u64), Problem> {
        let (proto, _) = self
            .store
            .load::<Value>(&format!("PROTO#{protocol}"), "META")
            .await?
            .ok_or_else(not_found)?;
        match self
            .store
            .load::<TicketRecord>(
                &ticket_pk(proto["ticket_id"].as_str().unwrap_or_default()),
                "META",
            )
            .await?
        {
            Some((t, v)) if t.storefront_id == storefront => Ok((t, v)),
            _ => Err(not_found()),
        }
    }

    async fn messages(&self, ticket_id: &str) -> Result<Vec<Value>, Problem> {
        let items = self
            .store
            .query_all(ticket_pk(ticket_id), "MSG#".into())
            .await?;
        Ok(items.into_iter().map(|i| i.doc).collect())
    }

    async fn get(&self, req: &Request, protocol: &str) -> HttpResult {
        let stf = auth::section(req, Section::Atendimento)?.to_owned();
        let (mut ticket, version) = self.owned(&stf, protocol).await?;
        let mut view = ticket.public();
        view["messages"] = json!(self.messages(&ticket.id).await?);
        if ticket.unread > 0 {
            ticket.unread = 0;
            // Lido pela vitrine; conflito aqui não importa (a próxima leitura zera).
            let _ = self
                .store
                .write(vec![WriteOp::replace(
                    Item::new(ticket_pk(&ticket.id), "META", &ticket)?,
                    version,
                )])
                .await;
        }
        Ok(Response::ok(&view))
    }

    async fn reply(&self, req: &Request, protocol: &str) -> HttpResult {
        let stf = auth::section(req, Section::Atendimento)?.to_owned();
        let account = auth::user(req)?.to_owned();
        #[derive(Deserialize)]
        struct Body {
            text: String,
        }
        let body: Body = req.json()?;
        let store = StoreIdempotency(Arc::clone(&self.store));
        idempotency::run(&store, req, &account, self.clock.epoch(), || async {
            let text = body.text.trim();
            if text.is_empty() || text.chars().count() > 2_000 {
                return Err(invalid("text", "A mensagem tem de 1 a 2.000 caracteres."));
            }
            let (mut ticket, version) = self.owned(&stf, protocol).await?;
            if ticket.status == "resolved" {
                return Err(problem("ticket-resolved"));
            }
            let profile: Value = internal::call(self.internal.as_ref(), "identity", "account_profile", &json!({ "account_id": account }))
                .await
                .unwrap_or_default();
            let name = profile["preferred_name"].as_str().or_else(|| profile["name"].as_str()).map(saci_commerce_kit::text::first_name);
            let message = MessageRecord {
                id: saci_ids::Id::new("msg").to_string(),
                author: "vitrine".into(),
                author_name: name,
                text: text.to_owned(),
                created_at: self.clock.rfc3339(),
            };
            ticket.status = "waiting_customer".into();
            ticket.updated_at = message.created_at.clone();
            let event = events::event(
                "support",
                "support.ticket.message_added.v1",
                &ticket.id,
                &json!({ "ticket_id": ticket.id, "message_id": message.id, "storefront_id": ticket.storefront_id, "author": "vitrine" }),
            )?;
            self.store
                .write(vec![
                    WriteOp::replace(Item::new(ticket_pk(&ticket.id), "META", &ticket)?, version),
                    WriteOp::put(Item::new(ticket_pk(&ticket.id), format!("MSG#{}#{}", message.created_at, message.id), &message)?),
                    WriteOp::put(Item::new(format!("STF#{stf}"), format!("TKT#{}", ticket.id), &index_doc(&ticket))?),
                    WriteOp::Publish(event),
                ])
                .await?;
            Ok(Response::ok(&message))
        })
        .await
    }

    async fn resolve(&self, req: &Request, protocol: &str) -> HttpResult {
        let stf = auth::section(req, Section::Atendimento)?.to_owned();
        let (mut ticket, version) = self.owned(&stf, protocol).await?;
        if ticket.status != "resolved" {
            ticket.status = "resolved".into();
            ticket.updated_at = self.clock.rfc3339();
            let event = events::event(
                "support",
                "support.ticket.resolved.v1",
                &ticket.id,
                &json!({ "ticket_id": ticket.id, "storefront_id": ticket.storefront_id }),
            )?;
            self.store
                .write(vec![
                    WriteOp::replace(Item::new(ticket_pk(&ticket.id), "META", &ticket)?, version),
                    WriteOp::put(Item::new(
                        format!("STF#{stf}"),
                        format!("TKT#{}", ticket.id),
                        &index_doc(&ticket),
                    )?),
                    WriteOp::Publish(event),
                ])
                .await?;
        }
        let mut view = ticket.public();
        view["messages"] = json!(self.messages(&ticket.id).await?);
        Ok(Response::ok(&view))
    }

    /// Próximo protocolo: `AAAAMMDD` + sequência de 5 dígitos.
    async fn next_protocol(&self) -> Result<String, Problem> {
        let day = self.clock.today();
        let prefix = format!(
            "{:04}{:02}{:02}",
            day.year(),
            u8::from(day.month()),
            day.day()
        );
        for _ in 0..10 {
            let current = self.store.get("SEQ", &prefix).await?;
            let (n, expect, version) = match &current {
                Some(i) => (
                    i.doc["n"].as_u64().unwrap_or(0),
                    Expect::Version(i.version),
                    i.version + 1,
                ),
                None => (0, Expect::Absent, 1),
            };
            let item = Item::new("SEQ", prefix.clone(), &json!({ "n": n + 1 }))?
                .with_version(version)
                .expiring(self.clock.epoch() + 3 * 86_400);
            match self.store.write(vec![WriteOp::Put { item, expect }]).await {
                Ok(()) => return Ok(format!("{prefix}{:05}", n + 1)),
                Err(StoreError::Conflict) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(problem("service-unavailable"))
    }

    /// Abre um chamado (repasse da Caipora). Textos de quem compra mascarados (S-23).
    pub async fn open(
        &self,
        ticket: TicketRecord,
        messages: Vec<MessageRecord>,
    ) -> Result<(), Problem> {
        let mut ops = vec![
            WriteOp::create(Item::new(ticket_pk(&ticket.id), "META", &ticket)?),
            WriteOp::create(Item::new(
                format!("PROTO#{}", ticket.protocol),
                "META",
                &json!({ "ticket_id": ticket.id, "storefront_id": ticket.storefront_id }),
            )?),
            WriteOp::put(Item::new(
                format!("STF#{}", ticket.storefront_id),
                format!("TKT#{}", ticket.id),
                &index_doc(&ticket),
            )?),
        ];
        for m in messages {
            let mut m = m;
            if m.author == "customer" {
                m.text = redact(&m.text);
            }
            ops.push(WriteOp::put(Item::new(
                ticket_pk(&ticket.id),
                format!("MSG#{}#{}", m.created_at, m.id),
                &m,
            )?));
        }
        let mut data = json!({ "ticket_id": ticket.id, "storefront_id": ticket.storefront_id, "conversation_id": ticket.conversation_id });
        if let Some(order) = ticket.order.as_ref().and_then(|o| o["id"].as_str()) {
            data["order_id"] = json!(order);
        }
        if ticket.conversation_id.is_some() {
            ops.push(WriteOp::Publish(events::event(
                "support",
                "support.ticket.opened.v1",
                &ticket.id,
                &data,
            )?));
        }
        Ok(self.store.write(ops).await?)
    }
}

fn index_doc(t: &TicketRecord) -> Value {
    json!({ "ticket_id": t.id, "status": t.status, "updated_at": t.updated_at })
}

impl InternalHandler for Support {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            match op {
                "open_tickets" => {
                    #[derive(Deserialize)]
                    struct S {
                        storefront_id: String,
                    }
                    let s: S = payload(body)?;
                    let index = self
                        .store
                        .query_all(format!("STF#{}", s.storefront_id), "TKT#".into())
                        .await?;
                    reply(
                        &json!({ "count": index.iter().filter(|i| i.doc["status"] != "resolved").count() }),
                    )
                }
                _ => Err(unknown_op("support", op)),
            }
        })
    }
}

/// Consumidor de `assistant.handoff.requested.v1`: abre o chamado.
#[derive(Debug, Clone)]
pub struct Handoffs(pub Support);

impl Consumer for Handoffs {
    fn name(&self) -> &'static str {
        "support-handoffs"
    }

    fn accepts(&self) -> &'static [&'static str] {
        &["assistant.handoff.requested.v1"]
    }

    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            #[allow(clippy::struct_field_names)]
            struct Handoff {
                conversation_id: String,
                storefront_id: String,
                #[serde(default)]
                order_id: Option<String>,
            }
            let e: Handoff = data(event)?;
            let s = &self.0;
            // O evento só traz ids: o resto vem da conversa (chamada interna ao `assistant`).
            let details: Value = internal::call(
                s.internal.as_ref(),
                "assistant",
                "handoff_details",
                &json!({ "conversation_id": e.conversation_id }),
            )
            .await
            .map_err(ConsumeError::from)?;
            let now = s.clock.rfc3339();
            let order = match (&e.order_id, details["order_number"].as_str()) {
                (Some(id), Some(number)) => Some(json!({ "id": id, "number": number })),
                _ => None,
            };
            let ticket = TicketRecord {
                id: saci_ids::Id::new(saci_ids::prefix::TICKET).to_string(),
                protocol: s.next_protocol().await.map_err(ConsumeError::from)?,
                storefront_id: e.storefront_id,
                subject: details["subject"]
                    .as_str()
                    .unwrap_or("Conversa da Caipora")
                    .chars()
                    .take(120)
                    .collect(),
                status: "open".into(),
                opened_at: now.clone(),
                updated_at: now,
                customer: details["customer"].clone(),
                order,
                handoff_summary: redact(details["summary"].as_str().unwrap_or_default()),
                unread: u32::try_from(details["messages"].as_array().map_or(0, Vec::len))
                    .unwrap_or(0),
                conversation_id: Some(e.conversation_id),
            };
            let messages: Vec<MessageRecord> =
                serde_json::from_value(details["messages"].clone()).unwrap_or_default();
            s.open(ticket, messages).await.map_err(ConsumeError::from)
        })
    }
}

/// Migração `0001-demo-tickets` (só fora de produção).
pub async fn migrate(support: &Support, allow_fixtures: bool) -> Result<Vec<String>, Problem> {
    const DEMO: &str = include_str!("../migrations/0001-demo-tickets.json");
    if !allow_fixtures
        || support
            .store
            .get("MIGRATION", "0001-demo-tickets")
            .await?
            .is_some()
    {
        return Ok(Vec::new());
    }
    #[derive(Deserialize)]
    struct Demo {
        tickets: Vec<Value>,
    }
    let demo: Demo =
        serde_json::from_str(DEMO).map_err(|e| saci_commerce_kit::problem::internal(&e))?;
    for t in demo.tickets {
        let messages: Vec<MessageRecord> =
            serde_json::from_value(t["messages"].clone()).unwrap_or_default();
        let mut record: TicketRecord =
            serde_json::from_value(t).map_err(|e| saci_commerce_kit::problem::internal(&e))?;
        record.conversation_id = None;
        support.open(record, messages).await?;
    }
    support
        .store
        .write(vec![WriteOp::create(Item::new(
            "MIGRATION",
            "0001-demo-tickets",
            &json!({ "at": support.clock.rfc3339() }),
        )?)])
        .await?;
    Ok(vec!["0001-demo-tickets".into()])
}
