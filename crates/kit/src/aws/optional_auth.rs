//! Autenticação opcional para rotas públicas que também aceitam conta.
//!
//! `POST /v1/checkout` é `USER` **ou** `PUB-L` com `buyer` (compra sem conta).
//! O API Gateway não tem autorizador opcional: sem token, o autorizador nega.
//! Então a rota é pública na borda, e a função confere o `Authorization`,
//! quando vier, com **o mesmo código do autorizador da plataforma**
//! (`saci-authorizer`, ADR 0005): mesma lista de emissores, mesmas chaves da
//! tabela `authz`, mesma revogação. Token presente e inválido é 401 (a
//! interface renova a sessão); sem token, a requisição segue anônima.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use saci_authorizer::adapters::dynamodb::DynamoDbAuthzStore;
use saci_authorizer::application::{Authorizer, Config, Credentials};
use saci_authz::AuthContext;
use serde_json::Value;

use crate::http::Request;

/// Autenticador opcional.
#[derive(Debug)]
pub struct OptionalAuth {
    authorizer: Authorizer<DynamoDbAuthzStore>,
}

impl OptionalAuth {
    /// Lê `AUTHZ_TABLE` e `TRUSTED_ISSUERS` (separados por vírgula) do ambiente.
    pub fn from_env(config: &aws_config::SdkConfig) -> Result<Self, String> {
        let table = crate::env("AUTHZ_TABLE")?;
        let trusted: BTreeSet<String> = crate::env_or("TRUSTED_ISSUERS", "")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let store = DynamoDbAuthzStore::new(aws_sdk_dynamodb::Client::new(config), table);
        let cfg = Config {
            trusted_issuers: trusted,
            issuer_cache_ttl: Duration::from_secs(300),
            clock_skew_secs: 30,
        };
        Ok(Self {
            authorizer: Authorizer::new(store, cfg),
        })
    }

    /// Preenche `req.auth` quando há `Authorization`. `Err` = token inválido (401).
    pub async fn authenticate(&self, req: &mut Request, now: u64) -> Result<(), String> {
        if req.auth.is_some() {
            return Ok(());
        }
        let Some(authorization) = req.header("authorization").map(str::to_owned) else {
            return Ok(());
        };
        let credentials = Credentials {
            authorization: Some(authorization),
            guest_token: None,
        };
        match self.authorizer.authorize(&credentials, now).await {
            Ok(principal) => {
                let fields: HashMap<String, Value> = principal
                    .context
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), Value::from(v)))
                    .collect();
                req.auth = AuthContext::from_fields(&fields).ok();
                Ok(())
            }
            Err(denial) => {
                tracing::info!(reason = %denial, "token recusado na rota com autenticação opcional");
                Err(denial.to_string())
            }
        }
    }
}
