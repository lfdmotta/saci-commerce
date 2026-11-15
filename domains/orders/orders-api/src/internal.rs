//! Operações internas dos pedidos (função `orders-internal`) e o consumidor
//! de eventos que avança as vendas.
//!
//! | Operação | Quem chama | O que faz |
//! | --- | --- | --- |
//! | `create_order` | `checkout` | Cria o pedido e uma venda por vitrine, aguardando pagamento |
//! | `mark_paid` | `checkout` | Pagamento confirmado: as vendas entram em produção, com prazo |
//! | `cancel` | `checkout` | Pagamento recusado ou Pix vencido |
//! | `order_status` | `checkout` | Pedido e situação, para `GET /v1/checkout/orders/{id}/payment` |
//! | `listing_has_open_orders` | `catalog` | Peça com venda em aberto não pode ser retirada |
//! | `storefront_to_do` | `sellers` | Vendas "a fazer" (painel) |
//! | `sale_for_review` | `reviews` | Venda da conta e já entregue? |
//! | `sale_summary` | `shipping` | Vitrine, situação e rastreio de uma venda |
//! | `sale_for_label` | `shipping` | Remetente e destinatário da etiqueta (só em `awaiting_shipment`) |
//! | `sale_parties` | `notifications` | Conta de quem comprou e vitrine |

use saci_commerce_kit::events::{ConsumeError, Consumer, Event, data};
use saci_commerce_kit::internal::{self, InternalHandler, payload, reply, unknown_op};
use saci_commerce_kit::problem::{not_found, problem};
use saci_commerce_kit::store::{Expect, Item, StoreError, StoreExt, WriteOp};
use saci_commerce_kit::types::Money;
use saci_commerce_kit::{BoxFut, Problem};
use serde::Deserialize;
use serde_json::{Value, json};
use time::Duration;

use crate::api::{Orders, sale_event};
use crate::model::{
    Days, OrderLine, OrderRecord, SaleIndex, SaleItem, SaleRecord, SaleShipping, group_of, keys,
    sale_status as st,
};

/// Pedido novo, como o checkout manda (nenhum dado pessoal além do primeiro nome e da cidade).
#[derive(Debug, Clone, Deserialize)]
pub struct NewOrder {
    /// Id escolhido pelo checkout (idempotência da saga).
    pub order_id: String,
    /// Conta, quando há.
    #[serde(default)]
    pub account_id: Option<String>,
    /// `pix`, `credito` ou `debito`.
    pub payment_method: String,
    /// Endereço no cofre.
    pub address_ref: String,
    /// "Cidade, UF".
    pub destination: String,
    /// `{ first_name, city, state }`.
    pub buyer: Value,
    /// Linhas, com o preço do catálogo.
    pub lines: Vec<OrderLine>,
    /// Frete por vitrine.
    pub shipping: Vec<ShippingChoice>,
}

/// Frete escolhido para uma vitrine.
#[derive(Debug, Clone, Deserialize)]
pub struct ShippingChoice {
    /// Vitrine.
    pub storefront_id: String,
    /// Serviço.
    pub service: String,
    /// Preço.
    pub price: Money,
    /// Prazo.
    pub days: Days,
}

#[derive(Debug, Deserialize)]
struct OrderRef {
    order_id: String,
}

#[derive(Debug, Deserialize)]
struct SaleRef {
    sale_id: String,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StorefrontRef {
    storefront_id: String,
}

#[derive(Debug, Deserialize)]
struct ListingRef {
    listing_id: String,
}

impl Orders {
    /// Próximo número legível (contador com escrita condicional).
    async fn next_number(&self) -> Result<String, Problem> {
        for _ in 0..10 {
            let current = self.store.get("SEQ", "ORDER").await?;
            let (n, expect, version) = match &current {
                Some(i) => (
                    i.doc["n"].as_u64().unwrap_or(48_300),
                    Expect::Version(i.version),
                    i.version + 1,
                ),
                None => (48_300, Expect::Absent, 1),
            };
            let item = Item::new("SEQ", "ORDER", &json!({ "n": n + 1 }))?.with_version(version);
            match self.store.write(vec![WriteOp::Put { item, expect }]).await {
                Ok(()) => return Ok((n + 1).to_string()),
                Err(StoreError::Conflict) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(problem("service-unavailable"))
    }

    /// Cria o pedido e as vendas. Repetir com o mesmo `order_id` devolve o existente.
    pub async fn create_order(&self, new: NewOrder) -> Result<Value, Problem> {
        if let Some((existing, _)) = self
            .store
            .load::<OrderRecord>(&keys::order(&new.order_id), keys::META)
            .await?
        {
            return self.order_view(&existing).await;
        }
        let number = self.next_number().await?;
        let at = self.clock.rfc3339();
        let subtotal: i64 = new
            .lines
            .iter()
            .map(|l| l.unit_price.amount * i64::from(l.quantity))
            .sum();
        let freight: i64 = new.shipping.iter().map(|s| s.price.amount).sum();
        let mut sales = Vec::new();
        for choice in &new.shipping {
            let lines: Vec<&OrderLine> = new
                .lines
                .iter()
                .filter(|l| l.storefront_id == choice.storefront_id)
                .collect();
            let Some(first) = lines.first() else { continue };
            let production = lines.iter().map(|l| l.production_days).max().unwrap_or(0);
            sales.push(SaleRecord {
                id: saci_ids::Id::new(saci_ids::prefix::SALE).to_string(),
                order_id: new.order_id.clone(),
                number: format!("{number}-{}", sales.len() + 1),
                storefront_id: choice.storefront_id.clone(),
                storefront_name: first.storefront_name.clone(),
                account_id: new.account_id.clone(),
                status: st::PENDING_PAYMENT.into(),
                placed_at: at.clone(),
                updated_at: at.clone(),
                buyer: new.buyer.clone(),
                items: lines
                    .iter()
                    .map(|l| SaleItem {
                        listing_id: l.listing_id.clone(),
                        title: l.title.clone(),
                        photo_url: l.photo_url.clone(),
                        quantity: l.quantity,
                        unit_price: l.unit_price.clone(),
                        customization: None,
                    })
                    .collect(),
                deadline: json!({ "ship_by": at, "source": "listing", "production_days": production }),
                shipping: SaleShipping {
                    service: choice.service.clone(),
                    price: choice.price.clone(),
                    days: choice.days,
                    label_available: false,
                    tracking_code: None,
                    shipped_at: None,
                    delivered_at: None,
                    estimated_delivery: None,
                },
                address_ref: Some(new.address_ref.clone()),
                address_fixture: None,
                invoice: Some(json!({ "required": false })),
                return_info: None,
                ticket_protocol: None,
                history: Vec::new(),
            });
        }
        let order = OrderRecord {
            id: new.order_id.clone(),
            number,
            account_id: new.account_id.clone(),
            status: "awaiting-payment".into(),
            lines: new.lines.clone(),
            subtotal: Money::brl(subtotal),
            shipping: Money::brl(freight),
            total: Money::brl(subtotal + freight),
            delivery_days: Days {
                min: new.shipping.iter().map(|s| s.days.min).max().unwrap_or(0),
                max: new.shipping.iter().map(|s| s.days.max).max().unwrap_or(0),
            },
            address_ref: Some(new.address_ref),
            destination: new.destination,
            payment_method: new.payment_method,
            sale_ids: sales.iter().map(|s| s.id.clone()).collect(),
            created_at: at.clone(),
            updated_at: at.clone(),
        };
        let mut ops = vec![WriteOp::create(Item::new(
            keys::order(&order.id),
            keys::META,
            &order,
        )?)];
        if let Some(account) = &order.account_id {
            ops.push(WriteOp::put(Item::new(
                keys::account(account),
                format!("ORD#{at}#{}", order.id),
                &json!({}),
            )?));
        }
        for s in &sales {
            ops.push(WriteOp::create(Item::new(
                keys::sale(&s.id),
                keys::META,
                s,
            )?));
            ops.push(WriteOp::put(Item::new(
                keys::storefront(&s.storefront_id),
                format!("SAL#{}", s.id),
                &SaleIndex::from(s),
            )?));
            for item in &s.items {
                ops.push(WriteOp::put(Item::new(
                    keys::listing(&item.listing_id),
                    format!("SAL#{}", s.id),
                    &json!({}),
                )?));
            }
        }
        match self.store.write(ops).await {
            Ok(()) | Err(StoreError::Conflict) => {}
            Err(e) => return Err(e.into()),
        }
        self.order_view(&self.order(&order.id).await?).await
    }

    /// Pagamento confirmado: vendas em produção, com o prazo de postagem.
    pub async fn mark_paid(&self, order_id: &str) -> Result<Value, Problem> {
        let (mut order, version) = self
            .store
            .load::<OrderRecord>(&keys::order(order_id), keys::META)
            .await?
            .ok_or_else(not_found)?;
        if order.status == "awaiting-payment" {
            order.status = "paid".into();
            order.updated_at = self.clock.rfc3339();
            self.store
                .write(vec![WriteOp::replace(
                    Item::new(keys::order(order_id), keys::META, &order)?,
                    version,
                )])
                .await?;
        }
        let now = self.clock.now();
        for id in &order.sale_ids {
            let result = self
                .transition(
                    id,
                    &[st::PENDING_PAYMENT],
                    st::IN_PRODUCTION,
                    |s| {
                        // Prazo: produção + 2 dias para embalar; previsão: + o prazo da transportadora.
                        let production = s.deadline["production_days"].as_u64().unwrap_or(0);
                        let ship_by =
                            now + Duration::days(i64::try_from(production).unwrap_or(0) + 2);
                        s.deadline["ship_by"] = json!(saci_commerce_kit::clock::rfc3339(ship_by));
                        let eta = ship_by + Duration::days(i64::from(s.shipping.days.max));
                        s.shipping.estimated_delivery =
                            Some(saci_commerce_kit::clock::rfc3339(eta));
                        Ok(())
                    },
                    None,
                )
                .await;
            if let Err(e) = result
                && e.code != "invalid-state"
            {
                return Err(e);
            }
        }
        self.order_view(&self.order(order_id).await?).await
    }

    /// Cancela um pedido não pago.
    pub async fn cancel(&self, order_id: &str) -> Result<Value, Problem> {
        let (mut order, version) = self
            .store
            .load::<OrderRecord>(&keys::order(order_id), keys::META)
            .await?
            .ok_or_else(not_found)?;
        if order.status == "awaiting-payment" {
            order.status = "cancelled".into();
            order.updated_at = self.clock.rfc3339();
            self.store
                .write(vec![WriteOp::replace(
                    Item::new(keys::order(order_id), keys::META, &order)?,
                    version,
                )])
                .await?;
            for id in &order.sale_ids {
                let _ = self
                    .transition(id, &[st::PENDING_PAYMENT], st::CANCELLED, |_| Ok(()), None)
                    .await;
            }
        }
        self.order_view(&self.order(order_id).await?).await
    }
}

impl InternalHandler for Orders {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            match op {
                "create_order" => {
                    reply(&json!({ "order": self.create_order(payload(body)?).await? }))
                }
                "mark_paid" => reply(
                    &json!({ "order": self.mark_paid(&payload::<OrderRef>(body)?.order_id).await? }),
                ),
                "cancel" => reply(
                    &json!({ "order": self.cancel(&payload::<OrderRef>(body)?.order_id).await? }),
                ),
                "order_status" => {
                    let order = self.order(&payload::<OrderRef>(body)?.order_id).await?;
                    reply(
                        &json!({ "order": self.order_view(&order).await?, "status": order.status, "account_id": order.account_id }),
                    )
                }
                "listing_has_open_orders" => {
                    let id = payload::<ListingRef>(body)?.listing_id;
                    let open = !self
                        .store
                        .query_all(keys::listing(&id), "SAL#".into())
                        .await?
                        .is_empty();
                    reply(&json!({ "open": open }))
                }
                "storefront_to_do" => {
                    let id = payload::<StorefrontRef>(body)?.storefront_id;
                    let index = self
                        .store
                        .query_all(keys::storefront(&id), "SAL#".into())
                        .await?;
                    let count = index
                        .iter()
                        .filter(|i| {
                            group_of(i.doc["status"].as_str().unwrap_or_default())
                                == Some("a-fazer")
                        })
                        .count();
                    reply(&json!({ "count": count }))
                }
                "sale_for_review" => {
                    let r: SaleRef = payload(body)?;
                    let (sale, _) = self.sale(&r.sale_id).await?;
                    if sale.account_id.is_none() || sale.account_id != r.account_id {
                        return Err(not_found());
                    }
                    reply(&json!({
                        "storefront_id": sale.storefront_id,
                        "delivered": matches!(sale.status.as_str(), st::DELIVERED | st::INVOICED),
                        "city": sale.buyer["city"], "state": sale.buyer["state"],
                    }))
                }
                "sale_summary" => {
                    let (s, _) = self.sale(&payload::<SaleRef>(body)?.sale_id).await?;
                    reply(
                        &json!({ "storefront_id": s.storefront_id, "status": s.status, "number": s.number, "tracking_code": s.shipping.tracking_code }),
                    )
                }
                "sale_for_label" => {
                    let (s, _) = self.sale(&payload::<SaleRef>(body)?.sale_id).await?;
                    if s.status != st::AWAITING_SHIPMENT {
                        return Err(problem("label-not-ready"));
                    }
                    let address = self
                        .address_for_sale(&s, "label")
                        .await?
                        .ok_or_else(not_found)?;
                    let summary: Value = internal::call(
                        self.internal.as_ref(),
                        "sellers",
                        "storefront_summary",
                        &json!({ "storefront_id": s.storefront_id }),
                    )
                    .await
                    .unwrap_or_else(|_| json!({ "brand_name": s.storefront_name }));
                    let mut recipient =
                        vec![address["recipient"].as_str().unwrap_or_default().to_owned()];
                    recipient.extend(
                        address["lines"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned),
                    );
                    reply(&json!({
                        "storefront_id": s.storefront_id, "number": s.number, "service": s.shipping.service,
                        "sender": [summary["brand_name"].as_str().unwrap_or(&s.storefront_name)],
                        "recipient": recipient, "postal_code": address["postal_code"],
                    }))
                }
                "sale_parties" => {
                    let (s, _) = self.sale(&payload::<SaleRef>(body)?.sale_id).await?;
                    reply(
                        &json!({ "account_id": s.account_id, "storefront_id": s.storefront_id, "order_id": s.order_id }),
                    )
                }
                "order_parties" => {
                    let order = self.order(&payload::<OrderRef>(body)?.order_id).await?;
                    reply(&json!({ "account_id": order.account_id, "sale_ids": order.sale_ids }))
                }
                _ => Err(unknown_op("orders", op)),
            }
        })
    }
}

/// Consumidor que avança as vendas: etiqueta comprada, entrega, repasse.
#[derive(Debug, Clone)]
pub struct SaleProgress(pub Orders);

impl Consumer for SaleProgress {
    fn name(&self) -> &'static str {
        "orders-progress"
    }

    fn accepts(&self) -> &'static [&'static str] {
        &[
            "shipping.label.purchased.v1",
            "shipping.tracking.updated.v1",
            "finance.payout.sent.v1",
        ]
    }

    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
        Box::pin(async move {
            let o = &self.0;
            match event.event_type.as_str() {
                "shipping.label.purchased.v1" => {
                    let e: SaleRef = data(event)?;
                    for _ in 0..5 {
                        let (mut sale, version) = o.sale(&e.sale_id).await?;
                        sale.shipping.label_available = true;
                        match o
                            .store
                            .write(vec![WriteOp::replace(
                                Item::new(keys::sale(&sale.id), keys::META, &sale)?,
                                version,
                            )])
                            .await
                        {
                            Err(StoreError::Conflict) => {}
                            other => return other.map_err(ConsumeError::from),
                        }
                    }
                    Err(ConsumeError::Retry("conflito ao liberar a etiqueta".into()))
                }
                "shipping.tracking.updated.v1" => {
                    #[derive(Deserialize)]
                    struct Tracking {
                        sale_id: String,
                        status: String,
                    }
                    let e: Tracking = data(event)?;
                    if e.status == "delivered" {
                        let at = o.clock.rfc3339();
                        let result = o
                            .transition(
                                &e.sale_id,
                                &[st::SHIPPED],
                                st::DELIVERED,
                                |s| {
                                    s.shipping.delivered_at = Some(at.clone());
                                    Ok(())
                                },
                                Some("orders.sale.delivered.v1"),
                            )
                            .await;
                        if let Err(p) = result
                            && p.code != "invalid-state"
                        {
                            return Err(p.into());
                        }
                    }
                    Ok(())
                }
                _ => {
                    #[derive(Deserialize)]
                    struct Payout {
                        sale_ids: Vec<String>,
                    }
                    for id in data::<Payout>(event)?.sale_ids {
                        let _ = o
                            .transition(&id, &[st::DELIVERED], st::INVOICED, |_| Ok(()), None)
                            .await;
                    }
                    Ok(())
                }
            }
        })
    }
}

/// Usado pelo seed para gravar uma venda já num estado avançado.
pub(crate) fn index_ops(sale: &SaleRecord) -> Result<Vec<WriteOp>, Problem> {
    let mut ops = vec![
        WriteOp::put(Item::new(keys::sale(&sale.id), keys::META, sale)?),
        WriteOp::put(Item::new(
            keys::storefront(&sale.storefront_id),
            format!("SAL#{}", sale.id),
            &SaleIndex::from(sale),
        )?),
    ];
    if matches!(
        sale.status.as_str(),
        st::IN_PRODUCTION | st::AWAITING_SHIPMENT | st::SHIPPED | st::RETURN_OPENED
    ) {
        for item in &sale.items {
            ops.push(WriteOp::put(Item::new(
                keys::listing(&item.listing_id),
                format!("SAL#{}", sale.id),
                &json!({}),
            )?));
        }
    }
    let _ = sale_event;
    Ok(ops)
}
