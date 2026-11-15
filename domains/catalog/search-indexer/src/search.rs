//! Leitura da projeção (função `catalog-search`): a parte pública e mais
//! acessada da feira.
//!
//! | Rota | O que faz |
//! | --- | --- |
//! | `GET /v1/catalog/categories` | Categorias com a contagem de peças |
//! | `GET /v1/catalog/listings` | Prateleiras do layout (`placement`), "mais da vitrine" (`artisan` + `exclude`), sacola (`ids`) |
//! | `GET /v1/catalog/search` | Busca sem acento, páginas numeradas (até 100) e facetas sobre a busca livre |
//! | `GET /v1/catalog/artisans` | Diretório: corredores por região, Norte → Sul |
//! | `GET /v1/catalog/artisans/{id}` | Perfil do artesão |
//! | `GET /v1/catalog/storefronts/{url_name}/listings` | Peças da vitrine, com facetas da vitrine inteira |
//!
//! A projeção inteira é lida para a memória da função e renovada a cada
//! `ttl` (30 s por padrão): uma consulta é um filtro em memória, em
//! microssegundos, sem banco no caminho quente (ADR 0005). As regras de filtro,
//! ordenação e faceta são as dos mocks da interface (`listingQuery.ts`).

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use saci_catalog_api::model::{Category, CategoryRecord};
use saci_commerce_kit::clock::Clock;
use saci_commerce_kit::cursor::CursorSigner;
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::problem::{invalid, not_found};
use saci_commerce_kit::store::{DocumentStore, Item, StoreExt};
use saci_commerce_kit::text::{capitalize, matches_all, normalize, words};
use saci_commerce_kit::types::{BADGES, Page, REGIONS};
use saci_commerce_kit::{BoxFut, Problem};
use serde_json::{Value, json};

use crate::projection::{DirectoryEntry, SearchDoc, keys};

/// Tamanho da página da busca (o mesmo do mock).
const SEARCH_PAGE: usize = 50;
/// Limite de profundidade da busca.
const MAX_PAGE: usize = 100;
/// Peças por página da vitrine, quando não pedido.
const STOREFRONT_PAGE: usize = 12;
/// Faixas de preço sugeridas, em centavos: até R$ 100, de R$ 100 a R$ 250, acima.
const PRICE_STEPS: [i64; 2] = [10_000, 25_000];

/// Texto de cada corredor do diretório.
fn aisle_note(region: &str) -> &'static str {
    match region {
        "Norte" => "Arumã do Rio Negro, sementes da Amazônia e fios trançados à mão.",
        "Nordeste" => {
            "Barro de Caruaru, palha de carnaúba, o bordado de Caicó e o algodão das redes."
        }
        "Centro-Oeste" => "Capim-dourado, barro e madeira do Cerrado.",
        "Sudeste" => "O barro do Jequitinhonha e a cerâmica de alta temperatura da Serra do Mar.",
        _ => "Madeira de reflorestamento, trabalhada nas oficinas do litoral.",
    }
}

/// A projeção em memória.
#[derive(Debug, Default)]
struct Snapshot {
    docs: Vec<SearchDoc>,
    entries: Vec<DirectoryEntry>,
    categories: Vec<CategoryRecord>,
}

/// A função de leitura.
#[derive(Debug)]
pub struct Search {
    store: Arc<dyn DocumentStore>,
    clock: Arc<dyn Clock>,
    cursors: CursorSigner,
    ttl: Duration,
    cache: RwLock<Option<(Instant, Arc<Snapshot>)>>,
}

impl Search {
    /// Cria a leitura. `ttl` zero relê a projeção a cada chamada (desenvolvimento).
    #[must_use]
    pub fn new(
        store: Arc<dyn DocumentStore>,
        clock: Arc<dyn Clock>,
        cursors: CursorSigner,
        ttl: Duration,
    ) -> Self {
        Self {
            store,
            clock,
            cursors,
            ttl,
            cache: RwLock::new(None),
        }
    }

    async fn snapshot(&self) -> Result<Arc<Snapshot>, Problem> {
        if let Ok(guard) = self.cache.read()
            && let Some((at, snap)) = guard.as_ref()
            && at.elapsed() < self.ttl
        {
            return Ok(Arc::clone(snap));
        }
        let (docs, entries, categories) = tokio::try_join!(
            self.store.query_all(keys::LISTING.into(), String::new()),
            self.store.query_all(keys::ARTISAN.into(), String::new()),
            self.store.query_all(keys::CATEGORY.into(), String::new()),
        )?;
        let mut snap = Snapshot {
            docs: docs.iter().map(Item::parse).collect::<Result<_, _>>()?,
            entries: entries.iter().map(Item::parse).collect::<Result<_, _>>()?,
            categories: categories
                .iter()
                .map(Item::parse)
                .collect::<Result<_, _>>()?,
        };
        snap.docs.sort_by_key(|d| d.seq);
        let snap = Arc::new(snap);
        if let Ok(mut guard) = self.cache.write() {
            *guard = Some((Instant::now(), Arc::clone(&snap)));
        }
        Ok(snap)
    }

    async fn route(&self, req: &Request) -> HttpResult {
        match (req.method.as_str(), req.segments().as_slice()) {
            ("GET", ["v1", "catalog", "categories"]) => self.categories(req).await,
            ("GET", ["v1", "catalog", "listings"]) => self.listings(req).await,
            ("GET", ["v1", "catalog", "search"]) => self.search(req).await,
            ("GET", ["v1", "catalog", "artisans"]) => self.directory(req).await,
            ("GET", ["v1", "catalog", "artisans", id]) => self.artisan(id).await,
            ("GET", ["v1", "catalog", "storefronts", url_name, "listings"]) => {
                self.storefront(req, url_name).await
            }
            _ => Err(not_found()),
        }
    }

    async fn categories(&self, req: &Request) -> HttpResult {
        let limit = req.query_int("limit", 1, 48)?;
        let snap = self.snapshot().await?;
        let mut items: Vec<Category> = snap
            .categories
            .iter()
            .map(|c| Category {
                id: c.id.clone(),
                slug: c.slug.clone(),
                name: c.name.clone(),
                icon: c.icon.clone(),
                listing_count: u32::try_from(
                    snap.docs
                        .iter()
                        .filter(|d| d.listing.category.slug == c.slug)
                        .count(),
                )
                .unwrap_or(0),
            })
            .collect();
        if let Some(l) = limit {
            items.truncate(usize::try_from(l).unwrap_or(48));
        }
        Ok(Response::ok(&Page::all(items)).public(300))
    }

    async fn listings(&self, req: &Request) -> HttpResult {
        let placement = req.query_text("placement", 60)?;
        if placement.is_some_and(|p| {
            !p.split('.').all(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            })
        }) {
            return Err(invalid("placement", "Espaço inválido."));
        }
        let category = req.query("category");
        let artisan = req.query("artisan");
        let ids = req.query_list("ids");
        let exclude = req.query_list("exclude");
        if ids.len() > 24 || exclude.len() > 24 {
            return Err(invalid(
                if ids.len() > 24 { "ids" } else { "exclude" },
                "Use no máximo 24 ids.",
            ));
        }
        let badge = req.query_enum("badge", &BADGES)?;
        let price_max = req.query_int("price_max", 0, i64::MAX)?;
        let region = req.query_enum("region", &REGIONS)?;
        let sort = req.query_enum("sort", &["relevance", "newest", "price-asc", "price-desc"])?;
        let limit = usize::try_from(req.query_int("limit", 1, 48)?.unwrap_or(24)).unwrap_or(24);
        let snap = self.snapshot().await?;
        let mut result: Vec<&SearchDoc> = snap
            .docs
            .iter()
            .filter(|d| placement.is_none_or(|p| d.placements.iter().any(|x| x == p)))
            .filter(|d| category.is_none_or(|c| d.listing.category.slug == c))
            .filter(|d| artisan.is_none_or(|a| d.listing.artisan.id == a))
            .filter(|d| ids.is_empty() || ids.contains(&d.listing.id))
            .filter(|d| !exclude.contains(&d.listing.id))
            .filter(|d| badge.is_none_or(|b| d.listing.badges.iter().any(|x| x == b)))
            .filter(|d| region.is_none_or(|r| d.listing.artisan.region == r))
            .filter(|d| price_max.is_none_or(|m| d.listing.price.amount <= m))
            .collect();
        sort_docs(&mut result, sort);
        if matches!(sort, None | Some("relevance")) && placement.is_some() {
            // Patrocinados primeiro nos espaços da feira, sempre marcados como tais.
            result.sort_by_key(|d| d.listing.sponsored.is_none());
        }
        if !ids.is_empty() && sort.is_none() {
            // A sacola pede na ordem dela.
            result.sort_by_key(|d| ids.iter().position(|i| *i == d.listing.id));
        }
        let items: Vec<_> = result
            .into_iter()
            .take(limit)
            .map(|d| d.listing.clone())
            .collect();
        Ok(Response::ok(&Page::all(items)).public(60))
    }

    async fn search(&self, req: &Request) -> HttpResult {
        let q = req.query_text("q", 120)?;
        let category = req.query("category");
        let region = req.query_enum("region", &REGIONS)?;
        let material = req.query("material").map(normalize);
        let badge = req.query_enum("badge", &BADGES)?;
        let price_min = req.query_int("price_min", 0, i64::MAX)?;
        let price_max = req.query_int("price_max", 0, i64::MAX)?;
        let sort = req.query_enum("sort", &["relevance", "newest", "price-asc", "price-desc"])?;
        let page = usize::try_from(
            req.query_int("page", 1, i64::try_from(MAX_PAGE).unwrap_or(100))?
                .unwrap_or(1),
        )
        .unwrap_or(1);
        let snap = self.snapshot().await?;
        let w = words(q);
        let matched: Vec<&SearchDoc> = snap
            .docs
            .iter()
            .filter(|d| matches_all(&d.text, &w))
            .collect();
        let mut result: Vec<&SearchDoc> = matched
            .iter()
            .copied()
            .filter(|d| category.is_none_or(|c| d.listing.category.slug == c))
            .filter(|d| region.is_none_or(|r| d.listing.artisan.region == r))
            .filter(|d| {
                material
                    .as_ref()
                    .is_none_or(|m| d.materials_norm.contains(m))
            })
            .filter(|d| badge.is_none_or(|b| d.listing.badges.iter().any(|x| x == b)))
            .filter(|d| in_range(d.listing.price.amount, price_min, price_max))
            .collect();
        sort_docs(&mut result, sort);
        if matches!(sort, None | Some("relevance")) && !w.is_empty() {
            let score = |d: &SearchDoc| {
                w.iter()
                    .filter(|x| normalize(&d.listing.title).contains(x.as_str()))
                    .count()
            };
            result.sort_by_key(|d| std::cmp::Reverse(score(d)));
        }
        let total = result.len();
        let total_pages = total.div_ceil(SEARCH_PAGE);
        let page = page.clamp(1, total_pages.max(1));
        let start = (page - 1) * SEARCH_PAGE;
        let items: Vec<_> = result
            .into_iter()
            .skip(start)
            .take(SEARCH_PAGE)
            .map(|d| {
                let mut l = d.listing.clone();
                l.sponsored = None;
                l
            })
            .collect();
        let body = json!({
            "items": items, "page": page, "page_size": SEARCH_PAGE, "total_pages": total_pages, "total": total,
            "facets": catalog_facets(&matched, &snap.categories),
        });
        Ok(Response::ok(&body).public(60))
    }

    async fn directory(&self, req: &Request) -> HttpResult {
        let q = req.query_text("q", 120)?;
        let category = req.query("category");
        let snap = self.snapshot().await?;
        let w = words(q);
        let matched: Vec<&DirectoryEntry> = snap
            .entries
            .iter()
            .filter(|e| matches_all(&e.text, &w))
            .collect();
        let result: Vec<&DirectoryEntry> = matched
            .iter()
            .copied()
            .filter(|e| category.is_none_or(|c| e.category_slugs.iter().any(|s| s == c)))
            .collect();
        let aisles: Vec<Value> = REGIONS
            .iter()
            .filter_map(|region| {
                let mut here: Vec<&&DirectoryEntry> = result.iter().filter(|e| e.artisan.summary.region == *region).collect();
                here.sort_by(|a, b| a.artisan.summary.name.cmp(&b.artisan.summary.name));
                (!here.is_empty()).then(|| {
                    json!({ "region": region, "description": aisle_note(region), "storefronts": here.iter().map(|e| e.public()).collect::<Vec<_>>() })
                })
            })
            .collect();
        let categories: Vec<Value> = snap
            .categories
            .iter()
            .map(|c| {
                (
                    c,
                    matched
                        .iter()
                        .filter(|e| e.category_slugs.contains(&c.slug))
                        .count(),
                )
            })
            .filter(|(_, n)| *n > 0)
            .map(|(c, n)| json!({ "value": c.slug, "label": c.name, "count": n }))
            .collect();
        let body = json!({ "aisles": aisles, "total": result.len(), "facets": { "total": matched.len(), "categories": categories } });
        Ok(Response::ok(&body).public(60))
    }

    async fn artisan(&self, id: &str) -> HttpResult {
        let snap = self.snapshot().await?;
        let entry = snap
            .entries
            .iter()
            .find(|e| e.artisan.summary.id == id)
            .ok_or_else(not_found)?;
        Ok(Response::ok(&entry.artisan).public(300))
    }

    async fn storefront(&self, req: &Request, url_name: &str) -> HttpResult {
        let q = req.query_text("q", 120)?;
        let section = req.query("section");
        let badge = req.query_enum("badge", &BADGES)?;
        let price_min = req.query_int("price_min", 0, i64::MAX)?;
        let price_max = req.query_int("price_max", 0, i64::MAX)?;
        let sort = req.query_enum("sort", &["featured", "newest", "price-asc", "price-desc"])?;
        let limit = usize::try_from(req.query_int("limit", 1, 48)?.unwrap_or(12))
            .unwrap_or(STOREFRONT_PAGE);
        let snap = self.snapshot().await?;
        if !snap
            .entries
            .iter()
            .any(|e| e.artisan.summary.storefront_url_name == url_name)
        {
            return Err(not_found());
        }
        let mut all: Vec<&SearchDoc> = snap
            .docs
            .iter()
            .filter(|d| d.listing.artisan.storefront_url_name == url_name)
            .collect();
        all.sort_by_key(|d| (d.rank, d.seq));
        let w = words(q);
        let shelf_of = |d: &SearchDoc| {
            d.listing
                .section
                .as_ref()
                .map_or_else(|| d.listing.category.slug.clone(), |s| s.slug.clone())
        };
        let mut result: Vec<&SearchDoc> = all
            .iter()
            .copied()
            .filter(|d| matches_all(&d.store_text, &w))
            .filter(|d| section.is_none_or(|s| shelf_of(d) == s))
            .filter(|d| badge.is_none_or(|b| d.listing.badges.iter().any(|x| x == b)))
            .filter(|d| in_range(d.listing.price.amount, price_min, price_max))
            .collect();
        if sort != Some("featured") {
            sort_docs(&mut result, sort);
        }
        // Cursor preso à vitrine e aos filtros: não serve em outra lista.
        let scope = format!(
            "{url_name}?{}",
            req.query
                .iter()
                .filter(|(k, _)| k != "cursor")
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&")
        );
        let offset = match req.query("cursor") {
            Some(c) => self
                .cursors
                .decode(&scope, c)?
                .parse::<usize>()
                .map_err(|_| invalid("cursor", "Cursor inválido."))?,
            None => 0,
        };
        let end = offset + limit;
        let items: Vec<_> = result
            .iter()
            .skip(offset)
            .take(limit)
            .map(|d| {
                let mut l = d.listing.clone();
                l.sponsored = None;
                l
            })
            .collect();
        let mut sections: Vec<Value> = Vec::new();
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for d in &all {
            let (slug, name) = d.listing.section.as_ref().map_or_else(
                || {
                    (
                        d.listing.category.slug.clone(),
                        d.listing.category.name.clone(),
                    )
                },
                |s| (s.slug.clone(), s.name.clone()),
            );
            if let Some(&i) = seen.get(&slug) {
                sections[i]["count"] = json!(sections[i]["count"].as_u64().unwrap_or(0) + 1);
            } else {
                seen.insert(slug.clone(), sections.len());
                sections.push(json!({ "slug": slug, "name": name, "count": 1 }));
            }
        }
        let mut body = json!({
            "items": items, "total": result.len(),
            "facets": { "total": all.len(), "sections": sections, "badges": count_badges(&all), "price_bands": price_bands(&all), "currency": "BRL" },
        });
        if end < result.len() {
            body["next_cursor"] = json!(self.cursors.encode(&scope, &end.to_string()));
        }
        Ok(Response::ok(&body).public(60))
    }
}

impl HttpService for Search {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            let _ = self.clock.epoch();
            http::finish(self.route(&req).await, &id)
        })
    }
}

fn sort_docs(items: &mut [&SearchDoc], sort: Option<&str>) {
    match sort {
        Some("newest") => items.sort_by(|a, b| b.published_at.cmp(&a.published_at)),
        Some("price-asc") => items.sort_by_key(|d| d.listing.price.amount),
        Some("price-desc") => items.sort_by_key(|d| std::cmp::Reverse(d.listing.price.amount)),
        _ => {}
    }
}

fn in_range(amount: i64, min: Option<i64>, max: Option<i64>) -> bool {
    min.is_none_or(|m| amount >= m) && max.is_none_or(|m| amount < m)
}

fn count_badges(items: &[&SearchDoc]) -> Value {
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    for d in items {
        for b in &d.listing.badges {
            if let Some(known) = BADGES.iter().find(|x| *x == b) {
                *counts.entry(known).or_default() += 1;
            }
        }
    }
    json!(counts)
}

fn price_bands(items: &[&SearchDoc]) -> Vec<Value> {
    let edges = [None, Some(PRICE_STEPS[0]), Some(PRICE_STEPS[1]), None];
    edges
        .windows(2)
        .filter_map(|w| {
            let count = items
                .iter()
                .filter(|d| in_range(d.listing.price.amount, w[0], w[1]))
                .count();
            (count > 0).then(|| {
                let mut band = json!({ "count": count });
                if let Some(min) = w[0] {
                    band["min"] = json!(min);
                }
                if let Some(max) = w[1] {
                    band["max"] = json!(max);
                }
                band
            })
        })
        .collect()
}

fn catalog_facets(items: &[&SearchDoc], categories: &[CategoryRecord]) -> Value {
    let count = |f: &dyn Fn(&SearchDoc) -> bool| items.iter().filter(|d| f(d)).count();
    let cats: Vec<Value> = categories
        .iter()
        .map(|c| (c, count(&|d| d.listing.category.slug == c.slug)))
        .filter(|(_, n)| *n > 0)
        .map(|(c, n)| json!({ "value": c.slug, "label": c.name, "count": n }))
        .collect();
    let regions: Vec<Value> = REGIONS
        .iter()
        .map(|r| (r, count(&|d| d.listing.artisan.region == *r)))
        .filter(|(_, n)| *n > 0)
        .map(|(r, n)| json!({ "value": r, "label": r, "count": n }))
        .collect();
    let mut materials: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for d in items {
        for m in &d.listing.materials {
            let e = materials
                .entry(normalize(m))
                .or_insert_with(|| (capitalize(m), 0));
            e.1 += 1;
        }
    }
    let mut materials: Vec<(String, (String, usize))> = materials.into_iter().collect();
    materials.sort_by(|a, b| b.1.1.cmp(&a.1.1).then(a.1.0.cmp(&b.1.0)));
    let materials: Vec<Value> = materials
        .into_iter()
        .take(10)
        .map(|(value, (label, count))| json!({ "value": value, "label": label, "count": count }))
        .collect();
    json!({
        "total": items.len(), "categories": cats, "regions": regions, "materials": materials,
        "badges": count_badges(items), "price_bands": price_bands(items), "currency": "BRL",
    })
}
