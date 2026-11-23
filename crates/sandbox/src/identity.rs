//! Sandbox do `identity`: contas, sessões e o cofre de dados pessoais.
//!
//! Rotas HTTP (contrato `saci-contracts/http/identity.yaml`):
//! `GET /password-policy`, `POST /accounts`, `POST /accounts/from-order`,
//! `POST /verifications/{id}/confirm|resend`, `POST /me/contact-verifications`
//! (e `/confirm`), `PUT /me/document`, `POST /sessions`,
//! `POST /sessions/refresh`, `POST /sessions/step-up`, `DELETE /sessions/current`,
//! `DELETE /sessions`, `POST /password-resets`, `POST /password-resets/confirm`,
//! `PUT /me/password`, `POST /me/contact-confirmations`.
//!
//! Como o `saci-identity` (#27): o cadastro pede só nome, e-mail **ou**
//! celular e senha, sem CPF; a conta abre na hora (201, com a sessão) e o
//! contato fica não confirmado até o código. Um contato, uma conta: com um
//! contato já cadastrado, 202 "código enviado", sem sessão. A entrada aceita
//! e-mail ou celular (E.164); o CPF não serve para entrar.
//!
//! Regras do sandbox: o código é sempre [`VERIFICATION_CODE`]; qualquer conta
//! de exemplo entra com a senha das sementes; a sessão é um JWT ES256 de 10
//! minutos com refresh rotativo (cookie `HttpOnly` no site, corpo no app),
//! como na proposta (7.6). O sandbox confere só o formato do celular; a lista
//! de DDIs aceitos é conferida pelo cofre do `saci-identity`.
//!
//! Tabela (porta de armazenamento do sandbox):
//!
//! | `pk` | `sk` | Documento |
//! | --- | --- | --- |
//! | `ACC#<conta>` | `META` | [`Account`] |
//! | `DOC#<CPF ou CNPJ>` | `META` | conta do documento |
//! | `CONTACT#<e-mail ou celular>` | `META` | conta do contato |
//! | `VER#<id>` | `META` | cadastro com contato já cadastrado (nunca confere) |
//! | `RESET#<id>` | `META` | recuperação de senha |
//! | `RT#<token>` | `META` | refresh token |
//! | `ADDR#<ref>` | `META` | endereço guardado (cofre) |
//! | `GUEST#<pedido>` | `META` | comprador sem conta (cofre) |
//! | `REVOKED#<conta>` | `META` | "encerrar em todos os aparelhos" |

use saci_commerce_kit::auth;
use saci_commerce_kit::clock::rfc3339_epoch;
use saci_commerce_kit::cursor::hmac_sha256;
use saci_commerce_kit::events;
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::internal::{InternalHandler, payload, reply, unknown_op};
use saci_commerce_kit::problem::{invalid, not_found, problem};
use saci_commerce_kit::store::{Item, StoreExt, WriteOp};
use saci_commerce_kit::text::{mask_cnpj, mask_cpf, mask_email, mask_phone, normalize_phone};
use saci_commerce_kit::{BoxFut, Problem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Sandbox, VERIFICATION_CODE, hash_password};

/// Vida do token de acesso (10 minutos).
pub const ACCESS_TTL: u64 = 600;
/// Vida máxima da sessão (30 dias).
pub const SESSION_TTL: u64 = 30 * 86_400;
/// Cookie do refresh no site.
pub const REFRESH_COOKIE: &str = "__Secure-saci_rt";

/// Uma conta do sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// Id (`acc_`).
    pub id: String,
    /// CPF ou CNPJ, só dígitos (fictício). Opcional desde o #27.
    #[serde(default)]
    pub document: Option<String>,
    /// Nome de registro.
    #[serde(default)]
    pub name: Option<String>,
    /// Nome preferido (o do cadastro).
    #[serde(default)]
    pub preferred_name: Option<String>,
    /// E-mail.
    #[serde(default)]
    pub email: Option<String>,
    /// E-mail confirmado com o código.
    #[serde(default)]
    pub email_verified: bool,
    /// Celular (E.164).
    #[serde(default)]
    pub phone: Option<String>,
    /// Celular confirmado com o código.
    #[serde(default)]
    pub phone_verified: bool,
    /// Troca de contato à espera de confirmação.
    #[serde(default)]
    pub pending_contact: Option<Value>,
    /// Hash da senha.
    pub password_hash: String,
    /// Criação.
    pub created_at: String,
}

impl Account {
    /// `AccountProfile` (contato em claro: o sandbox não tem cofre).
    #[must_use]
    pub fn profile(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "created_at": self.created_at,
        });
        if let Some(d) = &self.document {
            v["document_type"] = json!(if d.len() == 14 { "cnpj" } else { "cpf" });
            v["document_masked"] = json!(if d.len() == 14 {
                mask_cnpj(d)
            } else {
                mask_cpf(d)
            });
        }
        for (k, val) in [
            ("name", &self.name),
            ("preferred_name", &self.preferred_name),
            ("email", &self.email),
            ("phone", &self.phone),
        ] {
            if let Some(val) = val {
                v[k] = json!(val);
            }
        }
        if self.email.is_some() {
            v["email_verified"] = json!(self.email_verified);
        }
        if self.phone.is_some() {
            v["phone_verified"] = json!(self.phone_verified);
        }
        v
    }

    /// O primeiro contato ainda não confirmado: canal e destino.
    fn unverified(&self) -> Option<(&'static str, String)> {
        match (&self.email, &self.phone) {
            (Some(e), _) if !self.email_verified => Some(("email", e.clone())),
            (_, Some(p)) if !self.phone_verified => Some(("phone", p.clone())),
            _ => None,
        }
    }
}

fn masked(channel: &str, destination: &str) -> String {
    if channel == "email" {
        mask_email(destination)
    } else {
        mask_phone(destination)
    }
}

#[derive(Debug, Deserialize)]
struct DemoAccounts {
    password: String,
    accounts: Vec<DemoAccount>,
}

#[derive(Debug, Deserialize)]
struct DemoAccount {
    id: String,
    cpf_base: String,
    name: String,
    #[serde(default)]
    preferred_name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    phone: Option<String>,
    created_at: String,
    #[serde(default)]
    address: Option<Value>,
    #[serde(default)]
    payment: Option<Value>,
}

/// O CPF de exemplo a partir dos 9 primeiros dígitos (nada válido escrito no repositório).
#[must_use]
pub fn cpf_from_base(base: &str) -> String {
    let digits: Vec<u8> = base
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    let [a, b] = saci_mask::cpf_check_digits(&digits);
    format!("{base}{a}{b}")
}

/// A chave do índice de um contato (e-mail em minúsculas, celular em E.164).
fn contact_key(value: &str) -> String {
    format!("CONTACT#{}", value.trim().to_lowercase())
}

impl Sandbox {
    /// Carrega as contas de exemplo (`local-env/sandbox/demo-accounts.json`).
    pub async fn seed_accounts(&self) -> Result<(), Problem> {
        let demo: DemoAccounts = serde_json::from_str(include_str!(
            "../../../local-env/sandbox/demo-accounts.json"
        ))
        .map_err(|e| saci_commerce_kit::problem::internal(&e))?;
        for d in demo.accounts {
            let document = cpf_from_base(&d.cpf_base);
            let phone = d.phone.as_deref().and_then(normalize_phone);
            let account = Account {
                id: d.id.clone(),
                document: Some(document.clone()),
                name: Some(d.name),
                preferred_name: d.preferred_name,
                email_verified: d.email.is_some(),
                email: d.email.clone(),
                phone_verified: phone.is_some(),
                phone: phone.clone(),
                pending_contact: None,
                password_hash: hash_password(&d.id, &demo.password),
                created_at: d.created_at,
            };
            let index = |key: String| {
                Item::new(key, "META", &json!({ "account_id": d.id })).map(WriteOp::put)
            };
            let mut ops = vec![
                WriteOp::put(Item::new(format!("ACC#{}", d.id), "META", &account)?),
                index(format!("DOC#{document}"))?,
            ];
            for contact in [d.email.as_deref(), phone.as_deref()].into_iter().flatten() {
                ops.push(index(contact_key(contact))?);
            }
            if let Some(pay) = d.payment {
                ops.push(WriteOp::put(Item::new(
                    format!("PAYPREF#{}", d.id),
                    "META",
                    &pay,
                )?));
            }
            self.store.write(ops).await?;
            if let Some(address) = d.address {
                // O endereço padrão da conta de exemplo, como o `customers` guardaria.
                let reference = self.put_address(&d.id, "delivery", address).await?;
                self.store
                    .write(vec![WriteOp::put(Item::new(
                        format!("DEFAULTADDR#{}", d.id),
                        "META",
                        &json!({ "ref": reference }),
                    )?)])
                    .await?;
            }
        }
        Ok(())
    }

    /// A conta (404 se não existe).
    pub async fn account(&self, id: &str) -> Result<(Account, u64), Problem> {
        self.store
            .load::<Account>(&format!("ACC#{id}"), "META")
            .await?
            .ok_or_else(not_found)
    }

    async fn account_by_key(&self, key: &str) -> Result<Option<Account>, Problem> {
        match self.store.load::<Value>(key, "META").await? {
            Some((v, _)) => Ok(Some(
                self.account(v["account_id"].as_str().unwrap_or_default())
                    .await?
                    .0,
            )),
            None => Ok(None),
        }
    }

    async fn account_by_document(&self, document: &str) -> Result<Option<Account>, Problem> {
        self.account_by_key(&format!("DOC#{document}")).await
    }

    /// A conta pelo que se digita para entrar: e-mail ou celular (E.164). O
    /// CPF/CNPJ não serve para entrar (#27).
    async fn account_by_login(&self, login: &str) -> Result<Option<Account>, Problem> {
        let login = login.trim();
        if login.contains('@') {
            self.account_by_key(&contact_key(login)).await
        } else if login.starts_with('+') {
            match normalize_phone(login) {
                Some(p) => self.account_by_key(&contact_key(&p)).await,
                None => Ok(None),
            }
        } else {
            // Só números: o celular brasileiro sem DDI, como o cofre do identity.
            match normalize_phone(login) {
                Some(p) if (10..=11).contains(&login.len()) => {
                    self.account_by_key(&contact_key(&p)).await
                }
                _ => Ok(None),
            }
        }
    }

    /// Emite uma sessão: token de acesso de 10 minutos e refresh rotativo.
    pub async fn session(
        &self,
        account: &Account,
        auth_time: u64,
        web: bool,
    ) -> Result<Response, Problem> {
        let now = self.clock.epoch();
        let claims = json!({
            "iss": self.issuer, "sub": account.id, "account_id": account.id, "client_id": self.audience,
            "token_use": "access", "auth_time": auth_time, "scope": "saci/api",
            "iat": now, "exp": now + ACCESS_TTL, "jti": saci_ids::Id::new("jti").to_string(),
        });
        let access = self
            .session_key
            .sign(&claims)
            .map_err(|e| saci_commerce_kit::problem::internal(&e))?;
        let refresh = format!(
            "rt_{}",
            saci_ids::Id::new("rt").as_str().trim_start_matches("rt_")
        );
        let rt = json!({ "account_id": account.id, "auth_time": auth_time, "issued": now });
        self.store
            .write(vec![WriteOp::put(
                Item::new(format!("RT#{refresh}"), "META", &rt)?.expiring(now + SESSION_TTL),
            )])
            .await?;
        let mut body = json!({
            "access_token": access, "expires_at": rfc3339_epoch(now + ACCESS_TTL),
            "auth_time": rfc3339_epoch(auth_time), "account": account.profile(),
        });
        let mut response;
        if web {
            response = Response::ok(&body);
            response = response.header(
                "set-cookie",
                &format!("{REFRESH_COOKIE}={refresh}; Path=/v1/identity/sessions; HttpOnly; Secure; SameSite=Strict; Max-Age={SESSION_TTL}"),
            );
        } else {
            body["refresh_token"] = json!(refresh);
            response = Response::ok(&body);
        }
        Ok(response)
    }

    /// Guarda um endereço no cofre e devolve a referência (`addr_`).
    pub async fn put_address(
        &self,
        owner: &str,
        purpose: &str,
        address: Value,
    ) -> Result<String, Problem> {
        let reference = saci_ids::Id::new(saci_ids::prefix::ADDRESS_REF).to_string();
        self.store
            .write(vec![WriteOp::put(Item::new(
                format!("ADDR#{reference}"),
                "META",
                &json!({ "owner": owner, "purpose": purpose, "address": address }),
            )?)])
            .await?;
        Ok(reference)
    }

    /// "Código enviado" de quem tentou um contato já cadastrado: nunca confere.
    async fn neutral_verification(
        &self,
        channel: &str,
        destination: &str,
    ) -> Result<Response, Problem> {
        let id = saci_ids::Id::new("ver").to_string();
        let expires = self.clock.epoch() + 15 * 60;
        self.store
            .write(vec![WriteOp::put(
                Item::new(
                    format!("VER#{id}"),
                    "META",
                    &json!({ "existing": true, "attempts": 0 }),
                )?
                .expiring(expires + 3_600),
            )])
            .await?;
        tracing::info!(
            channel,
            "sandbox: contato já cadastrado; aviso ao dono, nenhuma conta aberta"
        );
        Ok(Response::json(
            202,
            &json!({
                "verification_id": id, "channel": channel,
                "masked_destination": masked(channel, destination),
                "expires_at": rfc3339_epoch(expires),
            }),
        ))
    }

    /// Abre a conta nova: índices, evento e a sessão (201) com a confirmação do
    /// contato em andamento.
    async fn open_account(&self, account: &Account, web: bool) -> Result<Response, Problem> {
        let mut ops = vec![WriteOp::create(Item::new(
            format!("ACC#{}", account.id),
            "META",
            account,
        )?)];
        let index = |key: String| {
            Item::new(key, "META", &json!({ "account_id": account.id })).map(WriteOp::create)
        };
        for contact in [account.email.as_deref(), account.phone.as_deref()]
            .into_iter()
            .flatten()
        {
            ops.push(index(contact_key(contact))?);
        }
        if let Some(d) = &account.document {
            ops.push(index(format!("DOC#{d}"))?);
        }
        self.store.write(ops).await?;
        self.account_verified(account).await?;
        let now = self.clock.epoch();
        let mut response = self.session(account, now, web).await?;
        let mut body: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
        if let Some((channel, destination)) = account.unverified() {
            tracing::info!(
                channel,
                "sandbox: código de confirmação do contato {VERIFICATION_CODE}"
            );
            body["contact_verification"] = json!({
                "channel": channel,
                "masked_destination": masked(channel, &destination),
                "expires_at": rfc3339_epoch(now + 15 * 60),
            });
        }
        response.status = 201;
        response.body = body.to_string();
        Ok(response)
    }

    async fn account_verified(&self, account: &Account) -> Result<(), Problem> {
        let event = events::event(
            "identity",
            "identity.account.verified.v1",
            &account.id,
            &json!({ "account_id": account.id }),
        )?;
        Ok(self.store.write(vec![WriteOp::Publish(event)]).await?)
    }
}

/// As rotas HTTP de `/v1/identity`.
#[derive(Debug, Clone)]
pub struct IdentityHttp(pub Sandbox);

#[derive(Debug, Deserialize)]
struct SignUp {
    name: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    phone: Option<String>,
    password: String,
    accepted_terms_version: String,
}

fn cookie(req: &Request, name: &str) -> Option<String> {
    req.header("cookie")?
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

fn is_web(req: &Request) -> bool {
    req.header("x-client").is_none_or(|c| c.starts_with("web"))
}

/// O contato do cadastro: exatamente um, e-mail ou celular (em E.164).
fn one_contact(
    email: Option<&str>,
    phone: Option<&str>,
) -> Result<(&'static str, String), Problem> {
    let email = email.map(str::trim).filter(|e| !e.is_empty());
    let phone = phone.map(str::trim).filter(|p| !p.is_empty());
    match (email, phone) {
        (Some(e), None) => {
            let e = e.to_lowercase();
            if !e.contains('@') || e.split('@').nth(1).is_none_or(|d| !d.contains('.')) {
                return Err(invalid("email", "Confira o e-mail."));
            }
            Ok(("email", e))
        }
        (None, Some(p)) => normalize_phone(p)
            .map(|p| ("phone", p))
            .ok_or_else(|| invalid("phone", "Confira o celular, com o DDD.")),
        (Some(_), Some(_)) => Err(invalid(
            "phone",
            "Informe só um contato: e-mail ou celular.",
        )),
        (None, None) => Err(invalid("email", "Informe um e-mail ou um celular.")),
    }
}

impl HttpService for IdentityHttp {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

impl IdentityHttp {
    #[allow(clippy::too_many_lines)]
    async fn route(&self, req: &Request) -> HttpResult {
        let s = &self.0;
        let now = s.clock.epoch();
        match (req.method.as_str(), req.segments().as_slice()) {
            ("GET", ["v1", "identity", "password-policy"]) => Ok(Response::ok(
                &json!({ "description": "Use ao menos 8 caracteres.", "min_length": 8 }),
            )
            .public(3_600)),
            ("POST", ["v1", "identity", "accounts"]) => {
                let b: SignUp = req.json()?;
                let name = b.name.trim().to_owned();
                if name.chars().count() < 2 {
                    return Err(invalid("name", "Informe o seu nome."));
                }
                let (channel, destination) = one_contact(b.email.as_deref(), b.phone.as_deref())?;
                if b.password.chars().count() < 8 {
                    return Err(problem("weak-password")
                        .with_field("password", "Use ao menos 8 caracteres."));
                }
                if b.accepted_terms_version.is_empty() {
                    return Err(invalid("accepted_terms_version", "Aceite os termos."));
                }
                // Um contato, uma conta: "código enviado", sem sessão (S-15).
                if s.account_by_key(&contact_key(&destination))
                    .await?
                    .is_some()
                {
                    return s.neutral_verification(channel, &destination).await;
                }
                let id = saci_ids::Id::new(saci_ids::prefix::ACCOUNT).to_string();
                let account = Account {
                    password_hash: hash_password(&id, &b.password),
                    id,
                    document: None,
                    name: None,
                    preferred_name: Some(name),
                    email: (channel == "email").then(|| destination.clone()),
                    email_verified: false,
                    phone: (channel == "phone").then(|| destination.clone()),
                    phone_verified: false,
                    pending_contact: None,
                    created_at: s.clock.rfc3339(),
                };
                s.open_account(&account, is_web(req)).await
            }
            ("POST", ["v1", "identity", "accounts", "from-order"]) => {
                let order_id = auth::guest_order(req)?.to_owned();
                #[derive(Deserialize)]
                struct Body {
                    order_id: String,
                    password: String,
                }
                let b: Body = req.json()?;
                if b.order_id != order_id {
                    return Err(not_found());
                }
                if b.password.chars().count() < 8 {
                    return Err(problem("weak-password"));
                }
                let (buyer, _) = s
                    .store
                    .load::<Value>(&format!("GUEST#{order_id}"), "META")
                    .await?
                    .ok_or_else(not_found)?;
                let email = buyer["email"].as_str().map(str::to_lowercase);
                let phone = buyer["phone"].as_str().and_then(normalize_phone);
                let (channel, destination) = match (&email, &phone) {
                    (Some(e), _) => ("email", e.clone()),
                    (None, Some(p)) => ("phone", p.clone()),
                    _ => return Err(problem("contact-required")),
                };
                for contact in [email.as_deref(), phone.as_deref()].into_iter().flatten() {
                    if s.account_by_key(&contact_key(contact)).await?.is_some() {
                        return s.neutral_verification(channel, &destination).await;
                    }
                }
                // O CPF da nota fiscal entra na conta só se ainda não é de outra.
                let document = match buyer["cpf"].as_str().filter(|c| !c.is_empty()) {
                    Some(cpf) if s.account_by_document(cpf).await?.is_none() => {
                        Some(cpf.to_owned())
                    }
                    _ => None,
                };
                let id = saci_ids::Id::new(saci_ids::prefix::ACCOUNT).to_string();
                let account = Account {
                    password_hash: hash_password(&id, &b.password),
                    id,
                    document,
                    name: None,
                    preferred_name: buyer["name"].as_str().map(str::to_owned),
                    email,
                    email_verified: false,
                    phone,
                    phone_verified: false,
                    pending_contact: None,
                    created_at: s.clock.rfc3339(),
                };
                s.open_account(&account, is_web(req)).await
            }
            ("POST", ["v1", "identity", "verifications", id, "confirm"]) => {
                // Só o cadastro com contato já cadastrado chega aqui: nunca confere.
                let (mut ver, version) = s
                    .store
                    .load::<Value>(&format!("VER#{id}"), "META")
                    .await?
                    .ok_or_else(|| problem("code-expired"))?;
                let attempts = ver["attempts"].as_u64().unwrap_or(0);
                if attempts >= 5 {
                    return Err(problem("attempts-exceeded"));
                }
                ver["attempts"] = json!(attempts + 1);
                s.store
                    .write(vec![WriteOp::replace(
                        Item::new(format!("VER#{id}"), "META", &ver)?,
                        version,
                    )])
                    .await?;
                Err(problem("invalid-code").with_field("code", "O código não confere."))
            }
            ("POST", ["v1", "identity", "verifications", id, "resend"]) => {
                if s.store.get(&format!("VER#{id}"), "META").await?.is_none() {
                    return Err(problem("code-expired"));
                }
                Ok(Response::json(202, &json!({})))
            }
            ("POST", ["v1", "identity", "me", "contact-verifications"]) => {
                let account_id = auth::user(req)?.to_owned();
                let (account, _) = s.account(&account_id).await?;
                let (channel, destination) = account
                    .unverified()
                    .ok_or_else(|| problem("contact-already-verified"))?;
                tracing::info!(
                    channel,
                    "sandbox: código de confirmação do contato {VERIFICATION_CODE}"
                );
                Ok(Response::json(
                    202,
                    &json!({
                        "channel": channel,
                        "masked_destination": masked(channel, &destination),
                        "expires_at": rfc3339_epoch(now + 15 * 60),
                    }),
                ))
            }
            ("POST", ["v1", "identity", "me", "contact-verifications", "confirm"]) => {
                let account_id = auth::user(req)?.to_owned();
                #[derive(Deserialize)]
                struct Body {
                    code: String,
                }
                let b: Body = req.json()?;
                let (mut account, version) = s.account(&account_id).await?;
                let Some((channel, _)) = account.unverified() else {
                    return Err(problem("code-expired"));
                };
                if b.code != VERIFICATION_CODE {
                    return Err(problem("invalid-code").with_field("code", "O código não confere."));
                }
                if channel == "email" {
                    account.email_verified = true;
                } else {
                    account.phone_verified = true;
                }
                s.store
                    .write(vec![WriteOp::replace(
                        Item::new(format!("ACC#{account_id}"), "META", &account)?,
                        version,
                    )])
                    .await?;
                Ok(Response::no_content())
            }
            ("PUT", ["v1", "identity", "me", "document"]) => {
                let account_id = auth::recent_user(req, now)?.to_owned();
                #[derive(Deserialize)]
                struct Body {
                    document: String,
                }
                let b: Body = req.json()?;
                let document: String = b.document.chars().filter(char::is_ascii_digit).collect();
                if document.len() != 11 || !saci_mask::is_valid_cpf(&document) {
                    return Err(invalid("document", "Confira o CPF."));
                }
                let (mut account, version) = s.account(&account_id).await?;
                if account.document.is_some() {
                    return Err(problem("document-already-set"));
                }
                if s.account_by_document(&document).await?.is_some() {
                    return Err(problem("document-unavailable"));
                }
                account.document = Some(document.clone());
                let event = events::event(
                    "identity",
                    "identity.account.document_added.v1",
                    &account_id,
                    &json!({ "account_id": account_id, "document_type": "cpf" }),
                )?;
                s.store
                    .write(vec![
                        WriteOp::replace(
                            Item::new(format!("ACC#{account_id}"), "META", &account)?,
                            version,
                        ),
                        WriteOp::create(Item::new(
                            format!("DOC#{document}"),
                            "META",
                            &json!({ "account_id": account_id }),
                        )?),
                        WriteOp::Publish(event),
                    ])
                    .await?;
                Ok(Response::ok(
                    &json!({ "document_type": "cpf", "document_masked": mask_cpf(&document) }),
                ))
            }
            ("POST", ["v1", "identity", "sessions"]) => {
                #[derive(Deserialize)]
                struct SignIn {
                    #[serde(alias = "document")]
                    login: String,
                    password: String,
                    client: String,
                }
                let b: SignIn = req.json()?;
                let Some(account) = s.account_by_login(&b.login).await? else {
                    return Err(problem("invalid-credentials"));
                };
                if !password_ok(&account, &b.password) {
                    return Err(problem("invalid-credentials"));
                }
                s.session(&account, now, b.client == "web").await
            }
            ("POST", ["v1", "identity", "sessions", "refresh"]) => {
                #[derive(Deserialize, Default)]
                struct Body {
                    #[serde(default)]
                    refresh_token: Option<String>,
                }
                let body: Body = req.json().unwrap_or_default();
                let web = body.refresh_token.is_none();
                let token = body
                    .refresh_token
                    .or_else(|| cookie(req, REFRESH_COOKIE))
                    .ok_or_else(|| problem("session-expired"))?;
                let (rt, _) = s
                    .store
                    .load::<Value>(&format!("RT#{token}"), "META")
                    .await?
                    .ok_or_else(|| problem("session-expired"))?;
                let account_id = rt["account_id"].as_str().unwrap_or_default().to_owned();
                if let Some((rev, _)) = s
                    .store
                    .load::<Value>(&format!("REVOKED#{account_id}"), "META")
                    .await?
                    && rt["issued"].as_u64().unwrap_or(0) < rev["epoch"].as_u64().unwrap_or(0)
                {
                    return Err(problem("session-expired"));
                }
                // Rotação: o refresh usado deixa de valer.
                s.store
                    .write(vec![WriteOp::delete(format!("RT#{token}"), "META")])
                    .await?;
                let (account, _) = s.account(&account_id).await?;
                s.session(&account, rt["auth_time"].as_u64().unwrap_or(now), web)
                    .await
            }
            ("POST", ["v1", "identity", "sessions", "step-up"]) => {
                let account_id = auth::user(req)?.to_owned();
                #[derive(Deserialize)]
                struct Body {
                    #[serde(default)]
                    password: Option<String>,
                }
                let b: Body = req.json()?;
                let (account, _) = s.account(&account_id).await?;
                if !password_ok(&account, &b.password.unwrap_or_default()) {
                    return Err(
                        problem("wrong-password").with_field("password", "A senha não confere.")
                    );
                }
                s.session(
                    &account,
                    now,
                    cookie(req, REFRESH_COOKIE).is_some() || is_web(req),
                )
                .await
            }
            ("POST", ["v1", "identity", "me", "contact-confirmations"]) => {
                // A troca de contato pedida em `PATCH /v1/customers/me` passa a valer.
                let account_id = auth::user(req)?.to_owned();
                #[derive(Deserialize)]
                struct Body {
                    code: String,
                }
                let b: Body = req.json()?;
                let (mut account, version) = s.account(&account_id).await?;
                let pending_key = format!("PENDINGCONTACT#{account_id}");
                let Some((pending, _)) = s.store.load::<Value>(&pending_key, "META").await? else {
                    return Err(problem("code-expired"));
                };
                if b.code != VERIFICATION_CODE {
                    return Err(problem("invalid-code").with_field("code", "O código não confere."));
                }
                let value = pending["value"].as_str().map(str::to_owned);
                let old = if pending["channel"] == "email" {
                    account.email_verified = true;
                    std::mem::replace(&mut account.email, value.clone())
                } else {
                    account.phone_verified = true;
                    std::mem::replace(&mut account.phone, value.clone())
                };
                account.pending_contact = None;
                let mut ops = vec![
                    WriteOp::replace(
                        Item::new(format!("ACC#{account_id}"), "META", &account)?,
                        version,
                    ),
                    WriteOp::delete(pending_key, "META"),
                ];
                if let Some(old) = old {
                    ops.push(WriteOp::delete(contact_key(&old), "META"));
                }
                if let Some(new) = value {
                    ops.push(WriteOp::put(Item::new(
                        contact_key(&new),
                        "META",
                        &json!({ "account_id": account_id }),
                    )?));
                }
                s.store.write(ops).await?;
                Ok(Response::no_content())
            }
            ("DELETE", ["v1", "identity", "sessions", "current"]) => {
                if let Some(token) = cookie(req, REFRESH_COOKIE) {
                    s.store
                        .write(vec![WriteOp::delete(format!("RT#{token}"), "META")])
                        .await?;
                }
                Ok(Response::no_content().header("set-cookie", &format!("{REFRESH_COOKIE}=; Path=/v1/identity/sessions; HttpOnly; Secure; SameSite=Strict; Max-Age=0")))
            }
            ("DELETE", ["v1", "identity", "sessions"]) => {
                let account = auth::recent_user(req, now)?.to_owned();
                s.store
                    .write(vec![WriteOp::put(Item::new(
                        format!("REVOKED#{account}"),
                        "META",
                        &json!({ "epoch": now }),
                    )?)])
                    .await?;
                Ok(Response::no_content())
            }
            ("POST", ["v1", "identity", "password-resets"]) => {
                #[derive(Deserialize)]
                struct Body {
                    #[serde(alias = "document")]
                    login: String,
                }
                let b: Body = req.json()?;
                let id = saci_ids::Id::new("rst").to_string();
                let expires = now + 15 * 60;
                if let Some(account) = s.account_by_login(&b.login).await? {
                    s.store
                        .write(vec![WriteOp::put(
                            Item::new(
                                format!("RESET#{id}"),
                                "META",
                                &json!({ "account_id": account.id }),
                            )?
                            .expiring(expires),
                        )])
                        .await?;
                    tracing::info!("sandbox: código de recuperação {VERIFICATION_CODE}");
                }
                // Sempre igual: não revela se a entrada tem conta.
                Ok(Response::json(
                    202,
                    &json!({ "reset_id": id, "expires_at": rfc3339_epoch(expires) }),
                ))
            }
            ("POST", ["v1", "identity", "password-resets", "confirm"]) => {
                #[derive(Deserialize)]
                struct Body {
                    reset_id: String,
                    code: String,
                    new_password: String,
                }
                let b: Body = req.json()?;
                if b.new_password.chars().count() < 8 {
                    return Err(problem("weak-password"));
                }
                let (reset, _) = s
                    .store
                    .load::<Value>(&format!("RESET#{}", b.reset_id), "META")
                    .await?
                    .ok_or_else(|| problem("code-expired"))?;
                if b.code != VERIFICATION_CODE {
                    return Err(problem("invalid-code"));
                }
                let (mut account, version) = s
                    .account(reset["account_id"].as_str().unwrap_or_default())
                    .await?;
                account.password_hash = hash_password(&account.id, &b.new_password);
                s.store
                    .write(vec![
                        WriteOp::replace(
                            Item::new(format!("ACC#{}", account.id), "META", &account)?,
                            version,
                        ),
                        WriteOp::delete(format!("RESET#{}", b.reset_id), "META"),
                    ])
                    .await?;
                Ok(Response::no_content())
            }
            ("PUT", ["v1", "identity", "me", "password"]) => {
                let id = auth::recent_user(req, now)?.to_owned();
                #[derive(Deserialize)]
                struct Body {
                    new_password: String,
                }
                let b: Body = req.json()?;
                if b.new_password.chars().count() < 8 {
                    return Err(problem("weak-password"));
                }
                let (mut account, version) = s.account(&id).await?;
                account.password_hash = hash_password(&account.id, &b.new_password);
                s.store
                    .write(vec![WriteOp::replace(
                        Item::new(format!("ACC#{id}"), "META", &account)?,
                        version,
                    )])
                    .await?;
                Ok(Response::no_content())
            }
            _ => Err(not_found()),
        }
    }
}

/// A senha confere (sal = id da conta; contas antigas do sandbox usavam o documento).
fn password_ok(account: &Account, password: &str) -> bool {
    account.password_hash == hash_password(&account.id, password)
        || account
            .document
            .as_deref()
            .is_some_and(|d| account.password_hash == hash_password(d, password))
}

/// As operações internas do cofre (`identity`).
#[derive(Debug, Clone)]
pub struct IdentityInternal(pub Sandbox);

impl InternalHandler for IdentityInternal {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            let s = &self.0;
            match op {
                "account_profile" => {
                    #[derive(Deserialize)]
                    struct A {
                        account_id: String,
                    }
                    let (a, _) = s.account(&payload::<A>(body)?.account_id).await?;
                    let mut v = a.profile();
                    if a.document.as_deref().is_some_and(|d| d.len() == 11) {
                        v["cpf_masked"] = v["document_masked"].clone();
                    }
                    v["identity_status"] = json!(if a.name.is_some() {
                        "verified"
                    } else {
                        "unverified"
                    });
                    if let Some(p) = a.pending_contact {
                        v["pending_contact"] = p;
                    }
                    reply(&v)
                }
                "lookup_cpf" => {
                    #[derive(Deserialize)]
                    struct C {
                        cpf: String,
                    }
                    let cpf = payload::<C>(body)?.cpf;
                    let account = s.account_by_document(&cpf).await?.map(|a| a.id);
                    reply(&json!({ "fingerprint": fingerprint(&cpf), "account_id": account }))
                }
                "cpf_fingerprint_of" => {
                    #[derive(Deserialize)]
                    struct A {
                        account_id: String,
                    }
                    let (a, _) = s.account(&payload::<A>(body)?.account_id).await?;
                    // Conta sem CPF (#27): sem digital.
                    reply(
                        &json!({ "fingerprint": a.document.as_deref().filter(|d| d.len() == 11).map(fingerprint) }),
                    )
                }
                "put_address" => {
                    #[derive(Deserialize)]
                    struct P {
                        owner_account_id: String,
                        purpose: String,
                        address: Value,
                    }
                    let p: P = payload(body)?;
                    reply(
                        &json!({ "ref": s.put_address(&p.owner_account_id, &p.purpose, p.address).await? }),
                    )
                }
                "open_address" => {
                    #[derive(Deserialize)]
                    struct R {
                        #[serde(rename = "ref")]
                        reference: String,
                    }
                    let r: R = payload(body)?;
                    let (doc, _) = s
                        .store
                        .load::<Value>(&format!("ADDR#{}", r.reference), "META")
                        .await?
                        .ok_or_else(not_found)?;
                    reply(&json!({ "address": doc["address"] }))
                }
                "put_guest_buyer" => {
                    let order = body["order_id"].as_str().unwrap_or_default().to_owned();
                    s.store
                        .write(vec![WriteOp::put(Item::new(
                            format!("GUEST#{order}"),
                            "META",
                            &body,
                        )?)])
                        .await?;
                    reply(&json!({}))
                }
                "request_contact_change" => {
                    let id = body["account_id"].as_str().unwrap_or_default().to_owned();
                    let (mut a, version) = s.account(&id).await?;
                    let new_phone = match body["phone"].as_str() {
                        Some(p) => Some(
                            normalize_phone(p)
                                .ok_or_else(|| invalid("phone", "Confira o celular, com o DDD."))?,
                        ),
                        None => None,
                    };
                    let new_email = body["email"].as_str().map(str::to_lowercase);
                    // Tirar o último contato é recusado.
                    let removes_email = body["email"].is_null() && body.get("email").is_some();
                    let removes_phone = body["phone"].is_null() && body.get("phone").is_some();
                    let email_after = if removes_email { None } else { a.email.clone() };
                    let phone_after = if removes_phone { None } else { a.phone.clone() };
                    if email_after.is_none()
                        && phone_after.is_none()
                        && new_email.is_none()
                        && new_phone.is_none()
                    {
                        return Err(problem("contact-required"));
                    }
                    // Um contato, uma conta.
                    for contact in [new_email.as_deref(), new_phone.as_deref()]
                        .into_iter()
                        .flatten()
                    {
                        if s.account_by_key(&contact_key(contact))
                            .await?
                            .is_some_and(|other| other.id != id)
                        {
                            return Err(problem("contact-unavailable"));
                        }
                    }
                    let mut ops = Vec::new();
                    for (removed, old) in [(removes_email, &a.email), (removes_phone, &a.phone)] {
                        if let (true, Some(old)) = (removed, old) {
                            ops.push(WriteOp::delete(contact_key(old), "META"));
                        }
                    }
                    if removes_email {
                        a.email_verified = false;
                    }
                    if removes_phone {
                        a.phone_verified = false;
                    }
                    a.email = email_after;
                    a.phone = phone_after;
                    if let Some(e) = &new_email {
                        a.pending_contact = Some(
                            json!({ "channel": "email", "masked_destination": mask_email(e) }),
                        );
                        tracing::info!(
                            "sandbox: confirmação de e-mail novo com o código {VERIFICATION_CODE}"
                        );
                    } else if let Some(p) = &new_phone {
                        a.pending_contact = Some(
                            json!({ "channel": "phone", "masked_destination": mask_phone(p) }),
                        );
                    }
                    ops.push(WriteOp::replace(
                        Item::new(format!("ACC#{id}"), "META", &a)?,
                        version,
                    ));
                    // O contato novo só vale com o código (`POST /v1/identity/me/contact-confirmations`).
                    let pending = match (&new_email, &new_phone) {
                        (Some(e), _) => Some(json!({ "channel": "email", "value": e })),
                        (_, Some(p)) => Some(json!({ "channel": "phone", "value": p })),
                        _ => None,
                    };
                    if let Some(pending) = pending {
                        ops.push(WriteOp::put(Item::new(
                            format!("PENDINGCONTACT#{id}"),
                            "META",
                            &pending,
                        )?));
                    }
                    s.store.write(ops).await?;
                    reply(&json!({}))
                }
                "notify" | "send_to_address" => {
                    tracing::info!(template = %body["template"], "sandbox: aviso que sairia por e-mail, WhatsApp ou SMS");
                    reply(&json!({}))
                }
                "default_address" => {
                    let id = body["account_id"].as_str().unwrap_or_default();
                    let r = s
                        .store
                        .load::<Value>(&format!("DEFAULTADDR#{id}"), "META")
                        .await?
                        .map(|(v, _)| v["ref"].clone());
                    reply(&json!({ "ref": r }))
                }
                _ => Err(unknown_op("identity", op)),
            }
        })
    }
}

/// Digital do CPF (HMAC com chave do sandbox), como o cofre faria.
fn fingerprint(cpf: &str) -> String {
    hex::encode(hmac_sha256(b"saci-sandbox-cpf", &[cpf.as_bytes()]))
}
