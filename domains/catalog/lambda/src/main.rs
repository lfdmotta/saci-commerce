//! `saci-catalog`: as funções do domínio `catalog` (ADR 0003).
//!
//! | `SACI_FUNCTION` | Gatilho | O que roda |
//! | --- | --- | --- |
//! | `api` | API Gateway | `catalog-api`: ficha, atributos, anúncios do artesão |
//! | `search` | API Gateway | `search-indexer`: categorias, listas, busca, diretório, peças da vitrine |
//! | `internal` | Invocação direta | estoque e leituras para `checkout`, `customers`, `sellers`, `shipping` |
//! | `search-internal` | Invocação direta | `reindex_storefront`, `rebuild` |
//! | `indexer` | Fila de `catalog.listing.*` e `stock.changed` | projeção de busca |
//! | `artisans` | Fila de `sellers.storefront.*` | perfis do diretório |
//! | `relay` | DynamoDB Streams | *outbox* → EventBridge |
//! | `migrate` | Terraform (`aws_lambda_invocation`) | sementes e reindexação |
//!
//! Variáveis: `TABLE`, `SEARCH_TABLE`, `SACI_ENVIRONMENT`, `SACI_CURSOR_KEY_PARAM`,
//! `SEARCH_SNAPSHOT_SECS`, `INTERNAL_<ALVO>_FUNCTION`, `EVENT_BUS` (relay).

use std::sync::Arc;
use std::time::Duration;

use lambda_runtime::Error;
use saci_catalog_api::Catalog;
use saci_catalog_api::consumer::ArtisanSync;
use saci_commerce_kit::aws::dynamodb::DynamoStore;
use saci_commerce_kit::aws::lambda::{
    cursors, init_telemetry, run_consumer, run_http, run_internal, run_relay, run_task,
};
use saci_commerce_kit::aws::sdk_config;
use saci_commerce_kit::clock::{Clock, SystemClock};
use saci_commerce_kit::store::DocumentStore;
use saci_search_indexer::{Indexer, Search};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let function = saci_commerce_kit::env("SACI_FUNCTION")?;
    init_telemetry(&format!("catalog-{function}"));
    let config = sdk_config().await;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let catalog_store: Arc<dyn DocumentStore> = Arc::new(DynamoStore::from_env(&config, "TABLE")?);
    let internal = saci_commerce_sandbox::boot::internal_client(&config)?;
    let indexer = || -> Result<Indexer, Error> {
        Ok(Indexer {
            catalog: Arc::clone(&catalog_store),
            search: Arc::new(DynamoStore::from_env(&config, "SEARCH_TABLE")?),
            clock: Arc::clone(&clock),
        })
    };
    let catalog = Catalog {
        store: Arc::clone(&catalog_store),
        clock: Arc::clone(&clock),
        internal: Arc::clone(&internal),
        cursors: cursors(&config).await?,
    };

    match function.as_str() {
        "api" => run_http(Arc::new(catalog)).await,
        "search" => {
            let ttl = saci_commerce_kit::env_or("SEARCH_SNAPSHOT_SECS", "30")
                .parse()
                .unwrap_or(30);
            let search = Search::new(
                Arc::new(DynamoStore::from_env(&config, "SEARCH_TABLE")?),
                clock,
                cursors(&config).await?,
                Duration::from_secs(ttl),
            );
            run_http(Arc::new(search)).await
        }
        "internal" => run_internal(Arc::new(catalog)).await,
        "search-internal" => run_internal(Arc::new(indexer()?)).await,
        "indexer" => {
            run_consumer(
                Arc::clone(&catalog_store),
                vec![Arc::new(indexer()?)],
                clock,
            )
            .await
        }
        "artisans" => {
            run_consumer(
                Arc::clone(&catalog_store),
                vec![Arc::new(ArtisanSync(catalog))],
                clock,
            )
            .await
        }
        "relay" => run_relay(&config).await,
        "migrate" => {
            let fixtures = saci_commerce_kit::env_or("SACI_SEED_FIXTURES", "false") == "true";
            let catalog = Arc::new(catalog);
            run_task(move |_| {
                let catalog = Arc::clone(&catalog);
                async move {
                    let e = |p: saci_commerce_kit::Problem| format!("{} {}", p.status, p.code);
                    let applied = saci_catalog_api::seed::migrate(&catalog, fixtures)
                        .await
                        .map_err(e)?;
                    if !applied.is_empty() {
                        for id in saci_catalog_api::seed::storefront_ids().map_err(e)? {
                            // Perfis a partir das vitrines (o `sellers` migra antes).
                            if let Err(p) = catalog.refresh_artisan(&id).await {
                                tracing::warn!(code = %p.code, "perfil não sincronizado");
                            }
                        }
                    }
                    let rebuilt: serde_json::Value = saci_commerce_kit::internal::call(
                        catalog.internal.as_ref(),
                        "search",
                        "rebuild",
                        &json!({}),
                    )
                    .await
                    .map_err(e)?;
                    Ok(json!({ "applied": applied, "search": rebuilt }))
                }
            })
            .await
        }
        other => Err(format!("SACI_FUNCTION desconhecida: {other}").into()),
    }
}
