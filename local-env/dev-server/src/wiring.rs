//! Monta todos os serviços com os adaptadores em memória, como as Lambdas os
//! montam com os adaptadores da AWS (`domains/<domínio>/lambda`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use saci_carrier_webhooks::CarrierWebhooks;
use saci_catalog_api::Catalog;
use saci_checkout_saga::{Checkout, PaymentEvents};
use saci_commerce_kit::clock::{Clock, SystemClock};
use saci_commerce_kit::cursor::CursorSigner;
use saci_commerce_kit::events::{Consumer, Event, deliver};
use saci_commerce_kit::grants::MemoryGrants;
use saci_commerce_kit::http::HttpService;
use saci_commerce_kit::internal::LocalInternal;
use saci_commerce_kit::jwt::Es256Key;
use saci_commerce_kit::objects::{DevUploadSigner, DevUrlSigner, MemoryObjects};
use saci_commerce_kit::store::{DocumentStore, EventSink, MemoryStore};
use saci_commerce_sandbox::Sandbox;
use saci_commerce_sandbox::identity::IdentityHttp;
use saci_commerce_sandbox::payments::PaymentsHttp;
use saci_customers_api::{Customers, ProfileCreator};
use saci_media_api::Media;
use saci_media_pipeline::Pipeline;
use saci_notifications_api::Notifications;
use saci_notifications_dispatcher::{Dispatcher, InlineQueue, LogSender};
use saci_orders_api::{Orders, SaleProgress};
use saci_reviews_api::Reviews;
use saci_search_indexer::{Indexer, Search};
use saci_sellers_api::{Sellers, WorkspaceProjector};
use saci_shipping_api::carrier::{TableCarrier, TableDirectory};
use saci_shipping_api::{LabelBuyer, Shipping};
use saci_support_api::{Handoffs, Support};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Emissor e audiência das sessões do sandbox (os mesmos do ambiente local da plataforma).
pub(crate) const USER_ISSUER: &str = "https://auth.local.saci";
/// Audiência das sessões.
pub(crate) const USER_AUDIENCE: &str = "saci-local";
/// Emissor do guest token.
pub(crate) const GUEST_ISSUER: &str = "https://checkout.local.saci";
/// Audiência do guest token.
pub(crate) const GUEST_AUDIENCE: &str = "saci-api";

/// Barramento em processo: as escritas publicam aqui, um trabalhador entrega.
#[derive(Debug, Clone)]
pub(crate) struct Bus(UnboundedSender<Event>);

impl EventSink for Bus {
    fn publish(&self, events: Vec<Event>) {
        for e in events {
            tracing::info!(event = %e.event_type, subject = e.subject.as_deref().unwrap_or(""), "evento");
            let _ = self.0.send(e);
        }
    }
}

/// Tudo o que o servidor atende.
#[derive(Debug)]
pub(crate) struct App {
    /// Rotas HTTP por prefixo.
    pub catalog: Arc<dyn HttpService>,
    /// Busca.
    pub search: Arc<dyn HttpService>,
    /// Vitrines.
    pub sellers: Arc<dyn HttpService>,
    /// Clientes.
    pub customers: Arc<dyn HttpService>,
    /// Avaliações.
    pub reviews: Arc<dyn HttpService>,
    /// Frete.
    pub shipping: Arc<dyn HttpService>,
    /// Webhooks da transportadora.
    pub carrier_hooks: Arc<dyn HttpService>,
    /// Mídia.
    pub media: Arc<dyn HttpService>,
    /// Compra.
    pub checkout: Arc<dyn HttpService>,
    /// Pedidos.
    pub orders: Arc<dyn HttpService>,
    /// Atendimento.
    pub support: Arc<dyn HttpService>,
    /// Notificações.
    pub notifications: Arc<dyn HttpService>,
    /// Sandbox do `identity`.
    pub identity: Arc<dyn HttpService>,
    /// Sandbox do `payments`.
    pub payments: Arc<dyn HttpService>,
    /// Esteira de imagens.
    pub pipeline: Pipeline,
    /// Quarentena e CDN e `files.` em memória.
    pub quarantine: Arc<MemoryObjects>,
    /// CDN.
    pub cdn: Arc<MemoryObjects>,
    /// `files.`.
    pub files: Arc<MemoryObjects>,
    /// Assinatura das URLs de desenvolvimento.
    pub dev_signer: DevUrlSigner,
    /// Papéis de vitrine (o autorizador lê daqui).
    pub grants: Arc<MemoryGrants>,
    /// Chave das sessões.
    pub session_key: Arc<Es256Key>,
    /// Chave do guest token.
    pub guest_key: Arc<Es256Key>,
    /// Sandbox (repasse da Caipora simulado).
    pub sandbox: Sandbox,
    /// Barramento.
    pub bus: Bus,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Segredo de teste da transportadora.
    pub carrier_secret: Vec<u8>,
}

fn store(bus: &Bus) -> Arc<MemoryStore> {
    let s = Arc::new(MemoryStore::new());
    s.set_sink(Arc::new(bus.clone()));
    s
}

fn random_key() -> Vec<u8> {
    saci_ids::Id::new("key").as_str().as_bytes().to_vec()
}

/// Monta o aplicativo e devolve também o trabalhador do barramento.
pub(crate) fn build(public_url: &str) -> Result<(App, Worker), String> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let (tx, rx) = unbounded_channel();
    let bus = Bus(tx);
    let local = Arc::new(LocalInternal::new());
    let internal: Arc<LocalInternal> = Arc::clone(&local);
    let cursors = CursorSigner::new(&random_key());
    let grants = Arc::new(MemoryGrants::new());
    let session_key = Arc::new(
        Es256Key::generate("local-user")
            .map_err(|e| e.to_string())?
            .0,
    );
    let guest_key = Arc::new(
        Es256Key::generate("local-guest")
            .map_err(|e| e.to_string())?
            .0,
    );
    let dev_signer = DevUrlSigner::new(&format!("{public_url}/dev"), &random_key());

    let sandbox = Sandbox::new(
        store(&bus),
        Arc::clone(&clock),
        Arc::clone(&session_key),
        USER_ISSUER,
        USER_AUDIENCE,
        public_url,
    )?;
    sandbox.register(&local);

    let catalog_store = store(&bus);
    let catalog = Catalog {
        store: catalog_store.clone(),
        clock: Arc::clone(&clock),
        internal: internal.clone(),
        cursors: cursors.clone(),
    };
    let indexer = Indexer {
        catalog: catalog_store,
        search: store(&bus),
        clock: Arc::clone(&clock),
    };
    let search = Search::new(
        Arc::clone(&indexer.search),
        Arc::clone(&clock),
        cursors.clone(),
        Duration::ZERO,
    );
    let sellers = Sellers {
        store: store(&bus),
        grants: grants.clone(),
        internal: internal.clone(),
        clock: Arc::clone(&clock),
        cdn_base_url: format!("{public_url}/dev/cdn"),
    };
    let customers = Customers {
        store: store(&bus),
        internal: internal.clone(),
        clock: Arc::clone(&clock),
        cursors: cursors.clone(),
        hash_key: random_key(),
    };
    let reviews = Reviews {
        store: store(&bus),
        internal: internal.clone(),
        clock: Arc::clone(&clock),
        cursors: cursors.clone(),
    };
    let files = Arc::new(MemoryObjects::new());
    let shipping_store = store(&bus);
    let shipping = Shipping {
        store: shipping_store.clone(),
        internal: internal.clone(),
        clock: Arc::clone(&clock),
        directory: Arc::new(TableDirectory),
        carrier: Arc::new(TableCarrier),
        files: files.clone(),
        signer: Arc::new(dev_signer.clone().with_prefix("files")),
    };
    let carrier_secret = b"segredo-de-teste-da-transportadora".to_vec();
    let carrier_hooks = CarrierWebhooks {
        store: shipping_store,
        clock: Arc::clone(&clock),
        secrets: vec![("sandbox".into(), carrier_secret.clone())],
    };
    let media = Media {
        store: store(&bus),
        signer: Arc::new(DevUploadSigner(dev_signer.clone().with_prefix("uploads"))),
        clock: Arc::clone(&clock),
    };
    let quarantine = Arc::new(MemoryObjects::new());
    let cdn = Arc::new(MemoryObjects::new());
    let pipeline = Pipeline {
        media: media.clone(),
        quarantine: quarantine.clone(),
        cdn: cdn.clone(),
        cdn_base_url: format!("{public_url}/dev/cdn"),
    };
    let orders = Orders {
        store: store(&bus),
        internal: internal.clone(),
        clock: Arc::clone(&clock),
        cursors: cursors.clone(),
    };
    let checkout = Checkout {
        store: store(&bus),
        internal: internal.clone(),
        clock: Arc::clone(&clock),
        guest_signer: guest_key.clone(),
        guest_issuer: GUEST_ISSUER.into(),
        guest_audience: GUEST_AUDIENCE.into(),
        guest_ttl_secs: 86_400,
    };
    let support = Support {
        store: store(&bus),
        internal: internal.clone(),
        clock: Arc::clone(&clock),
        cursors: cursors.clone(),
    };
    let notifications = Notifications {
        store: store(&bus),
        clock: Arc::clone(&clock),
        hash_key: random_key(),
    };
    let dispatcher = Dispatcher {
        notifications: notifications.clone(),
        internal: internal.clone(),
        queue: Arc::new(InlineQueue {
            sender: Arc::new(LogSender),
            notifications: notifications.clone(),
        }),
    };

    local.register("catalog", Arc::new(catalog.clone()));
    local.register("search", Arc::new(indexer.clone()));
    local.register("sellers", Arc::new(sellers.clone()));
    local.register("shipping", Arc::new(shipping.clone()));
    local.register("media", Arc::new(media.clone()));
    local.register("orders", Arc::new(orders.clone()));
    local.register("support", Arc::new(support.clone()));

    let consumers: Vec<Arc<dyn Consumer>> = vec![
        Arc::new(saci_catalog_api::consumer::ArtisanSync(catalog.clone())),
        Arc::new(indexer.clone()),
        Arc::new(WorkspaceProjector(sellers.clone())),
        Arc::new(ProfileCreator(customers.clone())),
        Arc::new(LabelBuyer(shipping.clone())),
        Arc::new(SaleProgress(orders.clone())),
        Arc::new(PaymentEvents(checkout.clone())),
        Arc::new(Handoffs(support.clone())),
        Arc::new(dispatcher),
    ];
    let worker = Worker {
        rx: Mutex::new(Some(rx)),
        consumers,
        dedup: Arc::new(MemoryStore::new()),
        clock: Arc::clone(&clock),
        seeds: None,
    };

    let app = App {
        catalog: Arc::new(catalog.clone()),
        search: Arc::new(search),
        sellers: Arc::new(sellers.clone()),
        customers: Arc::new(customers),
        reviews: Arc::new(reviews.clone()),
        shipping: Arc::new(shipping),
        carrier_hooks: Arc::new(carrier_hooks),
        media: Arc::new(media),
        checkout: Arc::new(checkout),
        orders: Arc::new(orders.clone()),
        support: Arc::new(support.clone()),
        notifications: Arc::new(notifications),
        identity: Arc::new(IdentityHttp(sandbox.clone())),
        payments: Arc::new(PaymentsHttp(sandbox.clone())),
        pipeline,
        quarantine,
        cdn,
        files,
        dev_signer,
        grants,
        session_key,
        guest_key,
        sandbox: sandbox.clone(),
        bus,
        clock,
        carrier_secret,
    };
    let seeds = Seeds {
        sandbox,
        catalog,
        indexer,
        sellers,
        reviews,
        orders,
        support,
    };
    Ok((
        app,
        Worker {
            seeds: Some(seeds),
            ..worker
        },
    ))
}

/// O que as sementes precisam.
#[derive(Debug)]
pub(crate) struct Seeds {
    sandbox: Sandbox,
    catalog: Catalog,
    indexer: Indexer,
    sellers: Sellers,
    reviews: Reviews,
    orders: Orders,
    support: Support,
}

impl Seeds {
    /// Aplica as migrações de todos os serviços, na ordem de dependência.
    pub(crate) async fn run(&self) -> Result<(), String> {
        let e = |p: saci_commerce_kit::Problem| {
            format!("{} {}: {}", p.status, p.code, p.message.unwrap_or_default())
        };
        self.sandbox.seed_accounts().await.map_err(e)?;
        saci_sellers_api::seed::migrate(&self.sellers, true)
            .await
            .map_err(e)?;
        saci_catalog_api::seed::migrate(&self.catalog, true)
            .await
            .map_err(e)?;
        for id in saci_sellers_api::seed::launch_storefront_ids() {
            self.catalog.refresh_artisan(&id).await.map_err(e)?;
        }
        let projected = self.indexer.rebuild().await.map_err(e)?;
        saci_reviews_api::migrate(&self.reviews, true)
            .await
            .map_err(e)?;
        saci_orders_api::seed::migrate(&self.orders, true)
            .await
            .map_err(e)?;
        saci_support_api::migrate(&self.support, true)
            .await
            .map_err(e)?;
        tracing::info!(vitrines = projected, "sementes aplicadas");
        Ok(())
    }
}

/// Entrega os eventos aos consumidores, em ordem, com deduplicação.
#[derive(Debug)]
pub(crate) struct Worker {
    rx: Mutex<Option<UnboundedReceiver<Event>>>,
    consumers: Vec<Arc<dyn Consumer>>,
    dedup: Arc<MemoryStore>,
    clock: Arc<dyn Clock>,
    /// Sementes a aplicar antes de atender.
    pub seeds: Option<Seeds>,
}

impl Worker {
    /// Roda até o canal fechar. Falha passageira tenta de novo 3 vezes.
    pub(crate) async fn run(self) {
        let Some(mut rx) = self.rx.lock().ok().and_then(|mut r| r.take()) else {
            return;
        };
        while let Some(event) = rx.recv().await {
            for consumer in &self.consumers {
                for attempt in 0..3 {
                    let dedup: &dyn DocumentStore = self.dedup.as_ref();
                    match deliver(dedup, consumer.as_ref(), &event, self.clock.epoch()).await {
                        Ok(()) => break,
                        Err(err) => {
                            tracing::warn!(consumer = consumer.name(), attempt, error = %err, "consumidor falhou");
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                    }
                }
            }
        }
    }
}
