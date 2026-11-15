//! `orders-api`: pedidos e vendas (proposta 5.8, serviço `orders` da 3.1).
//!
//! - **Compras** de quem comprou (com conta ou com guest token), com os
//!   **pacotes por vitrine** (Apêndice B: uma compra de várias vitrines tem um
//!   pacote e um rastreio por vitrine).
//! - **Vendas** de cada vitrine: grupos e contagens do painel, "peça pronta"
//!   (libera etiqueta e endereço) e "postada" (rastreio).
//! - O ciclo avança por eventos: etiqueta comprada, entrega pela transportadora,
//!   repasse feito.
//!
//! Publica `orders.sale.{ready,shipped,delivered}.v1`.

pub mod api;
pub mod internal;
pub mod model;
pub mod seed;

pub use api::Orders;
pub use internal::SaleProgress;
