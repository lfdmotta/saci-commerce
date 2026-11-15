//! `notifications-api`: aparelhos e preferências (raiz nova `/v1/notifications`,
//! proposta 9.6; Apêndice B: "Notificações push — Fase 4").
//!
//! | Rota | Acesso |
//! | --- | --- |
//! | `PUT /v1/notifications/me/devices/{installation_id}` | `USER` (`{ platform, push_token }`) |
//! | `DELETE /v1/notifications/me/devices/{installation_id}` | `USER` |
//! | `GET /v1/notifications/me/preferences` | `USER` |
//! | `PUT /v1/notifications/me/preferences` | `USER` |
//!
//! O token do aparelho é C2 e fica só aqui; a notificação não leva dado
//! pessoal (só ids e um link interno que o app abre com a sessão). E-mail e SMS
//! saem pelo cofre, no `saci-identity`.
//!
//! Tabela `saci-<env>-notifications`:
//!
//! | `pk` | `sk` | Documento |
//! | --- | --- | --- |
//! | `ACC#<conta>` | `DEV#<instalação>` | [`Device`] |
//! | `ACC#<conta>` | `PREFS` | [`Preferences`] |
//! | `TOKEN#<digital do token>` | `META` | dono do token (um aparelho, uma conta) |

use std::sync::Arc;

use saci_commerce_kit::auth;
use saci_commerce_kit::clock::Clock;
use saci_commerce_kit::cursor::hmac_sha256;
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::problem::{invalid, not_found};
use saci_commerce_kit::store::{DocumentStore, Item, StoreExt, WriteOp};
use saci_commerce_kit::{BoxFut, Problem};
use serde::{Deserialize, Serialize};

/// Um aparelho.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Id da instalação do app.
    pub installation_id: String,
    /// `ios`, `android` ou `web`.
    pub platform: String,
    /// Token do Expo Push (`ExponentPushToken[...]`).
    pub push_token: String,
    /// Atualização.
    pub updated_at: String,
}

/// Preferências.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    /// Pedidos e vendas.
    pub orders: bool,
    /// Mensagens e chamados.
    pub messages: bool,
    /// Novidades da feira.
    pub marketing: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            orders: true,
            messages: true,
            marketing: false,
        }
    }
}

/// Dependências.
#[derive(Debug, Clone)]
pub struct Notifications {
    /// Tabela.
    pub store: Arc<dyn DocumentStore>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Chave da digital dos tokens.
    pub hash_key: Vec<u8>,
}

fn pk(account: &str) -> String {
    format!("ACC#{account}")
}

impl HttpService for Notifications {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

impl Notifications {
    async fn route(&self, req: &Request) -> HttpResult {
        let account = auth::user(req)?.to_owned();
        match (req.method.as_str(), req.segments().as_slice()) {
            ("PUT", ["v1", "notifications", "me", "devices", id]) => {
                self.put_device(&account, req, id).await
            }
            ("DELETE", ["v1", "notifications", "me", "devices", id]) => {
                let sk = format!("DEV#{id}");
                let (device, _) = self
                    .store
                    .load::<Device>(&pk(&account), &sk)
                    .await?
                    .ok_or_else(not_found)?;
                self.store
                    .write(vec![
                        WriteOp::delete(pk(&account), sk),
                        WriteOp::delete(
                            format!("TOKEN#{}", self.digest(&device.push_token)),
                            "META",
                        ),
                    ])
                    .await?;
                Ok(Response::no_content())
            }
            ("GET", ["v1", "notifications", "me", "preferences"]) => {
                Ok(Response::ok(&self.preferences(&account).await?))
            }
            ("PUT", ["v1", "notifications", "me", "preferences"]) => {
                let prefs: Preferences = req.json()?;
                self.store
                    .write(vec![WriteOp::put(Item::new(
                        pk(&account),
                        "PREFS",
                        &prefs,
                    )?)])
                    .await?;
                Ok(Response::ok(&prefs))
            }
            _ => Err(not_found()),
        }
    }

    fn digest(&self, token: &str) -> String {
        hex::encode(hmac_sha256(&self.hash_key, &[token.as_bytes()]))
    }

    async fn put_device(&self, account: &str, req: &Request, installation_id: &str) -> HttpResult {
        #[derive(Deserialize)]
        struct Body {
            platform: String,
            push_token: String,
        }
        let body: Body = req.json()?;
        if !matches!(body.platform.as_str(), "ios" | "android" | "web") {
            return Err(invalid("platform", "Use ios, android ou web."));
        }
        if installation_id.len() > 100
            || body.push_token.len() > 255
            || !body.push_token.starts_with("ExponentPushToken[")
        {
            return Err(invalid("push_token", "Token de notificação inválido."));
        }
        let digest = self.digest(&body.push_token);
        // Um token, uma conta: se o aparelho trocou de conta, a antiga deixa de receber.
        if let Some((owner, _)) = self
            .store
            .load::<serde_json::Value>(&format!("TOKEN#{digest}"), "META")
            .await?
            && let (Some(prev), Some(dev)) = (
                owner["account_id"].as_str(),
                owner["installation_id"].as_str(),
            )
            && prev != account
        {
            self.store
                .write(vec![WriteOp::delete(pk(prev), format!("DEV#{dev}"))])
                .await?;
        }
        let device = Device {
            installation_id: installation_id.to_owned(),
            platform: body.platform,
            push_token: body.push_token,
            updated_at: self.clock.rfc3339(),
        };
        self.store
            .write(vec![
                WriteOp::put(Item::new(pk(account), format!("DEV#{installation_id}"), &device)?),
                WriteOp::put(Item::new(format!("TOKEN#{digest}"), "META", &serde_json::json!({ "account_id": account, "installation_id": installation_id }))?),
            ])
            .await?;
        Ok(Response::no_content())
    }

    /// Preferências (padrão quando nunca salvas).
    pub async fn preferences(&self, account: &str) -> Result<Preferences, Problem> {
        Ok(self
            .store
            .load::<Preferences>(&pk(account), "PREFS")
            .await?
            .map(|(p, _)| p)
            .unwrap_or_default())
    }

    /// Aparelhos de uma conta.
    pub async fn devices(&self, account: &str) -> Result<Vec<Device>, Problem> {
        let items = self.store.query_all(pk(account), "DEV#".into()).await?;
        Ok(items.iter().map(Item::parse).collect::<Result<_, _>>()?)
    }

    /// Esquece um token recusado pelo Expo (`DeviceNotRegistered`).
    pub async fn forget_token(
        &self,
        account: &str,
        installation_id: &str,
        token: &str,
    ) -> Result<(), Problem> {
        Ok(self
            .store
            .write(vec![
                WriteOp::delete(pk(account), format!("DEV#{installation_id}")),
                WriteOp::delete(format!("TOKEN#{}", self.digest(token)), "META"),
            ])
            .await?)
    }
}
