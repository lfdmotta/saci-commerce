//! Comum dos serviços do `saci-commerce`.
//!
//! Tudo aqui é **independente de nuvem**: os serviços escrevem regras e rotas
//! contra estas portas e tipos, e o mesmo código roda numa Lambda, no servidor
//! de desenvolvimento (`local-env/dev-server`) ou num teste.
//!
//! | Módulo | Para |
//! | --- | --- |
//! | [`http`] | Requisição e resposta sem framework, roteamento por padrão de fatia, JSON e cabeçalhos |
//! | [`problem`] | Catálogo de códigos de recusa de `saci-contracts` (RFC 9457) |
//! | [`auth`] | Níveis de acesso da proposta (4.2) sobre o contexto do autorizador |
//! | [`store`] | Porta de armazenamento de documentos (`pk`/`sk`) com escrita condicional, transação e *outbox* |
//! | [`events`] | Eventos CloudEvents, consumidores e deduplicação |
//! | [`grants`] | Papel e seções de vitrine que o autorizador lê (tabela `authz`) |
//! | [`internal`] | Chamadas síncronas entre serviços (invocação direta em nuvem, em processo no local) |
//! | [`idempotency`] | `Idempotency-Key` sobre a porta de armazenamento |
//! | [`cursor`] | Cursor de página opaco e assinado |
//! | [`jwt`] | Tokens ES256 (guest token, sessões de desenvolvimento) |
//! | [`limits`] | Limites de negócio por janela (tentativas, convites) |
//! | [`objects`] | Arquivos: guardar, envio direto assinado e leitura assinada de 5 minutos |
//! | [`text`] | Busca sem acento, moderação de anúncio, máscaras |
//! | [`types`] | `Money`, `Image`, `Region`, `Uf` e as listas da API |
//! | [`clock`] | Relógio injetável |
//!
//! Com a feature `aws`, o módulo `aws` traz os adaptadores: DynamoDB, Lambda
//! (HTTP do API Gateway, SQS, invocação), SSM e EventBridge.

pub mod auth;
pub mod clock;
pub mod cursor;
pub mod events;
pub mod grants;
pub mod http;
pub mod idempotency;
pub mod internal;
pub mod jwt;
pub mod limits;
pub mod objects;
pub mod problem;
pub mod store;
pub mod text;
pub mod types;

#[cfg(feature = "aws")]
pub mod aws;

pub use saci_problem::Problem;

/// Futuro em caixa, para portas usadas como objeto (`Arc<dyn ...>`).
pub type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Lê uma variável de ambiente obrigatória, com mensagem clara.
pub fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("variável de ambiente {name} ausente"))
}

/// Lê uma variável de ambiente opcional, com padrão.
pub fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}
