//! `reviews-api`: avaliações das vitrines (proposta 5.12; contrato
//! `saci-contracts/http/reviews.yaml`).
//!
//! | Rota | Acesso |
//! | --- | --- |
//! | `GET /v1/reviews/storefronts/{url_name}` | `PUB` (`limit`, `cursor`, `with_photos`) |
//! | `POST /v1/reviews/me/reviews` | `USER` + `Idempotency-Key` |
//!
//! Só quem comprou avalia, e uma vez por venda: o `orders` confirma que a venda
//! é da conta e foi entregue (chamada interna `sale_for_review`). Texto com
//! contato, link ou preço fica em análise e não aparece. O resumo das notas é
//! mantido na mesma transação de cada avaliação publicada.
//!
//! Tabela `saci-<env>-reviews`:
//!
//! | `pk` | `sk` | Documento |
//! | --- | --- | --- |
//! | `STF#<vitrine>` | `<data>#<avaliação>` | avaliação publicada |
//! | `STFP#<vitrine>` | `<data>#<avaliação>` | avaliação publicada com fotos (filtro `with_photos`) |
//! | `SUM#<vitrine>` | `META` | resumo das notas |
//! | `SALE#<venda>` | `REVIEW` | uma avaliação por venda |
//! | `REVIEW#<avaliação>` | `META` | avaliação em análise |

use std::collections::BTreeMap;
use std::sync::Arc;

use saci_commerce_kit::auth;
use saci_commerce_kit::clock::Clock;
use saci_commerce_kit::cursor::CursorSigner;
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::idempotency::{self, StoreIdempotency};
use saci_commerce_kit::internal::{self, InternalClient};
use saci_commerce_kit::problem::{invalid, not_found, problem};
use saci_commerce_kit::store::{DocumentStore, Expect, Item, Query, StoreError, StoreExt, WriteOp};
use saci_commerce_kit::text::listing_violation;
use saci_commerce_kit::{BoxFut, Problem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Dependências do serviço.
#[derive(Debug, Clone)]
pub struct Reviews {
    /// Tabela.
    pub store: Arc<dyn DocumentStore>,
    /// Chamadas a `sellers` (endereço → vitrine) e `orders` (compra entregue).
    pub internal: Arc<dyn InternalClient>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Cursores.
    pub cursors: CursorSigner,
}

/// Resumo das notas de uma vitrine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    /// Quantas avaliações publicadas.
    pub total_reviews: u32,
    /// Soma das notas.
    pub rating_sum: u32,
    /// Quantas por nota (`"1"` a `"5"`).
    pub distribution: BTreeMap<String, u32>,
    /// Quantas com fotos.
    pub with_photos: u32,
}

impl Summary {
    fn add(&mut self, rating: u8, photos: bool) {
        self.total_reviews += 1;
        self.rating_sum += u32::from(rating);
        *self.distribution.entry(rating.to_string()).or_default() += 1;
        if photos {
            self.with_photos += 1;
        }
    }

    fn public(&self) -> Value {
        let avg = if self.total_reviews == 0 {
            0.0
        } else {
            f64::from(self.rating_sum) / f64::from(self.total_reviews)
        };
        let mut distribution: BTreeMap<String, u32> = (1..=5).map(|r| (r.to_string(), 0)).collect();
        distribution.extend(self.distribution.clone());
        json!({
            "average_rating": (avg * 10.0).round() / 10.0, "total_reviews": self.total_reviews,
            "distribution": distribution, "with_photos": self.with_photos,
        })
    }
}

/// Uma avaliação guardada (o `Review` do contrato, com a vitrine e a situação).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRecord {
    /// Id.
    pub review_id: String,
    /// Vitrine.
    pub storefront_id: String,
    /// Nota de 1 a 5.
    pub rating: u8,
    /// Título.
    pub title: String,
    /// Texto.
    pub body: String,
    /// Quem escreveu (`display_name`, cidade e UF se quis mostrar).
    pub author: Value,
    /// Quando.
    pub created_at: String,
    /// Compra verificada.
    pub verified_purchase: bool,
    /// Fotos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photos: Option<Vec<Value>>,
    /// `published` ou `in_review`.
    pub status: String,
}

impl ReviewRecord {
    fn public(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            o.remove("storefront_id");
            o.remove("status");
        }
        v
    }
}

#[derive(Debug, Deserialize)]
struct ReviewWrite {
    sale_id: String,
    rating: u8,
    title: String,
    body: String,
    display_name: String,
    show_location: bool,
    #[serde(default)]
    photo_upload_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SaleForReview {
    storefront_id: String,
    delivered: bool,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

impl HttpService for Reviews {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

impl Reviews {
    async fn route(&self, req: &Request) -> HttpResult {
        match (req.method.as_str(), req.segments().as_slice()) {
            ("GET", ["v1", "reviews", "storefronts", url_name]) => self.list(req, url_name).await,
            ("POST", ["v1", "reviews", "me", "reviews"]) => self.create(req).await,
            _ => Err(not_found()),
        }
    }

    async fn list(&self, req: &Request, url_name: &str) -> HttpResult {
        let limit = usize::try_from(req.query_int("limit", 1, 48)?.unwrap_or(10)).unwrap_or(10);
        let photos = req.query_bool("with_photos")?.unwrap_or(false);
        #[derive(Deserialize)]
        struct Storefront {
            storefront_id: String,
        }
        let storefront: Storefront = internal::call(
            self.internal.as_ref(),
            "sellers",
            "storefront_by_url",
            &json!({ "url_name": url_name }),
        )
        .await?;
        let id = storefront.storefront_id;
        let scope = format!("{id}:{photos}");
        let after = req
            .query("cursor")
            .map(|c| self.cursors.decode(&scope, c))
            .transpose()?;
        let partition = if photos {
            format!("STFP#{id}")
        } else {
            format!("STF#{id}")
        };
        let page = self
            .store
            .query(
                Query::prefix(partition, "")
                    .limit(limit)
                    .after(after)
                    .desc(),
            )
            .await?;
        let items: Vec<Value> = page
            .items
            .iter()
            .map(|i| i.parse::<ReviewRecord>().map(|r| r.public()))
            .collect::<Result<_, _>>()?;
        let summary = self
            .store
            .load::<Summary>(&format!("SUM#{id}"), "META")
            .await?
            .map(|(s, _)| s)
            .unwrap_or_default();
        let mut body = json!({ "summary": summary.public(), "items": items });
        if let Some(last) = page.last {
            body["next_cursor"] = json!(self.cursors.encode(&scope, &last));
        }
        Ok(Response::ok(&body).public(120))
    }

    async fn create(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        let write: ReviewWrite = req.json()?;
        let store = StoreIdempotency(Arc::clone(&self.store));
        idempotency::run(&store, req, &account, self.clock.epoch(), || async {
            if !(1..=5).contains(&write.rating) {
                return Err(invalid("rating", "A nota vai de 1 a 5."));
            }
            for (field, text, max) in [
                ("title", &write.title, 120),
                ("body", &write.body, 4_000),
                ("display_name", &write.display_name, 60),
            ] {
                let len = text.trim().chars().count();
                if len == 0 || len > max {
                    return Err(invalid(field, &format!("Use de 1 a {max} caracteres.")));
                }
            }
            if !write.photo_upload_ids.is_empty() {
                // O envio de imagens é só do painel (SELLER:*): fotos de quem compra
                // dependem de uma rota de envio própria, ainda sem contrato.
                return Err(invalid(
                    "photo_upload_ids",
                    "Fotos nas avaliações ainda não estão disponíveis.",
                ));
            }
            let sale: SaleForReview = internal::call(
                self.internal.as_ref(),
                "orders",
                "sale_for_review",
                &json!({ "sale_id": write.sale_id, "account_id": account }),
            )
            .await
            .map_err(|p| {
                if p.status == 404 {
                    problem("purchase-required")
                } else {
                    p
                }
            })?;
            if !sale.delivered {
                return Err(problem("purchase-required")
                    .with_message("A avaliação fica disponível depois da entrega."));
            }
            let flagged = [&write.title, &write.body, &write.display_name]
                .iter()
                .any(|t| listing_violation(t).is_some());
            let mut author = json!({ "display_name": write.display_name.trim() });
            if write.show_location
                && let (Some(c), Some(s)) = (sale.city, sale.state)
            {
                author["city"] = json!(c);
                author["state"] = json!(s);
            }
            let review = ReviewRecord {
                review_id: saci_ids::Id::new("rev").to_string(),
                storefront_id: sale.storefront_id.clone(),
                rating: write.rating,
                title: write.title.trim().to_owned(),
                body: write.body.trim().to_owned(),
                author,
                created_at: self.clock.rfc3339(),
                verified_purchase: true,
                photos: None,
                status: if flagged {
                    "in_review".into()
                } else {
                    "published".into()
                },
            };
            self.save(&write.sale_id, &review).await?;
            Ok(Response::json(
                201,
                &json!({ "review_id": review.review_id, "status": review.status }),
            ))
        })
        .await
    }

    /// Grava a avaliação, a marca de "uma por venda" e o resumo, juntos.
    pub async fn save(&self, sale_id: &str, review: &ReviewRecord) -> Result<(), Problem> {
        let sk = format!("{}#{}", review.created_at, review.review_id);
        for _ in 0..5 {
            let mut ops = vec![WriteOp::create(Item::new(
                format!("SALE#{sale_id}"),
                "REVIEW",
                &json!({ "review_id": review.review_id }),
            )?)];
            if review.status == "published" {
                let current = self
                    .store
                    .get(&format!("SUM#{}", review.storefront_id), "META")
                    .await?;
                let (mut summary, expect, version) = match &current {
                    Some(i) => (
                        i.parse::<Summary>()?,
                        Expect::Version(i.version),
                        i.version + 1,
                    ),
                    None => (Summary::default(), Expect::Absent, 1),
                };
                summary.add(
                    review.rating,
                    review.photos.as_ref().is_some_and(|p| !p.is_empty()),
                );
                ops.push(WriteOp::Put {
                    item: Item::new(format!("SUM#{}", review.storefront_id), "META", &summary)?
                        .with_version(version),
                    expect,
                });
                ops.push(WriteOp::put(Item::new(
                    format!("STF#{}", review.storefront_id),
                    sk.clone(),
                    review,
                )?));
                if review.photos.as_ref().is_some_and(|p| !p.is_empty()) {
                    ops.push(WriteOp::put(Item::new(
                        format!("STFP#{}", review.storefront_id),
                        sk.clone(),
                        review,
                    )?));
                }
            } else {
                ops.push(WriteOp::put(Item::new(
                    format!("REVIEW#{}", review.review_id),
                    "META",
                    review,
                )?));
            }
            match self.store.write(ops).await {
                Ok(()) => return Ok(()),
                Err(StoreError::Conflict) => {
                    if self
                        .store
                        .get(&format!("SALE#{sale_id}"), "REVIEW")
                        .await?
                        .is_some()
                    {
                        return Err(problem("already-reviewed"));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(problem("service-unavailable"))
    }
}

/// Migração `0001-launch-reviews`: avaliações de exemplo (só fora de produção).
pub async fn migrate(reviews: &Reviews, allow_fixtures: bool) -> Result<Vec<String>, Problem> {
    const LAUNCH: &str = include_str!("../migrations/0001-launch-reviews.json");
    if !allow_fixtures
        || reviews
            .store
            .get("MIGRATION", "0001-launch-reviews")
            .await?
            .is_some()
    {
        return Ok(Vec::new());
    }
    #[derive(Deserialize)]
    struct Launch {
        reviews: Vec<ReviewRecord>,
    }
    let data: Launch =
        serde_json::from_str(LAUNCH).map_err(|e| saci_commerce_kit::problem::internal(&e))?;
    for r in &data.reviews {
        reviews.save(&format!("seed-{}", r.review_id), r).await?;
    }
    reviews
        .store
        .write(vec![WriteOp::create(Item::new(
            "MIGRATION",
            "0001-launch-reviews",
            &json!({ "at": reviews.clock.rfc3339() }),
        )?)])
        .await?;
    Ok(vec!["0001-launch-reviews".into()])
}
