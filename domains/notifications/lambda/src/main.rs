//! `saci-notifications`: as funções do domínio `notifications` (ADR 0003).
//!
//! | `SACI_FUNCTION` | Gatilho | O que roda |
//! | --- | --- | --- |
//! | `api` | API Gateway | `/v1/notifications` |
//! | `dispatcher` | Fila de eventos (pedidos, vendas, chamados, repasses) | um aviso por aparelho na fila de envio |
//! | `push-sender` | Fila de envio (lotes de até 10) | envio ao Expo Push |
//!
//! Variáveis: `TABLE`, `PUSH_QUEUE_URL`, `PUSH_PROVIDER` (`log` até a saída para
//! `exp.host` ser liberada no firewall da plataforma), `SACI_CURSOR_KEY_PARAM`,
//! `INTERNAL_<ALVO>_FUNCTION`.

use std::sync::Arc;

use aws_lambda_events::event::sqs::{SqsBatchResponse, SqsEvent};
use lambda_runtime::{Error, LambdaEvent, service_fn};
use saci_commerce_kit::BoxFut;
use saci_commerce_kit::aws::dynamodb::DynamoStore;
use saci_commerce_kit::aws::lambda::{init_telemetry, run_consumer, run_http};
use saci_commerce_kit::aws::{sdk_config, ssm};
use saci_commerce_kit::clock::{Clock, SystemClock};
use saci_notifications_api::Notifications;
use saci_notifications_dispatcher::{
    Dispatcher, LogSender, Push, PushQueue, PushSender, send_batch,
};

/// Fila de envio no SQS.
#[derive(Debug, Clone)]
struct SqsQueue {
    client: aws_sdk_sqs::Client,
    url: String,
}

impl PushQueue for SqsQueue {
    fn enqueue(&self, pushes: Vec<Push>) -> BoxFut<'_, Result<(), String>> {
        Box::pin(async move {
            for chunk in pushes.chunks(10) {
                let mut batch = self.client.send_message_batch().queue_url(&self.url);
                for (i, p) in chunk.iter().enumerate() {
                    let body = serde_json::to_string(p).map_err(|e| e.to_string())?;
                    let entry = aws_sdk_sqs::types::SendMessageBatchRequestEntry::builder()
                        .id(i.to_string())
                        .message_body(body)
                        .build()
                        .map_err(|e| e.to_string())?;
                    batch = batch.entries(entry);
                }
                let out = batch
                    .send()
                    .await
                    .map_err(|e| e.into_service_error().to_string())?;
                if !out.failed().is_empty() {
                    return Err(format!(
                        "{} aviso(s) recusado(s) pela fila",
                        out.failed().len()
                    ));
                }
            }
            Ok(())
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let function = saci_commerce_kit::env("SACI_FUNCTION")?;
    init_telemetry(&format!("notifications-{function}"));
    let config = sdk_config().await;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = Arc::new(DynamoStore::from_env(&config, "TABLE")?);
    let notifications = Notifications {
        store: store.clone(),
        clock: Arc::clone(&clock),
        hash_key: ssm::secret(&config, "SACI_CURSOR_KEY").await?.into_bytes(),
    };
    match function.as_str() {
        "api" => run_http(Arc::new(notifications)).await,
        "dispatcher" => {
            let queue = SqsQueue {
                client: aws_sdk_sqs::Client::new(&config),
                url: saci_commerce_kit::env("PUSH_QUEUE_URL")?,
            };
            let dispatcher = Dispatcher {
                notifications,
                internal: saci_commerce_sandbox::boot::internal_client(&config)?,
                queue: Arc::new(queue),
            };
            run_consumer(store, vec![Arc::new(dispatcher)], clock).await
        }
        "push-sender" => {
            // Hoje só o registro: o envio ao Expo entra quando a saída para a internet existir.
            let sender: Arc<dyn PushSender> = Arc::new(LogSender);
            let notifications = Arc::new(notifications);
            lambda_runtime::run(service_fn(move |event: LambdaEvent<SqsEvent>| {
                let (sender, notifications) = (Arc::clone(&sender), Arc::clone(&notifications));
                async move {
                    let mut ids = Vec::new();
                    let mut pushes = Vec::new();
                    for r in event.payload.records {
                        if let Some(p) = r
                            .body
                            .as_deref()
                            .and_then(|b| serde_json::from_str::<Push>(b).ok())
                        {
                            ids.push(r.message_id.unwrap_or_default());
                            pushes.push(p);
                        }
                    }
                    let retry = send_batch(sender.as_ref(), &notifications, &pushes).await;
                    let mut response = SqsBatchResponse::default();
                    for i in retry {
                        response.add_failure(ids[i].clone());
                    }
                    Ok::<_, Error>(response)
                }
            }))
            .await
        }
        other => Err(format!("SACI_FUNCTION desconhecida: {other}").into()),
    }
}
