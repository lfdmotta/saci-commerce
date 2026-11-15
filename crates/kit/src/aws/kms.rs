//! [`TokenSigner`] com chave assimétrica do KMS (ECC NIST P-256): a chave
//! privada nunca sai do KMS. Usado pelo `checkout` para o guest token; a chave
//! pública vai para a tabela `authz` da plataforma como emissor `guest`.

use aws_sdk_kms::Client;
use aws_sdk_kms::primitives::Blob;
use aws_sdk_kms::types::{MessageType, SigningAlgorithmSpec};
use serde_json::Value;

use crate::BoxFut;
use crate::jwt::{JwtError, TokenSigner, assemble, der_to_jose, signing_input};

/// Assinatura por KMS.
#[derive(Debug, Clone)]
pub struct KmsSigner {
    client: Client,
    key_id: String,
    kid: String,
}

impl KmsSigner {
    /// `key_id` = ARN ou alias da chave; `kid` = id publicado no JWKS.
    #[must_use]
    pub fn new(
        config: &aws_config::SdkConfig,
        key_id: impl Into<String>,
        kid: impl Into<String>,
    ) -> Self {
        Self {
            client: Client::new(config),
            key_id: key_id.into(),
            kid: kid.into(),
        }
    }
}

impl TokenSigner for KmsSigner {
    fn sign_claims<'a>(&'a self, claims: &'a Value) -> BoxFut<'a, Result<String, JwtError>> {
        Box::pin(async move {
            let input = signing_input(&self.kid, claims);
            let out = self
                .client
                .sign()
                .key_id(&self.key_id)
                .message(Blob::new(input.as_bytes()))
                .message_type(MessageType::Raw)
                .signing_algorithm(SigningAlgorithmSpec::EcdsaSha256)
                .send()
                .await
                .map_err(|e| {
                    tracing::error!(error = %e.into_service_error(), "falha ao assinar no KMS");
                    JwtError::Key
                })?;
            let der = out.signature.map(Blob::into_inner).ok_or(JwtError::Key)?;
            Ok(assemble(&input, &der_to_jose(&der)?))
        })
    }
}
