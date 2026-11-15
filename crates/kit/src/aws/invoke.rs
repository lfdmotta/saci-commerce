//! [`InternalClient`] por invocação direta de Lambda (ADR 0004).
//!
//! Cada alvo (`catalog`, `orders`...) é resolvido para o nome de uma função
//! pela variável `INTERNAL_<ALVO>_FUNCTION` (ex.: `INTERNAL_CATALOG_FUNCTION=
//! saci-prod-catalog-internal`), que o Terraform preenche. A política IAM da
//! função que chama só permite `lambda:InvokeFunction` nesses ARNs.
//!
//! Alvos de outros repositórios ainda inexistentes (`identity`, `kyc`,
//! `payments`, `finance`, `assistant`) podem apontar para o *sandbox* em
//! processo, só fora de produção (`INTERNAL_<ALVO>_FUNCTION=sandbox`).

use std::collections::HashMap;
use std::sync::Arc;

use aws_sdk_lambda::Client;
use aws_sdk_lambda::primitives::Blob;
use saci_problem::Problem;
use serde_json::Value;

use crate::BoxFut;
use crate::internal::{InternalClient, InternalRequest, InternalResponse, LocalInternal};
use crate::problem::{internal, unavailable};

/// Invoca a função interna de outro domínio.
#[derive(Debug, Clone)]
pub struct LambdaInternal {
    client: Client,
    functions: HashMap<String, String>,
    sandbox: Option<Arc<LocalInternal>>,
}

impl LambdaInternal {
    /// Lê `INTERNAL_<ALVO>_FUNCTION` do ambiente para cada alvo.
    #[must_use]
    pub fn from_env(config: &aws_config::SdkConfig, targets: &[&str]) -> Self {
        let functions = targets
            .iter()
            .filter_map(|t| {
                let var = format!(
                    "INTERNAL_{}_FUNCTION",
                    t.to_ascii_uppercase().replace('-', "_")
                );
                std::env::var(var).ok().map(|f| ((*t).to_owned(), f))
            })
            .collect();
        Self {
            client: Client::new(config),
            functions,
            sandbox: None,
        }
    }

    /// Alvos marcados como `sandbox` são atendidos em processo por `local`.
    /// Recusado em produção.
    pub fn with_sandbox(mut self, local: Arc<LocalInternal>) -> Result<Self, String> {
        if crate::env_or("SACI_ENVIRONMENT", "local") == "prod"
            && self.functions.values().any(|f| f == "sandbox")
        {
            return Err("sandbox não é permitido em produção".into());
        }
        self.sandbox = Some(local);
        Ok(self)
    }
}

impl InternalClient for LambdaInternal {
    fn call<'a>(
        &'a self,
        target: &'a str,
        op: &'a str,
        payload: Value,
    ) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            let function = self.functions.get(target).ok_or_else(|| {
                unavailable(&format!("função interna de {target} não configurada"))
            })?;
            if function == "sandbox" {
                let local = self
                    .sandbox
                    .as_ref()
                    .ok_or_else(|| unavailable(&"sandbox não ligado"))?;
                return local.call(target, op, payload).await;
            }
            let request = InternalRequest {
                op: op.to_owned(),
                payload,
            };
            let body = serde_json::to_vec(&request).map_err(|e| internal(&e))?;
            let out = self
                .client
                .invoke()
                .function_name(function)
                .qualifier("live")
                .payload(Blob::new(body))
                .send()
                .await
                .map_err(|e| unavailable(&format!("{target}.{op}: {}", e.into_service_error())))?;
            if let Some(error) = out.function_error() {
                return Err(unavailable(&format!("{target}.{op} falhou: {error}")));
            }
            let bytes = out.payload.map(Blob::into_inner).unwrap_or_default();
            match serde_json::from_slice::<InternalResponse>(&bytes).map_err(|e| internal(&e))? {
                InternalResponse::Ok(v) => Ok(v),
                InternalResponse::Error(p) => Err(p),
            }
        })
    }
}
