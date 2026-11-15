//! Chamadas síncronas entre serviços (seção 3.2: "chamadas síncronas entre
//! serviços ficam só onde a resposta é necessária na hora").
//!
//! Cada serviço que responde a outros expõe um [`InternalHandler`]: operações
//! nomeadas (`reserve`, `get_sale_for_review`...) com corpo JSON. Quem chama usa
//! um [`InternalClient`] e não sabe onde o outro roda:
//!
//! - na nuvem, `aws::invoke::LambdaInternal` invoca a função `*-internal` do
//!   outro domínio pelo IAM (ADR 0004), sem rede pública e sem rota no API
//!   Gateway;
//! - no servidor de desenvolvimento, [`LocalInternal`] chama em processo.
//!
//! Formato no fio: `{"op": "...", "payload": {...}}` → `{"ok": ...}` ou
//! `{"error": <Problem>}`.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use saci_problem::Problem;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::BoxFut;
use crate::problem::{internal, unavailable};

/// Pedido no fio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalRequest {
    /// Operação.
    pub op: String,
    /// Corpo.
    #[serde(default)]
    pub payload: Value,
}

/// Resposta no fio.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InternalResponse {
    /// Sucesso.
    Ok(Value),
    /// Recusa, propagada a quem chamou a API.
    Error(Problem),
}

/// Quem responde a chamadas internas.
pub trait InternalHandler: Send + Sync + fmt::Debug {
    /// Trata uma operação.
    fn call<'a>(&'a self, op: &'a str, payload: Value) -> BoxFut<'a, Result<Value, Problem>>;
}

/// Quem faz chamadas internas.
pub trait InternalClient: Send + Sync + fmt::Debug {
    /// Chama `op` no serviço `target` (`catalog`, `orders`, `kyc`...).
    fn call<'a>(
        &'a self,
        target: &'a str,
        op: &'a str,
        payload: Value,
    ) -> BoxFut<'a, Result<Value, Problem>>;
}

/// Chamada tipada.
pub async fn call<Req: Serialize + Sync, Resp: DeserializeOwned>(
    client: &dyn InternalClient,
    target: &str,
    op: &str,
    request: &Req,
) -> Result<Resp, Problem> {
    let payload = serde_json::to_value(request).map_err(|e| internal(&e))?;
    let value = client.call(target, op, payload).await?;
    serde_json::from_value(value).map_err(|e| internal(&format!("resposta de {target}.{op}: {e}")))
}

/// Lê o corpo de uma operação.
pub fn payload<T: DeserializeOwned>(payload: Value) -> Result<T, Problem> {
    serde_json::from_value(payload)
        .map_err(|e| internal(&format!("chamada interna malformada: {e}")))
}

/// Serializa a resposta de uma operação.
pub fn reply<T: Serialize>(value: &T) -> Result<Value, Problem> {
    serde_json::to_value(value).map_err(|e| internal(&e))
}

/// Trata um pedido no formato do fio (usado pelo adaptador da Lambda).
pub async fn dispatch(handler: &dyn InternalHandler, request: InternalRequest) -> InternalResponse {
    match handler.call(&request.op, request.payload).await {
        Ok(v) => InternalResponse::Ok(v),
        Err(p) => InternalResponse::Error(p),
    }
}

/// Chamadas em processo: um mapa de alvo → tratador.
#[derive(Default, Clone)]
pub struct LocalInternal {
    handlers: Arc<RwLock<HashMap<String, Arc<dyn InternalHandler>>>>,
}

impl fmt::Debug for LocalInternal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<String> = self
            .handlers
            .read()
            .map(|h| h.keys().cloned().collect())
            .unwrap_or_default();
        write!(f, "LocalInternal({names:?})")
    }
}

impl LocalInternal {
    /// Vazio.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registra o tratador de um alvo.
    pub fn register(&self, target: &str, handler: Arc<dyn InternalHandler>) {
        if let Ok(mut h) = self.handlers.write() {
            h.insert(target.to_owned(), handler);
        }
    }
}

impl InternalClient for LocalInternal {
    fn call<'a>(
        &'a self,
        target: &'a str,
        op: &'a str,
        payload: Value,
    ) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            let handler = self
                .handlers
                .read()
                .ok()
                .and_then(|h| h.get(target).cloned());
            match handler {
                Some(h) => h.call(op, payload).await,
                None => Err(unavailable(&format!(
                    "serviço interno {target} não registrado"
                ))),
            }
        })
    }
}

/// Erro padrão para operação desconhecida.
#[must_use]
pub fn unknown_op(target: &str, op: &str) -> Problem {
    internal(&format!("operação interna desconhecida: {target}.{op}"))
}
