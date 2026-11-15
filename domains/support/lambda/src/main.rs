//! `saci-support`: as funções do domínio `support` (ADR 0003).
//!
//! | `SACI_FUNCTION` | Gatilho | O que roda |
//! | --- | --- | --- |
//! | `api` | API Gateway | `/v1/support` |
//! | `internal` | Invocação direta | chamados abertos (painel) |
//! | `handoffs` | Fila de `assistant.handoff.requested.v1` | abre o chamado |
//! | `relay` | DynamoDB Streams | *outbox* → EventBridge |
//! | `migrate` | Terraform | chamados de exemplo (fora de produção) |

use std::sync::Arc;

use lambda_runtime::Error;
use saci_commerce_kit::aws::dynamodb::DynamoStore;
use saci_commerce_kit::aws::lambda::{
    cursors, init_telemetry, run_consumer, run_http, run_internal, run_relay, run_task,
};
use saci_commerce_kit::aws::sdk_config;
use saci_commerce_kit::clock::{Clock, SystemClock};
use saci_support_api::{Handoffs, Support};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let function = saci_commerce_kit::env("SACI_FUNCTION")?;
    init_telemetry(&format!("support-{function}"));
    let config = sdk_config().await;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = Arc::new(DynamoStore::from_env(&config, "TABLE")?);
    let support = Support {
        store: store.clone(),
        internal: saci_commerce_sandbox::boot::internal_client(&config)?,
        clock: Arc::clone(&clock),
        cursors: cursors(&config).await?,
    };
    match function.as_str() {
        "api" => run_http(Arc::new(support)).await,
        "internal" => run_internal(Arc::new(support)).await,
        "handoffs" => run_consumer(store, vec![Arc::new(Handoffs(support))], clock).await,
        "relay" => run_relay(&config).await,
        "migrate" => {
            let fixtures = saci_commerce_kit::env_or("SACI_SEED_FIXTURES", "false") == "true";
            let support = Arc::new(support);
            run_task(move |_| {
                let support = Arc::clone(&support);
                async move {
                    let applied = saci_support_api::migrate(&support, fixtures)
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
