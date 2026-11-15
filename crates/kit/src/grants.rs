//! Concessões de vitrine: quem tem papel em qual vitrine, com quais seções.
//!
//! O autorizador da plataforma lê isso da tabela `authz` (`SUBJECT#<conta>` /
//! `GRANT`, com `storefront_id`, `role` e `sections`) e entrega no contexto de
//! cada requisição. Quem escreve é o `sellers` (abertura de vitrine, aceite
//! de convite, mudança e remoção na equipe). Revogar vale em até 60 s (cache do
//! autorizador).
//!
//! Adaptadores: [`MemoryGrants`] (servidor de desenvolvimento, que também faz
//! o papel do autorizador) e `aws::authz::AuthzGrants` (tabela da plataforma).

use std::collections::HashMap;
use std::fmt;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::BoxFut;
use crate::store::StoreError;

/// Uma concessão.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// Vitrine.
    pub storefront_id: String,
    /// `owner` ou `member`.
    pub role: String,
    /// Seções liberadas (vazio para `owner`).
    pub sections: Vec<String>,
}

/// A porta.
pub trait GrantStore: Send + Sync + fmt::Debug {
    /// Grava (ou troca) a concessão da conta.
    fn put<'a>(
        &'a self,
        account_id: &'a str,
        grant: &'a Grant,
    ) -> BoxFut<'a, Result<(), StoreError>>;
    /// Remove a concessão da conta.
    fn remove<'a>(&'a self, account_id: &'a str) -> BoxFut<'a, Result<(), StoreError>>;
    /// Lê a concessão da conta.
    fn get<'a>(&'a self, account_id: &'a str) -> BoxFut<'a, Result<Option<Grant>, StoreError>>;
}

/// Concessões em memória.
#[derive(Debug, Default)]
pub struct MemoryGrants {
    grants: RwLock<HashMap<String, Grant>>,
}

impl MemoryGrants {
    /// Vazio.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Leitura síncrona (o autorizador do servidor de desenvolvimento).
    #[must_use]
    pub fn lookup(&self, account_id: &str) -> Option<Grant> {
        self.grants
            .read()
            .ok()
            .and_then(|g| g.get(account_id).cloned())
    }
}

impl GrantStore for MemoryGrants {
    fn put<'a>(
        &'a self,
        account_id: &'a str,
        grant: &'a Grant,
    ) -> BoxFut<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.grants
                .write()
                .map_err(|_| StoreError::Backend("lock".into()))?
                .insert(account_id.to_owned(), grant.clone());
            Ok(())
        })
    }

    fn remove<'a>(&'a self, account_id: &'a str) -> BoxFut<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.grants
                .write()
                .map_err(|_| StoreError::Backend("lock".into()))?
                .remove(account_id);
            Ok(())
        })
    }

    fn get<'a>(&'a self, account_id: &'a str) -> BoxFut<'a, Result<Option<Grant>, StoreError>> {
        Box::pin(async move { Ok(self.lookup(account_id)) })
    }
}
