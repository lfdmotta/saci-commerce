//! `shipping-api`: o frete (proposta 5.6; contrato `saci-contracts/http/shipping.yaml`).
//!
//! | Rota | Acesso |
//! | --- | --- |
//! | `GET /v1/shipping/postal-codes/{cep}` | `PUB` (cache de 30 dias) |
//! | `POST /v1/shipping/quotes` | `PUB-L` (30/min por IP) |
//! | `GET /v1/shipping/me/sales/{sale_id}/label` | `SELLER:pedidos` |
//!
//! A etiqueta é comprada quando a venda fica pronta (`orders.sale.ready.v1`),
//! guardada no balde privado de `files.` e entregue por **URL assinada de 5
//! minutos**, pedida a cada vez (S-09). Nada público nem previsível.
//!
//! Tabela `saci-<env>-shipping`:
//!
//! | `pk` | `sk` | Documento |
//! | --- | --- | --- |
//! | `LABEL#<venda>` | `META` | etiqueta: vitrine, chave no balde, id na transportadora |
//! | `TRACK#<código>` | `META` | venda de um código de rastreio (para o webhook) |

pub mod carrier;

use std::collections::BTreeMap;
use std::sync::Arc;

use saci_commerce_kit::auth::{self, Section};
use saci_commerce_kit::clock::{Clock, rfc3339_epoch};
use saci_commerce_kit::events::{self, ConsumeError, Consumer, Event, data};
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::internal::{
    self, InternalClient, InternalHandler, payload, reply, unknown_op,
};
use saci_commerce_kit::limits;
use saci_commerce_kit::objects::{ObjectStore, UrlSigner};
use saci_commerce_kit::problem::{invalid, not_found, problem};
use saci_commerce_kit::store::{DocumentStore, Item, StoreExt, WriteOp};
use saci_commerce_kit::types::{Page, is_postal_code};
use saci_commerce_kit::{BoxFut, Problem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::carrier::{Carrier, LabelRequest, Parcel, PostalDirectory, Quote};

/// Validade do endereço assinado da etiqueta.
const LABEL_URL_SECS: u64 = 5 * 60;

/// Dependências do serviço.
#[derive(Debug, Clone)]
pub struct Shipping {
    /// Tabela.
    pub store: Arc<dyn DocumentStore>,
    /// Chamadas a `catalog` (origem das peças) e `orders` (venda e endereço).
    pub internal: Arc<dyn InternalClient>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Base de CEPs.
    pub directory: Arc<dyn PostalDirectory>,
    /// Transportadora.
    pub carrier: Arc<dyn Carrier>,
    /// Balde privado de `files.`.
    pub files: Arc<dyn ObjectStore>,
    /// Assinatura das URLs de `files.`.
    pub signer: Arc<dyn UrlSigner>,
}

/// Etiqueta guardada.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelRecord {
    /// Vitrine.
    pub storefront_id: String,
    /// Chave do PDF no balde de `files.`.
    pub key: String,
    /// Id na transportadora.
    pub label_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct QuoteRequest {
    postal_code: String,
    storefronts: Vec<QuoteStorefront>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct QuoteStorefront {
    artisan_id: String,
    items: Vec<QuoteItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct QuoteItem {
    listing_id: String,
    quantity: u32,
}

impl HttpService for Shipping {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

impl Shipping {
    async fn route(&self, req: &Request) -> HttpResult {
        match (req.method.as_str(), req.segments().as_slice()) {
            ("GET", ["v1", "shipping", "postal-codes", cep]) => {
                Ok(Response::ok(&self.postal_code(cep).await?).public(30 * 86_400))
            }
            ("POST", ["v1", "shipping", "quotes"]) => {
                let ip = req.source_ip.clone().unwrap_or_else(|| "?".into());
                limits::hit(
                    self.store.as_ref(),
                    &format!("{ip}:quotes"),
                    60,
                    30,
                    self.clock.epoch(),
                    "rate-limited",
                )
                .await?;
                let body: QuoteRequest = req.json()?;
                Ok(Response::ok(&Page::all(self.quote(&body).await?)))
            }
            ("GET", ["v1", "shipping", "me", "sales", sale_id, "label"]) => {
                self.label(req, sale_id).await
            }
            _ => Err(not_found()),
        }
    }

    async fn postal_code(&self, cep: &str) -> Result<carrier::PostalCodeInfo, Problem> {
        if !is_postal_code(cep) {
            return Err(invalid("postal_code", "Use os 8 dígitos do CEP."));
        }
        self.directory.lookup(cep).await.ok_or_else(not_found)
    }

    async fn quote(&self, request: &QuoteRequest) -> Result<Vec<Quote>, Problem> {
        if request.storefronts.is_empty() || request.storefronts.len() > 20 {
            return Err(invalid("storefronts", "Informe de 1 a 20 vitrines."));
        }
        if request.storefronts.iter().any(|s| {
            s.items.is_empty() || s.items.len() > 24 || s.items.iter().any(|i| i.quantity == 0)
        }) {
            return Err(invalid("storefronts", "Cada vitrine tem de 1 a 24 peças."));
        }
        let destination = self.postal_code(&request.postal_code).await?;
        let items: Vec<&QuoteItem> = request.storefronts.iter().flat_map(|s| &s.items).collect();
        #[derive(Deserialize)]
        struct Found {
            items: Vec<Value>,
        }
        let found: Found = internal::call(
            self.internal.as_ref(),
            "catalog",
            "checkout_items",
            &json!({ "items": items.iter().map(|i| json!({ "listing_id": i.listing_id, "quantity": i.quantity })).collect::<Vec<_>>() }),
        )
        .await?;
        let region_of: BTreeMap<&str, &str> = found
            .items
            .iter()
            .filter_map(|i| {
                Some((
                    i["listing_id"].as_str()?,
                    i["region"].as_str().filter(|r| !r.is_empty())?,
                ))
            })
            .collect();
        let parcels: Vec<Parcel> = request
            .storefronts
            .iter()
            .map(|s| Parcel {
                artisan_id: s.artisan_id.clone(),
                origin_region: s
                    .items
                    .iter()
                    .find_map(|i| region_of.get(i.listing_id.as_str()))
                    .map_or("Sudeste", |r| *r)
                    .to_owned(),
                pieces: s.items.iter().map(|i| i.quantity).sum(),
            })
            .collect();
        Ok(self.carrier.quote(&destination.state, &parcels).await)
    }

    async fn label(&self, req: &Request, sale_id: &str) -> HttpResult {
        let storefront = auth::section(req, Section::Pedidos)?.to_owned();
        match self
            .store
            .load::<LabelRecord>(&format!("LABEL#{sale_id}"), "META")
            .await?
        {
            Some((label, _)) if label.storefront_id == storefront => {
                let expires = self.clock.epoch() + LABEL_URL_SECS;
                let url = self.signer.sign(&label.key, expires)?;
                Ok(Response::ok(
                    &json!({ "url": url, "expires_at": rfc3339_epoch(expires) }),
                ))
            }
            Some(_) => Err(not_found()),
            None => {
                // Sem etiqueta: 404 se a venda não é desta vitrine, 409 se ainda não está pronta.
                let sale: Value = internal::call(
                    self.internal.as_ref(),
                    "orders",
                    "sale_summary",
                    &json!({ "sale_id": sale_id }),
                )
                .await?;
                if sale["storefront_id"].as_str() != Some(storefront.as_str()) {
                    return Err(not_found());
                }
                Err(problem("label-not-ready"))
            }
        }
    }

    /// Compra a etiqueta de uma venda pronta e avisa (`shipping.label.purchased.v1`).
    pub async fn buy_label(&self, sale_id: &str) -> Result<(), Problem> {
        if self
            .store
            .get(&format!("LABEL#{sale_id}"), "META")
            .await?
            .is_some()
        {
            return Ok(());
        }
        #[derive(Deserialize)]
        struct ForLabel {
            storefront_id: String,
            number: String,
            service: String,
            sender: Vec<String>,
            recipient: Vec<String>,
            postal_code: String,
        }
        let sale: ForLabel = internal::call(
            self.internal.as_ref(),
            "orders",
            "sale_for_label",
            &json!({ "sale_id": sale_id }),
        )
        .await?;
        let label = self
            .carrier
            .buy_label(&LabelRequest {
                sale_id: sale_id.to_owned(),
                sale_number: sale.number,
                sender: sale.sender,
                recipient: sale.recipient,
                postal_code: sale.postal_code,
                service: sale.service,
            })
            .await
            .map_err(|e| saci_commerce_kit::problem::unavailable(&e))?;
        let key = format!("labels/{sale_id}.pdf");
        self.files.put(&key, label.pdf, "application/pdf").await?;
        let record = LabelRecord {
            storefront_id: sale.storefront_id.clone(),
            key,
            label_id: label.label_id.clone(),
        };
        let event = events::event(
            "shipping",
            "shipping.label.purchased.v1",
            sale_id,
            &json!({ "label_id": label.label_id, "sale_id": sale_id, "storefront_id": sale.storefront_id }),
        )?;
        self.store
            .write(vec![
                WriteOp::create(Item::new(format!("LABEL#{sale_id}"), "META", &record)?),
                WriteOp::Publish(event),
            ])
            .await?;
        Ok(())
    }

    /// Lembra a venda de um código de rastreio (o webhook da transportadora só traz o código).
    pub async fn track(
        &self,
        sale_id: &str,
        storefront_id: &str,
        tracking_code: &str,
    ) -> Result<(), Problem> {
        let doc = json!({ "sale_id": sale_id, "storefront_id": storefront_id });
        Ok(self
            .store
            .write(vec![WriteOp::put(Item::new(
                format!("TRACK#{tracking_code}"),
                "META",
                &doc,
            )?)])
            .await?)
    }
}

impl InternalHandler for Shipping {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            match op {
                "postal_code" => {
                    #[derive(Deserialize)]
                    struct Cep {
                        postal_code: String,
                    }
                    reply(&self.postal_code(&payload::<Cep>(body)?.postal_code).await?)
                }
                "quote" => {
                    reply(&json!({ "items": self.quote(&payload::<QuoteRequest>(body)?).await? }))
                }
                _ => Err(unknown_op("shipping", op)),
            }
        })
    }
}

/// Consumidor: compra a etiqueta na venda pronta e guarda o rastreio do envio.
#[derive(Debug, Clone)]
pub struct LabelBuyer(pub Shipping);

impl Consumer for LabelBuyer {
    fn name(&self) -> &'static str {
        "shipping-labels"
    }

    fn accepts(&self) -> &'static [&'static str] {
        &["orders.sale.ready.v1", "orders.sale.shipped.v1"]
    }

    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Sale {
                sale_id: String,
                storefront_id: String,
            }
            let e: Sale = data(event)?;
            let result = if event.event_type == "orders.sale.ready.v1" {
                self.0.buy_label(&e.sale_id).await
            } else {
                let sale: Value = internal::call(
                    self.0.internal.as_ref(),
                    "orders",
                    "sale_summary",
                    &json!({ "sale_id": e.sale_id }),
                )
                .await?;
                match sale["tracking_code"].as_str() {
                    Some(code) => self.0.track(&e.sale_id, &e.storefront_id, code).await,
                    None => Ok(()),
                }
            };
            result.map_err(ConsumeError::from)
        })
    }
}
