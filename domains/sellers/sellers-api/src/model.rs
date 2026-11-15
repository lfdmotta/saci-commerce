//! Tipos das vitrines.
//!
//! Desenho da tabela `saci-<env>-sellers`:
//!
//! | `pk` | `sk` | Documento |
//! | --- | --- | --- |
//! | `STF#<vitrine>` | `META` | [`StorefrontRecord`] |
//! | `STF#<vitrine>` | `MEMBER#<pessoa>` | [`MemberRecord`] (equipe e convites) |
//! | `STF#<vitrine>` | `COUNTS` | [`Counts`] (projeção do painel) |
//! | `URL#<nome>` | `META` | [`UrlRecord`]: dono do endereço, ou redirecionamento por 12 meses |
//! | `ACC#<conta>` | `MEMBERSHIP` | [`Membership`]: a vitrine da conta (uma por conta) |
//! | `ACC#<conta>` | `CHECK#<conferência>` | [`CheckRecord`] (identidade, CNPJ) |
//! | `INV#<conta>` | `<pessoa>` | convite recebido por uma conta |
//! | `PENDING#<digital do CPF>` | `<pessoa>` | convite para um CPF ainda sem conta |
//! | `FONT` | `<ordem>#<id>` | par de letras |
//!
//! Nenhum CPF ou CNPJ inteiro fica aqui: só mascarado e a impressão digital
//! (HMAC) que o cofre de dados pessoais calcula (D9, seção 9.3). O endereço do
//! ateliê fica no cofre; aqui, só a referência opaca (`addr_`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Chaves.
pub mod keys {
    /// Vitrine.
    #[must_use]
    pub fn storefront(id: &str) -> String {
        format!("STF#{id}")
    }
    /// Endereço da vitrine.
    #[must_use]
    pub fn url(name: &str) -> String {
        format!("URL#{name}")
    }
    /// Conta.
    #[must_use]
    pub fn account(id: &str) -> String {
        format!("ACC#{id}")
    }
    /// Convites de uma conta.
    #[must_use]
    pub fn invitations(account: &str) -> String {
        format!("INV#{account}")
    }
    /// Convites para um CPF sem conta.
    #[must_use]
    pub fn pending(fingerprint: &str) -> String {
        format!("PENDING#{fingerprint}")
    }
    /// Documento único.
    pub const META: &str = "META";
    /// Vínculo da conta.
    pub const MEMBERSHIP: &str = "MEMBERSHIP";
    /// Contagens do painel.
    pub const COUNTS: &str = "COUNTS";
    /// Pares de letras.
    pub const FONTS: &str = "FONT";
}

/// Cidade e UF públicas (S-01: rua e número nunca vão na rota pública).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// Cidade.
    pub city: String,
    /// UF.
    pub state: String,
    /// País (ISO alfa-2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
}

/// Paleta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    /// Cor principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    /// Cor de destaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
}

/// A vitrine como fica guardada.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorefrontRecord {
    /// Id (`stf_`).
    pub id: String,
    /// Pessoa responsável.
    pub owner_account_id: String,
    /// Nome da vitrine.
    pub brand_name: String,
    /// Frase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slogan: Option<String>,
    /// Endereço na URL.
    pub url_name: String,
    /// `cpf` ou `cnpj`.
    pub sells_with: String,
    /// `draft` até a primeira peça publicada; depois `published`.
    pub status: String,
    /// Cidade e UF.
    pub location: Location,
    /// Referência ao endereço do ateliê no cofre (`addr_`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business_address_ref: Option<String>,
    /// CNPJ mascarado, quando vende com CNPJ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cnpj_masked: Option<String>,
    /// Paleta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<Palette>,
    /// Par de letras escolhido.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_option_id: Option<String>,
    /// Logotipo (só `cdn.`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
    /// Foto de capa (só `cdn.`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hero_image_url: Option<String>,
    /// Termos da vitrine aceitos.
    #[serde(default)]
    pub accepted_seller_terms_version: Option<String>,
    /// Criação.
    pub created_at: String,
    /// Última mudança.
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Dono de um endereço, ou o destino do redirecionamento.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlRecord {
    /// Vitrine.
    pub storefront_id: String,
    /// Endereço novo, quando este é antigo (redireciona por 12 meses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_to: Option<String>,
}

/// O vínculo de uma conta com uma vitrine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    /// Vitrine.
    pub storefront_id: String,
    /// `owner` ou `member`.
    pub role: String,
    /// Seções liberadas.
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// Uma pessoa da equipe (ou convidada).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRecord {
    /// Id (`tmb_`).
    pub id: String,
    /// Vitrine.
    pub storefront_id: String,
    /// Conta, quando já existe.
    #[serde(default)]
    pub account_id: Option<String>,
    /// CPF mascarado.
    pub cpf_masked: String,
    /// Digital do CPF calculada pelo cofre (para achar o convite).
    #[serde(default)]
    pub cpf_fingerprint: Option<String>,
    /// Nome, só depois do aceite (S-04).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `invited` ou `active`.
    pub status: String,
    /// Seções liberadas.
    pub permissions: Vec<String>,
    /// Quando entrou (ou foi convidada).
    pub added_at: String,
}

impl MemberRecord {
    /// Como sai na API (`TeamMember`).
    #[must_use]
    pub fn public(&self) -> Value {
        let mut v = serde_json::json!({
            "id": self.id, "cpf_masked": self.cpf_masked, "status": self.status,
            "permissions": self.permissions, "added_at": self.added_at,
        });
        if let (Some(name), "active") = (&self.name, self.status.as_str()) {
            v["name"] = Value::from(name.clone());
        }
        v
    }
}

/// Uma conferência de identidade ou de CNPJ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRecord {
    /// Id da conferência.
    pub check_id: String,
    /// `identity` ou `business`.
    pub kind: String,
    /// Nome de registro (identidade).
    #[serde(default)]
    pub official_name: Option<String>,
    /// CNPJ mascarado.
    #[serde(default)]
    pub cnpj_masked: Option<String>,
    /// Razão social.
    #[serde(default)]
    pub legal_name: Option<String>,
    /// Nome fantasia.
    #[serde(default)]
    pub trade_name: Option<String>,
    /// Quando (segundos).
    pub at: u64,
}

/// Contagens do painel (projeção alimentada por eventos; pode atrasar segundos).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// Vendas a fazer.
    pub orders_to_do: u32,
    /// Chamados abertos.
    pub open_tickets: u32,
    /// Anúncios ativos.
    pub active_listings: u32,
}

/// Par de letras (`FontOption`).
pub type FontOption = Value;
