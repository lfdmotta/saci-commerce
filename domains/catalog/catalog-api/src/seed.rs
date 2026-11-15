//! Migrações de dados do catálogo (proposta 9.7: em DynamoDB, "só correções em
//! massa, idempotentes e retomáveis").
//!
//! | Migração | O que faz | Ambientes |
//! | --- | --- | --- |
//! | `0001-launch-catalog` | Categorias, atributos, perfis e peças de lançamento | `local`, `dev`, `staging` (dados fictícios) |
//!
//! Cada migração aplicada fica registrada em `MIGRATION`/`<id>`; rodar de novo
//! não faz nada. Em produção as vitrines de lançamento são cadastradas pela
//! equipe da Saci (plano de ação, seção 4), não por esta semente.

use saci_commerce_kit::Problem;
use saci_commerce_kit::store::{Item, WriteOp};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::Catalog;
use crate::model::{ArtisanRecord, AttributeSpec, CategoryRecord, ListingRecord, keys};

const LAUNCH: &str = include_str!("../migrations/0001-launch-catalog.json");

#[derive(Debug, Deserialize)]
struct Launch {
    categories: Vec<CategoryRecord>,
    attribute_specs: Vec<AttributeSpec>,
    suggested_attributes: std::collections::BTreeMap<String, Vec<String>>,
    default_suggested_attributes: Vec<String>,
    artisans: Vec<ArtisanRecord>,
    listings: Vec<ListingRecord>,
}

/// Migrações conhecidas, na ordem.
pub const MIGRATIONS: &[&str] = &["0001-launch-catalog"];

/// Aplica as migrações pendentes. `allow_fixtures` libera as sementes fictícias
/// (nunca em produção).
pub async fn migrate(catalog: &Catalog, allow_fixtures: bool) -> Result<Vec<String>, Problem> {
    let mut applied = Vec::new();
    for id in MIGRATIONS {
        if catalog.store.get("MIGRATION", id).await?.is_some() {
            continue;
        }
        if !allow_fixtures {
            tracing::info!(migration = id, "semente fictícia ignorada neste ambiente");
            continue;
        }
        launch(catalog).await?;
        catalog
            .store
            .write(vec![WriteOp::create(Item::new(
                "MIGRATION",
                *id,
                &json!({ "at": catalog.clock.rfc3339() }),
            )?)])
            .await?;
        applied.push((*id).to_owned());
    }
    Ok(applied)
}

async fn launch(catalog: &Catalog) -> Result<(), Problem> {
    let data: Launch =
        serde_json::from_str(LAUNCH).map_err(|e| saci_commerce_kit::problem::internal(&e))?;
    let mut ops = Vec::new();
    for c in &data.categories {
        ops.push(WriteOp::put(Item::new(
            keys::CATEGORIES,
            format!("{:03}#{}", c.order, c.slug),
            c,
        )?));
    }
    for (i, spec) in data.attribute_specs.iter().enumerate() {
        ops.push(WriteOp::put(Item::new(
            keys::ATTR_SPECS,
            format!("{:03}#{}", i + 1, spec.key),
            spec,
        )?));
    }
    for (category, keys_) in &data.suggested_attributes {
        ops.push(WriteOp::put(Item::new(
            keys::ATTR_SUGGESTED,
            category.clone(),
            keys_,
        )?));
    }
    ops.push(WriteOp::put(Item::new(
        keys::ATTR_SUGGESTED,
        "*",
        &data.default_suggested_attributes,
    )?));
    for a in &data.artisans {
        ops.push(WriteOp::put(Item::new(
            keys::artisan(&a.storefront_id),
            keys::META,
            a,
        )?));
        ops.push(WriteOp::put(Item::new(
            keys::ARTISANS,
            a.storefront_id.clone(),
            &json!({}),
        )?));
    }
    for l in &data.listings {
        ops.push(WriteOp::put(Item::new(
            keys::listing(&l.id),
            keys::META,
            l,
        )?));
        ops.push(WriteOp::put(Item::new(
            keys::storefront(&l.storefront_id),
            format!("LST#{}", l.id),
            &l.summary(),
        )?));
    }
    // Lotes pequenos: cabem numa transação de qualquer adaptador.
    let mut rest = ops;
    while !rest.is_empty() {
        let tail = rest.split_off(rest.len().min(25));
        catalog.store.write(rest).await?;
        rest = tail;
    }
    Ok(())
}

/// Ids das vitrines com perfil no catálogo (para sincronizar com o `sellers`).
pub fn storefront_ids() -> Result<Vec<String>, Problem> {
    let data: Launch =
        serde_json::from_str(LAUNCH).map_err(|e| saci_commerce_kit::problem::internal(&e))?;
    Ok(data.artisans.into_iter().map(|a| a.storefront_id).collect())
}

/// O conteúdo bruto da semente (o indexador e os testes também a leem).
pub fn launch_json() -> Result<Value, Problem> {
    serde_json::from_str(LAUNCH).map_err(|e| saci_commerce_kit::problem::internal(&e))
}
