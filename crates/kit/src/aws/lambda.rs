//! Entradas de Lambda. Cada função de um domínio é uma destas, escolhida pela
//! variável `SACI_FUNCTION` no binário do domínio (ADR 0003):
//!
//! | Entrada | Gatilho | Resposta |
//! | --- | --- | --- |
//! | [`run_http`] | API Gateway REST (proxy) | HTTP, com o contexto do autorizador |
//! | [`run_consumer`] | Fila SQS da assinatura no EventBridge | Falha parcial por mensagem (`ReportBatchItemFailures`) |
//! | [`run_internal`] | Invocação direta de outro domínio | `{"ok": ...}` ou `{"error": ...}` |
//! | [`run_relay`] | DynamoDB Streams da tabela do serviço | Falha parcial (`saci_outbox::relay`) |
//! | [`run_scheduled`] | Regra agendada do EventBridge | — |

use std::collections::HashMap;
use std::sync::Arc;

use aws_lambda_events::event::sqs::{SqsBatchResponse, SqsEvent};
use lambda_http::{Body, RequestExt};
use lambda_runtime::{Error, LambdaEvent, service_fn};
use saci_events::CloudEvent;
use serde_json::Value;

use crate::clock::Clock;
use crate::events::{Consumer, Event, deliver};
use crate::http::{HttpService, Request};
use crate::internal::{InternalHandler, InternalRequest, dispatch};
use crate::store::DocumentStore;

/// Inicia a telemetria (logs JSON mascarados) com o nome da função.
pub fn init_telemetry(name: &str) {
    saci_telemetry::init(name);
}

/// Converte a requisição do API Gateway REST na [`Request`] do kit.
#[must_use]
pub fn to_request(event: &lambda_http::Request) -> Request {
    let mut query = Vec::new();
    for (k, _) in event
        .query_string_parameters_ref()
        .map(|q| q.iter().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        if let Some(values) = event.query_string_parameters_ref().and_then(|q| q.all(k)) {
            for v in values {
                query.push((k.to_owned(), v.to_owned()));
            }
        }
    }
    query.dedup();
    let headers: HashMap<String, String> = event
        .headers()
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|v| (k.as_str().to_ascii_lowercase(), v.to_owned()))
        })
        .collect();
    let source_ip = match event.request_context_ref() {
        Some(lambda_http::request::RequestContext::ApiGatewayV1(ctx)) => {
            ctx.identity.source_ip.clone()
        }
        #[allow(unreachable_patterns)]
        _ => None,
    };
    Request {
        method: event.method().as_str().to_owned(),
        path: crate::http::percent_decode(event.raw_http_path()),
        query,
        headers,
        body: event.body().as_ref().to_vec(),
        auth: saci_authz::lambda::auth_context(event).ok(),
        request_id: saci_authz::lambda::request_id(event).unwrap_or_else(|| "req_unknown".into()),
        source_ip,
    }
}

/// Função HTTP atrás do API Gateway.
pub async fn run_http(service: Arc<dyn HttpService>) -> Result<(), Error> {
    lambda_http::run(lambda_http::service_fn(
        move |event: lambda_http::Request| {
            let service = Arc::clone(&service);
            async move {
                let request = to_request(&event);
                let response = service.handle(request).await;
                let mut builder = lambda_http::Response::builder().status(response.status);
                for (k, v) in &response.headers {
                    builder = builder.header(k, v);
                }
                let body = if response.body.is_empty() {
                    Body::Empty
                } else {
                    Body::Text(response.body)
                };
                Ok::<_, Error>(builder.body(body)?)
            }
        },
    ))
    .await
}

/// Função consumidora: fila SQS ligada à regra do EventBridge (módulo
/// `event-subscription` da plataforma). O corpo de cada mensagem é o evento
/// do EventBridge, com o CloudEvent em `detail`.
pub async fn run_consumer(
    store: Arc<dyn DocumentStore>,
    consumers: Vec<Arc<dyn Consumer>>,
    clock: Arc<dyn Clock>,
) -> Result<(), Error> {
    lambda_runtime::run(service_fn(move |event: LambdaEvent<SqsEvent>| {
        let (store, consumers, clock) = (Arc::clone(&store), consumers.clone(), Arc::clone(&clock));
        async move {
            let mut response = SqsBatchResponse::default();
            for record in event.payload.records {
                let id = record.message_id.clone().unwrap_or_default();
                let parsed = record
                    .body
                    .as_deref()
                    .and_then(|b| serde_json::from_str::<Value>(b).ok())
                    .and_then(|v| v.get("detail").cloned())
                    .and_then(|d| serde_json::from_value::<Event>(d).ok());
                let Some(cloud_event) = parsed else {
                    tracing::error!(message = %id, "mensagem sem CloudEvent em `detail`");
                    // Repetir não conserta: deixa ir para a DLQ pela contagem de recebimentos.
                    response.add_failure(id);
                    continue;
                };
                for consumer in &consumers {
                    if deliver(
                        store.as_ref(),
                        consumer.as_ref(),
                        &cloud_event,
                        clock.epoch(),
                    )
                    .await
                    .is_err()
                    {
                        response.add_failure(id.clone());
                        break;
                    }
                }
            }
            Ok::<_, Error>(response)
        }
    }))
    .await
}

/// Função interna: atende chamadas síncronas de outros domínios.
pub async fn run_internal(handler: Arc<dyn InternalHandler>) -> Result<(), Error> {
    lambda_runtime::run(service_fn(move |event: LambdaEvent<InternalRequest>| {
        let handler = Arc::clone(&handler);
        async move { Ok::<_, Error>(dispatch(handler.as_ref(), event.payload).await) }
    }))
    .await
}

/// Função de retransmissão do *outbox*: stream da tabela → barramento.
pub async fn run_relay(config: &aws_config::SdkConfig) -> Result<(), Error> {
    let bus = crate::env("EVENT_BUS").map_err(Error::from)?;
    let publisher = Arc::new(saci_events::eventbridge::EventBridgePublisher::new(
        aws_sdk_eventbridge::Client::new(config),
        bus,
    ));
    lambda_runtime::run(service_fn(move |event: LambdaEvent<saci_outbox::StreamEvent>| {
        let publisher = Arc::clone(&publisher);
        async move { Ok::<_, Error>(saci_outbox::relay(event.payload, publisher.as_ref()).await) }
    }))
    .await
}

/// Função agendada (varreduras). O corpo do evento é ignorado.
pub async fn run_scheduled<F, Fut>(task: F) -> Result<(), Error>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + Send,
{
    let task = Arc::new(task);
    lambda_runtime::run(service_fn(move |_: LambdaEvent<Value>| {
        let task = Arc::clone(&task);
        async move { task().await.map_err(Error::from) }
    }))
    .await
}

/// Publica eventos direto no barramento (sem *outbox*): só para quem não tem
/// tabela própria, como a esteira de imagens ao rejeitar um envio.
pub async fn publish_direct(
    config: &aws_config::SdkConfig,
    events: &[CloudEvent<Value>],
) -> Result<(), String> {
    use saci_events::EventPublisher;
    let bus = crate::env("EVENT_BUS")?;
    saci_events::eventbridge::EventBridgePublisher::new(
        aws_sdk_eventbridge::Client::new(config),
        bus,
    )
    .publish(events)
    .await
    .map_err(|e| e.to_string())
}

/// Função de tarefa avulsa (migração de dados, reindexação): recebe um JSON,
/// devolve um JSON. Invocada pelo Terraform (`aws_lambda_invocation`) ou à mão.
pub async fn run_task<F, Fut>(task: F) -> Result<(), Error>
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, String>> + Send,
{
    let task = Arc::new(task);
    lambda_runtime::run(service_fn(move |event: LambdaEvent<Value>| {
        let task = Arc::clone(&task);
        async move { task(event.payload).await.map_err(Error::from) }
    }))
    .await
}

/// Assinador de cursores com a chave do SSM (`SACI_CURSOR_KEY_PARAM`) ou da
/// variável `SACI_CURSOR_KEY` (só local).
pub async fn cursors(config: &aws_config::SdkConfig) -> Result<crate::cursor::CursorSigner, Error> {
    let key = crate::aws::ssm::secret(config, "SACI_CURSOR_KEY")
        .await
        .map_err(Error::from)?;
    Ok(crate::cursor::CursorSigner::new(key.as_bytes()))
}

/// Liga a autenticação opcional (`aws::optional_auth`) na frente de um serviço HTTP.
#[derive(Debug)]
pub struct WithOptionalAuth {
    /// O serviço.
    pub inner: Arc<dyn HttpService>,
    /// O autenticador.
    pub auth: crate::aws::optional_auth::OptionalAuth,
}

impl HttpService for WithOptionalAuth {
    fn handle(&self, mut req: Request) -> crate::BoxFut<'_, crate::http::Response> {
        Box::pin(async move {
            let now = crate::clock::Clock::epoch(&crate::clock::SystemClock);
            if self.auth.authenticate(&mut req, now).await.is_err() {
                return crate::http::finish(
                    Err(crate::problem::problem("unauthorized")),
                    &req.request_id,
                );
            }
            self.inner.handle(req).await
        })
    }
}
