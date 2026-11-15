//! `saci-reviews`: as funções do domínio `reviews` (ADR 0003).
//!
//! | `SACI_FUNCTION` | Gatilho | O que roda |
//! | --- | --- | --- |
//! | `api` | API Gateway | `/v1/reviews` |
//! | `migrate` | Terraform | avaliações de exemplo (fora de produção) |
//!
//! Variáveis: `TABLE`, `SACI_CURSOR_KEY_PARAM`, `SACI_ENVIRONMENT`, `SACI_SEED_FIXTURES`,
//! `INTERNAL_<ALVO>_FUNCTION`.

use std::sync::Arc;

use lambda_runtime::Error;
use saci_commerce_kit::aws::dynamodb::DynamoStore;
use saci_commerce_kit::aws::lambda::{cursors, init_telemetry, run_http, run_task};
use saci_commerce_kit::aws::sdk_config;
use saci_commerce_kit::clock::SystemClock;
use saci_reviews_api::Reviews;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let function = saci_commerce_kit::env("SACI_FUNCTION")?;
    init_telemetry(&format!("reviews-{function}"));
    let config = sdk_config().await;
    let reviews = Reviews {
        store: Arc::new(DynamoStore::from_env(&config, "TABLE")?),
        internal: saci_commerce_sandbox::boot::internal_client(&config)?,
        clock: Arc::new(SystemClock),
        cursors: cursors(&config).await?,
    };
    match function.as_str() {
        "api" => run_http(Arc::new(reviews)).await,
        "migrate" => {
            let fixtures = saci_commerce_kit::env_or("SACI_SEED_FIXTURES", "false") == "true";
            let reviews = Arc::new(reviews);
            run_task(move |_| {
                let reviews = Arc::clone(&reviews);
                async move {
                    let applied = saci_reviews_api::migrate(&reviews, fixtures)
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
