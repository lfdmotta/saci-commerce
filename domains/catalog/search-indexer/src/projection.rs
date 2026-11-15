//! A projeção de busca (tabela `saci-<env>-catalog-search`).
//!
//! | `pk` | `sk` | Documento |
//! | --- | --- | --- |
//! | `LISTING` | `<anúncio>` | [`SearchDoc`]: o anúncio público pronto, mais o que a busca filtra |
//! | `ARTISAN` | `<vitrine>` | [`DirectoryEntry`]: a banca do diretório, já montada |
//! | `CATEGORY` | `<ordem>#<slug>` | categoria |
//!
//! A projeção é só de dado público (C0) e pode ser refeita do zero a partir do
//! catálogo (`rebuild`). ADR 0005 explica por que ela fica numa tabela e é lida
//! em memória, em vez de num OpenSearch, no começo.

use saci_catalog_api::model::{Artisan, Listing};
use saci_commerce_kit::types::{Image, Money};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Partições.
pub mod keys {
    /// Anúncios públicos.
    pub const LISTING: &str = "LISTING";
    /// Bancas do diretório.
    pub const ARTISAN: &str = "ARTISAN";
    /// Categorias.
    pub const CATEGORY: &str = "CATEGORY";
}

/// Um anúncio público na projeção.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchDoc {
    /// O anúncio como sai na API (com `sponsored`).
    pub listing: Listing,
    /// Espaços da feira.
    pub placements: Vec<String>,
    /// Ordem de curadoria (relevância padrão).
    pub seq: u64,
    /// Ordem na vitrine.
    pub rank: u32,
    /// Publicação (RFC 3339), para "novidades".
    pub published_at: String,
    /// Texto da busca da feira, sem acento: nome, categoria, artesão, cidade, matérias, etiquetas.
    pub text: String,
    /// Texto da busca na vitrine: nome, prateleira, categoria, matérias, etiquetas.
    pub store_text: String,
    /// Matérias normalizadas (filtro `material`).
    pub materials_norm: Vec<String>,
    /// Aceita personalização (banca "aceita encomenda").
    pub customizable: bool,
}

/// Uma peça na frente da banca.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayPiece {
    /// Anúncio.
    pub id: String,
    /// Nome.
    pub title: String,
    /// Capa.
    pub image: Image,
    /// Preço.
    pub price: Money,
}

/// Uma banca do diretório (`DirectoryStorefront`) e o que o filtro usa.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    /// O artesão.
    pub artisan: Artisan,
    /// Frase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slogan: Option<String>,
    /// Identidade da banca (`StorefrontTheme`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Value>,
    /// Até 3 peças na frente.
    pub display: Vec<DisplayPiece>,
    /// Quantas peças publicadas.
    pub listing_count: u32,
    /// Menor preço.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_from: Option<Money>,
    /// Aceita encomenda.
    pub takes_commissions: bool,
    /// Texto da busca, sem acento.
    pub text: String,
    /// Categorias das peças.
    pub category_slugs: Vec<String>,
}

impl DirectoryEntry {
    /// A banca como sai na API (sem os campos de filtro).
    #[must_use]
    pub fn public(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            o.remove("text");
            o.remove("category_slugs");
        }
        v
    }
}
