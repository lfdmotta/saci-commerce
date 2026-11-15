//! `media-api`: envio de imagens (proposta 5.5; achado S-14).
//!
//! | Rota | Acesso |
//! | --- | --- |
//! | `POST /v1/media/me/uploads` | `SELLER:anuncios` (fotos) ou `SELLER:vitrine` (logo e capa); 60 por hora por vitrine |
//! | `GET /v1/media/me/uploads/{id}` | O mesmo acesso |
//! | `PUT <upload_url>` | A assinatura da URL (direto no balde de quarentena) |
//!
//! O cliente pede um endereço de envio informando **tipo, tamanho e SHA-256**;
//! os três ficam presos à assinatura, que vale 5 minutos e um só objeto. A
//! imagem cai na **quarentena**; só depois de verificada e reescrita pela
//! esteira (`media-pipeline`) ganha `public_url` no CDN. Até lá, a tela usa a
//! prévia local.
//!
//! Tabela `saci-<env>-media`: `UPL#<envio>`/`META` → [`UploadRecord`].

use std::sync::Arc;

use saci_commerce_kit::auth::{self, Section};
use saci_commerce_kit::clock::{Clock, rfc3339_epoch};
use saci_commerce_kit::events;
use saci_commerce_kit::http::{self, HttpResult, HttpService, Request, Response};
use saci_commerce_kit::internal::{InternalHandler, payload, reply, unknown_op};
use saci_commerce_kit::limits;
use saci_commerce_kit::objects::{UploadGrant, UploadSigner};
use saci_commerce_kit::problem::{invalid, not_found, problem};
use saci_commerce_kit::store::{DocumentStore, Item, StoreExt, WriteOp};
use saci_commerce_kit::{BoxFut, Problem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Validade do endereço de envio.
pub const UPLOAD_URL_SECS: u64 = 5 * 60;
/// Tamanho máximo por finalidade, em bytes.
#[must_use]
pub fn max_bytes(purpose: &str) -> u64 {
    match purpose {
        "logo" => 2 * 1024 * 1024,
        _ => 15 * 1024 * 1024,
    }
}

/// Tipos aceitos (sem SVG: imagem vetorial pode carregar script).
pub const ACCEPTED: [&str; 3] = ["image/jpeg", "image/png", "image/webp"];

/// Um envio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadRecord {
    /// Id (`upl_`).
    pub upload_id: String,
    /// Vitrine dona.
    pub storefront_id: String,
    /// `listing-photo`, `logo` ou `hero`.
    pub purpose: String,
    /// Tipo declarado.
    pub content_type: String,
    /// Tamanho declarado.
    pub content_length: u64,
    /// SHA-256 declarado (base64).
    pub checksum_sha256: String,
    /// Chave na quarentena.
    pub key: String,
    /// `pending`, `scanning`, `ready` ou `rejected`.
    pub status: String,
    /// URL pública, depois de `ready`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    /// Largura publicada.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Altura publicada.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Motivo da recusa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<String>,
    /// Criação.
    pub created_at: String,
}

/// Chave do envio.
#[must_use]
pub fn upload_pk(id: &str) -> String {
    format!("UPL#{id}")
}

/// Dependências do serviço.
#[derive(Debug, Clone)]
pub struct Media {
    /// Tabela.
    pub store: Arc<dyn DocumentStore>,
    /// Assinatura do envio direto (S3 pré-assinado; local, o servidor de desenvolvimento).
    pub signer: Arc<dyn UploadSigner>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
}

#[derive(Debug, Deserialize)]
struct UploadRequest {
    purpose: String,
    content_type: String,
    content_length: u64,
    checksum_sha256: String,
}

impl HttpService for Media {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let id = req.request_id.clone();
            http::finish(self.route(&req).await, &id)
        })
    }
}

/// A vitrine de quem chama, se tem a seção de anúncios ou de vitrine.
fn storefront(req: &Request) -> Result<String, Problem> {
    auth::section(req, Section::Anuncios)
        .or_else(|_| auth::section(req, Section::Vitrine))
        .map(str::to_owned)
}

impl Media {
    async fn route(&self, req: &Request) -> HttpResult {
        match (req.method.as_str(), req.segments().as_slice()) {
            ("POST", ["v1", "media", "me", "uploads"]) => self.create(req).await,
            ("GET", ["v1", "media", "me", "uploads", id]) => {
                let stf = storefront(req)?;
                let record = self.owned(&stf, id).await?;
                let mut out = json!({ "status": record.status });
                if let Some(url) = record.public_url {
                    out["public_url"] = json!(url);
                }
                if let Some(reason) = record.rejection_reason {
                    out["rejection_reason"] = json!(reason);
                }
                Ok(Response::ok(&out))
            }
            _ => Err(not_found()),
        }
    }

    async fn create(&self, req: &Request) -> HttpResult {
        let body: UploadRequest = req.json()?;
        let section = match body.purpose.as_str() {
            "listing-photo" => Section::Anuncios,
            "logo" | "hero" => Section::Vitrine,
            _ => return Err(invalid("purpose", "Use listing-photo, logo ou hero.")),
        };
        let stf = auth::section(req, section)?.to_owned();
        if !ACCEPTED.contains(&body.content_type.as_str()) {
            return Err(problem("unsupported-media-type"));
        }
        if body.content_length == 0 || body.content_length > max_bytes(&body.purpose) {
            return Err(problem("content-too-large"));
        }
        if !valid_sha256_b64(&body.checksum_sha256) {
            return Err(invalid(
                "checksum_sha256",
                "Envie o SHA-256 da imagem em base64.",
            ));
        }
        limits::hit(
            self.store.as_ref(),
            &format!("{stf}:uploads"),
            3_600,
            60,
            self.clock.epoch(),
            "rate-limited",
        )
        .await?;
        let id = saci_ids::Id::new(saci_ids::prefix::UPLOAD).to_string();
        let key = format!("quarantine/{stf}/{id}");
        let grant = UploadGrant {
            key: key.clone(),
            content_type: body.content_type.clone(),
            content_length: body.content_length,
            checksum_sha256: body.checksum_sha256.clone(),
            expires_in: UPLOAD_URL_SECS,
        };
        let signed = self.signer.sign_upload(&grant).await?;
        let record = UploadRecord {
            upload_id: id.clone(),
            storefront_id: stf,
            purpose: body.purpose,
            content_type: body.content_type,
            content_length: body.content_length,
            checksum_sha256: body.checksum_sha256,
            key,
            status: "pending".into(),
            public_url: None,
            width: None,
            height: None,
            rejection_reason: None,
            created_at: self.clock.rfc3339(),
        };
        // Envio que nunca chegou some sozinho em 1 dia.
        let item =
            Item::new(upload_pk(&id), "META", &record)?.expiring(self.clock.epoch() + 86_400);
        self.store.write(vec![WriteOp::create(item)]).await?;
        Ok(Response::json(
            201,
            &json!({
                "upload_id": id, "upload_url": signed.url, "upload_headers": signed.headers,
                "expires_at": rfc3339_epoch(self.clock.epoch() + UPLOAD_URL_SECS),
            }),
        ))
    }

    async fn owned(&self, storefront: &str, id: &str) -> Result<UploadRecord, Problem> {
        match self
            .store
            .load::<UploadRecord>(&upload_pk(id), "META")
            .await?
        {
            Some((r, _)) if r.storefront_id == storefront => Ok(r),
            _ => Err(not_found()),
        }
    }

    /// Registra o resultado da esteira e avisa (`media.upload.ready/rejected.v1`).
    pub async fn finish(&self, mut record: UploadRecord) -> Result<(), Problem> {
        let event = if record.status == "ready" {
            events::event(
                "media",
                "media.upload.ready.v1",
                &record.upload_id,
                &json!({ "upload_id": record.upload_id, "storefront_id": record.storefront_id, "purpose": record.purpose }),
            )?
        } else {
            events::event(
                "media",
                "media.upload.rejected.v1",
                &record.upload_id,
                &json!({ "upload_id": record.upload_id, "storefront_id": record.storefront_id, "purpose": record.purpose,
                         "reason": record.rejection_reason.clone().unwrap_or_else(|| "not-an-image".into()) }),
            )?
        };
        record.content_length = record.content_length.max(1);
        // Pronto fica 1 ano (o anúncio guarda a própria cópia da URL).
        let item = Item::new(upload_pk(&record.upload_id), "META", &record)?
            .expiring(self.clock.epoch() + 365 * 86_400);
        Ok(self
            .store
            .write(vec![WriteOp::put(item), WriteOp::Publish(event)])
            .await?)
    }

    /// Envio pela chave na quarentena (a esteira recebe só a chave).
    pub async fn by_key(&self, key: &str) -> Result<Option<UploadRecord>, Problem> {
        let id = key.rsplit('/').next().unwrap_or_default();
        Ok(self
            .store
            .load::<UploadRecord>(&upload_pk(id), "META")
            .await?
            .map(|(r, _)| r)
            .filter(|r| r.key == key))
    }

    /// Marca o envio como em verificação.
    pub async fn mark_scanning(&self, record: &UploadRecord) -> Result<(), Problem> {
        let mut r = record.clone();
        r.status = "scanning".into();
        let item =
            Item::new(upload_pk(&r.upload_id), "META", &r)?.expiring(self.clock.epoch() + 86_400);
        Ok(self.store.write(vec![WriteOp::put(item)]).await?)
    }
}

impl InternalHandler for Media {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            match op {
                // `catalog` e `sellers`: estado dos envios citados num anúncio ou na vitrine.
                "uploads_for_listing" => {
                    #[derive(Deserialize)]
                    struct Q {
                        storefront_id: String,
                        upload_ids: Vec<String>,
                    }
                    let q: Q = payload(body)?;
                    let mut items = Vec::new();
                    for id in q.upload_ids.iter().take(10) {
                        let item = match self.owned(&q.storefront_id, id).await {
                            Ok(r) => {
                                json!({ "upload_id": id, "status": r.status, "public_url": r.public_url, "width": r.width, "height": r.height })
                            }
                            Err(_) => json!({ "upload_id": id, "status": "missing" }),
                        };
                        items.push(item);
                    }
                    reply(&json!({ "items": items }))
                }
                _ => Err(unknown_op("media", op)),
            }
        })
    }
}

/// SHA-256 em base64 padrão: 44 caracteres terminando em `=`.
#[must_use]
pub fn valid_sha256_b64(value: &str) -> bool {
    value.len() == 44
        && value.ends_with('=')
        && value[..43]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}
