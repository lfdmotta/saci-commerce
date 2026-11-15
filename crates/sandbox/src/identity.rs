//! Sandbox do `identity`: contas, sessões e o cofre de dados pessoais.
//!
//! Rotas HTTP (contrato `saci-contracts/http/identity.yaml`):
//! `GET /password-policy`, `POST /accounts`, `POST /accounts/from-order`,
//! `POST /verifications/{id}/confirm|resend`, `POST /sessions`,
//! `POST /sessions/refresh`, `POST /sessions/step-up`, `DELETE /sessions/current`,
//! `DELETE /sessions`, `POST /password-resets`, `POST /password-resets/confirm`,
//! `PUT /me/password`.
//!
//! Regras do sandbox: o código de verificação é sempre [`VERIFICATION_CODE`];
//! qualquer conta de exemplo entra com a senha das sementes; a sessão é um JWT
//! ES256 de 10 minutos com refresh rotativo (cookie `HttpOnly` no site, corpo
//! no app), como na proposta (7.6).
//!
//! Tabela (porta de armazenamento do sandbox):
//!
//! | `pk` | `sk` | Documento |
//! | --- | --- | --- |
//! | `ACC#<conta>` | `META` | [`Account`] |
//! | `DOC#<CPF ou CNPJ>` | `META` | conta do documento |
//! | `VER#<id>` | `META` | verificação pendente (cadastro, troca de contato) |
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
use saci_commerce_kit::text::{mask_cnpj, mask_cpf, mask_email, mask_phone};
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
    /// CPF ou CNPJ, só dígitos (fictício).
    pub document: String,
    /// Nome de registro.
    #[serde(default)]
    pub name: Option<String>,
    /// Nome preferido.
    #[serde(default)]
    pub preferred_name: Option<String>,
    /// E-mail.
    #[serde(default)]
    pub email: Option<String>,
    /// Celular.
    #[serde(default)]
    pub phone: Option<String>,
    /// Troca de contato à espera de confirmação.
    #[serde(default)]
    pub pending_contact: Option<Value>,
    /// Hash da senha.
    pub password_hash: String,
    /// Criação.
    pub created_at: String,
}

impl Account {
    /// `AccountProfile`.
    #[must_use]
    pub fn profile(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "document_type": if self.document.len() == 14 { "cnpj" } else { "cpf" },
            "document_masked": if self.document.len() == 14 { mask_cnpj(&self.document) } else { mask_cpf(&self.document) },
            "created_at": self.created_at,
        });
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
        v
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

impl Sandbox {
    /// Carrega as contas de exemplo (`local-env/sandbox/demo-accounts.json`).
    pub async fn seed_accounts(&self) -> Result<(), Problem> {
        let demo: DemoAccounts = serde_json::from_str(include_str!(
            "../../../local-env/sandbox/demo-accounts.json"
        ))
        .map_err(|e| saci_commerce_kit::problem::internal(&e))?;
        for d in demo.accounts {
            let document = cpf_from_base(&d.cpf_base);
            let account = Account {
                id: d.id.clone(),
                document: document.clone(),
                name: Some(d.name),
                preferred_name: d.preferred_name,
                email: d.email,
                phone: d.phone,
                pending_contact: None,
                password_hash: hash_password(&d.id, &demo.password),
                created_at: d.created_at,
            };
            let mut ops = vec![
                WriteOp::put(Item::new(format!("ACC#{}", d.id), "META", &account)?),
                WriteOp::put(Item::new(
                    format!("DOC#{document}"),
                    "META",
                    &json!({ "account_id": d.id }),
                )?),
            ];
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

    async fn account_by_document(&self, document: &str) -> Result<Option<Account>, Problem> {
        match self
            .store
            .load::<Value>(&format!("DOC#{document}"), "META")
            .await?
        {
            Some((v, _)) => Ok(Some(
                self.account(v["account_id"].as_str().unwrap_or_default())
                    .await?
                    .0,
            )),
            None => Ok(None),
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

    async fn start_verification(
        &self,
        purpose: &str,
        data: Value,
        channel: &str,
        destination: &str,
    ) -> Result<Value, Problem> {
        let id = saci_ids::Id::new("ver").to_string();
        let expires = self.clock.epoch() + 15 * 60;
        let mut doc = data;
        doc["purpose"] = json!(purpose);
        doc["attempts"] = json!(0);
        self.store
            .write(vec![WriteOp::put(
                Item::new(format!("VER#{id}"), "META", &doc)?.expiring(expires + 3_600),
            )])
            .await?;
        tracing::info!(
            purpose,
            channel,
            "sandbox: código de verificação {VERIFICATION_CODE}"
        );
        Ok(json!({
            "verification_id": id, "channel": channel,
            "masked_destination": if channel == "email" { mask_email(destination) } else { mask_phone(destination) },
            "expires_at": rfc3339_epoch(expires),
        }))
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
    document: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    phone: Option<String>,
    password: String,
    accepted_terms_version: String,
}

fn valid_document(d: &str) -> bool {
    (d.len() == 11 && saci_mask::is_valid_cpf(d)) || (d.len() == 14 && saci_mask::is_valid_cnpj(d))
}

fn cookie(req: &Request, name: &str) -> Option<String> {
    req.header("cookie")?
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
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
                if !valid_document(&b.document) {
                    return Err(invalid("document", "Confira o CPF ou CNPJ."));
                }
                if b.password.chars().count() < 8 {
                    return Err(problem("weak-password")
                        .with_field("password", "Use ao menos 8 caracteres."));
                }
                if b.accepted_terms_version.is_empty() {
                    return Err(invalid("accepted_terms_version", "Aceite os termos."));
                }
                let (channel, dest) = match (&b.email, &b.phone) {
                    (Some(e), _) => ("email", e.clone()),
                    (None, Some(p)) => ("sms", p.clone()),
                    _ => return Err(problem("contact-required")),
                };
                // Resposta igual com documento novo ou já cadastrado (S-15).
                let existing = s.account_by_document(&b.document).await?.is_some();
                let data = json!({ "document": b.document, "email": b.email, "phone": b.phone,
                    "password_hash": hash_password(&b.document, &b.password), "existing": existing });
                Ok(Response::json(
                    202,
                    &s.start_verification("signup", data, channel, &dest).await?,
                ))
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
                let document = buyer["cpf"].as_str().unwrap_or_default().to_owned();
                let (channel, dest) = buyer["email"].as_str().map_or_else(
                    || {
                        (
                            "sms",
                            buyer["phone"].as_str().unwrap_or_default().to_owned(),
                        )
                    },
                    |e| ("email", e.to_owned()),
                );
                let data = json!({ "document": document, "email": buyer["email"], "phone": buyer["phone"], "name": buyer["name"],
                    "password_hash": hash_password(&document, &b.password),
                    "existing": s.account_by_document(&document).await?.is_some() });
                Ok(Response::json(
                    202,
                    &s.start_verification("signup", data, channel, &dest).await?,
                ))
            }
            ("POST", ["v1", "identity", "verifications", id, "confirm"]) => {
                #[derive(Deserialize)]
                struct Code {
                    code: String,
                }
                let code: Code = req.json()?;
                let (mut ver, version) = s
                    .store
                    .load::<Value>(&format!("VER#{id}"), "META")
                    .await?
                    .ok_or_else(|| problem("code-expired"))?;
                let attempts = ver["attempts"].as_u64().unwrap_or(0);
                if attempts >= 5 {
                    return Err(problem("attempts-exceeded"));
                }
                if code.code != VERIFICATION_CODE || ver["existing"] == true {
                    ver["attempts"] = json!(attempts + 1);
                    s.store
                        .write(vec![WriteOp::replace(
                            Item::new(format!("VER#{id}"), "META", &ver)?,
                            version,
                        )])
                        .await?;
                    return Err(problem("invalid-code").with_field("code", "O código não confere."));
                }
                s.store
                    .write(vec![WriteOp::delete(format!("VER#{id}"), "META")])
                    .await?;
                let document = ver["document"].as_str().unwrap_or_default().to_owned();
                let account = Account {
                    id: saci_ids::Id::new(saci_ids::prefix::ACCOUNT).to_string(),
                    document: document.clone(),
                    name: ver["name"].as_str().map(str::to_owned),
                    preferred_name: None,
                    email: ver["email"].as_str().map(str::to_owned),
                    phone: ver["phone"].as_str().map(str::to_owned),
                    pending_contact: None,
                    password_hash: String::new(),
                    created_at: s.clock.rfc3339(),
                };
                // O hash usou o documento como sal no cadastro; guarda com o id.
                let mut account = account;
                account.password_hash =
                    ver["password_hash"].as_str().unwrap_or_default().to_owned();
                s.store
                    .write(vec![
                        WriteOp::create(Item::new(
                            format!("ACC#{}", account.id),
                            "META",
                            &account,
                        )?),
                        WriteOp::create(Item::new(
                            format!("DOC#{document}"),
                            "META",
                            &json!({ "account_id": account.id, "salt": "document" }),
                        )?),
                    ])
                    .await?;
                s.account_verified(&account).await?;
                s.session(
                    &account,
                    now,
                    req.header("x-client").is_none_or(|c| c.starts_with("web")),
                )
                .await
            }
            ("POST", ["v1", "identity", "verifications", id, "resend"]) => {
                if s.store.get(&format!("VER#{id}"), "META").await?.is_none() {
                    return Err(problem("code-expired"));
                }
                tracing::info!("sandbox: código reenviado {VERIFICATION_CODE}");
                Ok(Response::json(202, &json!({})))
            }
            ("POST", ["v1", "identity", "sessions"]) => {
                #[derive(Deserialize)]
                struct SignIn {
                    document: String,
                    password: String,
                    client: String,
                }
                let b: SignIn = req.json()?;
                let Some(account) = s.account_by_document(&b.document).await? else {
                    return Err(problem("invalid-credentials"));
                };
                let ok = account.password_hash == hash_password(&account.id, &b.password)
                    || account.password_hash == hash_password(&account.document, &b.password);
                if !ok {
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
                let password = b.password.unwrap_or_default();
                if account.password_hash != hash_password(&account.id, &password)
                    && account.password_hash != hash_password(&account.document, &password)
                {
                    return Err(
                        problem("wrong-password").with_field("password", "A senha não confere.")
                    );
                }
                s.session(
                    &account,
                    now,
                    cookie(req, REFRESH_COOKIE).is_some()
                        || req.header("x-client").is_none_or(|c| c.starts_with("web")),
                )
                .await
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
                    document: String,
                }
                let b: Body = req.json()?;
                let id = saci_ids::Id::new("rst").to_string();
                let expires = now + 15 * 60;
                if let Some(account) = s.account_by_document(&b.document).await? {
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
                // Sempre igual: não revela se o documento tem conta.
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
                    v["cpf_masked"] = v["document_masked"].clone();
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
                    reply(&json!({ "fingerprint": fingerprint(&a.document) }))
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
                    // Tirar o último contato é recusado.
                    let email_after = if body["email"].is_null() && body.get("email").is_some() {
                        None
                    } else {
                        a.email.clone()
                    };
                    let phone_after = if body["phone"].is_null() && body.get("phone").is_some() {
                        None
                    } else {
                        a.phone.clone()
                    };
                    if email_after.is_none()
                        && phone_after.is_none()
                        && body["email"].as_str().is_none()
                        && body["phone"].as_str().is_none()
                    {
                        return Err(problem("contact-required"));
                    }
                    a.email = email_after;
                    a.phone = phone_after;
                    if let Some(e) = body["email"].as_str() {
                        a.pending_contact = Some(
                            json!({ "channel": "email", "masked_destination": mask_email(e) }),
                        );
                        tracing::info!(
                            "sandbox: confirmação de e-mail novo com o código {VERIFICATION_CODE}"
                        );
                    } else if let Some(p) = body["phone"].as_str() {
                        a.pending_contact =
                            Some(json!({ "channel": "sms", "masked_destination": mask_phone(p) }));
                    }
                    s.store
                        .write(vec![WriteOp::replace(
                            Item::new(format!("ACC#{id}"), "META", &a)?,
                            version,
                        )])
                        .await?;
                    reply(&json!({}))
                }
                "notify" | "send_to_address" => {
                    tracing::info!(template = %body["template"], "sandbox: aviso que sairia por e-mail ou SMS");
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
