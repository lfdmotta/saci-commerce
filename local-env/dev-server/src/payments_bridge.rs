//! Ponte opcional com o servidor de desenvolvimento do `saci-payments`
//! (`SACI_DEV_PAYMENTS_URL=http://localhost:8789`).
//!
//! Sem a variável, nada muda: `payments` é o sandbox (`crates/sandbox`). Com ela,
//! este servidor faz o papel da borda de `pay.` e do barramento:
//!
//! | O quê | Como |
//! | --- | --- |
//! | `/v1/payments/*` | Repassado ao `saci-payments-dev`, com a conta já conferida aqui em `x-saci-dev-account` e `x-saci-dev-auth-time` (o contexto do autorizador, na nuvem) |
//! | Alvo interno `payments` (`charge`, `charge_status`, `refund`) | `POST /internal/payments` no `saci-payments-dev`, no formato `{op, payload}` das Lambdas |
//! | Eventos `payments.*` | O `saci-payments-dev` os entrega em `POST /dev/events` daqui (barramento em processo) |
//! | `checkout.order.abandoned.v1` | [`PaymentsEvents`] entrega em `POST /dev/events` de lá |
//!
//! A tokenização do cartão não passa por aqui: a interface chama direto o PSP
//! simulado, na URL que `GET /v1/payments/client-config` devolve
//! (`http://localhost:8789/psp/sandbox/tokens`), como faria com um PSP de verdade.

use saci_commerce_kit::BoxFut;
use saci_commerce_kit::events::{ConsumeError, Consumer, Event};

use crate::identity_bridge::Bridge;

/// Lê `SACI_DEV_PAYMENTS_URL`. Ausente: sem ponte.
pub(crate) fn from_env() -> Result<Option<Bridge>, String> {
    Bridge::from_var("SACI_DEV_PAYMENTS_URL", true)
}

/// Leva ao `saci-payments-dev` os eventos que ele assina na nuvem.
#[derive(Debug, Clone)]
pub(crate) struct PaymentsEvents(pub Bridge);

impl Consumer for PaymentsEvents {
    fn name(&self) -> &'static str {
        "dev-payments-bridge"
    }

    fn accepts(&self) -> &'static [&'static str] {
        &["checkout.order.abandoned.v1"]
    }

    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
        Box::pin(async move {
            let body =
                serde_json::to_vec(event).map_err(|e| ConsumeError::Malformed(e.to_string()))?;
            match self.0.post_json("/dev/events", &body).await {
                Ok(status) if status < 300 => Ok(()),
                Ok(status) => Err(ConsumeError::Retry(format!(
                    "saci-payments-dev respondeu {status}"
                ))),
                Err(e) => Err(ConsumeError::Retry(e)),
            }
        })
    }
}
