//! `saci-customers`: as funções do domínio `customers` (ADR 0003).
//!
//! | `SACI_FUNCTION` | Gatilho | O que roda |
//! | --- | --- | --- |
//! | `api` | API Gateway | `/v1/customers` |
//! | `profiles` | Fila de `identity.account.verified.v1` | cria o perfil |
//!
//! Variáveis: `TABLE`, `SACI_CURSOR_KEY_PARAM`, `SACI_ENVIRONMENT`, `INTERNAL_<ALVO>_FUNCTION`.

use std::sync::Arc;

use lambda_runtime::Error;
use saci_commerce_kit::aws::dynamodb::DynamoStore;
use saci_commerce_kit::aws::lambda::{cursors, init_telemetry, run_consumer, run_http};
use saci_commerce_kit::aws::sdk_config;
use saci_commerce_kit::clock::{Clock, SystemClock};
use saci_customers_api::{Customers, ProfileCreator};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let function = saci_commerce_kit::env("SACI_FUNCTION")?;
    init_telemetry(&format!("customers-{function}"));
    let config = sdk_config().await;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = Arc::new(DynamoStore::from_env(&config, "TABLE")?);
    let signer = cursors(&config).await?;
    // A digital do e-mail das novidades usa a mesma chave secreta do serviço.
    let hash_key = saci_commerce_kit::aws::ssm::secret(&config, "SACI_CURSOR_KEY")
        .await?
        .into_bytes();
    let customers = Customers {
        store: store.clone(),
        internal: saci_commerce_sandbox::boot::internal_client(&config)?,
        clock: Arc::clone(&clock),
        cursors: signer,
        hash_key,
    };
    match function.as_str() {
        "api" => run_http(Arc::new(customers)).await,
        "profiles" => run_consumer(store, vec![Arc::new(ProfileCreator(customers))], clock).await,
        other => Err(format!("SACI_FUNCTION desconhecida: {other}").into()),
    }
}
