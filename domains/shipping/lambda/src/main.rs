//! `saci-shipping`: as funções do domínio `shipping` (ADR 0003).
//!
//! | `SACI_FUNCTION` | Gatilho | O que roda |
//! | --- | --- | --- |
//! | `api` | API Gateway | `/v1/shipping` |
//! | `internal` | Invocação direta | CEP e cotação para `sellers` e `checkout` |
//! | `labels` | Fila de `orders.sale.ready/shipped.v1` | compra a etiqueta, guarda o rastreio |
//! | `hooks` | API regional com mTLS (`hooks.`) | `carrier-webhooks` |
//! | `relay` | DynamoDB Streams | *outbox* → EventBridge |
//!
//! Variáveis: `TABLE`, `FILES_BUCKET`, `FILES_BASE_URL`, `FILES_KEY_PAIR_ID` e
//! `FILES_PRIVATE_KEY_PARAM` (URL assinada do CloudFront; sem elas, assinatura de
//! desenvolvimento), `CARRIER_SECRETS_PARAM` (`provedor=segredo,...`), `EVENT_BUS`.

use std::sync::Arc;

use lambda_runtime::Error;
use saci_carrier_webhooks::CarrierWebhooks;
use saci_commerce_kit::aws::cloudfront::CloudFrontSigner;
use saci_commerce_kit::aws::dynamodb::DynamoStore;
use saci_commerce_kit::aws::lambda::{
    init_telemetry, run_consumer, run_http, run_internal, run_relay,
};
use saci_commerce_kit::aws::s3::S3Objects;
use saci_commerce_kit::aws::{sdk_config, ssm};
use saci_commerce_kit::clock::{Clock, SystemClock};
use saci_commerce_kit::objects::{DevUrlSigner, UrlSigner};
use saci_shipping_api::carrier::{TableCarrier, TableDirectory};
use saci_shipping_api::{LabelBuyer, Shipping};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let function = saci_commerce_kit::env("SACI_FUNCTION")?;
    init_telemetry(&format!("shipping-{function}"));
    let config = sdk_config().await;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = Arc::new(DynamoStore::from_env(&config, "TABLE")?);
    if function == "hooks" {
        let raw = ssm::secret(&config, "CARRIER_SECRETS").await?;
        let secrets = raw
            .split(',')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().as_bytes().to_vec()))
            .collect();
        return run_http(Arc::new(CarrierWebhooks {
            store,
            clock,
            secrets,
        }))
        .await;
    }
    let files_base = saci_commerce_kit::env_or("FILES_BASE_URL", "https://files.saci.art.br");
    let signer: Arc<dyn UrlSigner> = match (
        std::env::var("FILES_KEY_PAIR_ID"),
        std::env::var("FILES_PRIVATE_KEY_PARAM"),
    ) {
        (Ok(key_pair), Ok(param)) => Arc::new(CloudFrontSigner::new(
            &files_base,
            &key_pair,
            &ssm::parameter(&config, &param).await?,
        )?),
        _ => Arc::new(DevUrlSigner::new(
            &files_base,
            saci_commerce_kit::env_or("SACI_ENVIRONMENT", "local").as_bytes(),
        )),
    };
    let shipping = Shipping {
        store: store.clone(),
        internal: saci_commerce_sandbox::boot::internal_client(&config)?,
        clock: Arc::clone(&clock),
        directory: Arc::new(TableDirectory),
        carrier: Arc::new(TableCarrier),
        files: Arc::new(S3Objects::new(
            &config,
            saci_commerce_kit::env("FILES_BUCKET")?,
        )),
        signer,
    };
    match function.as_str() {
        "api" => run_http(Arc::new(shipping)).await,
        "internal" => run_internal(Arc::new(shipping)).await,
        "labels" => run_consumer(store, vec![Arc::new(LabelBuyer(shipping))], clock).await,
        "relay" => run_relay(&config).await,
        other => Err(format!("SACI_FUNCTION desconhecida: {other}").into()),
    }
}
