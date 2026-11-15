//! `saci-sellers`: as funções do domínio `sellers` (ADR 0003).
//!
//! | `SACI_FUNCTION` | Gatilho | O que roda |
//! | --- | --- | --- |
//! | `api` | API Gateway | `/v1/sellers` |
//! | `internal` | Invocação direta | perfil, endereço, resumo e equipe de uma vitrine |
//! | `workspace` | Fila de eventos de catálogo, pedidos, chamados e contas | contagens do painel, convites pendentes |
//! | `relay` | DynamoDB Streams | *outbox* → EventBridge |
//! | `migrate` | Terraform | pares de letras e vitrines de lançamento |
//!
//! Variáveis: `TABLE`, `AUTHZ_TABLE` (tabela `authz` da plataforma), `CDN_BASE_URL`,
//! `SACI_ENVIRONMENT`, `SACI_SEED_FIXTURES`, `INTERNAL_<ALVO>_FUNCTION`, `EVENT_BUS`.

use std::sync::Arc;

use lambda_runtime::Error;
use saci_commerce_kit::aws::authz::AuthzGrants;
use saci_commerce_kit::aws::dynamodb::{Client, DynamoStore};
use saci_commerce_kit::aws::lambda::{
    init_telemetry, run_consumer, run_http, run_internal, run_relay, run_task,
};
use saci_commerce_kit::aws::sdk_config;
use saci_commerce_kit::clock::{Clock, SystemClock};
use saci_sellers_api::{Sellers, WorkspaceProjector};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let function = saci_commerce_kit::env("SACI_FUNCTION")?;
    init_telemetry(&format!("sellers-{function}"));
    let config = sdk_config().await;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = Arc::new(DynamoStore::from_env(&config, "TABLE")?);
    let sellers = Sellers {
        store: store.clone(),
        grants: Arc::new(AuthzGrants::new(
            Client::new(&config),
            saci_commerce_kit::env_or("AUTHZ_TABLE", "saci-local-authz"),
        )),
        internal: saci_commerce_sandbox::boot::internal_client(&config)?,
        clock: Arc::clone(&clock),
        cdn_base_url: saci_commerce_kit::env_or("CDN_BASE_URL", "https://cdn.saci.art.br"),
    };
    match function.as_str() {
        "api" => run_http(Arc::new(sellers)).await,
        "internal" => run_internal(Arc::new(sellers)).await,
        "workspace" => {
            run_consumer(store, vec![Arc::new(WorkspaceProjector(sellers))], clock).await
        }
        "relay" => run_relay(&config).await,
        "migrate" => {
            let fixtures = saci_commerce_kit::env_or("SACI_SEED_FIXTURES", "false") == "true";
            let sellers = Arc::new(sellers);
            run_task(move |_| {
                let sellers = Arc::clone(&sellers);
                async move {
                    let applied = saci_sellers_api::seed::migrate(&sellers, fixtures)
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
