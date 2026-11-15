//! Rotas do `catalog-api` (proposta 5.4; contrato `saci-contracts/http/catalog.yaml`).
//!
//! | Rota | Acesso | O que faz |
//! | --- | --- | --- |
//! | `GET /v1/catalog/listings/{id}` | `PUB` | Ficha da peça (pausada, em análise ou retirada: 404) |
//! | `GET /v1/catalog/listing-attributes` | `PUB` | Atributos, com os sugeridos da categoria |
//! | `GET /v1/catalog/me/listings` | `SELLER:anuncios` | Anúncios da vitrine |
//! | `POST /v1/catalog/me/listings` | `SELLER:anuncios` + `Idempotency-Key` | Anúncio novo |
//! | `GET`/`PUT`/`DELETE /v1/catalog/me/listings/{id}` | `SELLER:anuncios` | Ler (com `ETag`), editar (`If-Match`), retirar |
//! | `POST /v1/catalog/me/listings/{id}/pause`, `/resume` | `SELLER:anuncios` | Pausar e reativar |
//!
//! A busca, as listas filtradas, o diretório e as categorias (com contagem) são
//! do `search-indexer`, que responde na mesma raiz.

use std::sync::Arc;

use saci_commerce_kit::auth::{self, Section};
use saci_commerce_kit::clock::Clock;
use saci_commerce_kit::cursor::CursorSigner;
use saci_commerce_kit::events::{self, Event};
use saci_commerce_kit::http::{
    self, HttpResult, HttpService, Request, Response, etag, require_if_match,
};
use saci_commerce_kit::idempotency::{self, StoreIdempotency};
use saci_commerce_kit::internal::{self, InternalClient};
use saci_commerce_kit::problem::{invalid, not_found, problem};
use saci_commerce_kit::store::{DocumentStore, Item, StoreExt, WriteOp};
use saci_commerce_kit::text::{Violation, listing_violation};
use saci_commerce_kit::types::{Money, Page};
use saci_commerce_kit::{BoxFut, Problem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{
    ArtisanRecord, AttributeSpec, AttributeValue, CategoryRecord, ListingRecord, ListingStatus,
    Photo, SellerListing, keys, listing_detail,
};

/// Dependências do serviço.
#[derive(Debug, Clone)]
pub struct Catalog {
    /// Tabela do catálogo.
    pub store: Arc<dyn DocumentStore>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Chamadas a `media` (fotos), `orders` (pedidos em aberto), `sellers` e `search`.
    pub internal: Arc<dyn InternalClient>,
    /// Cursores assinados.
    pub cursors: CursorSigner,
}

/// Página do painel.
const PAGE: usize = 24;

/// Corpo de `POST` e `PUT` (`ListingWrite`).
#[derive(Debug, Clone, Deserialize)]
pub struct ListingWrite {
    title: String,
    description: String,
    category: String,
    price: Money,
    quantity: u32,
    production_days: u32,
    customizable: bool,
    ships_abroad: bool,
    #[serde(default)]
    attributes: Vec<WriteAttribute>,
    #[serde(default)]
    photo_upload_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct WriteAttribute {
    key: String,
    label: String,
    value: Value,
    #[serde(default)]
    unit: Option<String>,
}

/// `ListingWriteResult`.
#[derive(Debug, Serialize)]
struct WriteResult<'a> {
    listing_id: &'a str,
    status: &'a str,
}

/// Situação de um envio, como o `media` responde.
#[derive(Debug, Clone, Deserialize)]
pub struct UploadState {
    /// Envio.
    pub upload_id: String,
    /// `pending`, `scanning`, `ready`, `rejected` ou `missing`.
    pub status: String,
    /// URL pública, quando pronta.
    #[serde(default)]
    pub public_url: Option<String>,
    /// Largura.
    #[serde(default)]
    pub width: Option<u32>,
    /// Altura.
    #[serde(default)]
    pub height: Option<u32>,
}

impl HttpService for Catalog {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

impl Catalog {
    async fn route(&self, req: &Request) -> HttpResult {
        match (req.method.as_str(), req.segments().as_slice()) {
            ("GET", ["v1", "catalog", "listings", id]) => self.get_listing(id).await,
            ("GET", ["v1", "catalog", "listing-attributes"]) => self.attributes(req).await,
            ("GET", ["v1", "catalog", "me", "listings"]) => self.my_listings(req).await,
            ("POST", ["v1", "catalog", "me", "listings"]) => self.create(req).await,
            ("GET", ["v1", "catalog", "me", "listings", id]) => self.my_listing(req, id).await,
            ("PUT", ["v1", "catalog", "me", "listings", id]) => self.update(req, id).await,
            ("DELETE", ["v1", "catalog", "me", "listings", id]) => self.withdraw(req, id).await,
            (
                "POST",
                [
                    "v1",
                    "catalog",
                    "me",
                    "listings",
                    id,
                    action @ ("pause" | "resume"),
                ],
            ) => self.toggle(req, id, action == &"pause").await,
            _ => Err(not_found()),
        }
    }

    // --- leitura pública ---------------------------------------------------------------

    async fn get_listing(&self, id: &str) -> HttpResult {
        let Some((record, _)) = self
            .store
            .load::<ListingRecord>(&keys::listing(id), keys::META)
            .await?
        else {
            return Err(not_found());
        };
        if !record.is_public() {
            return Err(not_found());
        }
        let artisan = self.artisan(&record.storefront_id).await?;
        let category = self.category(&record.category).await?;
        Ok(Response::ok(&listing_detail(
            &record,
            &artisan,
            &category,
            self.clock.epoch(),
        ))
        .public(60))
    }

    async fn attributes(&self, req: &Request) -> HttpResult {
        let category = req.query_text("category", 60)?;
        let specs = self
            .store
            .query_all(keys::ATTR_SPECS.into(), String::new())
            .await?;
        // Sugestões da categoria; sem categoria (ou sem sugestão própria), as gerais (`*`).
        let mut suggested: Option<Vec<String>> = None;
        if let Some(c) = category {
            suggested = self
                .store
                .load(keys::ATTR_SUGGESTED, c)
                .await?
                .map(|(v, _)| v);
        }
        if suggested.is_none() {
            suggested = self
                .store
                .load(keys::ATTR_SUGGESTED, "*")
                .await?
                .map(|(v, _)| v);
        }
        let suggested = suggested.unwrap_or_default();
        let mut items = specs
            .iter()
            .map(Item::parse::<AttributeSpec>)
            .collect::<Result<Vec<_>, _>>()?;
        for spec in &mut items {
            spec.suggested = Some(suggested.contains(&spec.key));
        }
        Ok(Response::ok(&Page::all(items)).public(300))
    }

    // --- painel ---------------------------------------------------------------------------

    async fn my_listings(&self, req: &Request) -> HttpResult {
        let storefront = auth::section(req, Section::Anuncios)?;
        let status = req.query_enum("status", &["published", "in_review", "paused", "sold_out"])?;
        let scope = format!("{storefront}:me/listings:{}", status.unwrap_or("*"));
        let after = req
            .query("cursor")
            .map(|c| self.cursors.decode(&scope, c))
            .transpose()?;
        let items = self
            .store
            .query_all(keys::storefront(storefront), "LST#".into())
            .await?;
        let mut summaries = items
            .iter()
            .map(Item::parse::<SellerListing>)
            .collect::<Result<Vec<_>, _>>()?;
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.id.cmp(&a.id)));
        if let Some(s) = status {
            summaries.retain(|l| l.status == s);
        }
        let start = after
            .and_then(|last| summaries.iter().position(|l| l.id == last).map(|p| p + 1))
            .unwrap_or(0);
        let page: Vec<SellerListing> = summaries.iter().skip(start).take(PAGE).cloned().collect();
        let next_cursor = (start + PAGE < summaries.len())
            .then(|| page.last().map(|l| self.cursors.encode(&scope, &l.id)))
            .flatten();
        Ok(Response::ok(&Page {
            items: page,
            next_cursor,
        }))
    }

    async fn my_listing(&self, req: &Request, id: &str) -> HttpResult {
        let storefront = auth::section(req, Section::Anuncios)?;
        let (record, version) = self.owned(storefront, id).await?;
        let attributes: Vec<Value> = record
            .attributes
            .iter()
            .map(|a| json!({ "key": a.key, "label": a.label, "kind": a.kind, "value": a.value, "unit": a.unit }))
            .map(|mut v| {
                if v["unit"].is_null()
                    && let Some(o) = v.as_object_mut() {
                        o.remove("unit");
                    }
                v
            })
            .collect();
        let body = json!({
            "id": record.id, "status": record.seller_status(), "title": record.title,
            "description": record.description, "category": record.category, "price": record.price,
            "quantity": record.quantity, "production_days": record.production_days,
            "customizable": record.customizable, "ships_abroad": record.ships_abroad,
            "attributes": attributes,
            "photos": record.photos.iter().map(|p| json!({ "photo_id": p.photo_id, "url": p.url })).collect::<Vec<_>>(),
        });
        Ok(Response::ok(&body).header("etag", &etag(version)))
    }

    async fn create(&self, req: &Request) -> HttpResult {
        let storefront = auth::section(req, Section::Anuncios)?.to_owned();
        let account = auth::user(req)?.to_owned();
        let write: ListingWrite = req.json()?;
        let now = self.clock.epoch();
        let store = StoreIdempotency(Arc::clone(&self.store));
        idempotency::run(&store, req, &account, now, || async {
            let category = self.validate(&write).await?;
            let photos = self.resolve_photos(&storefront, &write, &[]).await?;
            let id = saci_ids::Id::new(saci_ids::prefix::LISTING).to_string();
            let at = self.clock.rfc3339();
            let record = ListingRecord {
                id: id.clone(),
                storefront_id: storefront.clone(),
                title: write.title.trim().to_owned(),
                description: write.description.trim().to_owned(),
                category: category.slug.clone(),
                category_path: None,
                price: write.price.clone(),
                compare_at_price: None,
                quantity: write.quantity,
                stock: write.quantity,
                production_days: write.production_days,
                customizable: write.customizable,
                ships_abroad: write.ships_abroad,
                attributes: attributes(&write),
                photos,
                status: ListingStatus::Published,
                curated_badges: derived_badges(&write),
                materials: materials(&write),
                tags: Vec::new(),
                section: None,
                rank: 1_000,
                placements: Vec::new(),
                sponsored: None,
                seq: 1_000_000 + now,
                created_at: at.clone(),
                updated_at: at.clone(),
                published_at: Some(at),
            };
            let event = listing_event("published", &record)?;
            self.store
                .write(vec![
                    WriteOp::create(Item::new(keys::listing(&id), keys::META, &record)?),
                    WriteOp::put(Item::new(
                        keys::storefront(&storefront),
                        format!("LST#{id}"),
                        &record.summary(),
                    )?),
                    WriteOp::Publish(event),
                ])
                .await?;
            Ok(Response::json(
                201,
                &WriteResult {
                    listing_id: &id,
                    status: "published",
                },
            ))
        })
        .await
    }

    async fn update(&self, req: &Request, id: &str) -> HttpResult {
        let storefront = auth::section(req, Section::Anuncios)?.to_owned();
        let (mut record, version) = self.owned(&storefront, id).await?;
        require_if_match(req, version)?;
        let write: ListingWrite = req.json()?;
        let category = self.validate(&write).await?;
        let photos = self
            .resolve_photos(&storefront, &write, &record.photos)
            .await?;
        let old_stock = record.stock;
        let delta = i64::from(write.quantity) - i64::from(record.quantity);
        record.stock = u32::try_from((i64::from(record.stock) + delta).max(0)).unwrap_or(0);
        record.quantity = write.quantity;
        record.title = write.title.trim().to_owned();
        record.description = write.description.trim().to_owned();
        if record.category != category.slug {
            record.category_path = None;
        }
        record.category = category.slug;
        record.price = write.price.clone();
        record.production_days = write.production_days;
        record.customizable = write.customizable;
        record.ships_abroad = write.ships_abroad;
        record.attributes = attributes(&write);
        record.materials = materials(&write);
        record.photos = photos;
        let keep: Vec<String> = record
            .curated_badges
            .iter()
            .filter(|b| *b == "sustentavel")
            .cloned()
            .collect();
        record.curated_badges = derived_badges(&write).into_iter().chain(keep).collect();
        record.updated_at = self.clock.rfc3339();
        let mut ops = vec![
            WriteOp::replace(Item::new(keys::listing(id), keys::META, &record)?, version),
            WriteOp::put(Item::new(
                keys::storefront(&storefront),
                format!("LST#{id}"),
                &record.summary(),
            )?),
            WriteOp::Publish(listing_event("updated", &record)?),
        ];
        if record.stock != old_stock {
            ops.push(WriteOp::Publish(stock_event(&record)?));
        }
        self.store.write(ops).await.map_err(|e| match e {
            saci_commerce_kit::store::StoreError::Conflict => problem("precondition-failed"),
            other => other.into(),
        })?;
        Ok(Response::ok(&WriteResult {
            listing_id: id,
            status: record.seller_status(),
        })
        .header("etag", &etag(version + 1)))
    }

    async fn withdraw(&self, req: &Request, id: &str) -> HttpResult {
        let storefront = auth::section(req, Section::Anuncios)?.to_owned();
        let (mut record, version) = self.owned(&storefront, id).await?;
        #[derive(Deserialize)]
        struct Open {
            open: bool,
        }
        let open: Open = internal::call(
            self.internal.as_ref(),
            "orders",
            "listing_has_open_orders",
            &json!({ "listing_id": id }),
        )
        .await?;
        if open.open {
            return Err(problem("listing-has-open-orders"));
        }
        record.status = ListingStatus::Withdrawn;
        record.updated_at = self.clock.rfc3339();
        self.store
            .write(vec![
                WriteOp::replace(Item::new(keys::listing(id), keys::META, &record)?, version),
                WriteOp::delete(keys::storefront(&storefront), format!("LST#{id}")),
                WriteOp::Publish(listing_event("withdrawn", &record)?),
            ])
            .await?;
        Ok(Response::no_content())
    }

    async fn toggle(&self, req: &Request, id: &str, pause: bool) -> HttpResult {
        let storefront = auth::section(req, Section::Anuncios)?.to_owned();
        let (mut record, version) = self.owned(&storefront, id).await?;
        let (from, to, kind) = if pause {
            (ListingStatus::Published, ListingStatus::Paused, "paused")
        } else {
            (ListingStatus::Paused, ListingStatus::Published, "published")
        };
        if record.status != from {
            return Err(problem("invalid-state"));
        }
        record.status = to;
        record.updated_at = self.clock.rfc3339();
        if !pause && record.published_at.is_none() {
            record.published_at = Some(record.updated_at.clone());
        }
        self.store
            .write(vec![
                WriteOp::replace(Item::new(keys::listing(id), keys::META, &record)?, version),
                WriteOp::put(Item::new(
                    keys::storefront(&storefront),
                    format!("LST#{id}"),
                    &record.summary(),
                )?),
                WriteOp::Publish(listing_event(kind, &record)?),
            ])
            .await?;
        Ok(Response::ok(&WriteResult {
            listing_id: id,
            status: record.seller_status(),
        }))
    }

    // --- apoio -----------------------------------------------------------------------------

    /// O anúncio, se existe, não foi retirado e é desta vitrine (senão, 404).
    pub(crate) async fn owned(
        &self,
        storefront: &str,
        id: &str,
    ) -> Result<(ListingRecord, u64), Problem> {
        match self
            .store
            .load::<ListingRecord>(&keys::listing(id), keys::META)
            .await?
        {
            Some((r, v))
                if r.storefront_id == storefront && r.status != ListingStatus::Withdrawn =>
            {
                Ok((r, v))
            }
            _ => Err(not_found()),
        }
    }

    /// Perfil do artesão (vitrine sem perfil ainda: um mínimo a partir do id).
    pub async fn artisan(&self, storefront_id: &str) -> Result<ArtisanRecord, Problem> {
        Ok(self
            .store
            .load::<ArtisanRecord>(&keys::artisan(storefront_id), keys::META)
            .await?
            .map_or_else(
                || ArtisanRecord {
                    storefront_id: storefront_id.to_owned(),
                    name: "Artesão da Saci".into(),
                    city: String::new(),
                    state: "SP".into(),
                    region: "Sudeste".into(),
                    storefront_url_name: String::new(),
                    craft: String::new(),
                    since: 0,
                    bio: String::new(),
                    quote: String::new(),
                    portrait: None,
                    slogan: None,
                    theme: None,
                },
                |(a, _)| a,
            ))
    }

    /// Todas as categorias, na ordem da curadoria.
    pub async fn categories(&self) -> Result<Vec<CategoryRecord>, Problem> {
        let items = self
            .store
            .query_all(keys::CATEGORIES.into(), String::new())
            .await?;
        Ok(items.iter().map(Item::parse).collect::<Result<_, _>>()?)
    }

    async fn category(&self, slug: &str) -> Result<CategoryRecord, Problem> {
        self.categories()
            .await?
            .into_iter()
            .find(|c| c.slug == slug)
            .ok_or_else(|| invalid("category", "Escolha uma categoria da lista."))
    }

    async fn validate(&self, write: &ListingWrite) -> Result<CategoryRecord, Problem> {
        let title_len = write.title.trim().chars().count();
        if !(3..=120).contains(&title_len) {
            return Err(invalid(
                "title",
                "O nome precisa ter de 3 a 120 caracteres.",
            ));
        }
        if write.description.chars().count() > 5_000 {
            return Err(invalid(
                "description",
                "A descrição pode ter até 5.000 caracteres.",
            ));
        }
        for (field, text) in [("title", &write.title), ("description", &write.description)] {
            if let Some(v) = listing_violation(text) {
                let message = match v {
                    Violation::Contact => {
                        "Tire o contato: a conversa com quem compra acontece pela Saci."
                    }
                    Violation::Link => "Tire o link: a peça precisa ser vendida pela Saci.",
                    Violation::Price => "Tire o preço do texto: ele vai só no campo de preço.",
                };
                return Err(problem("listing-rejected").with_field(field, message));
            }
        }
        if !write.price.is_valid() || write.price.amount == 0 || write.price.currency != "BRL" {
            return Err(invalid("price", "Informe o preço em reais."));
        }
        if !(1..=9_999).contains(&write.quantity) {
            return Err(invalid("quantity", "A quantidade vai de 1 a 9.999."));
        }
        if write.production_days > 180 {
            return Err(invalid(
                "production_days",
                "O prazo de produção vai até 180 dias.",
            ));
        }
        if write.photo_upload_ids.is_empty() || write.photo_upload_ids.len() > 10 {
            return Err(invalid("photo_upload_ids", "Envie de 1 a 10 fotos."));
        }
        for a in &write.attributes {
            let ok_value = match &a.value {
                Value::String(s) => !s.trim().is_empty() && s.chars().count() <= 200,
                Value::Number(_) => true,
                _ => false,
            };
            if a.key.trim().is_empty() || a.label.trim().is_empty() || !ok_value {
                return Err(invalid("attributes", "Confira os atributos da peça."));
            }
            if let Value::String(s) = &a.value
                && listing_violation(s).is_some()
            {
                return Err(problem("listing-rejected").with_field(
                    "attributes",
                    "Os atributos não podem ter contato, link ou preço.",
                ));
            }
        }
        self.category(&write.category).await
    }

    /// Fotos novas (`upl_`, precisam estar prontas no `media`) e mantidas (`pho_`).
    async fn resolve_photos(
        &self,
        storefront: &str,
        write: &ListingWrite,
        existing: &[Photo],
    ) -> Result<Vec<Photo>, Problem> {
        let uploads: Vec<&String> = write
            .photo_upload_ids
            .iter()
            .filter(|i| i.starts_with("upl_"))
            .collect();
        let states: Vec<UploadState> = if uploads.is_empty() {
            Vec::new()
        } else {
            #[derive(Deserialize)]
            struct Out {
                items: Vec<UploadState>,
            }
            let out: Out = internal::call(
                self.internal.as_ref(),
                "media",
                "uploads_for_listing",
                &json!({ "storefront_id": storefront, "upload_ids": uploads }),
            )
            .await?;
            out.items
        };
        let mut photos = Vec::with_capacity(write.photo_upload_ids.len());
        for (n, id) in write.photo_upload_ids.iter().enumerate() {
            let alt = format!("{}, foto {}", write.title.trim(), n + 1);
            if id.starts_with("pho_") {
                let photo = existing.iter().find(|p| p.photo_id == *id).ok_or_else(|| {
                    invalid("photo_upload_ids", "Uma das fotos não é deste anúncio.")
                })?;
                photos.push(Photo {
                    alt,
                    ..photo.clone()
                });
                continue;
            }
            let state = states.iter().find(|s| s.upload_id == *id);
            match state.map(|s| s.status.as_str()) {
                Some("ready") => {
                    let s = state.ok_or_else(not_found)?;
                    photos.push(Photo {
                        photo_id: saci_ids::Id::new("pho").to_string(),
                        url: s.public_url.clone().unwrap_or_default(),
                        alt,
                        width: s.width,
                        height: s.height,
                    });
                }
                Some("pending" | "scanning") => return Err(problem("photo-not-ready")),
                _ => {
                    return Err(invalid(
                        "photo_upload_ids",
                        "Uma das fotos foi recusada ou não existe. Envie outra.",
                    ));
                }
            }
        }
        Ok(photos)
    }
}

fn attributes(write: &ListingWrite) -> Vec<AttributeValue> {
    write
        .attributes
        .iter()
        .map(|a| AttributeValue {
            key: a.key.trim().to_owned(),
            label: a.label.trim().to_owned(),
            kind: if a.value.is_number() {
                "number".into()
            } else {
                "text".into()
            },
            value: a.value.clone(),
            unit: a.unit.clone().filter(|u| !u.is_empty()),
        })
        .collect()
}

fn materials(write: &ListingWrite) -> Vec<String> {
    write
        .attributes
        .iter()
        .filter(|a| matches!(a.key.as_str(), "material" | "materia" | "livre:materia"))
        .filter_map(|a| a.value.as_str().map(str::to_owned))
        .collect()
}

/// Selos calculados na escrita: `peca-unica` (uma só) e `encomenda` (aceita personalização).
fn derived_badges(write: &ListingWrite) -> Vec<String> {
    let mut badges = Vec::new();
    if write.quantity == 1 {
        badges.push("peca-unica".to_owned());
    }
    if write.customizable {
        badges.push("encomenda".to_owned());
    }
    badges
}

/// `catalog.listing.<ação>.v1`.
pub(crate) fn listing_event(action: &str, record: &ListingRecord) -> Result<Event, Problem> {
    events::event(
        "catalog",
        &format!("catalog.listing.{action}.v1"),
        &record.id,
        &json!({ "listing_id": record.id, "storefront_id": record.storefront_id }),
    )
}

/// `catalog.stock.changed.v1`.
pub(crate) fn stock_event(record: &ListingRecord) -> Result<Event, Problem> {
    events::event(
        "catalog",
        "catalog.stock.changed.v1",
        &record.id,
        &json!({ "listing_id": record.id, "storefront_id": record.storefront_id, "stock": record.stock }),
    )
}
