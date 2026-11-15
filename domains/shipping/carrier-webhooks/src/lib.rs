//! `carrier-webhooks`: o rastreio que a transportadora envia (proposta 9.6,
//! "entrada própria em hooks.").
//!
//! `POST /carrier/{provider}` no host `hooks.saci.art.br` (M2M). Em nuvem, a
//! entrada é um API Gateway regional com **mTLS** (o único endereço público
//! além do CloudFront, seção 2.3); aqui, além do certificado, a função confere
//! a assinatura do corpo:
//!
//! - `X-Saci-Signature: sha256=<hex>` = HMAC-SHA256 do corpo com o segredo
//!   combinado com a transportadora (no Secrets Manager);
//! - idempotente pelo `event_id` da transportadora;
//! - responde rápido (202), grava e publica `shipping.tracking.updated.v1`;
//! - o corpo nunca vai para o log (tem endereço).
//!
//! O formato do corpo é o normalizado da Saci; o de cada transportadora entra
//! como tradução neste crate quando houver contrato.

use std::sync::Arc;

use saci_commerce_kit::clock::Clock;
use saci_commerce_kit::cursor::{constant_time_eq, hmac_sha256};
use saci_commerce_kit::events::{self, first_delivery};
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::problem::{invalid, not_found, problem};
use saci_commerce_kit::store::{DocumentStore, StoreExt, WriteOp};
use saci_commerce_kit::{BoxFut, Problem};
use serde::Deserialize;
use serde_json::{Value, json};

/// Dependências.
#[derive(Clone)]
pub struct CarrierWebhooks {
    /// Tabela do `shipping`.
    pub store: Arc<dyn DocumentStore>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Segredo da assinatura, por transportadora.
    pub secrets: Vec<(String, Vec<u8>)>,
}

impl std::fmt::Debug for CarrierWebhooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let providers: Vec<&str> = self.secrets.iter().map(|(p, _)| p.as_str()).collect();
        write!(f, "CarrierWebhooks({providers:?})")
    }
}

#[derive(Debug, Deserialize)]
struct TrackingUpdate {
    event_id: String,
    tracking_code: String,
    status: String,
}

impl HttpService for CarrierWebhooks {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

impl CarrierWebhooks {
    async fn route(&self, req: &Request) -> HttpResult {
        match (req.method.as_str(), req.segments().as_slice()) {
            ("POST", ["carrier", provider] | ["v1", "shipping", "hooks", "carrier", provider]) => {
                self.receive(req, provider).await
            }
            _ => Err(not_found()),
        }
    }

    fn verify(&self, provider: &str, req: &Request) -> Result<(), Problem> {
        let secret = self
            .secrets
            .iter()
            .find(|(p, _)| p == provider)
            .map(|(_, s)| s)
            .ok_or_else(not_found)?;
        let header = req
            .header("x-saci-signature")
            .and_then(|h| h.strip_prefix("sha256="))
            .ok_or_else(|| problem("unauthorized"))?;
        let expected = hex::encode(hmac_sha256(secret, &[&req.body]));
        if constant_time_eq(expected.as_bytes(), header.to_ascii_lowercase().as_bytes()) {
            Ok(())
        } else {
            Err(problem("unauthorized"))
        }
    }

    async fn receive(&self, req: &Request, provider: &str) -> HttpResult {
        self.verify(provider, req)?;
        let update: TrackingUpdate = req.json()?;
        if !["posted", "in_transit", "out_for_delivery", "delivered"]
            .contains(&update.status.as_str())
        {
            return Err(invalid("status", "Situação desconhecida."));
        }
        if !first_delivery(
            self.store.as_ref(),
            &format!("carrier-{provider}"),
            &update.event_id,
            self.clock.epoch(),
        )
        .await?
        {
            return Ok(Response::json(202, &json!({ "duplicate": true })));
        }
        let Some((sale, _)) = self
            .store
            .load::<Value>(&format!("TRACK#{}", update.tracking_code), "META")
            .await?
        else {
            // Código desconhecido: aceita (a transportadora não deve repetir), registra e segue.
            tracing::warn!(provider, "rastreio de código desconhecido");
            return Ok(Response::json(202, &json!({})));
        };
        let event = events::event(
            "shipping",
            "shipping.tracking.updated.v1",
            sale["sale_id"].as_str().unwrap_or_default(),
            &json!({ "sale_id": sale["sale_id"], "storefront_id": sale["storefront_id"], "status": update.status }),
        )?;
        self.store.write(vec![WriteOp::Publish(event)]).await?;
        Ok(Response::json(202, &json!({})))
    }
}

/// Assina um corpo como a transportadora (testes e simulação local).
#[must_use]
pub fn sign(secret: &[u8], body: &[u8]) -> String {
    format!("sha256={}", hex::encode(hmac_sha256(secret, &[body])))
}
