//! [`GrantStore`] sobre a tabela `authz` da plataforma (SSM
//! `/saci/<env>/gateway/authz-table`), no formato que o autorizador lê:
//! `pk = SUBJECT#<conta>`, `sk = GRANT`, `storefront_id` (S), `role` (S) e
//! `sections` (S, separado por vírgula).

use std::collections::HashMap;

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::types::AttributeValue;

use crate::BoxFut;
use crate::grants::{Grant, GrantStore};
use crate::store::StoreError;

/// Tabela `authz`.
#[derive(Debug, Clone)]
pub struct AuthzGrants {
    client: Client,
    table: String,
}

impl AuthzGrants {
    /// Cria o adaptador.
    #[must_use]
    pub fn new(client: Client, table: impl Into<String>) -> Self {
        Self {
            client,
            table: table.into(),
        }
    }
}

fn key(account: &str) -> HashMap<String, AttributeValue> {
    HashMap::from([
        (
            "pk".to_owned(),
            AttributeValue::S(format!("SUBJECT#{account}")),
        ),
        ("sk".to_owned(), AttributeValue::S("GRANT".into())),
    ])
}

fn backend(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

impl GrantStore for AuthzGrants {
    fn put<'a>(
        &'a self,
        account_id: &'a str,
        grant: &'a Grant,
    ) -> BoxFut<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let mut item = key(account_id);
            item.insert(
                "storefront_id".into(),
                AttributeValue::S(grant.storefront_id.clone()),
            );
            item.insert("role".into(), AttributeValue::S(grant.role.clone()));
            item.insert(
                "sections".into(),
                AttributeValue::S(grant.sections.join(",")),
            );
            self.client
                .put_item()
                .table_name(&self.table)
                .set_item(Some(item))
                .send()
                .await
                .map_err(|e| backend(e.into_service_error()))?;
            Ok(())
        })
    }

    fn remove<'a>(&'a self, account_id: &'a str) -> BoxFut<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.client
                .delete_item()
                .table_name(&self.table)
                .set_key(Some(key(account_id)))
                .send()
                .await
                .map_err(|e| backend(e.into_service_error()))?;
            Ok(())
        })
    }

    fn get<'a>(&'a self, account_id: &'a str) -> BoxFut<'a, Result<Option<Grant>, StoreError>> {
        Box::pin(async move {
            let out = self
                .client
                .get_item()
                .table_name(&self.table)
                .set_key(Some(key(account_id)))
                .send()
                .await
                .map_err(|e| backend(e.into_service_error()))?;
            Ok(out.item.and_then(|i| {
                let s = |n: &str| i.get(n).and_then(|v| v.as_s().ok()).cloned();
                Some(Grant {
                    storefront_id: s("storefront_id")?,
                    role: s("role")?,
                    sections: s("sections")
                        .unwrap_or_default()
                        .split(',')
                        .filter(|x| !x.is_empty())
                        .map(str::to_owned)
                        .collect(),
                })
            }))
        })
    }
}
