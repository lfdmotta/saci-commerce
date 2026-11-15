//! *Sandbox* dos domínios que o `saci-commerce` usa e que vivem em outros
//! repositórios ainda não criados (plano de ação, etapas 2, 3 e 5):
//!
//! | Alvo interno | Repositório real | O que o sandbox faz |
//! | --- | --- | --- |
//! | `identity` | `saci-identity` (cofre, contas, sessões) | Contas de exemplo, cadastro com código fixo, sessões JWT, cofre de endereços em claro |
//! | `kyc` | `saci-identity` (`kyc-gateway`) | Regras fixas de idade, nascimento e CNPJ |
//! | `payments` | `saci-payments` | Tokenização, cobrança por cartão (recusa por final do número) e Pix pago em 20 s |
//! | `finance` | `saci-finance` | Comissão fixa de 12% |
//! | `assistant` | `saci-assistant` | Detalhes de um repasse registrado à mão |
//!
//! Também atende as rotas HTTP de `/v1/identity` e `/v1/payments` que a
//! interface chama, para o teste local ponta a ponta ([`identity::IdentityHttp`],
//! [`payments::PaymentsHttp`]).
//!
//! **Nunca em produção**: [`Sandbox::new`] recusa `SACI_ENVIRONMENT=prod`.
//! Os dados ficam numa porta de armazenamento própria (memória no servidor de
//! desenvolvimento; uma tabela `saci-<env>-sandbox` no LocalStack e em dev),
//! para que todas as funções vejam o mesmo cofre e as mesmas cobranças.

#[cfg(feature = "aws")]
pub mod boot;
pub mod identity;
pub mod others;
pub mod payments;

use std::sync::Arc;

use saci_commerce_kit::clock::Clock;
use saci_commerce_kit::internal::LocalInternal;
use saci_commerce_kit::jwt::Es256Key;
use saci_commerce_kit::store::DocumentStore;

/// Configuração e estado do sandbox.
#[derive(Debug, Clone)]
pub struct Sandbox {
    /// Armazenamento do sandbox.
    pub store: Arc<dyn DocumentStore>,
    /// Relógio.
    pub clock: Arc<dyn Clock>,
    /// Chave que assina as sessões (o autorizador confia nela no local).
    pub session_key: Arc<Es256Key>,
    /// Emissor das sessões (`iss`).
    pub issuer: String,
    /// Audiência (`client_id`).
    pub audience: String,
    /// Base pública do servidor de desenvolvimento (para a URL de tokenização).
    pub public_base_url: String,
}

/// Código de verificação fixo do sandbox (o mesmo do mock da interface).
pub const VERIFICATION_CODE: &str = "123456";

impl Sandbox {
    /// Cria o sandbox. Recusa produção.
    pub fn new(
        store: Arc<dyn DocumentStore>,
        clock: Arc<dyn Clock>,
        session_key: Arc<Es256Key>,
        issuer: &str,
        audience: &str,
        public_base_url: &str,
    ) -> Result<Self, String> {
        if saci_commerce_kit::env_or("SACI_ENVIRONMENT", "local") == "prod" {
            return Err("o sandbox não roda em produção".into());
        }
        Ok(Self {
            store,
            clock,
            session_key,
            issuer: issuer.to_owned(),
            audience: audience.to_owned(),
            public_base_url: public_base_url.trim_end_matches('/').to_owned(),
        })
    }

    /// Registra os alvos internos do sandbox (`identity`, `kyc`, `payments`,
    /// `finance`, `assistant`) num [`LocalInternal`].
    pub fn register(&self, local: &LocalInternal) {
        local.register(
            "identity",
            Arc::new(identity::IdentityInternal(self.clone())),
        );
        local.register("kyc", Arc::new(others::Kyc(self.clone())));
        local.register(
            "payments",
            Arc::new(payments::PaymentsInternal(self.clone())),
        );
        local.register("finance", Arc::new(others::Finance));
        local.register("assistant", Arc::new(others::Assistant(self.clone())));
    }
}

/// Hash de senha do sandbox (SHA-256 com sal). Só para dado fictício: o real
/// fica no Cognito.
#[must_use]
pub fn hash_password(salt: &str, password: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(format!("{salt}:{password}").as_bytes()))
}
