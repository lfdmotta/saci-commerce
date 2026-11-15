//! Arquivos: guardar e ler objetos, e endereços assinados de curta duração.
//!
//! | Porta | Para | Adaptadores |
//! | --- | --- | --- |
//! | [`ObjectStore`] | Etiquetas (`files.`), imagens em quarentena e publicadas (`cdn.`) | [`MemoryObjects`]; `aws::s3::S3Objects` |
//! | [`UploadSigner`] | Endereço de envio direto (só `PUT`, um objeto, tamanho e hash assinados, S-14) | [`DevUploadSigner`]; `aws::s3::S3Objects` |
//! | [`UrlSigner`] | Leitura privada por 5 minutos (`files.`, S-09) | [`DevUrlSigner`]; `aws::cloudfront::CloudFrontSigner` |

use std::collections::HashMap;
use std::fmt;
use std::sync::RwLock;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::BoxFut;
use crate::cursor::{constant_time_eq, hmac_sha256};

/// Falha de arquivo.
#[derive(Debug, Clone, thiserror::Error)]
#[error("falha de arquivo: {0}")]
pub struct ObjectError(pub String);

impl From<ObjectError> for saci_problem::Problem {
    fn from(e: ObjectError) -> Self {
        crate::problem::unavailable(&e)
    }
}

/// Um objeto lido.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    /// Conteúdo.
    pub bytes: Vec<u8>,
    /// Tipo.
    pub content_type: String,
}

/// Guarda e lê objetos num "balde" (bucket).
pub trait ObjectStore: Send + Sync + fmt::Debug {
    /// Grava.
    fn put<'a>(
        &'a self,
        key: &'a str,
        bytes: Vec<u8>,
        content_type: &'a str,
    ) -> BoxFut<'a, Result<(), ObjectError>>;
    /// Lê (ausente: `None`).
    fn get<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<Option<Object>, ObjectError>>;
    /// Apaga.
    fn delete<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<(), ObjectError>>;
}

/// Pedido de endereço de envio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadGrant {
    /// Chave do objeto na quarentena.
    pub key: String,
    /// Tipo aceito.
    pub content_type: String,
    /// Tamanho exato.
    pub content_length: u64,
    /// SHA-256 em base64.
    pub checksum_sha256: String,
    /// Validade em segundos.
    pub expires_in: u64,
}

/// Endereço assinado de envio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedUpload {
    /// URL do `PUT`.
    pub url: String,
    /// Cabeçalhos que o cliente repete exatamente.
    pub headers: HashMap<String, String>,
}

/// Assina envios diretos.
pub trait UploadSigner: Send + Sync + fmt::Debug {
    /// Endereço de `PUT` para um objeto, com tamanho e hash presos à assinatura.
    fn sign_upload<'a>(
        &'a self,
        grant: &'a UploadGrant,
    ) -> BoxFut<'a, Result<SignedUpload, ObjectError>>;
}

/// Assina leituras privadas.
pub trait UrlSigner: Send + Sync + fmt::Debug {
    /// URL de leitura de `path` válida até `expires_at` (segundos).
    fn sign(&self, path: &str, expires_at: u64) -> Result<String, ObjectError>;
}

/// Objetos em memória.
#[derive(Debug, Default)]
pub struct MemoryObjects {
    objects: RwLock<HashMap<String, Object>>,
}

impl MemoryObjects {
    /// Vazio.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ObjectStore for MemoryObjects {
    fn put<'a>(
        &'a self,
        key: &'a str,
        bytes: Vec<u8>,
        content_type: &'a str,
    ) -> BoxFut<'a, Result<(), ObjectError>> {
        Box::pin(async move {
            self.objects
                .write()
                .map_err(|_| ObjectError("lock".into()))?
                .insert(
                    key.to_owned(),
                    Object {
                        bytes,
                        content_type: content_type.to_owned(),
                    },
                );
            Ok(())
        })
    }

    fn get<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<Option<Object>, ObjectError>> {
        Box::pin(async move {
            Ok(self
                .objects
                .read()
                .map_err(|_| ObjectError("lock".into()))?
                .get(key)
                .cloned())
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<(), ObjectError>> {
        Box::pin(async move {
            self.objects
                .write()
                .map_err(|_| ObjectError("lock".into()))?
                .remove(key);
            Ok(())
        })
    }
}

/// Assinatura HMAC de desenvolvimento: `?expires=<t>&signature=<mac>`.
///
/// O servidor de desenvolvimento confere com [`DevUrlSigner::verify`]. Só fora
/// de produção.
#[derive(Clone)]
pub struct DevUrlSigner {
    base_url: String,
    key: Vec<u8>,
}

impl fmt::Debug for DevUrlSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DevUrlSigner({})", self.base_url)
    }
}

impl DevUrlSigner {
    /// `base_url` sem barra final (ex.: `http://localhost:8787/dev/files`).
    #[must_use]
    pub fn new(base_url: &str, key: &[u8]) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            key: key.to_vec(),
        }
    }

    /// O mesmo assinador, com um segmento a mais na base (`.../dev` → `.../dev/files`).
    #[must_use]
    pub fn with_prefix(self, prefix: &str) -> Self {
        Self {
            base_url: format!("{}/{}", self.base_url, prefix.trim_matches('/')),
            key: self.key,
        }
    }

    /// Base das URLs.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn mac(&self, path: &str, expires_at: u64) -> String {
        URL_SAFE_NO_PAD.encode(
            &hmac_sha256(
                &self.key,
                &[path.as_bytes(), b"\0", expires_at.to_string().as_bytes()],
            )[..18],
        )
    }

    /// Confere a assinatura e a validade.
    #[must_use]
    pub fn verify(&self, path: &str, expires_at: u64, signature: &str, now: u64) -> bool {
        now <= expires_at
            && constant_time_eq(self.mac(path, expires_at).as_bytes(), signature.as_bytes())
    }
}

impl UrlSigner for DevUrlSigner {
    fn sign(&self, path: &str, expires_at: u64) -> Result<String, ObjectError> {
        let path = path.trim_start_matches('/');
        Ok(format!(
            "{}/{path}?expires={expires_at}&signature={}",
            self.base_url,
            self.mac(path, expires_at)
        ))
    }
}

/// Envio direto de desenvolvimento: `PUT` no servidor local, com os mesmos
/// cabeçalhos presos que o S3 exigiria.
#[derive(Debug, Clone)]
pub struct DevUploadSigner(pub DevUrlSigner);

impl UploadSigner for DevUploadSigner {
    fn sign_upload<'a>(
        &'a self,
        grant: &'a UploadGrant,
    ) -> BoxFut<'a, Result<SignedUpload, ObjectError>> {
        Box::pin(async move {
            let expires = crate::clock::Clock::epoch(&crate::clock::SystemClock) + grant.expires_in;
            let url = self.0.sign(&grant.key, expires)?;
            let headers = HashMap::from([
                ("Content-Type".to_owned(), grant.content_type.clone()),
                (
                    "x-amz-checksum-sha256".to_owned(),
                    grant.checksum_sha256.clone(),
                ),
            ]);
            Ok(SignedUpload { url, headers })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_signer_roundtrip() {
        let s = DevUrlSigner::new("http://localhost:8787/dev/files/", b"k");
        let url = s.sign("/labels/sal_1.pdf", 100).unwrap_or_default();
        assert!(url.starts_with(
            "http://localhost:8787/dev/files/labels/sal_1.pdf?expires=100&signature="
        ));
        let sig = url.rsplit("signature=").next().unwrap_or_default();
        assert!(s.verify("labels/sal_1.pdf", 100, sig, 99));
        assert!(!s.verify("labels/sal_1.pdf", 100, sig, 101));
        assert!(!s.verify("labels/sal_2.pdf", 100, sig, 99));
    }
}
