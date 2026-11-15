//! O indexador: mantém a projeção de busca a partir do catálogo.
//!
//! Gatilhos:
//!
//! - eventos `catalog.listing.*.v1` e `catalog.stock.changed.v1` (consumidor
//!   `search-indexer`): projeta de novo o anúncio e a banca da vitrine;
//! - chamada interna `reindex_storefront`, depois que o perfil de um artesão
//!   mudou (o `catalog-api` chama ao consumir `sellers.storefront.*`);
//! - chamada interna `rebuild`: refaz tudo (migração, correção).
//!
//! Lê a tabela do catálogo (mesmo domínio, só leitura) e escreve só na tabela
//! da projeção.

use std::collections::BTreeSet;
use std::sync::Arc;

use saci_catalog_api::model::{
    ArtisanRecord, CategoryRecord, ListingRecord, keys as catalog, public_listing,
};
use saci_commerce_kit::clock::Clock;
use saci_commerce_kit::events::{ConsumeError, Consumer, Event, data};
use saci_commerce_kit::internal::{InternalHandler, payload, reply, unknown_op};
use saci_commerce_kit::store::{DocumentStore, Item, StoreExt, WriteOp};
use saci_commerce_kit::text::normalize;
use saci_commerce_kit::{BoxFut, Problem};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::projection::{DirectoryEntry, DisplayPiece, SearchDoc, keys};

/// O indexador.
#[derive(Debug, Clone)]
pub struct Indexer {
    /// Tabela do catálogo (leitura).
    pub catalog: Arc<dyn DocumentStore>,
    /// Tabela da projeção (escrita).
    pub search: Arc<dyn DocumentStore>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
}

impl Indexer {
    async fn categories(&self) -> Result<Vec<CategoryRecord>, Problem> {
        let items = self
            .catalog
            .query_all(catalog::CATEGORIES.into(), String::new())
            .await?;
        Ok(items.iter().map(Item::parse).collect::<Result<_, _>>()?)
    }

    async fn artisan(&self, storefront_id: &str) -> Result<Option<ArtisanRecord>, Problem> {
        Ok(self
            .catalog
            .load(&catalog::artisan(storefront_id), catalog::META)
            .await?
            .map(|(a, _)| a))
    }

    fn doc(
        record: &ListingRecord,
        artisan: &ArtisanRecord,
        category: &CategoryRecord,
        now: u64,
    ) -> SearchDoc {
        let listing = public_listing(record, artisan, category, now);
        let text = normalize(
            &[
                record.title.as_str(),
                category.name.as_str(),
                artisan.name.as_str(),
                artisan.city.as_str(),
                &record.materials.join(" "),
                &record.tags.join(" "),
            ]
            .join(" "),
        );
        let shelf = record
            .section
            .as_ref()
            .map_or(category.name.as_str(), |s| s.name.as_str());
        let store_text = normalize(
            &[
                record.title.as_str(),
                shelf,
                category.name.as_str(),
                &record.materials.join(" "),
                &record.tags.join(" "),
            ]
            .join(" "),
        );
        SearchDoc {
            listing,
            placements: record.placements.clone(),
            seq: record.seq,
            rank: record.rank,
            published_at: record
                .published_at
                .clone()
                .unwrap_or_else(|| record.created_at.clone()),
            text,
            store_text,
            materials_norm: record.materials.iter().map(|m| normalize(m)).collect(),
            customizable: record.customizable,
        }
    }

    /// Projeta um anúncio (ou o tira da projeção) e refaz a banca da vitrine.
    pub async fn project_listing(&self, listing_id: &str) -> Result<(), Problem> {
        let Some((record, _)) = self
            .catalog
            .load::<ListingRecord>(&catalog::listing(listing_id), catalog::META)
            .await?
        else {
            self.search
                .write(vec![WriteOp::delete(keys::LISTING, listing_id)])
                .await?;
            return Ok(());
        };
        self.project_record(&record, &self.categories().await?)
            .await?;
        self.reindex_entry(&record.storefront_id).await
    }

    async fn project_record(
        &self,
        record: &ListingRecord,
        categories: &[CategoryRecord],
    ) -> Result<(), Problem> {
        let artisan = self.artisan(&record.storefront_id).await?;
        let category = categories.iter().find(|c| c.slug == record.category);
        match (record.is_public(), artisan, category) {
            (true, Some(artisan), Some(category)) => {
                let doc = Self::doc(record, &artisan, category, self.clock.epoch());
                self.search
                    .write(vec![WriteOp::put(Item::new(
                        keys::LISTING,
                        record.id.clone(),
                        &doc,
                    )?)])
                    .await?;
            }
            _ => {
                self.search
                    .write(vec![WriteOp::delete(keys::LISTING, record.id.clone())])
                    .await?;
            }
        }
        Ok(())
    }

    /// Projeta de novo todas as peças de uma vitrine e a sua banca.
    pub async fn reindex_storefront(&self, storefront_id: &str) -> Result<(), Problem> {
        let categories = self.categories().await?;
        for record in self.storefront_listings(storefront_id).await? {
            self.project_record(&record, &categories).await?;
        }
        self.reindex_entry(storefront_id).await
    }

    async fn storefront_listings(
        &self,
        storefront_id: &str,
    ) -> Result<Vec<ListingRecord>, Problem> {
        let index = self
            .catalog
            .query_all(catalog::storefront(storefront_id), "LST#".into())
            .await?;
        let mut out = Vec::with_capacity(index.len());
        for item in index {
            let id = item.sk.trim_start_matches("LST#");
            if let Some((r, _)) = self
                .catalog
                .load::<ListingRecord>(&catalog::listing(id), catalog::META)
                .await?
            {
                out.push(r);
            }
        }
        out.sort_by_key(|r| (r.rank, r.seq));
        Ok(out)
    }

    /// Refaz a banca de uma vitrine.
    async fn reindex_entry(&self, storefront_id: &str) -> Result<(), Problem> {
        let Some(artisan) = self.artisan(storefront_id).await? else {
            return Ok(());
        };
        let categories = self.categories().await?;
        let pieces: Vec<ListingRecord> = self
            .storefront_listings(storefront_id)
            .await?
            .into_iter()
            .filter(ListingRecord::is_public)
            .collect();
        let display = pieces
            .iter()
            .filter_map(|l| {
                l.photos.first().map(|p| DisplayPiece {
                    id: l.id.clone(),
                    title: l.title.clone(),
                    image: saci_commerce_kit::types::Image {
                        url: p.url.clone(),
                        alt: p.alt.clone(),
                        width: p.width,
                        height: p.height,
                    },
                    price: l.price.clone(),
                })
            })
            .take(3)
            .collect();
        let category_names: Vec<&str> = pieces
            .iter()
            .filter_map(|l| {
                categories
                    .iter()
                    .find(|c| c.slug == l.category)
                    .map(|c| c.name.as_str())
            })
            .collect();
        let text = normalize(
            &[
                artisan.name.as_str(),
                artisan.city.as_str(),
                artisan.state.as_str(),
                artisan.region.as_str(),
                artisan.craft.as_str(),
                artisan.bio.as_str(),
                artisan.slogan.as_deref().unwrap_or_default(),
                &category_names.join(" "),
                &pieces
                    .iter()
                    .flat_map(|l| l.materials.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" "),
            ]
            .join(" "),
        );
        let entry = DirectoryEntry {
            artisan: artisan.profile(),
            slogan: artisan.slogan.clone(),
            identity: artisan.theme.clone(),
            display,
            listing_count: u32::try_from(pieces.len()).unwrap_or(u32::MAX),
            price_from: pieces
                .iter()
                .map(|l| &l.price)
                .min_by_key(|p| p.amount)
                .cloned(),
            takes_commissions: pieces
                .iter()
                .any(|l| l.customizable || l.curated_badges.iter().any(|b| b == "encomenda")),
            text,
            category_slugs: pieces
                .iter()
                .map(|l| l.category.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        };
        self.search
            .write(vec![WriteOp::put(Item::new(
                keys::ARTISAN,
                storefront_id,
                &entry,
            )?)])
            .await?;
        Ok(())
    }

    /// Refaz a projeção inteira: categorias, peças e bancas.
    pub async fn rebuild(&self) -> Result<usize, Problem> {
        let categories = self.categories().await?;
        let mut ops = Vec::new();
        for c in &categories {
            ops.push(WriteOp::put(Item::new(
                keys::CATEGORY,
                format!("{:03}#{}", c.order, c.slug),
                c,
            )?));
        }
        self.search.write(ops).await?;
        let mut count = 0;
        for storefront in self.storefronts().await? {
            self.reindex_storefront(&storefront).await?;
            count += 1;
        }
        Ok(count)
    }

    /// Vitrines com perfil no catálogo (partição `ARTISANS` do catálogo).
    async fn storefronts(&self) -> Result<Vec<String>, Problem> {
        Ok(self
            .catalog
            .query_all(catalog::ARTISANS.into(), String::new())
            .await?
            .into_iter()
            .map(|i| i.sk)
            .collect())
    }
}

#[derive(Debug, Deserialize)]
struct ListingEvent {
    listing_id: String,
}

#[derive(Debug, Deserialize)]
struct StorefrontRef {
    storefront_id: String,
}

impl Consumer for Indexer {
    fn name(&self) -> &'static str {
        "search-indexer"
    }

    fn accepts(&self) -> &'static [&'static str] {
        &[
            "catalog.listing.published.v1",
            "catalog.listing.updated.v1",
            "catalog.listing.paused.v1",
            "catalog.listing.withdrawn.v1",
            "catalog.stock.changed.v1",
        ]
    }

    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
        Box::pin(async move {
            let e: ListingEvent = data(event)?;
            self.project_listing(&e.listing_id)
                .await
                .map_err(ConsumeError::from)
        })
    }
}

impl InternalHandler for Indexer {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            match op {
                "reindex_storefront" => {
                    self.reindex_storefront(&payload::<StorefrontRef>(body)?.storefront_id)
                        .await?;
                    reply(&json!({}))
                }
                "rebuild" => reply(&json!({ "storefronts": self.rebuild().await? })),
                _ => Err(unknown_op("search", op)),
            }
        })
    }
}
