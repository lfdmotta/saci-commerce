//! Migração `0001-demo-orders`: compras da conta de exemplo e vendas do
//! Ateliê Barro Vivo (só fora de produção; dados fictícios).
//!
//! As linhas das compras são preenchidas com o preço e o nome do catálogo
//! (chamada interna `checkout_items`), e o endereço de entrega vai para o
//! cofre, como numa compra de verdade.

use std::collections::BTreeMap;

use saci_commerce_kit::Problem;
use saci_commerce_kit::internal;
use saci_commerce_kit::store::{Item, WriteOp};
use saci_commerce_kit::types::Money;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::Orders;
use crate::internal::index_ops;
use crate::model::{
    Days, OrderLine, OrderRecord, SaleItem, SaleRecord, SaleShipping, keys, sale_status as st,
};

const DEMO: &str = include_str!("../migrations/0001-demo-orders.json");

#[derive(Debug, Deserialize)]
struct Demo {
    buyer_address: Value,
    purchases: Vec<Purchase>,
    sales: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct Purchase {
    id: String,
    number: String,
    account_id: String,
    created_at: String,
    updated_at: String,
    status: String,
    #[serde(default)]
    tracking_code: Option<String>,
    items: Vec<PurchaseItem>,
}

#[derive(Debug, Deserialize)]
struct PurchaseItem {
    listing_id: String,
    quantity: u32,
}

/// Aplica a migração.
pub async fn migrate(orders: &Orders, allow_fixtures: bool) -> Result<Vec<String>, Problem> {
    if !allow_fixtures
        || orders
            .store
            .get("MIGRATION", "0001-demo-orders")
            .await?
            .is_some()
    {
        return Ok(Vec::new());
    }
    let demo: Demo =
        serde_json::from_str(DEMO).map_err(|e| saci_commerce_kit::problem::internal(&e))?;
    for p in &demo.purchases {
        purchase(orders, p, &demo.buyer_address).await?;
    }
    for raw in demo.sales {
        let sale = seller_sale(&raw)?;
        orders.store.write(index_ops(&sale)?).await?;
    }
    orders
        .store
        .write(vec![WriteOp::create(Item::new(
            "MIGRATION",
            "0001-demo-orders",
            &json!({ "at": orders.clock.rfc3339() }),
        )?)])
        .await?;
    Ok(vec!["0001-demo-orders".into()])
}

async fn purchase(orders: &Orders, p: &Purchase, address: &Value) -> Result<(), Problem> {
    #[derive(Deserialize)]
    struct Items {
        items: Vec<Value>,
    }
    let found: Items = internal::call(
        orders.internal.as_ref(),
        "catalog",
        "checkout_items",
        &json!({ "items": p.items.iter().map(|i| json!({ "listing_id": i.listing_id, "quantity": 1 })).collect::<Vec<_>>() }),
    )
    .await?;
    #[derive(Deserialize)]
    struct Ref {
        #[serde(rename = "ref")]
        reference: String,
    }
    let saved: Ref = internal::call(
        orders.internal.as_ref(),
        "identity",
        "put_address",
        &json!({ "owner_account_id": p.account_id, "purpose": "delivery", "address": address }),
    )
    .await?;
    let lines: Vec<OrderLine> = p
        .items
        .iter()
        .zip(&found.items)
        .map(|(i, f)| OrderLine {
            listing_id: i.listing_id.clone(),
            title: f["title"].as_str().unwrap_or_default().to_owned(),
            storefront_id: f["storefront_id"].as_str().unwrap_or_default().to_owned(),
            storefront_name: f["storefront_name"].as_str().unwrap_or_default().to_owned(),
            quantity: i.quantity,
            unit_price: serde_json::from_value(f["unit_price"].clone())
                .unwrap_or_else(|_| Money::zero()),
            photo_url: f["cover_url"].as_str().map(str::to_owned),
            production_days: 0,
        })
        .collect();
    let mut by_store: BTreeMap<String, Vec<&OrderLine>> = BTreeMap::new();
    for l in &lines {
        by_store.entry(l.storefront_id.clone()).or_default().push(l);
    }
    let sale_status = match p.status.as_str() {
        "preparing" => st::AWAITING_SHIPMENT,
        "shipped" => st::SHIPPED,
        _ => st::DELIVERED,
    };
    let mut sales = Vec::new();
    for (n, (storefront, ls)) in by_store.iter().enumerate() {
        sales.push(SaleRecord {
            id: saci_ids::Id::new(saci_ids::prefix::SALE).to_string(),
            order_id: p.id.clone(),
            number: format!("{}-{}", p.number, n + 1),
            storefront_id: storefront.clone(),
            storefront_name: ls[0].storefront_name.clone(),
            account_id: Some(p.account_id.clone()),
            status: sale_status.into(),
            placed_at: p.created_at.clone(),
            updated_at: p.updated_at.clone(),
            buyer: json!({ "first_name": "Marina", "city": "Fortaleza", "state": "CE" }),
            items: ls
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
            deadline: json!({ "ship_by": p.updated_at, "source": "listing", "production_days": 0 }),
            shipping: SaleShipping {
                service: "Correios PAC".into(),
                price: Money::brl(2_390),
                days: Days { min: 5, max: 9 },
                label_available: sale_status != st::IN_PRODUCTION,
                tracking_code: p
                    .tracking_code
                    .clone()
                    .filter(|_| sale_status != st::IN_PRODUCTION),
                shipped_at: (sale_status != st::IN_PRODUCTION).then(|| p.updated_at.clone()),
                delivered_at: (sale_status == st::DELIVERED).then(|| p.updated_at.clone()),
                estimated_delivery: None,
            },
            address_ref: Some(saved.reference.clone()),
            address_fixture: None,
            invoice: Some(json!({ "required": false })),
            return_info: None,
            ticket_protocol: None,
            history: vec![json!({ "status": sale_status, "at": p.updated_at })],
        });
    }
    let subtotal: i64 = lines
        .iter()
        .map(|l| l.unit_price.amount * i64::from(l.quantity))
        .sum();
    let freight = 2_390 * i64::try_from(sales.len()).unwrap_or(1);
    let order = OrderRecord {
        id: p.id.clone(),
        number: p.number.clone(),
        account_id: Some(p.account_id.clone()),
        status: "paid".into(),
        lines: lines.clone(),
        subtotal: Money::brl(subtotal),
        shipping: Money::brl(freight),
        total: Money::brl(subtotal + freight),
        delivery_days: Days { min: 5, max: 9 },
        address_ref: Some(saved.reference),
        destination: "Fortaleza, CE".into(),
        payment_method: "credito".into(),
        sale_ids: sales.iter().map(|s| s.id.clone()).collect(),
        created_at: p.created_at.clone(),
        updated_at: p.updated_at.clone(),
    };
    let mut ops = vec![
        WriteOp::put(Item::new(keys::order(&order.id), keys::META, &order)?),
        WriteOp::put(Item::new(
            keys::account(&p.account_id),
            format!("ORD#{}#{}", p.created_at, p.id),
            &json!({}),
        )?),
    ];
    for s in &sales {
        ops.extend(index_ops(s)?);
    }
    orders.store.write(ops).await?;
    Ok(())
}

/// Venda da semente, no formato do contrato (`SellerOrderResponse` do mock).
fn seller_sale(raw: &Value) -> Result<SaleRecord, Problem> {
    let get = |k: &str| raw[k].clone();
    let shipping = get("shipping");
    Ok(SaleRecord {
        id: raw["id"].as_str().unwrap_or_default().to_owned(),
        order_id: raw["order_id"].as_str().unwrap_or_default().to_owned(),
        number: raw["number"].as_str().unwrap_or_default().to_owned(),
        storefront_id: raw["storefront_id"].as_str().unwrap_or_default().to_owned(),
        storefront_name: "Ateliê Barro Vivo".into(),
        account_id: None,
        status: raw["status"].as_str().unwrap_or(st::IN_PRODUCTION).to_owned(),
        placed_at: raw["placed_at"].as_str().unwrap_or_default().to_owned(),
        updated_at: raw["updated_at"].as_str().unwrap_or_default().to_owned(),
        buyer: get("buyer"),
        items: serde_json::from_value(get("items")).map_err(|e| saci_commerce_kit::problem::internal(&e))?,
        deadline: get("deadline"),
        shipping: SaleShipping {
            service: shipping["service"].as_str().unwrap_or("Correios PAC").to_owned(),
            price: serde_json::from_value(shipping["price"].clone()).unwrap_or_else(|_| Money::brl(2_390)),
            days: Days { min: 3, max: 8 },
            label_available: shipping["label_available"].as_bool().unwrap_or(false),
            tracking_code: shipping["tracking_code"].as_str().map(str::to_owned),
            shipped_at: shipping["shipped_at"].as_str().map(str::to_owned),
            delivered_at: shipping["delivered_at"].as_str().map(str::to_owned),
            estimated_delivery: shipping["estimated_delivery"].as_str().map(str::to_owned),
        },
        address_ref: None,
        // Endereço fictício quando o exemplo não traz (vendas ainda em produção).
        address_fixture: Some(shipping["address"].clone()).filter(|a| !a.is_null()).or_else(|| {
            Some(json!({
                "recipient": format!("{} (exemplo)", raw["buyer"]["first_name"].as_str().unwrap_or("Cliente")),
                "lines": ["Rua de Exemplo, 100", format!("{} — {}", raw["buyer"]["city"].as_str().unwrap_or("Fortaleza"), raw["buyer"]["state"].as_str().unwrap_or("CE"))],
                "postal_code": "60175047",
            }))
        }),
        invoice: Some(get("invoice")).filter(|v| !v.is_null()),
        return_info: Some(get("return")).filter(|v| !v.is_null()),
        ticket_protocol: raw["ticket_protocol"].as_str().map(str::to_owned),
        history: raw["history"].as_array().cloned().unwrap_or_default(),
    })
}
