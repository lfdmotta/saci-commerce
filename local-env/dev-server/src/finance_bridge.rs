//! Ponte opcional com o servidor de desenvolvimento do `saci-finance`
//! (`SACI_DEV_FINANCE_URL=http://localhost:8790`).
//!
//! Sem a variável, nada muda: `finance` é o sandbox (`crates/sandbox`) para o
//! `orders` e `/v1/finance` não responde (a interface o simula). Com ela:
//!
//! | O quê | Como |
//! | --- | --- |
//! | `/v1/finance/*` | Repassado ao `saci-finance-dev` com a conta e o papel na vitrine conferidos aqui (`x-saci-dev-account`, `x-saci-dev-storefront`, `x-saci-dev-role`): o Financeiro é só da pessoa responsável |
//! | Alvo interno `finance` (`sale_payment`) | `POST /internal/finance` no `saci-finance-dev` |
//! | `orders.sales_for_finance` | O finance chama `POST /internal/orders` daqui (só de `127.0.0.1`) |
//! | Eventos que o finance assina | [`FinanceEvents`] entrega em `POST /dev/events` de lá |
//! | `finance.payout.sent.v1` | O finance o entrega em `POST /dev/events` daqui (o `orders` marca as vendas como `invoiced`) |

use saci_commerce_kit::BoxFut;
use saci_commerce_kit::events::{ConsumeError, Consumer, Event};

use crate::identity_bridge::Bridge;

/// Lê `SACI_DEV_FINANCE_URL`. Ausente: sem ponte.
pub(crate) fn from_env() -> Result<Option<Bridge>, String> {
    Bridge::from_var("SACI_DEV_FINANCE_URL", true)
}

/// Leva ao `saci-finance-dev` os eventos que ele assina na nuvem.
#[derive(Debug, Clone)]
pub(crate) struct FinanceEvents(pub Bridge);

impl Consumer for FinanceEvents {
    fn name(&self) -> &'static str {
        "dev-finance-bridge"
    }

    fn accepts(&self) -> &'static [&'static str] {
        &[
            "payments.charge.authorized.v1",
            "payments.pix.paid.v1",
            "payments.refund.done.v1",
            "payments.payout.completed.v1",
            "payments.payout.failed.v1",
            "orders.sale.delivered.v1",
            "orders.return.opened.v1",
            "shipping.label.purchased.v1",
        ]
    }

    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
        Box::pin(async move {
            let body =
                serde_json::to_vec(event).map_err(|e| ConsumeError::Malformed(e.to_string()))?;
            match self.0.post_json("/dev/events", &body).await {
                Ok(status) if status < 300 => Ok(()),
                Ok(status) => Err(ConsumeError::Retry(format!(
                    "saci-finance-dev respondeu {status}"
                ))),
                Err(e) => Err(ConsumeError::Retry(e)),
            }
        })
    }
}
