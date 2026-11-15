//! `sellers-api`: as vitrines (proposta 5.3, serviço `sellers` da 3.1).
//!
//! - Vitrine pública com só cidade e UF (S-01) e redirecionamento do endereço
//!   antigo por 12 meses (S-21).
//! - Abertura com as **referências** das conferências feitas (identidade e CNPJ
//!   pelo `kyc-gateway`), nunca com nascimento ou CNPJ reenviados; 5 tentativas
//!   de identidade por conta em 24 horas (S-05).
//! - Identidade visual com imagens só do CDN (S-22) e edição com `If-Match`.
//! - Equipe com convite sem nome e aceite (S-04, D5), limite diário de convites,
//!   e o papel gravado na tabela `authz` da plataforma, que o autorizador lê.
//! - O painel (`/me/workspace`) com contagens projetadas por eventos.
//!
//! Publica `sellers.storefront.{opened,updated}.v1` e `sellers.team.member_changed.v1`.

pub mod api;
pub mod internal;
pub mod model;
pub mod seed;

pub use api::Sellers;
pub use internal::WorkspaceProjector;
