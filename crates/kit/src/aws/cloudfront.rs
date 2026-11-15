//! [`UrlSigner`] de URLs assinadas do CloudFront (política "canned"), para
//! `files.saci.art.br` (S-09: etiqueta com endereço só por 5 minutos).
//!
//! O CloudFront confere a assinatura RSA-SHA1 pela chave pública do grupo de
//! chaves da distribuição (`edge/` da plataforma, `files_public_key_pem`). A
//! chave privada fica no Secrets Manager e é lida uma vez no início da função.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use sha1::Sha1;

use crate::objects::{ObjectError, UrlSigner};

/// Assinador de URLs do CloudFront.
pub struct CloudFrontSigner {
    base_url: String,
    key_pair_id: String,
    key: SigningKey<Sha1>,
}

impl std::fmt::Debug for CloudFrontSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CloudFrontSigner({}, {})",
            self.base_url, self.key_pair_id
        )
    }
}

impl CloudFrontSigner {
    /// `base_url` = `https://files.saci.art.br`; `private_key_pem` em PKCS#8.
    pub fn new(
        base_url: &str,
        key_pair_id: &str,
        private_key_pem: &str,
    ) -> Result<Self, ObjectError> {
        let key = rsa::RsaPrivateKey::from_pkcs8_pem(private_key_pem)
            .map_err(|e| ObjectError(format!("chave do CloudFront: {e}")))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            key_pair_id: key_pair_id.to_owned(),
            key: SigningKey::<Sha1>::new(key),
        })
    }
}

/// Base64 com os três caracteres trocados que o CloudFront exige.
fn cf_base64(bytes: &[u8]) -> String {
    STANDARD
        .encode(bytes)
        .replace('+', "-")
        .replace('=', "_")
        .replace('/', "~")
}

impl UrlSigner for CloudFrontSigner {
    fn sign(&self, path: &str, expires_at: u64) -> Result<String, ObjectError> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        let policy = format!(
            r#"{{"Statement":[{{"Resource":"{url}","Condition":{{"DateLessThan":{{"AWS:EpochTime":{expires_at}}}}}}}]}}"#
        );
        let signature = self.key.sign(policy.as_bytes()).to_vec();
        Ok(format!(
            "{url}?Expires={expires_at}&Signature={}&Key-Pair-Id={}",
            cf_base64(&signature),
            self.key_pair_id
        ))
    }
}
