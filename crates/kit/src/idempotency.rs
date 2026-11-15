//! `Idempotency-Key` sobre a porta de armazenamento (seção 4.1 e achado S-06).
//!
//! As regras vêm de `saci-idempotency` (plataforma): chave UUID obrigatória,
//! escopo por quem chama, impressão digital do corpo, resposta guardada por 24
//! horas e reserva com prazo (*lease*). Aqui só muda o armazenamento: o
//! documento `IDEMP#<escopo>#<chave>`/`META` vai na tabela do próprio serviço.
//!
//! Só respostas **2xx** ficam guardadas. Uma recusa (preço mudou, cartão
//! recusado) libera a chave, e a pessoa pode tentar de novo com a mesma chave,
//! como a interface faz.

use std::future::Future;
use std::sync::Arc;

use saci_idempotency::{
    Begin, IdempotencyError, IdempotencyKey, IdempotencyStore, LEASE_SECS, RETENTION_SECS,
    StoredResponse, fingerprint, scope_key,
};
use saci_problem::Problem;
use serde::{Deserialize, Serialize};

use crate::http::{HttpResult, Request, Response};
use crate::problem::problem;
use crate::store::{DocumentStore, Expect, Item, StoreError, WriteOp};

/// Adaptador de [`IdempotencyStore`] sobre qualquer [`DocumentStore`].
#[derive(Debug, Clone)]
pub struct StoreIdempotency(pub Arc<dyn DocumentStore>);

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    fingerprint: String,
    lease_until: u64,
    response: Option<(u16, String, String)>,
}

fn pk(scope: &str) -> String {
    format!("IDEMP#{scope}")
}

#[allow(clippy::needless_pass_by_value)] // usado em map_err
fn err(e: StoreError) -> IdempotencyError {
    IdempotencyError::Store(e.to_string())
}

impl IdempotencyStore for StoreIdempotency {
    async fn begin(&self, scope: &str, fp: &str, now: u64) -> Result<Begin, IdempotencyError> {
        let current = self.0.get(&pk(scope), "META").await.map_err(err)?;
        let expect = match &current {
            None => Expect::Absent,
            Some(item) => {
                let record: Record = item.parse().map_err(err)?;
                if record.fingerprint != fp {
                    return Ok(Begin::Mismatch);
                }
                if let Some((status, content_type, body)) = record.response {
                    return Ok(Begin::Replay(StoredResponse {
                        status,
                        content_type,
                        body,
                    }));
                }
                if record.lease_until > now {
                    return Ok(Begin::InProgress);
                }
                Expect::Version(item.version)
            }
        };
        let record = Record {
            fingerprint: fp.to_owned(),
            lease_until: now + LEASE_SECS,
            response: None,
        };
        let version = current.map_or(1, |i| i.version + 1);
        let item = Item::new(pk(scope), "META", &record)
            .map_err(err)?
            .with_version(version)
            .expiring(now + RETENTION_SECS);
        match self.0.write(vec![WriteOp::Put { item, expect }]).await {
            Ok(()) => Ok(Begin::Started),
            // Outra requisição com a mesma chave ganhou a corrida.
            Err(StoreError::Conflict) => Ok(Begin::InProgress),
            Err(e) => Err(err(e)),
        }
    }

    async fn complete(
        &self,
        scope: &str,
        response: &StoredResponse,
        now: u64,
    ) -> Result<(), IdempotencyError> {
        let current = self.0.get(&pk(scope), "META").await.map_err(err)?;
        let Some(current) = current else {
            return Ok(());
        };
        let mut record: Record = current.parse().map_err(err)?;
        record.response = Some((
            response.status,
            response.content_type.clone(),
            response.body.clone(),
        ));
        let item = Item::new(pk(scope), "META", &record)
            .map_err(err)?
            .with_version(current.version + 1)
            .expiring(now + RETENTION_SECS);
        self.0.write(vec![WriteOp::put(item)]).await.map_err(err)
    }

    async fn release(&self, scope: &str) -> Result<(), IdempotencyError> {
        self.0
            .write(vec![WriteOp::delete(pk(scope), "META")])
            .await
            .map_err(err)
    }
}

fn store_problem(e: &IdempotencyError) -> Problem {
    crate::problem::unavailable(e)
}

/// Roda `action` uma vez por `Idempotency-Key` dentro do escopo `scope`
/// (a conta, ou o IP em compra sem conta).
///
/// - chave ausente ou inválida: `validation-failed`;
/// - mesma chave, outro corpo: `idempotency-mismatch` (422);
/// - mesma chave durante o processamento: `invalid-state` (409);
/// - mesma chave depois do sucesso: a mesma resposta, sem repetir o efeito.
pub async fn run<F, Fut>(
    store: &StoreIdempotency,
    req: &Request,
    scope: &str,
    now: u64,
    action: F,
) -> HttpResult
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = HttpResult>,
{
    let key = IdempotencyKey::require(req.header("idempotency-key"))?;
    let fp = fingerprint(scope, &req.method, &req.path, &req.body);
    let scoped = scope_key(scope, &key);
    match store
        .begin(&scoped, &fp, now)
        .await
        .map_err(|e| store_problem(&e))?
    {
        Begin::Started => {}
        Begin::Replay(stored) => {
            let mut r = Response::text(stored.status, &stored.body);
            r = r
                .header("content-type", &stored.content_type)
                .header("idempotent-replay", "true");
            return Ok(r);
        }
        Begin::InProgress => {
            return Err(problem("invalid-state").with_message(
                "Uma tentativa igual ainda está em andamento. Aguarde um instante.",
            ));
        }
        Begin::Mismatch => return Err(problem("idempotency-mismatch")),
    }
    let result = action().await;
    match &result {
        Ok(r) if (200..300).contains(&r.status) => {
            let stored = StoredResponse {
                status: r.status,
                content_type: r
                    .header_value("content-type")
                    .unwrap_or("application/json")
                    .to_owned(),
                body: r.body.clone(),
            };
            store
                .complete(&scoped, &stored, now)
                .await
                .map_err(|e| store_problem(&e))?;
        }
        _ => {
            let _ = store.release(&scoped).await;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use serde_json::json;

    const KEY: &str = "8b9f3c3e-1d2a-4f5b-9c7d-0e1f2a3b4c5d";

    #[tokio::test]
    async fn replays_success_and_releases_on_error() -> Result<(), Problem> {
        let store = StoreIdempotency(Arc::new(
            MemoryStore::new().with_clock(Arc::new(crate::clock::FixedClock::at(100))),
        ));
        let req = Request::new("POST", "/v1/checkout")
            .with_header("Idempotency-Key", KEY)
            .with_json(&json!({ "a": 1 }));
        let first = run(&store, &req, "acc_1", 100, || async {
            Ok(Response::json(201, &json!({ "id": 1 })))
        })
        .await?;
        let again = run(&store, &req, "acc_1", 101, || async {
            Ok(Response::json(201, &json!({ "id": 2 })))
        })
        .await?;
        assert_eq!(first.body, again.body);

        let other = req.clone().with_json(&json!({ "a": 2 }));
        let mismatch = run(&store, &other, "acc_1", 102, || async {
            Ok(Response::no_content())
        })
        .await;
        assert_eq!(
            mismatch.map_err(|p| p.code),
            Err("idempotency-mismatch".into())
        );

        let fresh = Request::new("POST", "/v1/checkout")
            .with_header("Idempotency-Key", "9b9f3c3e-1d2a-4f5b-9c7d-0e1f2a3b4c5d")
            .with_json(&json!({}));
        let declined = run(&store, &fresh, "acc_1", 103, || async {
            Err(problem("card-declined"))
        })
        .await;
        assert!(declined.is_err());
        let retried = run(&store, &fresh, "acc_1", 104, || async {
            Ok(Response::json(201, &json!({})))
        })
        .await?;
        assert_eq!(retried.status, 201);
        Ok(())
    }

    #[tokio::test]
    async fn requires_key() {
        let store = StoreIdempotency(Arc::new(
            MemoryStore::new().with_clock(Arc::new(crate::clock::FixedClock::at(100))),
        ));
        let req = Request::new("POST", "/v1/checkout");
        let r = run(&store, &req, "acc_1", 1, || async {
            Ok(Response::no_content())
        })
        .await;
        assert_eq!(
            r.map(|_| ())
                .map_err(|p| crate::problem::normalize(p).status),
            Err(422)
        );
    }
}
