//! Rotas do `orders-api` (proposta 5.8).
//!
//! | Rota | Acesso |
//! | --- | --- |
//! | `GET /v1/orders/me/purchases` | `USER` |
//! | `GET /v1/orders/me/purchases/{id}` | `USER` |
//! | `GET /v1/orders/guest/{id}` | `GUEST` (o pedido do token) |
//! | `GET /v1/orders/me/sales` | `SELLER:pedidos` (`group`, `cursor`) |
//! | `GET /v1/orders/me/sales/{id}` | `SELLER:pedidos` |
//! | `POST /v1/orders/me/sales/{id}/ready` | `SELLER:pedidos` (só em `in_production`) |
//! | `POST /v1/orders/me/sales/{id}/shipment` | `SELLER:pedidos` (só em `awaiting_shipment`) |
//!
//! Regras de segurança aplicadas aqui:
//!
//! - quem compra vê só a cidade de destino; a vitrine vê o endereço completo
//!   **só em `awaiting_shipment`**, aberto no cofre na hora (9.3);
//! - `payment` (comissão, líquido, repasse) só para a pessoa responsável, que é
//!   quem acessa o Financeiro (S-10);
//! - venda de outra vitrine é 404.

use std::sync::Arc;

use saci_commerce_kit::auth::{self, Role, Section};
use saci_commerce_kit::clock::Clock;
use saci_commerce_kit::cursor::CursorSigner;
use saci_commerce_kit::events::{self, Event};
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::internal::{self, InternalClient};
use saci_commerce_kit::problem::{invalid, not_found, problem};
use saci_commerce_kit::store::{DocumentStore, Item, Query, StoreError, StoreExt, WriteOp};
use saci_commerce_kit::types::{Money, Page};
use saci_commerce_kit::{BoxFut, Problem};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::{OrderRecord, SaleIndex, SaleRecord, group_of, keys, sale_status as st};

/// Compras por página.
const PURCHASE_PAGE: usize = 10;
/// Vendas por página.
const SALE_PAGE: usize = 10;

/// Dependências do serviço.
#[derive(Debug, Clone)]
pub struct Orders {
    /// Tabela.
    pub store: Arc<dyn DocumentStore>,
    /// Chamadas a `identity` (cofre: endereço) e `finance` (valores da venda).
    pub internal: Arc<dyn InternalClient>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Cursores.
    pub cursors: CursorSigner,
}

impl HttpService for Orders {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

/// Situação do pedido vista por quem compra, a partir das vendas.
#[must_use]
pub fn order_status(order: &OrderRecord, sales: &[SaleRecord]) -> String {
    if order.status == "cancelled" || order.status == "awaiting-payment" {
        return order.status.clone();
    }
    let pkg: Vec<&str> = sales.iter().map(|s| package_status(&s.status)).collect();
    if pkg.is_empty() {
        return order.status.clone();
    }
    if pkg.iter().all(|s| *s == "delivered") {
        "delivered".into()
    } else if pkg.iter().all(|s| matches!(*s, "shipped" | "delivered")) {
        "shipped".into()
    } else if sales.iter().all(|s| s.status == st::IN_PRODUCTION) {
        // Pago e ainda nenhuma vitrine avançou.
        "paid".into()
    } else {
        "preparing".into()
    }
}

/// Situação de um pacote no vocabulário de quem compra.
#[must_use]
pub fn package_status(sale: &str) -> &'static str {
    match sale {
        st::PENDING_PAYMENT => "awaiting-payment",
        st::IN_PRODUCTION | st::AWAITING_SHIPMENT => "preparing",
        st::SHIPPED => "shipped",
        st::CANCELLED => "cancelled",
        _ => "delivered",
    }
}

impl Orders {
    async fn route(&self, req: &Request) -> HttpResult {
        match (req.method.as_str(), req.segments().as_slice()) {
            ("GET", ["v1", "orders", "me", "purchases"]) => self.purchases(req).await,
            ("GET", ["v1", "orders", "me", "purchases", id]) => {
                let account = auth::user(req)?.to_owned();
                let order = self.order(id).await?;
                if order.account_id.as_deref() != Some(account.as_str()) {
                    return Err(not_found());
                }
                Ok(Response::ok(&self.purchase(&order).await?))
            }
            ("GET", ["v1", "orders", "guest", id]) => {
                if auth::guest_order(req)? != *id {
                    return Err(not_found());
                }
                Ok(Response::ok(&self.purchase(&self.order(id).await?).await?))
            }
            ("GET", ["v1", "orders", "me", "sales"]) => self.sales(req).await,
            ("GET", ["v1", "orders", "me", "sales", id]) => {
                let storefront = auth::section(req, Section::Pedidos)?.to_owned();
                let sale = self.owned_sale(&storefront, id).await?;
                Ok(Response::ok(
                    &self
                        .sale_view(&sale, auth::role(req) == Some(Role::Owner))
                        .await?,
                ))
            }
            ("POST", ["v1", "orders", "me", "sales", id, "ready"]) => self.ready(req, id).await,
            ("POST", ["v1", "orders", "me", "sales", id, "shipment"]) => self.ship(req, id).await,
            _ => Err(not_found()),
        }
    }

    // --- compras ---------------------------------------------------------------------------

    async fn purchases(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        let scope = format!("{account}:purchases");
        let after = req
            .query("cursor")
            .map(|c| self.cursors.decode(&scope, c))
            .transpose()?;
        let page = self
            .store
            .query(
                Query::prefix(keys::account(&account), "ORD#")
                    .limit(PURCHASE_PAGE)
                    .after(after)
                    .desc(),
            )
            .await?;
        let mut items = Vec::with_capacity(page.items.len());
        for idx in &page.items {
            let id = idx.sk.rsplit('#').next().unwrap_or_default();
            if let Ok(order) = self.order(id).await {
                items.push(self.purchase(&order).await?);
            }
        }
        let next_cursor = page.last.map(|l| self.cursors.encode(&scope, &l));
        Ok(Response::ok(&Page { items, next_cursor }))
    }

    /// Um pedido (404 se não existe).
    pub async fn order(&self, id: &str) -> Result<OrderRecord, Problem> {
        Ok(self
            .store
            .load::<OrderRecord>(&keys::order(id), keys::META)
            .await?
            .ok_or_else(not_found)?
            .0)
    }

    /// Uma venda (404 se não existe).
    pub async fn sale(&self, id: &str) -> Result<(SaleRecord, u64), Problem> {
        self.store
            .load::<SaleRecord>(&keys::sale(id), keys::META)
            .await?
            .ok_or_else(not_found)
    }

    async fn sales_of(&self, order: &OrderRecord) -> Result<Vec<SaleRecord>, Problem> {
        let mut out = Vec::new();
        for id in &order.sale_ids {
            out.push(self.sale(id).await?.0);
        }
        Ok(out)
    }

    /// `Order` do contrato (resposta do checkout e base da `Purchase`).
    pub async fn order_view(&self, order: &OrderRecord) -> Result<Value, Problem> {
        let sales = self.sales_of(order).await?;
        Ok(json!({
            "id": order.id, "number": order.number, "created_at": order.created_at,
            "status": order_status(order, &sales),
            "lines": order.lines.iter().map(|l| json!({
                "listing_id": l.listing_id, "title": l.title, "storefront_name": l.storefront_name,
                "quantity": l.quantity, "unit_price": l.unit_price,
            })).collect::<Vec<_>>(),
            "subtotal": order.subtotal, "shipping": order.shipping, "total": order.total,
            "package_count": order.sale_ids.len(), "delivery_days": order.delivery_days,
        }))
    }

    /// `Purchase`: o pedido, os pacotes por vitrine e o destino (só cidade e UF).
    pub async fn purchase(&self, order: &OrderRecord) -> Result<Value, Problem> {
        let sales = self.sales_of(order).await?;
        let mut v = self.order_view(order).await?;
        v["updated_at"] = json!(
            sales
                .iter()
                .map(|s| s.updated_at.clone())
                .chain([order.updated_at.clone()])
                .max()
        );
        v["destination"] = json!(order.destination);
        v["pieces"] = json!(order
            .lines
            .iter()
            .map(|l| {
                let mut p = json!({ "listing_id": l.listing_id, "title": l.title, "storefront_name": l.storefront_name, "quantity": l.quantity });
                if let Some(url) = &l.photo_url {
                    p["image"] = json!({ "url": url, "alt": l.title });
                }
                p
            })
            .collect::<Vec<_>>());
        v["packages"] = json!(sales
            .iter()
            .map(|s| {
                let mut p = json!({ "storefront_name": s.storefront_name, "status": package_status(&s.status) });
                if let Some(code) = &s.shipping.tracking_code {
                    p["tracking_code"] = json!(code);
                }
                if let Some(eta) = &s.shipping.estimated_delivery {
                    p["estimated_delivery"] = json!(eta);
                }
                p
            })
            .collect::<Vec<_>>());
        if let Some(code) = sales.iter().find_map(|s| s.shipping.tracking_code.clone()) {
            v["tracking_code"] = json!(code);
        }
        Ok(v)
    }

    // --- vendas -----------------------------------------------------------------------------

    async fn sales(&self, req: &Request) -> HttpResult {
        let storefront = auth::section(req, Section::Pedidos)?.to_owned();
        let group = req
            .query_enum(
                "group",
                &["a-fazer", "a-caminho", "concluidos", "devolucoes"],
            )?
            .unwrap_or("a-fazer");
        let index = self
            .store
            .query_all(keys::storefront(&storefront), "SAL#".into())
            .await?;
        let mut entries: Vec<SaleIndex> =
            index.iter().map(Item::parse).collect::<Result<_, _>>()?;
        let mut counts = json!({ "a-fazer": 0, "a-caminho": 0, "concluidos": 0, "devolucoes": 0 });
        for e in &entries {
            if let Some(g) = group_of(&e.status) {
                counts[g] = json!(counts[g].as_u64().unwrap_or(0) + 1);
            }
        }
        entries.retain(|e| group_of(&e.status) == Some(group));
        // A fazer: o prazo mais perto primeiro. Os demais: os mais recentes.
        if group == "a-fazer" {
            entries.sort_by(|a, b| a.ship_by.cmp(&b.ship_by).then(a.sale_id.cmp(&b.sale_id)));
        } else {
            entries.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then(a.sale_id.cmp(&b.sale_id))
            });
        }
        let scope = format!("{storefront}:sales:{group}");
        let start = match req.query("cursor") {
            Some(c) => self
                .cursors
                .decode(&scope, c)?
                .parse::<usize>()
                .map_err(|_| invalid("cursor", "Cursor inválido."))?,
            None => 0,
        };
        let owner = auth::role(req) == Some(Role::Owner);
        let mut items = Vec::new();
        for e in entries.iter().skip(start).take(SALE_PAGE) {
            items.push(
                self.sale_view(&self.sale(&e.sale_id).await?.0, owner)
                    .await?,
            );
        }
        let mut body = json!({ "items": items, "counts": counts });
        if start + SALE_PAGE < entries.len() {
            body["next_cursor"] = json!(
                self.cursors
                    .encode(&scope, &(start + SALE_PAGE).to_string())
            );
        }
        Ok(Response::ok(&body))
    }

    async fn owned_sale(&self, storefront: &str, id: &str) -> Result<SaleRecord, Problem> {
        match self
            .store
            .load::<SaleRecord>(&keys::sale(id), keys::META)
            .await?
        {
            Some((s, _)) if s.storefront_id == storefront && group_of(&s.status).is_some() => Ok(s),
            _ => Err(not_found()),
        }
    }

    /// `Sale` do contrato.
    pub async fn sale_view(&self, s: &SaleRecord, owner: bool) -> Result<Value, Problem> {
        let mut shipping = json!({
            "service": s.shipping.service, "price": s.shipping.price, "label_available": s.shipping.label_available,
        });
        for (k, v) in [
            ("tracking_code", &s.shipping.tracking_code),
            ("shipped_at", &s.shipping.shipped_at),
            ("delivered_at", &s.shipping.delivered_at),
            ("estimated_delivery", &s.shipping.estimated_delivery),
        ] {
            if let Some(v) = v {
                shipping[k] = json!(v);
            }
        }
        if s.status == st::AWAITING_SHIPMENT
            && let Some(address) = self.address_for_sale(s, "sale").await?
        {
            shipping["address"] = address;
        }
        let mut v = json!({
            "id": s.id, "number": s.number, "status": s.status, "placed_at": s.placed_at, "updated_at": s.updated_at,
            "buyer": s.buyer, "items": s.items, "deadline": s.deadline, "shipping": shipping, "history": s.history,
        });
        for (k, val) in [("invoice", &s.invoice), ("return", &s.return_info)] {
            if let Some(val) = val {
                v[k] = val.clone();
            }
        }
        if let Some(p) = &s.ticket_protocol {
            v["ticket_protocol"] = json!(p);
        }
        if owner {
            // S-10: os valores vêm do `finance` e só para quem acessa o Financeiro.
            let items_total = s.items_total();
            match internal::call::<_, Value>(
                self.internal.as_ref(),
                "finance",
                "sale_payment",
                &json!({ "sale_id": s.id, "storefront_id": s.storefront_id, "items_total": items_total, "status": s.status }),
            )
            .await
            {
                Ok(p) => v["payment"] = json!({ "items_total": items_total, "fee": p["fee"], "net": p["net"], "payout_date": p["payout_date"] }),
                Err(e) => tracing::warn!(code = %e.code, "valores da venda indisponíveis"),
            }
            if v["payment"]["payout_date"].is_null()
                && let Some(o) = v["payment"].as_object_mut()
            {
                o.remove("payout_date");
            }
        }
        Ok(v)
    }

    /// Endereço de entrega aberto no cofre: `{ recipient, lines, postal_code }`.
    pub async fn address_for_sale(
        &self,
        s: &SaleRecord,
        purpose: &str,
    ) -> Result<Option<Value>, Problem> {
        if let Some(fixture) = &s.address_fixture {
            return Ok(Some(fixture.clone()));
        }
        let Some(reference) = &s.address_ref else {
            return Ok(None);
        };
        #[derive(Deserialize)]
        struct Opened {
            address: saci_commerce_kit::types::ShippingAddress,
        }
        let opened: Opened = internal::call(
            self.internal.as_ref(),
            "identity",
            "open_address",
            &json!({ "ref": reference, "purpose": purpose, "storefront_id": s.storefront_id, "sale_id": s.id }),
        )
        .await?;
        Ok(Some(json!({
            "recipient": opened.address.recipient(), "lines": opened.address.lines(),
            "postal_code": opened.address.postal_code().unwrap_or_default(),
        })))
    }

    async fn ready(&self, req: &Request, id: &str) -> HttpResult {
        let storefront = auth::section(req, Section::Pedidos)?.to_owned();
        self.owned_sale(&storefront, id).await?;
        let sale = self
            .transition(
                id,
                &[st::IN_PRODUCTION],
                st::AWAITING_SHIPMENT,
                |_| Ok(()),
                Some("orders.sale.ready.v1"),
            )
            .await?;
        Ok(Response::ok(
            &self
                .sale_view(&sale, auth::role(req) == Some(Role::Owner))
                .await?,
        ))
    }

    async fn ship(&self, req: &Request, id: &str) -> HttpResult {
        let storefront = auth::section(req, Section::Pedidos)?.to_owned();
        #[derive(Deserialize)]
        struct Body {
            tracking_code: String,
        }
        let body: Body = req.json()?;
        let code = body.tracking_code.trim().to_ascii_uppercase();
        if !(8..=40).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(invalid("tracking_code", "Confira o código de rastreio."));
        }
        self.owned_sale(&storefront, id).await?;
        let at = self.clock.rfc3339();
        let sale = self
            .transition(
                id,
                &[st::AWAITING_SHIPMENT],
                st::SHIPPED,
                |s| {
                    s.shipping.tracking_code = Some(code.clone());
                    s.shipping.shipped_at = Some(at.clone());
                    Ok(())
                },
                Some("orders.sale.shipped.v1"),
            )
            .await?;
        Ok(Response::ok(
            &self
                .sale_view(&sale, auth::role(req) == Some(Role::Owner))
                .await?,
        ))
    }

    /// Muda a situação de uma venda, com histórico, índice do painel, venda em
    /// aberto da peça e evento, numa transação. Situação fora de `from`: 409.
    pub async fn transition(
        &self,
        id: &str,
        from: &[&str],
        to: &str,
        change: impl Fn(&mut SaleRecord) -> Result<(), Problem>,
        event: Option<&str>,
    ) -> Result<SaleRecord, Problem> {
        for _ in 0..5 {
            let (mut sale, version) = self.sale(id).await?;
            if !from.contains(&sale.status.as_str()) {
                return Err(problem("invalid-state"));
            }
            let at = self.clock.rfc3339();
            sale.status = to.to_owned();
            sale.updated_at = at.clone();
            sale.history.push(json!({ "status": to, "at": at }));
            change(&mut sale)?;
            let mut ops = vec![
                WriteOp::replace(Item::new(keys::sale(id), keys::META, &sale)?, version),
                WriteOp::put(Item::new(
                    keys::storefront(&sale.storefront_id),
                    format!("SAL#{id}"),
                    &SaleIndex::from(&sale),
                )?),
            ];
            if matches!(
                to,
                st::DELIVERED | st::INVOICED | st::CANCELLED | st::RETURNED
            ) {
                for item in &sale.items {
                    ops.push(WriteOp::delete(
                        keys::listing(&item.listing_id),
                        format!("SAL#{id}"),
                    ));
                }
            }
            if let Some(t) = event {
                ops.push(WriteOp::Publish(sale_event(t, &sale)?));
            }
            match self.store.write(ops).await {
                Ok(()) => return Ok(sale),
                Err(StoreError::Conflict) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(problem("invalid-state"))
    }
}

/// `orders.sale.*.v1`.
pub fn sale_event(event_type: &str, sale: &SaleRecord) -> Result<Event, Problem> {
    events::event(
        "orders",
        event_type,
        &sale.id,
        &json!({ "sale_id": sale.id, "order_id": sale.order_id, "storefront_id": sale.storefront_id }),
    )
}

/// Soma de valores.
#[must_use]
pub fn sum(values: impl Iterator<Item = i64>) -> Money {
    Money::brl(values.sum())
}
