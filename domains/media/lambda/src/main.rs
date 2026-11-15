//! `saci-media`: as funções do domínio `media` (ADR 0003).
//!
//! | `SACI_FUNCTION` | Gatilho | O que roda |
//! | --- | --- | --- |
//! | `api` | API Gateway | `/v1/media` |
//! | `internal` | Invocação direta | estado dos envios para `catalog` e `sellers` |
//! | `pipeline` | Evento do S3 (quarentena) ou resultado do GuardDuty Malware Protection | esteira de imagens |
//! | `relay` | DynamoDB Streams | *outbox* → EventBridge |
//!
//! Variáveis: `TABLE`, `QUARANTINE_BUCKET`, `CDN_BUCKET`, `CDN_BASE_URL`,
//! `MALWARE_SCAN` (`guardduty` ou `none`), `EVENT_BUS` (relay).

use std::sync::Arc;

use lambda_runtime::{Error, LambdaEvent, service_fn};
use saci_commerce_kit::aws::dynamodb::DynamoStore;
use saci_commerce_kit::aws::lambda::{init_telemetry, run_http, run_internal, run_relay};
use saci_commerce_kit::aws::s3::S3Objects;
use saci_commerce_kit::aws::sdk_config;
use saci_commerce_kit::clock::SystemClock;
use saci_media_api::Media;
use saci_media_pipeline::Pipeline;
use serde_json::Value;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let function = saci_commerce_kit::env("SACI_FUNCTION")?;
    init_telemetry(&format!("media-{function}"));
    let config = sdk_config().await;
    let quarantine = Arc::new(S3Objects::new(
        &config,
        saci_commerce_kit::env("QUARANTINE_BUCKET")?,
    ));
    let media = Media {
        store: Arc::new(DynamoStore::from_env(&config, "TABLE")?),
        signer: quarantine.clone(),
        clock: Arc::new(SystemClock),
    };
    match function.as_str() {
        "api" => run_http(Arc::new(media)).await,
        "internal" => run_internal(Arc::new(media)).await,
        "relay" => run_relay(&config).await,
        "pipeline" => {
            let pipeline = Arc::new(Pipeline {
                media,
                quarantine,
                cdn: Arc::new(S3Objects::new(
                    &config,
                    saci_commerce_kit::env("CDN_BUCKET")?,
                )),
                cdn_base_url: saci_commerce_kit::env_or("CDN_BASE_URL", "https://cdn.saci.art.br"),
            });
            let guardduty = saci_commerce_kit::env_or("MALWARE_SCAN", "none") == "guardduty";
            lambda_runtime::run(service_fn(move |event: LambdaEvent<Value>| {
                let pipeline = Arc::clone(&pipeline);
                async move {
                    for (key, malware) in objects(&event.payload, guardduty) {
                        if let Err(p) = pipeline.process(&key, malware).await {
                            // Falha passageira: a invocação falha e o gatilho tenta de novo.
                            return Err(Error::from(format!("{} {}", p.status, p.code)));
                        }
                    }
                    Ok::<_, Error>(())
                }
            }))
            .await
        }
        other => Err(format!("SACI_FUNCTION desconhecida: {other}").into()),
    }
}

/// Chaves dos objetos e o veredito de malware de um evento.
///
/// - Notificação do S3 (`Records[].s3.object.key`): sem verificação (local), ou
///   ignorada quando a verificação é do GuardDuty (o resultado chega depois).
/// - Resultado do GuardDuty Malware Protection (EventBridge): `detail.s3ObjectDetails.objectKey`
///   e `detail.scanResultDetails.scanResultStatus` (`NO_THREATS_FOUND` ou `THREATS_FOUND`).
fn objects(event: &Value, guardduty: bool) -> Vec<(String, Option<bool>)> {
    if let Some(detail) = event.get("detail") {
        let key = detail["s3ObjectDetails"]["objectKey"]
            .as_str()
            .unwrap_or_default();
        let status = detail["scanResultDetails"]["scanResultStatus"]
            .as_str()
            .unwrap_or_default();
        return match status {
            "NO_THREATS_FOUND" => vec![(key.to_owned(), Some(false))],
            // Ameaça, ou sem veredito (tipo não suportado, falha): recusa por segurança.
            _ => vec![(key.to_owned(), Some(true))],
        };
    }
    if guardduty {
        return Vec::new();
    }
    event["Records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["s3"]["object"]["key"].as_str())
        .map(|k| (saci_commerce_kit::http::percent_decode(k), None))
        .collect()
}
