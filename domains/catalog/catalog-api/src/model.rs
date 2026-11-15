//! Tipos do catálogo: o que fica guardado e o que sai na API.
//!
//! Desenho da tabela `saci-<env>-catalog` (um documento por item):
//!
//! | `pk` | `sk` | Documento |
//! | --- | --- | --- |
//! | `LST#<anúncio>` | `META` | [`ListingRecord`] |
//! | `STF#<vitrine>` | `LST#<anúncio>` | [`SellerListing`] (índice do painel) |
//! | `RSV#<pedido>` | `<anúncio>` | [`Reservation`] (peça presa por um checkout) |
//! | `ART#<vitrine>` | `META` | [`ArtisanRecord`] (perfil do diretório) |
//! | `ARTISANS` | `<vitrine>` | lista das vitrines com perfil |
//! | `CAT` | `<ordem>#<slug>` | [`CategoryRecord`] |
//! | `ATTRSPEC` | `<ordem>#<chave>` | [`AttributeSpec`] |
//! | `ATTRSUG` | `<categoria>` | chaves sugeridas da categoria |
//! | `MIGRATION` | `<id>` | migração aplicada |

use saci_commerce_kit::clock::parse_rfc3339;
use saci_commerce_kit::types::{Image, Money};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Chaves da tabela.
pub mod keys {
    /// Partição de um anúncio.
    #[must_use]
    pub fn listing(id: &str) -> String {
        format!("LST#{id}")
    }
    /// Partição dos anúncios de uma vitrine.
    #[must_use]
    pub fn storefront(id: &str) -> String {
        format!("STF#{id}")
    }
    /// Partição das reservas de um pedido.
    #[must_use]
    pub fn reservation(order_id: &str) -> String {
        format!("RSV#{order_id}")
    }
    /// Partição do perfil de um artesão.
    #[must_use]
    pub fn artisan(storefront_id: &str) -> String {
        format!("ART#{storefront_id}")
    }
    /// `sk` de documento único.
    pub const META: &str = "META";
    /// Partição que lista as vitrines com perfil (para refazer a projeção).
    pub const ARTISANS: &str = "ARTISANS";
    /// Partição das categorias.
    pub const CATEGORIES: &str = "CAT";
    /// Partição dos atributos.
    pub const ATTR_SPECS: &str = "ATTRSPEC";
    /// Partição das sugestões de atributo por categoria.
    pub const ATTR_SUGGESTED: &str = "ATTRSUG";
}

/// Situação de um anúncio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingStatus {
    /// Na feira.
    Published,
    /// Em moderação.
    InReview,
    /// Pausado pela vitrine.
    Paused,
    /// Retirado (exclusão lógica).
    Withdrawn,
}

/// Prateleira da vitrine (seção do artesão).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shelf {
    /// Identificador.
    pub slug: String,
    /// Nome.
    pub name: String,
}

/// Valor de atributo: texto ou número.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttributeValue {
    /// Chave (`altura`, ou `livre:<nome>`).
    pub key: String,
    /// Rótulo.
    pub label: String,
    /// Tipo do atributo.
    #[serde(default = "text_kind")]
    pub kind: String,
    /// Valor (texto ou número).
    pub value: Value,
    /// Unidade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

fn text_kind() -> String {
    "text".into()
}

impl AttributeValue {
    /// Valor como texto, com unidade (`32 cm`).
    #[must_use]
    pub fn display(&self) -> String {
        let value = match &self.value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string().replace('.', ","),
            other => other.to_string(),
        };
        match &self.unit {
            Some(u) => format!("{value} {u}"),
            None => value,
        }
    }
}

/// Foto publicada de um anúncio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Photo {
    /// Id estável da foto (`pho_`).
    pub photo_id: String,
    /// URL pública (`cdn.`).
    pub url: String,
    /// Texto alternativo.
    #[serde(default)]
    pub alt: String,
    /// Largura.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Altura.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

/// O anúncio como fica guardado.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListingRecord {
    /// Id (`lst_`).
    pub id: String,
    /// Vitrine dona (`stf_`), que também é o id do artesão na API.
    pub storefront_id: String,
    /// Nome.
    pub title: String,
    /// Descrição.
    pub description: String,
    /// `slug` da categoria.
    pub category: String,
    /// Caminho da categoria para exibição ("Decoração > Vasos").
    #[serde(default)]
    pub category_path: Option<String>,
    /// Preço.
    pub price: Money,
    /// Preço "de" (riscado).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare_at_price: Option<Money>,
    /// Quantidade anunciada.
    pub quantity: u32,
    /// Estoque disponível (já descontadas as reservas).
    pub stock: u32,
    /// Dias de produção (0 = pronta-entrega).
    #[serde(default)]
    pub production_days: u32,
    /// Aceita personalização.
    #[serde(default)]
    pub customizable: bool,
    /// Envia para o exterior.
    #[serde(default)]
    pub ships_abroad: bool,
    /// Atributos, na ordem do artesão.
    #[serde(default)]
    pub attributes: Vec<AttributeValue>,
    /// Fotos, a primeira é a capa.
    #[serde(default)]
    pub photos: Vec<Photo>,
    /// Situação.
    pub status: ListingStatus,
    /// Selos definidos pela curadoria ou calculados na escrita.
    #[serde(default)]
    pub curated_badges: Vec<String>,
    /// Matérias.
    #[serde(default)]
    pub materials: Vec<String>,
    /// Etiquetas de busca.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Prateleira da vitrine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<Shelf>,
    /// Posição na vitrine (ordem "featured").
    #[serde(default)]
    pub rank: u32,
    /// Espaços da feira em que aparece (curadoria ou campanha).
    #[serde(default)]
    pub placements: Vec<String>,
    /// Campanha de patrocínio, quando há.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sponsored: Option<String>,
    /// Ordem global de curadoria (relevância padrão).
    #[serde(default)]
    pub seq: u64,
    /// Criação.
    pub created_at: String,
    /// Última mudança.
    pub updated_at: String,
    /// Publicação (ordem "newest").
    #[serde(default)]
    pub published_at: Option<String>,
}

impl ListingRecord {
    /// Visível na feira (publicado; esgotado continua visível com estoque 0).
    #[must_use]
    pub fn is_public(&self) -> bool {
        self.status == ListingStatus::Published
    }

    /// Situação vista pela vitrine (`sold_out` quando publicado e sem estoque).
    #[must_use]
    pub fn seller_status(&self) -> &'static str {
        match (self.status, self.stock) {
            (ListingStatus::Published, 0) => "sold_out",
            (ListingStatus::Published, _) => "published",
            (ListingStatus::InReview, _) => "in_review",
            (ListingStatus::Paused | ListingStatus::Withdrawn, _) => "paused",
        }
    }

    /// Selos públicos: os guardados, mais `novo` (até 30 dias) e `ultimas`.
    #[must_use]
    pub fn badges(&self, now: u64) -> Vec<String> {
        let mut badges = self.curated_badges.clone();
        let mut add = |b: &str| {
            if !badges.iter().any(|x| x == b) {
                badges.push(b.to_owned());
            }
        };
        let published = self.published_at.as_deref().and_then(parse_rfc3339);
        if published.is_some_and(|p| now.saturating_sub(p) <= 30 * 86_400) {
            add("novo");
        }
        if self.quantity > 3 && (1..=3).contains(&self.stock) {
            add("ultimas");
        }
        badges
    }

    /// Imagens públicas.
    #[must_use]
    pub fn images(&self) -> Vec<Image> {
        self.photos
            .iter()
            .map(|p| Image {
                url: p.url.clone(),
                alt: p.alt.clone(),
                width: p.width,
                height: p.height,
            })
            .collect()
    }

    /// Resumo para o painel.
    #[must_use]
    pub fn summary(&self) -> SellerListing {
        SellerListing {
            id: self.id.clone(),
            title: self.title.clone(),
            cover_url: self.photos.first().map(|p| p.url.clone()),
            price: self.price.clone(),
            status: self.seller_status().to_owned(),
            stock: self.stock,
            updated_at: self.updated_at.clone(),
        }
    }
}

/// Uma peça presa por um checkout em andamento.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    /// Anúncio.
    pub listing_id: String,
    /// Quantidade presa.
    pub quantity: u32,
}

/// Categoria guardada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryRecord {
    /// Id (`cat_<slug>`).
    pub id: String,
    /// Slug.
    pub slug: String,
    /// Nome.
    pub name: String,
    /// Ícone da interface.
    pub icon: String,
    /// Ordem da curadoria.
    pub order: u32,
}

/// Perfil de um artesão no diretório (a vitrine vista pelo catálogo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtisanRecord {
    /// Vitrine (id do artesão na API).
    pub storefront_id: String,
    /// Nome de exibição.
    pub name: String,
    /// Cidade.
    pub city: String,
    /// UF.
    pub state: String,
    /// Região.
    pub region: String,
    /// Nome da vitrine na URL.
    pub storefront_url_name: String,
    /// Ofício.
    #[serde(default)]
    pub craft: String,
    /// Desde.
    #[serde(default)]
    pub since: u32,
    /// Biografia.
    #[serde(default)]
    pub bio: String,
    /// Citação.
    #[serde(default)]
    pub quote: String,
    /// Retrato.
    #[serde(default)]
    pub portrait: Option<Image>,
    /// Frase da vitrine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slogan: Option<String>,
    /// Identidade visual para a banca (`StorefrontTheme`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Value>,
}

impl ArtisanRecord {
    /// O resumo que vai em cada anúncio.
    #[must_use]
    pub fn summary(&self) -> ArtisanSummary {
        ArtisanSummary {
            id: self.storefront_id.clone(),
            name: self.name.clone(),
            city: self.city.clone(),
            state: self.state.clone(),
            region: self.region.clone(),
            storefront_url_name: self.storefront_url_name.clone(),
        }
    }

    /// O perfil público.
    #[must_use]
    pub fn profile(&self) -> Artisan {
        Artisan {
            summary: self.summary(),
            craft: self.craft.clone(),
            since: self.since,
            bio: self.bio.clone(),
            quote: self.quote.clone(),
            portrait: self.portrait.clone().unwrap_or_else(|| Image {
                url: "https://cdn.saci.art.br/padrao/retrato.webp".into(),
                alt: format!("Retrato de {}", self.name),
                width: None,
                height: None,
            }),
        }
    }
}

// --- API (snake_case, como em saci-contracts/http/catalog.yaml) -------------------------

/// `CategoryRef`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryRef {
    /// Id.
    pub id: String,
    /// Slug.
    pub slug: String,
    /// Nome.
    pub name: String,
}

/// `Category`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    /// Id.
    pub id: String,
    /// Slug.
    pub slug: String,
    /// Nome.
    pub name: String,
    /// Ícone.
    pub icon: String,
    /// Quantas peças publicadas.
    pub listing_count: u32,
}

/// `ArtisanSummary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtisanSummary {
    /// Id do artesão (a vitrine).
    pub id: String,
    /// Nome.
    pub name: String,
    /// Cidade.
    pub city: String,
    /// UF.
    pub state: String,
    /// Região.
    pub region: String,
    /// Nome da vitrine na URL.
    pub storefront_url_name: String,
}

/// `Artisan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artisan {
    /// O resumo.
    #[serde(flatten)]
    pub summary: ArtisanSummary,
    /// Ofício.
    pub craft: String,
    /// Desde.
    pub since: u32,
    /// Biografia.
    pub bio: String,
    /// Citação.
    pub quote: String,
    /// Retrato.
    pub portrait: Image,
}

/// Patrocínio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sponsored {
    /// Campanha.
    pub campaign_id: String,
}

/// `Listing`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    /// Id.
    pub id: String,
    /// Nome.
    pub title: String,
    /// Categoria.
    pub category: CategoryRef,
    /// Artesão.
    pub artisan: ArtisanSummary,
    /// Preço.
    pub price: Money,
    /// Preço "de".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compare_at_price: Option<Money>,
    /// Fotos.
    pub images: Vec<Image>,
    /// Selos.
    pub badges: Vec<String>,
    /// Patrocínio (só nos espaços da feira).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sponsored: Option<Sponsored>,
    /// Matérias.
    pub materials: Vec<String>,
    /// Etiquetas.
    pub tags: Vec<String>,
    /// Estoque.
    pub stock: u32,
    /// Prateleira da vitrine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<Shelf>,
}

/// Par nome e valor da ficha.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameValue {
    /// Nome.
    pub name: String,
    /// Valor.
    pub value: String,
}

/// `ListingDetail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListingDetail {
    /// O anúncio.
    #[serde(flatten)]
    pub listing: Listing,
    /// Descrição.
    pub description: String,
    /// Aceita personalização.
    pub customizable: bool,
    /// Caminho da categoria.
    pub category_path: String,
    /// Ficha técnica.
    pub attributes: Vec<NameValue>,
}

/// `SellerListing` (painel).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SellerListing {
    /// Id.
    pub id: String,
    /// Nome.
    pub title: String,
    /// Capa.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    /// Preço.
    pub price: Money,
    /// Situação vista pela vitrine.
    pub status: String,
    /// Estoque.
    pub stock: u32,
    /// Última mudança.
    pub updated_at: String,
}

/// `AttributeSpec`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttributeSpec {
    /// Chave.
    pub key: String,
    /// Rótulo.
    pub label: String,
    /// Tipo.
    pub kind: String,
    /// Opções.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<Value>>,
    /// Unidades.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<Vec<String>>,
    /// Unidade padrão.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_unit: Option<String>,
    /// Ajuda.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// Sugerido para a categoria pedida.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested: Option<bool>,
}

/// Monta o anúncio público.
#[must_use]
pub fn public_listing(
    record: &ListingRecord,
    artisan: &ArtisanRecord,
    category: &CategoryRecord,
    now: u64,
) -> Listing {
    Listing {
        id: record.id.clone(),
        title: record.title.clone(),
        category: CategoryRef {
            id: category.id.clone(),
            slug: category.slug.clone(),
            name: category.name.clone(),
        },
        artisan: artisan.summary(),
        price: record.price.clone(),
        compare_at_price: record.compare_at_price.clone(),
        images: record.images(),
        badges: record.badges(now),
        sponsored: record
            .sponsored
            .clone()
            .map(|campaign_id| Sponsored { campaign_id }),
        materials: record.materials.clone(),
        tags: record.tags.clone(),
        stock: record.stock,
        section: record.section.clone(),
    }
}

/// Monta a ficha pública.
#[must_use]
pub fn listing_detail(
    record: &ListingRecord,
    artisan: &ArtisanRecord,
    category: &CategoryRecord,
    now: u64,
) -> ListingDetail {
    let mut listing = public_listing(record, artisan, category, now);
    // A ficha não é espaço da feira: sem patrocínio.
    listing.sponsored = None;
    ListingDetail {
        listing,
        description: record.description.clone(),
        customizable: record.customizable,
        category_path: record
            .category_path
            .clone()
            .unwrap_or_else(|| category.name.clone()),
        attributes: record
            .attributes
            .iter()
            .map(|a| NameValue {
                name: a.label.clone(),
                value: a.display(),
            })
            .collect(),
    }
}
