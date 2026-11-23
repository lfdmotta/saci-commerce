//! `checkout-saga`: a compra (proposta 3.3 e 5.7).
//!
//! | Rota | Acesso |
//! | --- | --- |
//! | `POST /v1/checkout` | `USER`, ou `PUB-L` com `buyer` + `Idempotency-Key` |
//! | `GET /v1/checkout/orders/{id}/payment` | `USER` (o dono) ou `GUEST` (o pedido do token) |
//!
//! A saga, com compensação em cada passo:
//!
//! ```text
//! preço e estoque (catalog) → frete (shipping) → total confere com `expected_total`?
//!   → endereço e comprador no cofre (identity) → reserva do estoque (catalog, escrita condicional)
//!   → pedido e vendas (orders) → cobrança (payments, com split por vitrine)
//!       cartão aprovado → pedido pago, reserva confirmada, `checkout.order.confirmed.v1`
//!       cartão recusado → solta a reserva, cancela o pedido, `checkout.order.abandoned.v1`, 402
//!       Pix             → aguardando até 30 min (`payments.pix.paid/expired.v1`)
//! ```
//!
//! O preço nunca vem do cliente. Nenhum dado pessoal crítico fica no estado da
//! saga (D9): CPF e endereço vão para o cofre antes, e a saga guarda só as
//! referências. Uma varredura agendada compensa sagas interrompidas
//! (`sweep`), e a reserva tem prazo como última garantia. ADR 0007 explica a
//! saga em Rust em vez de Step Functions.

use std::collections::BTreeMap;
use std::sync::Arc;

use saci_commerce_kit::auth::{self, PrincipalKind};
use saci_commerce_kit::clock::{Clock, rfc3339_epoch};
use saci_commerce_kit::events::{self, ConsumeError, Consumer, Event, data};
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::idempotency::{self, StoreIdempotency};
use saci_commerce_kit::internal::{self, InternalClient};
use saci_commerce_kit::jwt::TokenSigner;
use saci_commerce_kit::problem::{invalid, not_found, problem};
use saci_commerce_kit::store::{DocumentStore, Item, StoreExt, WriteOp};
use saci_commerce_kit::text::first_name;
use saci_commerce_kit::types::{Money, ShippingAddress};
use saci_commerce_kit::{BoxFut, Problem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Prazo da reserva de estoque: a varredura compensa bem antes disso.
const RESERVATION_TTL_SECS: u64 = 2 * 3600;
/// Saga sem resposta por mais que isto é compensada pela varredura.
const STUCK_SECS: u64 = 120;
/// Folga depois do vencimento do Pix antes de compensar por conta própria.
const PIX_GRACE_SECS: u64 = 5 * 60;

/// Dependências do serviço.
#[derive(Debug, Clone)]
pub struct Checkout {
    /// Tabela (idempotência e estado das sagas).
    pub store: Arc<dyn DocumentStore>,
    /// `catalog`, `shipping`, `orders`, `identity` (cofre) e `payments`.
    pub internal: Arc<dyn InternalClient>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Assinatura do guest token.
    pub guest_signer: Arc<dyn TokenSigner>,
    /// Emissor (`iss`) do guest token, igual ao cadastrado na tabela `authz`.
    pub guest_issuer: String,
    /// Audiência do guest token.
    pub guest_audience: String,
    /// Validade do guest token (a proposta pede 30 dias; limitado pela vida máxima do emissor).
    pub guest_ttl_secs: u64,
}

/// Estado de uma saga (só ids e valores, nada de C3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Saga {
    /// Pedido.
    pub order_id: String,
    /// `started`, `awaiting_payment`, `confirmed`, `abandoned`.
    pub state: String,
    /// Conta, quando há.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Peças.
    pub items: Vec<ItemRequest>,
    /// Vitrines.
    pub storefront_ids: Vec<String>,
    /// Até quando esperar (segundos).
    pub deadline: u64,
    /// Método.
    pub method: String,
    /// Pix gerado, para repetir no `GET`.
    #[serde(default)]
    pub pix: Option<Value>,
}

/// Peça pedida.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemRequest {
    /// Anúncio.
    pub listing_id: String,
    /// Quantidade.
    pub quantity: u32,
}

#[derive(Debug, Deserialize)]
struct Buyer {
    /// "CPF na nota fiscal?": opcional (#27). Informado, vai para a nota.
    #[serde(default)]
    cpf: Option<String>,
    name: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    phone: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Payment {
    method: String,
    #[serde(default)]
    card_token: Option<String>,
    #[serde(default)]
    saved_card_id: Option<String>,
    #[serde(default)]
    save_card: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CheckoutRequest {
    items: Vec<ItemRequest>,
    shipping_address: ShippingAddress,
    #[serde(default)]
    buyer: Option<Buyer>,
    payment: Payment,
    expected_total: Money,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogItem {
    listing_id: String,
    available: bool,
    title: String,
    storefront_id: String,
    storefront_name: String,
    unit_price: Money,
    production_days: u32,
    #[serde(default)]
    cover_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Quote {
    artisan_id: String,
    service: String,
    price: Money,
    days: Value,
}

#[derive(Debug, Deserialize)]
struct ChargeResult {
    status: String,
    #[serde(default)]
    pix: Option<Value>,
    #[serde(default)]
    next_action: Option<Value>,
}

fn saga_pk(order_id: &str) -> String {
    format!("SAGA#{order_id}")
}

impl HttpService for Checkout {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

impl Checkout {
    async fn route(&self, req: &Request) -> HttpResult {
        match (req.method.as_str(), req.segments().as_slice()) {
            ("POST", ["v1", "checkout"]) => self.checkout(req).await,
            ("GET", ["v1", "checkout", "orders", id, "payment"]) => {
                self.payment_status(req, id).await
            }
            _ => Err(not_found()),
        }
    }

    async fn checkout(&self, req: &Request) -> HttpResult {
        let account = auth::optional_user(req).map(str::to_owned);
        let body: CheckoutRequest = req.json()?;
        // Escopo da chave: a conta, ou o IP na compra sem conta (S-06).
        let scope = account
            .clone()
            .unwrap_or_else(|| format!("guest:{}", req.source_ip.clone().unwrap_or_default()));
        let store = StoreIdempotency(Arc::clone(&self.store));
        idempotency::run(&store, req, &scope, self.clock.epoch(), || {
            self.run(account.clone(), body)
        })
        .await
    }

    fn validate(account: Option<&str>, body: &CheckoutRequest) -> Result<(), Problem> {
        if body.items.is_empty()
            || body.items.len() > 50
            || body
                .items
                .iter()
                .any(|i| i.quantity == 0 || i.quantity > 99)
        {
            return Err(invalid("items", "A sacola precisa ter de 1 a 50 peças."));
        }
        if let Err((field, message)) = body.shipping_address.validate() {
            return Err(invalid(&format!("shipping_address.{field}"), message));
        }
        if !matches!(body.shipping_address, ShippingAddress::Brasil { .. }) {
            return Err(invalid(
                "shipping_address",
                "Por enquanto, as vitrines só entregam no Brasil.",
            ));
        }
        if account.is_none() {
            let buyer = body
                .buyer
                .as_ref()
                .ok_or_else(|| invalid("buyer", "Informe quem está comprando."))?;
            if let Some(cpf) = buyer.cpf.as_deref().filter(|c| !c.is_empty())
                && (!saci_mask::is_valid_cpf(cpf) || cpf.len() != 11)
            {
                return Err(invalid("buyer.cpf", "Confira o CPF."));
            }
            if buyer.name.trim().chars().count() < 3 {
                return Err(invalid("buyer.name", "Informe o nome completo."));
            }
            if buyer.email.is_none() && buyer.phone.is_none() {
                return Err(invalid(
                    "buyer.email",
                    "Informe um e-mail ou um celular para receber o pedido.",
                ));
            }
            // Só o formato: o DDI na lista aceita é conferido pelo cofre.
            if buyer
                .phone
                .as_deref()
                .is_some_and(|p| saci_commerce_kit::text::normalize_phone(p).is_none())
            {
                return Err(invalid("buyer.phone", "Confira o celular, com o DDD."));
            }
        }
        match body.payment.method.as_str() {
            "pix" => {}
            "credito" | "debito" if body.payment.card_token.is_some() => {}
            "credito" | "debito" => {
                return Err(invalid("payment.card_token", "Confira os dados do cartão."));
            }
            _ => return Err(invalid("payment.method", "Forma de pagamento não aceita.")),
        }
        if body.payment.saved_card_id.is_some() && account.is_none() {
            return Err(invalid(
                "payment.saved_card_id",
                "Cartão salvo só com conta.",
            ));
        }
        if !body.expected_total.is_valid() {
            return Err(invalid("expected_total", "Informe o total em centavos."));
        }
        Ok(())
    }

    /// A saga. Devolve a resposta `201` ou a recusa, já compensada.
    async fn run(&self, account: Option<String>, body: CheckoutRequest) -> HttpResult {
        Self::validate(account.as_deref(), &body)?;
        let client = self.internal.as_ref();

        // 1. Preço e estoque do catálogo: o preço nunca vem do cliente.
        #[derive(Deserialize)]
        struct Items {
            items: Vec<CatalogItem>,
        }
        let catalog: Items = internal::call(
            client,
            "catalog",
            "checkout_items",
            &json!({ "items": body.items }),
        )
        .await?;
        if let Some(gone) = catalog.items.iter().find(|i| !i.available) {
            let message = if gone.title.is_empty() {
                "Uma das peças saiu da feira.".to_owned()
            } else {
                format!(
                    "\"{}\" não está mais disponível nessa quantidade.",
                    gone.title
                )
            };
            return Err(problem("item-unavailable").with_message(message));
        }

        // 2. Frete por vitrine (um pacote por vitrine).
        let mut by_store: BTreeMap<String, Vec<&ItemRequest>> = BTreeMap::new();
        for (req_item, cat) in body.items.iter().zip(&catalog.items) {
            by_store
                .entry(cat.storefront_id.clone())
                .or_default()
                .push(req_item);
        }
        let postal_code = body
            .shipping_address
            .postal_code()
            .unwrap_or_default()
            .to_owned();
        #[derive(Deserialize)]
        struct Quotes {
            items: Vec<Quote>,
        }
        let quotes: Quotes = internal::call(
            client,
            "shipping",
            "quote",
            &json!({ "postal_code": postal_code, "storefronts": by_store.iter().map(|(s, items)| json!({ "artisan_id": s, "items": items })).collect::<Vec<_>>() }),
        )
        .await?;

        // 3. O total confere com o que a pessoa viu?
        let subtotal: i64 = body
            .items
            .iter()
            .zip(&catalog.items)
            .map(|(r, c)| c.unit_price.amount * i64::from(r.quantity))
            .sum();
        let freight: i64 = quotes.items.iter().map(|q| q.price.amount).sum();
        let total = subtotal + freight;
        if body.expected_total.amount != total {
            return Err(problem("price-changed").with_message(
                "O preço de alguma peça ou do frete mudou. Confira o total e pague de novo.",
            ));
        }

        // 4. Dados pessoais vão para o cofre; a saga guarda só referências.
        let order_id = saci_ids::Id::new(saci_ids::prefix::ORDER).to_string();
        let owner = account
            .clone()
            .unwrap_or_else(|| format!("guest:{order_id}"));
        #[derive(Deserialize)]
        struct Ref {
            #[serde(rename = "ref")]
            reference: String,
        }
        let address: Ref = internal::call(
            client,
            "identity",
            "put_address",
            &json!({ "owner_account_id": owner, "purpose": "delivery", "address": body.shipping_address }),
        )
        .await?;
        let buyer_first_name = match (&account, &body.buyer) {
            (_, Some(b)) if account.is_none() => {
                let _: Value = internal::call(
                    client,
                    "identity",
                    "put_guest_buyer",
                    &json!({ "order_id": order_id, "cpf": b.cpf.as_deref().filter(|c| !c.is_empty()), "name": b.name, "email": b.email,
                             "phone": b.phone.as_deref().and_then(saci_commerce_kit::text::normalize_phone) }),
                )
                .await?;
                first_name(&b.name)
            }
            (Some(a), _) => {
                let profile: Value = internal::call(
                    client,
                    "identity",
                    "account_profile",
                    &json!({ "account_id": a }),
                )
                .await?;
                profile["preferred_name"]
                    .as_str()
                    .or_else(|| profile["name"].as_str())
                    .map_or_else(|| "Cliente".into(), first_name)
            }
            _ => "Cliente".into(),
        };
        let (city, state) = match &body.shipping_address {
            ShippingAddress::Brasil { city, state, .. } => (city.clone(), state.clone()),
            ShippingAddress::Exterior { city, country, .. } => (city.clone(), country.clone()),
        };

        // 5. Estado da saga e reserva do estoque (compensável).
        let now = self.clock.epoch();
        let mut saga = Saga {
            order_id: order_id.clone(),
            state: "started".into(),
            account_id: account.clone(),
            items: body.items.clone(),
            storefront_ids: by_store.keys().cloned().collect(),
            deadline: now + STUCK_SECS,
            method: body.payment.method.clone(),
            pix: None,
        };
        self.save(&saga, None).await?;
        if let Err(p) = internal::call::<_, Value>(
            client,
            "catalog",
            "reserve",
            &json!({ "order_id": order_id, "items": body.items, "ttl_secs": RESERVATION_TTL_SECS }),
        )
        .await
        {
            self.abandon(&mut saga, "payment-failed", false).await?;
            return Err(p);
        }

        // 6. Pedido e vendas.
        let lines: Vec<Value> = body
            .items
            .iter()
            .zip(&catalog.items)
            .map(|(r, c)| {
                json!({
                    "listing_id": c.listing_id, "title": c.title, "storefront_id": c.storefront_id,
                    "storefront_name": c.storefront_name, "quantity": r.quantity, "unit_price": c.unit_price,
                    "photo_url": c.cover_url, "production_days": c.production_days,
                })
            })
            .collect();
        let shipping: Vec<Value> = quotes
            .items
            .iter()
            .map(|q| json!({ "storefront_id": q.artisan_id, "service": q.service, "price": q.price, "days": q.days }))
            .collect();
        let created: Value = match internal::call(
            client,
            "orders",
            "create_order",
            &json!({
                "order_id": order_id, "account_id": account, "payment_method": body.payment.method,
                "address_ref": address.reference, "destination": body.shipping_address.place(),
                "buyer": { "first_name": buyer_first_name, "city": city, "state": state },
                "lines": lines, "shipping": shipping,
            }),
        )
        .await
        {
            Ok(v) => v,
            Err(p) => {
                self.abandon(&mut saga, "payment-failed", true).await?;
                return Err(p);
            }
        };

        // 7. Cobrança, com split por vitrine.
        let splits: Vec<Value> = by_store
            .iter()
            .map(|(s, items)| {
                let pieces: i64 = items
                    .iter()
                    .map(|r| {
                        catalog
                            .items
                            .iter()
                            .find(|c| c.listing_id == r.listing_id)
                            .map_or(0, |c| c.unit_price.amount * i64::from(r.quantity))
                    })
                    .sum();
                let freight = quotes
                    .items
                    .iter()
                    .find(|q| q.artisan_id == *s)
                    .map_or(0, |q| q.price.amount);
                json!({ "storefront_id": s, "amount": Money::brl(pieces + freight) })
            })
            .collect();
        let charge: ChargeResult = match internal::call(
            client,
            "payments",
            "charge",
            &json!({
                "order_id": order_id, "account_id": account, "method": body.payment.method, "amount": Money::brl(total),
                "card_token": body.payment.card_token, "saved_card_id": body.payment.saved_card_id,
                "save_card": body.payment.save_card.unwrap_or(false), "splits": splits,
            }),
        )
        .await
        {
            Ok(c) => c,
            Err(p) => {
                self.abandon(&mut saga, "payment-failed", true).await?;
                return Err(p);
            }
        };
        let mut response = json!({});
        match charge.status.as_str() {
            "authorized" | "paid" => {
                let order = self.confirm(&mut saga).await?;
                response["order"] = order;
            }
            "pending" => {
                saga.state = "awaiting_payment".into();
                let pix_deadline = charge
                    .pix
                    .as_ref()
                    .and_then(|p| p["expires_at"].as_str())
                    .and_then(saci_commerce_kit::clock::parse_rfc3339)
                    .unwrap_or(now + 30 * 60);
                saga.deadline = pix_deadline + PIX_GRACE_SECS;
                saga.pix.clone_from(&charge.pix);
                self.save(&saga, None).await?;
                response["order"] = created["order"].clone();
                if let Some(pix) = charge.pix {
                    response["pix"] = json!({ "code": pix["code"], "amount": Money::brl(total), "expires_at": pix["expires_at"] });
                }
                if let Some(next) = charge.next_action {
                    response["next_action"] = next;
                }
            }
            _ => {
                self.abandon(&mut saga, "payment-failed", true).await?;
                return Err(problem("card-declined"));
            }
        }
        if account.is_none() {
            response["guest_token"] = json!(self.guest_token(&order_id).await?);
        }
        Ok(Response::json(201, &response))
    }

    async fn guest_token(&self, order_id: &str) -> Result<String, Problem> {
        let now = self.clock.epoch();
        let claims = json!({
            "iss": self.guest_issuer, "aud": self.guest_audience, "sub": order_id, "order_id": order_id,
            "iat": now, "exp": now + self.guest_ttl_secs, "jti": saci_ids::Id::new("gtk").to_string(),
        });
        self.guest_signer
            .sign_claims(&claims)
            .await
            .map_err(|e| saci_commerce_kit::problem::internal(&e))
    }

    /// Grava o estado da saga e a marca de saga aberta (para a varredura).
    async fn save(&self, saga: &Saga, event: Option<Event>) -> Result<(), Problem> {
        let mut ops = vec![WriteOp::put(
            Item::new(saga_pk(&saga.order_id), "META", saga)?
                .expiring(self.clock.epoch() + 90 * 86_400),
        )];
        if matches!(saga.state.as_str(), "started" | "awaiting_payment") {
            ops.push(WriteOp::put(Item::new(
                "OPEN_SAGAS",
                saga.order_id.clone(),
                &json!({ "deadline": saga.deadline }),
            )?));
        } else {
            ops.push(WriteOp::delete("OPEN_SAGAS", saga.order_id.clone()));
        }
        if let Some(e) = event {
            ops.push(WriteOp::Publish(e));
        }
        Ok(self.store.write(ops).await?)
    }

    /// Pagamento confirmado: pedido pago, reserva confirmada, evento.
    async fn confirm(&self, saga: &mut Saga) -> Result<Value, Problem> {
        let client = self.internal.as_ref();
        let paid: Value = internal::call(
            client,
            "orders",
            "mark_paid",
            &json!({ "order_id": saga.order_id }),
        )
        .await?;
        let _: Value = internal::call(
            client,
            "catalog",
            "commit",
            &json!({ "order_id": saga.order_id }),
        )
        .await?;
        saga.state = "confirmed".into();
        let mut data = json!({ "order_id": saga.order_id, "storefront_ids": saga.storefront_ids, "items": saga.items });
        if let Some(a) = &saga.account_id {
            data["account_id"] = json!(a);
        }
        let event = events::event(
            "checkout",
            "checkout.order.confirmed.v1",
            &saga.order_id,
            &data,
        )?;
        self.save(saga, Some(event)).await?;
        Ok(paid["order"].clone())
    }

    /// Compensação: solta a reserva e, se o pedido já existe, cancela.
    async fn abandon(
        &self,
        saga: &mut Saga,
        reason: &str,
        order_created: bool,
    ) -> Result<(), Problem> {
        let client = self.internal.as_ref();
        let _: Value = internal::call(
            client,
            "catalog",
            "release",
            &json!({ "order_id": saga.order_id }),
        )
        .await?;
        if order_created {
            let _: Value = internal::call(
                client,
                "orders",
                "cancel",
                &json!({ "order_id": saga.order_id }),
            )
            .await?;
        }
        saga.state = "abandoned".into();
        let event = events::event(
            "checkout",
            "checkout.order.abandoned.v1",
            &saga.order_id,
            &json!({ "order_id": saga.order_id, "items": saga.items, "reason": reason }),
        )?;
        self.save(saga, Some(event)).await
    }

    async fn saga(&self, order_id: &str) -> Result<Option<Saga>, Problem> {
        Ok(self
            .store
            .load::<Saga>(&saga_pk(order_id), "META")
            .await?
            .map(|(s, _)| s))
    }

    async fn payment_status(&self, req: &Request, order_id: &str) -> HttpResult {
        let auth = req.auth()?;
        let status: Value = internal::call(
            self.internal.as_ref(),
            "orders",
            "order_status",
            &json!({ "order_id": order_id }),
        )
        .await?;
        let allowed = match auth.kind {
            PrincipalKind::Guest => auth.order_id.as_deref() == Some(order_id),
            PrincipalKind::User => {
                auth.account_id.is_some()
                    && status["account_id"].as_str() == auth.account_id.as_deref()
            }
            PrincipalKind::Conversation => false,
        };
        if !allowed {
            return Err(not_found());
        }
        let mut order = status["order"].clone();
        let mut state = status["status"].as_str().unwrap_or_default().to_owned();
        if state == "awaiting-payment" {
            // Pergunta ao meio de pagamento (o webhook pode ter se perdido).
            if let Some(mut saga) = self
                .saga(order_id)
                .await?
                .filter(|s| s.state == "awaiting_payment")
            {
                let charge: Value = internal::call(
                    self.internal.as_ref(),
                    "payments",
                    "charge_status",
                    &json!({ "order_id": order_id }),
                )
                .await?;
                match charge["status"].as_str() {
                    Some("paid" | "authorized") => {
                        order = self.confirm(&mut saga).await?;
                        state = "paid".into();
                    }
                    Some("expired" | "failed") => {
                        self.abandon(&mut saga, "pix-expired", true).await?;
                        state = "cancelled".into();
                    }
                    _ => {}
                }
            }
        }
        let payment = match state.as_str() {
            "awaiting-payment" => "pending",
            "cancelled" => "expired",
            _ => "paid",
        };
        Ok(Response::ok(&json!({ "status": payment, "order": order })))
    }

    /// Varredura agendada: compensa sagas paradas e Pix vencidos sem aviso.
    pub async fn sweep(&self) -> Result<usize, Problem> {
        let now = self.clock.epoch();
        let mut done = 0;
        for open in self
            .store
            .query_all("OPEN_SAGAS".into(), String::new())
            .await?
        {
            if open.doc["deadline"].as_u64().unwrap_or(0) > now {
                continue;
            }
            let Some(mut saga) = self.saga(&open.sk).await? else {
                self.store
                    .write(vec![WriteOp::delete(open.pk, open.sk)])
                    .await?;
                continue;
            };
            let reason = if saga.state == "awaiting_payment" {
                "pix-expired"
            } else {
                "payment-failed"
            };
            tracing::warn!(order = %saga.order_id, state = %saga.state, "saga compensada pela varredura");
            self.abandon(&mut saga, reason, true).await?;
            done += 1;
        }
        Ok(done)
    }
}

/// Consumidor dos eventos de pagamento (`payments.*`).
#[derive(Debug, Clone)]
pub struct PaymentEvents(pub Checkout);

impl Consumer for PaymentEvents {
    fn name(&self) -> &'static str {
        "checkout-payments"
    }

    fn accepts(&self) -> &'static [&'static str] {
        &[
            "payments.pix.paid.v1",
            "payments.charge.authorized.v1",
            "payments.pix.expired.v1",
            "payments.charge.failed.v1",
        ]
    }

    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Charge {
                order_id: String,
            }
            let e: Charge = data(event)?;
            let Some(mut saga) = self.0.saga(&e.order_id).await? else {
                return Ok(());
            };
            if saga.state != "awaiting_payment" {
                return Ok(());
            }
            let result = match event.event_type.as_str() {
                "payments.pix.paid.v1" | "payments.charge.authorized.v1" => {
                    self.0.confirm(&mut saga).await.map(|_| ())
                }
                "payments.pix.expired.v1" => self.0.abandon(&mut saga, "pix-expired", true).await,
                _ => self.0.abandon(&mut saga, "payment-failed", true).await,
            };
            result.map_err(ConsumeError::from)
        })
    }
}

/// Validade em texto (para logs e respostas).
#[must_use]
pub fn expires_text(epoch: u64) -> String {
    rfc3339_epoch(epoch)
}
