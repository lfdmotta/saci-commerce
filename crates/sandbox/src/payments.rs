//! Sandbox do `payments` (host `pay.`): configuração de tokenização,
//! preferências, cartões salvos, e a cobrança que o `checkout` chama.
//!
//! Regras do sandbox (cartões de teste, nunca guarda número):
//!
//! | Cartão | Resultado |
//! | --- | --- |
//! | Número terminado em `0002` | Recusado (`card-declined`) |
//! | Qualquer outro | Aprovado |
//! | Pix | Pendente; pago 20 s depois da cobrança (como o mock da interface); vence em 30 min |

use saci_commerce_kit::auth;
use saci_commerce_kit::clock::rfc3339_epoch;
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::internal::{InternalHandler, payload, reply, unknown_op};
use saci_commerce_kit::problem::{invalid, not_found, problem};
use saci_commerce_kit::store::{Item, StoreExt, WriteOp};
use saci_commerce_kit::{BoxFut, Problem};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Sandbox;

/// Depois de quanto tempo o Pix do sandbox é pago.
pub const PIX_AUTO_PAY_SECS: u64 = 20;
/// Validade do Pix.
pub const PIX_TTL_SECS: u64 = 30 * 60;

fn brand(number: &str) -> &'static str {
    match number.as_bytes().first() {
        Some(b'4') => "visa",
        Some(b'5') => "mastercard",
        Some(b'3') => "amex",
        Some(b'6') => "elo",
        _ => "outra",
    }
}

impl Sandbox {
    async fn preferences(&self, account: &str) -> Result<Value, Problem> {
        Ok(self
            .store
            .load::<Value>(&format!("PAYPREF#{account}"), "META")
            .await?
            .map_or_else(|| json!({ "default_method": "pix" }), |(v, _)| v))
    }

    async fn save_preferences(&self, account: &str, prefs: &Value) -> Result<(), Problem> {
        Ok(self
            .store
            .write(vec![WriteOp::put(Item::new(
                format!("PAYPREF#{account}"),
                "META",
                prefs,
            )?)])
            .await?)
    }

    async fn token(&self, token: &str) -> Result<Value, Problem> {
        Ok(self
            .store
            .load::<Value>(&format!("TOK#{token}"), "META")
            .await?
            .ok_or_else(|| invalid("card_token", "Token vencido. Digite o cartão de novo."))?
            .0)
    }
}

/// As rotas HTTP de `/v1/payments` e a tokenização (`/dev/psp/tokens`).
#[derive(Debug, Clone)]
pub struct PaymentsHttp(pub Sandbox);

impl HttpService for PaymentsHttp {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

impl PaymentsHttp {
    async fn route(&self, req: &Request) -> HttpResult {
        let s = &self.0;
        let now = s.clock.epoch();
        match (req.method.as_str(), req.segments().as_slice()) {
            ("GET", ["v1", "payments", "client-config"]) => Ok(Response::ok(&json!({
                "provider": "sandbox", "public_key": "pk_sandbox",
                "tokenize_url": format!("{}/dev/psp/tokens", s.public_base_url), "methods": ["pix", "credito", "debito"],
            }))
            .public(300)),
            ("POST", ["dev", "psp", "tokens"]) => {
                // Faz o papel do PSP: recebe o cartão direto do aparelho. Guarda só bandeira e final.
                let body: Value = serde_json::from_slice(&req.body).map_err(|_| invalid("card", "Confira o cartão."))?;
                let id = saci_ids::Id::new("tok").to_string();
                let (brand, last4, decline) = if let Some("cvv") = body["type"].as_str() { ("outra", String::new(), false) } else {
                    let number: String = body["card"]["number"].as_str().unwrap_or_default().chars().filter(char::is_ascii_digit).collect();
                    if number.len() < 13 {
                        return Err(invalid("card", "Confira o número do cartão."));
                    }
                    (brand(&number), number[number.len() - 4..].to_owned(), number.ends_with("0002"))
                };
                s.store
                    .write(vec![WriteOp::put(Item::new(format!("TOK#{id}"), "META", &json!({ "brand": brand, "last4": last4, "decline": decline }))?.expiring(now + 600))])
                    .await?;
                Ok(Response::ok(&json!({ "id": id, "expires_at": rfc3339_epoch(now + 60), "card": { "brand": brand } })))
            }
            ("GET", ["v1", "payments", "me", "preferences"]) => Ok(Response::ok(&s.preferences(auth::user(req)?).await?)),
            ("PUT", ["v1", "payments", "me", "preferences", "default"]) => {
                let account = auth::user(req)?.to_owned();
                #[derive(Deserialize)]
                struct Body {
                    method: String,
                }
                let b: Body = req.json()?;
                if !["pix", "credito", "debito"].contains(&b.method.as_str()) {
                    return Err(invalid("method", "Forma de pagamento não aceita."));
                }
                let mut prefs = s.preferences(&account).await?;
                prefs["default_method"] = json!(b.method);
                s.save_preferences(&account, &prefs).await?;
                Ok(Response::ok(&prefs))
            }
            ("POST", ["v1", "payments", "me", "cards"]) => {
                let account = auth::recent_user(req, now)?.to_owned();
                #[derive(Deserialize)]
                struct Body {
                    card_token: String,
                    kind: String,
                }
                let b: Body = req.json()?;
                let tok = s.token(&b.card_token).await?;
                if tok["decline"] == true {
                    return Err(problem("card-declined"));
                }
                let mut prefs = s.preferences(&account).await?;
                prefs["saved_card"] = json!({ "id": saci_ids::Id::new("card").to_string(), "kind": b.kind, "saved_at": s.clock.rfc3339(), "brand": tok["brand"], "last4": tok["last4"] });
                s.save_preferences(&account, &prefs).await?;
                Ok(Response::json(201, &prefs))
            }
            ("DELETE", ["v1", "payments", "me", "cards", id]) => {
                let account = auth::recent_user(req, now)?.to_owned();
                let mut prefs = s.preferences(&account).await?;
                if prefs["saved_card"]["id"].as_str() != Some(*id) {
                    return Err(not_found());
                }
                if let Some(o) = prefs.as_object_mut() {
                    o.remove("saved_card");
                }
                s.save_preferences(&account, &prefs).await?;
                Ok(Response::ok(&prefs))
            }
            _ => Err(not_found()),
        }
    }
}

/// As operações internas de cobrança (`payments`), no formato que o `checkout` espera do `saci-payments`.
#[derive(Debug, Clone)]
pub struct PaymentsInternal(pub Sandbox);

#[derive(Debug, Deserialize)]
struct Charge {
    order_id: String,
    #[serde(default)]
    account_id: Option<String>,
    method: String,
    amount: Value,
    #[serde(default)]
    card_token: Option<String>,
    #[serde(default)]
    save_card: bool,
}

impl InternalHandler for PaymentsInternal {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            let s = &self.0;
            let now = s.clock.epoch();
            match op {
                "charge" => {
                    let c: Charge = payload(body)?;
                    let key = format!("CHG#{}", c.order_id);
                    if let Some((existing, _)) = s.store.load::<Value>(&key, "META").await? {
                        return reply(&existing["result"]);
                    }
                    let result = if c.method == "pix" {
                        let expires = now + PIX_TTL_SECS;
                        let cents = c.amount["amount"].as_i64().unwrap_or(0);
                        json!({ "charge_id": saci_ids::Id::new("chg").to_string(), "status": "pending",
                                "pix": { "code": format!("00020126580014br.gov.bcb.pix0136saci-sandbox-{}5204000053039865406{}.{:02}5802BR6304ABCD", c.order_id, cents / 100, cents % 100),
                                         "expires_at": rfc3339_epoch(expires) } })
                    } else {
                        let tok = s.token(c.card_token.as_deref().unwrap_or_default()).await?;
                        let declined = tok["decline"] == true;
                        if !declined
                            && c.save_card
                            && let Some(account) = &c.account_id
                        {
                            let mut prefs = s.preferences(account).await?;
                            prefs["saved_card"] = json!({ "id": saci_ids::Id::new("card").to_string(), "kind": c.method, "saved_at": s.clock.rfc3339(), "brand": tok["brand"], "last4": tok["last4"] });
                            s.save_preferences(account, &prefs).await?;
                        }
                        json!({ "charge_id": saci_ids::Id::new("chg").to_string(), "status": if declined { "declined" } else { "authorized" } })
                    };
                    s.store
                        .write(vec![WriteOp::put(Item::new(
                            key,
                            "META",
                            &json!({ "result": result, "created": now, "method": c.method }),
                        )?)])
                        .await?;
                    reply(&result)
                }
                "charge_status" => {
                    let order = body["order_id"].as_str().unwrap_or_default();
                    let (c, _) = s
                        .store
                        .load::<Value>(&format!("CHG#{order}"), "META")
                        .await?
                        .ok_or_else(not_found)?;
                    let created = c["created"].as_u64().unwrap_or(now);
                    let status = match c["result"]["status"].as_str() {
                        Some("pending") if now >= created + PIX_TTL_SECS => "expired",
                        Some("pending") if now >= created + PIX_AUTO_PAY_SECS => "paid",
                        Some(other) => other,
                        None => "failed",
                    };
                    reply(&json!({ "status": status, "charge_id": c["result"]["charge_id"] }))
                }
                _ => Err(unknown_op("payments", op)),
            }
        })
    }
}
