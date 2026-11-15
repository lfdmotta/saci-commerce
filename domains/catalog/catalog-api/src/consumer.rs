//! Consumidor de eventos das vitrines (`sellers.storefront.opened/updated.v1`).
//!
//! O evento só traz ids (seção 3.2), então o catálogo busca a vitrine pública
//! no `sellers` (chamada interna `storefront_profile`), atualiza o perfil do
//! artesão (nome, cidade, frase e identidade visual da banca) e pede ao
//! `search-indexer` para projetar de novo as peças daquela vitrine.

use saci_commerce_kit::BoxFut;
use saci_commerce_kit::Problem;
use saci_commerce_kit::events::{ConsumeError, Consumer, Event, data};
use saci_commerce_kit::internal;
use saci_commerce_kit::store::{Item, StoreExt, WriteOp};
use saci_commerce_kit::types::region_of;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::Catalog;
use crate::model::{ArtisanRecord, keys};

/// O que o `sellers` responde em `storefront_profile`.
#[derive(Debug, Clone, Deserialize)]
pub struct StorefrontProfile {
    /// Vitrine.
    pub storefront_id: String,
    /// Nome.
    pub brand_name: String,
    /// Nome na URL.
    pub url_name: String,
    /// Frase.
    #[serde(default)]
    pub slogan: Option<String>,
    /// Cidade.
    pub city: String,
    /// UF.
    pub state: String,
    /// Identidade da banca (`StorefrontTheme`).
    #[serde(default)]
    pub theme: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct StorefrontEvent {
    storefront_id: String,
}

impl Catalog {
    /// Atualiza o perfil do artesão a partir da vitrine e reprojeta as peças.
    pub async fn refresh_artisan(&self, storefront_id: &str) -> Result<(), Problem> {
        let profile: StorefrontProfile = internal::call(
            self.internal.as_ref(),
            "sellers",
            "storefront_profile",
            &json!({ "storefront_id": storefront_id }),
        )
        .await?;
        let current = self
            .store
            .load::<ArtisanRecord>(&keys::artisan(storefront_id), keys::META)
            .await?;
        let mut record = current.map_or_else(
            || ArtisanRecord {
                storefront_id: storefront_id.to_owned(),
                name: String::new(),
                city: String::new(),
                state: String::new(),
                region: String::new(),
                storefront_url_name: String::new(),
                craft: "Artesã(o)".into(),
                since: 0,
                bio: String::new(),
                quote: String::new(),
                portrait: None,
                slogan: None,
                theme: None,
            },
            |(a, _)| a,
        );
        record.name = profile.brand_name;
        record.storefront_url_name = profile.url_name;
        record.city = profile.city;
        record.region = region_of(&profile.state).unwrap_or("Sudeste").to_owned();
        record.state = profile.state;
        record.slogan = profile.slogan;
        record.theme = profile.theme;
        self.store
            .write(vec![
                WriteOp::put(Item::new(
                    keys::artisan(storefront_id),
                    keys::META,
                    &record,
                )?),
                WriteOp::put(Item::new(keys::ARTISANS, storefront_id, &json!({}))?),
            ])
            .await?;
        let _: Value = internal::call(
            self.internal.as_ref(),
            "search",
            "reindex_storefront",
            &json!({ "storefront_id": storefront_id }),
        )
        .await?;
        Ok(())
    }
}

/// O consumidor.
#[derive(Debug, Clone)]
pub struct ArtisanSync(pub Catalog);

impl Consumer for ArtisanSync {
    fn name(&self) -> &'static str {
        "catalog-artisans"
    }

    fn accepts(&self) -> &'static [&'static str] {
        &[
            "sellers.storefront.opened.v1",
            "sellers.storefront.updated.v1",
        ]
    }

    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
        Box::pin(async move {
            let e: StorefrontEvent = data(event)?;
            self.0
                .refresh_artisan(&e.storefront_id)
                .await
                .map_err(ConsumeError::from)
        })
    }
}
