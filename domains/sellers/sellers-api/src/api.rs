//! Rotas do `sellers-api` (proposta 5.3; contrato `saci-contracts/http/sellers.yaml`).
//!
//! | Rota | Acesso |
//! | --- | --- |
//! | `GET /v1/sellers/storefronts/{url_name}` | `PUB` (301 para endereço antigo) |
//! | `GET /v1/sellers/storefront-fonts` | `PUB` |
//! | `GET /v1/sellers/storefronts/availability?name=` | `USER` |
//! | `POST /v1/sellers/onboarding/identity` | `USER` (5 por conta em 24 h) |
//! | `POST /v1/sellers/onboarding/business` | `USER` (10 por conta por dia) |
//! | `POST /v1/sellers/storefronts` | `USER+` |
//! | `GET /v1/sellers/me/workspace` | `USER` (404 = conta sem vitrine) |
//! | `GET`/`PUT /v1/sellers/me/storefront` | `SELLER:vitrine` (`ETag`/`If-Match`) |
//! | `GET /v1/sellers/me/team` | `OWNER` |
//! | `POST /v1/sellers/me/team` | `OWNER+` (20 convites por vitrine por dia) |
//! | `PATCH`/`DELETE /v1/sellers/me/team/{id}` | `OWNER+` |
//! | `GET /v1/sellers/me/team-invitations` | `USER` |
//! | `POST /v1/sellers/me/team-invitations/{id}/accept`, `/decline` | `USER+` |

use std::sync::Arc;

use saci_commerce_kit::auth::{self, Section};
use saci_commerce_kit::clock::{Clock, parse_date};
use saci_commerce_kit::events::{self, Event};
use saci_commerce_kit::grants::{Grant, GrantStore};
use saci_commerce_kit::http::{
    self, HttpResult, HttpService, Request, Response, etag, require_if_match,
};
use saci_commerce_kit::internal::{self, InternalClient};
use saci_commerce_kit::limits;
use saci_commerce_kit::problem::{invalid, not_found, problem};
use saci_commerce_kit::store::{DocumentStore, Expect, Item, StoreError, StoreExt, WriteOp};
use saci_commerce_kit::text::{normalize, slugify};
use saci_commerce_kit::types::{Page, SECTIONS, is_hex_color, is_postal_code, is_url_name};
use saci_commerce_kit::{BoxFut, Problem};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::{
    CheckRecord, Counts, Location, MemberRecord, Membership, Palette, StorefrontRecord, UrlRecord,
    keys,
};

/// Endereços que nenhuma vitrine pode ter (rotas e nomes da plataforma, S-21).
pub const RESERVED_URL_NAMES: &[&str] = &[
    "availability",
    "me",
    "onboarding",
    "admin",
    "api",
    "saci",
    "saci-oficial",
    "oficial",
    "suporte",
    "ajuda",
    "caipora",
    "login",
    "conta",
    "checkout",
    "pagamento",
    "vitrine",
    "vitrines",
    "artesaos",
    "busca",
    "sacola",
];

/// Palavras proibidas no nome da vitrine (S-21: ninguém se passa pela Saci).
const FORBIDDEN_BRAND_WORDS: &[&str] = &["saci", "caipora", "oficial", "suporte"];

/// Validade de uma conferência para abrir a vitrine.
const CHECK_TTL_SECS: u64 = 24 * 3600;

/// Dependências do serviço.
#[derive(Debug, Clone)]
pub struct Sellers {
    /// Tabela das vitrines.
    pub store: Arc<dyn DocumentStore>,
    /// Papéis na tabela `authz` da plataforma.
    pub grants: Arc<dyn GrantStore>,
    /// Chamadas a `identity` (cofre), `kyc`, `shipping`, `media`, `catalog`, `orders`, `support`.
    pub internal: Arc<dyn InternalClient>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Host público das imagens enviadas (`https://cdn.saci.art.br`), para a regra S-22.
    pub cdn_base_url: String,
}

impl HttpService for Sellers {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

#[derive(Debug, Deserialize)]
struct IdentityBody {
    birth_date: String,
}

#[derive(Debug, Deserialize)]
struct BusinessBody {
    cnpj: String,
}

#[derive(Debug, Deserialize)]
struct OpenBody {
    identity_check_id: String,
    #[serde(default)]
    business_check_id: Option<String>,
    postal_code: String,
    accepted_seller_terms_version: String,
}

#[derive(Debug, Deserialize)]
struct UpdateBody {
    brand_name: String,
    #[serde(default)]
    slogan: Option<String>,
    storefront_url_name: String,
    visual_identity: VisualUpdate,
}

/// `null` apaga; ausente mantém.
#[derive(Debug, Deserialize)]
struct VisualUpdate {
    palette: Option<Palette>,
    font_option_id: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    logo_upload_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    hero_upload_id: Option<Option<String>>,
}

fn double_option<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

#[derive(Debug, Deserialize)]
struct InviteBody {
    cpf: String,
    permissions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PermissionsBody {
    permissions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct KycIdentity {
    result: String,
    #[serde(default)]
    official_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KycBusiness {
    result: String,
    #[serde(default)]
    cnpj_masked: Option<String>,
    #[serde(default)]
    legal_name: Option<String>,
    #[serde(default)]
    trade_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CpfLookup {
    fingerprint: String,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PostalCode {
    city: String,
    state: String,
}

impl Sellers {
    async fn route(&self, req: &Request) -> HttpResult {
        match (req.method.as_str(), req.segments().as_slice()) {
            ("GET", ["v1", "sellers", "storefront-fonts"]) => self.fonts().await,
            ("GET", ["v1", "sellers", "storefronts", "availability"]) => {
                self.availability(req).await
            }
            ("GET", ["v1", "sellers", "storefronts", url_name]) => {
                self.public_storefront(url_name).await
            }
            ("POST", ["v1", "sellers", "onboarding", "identity"]) => self.check_identity(req).await,
            ("POST", ["v1", "sellers", "onboarding", "business"]) => self.check_business(req).await,
            ("POST", ["v1", "sellers", "storefronts"]) => self.open(req).await,
            ("GET", ["v1", "sellers", "me", "workspace"]) => self.workspace(req).await,
            ("GET", ["v1", "sellers", "me", "storefront"]) => self.private_storefront(req).await,
            ("PUT", ["v1", "sellers", "me", "storefront"]) => self.update_storefront(req).await,
            ("GET", ["v1", "sellers", "me", "team"]) => self.team(req).await,
            ("POST", ["v1", "sellers", "me", "team"]) => self.invite(req).await,
            ("PATCH", ["v1", "sellers", "me", "team", id]) => self.change_member(req, id).await,
            ("DELETE", ["v1", "sellers", "me", "team", id]) => self.remove_member(req, id).await,
            ("GET", ["v1", "sellers", "me", "team-invitations"]) => self.invitations(req).await,
            ("POST", ["v1", "sellers", "me", "team-invitations", id, "accept"]) => {
                self.answer(req, id, true).await
            }
            ("POST", ["v1", "sellers", "me", "team-invitations", id, "decline"]) => {
                self.answer(req, id, false).await
            }
            _ => Err(not_found()),
        }
    }

    // --- público ---------------------------------------------------------------------------

    async fn fonts(&self) -> HttpResult {
        let items = self
            .store
            .query_all(keys::FONTS.into(), String::new())
            .await?;
        let fonts: Vec<Value> = items.iter().map(Item::parse).collect::<Result<_, _>>()?;
        Ok(Response::ok(&Page::all(fonts)).public(3600))
    }

    async fn public_storefront(&self, url_name: &str) -> HttpResult {
        let Some((url, _)) = self
            .store
            .load::<UrlRecord>(&keys::url(url_name), keys::META)
            .await?
        else {
            return Err(not_found());
        };
        if let Some(target) = url.redirect_to {
            let location = format!("/v1/sellers/storefronts/{target}");
            return Ok(
                Response::json(301, &json!({ "storefront_url_name": target }))
                    .header("location", &location)
                    .public(3600),
            );
        }
        let (storefront, _) = self.storefront(&url.storefront_id).await?;
        Ok(Response::ok(&self.public_view(&storefront).await?).public(60))
    }

    /// `PublicStorefront`: só cidade, UF e país (S-01).
    pub(crate) async fn public_view(&self, s: &StorefrontRecord) -> Result<Value, Problem> {
        let mut v = json!({
            "brand_name": s.brand_name, "storefront_url_name": s.url_name,
            "location": s.location, "visual_identity": self.visual_identity(s).await?,
        });
        if let Some(slogan) = &s.slogan {
            v["slogan"] = json!(slogan);
        }
        Ok(v)
    }

    async fn visual_identity(&self, s: &StorefrontRecord) -> Result<Value, Problem> {
        let mut vi = serde_json::Map::new();
        if let Some(p) = &s.palette {
            vi.insert("palette".into(), json!(p));
        }
        if let Some(url) = s.logo_url.as_ref().filter(|u| self.trusted(u)) {
            vi.insert("logo_url".into(), json!(url));
        }
        if let Some(url) = s.hero_image_url.as_ref().filter(|u| self.trusted(u)) {
            vi.insert("hero_image_url".into(), json!(url));
        }
        if let Some(font) = self.font(s.font_option_id.as_deref()).await? {
            vi.insert(
                "fonts".into(),
                json!({ "display": font["display"], "body": font["body"] }),
            );
        }
        Ok(Value::Object(vi))
    }

    /// S-22: só imagens do CDN da Saci (as sementes de desenvolvimento usam outros hosts e passam fora de produção).
    fn trusted(&self, url: &str) -> bool {
        url.starts_with(&self.cdn_base_url)
            || saci_commerce_kit::env_or("SACI_ENVIRONMENT", "local") != "prod"
    }

    async fn font(&self, id: Option<&str>) -> Result<Option<Value>, Problem> {
        let Some(id) = id else { return Ok(None) };
        let items = self
            .store
            .query_all(keys::FONTS.into(), String::new())
            .await?;
        Ok(items
            .iter()
            .filter_map(|i| i.parse::<Value>().ok())
            .find(|f| f["id"] == id))
    }

    /// `StorefrontTheme` da banca no diretório (logo, cores, fontes).
    pub(crate) async fn theme(&self, s: &StorefrontRecord) -> Result<Option<Value>, Problem> {
        let mut t = serde_json::Map::new();
        if let Some(url) = s.logo_url.as_ref().filter(|u| self.trusted(u)) {
            t.insert("logo".into(), json!({ "url": url, "alt": s.brand_name }));
        }
        if let Some(p) = s
            .palette
            .as_ref()
            .and_then(|p| p.primary.as_ref().map(|primary| (primary, &p.accent)))
        {
            let mut colors = json!({ "primary": p.0 });
            if let Some(a) = p.1 {
                colors["accent"] = json!(a);
            }
            t.insert("colors".into(), colors);
        }
        if let Some(font) = self.font(s.font_option_id.as_deref()).await? {
            t.insert(
                "fonts".into(),
                json!({ "display": font["display"], "body": font["body"] }),
            );
        }
        Ok((!t.is_empty()).then_some(Value::Object(t)))
    }

    // --- abertura ----------------------------------------------------------------------------

    async fn availability(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        limits::hit(
            self.store.as_ref(),
            &format!("{account}:availability"),
            60,
            30,
            self.clock.epoch(),
            "rate-limited",
        )
        .await?;
        let name = req
            .query("name")
            .ok_or_else(|| invalid("name", "Informe o endereço."))?;
        if !is_url_name(name) {
            return Err(invalid(
                "name",
                "Use letras minúsculas, números e hífen (2 a 60).",
            ));
        }
        let mine = self.membership(&account).await?.map(|m| m.storefront_id);
        let available = !RESERVED_URL_NAMES.contains(&name)
            && match self
                .store
                .load::<UrlRecord>(&keys::url(name), keys::META)
                .await?
            {
                None => true,
                Some((u, _)) => {
                    mine.as_deref() == Some(u.storefront_id.as_str()) && u.redirect_to.is_none()
                }
            };
        Ok(Response::ok(&json!({ "available": available })))
    }

    async fn check_identity(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        let body: IdentityBody = req.json()?;
        if parse_date(&body.birth_date).is_none() {
            return Err(invalid("birth_date", "Use a data no formato AAAA-MM-DD."));
        }
        // S-05: 5 conferências por conta em 24 horas.
        limits::hit(
            self.store.as_ref(),
            &format!("{account}:identity"),
            86_400,
            5,
            self.clock.epoch(),
            "attempts-exceeded",
        )
        .await?;
        let kyc: KycIdentity = internal::call(
            self.internal.as_ref(),
            "kyc",
            "check_identity",
            &json!({ "account_id": account, "birth_date": body.birth_date }),
        )
        .await?;
        match kyc.result.as_str() {
            "ok" => {}
            "underage" => {
                return Err(
                    problem("underage").with_field("birth_date", "É preciso ter 18 anos ou mais.")
                );
            }
            _ => {
                return Err(
                    problem("identity-mismatch").with_field("birth_date", "Confira a data.")
                );
            }
        }
        let check = CheckRecord {
            check_id: saci_ids::Id::new("chk").to_string(),
            kind: "identity".into(),
            official_name: kyc.official_name.clone(),
            cnpj_masked: None,
            legal_name: None,
            trade_name: None,
            at: self.clock.epoch(),
        };
        self.save_check(&account, &check).await?;
        Ok(Response::ok(
            &json!({ "check_id": check.check_id, "official_name": kyc.official_name.unwrap_or_default() }),
        ))
    }

    async fn check_business(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        let body: BusinessBody = req.json()?;
        if body.cnpj.len() != 14
            || !body.cnpj.bytes().all(|b| b.is_ascii_digit())
            || !saci_mask::is_valid_cnpj(&body.cnpj)
        {
            return Err(invalid("cnpj", "Confira o CNPJ."));
        }
        limits::hit(
            self.store.as_ref(),
            &format!("{account}:business"),
            86_400,
            10,
            self.clock.epoch(),
            "rate-limited",
        )
        .await?;
        let kyc: KycBusiness = internal::call(
            self.internal.as_ref(),
            "kyc",
            "check_business",
            &json!({ "account_id": account, "cnpj": body.cnpj }),
        )
        .await?;
        match kyc.result.as_str() {
            "ok" => {}
            "inactive" => return Err(problem("cnpj-inactive").with_field("cnpj", "CNPJ inativo.")),
            _ => {
                return Err(problem("cnpj-not-partner")
                    .with_field("cnpj", "O seu CPF não é sócio deste CNPJ."));
            }
        }
        let check = CheckRecord {
            check_id: saci_ids::Id::new("chk").to_string(),
            kind: "business".into(),
            official_name: None,
            cnpj_masked: kyc.cnpj_masked.clone(),
            legal_name: kyc.legal_name.clone(),
            trade_name: kyc.trade_name.clone(),
            at: self.clock.epoch(),
        };
        self.save_check(&account, &check).await?;
        let mut out = json!({ "check_id": check.check_id, "cnpj_masked": check.cnpj_masked, "legal_name": check.legal_name });
        if let Some(t) = check.trade_name {
            out["trade_name"] = json!(t);
        }
        Ok(Response::ok(&out))
    }

    async fn save_check(&self, account: &str, check: &CheckRecord) -> Result<(), Problem> {
        let item = Item::new(
            keys::account(account),
            format!("CHECK#{}", check.check_id),
            check,
        )?
        .expiring(check.at + CHECK_TTL_SECS);
        Ok(self.store.write(vec![WriteOp::put(item)]).await?)
    }

    async fn check(
        &self,
        account: &str,
        id: &str,
        kind: &str,
        field: &str,
    ) -> Result<CheckRecord, Problem> {
        match self
            .store
            .load::<CheckRecord>(&keys::account(account), &format!("CHECK#{id}"))
            .await?
        {
            Some((c, _)) if c.kind == kind => Ok(c),
            _ => Err(invalid(
                field,
                "Conferência vencida ou inexistente. Faça de novo.",
            )),
        }
    }

    async fn open(&self, req: &Request) -> HttpResult {
        let account = auth::recent_user(req, self.clock.epoch())?.to_owned();
        let body: OpenBody = req.json()?;
        if !is_postal_code(&body.postal_code) {
            return Err(invalid("postal_code", "Use os 8 dígitos do CEP."));
        }
        if body.accepted_seller_terms_version.trim().is_empty()
            || body.accepted_seller_terms_version.len() > 40
        {
            return Err(invalid(
                "accepted_seller_terms_version",
                "Aceite os termos da vitrine.",
            ));
        }
        if self.membership(&account).await?.is_some() {
            return Err(problem("storefront-exists"));
        }
        let identity = self
            .check(
                &account,
                &body.identity_check_id,
                "identity",
                "identity_check_id",
            )
            .await?;
        let business = match &body.business_check_id {
            Some(id) => Some(
                self.check(&account, id, "business", "business_check_id")
                    .await?,
            ),
            None => None,
        };
        let place: PostalCode = internal::call(
            self.internal.as_ref(),
            "shipping",
            "postal_code",
            &json!({ "postal_code": body.postal_code }),
        )
        .await?;
        let brand = business
            .as_ref()
            .and_then(|b| b.trade_name.clone().or_else(|| b.legal_name.clone()))
            .or_else(|| identity.official_name.clone())
            .unwrap_or_else(|| "Minha vitrine".into());
        let id = saci_ids::Id::new(saci_ids::prefix::STOREFRONT).to_string();
        #[derive(Deserialize)]
        struct Ref {
            #[serde(rename = "ref")]
            reference: String,
        }
        let address: Ref = internal::call(
            self.internal.as_ref(),
            "identity",
            "put_address",
            &json!({ "owner_account_id": account, "purpose": "business",
                     "address": { "postal_code": body.postal_code, "city": place.city, "state": place.state, "country": "BR" } }),
        )
        .await?;
        let now = self.clock.rfc3339();
        let record = StorefrontRecord {
            id: id.clone(),
            owner_account_id: account.clone(),
            brand_name: brand.clone(),
            slogan: None,
            url_name: String::new(),
            sells_with: if business.is_some() {
                "cnpj".into()
            } else {
                "cpf".into()
            },
            status: "draft".into(),
            location: Location {
                city: place.city,
                state: place.state,
                country: Some("BR".into()),
            },
            business_address_ref: Some(address.reference),
            cnpj_masked: business.and_then(|b| b.cnpj_masked),
            palette: None,
            font_option_id: None,
            logo_url: None,
            hero_image_url: None,
            accepted_seller_terms_version: Some(body.accepted_seller_terms_version),
            created_at: now.clone(),
            updated_at: Some(now),
        };
        // Endereço único: tenta o nome, depois -2, -3...
        let base = Some(slugify(&brand))
            .filter(|s| is_url_name(s) && !RESERVED_URL_NAMES.contains(&s.as_str()))
            .unwrap_or_else(|| "vitrine".into());
        let membership = Membership {
            storefront_id: id.clone(),
            role: "owner".into(),
            permissions: Vec::new(),
        };
        let opened = events::event(
            "sellers",
            "sellers.storefront.opened.v1",
            &id,
            &json!({ "storefront_id": id, "owner_account_id": account, "sells_with": record.sells_with }),
        )?;
        for n in 1..50 {
            let url_name = if n == 1 {
                base.clone()
            } else {
                format!("{}-{n}", base.chars().take(56).collect::<String>())
            };
            let mut record = record.clone();
            record.url_name = url_name.clone();
            let ops = vec![
                WriteOp::create(Item::new(keys::storefront(&id), keys::META, &record)?),
                WriteOp::create(Item::new(
                    keys::url(&url_name),
                    keys::META,
                    &UrlRecord {
                        storefront_id: id.clone(),
                        redirect_to: None,
                    },
                )?),
                WriteOp::create(Item::new(
                    keys::account(&account),
                    keys::MEMBERSHIP,
                    &membership,
                )?),
                WriteOp::Publish(opened.clone()),
            ];
            match self.store.write(ops).await {
                Ok(()) => {
                    self.grants
                        .put(
                            &account,
                            &Grant {
                                storefront_id: id.clone(),
                                role: "owner".into(),
                                sections: Vec::new(),
                            },
                        )
                        .await?;
                    return Ok(Response::json(
                        201,
                        &json!({ "storefront_id": id, "status": "draft" }),
                    ));
                }
                Err(StoreError::Conflict) => {
                    if self.membership(&account).await?.is_some() {
                        return Err(problem("storefront-exists"));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(problem("url-name-taken"))
    }

    // --- painel ------------------------------------------------------------------------------

    async fn workspace(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        let Some(m) = self.membership(&account).await? else {
            return Err(not_found());
        };
        let (s, _) = self.storefront(&m.storefront_id).await?;
        let counts = self
            .store
            .load::<Counts>(&keys::storefront(&s.id), keys::COUNTS)
            .await?
            .map(|(c, _)| c)
            .unwrap_or_default();
        let mut out = json!({
            "storefront_url_name": s.url_name, "brand_name": s.brand_name, "role": m.role,
            "permissions": if m.role == "owner" { Vec::new() } else { m.permissions },
            "sells_with": s.sells_with, "status": s.status, "counts": counts,
        });
        if let Some(logo) = s.logo_url.filter(|u| self.trusted(u)) {
            out["logo_url"] = json!(logo);
        }
        Ok(Response::ok(&out))
    }

    async fn private_storefront(&self, req: &Request) -> HttpResult {
        let id = auth::section(req, Section::Vitrine)?.to_owned();
        let (s, version) = self.storefront(&id).await?;
        Ok(Response::ok(&self.private_view(&s).await?).header("etag", &etag(version)))
    }

    async fn private_view(&self, s: &StorefrontRecord) -> Result<Value, Problem> {
        let mut v = self.public_view(s).await?;
        v["font_option_id"] = json!(s.font_option_id);
        if let Some(reference) = &s.business_address_ref {
            #[derive(Deserialize)]
            struct Opened {
                address: Value,
            }
            let opened: Opened = internal::call(
                self.internal.as_ref(),
                "identity",
                "open_address",
                &json!({ "ref": reference, "purpose": "business", "storefront_id": s.id }),
            )
            .await?;
            v["business_address"] = opened.address;
        }
        Ok(v)
    }

    async fn update_storefront(&self, req: &Request) -> HttpResult {
        let id = auth::section(req, Section::Vitrine)?.to_owned();
        let (mut s, version) = self.storefront(&id).await?;
        require_if_match(req, version)?;
        let body: UpdateBody = req.json()?;
        let brand = body.brand_name.trim();
        if !(2..=80).contains(&brand.chars().count()) {
            return Err(invalid("brand_name", "O nome tem de 2 a 80 caracteres."));
        }
        let words: Vec<String> = normalize(brand)
            .split(|c: char| !c.is_alphanumeric())
            .map(str::to_owned)
            .collect();
        if words
            .iter()
            .any(|w| FORBIDDEN_BRAND_WORDS.contains(&w.as_str()))
        {
            return Err(problem("name-not-allowed").with_field("brand_name", "Escolha outro nome."));
        }
        if body
            .slogan
            .as_ref()
            .is_some_and(|s| s.chars().count() > 160)
        {
            return Err(invalid("slogan", "A frase tem até 160 caracteres."));
        }
        let url_name = body.storefront_url_name.trim();
        if !is_url_name(url_name) {
            return Err(invalid(
                "storefront_url_name",
                "Use letras minúsculas, números e hífen (2 a 60).",
            ));
        }
        if RESERVED_URL_NAMES.contains(&url_name) {
            return Err(problem("url-name-taken")
                .with_field("storefront_url_name", "Este endereço é reservado."));
        }
        if let Some(p) = &body.visual_identity.palette {
            for (field, color) in [
                ("palette.primary", &p.primary),
                ("palette.accent", &p.accent),
            ] {
                if color.as_ref().is_some_and(|c| !is_hex_color(c)) {
                    return Err(invalid(field, "Use a cor no formato #RRGGBB."));
                }
            }
        }
        if let Some(font) = &body.visual_identity.font_option_id
            && self.font(Some(font)).await?.is_none()
        {
            return Err(invalid(
                "font_option_id",
                "Escolha um par de letras da lista.",
            ));
        }
        let logo = self
            .resolve_image(
                &id,
                body.visual_identity.logo_upload_id.as_ref(),
                s.logo_url.clone(),
                "logo_upload_id",
            )
            .await?;
        let hero = self
            .resolve_image(
                &id,
                body.visual_identity.hero_upload_id.as_ref(),
                s.hero_image_url.clone(),
                "hero_upload_id",
            )
            .await?;

        let previous_url = s.url_name.clone();
        s.brand_name = brand.to_owned();
        s.slogan = body
            .slogan
            .map(|x| x.trim().to_owned())
            .filter(|x| !x.is_empty());
        s.url_name = url_name.to_owned();
        s.palette = body.visual_identity.palette;
        s.font_option_id = body.visual_identity.font_option_id;
        s.logo_url = logo;
        s.hero_image_url = hero;
        s.updated_at = Some(self.clock.rfc3339());

        let mut data = json!({ "storefront_id": id });
        let mut ops = vec![WriteOp::replace(
            Item::new(keys::storefront(&id), keys::META, &s)?,
            version,
        )];
        if previous_url != s.url_name {
            data["previous_url_name"] = json!(previous_url);
            let taken = self
                .store
                .load::<UrlRecord>(&keys::url(&s.url_name), keys::META)
                .await?;
            let expect = match taken {
                // O próprio endereço antigo desta vitrine pode voltar a ser usado.
                Some((u, v)) if u.storefront_id == id => Expect::Version(v),
                Some(_) => {
                    return Err(problem("url-name-taken").with_field(
                        "storefront_url_name",
                        "Este endereço já é de outra vitrine.",
                    ));
                }
                None => Expect::Absent,
            };
            ops.push(WriteOp::Put {
                item: Item::new(
                    keys::url(&s.url_name),
                    keys::META,
                    &UrlRecord {
                        storefront_id: id.clone(),
                        redirect_to: None,
                    },
                )?,
                expect,
            });
            // O endereço antigo redireciona e fica reservado por 12 meses (S-21).
            ops.push(WriteOp::put(
                Item::new(
                    keys::url(&previous_url),
                    keys::META,
                    &UrlRecord {
                        storefront_id: id.clone(),
                        redirect_to: Some(s.url_name.clone()),
                    },
                )?
                .expiring(self.clock.epoch() + 365 * 86_400),
            ));
        }
        ops.push(WriteOp::Publish(events::event(
            "sellers",
            "sellers.storefront.updated.v1",
            &id,
            &data,
        )?));
        self.store.write(ops).await.map_err(|e| match e {
            StoreError::Conflict => problem("precondition-failed"),
            other => other.into(),
        })?;
        Ok(Response::ok(&self.private_view(&s).await?).header("etag", &etag(version + 1)))
    }

    /// `upl_` novo (pronto no `media`), `null` tira, ausente mantém.
    async fn resolve_image(
        &self,
        storefront: &str,
        value: Option<&Option<String>>,
        current: Option<String>,
        field: &str,
    ) -> Result<Option<String>, Problem> {
        match value {
            None => Ok(current),
            Some(None) => Ok(None),
            Some(Some(upload)) => {
                #[derive(Deserialize)]
                struct Out {
                    items: Vec<Value>,
                }
                let out: Out = internal::call(
                    self.internal.as_ref(),
                    "media",
                    "uploads_for_listing",
                    &json!({ "storefront_id": storefront, "upload_ids": [upload] }),
                )
                .await?;
                let state = out.items.first().cloned().unwrap_or(Value::Null);
                match state["status"].as_str() {
                    Some("ready") => Ok(state["public_url"].as_str().map(str::to_owned)),
                    Some("pending" | "scanning") => Err(problem("photo-not-ready")),
                    _ => Err(invalid(
                        field,
                        "A imagem foi recusada ou não existe. Envie outra.",
                    )),
                }
            }
        }
    }

    // --- equipe -------------------------------------------------------------------------------

    async fn team(&self, req: &Request) -> HttpResult {
        let id = auth::owner(req)?.to_owned();
        let members = self.members(&id).await?;
        Ok(Response::ok(&Page::all(
            members.iter().map(MemberRecord::public).collect::<Vec<_>>(),
        )))
    }

    async fn members(&self, storefront: &str) -> Result<Vec<MemberRecord>, Problem> {
        let items = self
            .store
            .query_all(keys::storefront(storefront), "MEMBER#".into())
            .await?;
        Ok(items.iter().map(Item::parse).collect::<Result<_, _>>()?)
    }

    async fn invite(&self, req: &Request) -> HttpResult {
        let id = auth::recent_owner(req, self.clock.epoch())?.to_owned();
        let owner = auth::user(req)?.to_owned();
        let body: InviteBody = req.json()?;
        let permissions = validate_permissions(&body.permissions)?;
        if body.cpf.len() != 11
            || !body.cpf.bytes().all(|b| b.is_ascii_digit())
            || !saci_mask::is_valid_cpf(&body.cpf)
        {
            return Err(invalid("cpf", "Confira o CPF."));
        }
        let (s, _) = self.storefront(&id).await?;
        if s.sells_with != "cnpj" {
            return Err(problem("needs-cnpj"));
        }
        let lookup: CpfLookup = internal::call(
            self.internal.as_ref(),
            "identity",
            "lookup_cpf",
            &json!({ "cpf": body.cpf }),
        )
        .await?;
        if lookup.account_id.as_deref() == Some(owner.as_str()) {
            return Err(problem("already-member"));
        }
        let members = self.members(&id).await?;
        if members
            .iter()
            .any(|m| m.cpf_fingerprint.as_deref() == Some(lookup.fingerprint.as_str()))
        {
            return Err(problem("already-member"));
        }
        // S-04: limite diário por vitrine, contra consulta em massa.
        limits::hit(
            self.store.as_ref(),
            &format!("{id}:invites"),
            86_400,
            20,
            self.clock.epoch(),
            "invites-exceeded",
        )
        .await?;
        let member = MemberRecord {
            id: saci_ids::Id::new("tmb").to_string(),
            storefront_id: id.clone(),
            account_id: lookup.account_id.clone(),
            cpf_masked: saci_commerce_kit::text::mask_cpf(&body.cpf),
            cpf_fingerprint: Some(lookup.fingerprint.clone()),
            name: None,
            status: "invited".into(),
            permissions,
            added_at: self.clock.rfc3339(),
        };
        let invitation = json!({ "member_id": member.id, "storefront_id": id });
        let mut ops = vec![WriteOp::create(Item::new(
            keys::storefront(&id),
            format!("MEMBER#{}", member.id),
            &member,
        )?)];
        match &lookup.account_id {
            Some(account) => {
                ops.push(WriteOp::put(Item::new(
                    keys::invitations(account),
                    member.id.clone(),
                    &invitation,
                )?));
                ops.push(WriteOp::Publish(member_event(&id, account, "invited")?));
            }
            None => ops.push(WriteOp::put(Item::new(
                keys::pending(&lookup.fingerprint),
                member.id.clone(),
                &invitation,
            )?)),
        }
        self.store.write(ops).await?;
        // Mesma resposta com ou sem conta: não revela quem tem cadastro (S-04).
        Ok(Response::json(202, &member.public()))
    }

    async fn member(
        &self,
        storefront: &str,
        member_id: &str,
    ) -> Result<(MemberRecord, u64), Problem> {
        self.store
            .load::<MemberRecord>(
                &keys::storefront(storefront),
                &format!("MEMBER#{member_id}"),
            )
            .await?
            .ok_or_else(not_found)
    }

    async fn change_member(&self, req: &Request, member_id: &str) -> HttpResult {
        let id = auth::recent_owner(req, self.clock.epoch())?.to_owned();
        let body: PermissionsBody = req.json()?;
        let permissions = validate_permissions(&body.permissions)?;
        let (mut m, version) = self.member(&id, member_id).await?;
        m.permissions = permissions;
        let mut ops = vec![WriteOp::replace(
            Item::new(keys::storefront(&id), format!("MEMBER#{member_id}"), &m)?,
            version,
        )];
        if let (Some(account), "active") = (&m.account_id, m.status.as_str()) {
            let membership = Membership {
                storefront_id: id.clone(),
                role: "member".into(),
                permissions: m.permissions.clone(),
            };
            ops.push(WriteOp::put(Item::new(
                keys::account(account),
                keys::MEMBERSHIP,
                &membership,
            )?));
            ops.push(WriteOp::Publish(member_event(&id, account, "updated")?));
        }
        self.store.write(ops).await?;
        if let (Some(account), "active") = (&m.account_id, m.status.as_str()) {
            self.grants
                .put(
                    account,
                    &Grant {
                        storefront_id: id.clone(),
                        role: "member".into(),
                        sections: m.permissions.clone(),
                    },
                )
                .await?;
        }
        Ok(Response::ok(&m.public()))
    }

    async fn remove_member(&self, req: &Request, member_id: &str) -> HttpResult {
        let id = auth::recent_owner(req, self.clock.epoch())?.to_owned();
        let (m, _) = self.member(&id, member_id).await?;
        let mut ops = vec![WriteOp::delete(
            keys::storefront(&id),
            format!("MEMBER#{member_id}"),
        )];
        if let Some(account) = &m.account_id {
            ops.push(WriteOp::delete(keys::invitations(account), member_id));
            if m.status == "active" {
                ops.push(WriteOp::delete(keys::account(account), keys::MEMBERSHIP));
                ops.push(WriteOp::Publish(member_event(&id, account, "removed")?));
            }
        }
        if let Some(fp) = &m.cpf_fingerprint {
            ops.push(WriteOp::delete(keys::pending(fp), member_id));
        }
        self.store.write(ops).await?;
        if let (Some(account), "active") = (&m.account_id, m.status.as_str()) {
            // Efeito em até 60 s: o cache do autorizador.
            self.grants.remove(account).await?;
        }
        Ok(Response::no_content())
    }

    async fn invitations(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        let items = self
            .store
            .query_all(keys::invitations(&account), String::new())
            .await?;
        let mut out = Vec::new();
        for item in items {
            let inv: Value = item.parse()?;
            let storefront = inv["storefront_id"].as_str().unwrap_or_default().to_owned();
            if let Ok((m, _)) = self.member(&storefront, &item.sk).await
                && m.status == "invited"
            {
                let (s, _) = self.storefront(&storefront).await?;
                out.push(
                    json!({ "id": m.id, "brand_name": s.brand_name, "permissions": m.permissions }),
                );
            }
        }
        Ok(Response::ok(&Page::all(out)))
    }

    async fn answer(&self, req: &Request, member_id: &str, accept: bool) -> HttpResult {
        let account = auth::recent_user(req, self.clock.epoch())?.to_owned();
        let Some((inv, _)) = self
            .store
            .load::<Value>(&keys::invitations(&account), member_id)
            .await?
        else {
            return Err(not_found());
        };
        let storefront = inv["storefront_id"].as_str().unwrap_or_default().to_owned();
        let (mut m, version) = self.member(&storefront, member_id).await?;
        if m.account_id.as_deref() != Some(account.as_str()) || m.status != "invited" {
            return Err(not_found());
        }
        if !accept {
            self.store
                .write(vec![
                    WriteOp::delete(keys::invitations(&account), member_id),
                    WriteOp::delete(keys::storefront(&storefront), format!("MEMBER#{member_id}")),
                ])
                .await?;
            return Ok(Response::no_content());
        }
        if self.membership(&account).await?.is_some() {
            return Err(
                problem("already-member").with_message("Esta conta já participa de uma vitrine.")
            );
        }
        #[derive(Deserialize)]
        struct Profile {
            #[serde(default)]
            name: Option<String>,
        }
        let profile: Profile = internal::call(
            self.internal.as_ref(),
            "identity",
            "account_profile",
            &json!({ "account_id": account }),
        )
        .await?;
        m.status = "active".into();
        m.name = profile.name;
        m.added_at = self.clock.rfc3339();
        let membership = Membership {
            storefront_id: storefront.clone(),
            role: "member".into(),
            permissions: m.permissions.clone(),
        };
        self.store
            .write(vec![
                WriteOp::replace(
                    Item::new(
                        keys::storefront(&storefront),
                        format!("MEMBER#{member_id}"),
                        &m,
                    )?,
                    version,
                ),
                WriteOp::create(Item::new(
                    keys::account(&account),
                    keys::MEMBERSHIP,
                    &membership,
                )?),
                WriteOp::delete(keys::invitations(&account), member_id),
                WriteOp::Publish(member_event(&storefront, &account, "accepted")?),
            ])
            .await?;
        self.grants
            .put(
                &account,
                &Grant {
                    storefront_id: storefront,
                    role: "member".into(),
                    sections: m.permissions.clone(),
                },
            )
            .await?;
        Ok(Response::no_content())
    }

    // --- apoio --------------------------------------------------------------------------------

    /// A vitrine da conta, quando há.
    pub async fn membership(&self, account: &str) -> Result<Option<Membership>, Problem> {
        Ok(self
            .store
            .load::<Membership>(&keys::account(account), keys::MEMBERSHIP)
            .await?
            .map(|(m, _)| m))
    }

    /// Uma vitrine (404 se não existe).
    pub async fn storefront(&self, id: &str) -> Result<(StorefrontRecord, u64), Problem> {
        self.store
            .load::<StorefrontRecord>(&keys::storefront(id), keys::META)
            .await?
            .ok_or_else(not_found)
    }
}

fn validate_permissions(permissions: &[String]) -> Result<Vec<String>, Problem> {
    // `financeiro` e `equipe` nunca são liberáveis.
    if permissions.is_empty() || permissions.iter().any(|p| !SECTIONS.contains(&p.as_str())) {
        return Err(invalid(
            "permissions",
            "Escolha ao menos uma seção: anúncios, vitrine, pedidos ou atendimento.",
        ));
    }
    let mut out: Vec<String> = permissions.to_vec();
    out.sort();
    out.dedup();
    Ok(out)
}

fn member_event(storefront: &str, account: &str, change: &str) -> Result<Event, Problem> {
    events::event(
        "sellers",
        "sellers.team.member_changed.v1",
        storefront,
        &json!({ "storefront_id": storefront, "member_account_id": account, "change": change }),
    )
}
