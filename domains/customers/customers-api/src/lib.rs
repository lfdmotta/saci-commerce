//! `customers-api`: quem compra (proposta 5.2; contrato `saci-contracts/http/customers.yaml`).
//!
//! | Rota | Acesso |
//! | --- | --- |
//! | `GET /v1/customers/me` | `USER` |
//! | `PATCH /v1/customers/me` | `USER`; `USER+` quando muda o contato |
//! | `PUT /v1/customers/me/address` | `USER` (avisa no contato) |
//! | `GET /v1/customers/me/favorites` | `USER` |
//! | `PUT`/`DELETE /v1/customers/me/favorites/{listing_id}` | `USER` |
//! | `POST /v1/customers/newsletter-subscriptions` | `PUB-L` |
//!
//! Com o cofre de dados pessoais (D9, seção 9.3), o CPF, o nome de registro,
//! o contato e o endereço vivem no `saci-identity`. Este serviço guarda só o
//! nome preferido, a **referência** do endereço (`addr_`) e os favoritos, e
//! monta o perfil pedindo o resto ao cofre (chamadas internas a `identity`).
//!
//! Tabela `saci-<env>-customers`:
//!
//! | `pk` | `sk` | Documento |
//! | --- | --- | --- |
//! | `ACC#<conta>` | `PROFILE` | nome preferido, referência do endereço, criação |
//! | `ACC#<conta>` | `FAV#<anúncio>` | favorito, com a data |
//! | `NEWS#<digital do e-mail>` | `META` | inscrição nas novidades (dois passos) |

use std::sync::Arc;

use saci_commerce_kit::auth;
use saci_commerce_kit::clock::Clock;
use saci_commerce_kit::cursor::{CursorSigner, hmac_sha256};
use saci_commerce_kit::events::{ConsumeError, Consumer, Event, data};
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::internal::{self, InternalClient};
use saci_commerce_kit::limits;
use saci_commerce_kit::problem::{invalid, not_found};
use saci_commerce_kit::store::{DocumentStore, Item, StoreExt, WriteOp};
use saci_commerce_kit::types::{Page, ShippingAddress};
use saci_commerce_kit::{BoxFut, Problem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Favoritos por página.
const PAGE: usize = 24;

/// Dependências do serviço.
#[derive(Debug, Clone)]
pub struct Customers {
    /// Tabela.
    pub store: Arc<dyn DocumentStore>,
    /// Chamadas a `identity` (cofre) e `catalog`.
    pub internal: Arc<dyn InternalClient>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Cursores (e a chave da digital do e-mail).
    pub cursors: CursorSigner,
    /// Chave da digital do e-mail das novidades.
    pub hash_key: Vec<u8>,
}

/// O que fica guardado da pessoa.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileRecord {
    /// Nome preferido.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_name: Option<String>,
    /// Endereço de entrega no cofre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_ref: Option<String>,
    /// Criação.
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
struct Patch {
    #[serde(default, deserialize_with = "double")]
    preferred_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "double")]
    email: Option<Option<String>>,
    #[serde(default, deserialize_with = "double")]
    phone: Option<Option<String>>,
}

fn double<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

fn pk(account: &str) -> String {
    format!("ACC#{account}")
}

impl HttpService for Customers {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

impl Customers {
    async fn route(&self, req: &Request) -> HttpResult {
        match (req.method.as_str(), req.segments().as_slice()) {
            ("GET", ["v1", "customers", "me"]) => self.me(req).await,
            ("PATCH", ["v1", "customers", "me"]) => self.patch(req).await,
            ("PUT", ["v1", "customers", "me", "address"]) => self.put_address(req).await,
            ("GET", ["v1", "customers", "me", "favorites"]) => self.favorites(req).await,
            ("PUT", ["v1", "customers", "me", "favorites", id]) => {
                self.favorite(req, id, true).await
            }
            ("DELETE", ["v1", "customers", "me", "favorites", id]) => {
                self.favorite(req, id, false).await
            }
            ("POST", ["v1", "customers", "newsletter-subscriptions"]) => self.newsletter(req).await,
            _ => Err(not_found()),
        }
    }

    /// O perfil guardado (criado na primeira leitura, se o evento ainda não chegou).
    async fn record(&self, account: &str) -> Result<ProfileRecord, Problem> {
        if let Some((r, _)) = self
            .store
            .load::<ProfileRecord>(&pk(account), "PROFILE")
            .await?
        {
            return Ok(r);
        }
        // O cofre pode já ter um endereço da conta (ex.: cadastro no fim da compra).
        let default: Value = internal::call(
            self.internal.as_ref(),
            "identity",
            "default_address",
            &json!({ "account_id": account }),
        )
        .await
        .unwrap_or_default();
        let record = ProfileRecord {
            created_at: self.clock.rfc3339(),
            address_ref: default["ref"].as_str().map(str::to_owned),
            ..ProfileRecord::default()
        };
        let _ = self
            .store
            .write(vec![WriteOp::create(Item::new(
                pk(account),
                "PROFILE",
                &record,
            )?)])
            .await;
        Ok(record)
    }

    async fn profile(&self, account: &str) -> Result<Value, Problem> {
        let record = self.record(account).await?;
        let mut identity: Value = internal::call(
            self.internal.as_ref(),
            "identity",
            "account_profile",
            &json!({ "account_id": account }),
        )
        .await?;
        let mut out = json!({
            "id": account,
            "cpf_masked": identity["cpf_masked"],
            "identity_status": identity.get("identity_status").cloned().unwrap_or_else(|| json!("unverified")),
            "created_at": identity.get("created_at").cloned().unwrap_or_else(|| json!(record.created_at)),
        });
        for field in ["email", "phone", "pending_contact"] {
            if let Some(v) = identity
                .get_mut(field)
                .map(Value::take)
                .filter(|v| !v.is_null())
            {
                out[field] = v;
            }
        }
        if let Some(name) = identity.get("name").filter(|v| !v.is_null()) {
            out["official_name"] = name.clone();
        }
        if let Some(p) = record
            .preferred_name
            .or_else(|| identity["preferred_name"].as_str().map(str::to_owned))
        {
            out["preferred_name"] = json!(p);
        }
        if let Some(reference) = record.address_ref {
            let opened: Value = internal::call(
                self.internal.as_ref(),
                "identity",
                "open_address",
                &json!({ "ref": reference, "purpose": "delivery", "account_id": account }),
            )
            .await?;
            out["address"] = opened["address"].clone();
        }
        Ok(out)
    }

    async fn me(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        Ok(Response::ok(&self.profile(&account).await?))
    }

    async fn patch(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        let patch: Patch = req.json()?;
        if patch.email.is_some() || patch.phone.is_some() {
            // S-12: trocar o contato exige senha recente; a troca só vale depois
            // de confirmada no destino novo, com aviso no antigo (no cofre).
            auth::recent_user(req, self.clock.epoch())?;
            if let Some(Some(email)) = &patch.email
                && (email.len() > 254
                    || !email.contains('@')
                    || email.split('@').nth(1).is_none_or(|d| !d.contains('.')))
            {
                return Err(invalid("email", "Confira o e-mail."));
            }
            if let Some(Some(phone)) = &patch.phone
                && (!(10..=11).contains(&phone.len()) || !phone.bytes().all(|b| b.is_ascii_digit()))
            {
                return Err(invalid("phone", "Use o celular com DDD, só números."));
            }
            // Só os campos que vieram: no cofre, `null` tira o contato e ausente
            // mantém. Mandar `"phone": null` quando só o e-mail mudou apagaria o
            // celular.
            let mut change = json!({ "account_id": account });
            if let Some(email) = &patch.email {
                change["email"] = json!(email);
            }
            if let Some(phone) = &patch.phone {
                change["phone"] = json!(phone);
            }
            let _: Value = internal::call(
                self.internal.as_ref(),
                "identity",
                "request_contact_change",
                &change,
            )
            .await?;
        }
        if let Some(name) = patch.preferred_name {
            let name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
            if name.as_ref().is_some_and(|n| n.chars().count() > 60) {
                return Err(invalid("preferred_name", "Use até 60 caracteres."));
            }
            let mut record = self.record(&account).await?;
            record.preferred_name = name;
            self.store
                .write(vec![WriteOp::put(Item::new(
                    pk(&account),
                    "PROFILE",
                    &record,
                )?)])
                .await?;
        }
        Ok(Response::ok(&self.profile(&account).await?))
    }

    async fn put_address(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        let address: ShippingAddress = req.json()?;
        if let Err((field, message)) = address.validate() {
            return Err(invalid(field, message));
        }
        #[derive(Deserialize)]
        struct Ref {
            #[serde(rename = "ref")]
            reference: String,
        }
        let saved: Ref = internal::call(
            self.internal.as_ref(),
            "identity",
            "put_address",
            &json!({ "owner_account_id": account, "purpose": "delivery", "address": address }),
        )
        .await?;
        let mut record = self.record(&account).await?;
        record.address_ref = Some(saved.reference);
        self.store
            .write(vec![WriteOp::put(Item::new(
                pk(&account),
                "PROFILE",
                &record,
            )?)])
            .await?;
        // Aviso no contato cadastrado: endereço trocado é sinal comum de conta tomada.
        let _: Value = internal::call(
            self.internal.as_ref(),
            "identity",
            "notify",
            &json!({ "account_id": account, "template": "address-changed" }),
        )
        .await?;
        Ok(Response::ok(&address))
    }

    async fn favorites(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        let scope = format!("{account}:favorites");
        let after = req
            .query("cursor")
            .map(|c| self.cursors.decode(&scope, c))
            .transpose()?;
        let page = self
            .store
            .query(
                saci_commerce_kit::store::Query::prefix(pk(&account), "FAV#")
                    .limit(PAGE)
                    .after(after),
            )
            .await?;
        let ids: Vec<String> = page
            .items
            .iter()
            .map(|i| i.sk.trim_start_matches("FAV#").to_owned())
            .collect();
        #[derive(Deserialize)]
        struct Listings {
            items: Vec<Value>,
        }
        let listings: Listings = if ids.is_empty() {
            Listings { items: Vec::new() }
        } else {
            internal::call(
                self.internal.as_ref(),
                "catalog",
                "public_listings",
                &json!({ "ids": ids }),
            )
            .await?
        };
        let next_cursor = page.last.map(|last| self.cursors.encode(&scope, &last));
        Ok(Response::ok(&Page {
            items: listings.items,
            next_cursor,
        }))
    }

    async fn favorite(&self, req: &Request, listing_id: &str, add: bool) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        let sk = format!("FAV#{listing_id}");
        if !add {
            if self.store.get(&pk(&account), &sk).await?.is_none() {
                return Err(not_found());
            }
            self.store
                .write(vec![WriteOp::delete(pk(&account), sk)])
                .await?;
            return Ok(Response::no_content());
        }
        #[derive(Deserialize)]
        struct Listings {
            items: Vec<Value>,
        }
        let found: Listings = internal::call(
            self.internal.as_ref(),
            "catalog",
            "public_listings",
            &json!({ "ids": [listing_id] }),
        )
        .await?;
        if found.items.is_empty() {
            return Err(not_found());
        }
        self.store
            .write(vec![WriteOp::put(Item::new(
                pk(&account),
                sk,
                &json!({ "at": self.clock.rfc3339() }),
            )?)])
            .await?;
        Ok(Response::no_content())
    }

    async fn newsletter(&self, req: &Request) -> HttpResult {
        #[derive(Deserialize)]
        struct Body {
            email: String,
        }
        let body: Body = req.json()?;
        let email = body.email.trim().to_lowercase();
        if email.len() > 254
            || !email.contains('@')
            || email.split('@').nth(1).is_none_or(|d| !d.contains('.'))
        {
            return Err(invalid("email", "Confira o e-mail."));
        }
        let ip = req.source_ip.clone().unwrap_or_else(|| "?".into());
        limits::hit(
            self.store.as_ref(),
            &format!("{ip}:newsletter"),
            3_600,
            10,
            self.clock.epoch(),
            "rate-limited",
        )
        .await?;
        let digest = hex(&hmac_sha256(&self.hash_key, &[email.as_bytes()]));
        let key = format!("NEWS#{digest}");
        if self.store.get(&key, "META").await?.is_none() {
            let token = saci_ids::Id::new("nws").to_string();
            self.store
                .write(vec![WriteOp::create(Item::new(
                    key,
                    "META",
                    &json!({ "status": "pending", "token": token, "at": self.clock.rfc3339() }),
                )?)])
                .await
                .ok();
            // Dois passos: o e-mail de confirmação sai pelo cofre (o endereço não fica aqui).
            let _: Value = internal::call(
                self.internal.as_ref(),
                "identity",
                "send_to_address",
                &json!({ "email": email, "template": "newsletter-confirm", "token": token }),
            )
            .await?;
        }
        // Sempre igual: não revela se o e-mail já estava inscrito.
        Ok(Response::json(202, &json!({})))
    }
}

fn hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// Cria o perfil quando a conta é verificada (`identity.account.verified.v1`).
#[derive(Debug, Clone)]
pub struct ProfileCreator(pub Customers);

impl Consumer for ProfileCreator {
    fn name(&self) -> &'static str {
        "customers-profile"
    }

    fn accepts(&self) -> &'static [&'static str] {
        &["identity.account.verified.v1"]
    }

    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Verified {
                account_id: String,
            }
            let e: Verified = data(event)?;
            self.0
                .record(&e.account_id)
                .await
                .map(|_| ())
                .map_err(ConsumeError::from)
        })
    }
}
