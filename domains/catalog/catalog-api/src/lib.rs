//! `catalog-api`: o dono dos anúncios (proposta 5.4, serviço `catalog` da 3.1).
//!
//! - **Escrita** do artesão (`/v1/catalog/me/listings*`), com moderação do texto
//!   (S-07), fotos só depois de verificadas (S-14), `Idempotency-Key` no anúncio
//!   novo e `If-Match` na edição.
//! - **Leitura por id** pública (`/v1/catalog/listings/{id}`) e atributos.
//! - **Estoque com escrita condicional** para o `checkout` (operações internas
//!   `reserve`, `commit`, `release`): dois compradores simultâneos não levam a
//!   mesma peça única.
//! - **Perfis do diretório** de artesãos, sincronizados com o `sellers` por evento.
//!
//! Publica `catalog.listing.{published,updated,paused,withdrawn}.v1` e
//! `catalog.stock.changed.v1` pelo *outbox*.
//!
//! O crate não depende de nuvem: recebe as portas prontas ([`api::Catalog`]).

pub mod api;
pub mod consumer;
pub mod internal;
pub mod model;
pub mod seed;

pub use api::Catalog;
