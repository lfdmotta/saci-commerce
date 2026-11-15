//! Eventos entre serviços (seção 3.2): CloudEvents só com ids, publicados pelo
//! *outbox* ([`crate::store::WriteOp::Publish`]) e consumidos com deduplicação.
//!
//! Um consumidor implementa [`Consumer`]. O mesmo consumidor roda:
//!
//! - na nuvem, numa Lambda ligada à fila SQS da sua assinatura no EventBridge
//!   (`aws::lambda::run_consumer`), com falha parcial por mensagem;
//! - no servidor de desenvolvimento, chamado em processo logo depois da escrita.
//!
//! A entrega é "pelo menos uma vez": todo consumidor passa por [`first_delivery`]
//! antes de agir, que grava `EVT#<consumidor>`/`<id do evento>` com escrita
//! condicional e TTL de 7 dias.

use std::fmt;

use saci_events::CloudEvent;
use saci_problem::Problem;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::BoxFut;
use crate::problem::internal;
use crate::store::{DocumentStore, Item, StoreError, WriteOp};

/// Evento no formato do barramento.
pub type Event = CloudEvent<Value>;

/// Por quanto tempo um id de evento processado é lembrado.
pub const DEDUP_SECS: u64 = 7 * 24 * 3600;

/// Monta e valida um evento (`source` = `saci.<domínio>`; `type` versionado;
/// `data` sem dado pessoal). Recusa vira 500: é erro de programação.
pub fn event(
    domain: &str,
    event_type: &str,
    subject: &str,
    data: &impl Serialize,
) -> Result<Event, Problem> {
    let data = serde_json::to_value(data).map_err(|e| internal(&e))?;
    CloudEvent::new(&format!("saci.{domain}"), event_type, Some(subject), data)
        .and_then(CloudEvent::into_json)
        .map_err(|e| internal(&e))
}

/// Lê o `data` tipado de um evento.
pub fn data<T: DeserializeOwned>(event: &Event) -> Result<T, ConsumeError> {
    serde_json::from_value(event.data.clone())
        .map_err(|e| ConsumeError::Malformed(format!("{}: {e}", event.event_type)))
}

/// Falha ao consumir.
#[derive(Debug, thiserror::Error)]
pub enum ConsumeError {
    /// O evento não tem o formato esperado: repetir não resolve (vai para a DLQ e alerta).
    #[error("evento malformado: {0}")]
    Malformed(String),
    /// Falha passageira: a mensagem volta para a fila.
    #[error("falha passageira: {0}")]
    Retry(String),
}

impl From<StoreError> for ConsumeError {
    fn from(e: StoreError) -> Self {
        Self::Retry(e.to_string())
    }
}

impl From<Problem> for ConsumeError {
    fn from(p: Problem) -> Self {
        Self::Retry(format!("{} {}", p.status, p.code))
    }
}

/// Um consumidor de eventos.
pub trait Consumer: Send + Sync + fmt::Debug {
    /// Nome estável (entra na chave de deduplicação).
    fn name(&self) -> &'static str;

    /// Tipos que este consumidor trata (`orders.sale.ready.v1`). Os outros são ignorados.
    fn accepts(&self) -> &'static [&'static str];

    /// Trata um evento. Só é chamado na primeira entrega de cada `id`.
    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>>;
}

/// Registra a entrega e diz se é a primeira. Repetida: o consumidor ignora.
pub async fn first_delivery(
    store: &dyn DocumentStore,
    consumer: &str,
    event_id: &str,
    now: u64,
) -> Result<bool, StoreError> {
    let item =
        Item::new(format!("EVT#{consumer}"), event_id, &Value::Null)?.expiring(now + DEDUP_SECS);
    match store.write(vec![WriteOp::create(item)]).await {
        Ok(()) => Ok(true),
        Err(StoreError::Conflict) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Entrega um evento a um consumidor, com deduplicação.
///
/// Se o tratamento falhar, a marca de entrega é desfeita para a repetição
/// funcionar.
pub async fn deliver(
    store: &dyn DocumentStore,
    consumer: &dyn Consumer,
    event: &Event,
    now: u64,
) -> Result<(), ConsumeError> {
    if !consumer.accepts().contains(&event.event_type.as_str()) {
        return Ok(());
    }
    if !first_delivery(store, consumer.name(), &event.id, now).await? {
        tracing::info!(consumer = consumer.name(), event = %event.id, "evento repetido ignorado");
        return Ok(());
    }
    let result = consumer.handle(event).await;
    if let Err(e) = &result {
        tracing::warn!(consumer = consumer.name(), event = %event.id, error = %e, "falha ao consumir");
        let _ = store
            .write(vec![WriteOp::delete(
                format!("EVT#{}", consumer.name()),
                event.id.clone(),
            )])
            .await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct Counter(AtomicUsize);

    impl Consumer for Counter {
        fn name(&self) -> &'static str {
            "counter"
        }
        fn accepts(&self) -> &'static [&'static str] {
            &["orders.sale.ready.v1"]
        }
        fn handle<'a>(&'a self, _event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn deduplicates_by_event_id() -> Result<(), ConsumeError> {
        let store =
            MemoryStore::new().with_clock(std::sync::Arc::new(crate::clock::FixedClock::at(10)));
        let consumer = Counter::default();
        let ev = event(
            "orders",
            "orders.sale.ready.v1",
            "sal_1",
            &json!({ "sale_id": "sal_1" }),
        )?;
        deliver(&store, &consumer, &ev, 10).await?;
        deliver(&store, &consumer, &ev, 11).await?;
        assert_eq!(consumer.0.load(Ordering::SeqCst), 1);
        let other = event("orders", "orders.sale.shipped.v1", "sal_1", &json!({}))?;
        deliver(&store, &consumer, &other, 12).await?;
        assert_eq!(consumer.0.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn rejects_personal_data_in_events() {
        assert!(
            event(
                "customers",
                "customers.profile.updated.v1",
                "acc_1",
                &json!({ "email": "a@b.com" })
            )
            .is_err()
        );
    }
}
