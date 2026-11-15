//! Operações internas do catálogo (função `catalog-internal`, ADR 0004).
//!
//! | Operação | Quem chama | O que faz |
//! | --- | --- | --- |
//! | `checkout_items` | `checkout` | Preço, estoque e vitrine de cada peça (o preço nunca vem do cliente) |
//! | `reserve` | `checkout` | Prende o estoque do pedido, com escrita condicional (peça única não sai duas vezes) |
//! | `commit` | `checkout` | Confirma a reserva (o estoque já foi descontado) |
//! | `release` | `checkout` | Devolve o estoque de um pedido não pago |
//! | `public_listings` | `customers` (favoritos) | Anúncios públicos por id |
//! | `active_listings` | `sellers` (painel) | Quantas peças publicadas a vitrine tem |
//!
//! Reservar, confirmar e soltar são idempotentes por pedido: repetir não
//! desconta nem devolve duas vezes.

use saci_commerce_kit::internal::{InternalHandler, payload, reply, unknown_op};
use saci_commerce_kit::problem::problem;
use saci_commerce_kit::store::{Expect, Item, StoreError, StoreExt, WriteOp};
use saci_commerce_kit::types::Money;
use saci_commerce_kit::{BoxFut, Problem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::{Catalog, stock_event};
use crate::model::{Listing, ListingRecord, Reservation, keys, public_listing};

/// Peça pedida.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemRequest {
    /// Anúncio.
    pub listing_id: String,
    /// Quantidade.
    pub quantity: u32,
}

/// Uma peça como o checkout precisa.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutItem {
    /// Anúncio.
    pub listing_id: String,
    /// Disponível para compra (publicado e com estoque).
    pub available: bool,
    /// Nome.
    pub title: String,
    /// Vitrine.
    pub storefront_id: String,
    /// Nome da vitrine (artesão).
    pub storefront_name: String,
    /// Região do artesão (frete).
    pub region: String,
    /// Preço do catálogo.
    pub unit_price: Money,
    /// Estoque.
    pub stock: u32,
    /// Dias de produção.
    pub production_days: u32,
    /// Capa.
    pub cover_url: Option<String>,
    /// Envia ao exterior.
    pub ships_abroad: bool,
}

#[derive(Debug, Deserialize)]
struct Items {
    items: Vec<ItemRequest>,
}

#[derive(Debug, Deserialize)]
struct Reserve {
    order_id: String,
    items: Vec<ItemRequest>,
    #[serde(default = "default_ttl")]
    ttl_secs: u64,
}

fn default_ttl() -> u64 {
    2 * 3600
}

#[derive(Debug, Deserialize)]
struct OrderRef {
    order_id: String,
}

#[derive(Debug, Deserialize)]
struct StorefrontRef {
    storefront_id: String,
}

#[derive(Debug, Deserialize)]
struct Ids {
    ids: Vec<String>,
}

impl InternalHandler for Catalog {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            match op {
                "checkout_items" => reply(
                    &json!({ "items": self.checkout_items(payload::<Items>(body)?.items).await? }),
                ),
                "reserve" => {
                    let r: Reserve = payload(body)?;
                    self.reserve(&r.order_id, &r.items, r.ttl_secs).await?;
                    reply(&json!({}))
                }
                "commit" => {
                    self.commit(&payload::<OrderRef>(body)?.order_id).await?;
                    reply(&json!({}))
                }
                "release" => {
                    self.release(&payload::<OrderRef>(body)?.order_id).await?;
                    reply(&json!({}))
                }
                "active_listings" => {
                    let id = payload::<StorefrontRef>(body)?.storefront_id;
                    let index = self
                        .store
                        .query_all(keys::storefront(&id), "LST#".into())
                        .await?;
                    let count = index
                        .iter()
                        .filter(|i| i.doc["status"] == "published")
                        .count();
                    reply(&json!({ "count": count }))
                }
                "public_listings" => reply(
                    &json!({ "items": self.public_listings(&payload::<Ids>(body)?.ids).await? }),
                ),
                _ => Err(unknown_op("catalog", op)),
            }
        })
    }
}

impl Catalog {
    async fn checkout_items(&self, items: Vec<ItemRequest>) -> Result<Vec<CheckoutItem>, Problem> {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let Some((r, _)) = self
                .store
                .load::<ListingRecord>(&keys::listing(&item.listing_id), keys::META)
                .await?
            else {
                out.push(CheckoutItem {
                    listing_id: item.listing_id,
                    available: false,
                    title: String::new(),
                    storefront_id: String::new(),
                    storefront_name: String::new(),
                    region: String::new(),
                    unit_price: Money::zero(),
                    stock: 0,
                    production_days: 0,
                    cover_url: None,
                    ships_abroad: false,
                });
                continue;
            };
            let artisan = self.artisan(&r.storefront_id).await?;
            out.push(CheckoutItem {
                available: r.is_public() && r.stock >= item.quantity,
                listing_id: r.id.clone(),
                title: r.title.clone(),
                storefront_id: r.storefront_id.clone(),
                storefront_name: artisan.name,
                region: artisan.region,
                unit_price: r.price.clone(),
                stock: r.stock,
                production_days: r.production_days,
                cover_url: r.photos.first().map(|p| p.url.clone()),
                ships_abroad: r.ships_abroad,
            });
        }
        Ok(out)
    }

    /// Prende o estoque de todas as peças numa transação. Concorrência: a escrita
    /// é condicional à versão lida, e conflito tenta de novo (até 5 vezes).
    async fn reserve(
        &self,
        order_id: &str,
        items: &[ItemRequest],
        ttl: u64,
    ) -> Result<(), Problem> {
        // Um item por anúncio: a transação não aceita duas escritas no mesmo item.
        let mut merged: Vec<ItemRequest> = Vec::new();
        for item in items {
            match merged.iter_mut().find(|m| m.listing_id == item.listing_id) {
                Some(m) => m.quantity += item.quantity,
                None => merged.push(item.clone()),
            }
        }
        let items = merged.as_slice();
        if !self
            .store
            .query_all(keys::reservation(order_id), String::new())
            .await?
            .is_empty()
        {
            return Ok(()); // já reservado: repetição do mesmo pedido
        }
        let expires = self.clock.epoch() + ttl;
        for attempt in 0..5 {
            let mut ops = Vec::new();
            for item in items {
                let Some((mut r, version)) = self
                    .store
                    .load::<ListingRecord>(&keys::listing(&item.listing_id), keys::META)
                    .await?
                else {
                    return Err(problem("item-unavailable"));
                };
                if !r.is_public() || r.stock < item.quantity || item.quantity == 0 {
                    return Err(problem("item-unavailable").with_message(format!(
                        "\"{}\" não tem mais a quantidade pedida.",
                        r.title
                    )));
                }
                r.stock -= item.quantity;
                ops.push(WriteOp::replace(
                    Item::new(keys::listing(&r.id), keys::META, &r)?,
                    version,
                ));
                ops.push(WriteOp::put(Item::new(
                    keys::storefront(&r.storefront_id),
                    format!("LST#{}", r.id),
                    &r.summary(),
                )?));
                ops.push(WriteOp::Put {
                    item: Item::new(
                        keys::reservation(order_id),
                        r.id.clone(),
                        &Reservation {
                            listing_id: r.id.clone(),
                            quantity: item.quantity,
                        },
                    )?
                    .expiring(expires),
                    expect: Expect::Absent,
                });
                ops.push(WriteOp::Publish(stock_event(&r)?));
            }
            match self.store.write(ops).await {
                Ok(()) => return Ok(()),
                Err(StoreError::Conflict) if attempt < 4 => {}
                Err(StoreError::Conflict) => return Err(problem("item-unavailable")),
                Err(e) => return Err(e.into()),
            }
        }
        Err(problem("item-unavailable"))
    }

    /// Confirma: apaga as reservas (o estoque já saiu na reserva).
    async fn commit(&self, order_id: &str) -> Result<(), Problem> {
        let reservations = self
            .store
            .query_all(keys::reservation(order_id), String::new())
            .await?;
        let ops = reservations
            .into_iter()
            .map(|i| WriteOp::delete(i.pk, i.sk))
            .collect::<Vec<_>>();
        if !ops.is_empty() {
            self.store.write(ops).await?;
        }
        Ok(())
    }

    /// Solta: devolve o estoque de cada reserva ainda presente e a apaga.
    async fn release(&self, order_id: &str) -> Result<(), Problem> {
        let reservations = self
            .store
            .query_all(keys::reservation(order_id), String::new())
            .await?;
        for item in reservations {
            let reservation: Reservation = item.parse()?;
            for attempt in 0..5 {
                let Some((mut r, version)) = self
                    .store
                    .load::<ListingRecord>(&keys::listing(&reservation.listing_id), keys::META)
                    .await?
                else {
                    break;
                };
                r.stock += reservation.quantity;
                let ops = vec![
                    WriteOp::Delete {
                        pk: item.pk.clone(),
                        sk: item.sk.clone(),
                        expect: Expect::Present,
                    },
                    WriteOp::replace(Item::new(keys::listing(&r.id), keys::META, &r)?, version),
                    WriteOp::put(Item::new(
                        keys::storefront(&r.storefront_id),
                        format!("LST#{}", r.id),
                        &r.summary(),
                    )?),
                    WriteOp::Publish(stock_event(&r)?),
                ];
                match self.store.write(ops).await {
                    Ok(()) => break,
                    Err(StoreError::Conflict) if attempt < 4 => {
                        // Reserva já solta por outra execução: nada a devolver.
                        if self.store.get(&item.pk, &item.sk).await?.is_none() {
                            break;
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(())
    }

    async fn public_listings(&self, ids: &[String]) -> Result<Vec<Listing>, Problem> {
        let now = self.clock.epoch();
        let categories = self.categories().await?;
        let mut out = Vec::new();
        for id in ids.iter().take(48) {
            if let Some((r, _)) = self
                .store
                .load::<ListingRecord>(&keys::listing(id), keys::META)
                .await?
            {
                if !r.is_public() {
                    continue;
                }
                let artisan = self.artisan(&r.storefront_id).await?;
                if let Some(category) = categories.iter().find(|c| c.slug == r.category) {
                    out.push(public_listing(&r, &artisan, category, now));
                }
            }
        }
        Ok(out)
    }
}
