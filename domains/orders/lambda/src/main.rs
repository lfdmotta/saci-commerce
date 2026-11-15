//! `saci-orders`: as funções do domínio `orders` (ADR 0003).
//!
//! | `SACI_FUNCTION` | Gatilho | O que roda |
//! | --- | --- | --- |
//! | `api` | API Gateway (compras: `user`; pedido sem conta: `mixed`) | `/v1/orders` |
//! | `internal` | Invocação direta | pedido do checkout, vendas para `shipping`, `reviews`, `sellers`, `catalog` |
//! | `progress` | Fila de `shipping.*` e `finance.payout.sent.v1` | etiqueta, entrega, repasse |
//! | `relay` | DynamoDB Streams | *outbox* → EventBridge |
//! | `migrate` | Terraform | compras e vendas de exemplo (fora de produção) |
//!
//! Variáveis: `TABLE`, `SACI_CURSOR_KEY_PARAM`, `SACI_ENVIRONMENT`, `SACI_SEED_FIXTURES`,
//! `INTERNAL_<ALVO>_FUNCTION`, `EVENT_BUS`.

use std::sync::Arc;

use lambda_runtime::Error;
use saci_commerce_kit::aws::dynamodb::DynamoStore;
use saci_commerce_kit::aws::lambda::{
    cursors, init_telemetry, run_consumer, run_http, run_internal, run_relay, run_task,
};
use saci_commerce_kit::aws::sdk_config;
use saci_commerce_kit::clock::{Clock, SystemClock};
use saci_orders_api::{Orders, SaleProgress};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let function = saci_commerce_kit::env("SACI_FUNCTION")?;
    init_telemetry(&format!("orders-{function}"));
    let config = sdk_config().await;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = Arc::new(DynamoStore::from_env(&config, "TABLE")?);
    let orders = Orders {
        store: store.clone(),
        internal: saci_commerce_sandbox::boot::internal_client(&config)?,
        clock: Arc::clone(&clock),
        cursors: cursors(&config).await?,
    };
    match function.as_str() {
        "api" => run_http(Arc::new(orders)).await,
        "internal" => run_internal(Arc::new(orders)).await,
        "progress" => run_consumer(store, vec![Arc::new(SaleProgress(orders))], clock).await,
        "relay" => run_relay(&config).await,
        "migrate" => {
            let fixtures = saci_commerce_kit::env_or("SACI_SEED_FIXTURES", "false") == "true";
            let orders = Arc::new(orders);
            run_task(move |_| {
                let orders = Arc::clone(&orders);
                async move {
                    let applied = saci_orders_api::seed::migrate(&orders, fixtures)
                        .await
                        .map_err(|p| format!("{} {}", p.status, p.code))?;
                    Ok(json!({ "applied": applied }))
                }
            })
            .await
        }
        other => Err(format!("SACI_FUNCTION desconhecida: {other}").into()),
    }
}
