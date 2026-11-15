//! Tipos comuns dos contratos (`saci-contracts/common/schemas.yaml`).

use serde::{Deserialize, Serialize};

/// Valor em centavos (`28000` = R$ 280,00) e moeda ISO 4217.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    /// Centavos.
    pub amount: i64,
    /// Moeda (`BRL`).
    pub currency: String,
}

impl Money {
    /// Reais em centavos.
    #[must_use]
    pub fn brl(amount: i64) -> Self {
        Self {
            amount,
            currency: "BRL".into(),
        }
    }

    /// Zero real.
    #[must_use]
    pub fn zero() -> Self {
        Self::brl(0)
    }

    /// Valor válido: inteiro não negativo e moeda de 3 letras maiúsculas.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.amount >= 0
            && self.currency.len() == 3
            && self.currency.chars().all(|c| c.is_ascii_uppercase())
    }
}

/// Imagem publicada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// URL `https://`.
    pub url: String,
    /// Texto alternativo.
    pub alt: String,
    /// Largura.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Altura.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

/// Uma página `{ items, next_cursor? }` (`next_cursor` ausente na última).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Os itens.
    pub items: Vec<T>,
    /// Cursor opaco da próxima página.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Página única.
    #[must_use]
    pub fn all(items: Vec<T>) -> Self {
        Self {
            items,
            next_cursor: None,
        }
    }
}

/// As cinco regiões, na ordem Norte → Sul usada nos corredores e facetas.
pub const REGIONS: [&str; 5] = ["Norte", "Nordeste", "Centro-Oeste", "Sudeste", "Sul"];

/// Todas as UFs.
pub const UFS: [&str; 27] = [
    "AC", "AL", "AM", "AP", "BA", "CE", "DF", "ES", "GO", "MA", "MG", "MS", "MT", "PA", "PB", "PE",
    "PI", "PR", "RJ", "RN", "RO", "RR", "RS", "SC", "SE", "SP", "TO",
];

/// Região de uma UF.
#[must_use]
pub fn region_of(uf: &str) -> Option<&'static str> {
    Some(match uf {
        "AC" | "AM" | "AP" | "PA" | "RO" | "RR" | "TO" => "Norte",
        "AL" | "BA" | "CE" | "MA" | "PB" | "PE" | "PI" | "RN" | "SE" => "Nordeste",
        "DF" | "GO" | "MS" | "MT" => "Centro-Oeste",
        "ES" | "MG" | "RJ" | "SP" => "Sudeste",
        "PR" | "RS" | "SC" => "Sul",
        _ => return None,
    })
}

/// Posição da região na ordem Norte → Sul (para distância de frete e ordenação).
#[must_use]
pub fn region_index(region: &str) -> usize {
    REGIONS.iter().position(|r| *r == region).unwrap_or(3)
}

/// Selos aceitos.
pub const BADGES: [&str; 5] = ["peca-unica", "sustentavel", "ultimas", "novo", "encomenda"];

/// Seções liberáveis à equipe.
pub const SECTIONS: [&str; 4] = ["anuncios", "vitrine", "pedidos", "atendimento"];

/// Endereço de entrega (`ShippingAddress`), no Brasil ou no exterior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShippingAddress {
    /// Endereço brasileiro.
    Brasil {
        /// Quem recebe.
        recipient: String,
        /// CEP, 8 dígitos.
        postal_code: String,
        /// Logradouro.
        street: String,
        /// Número.
        number: String,
        /// Complemento.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        complement: Option<String>,
        /// Bairro.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        neighborhood: Option<String>,
        /// Cidade.
        city: String,
        /// UF.
        state: String,
    },
    /// Endereço no exterior, formato livre.
    Exterior {
        /// Quem recebe.
        recipient: String,
        /// País.
        country: String,
        /// ISO 3166-1 alfa-2.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        country_code: Option<String>,
        /// Linha 1.
        line1: String,
        /// Linha 2.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line2: Option<String>,
        /// Cidade.
        city: String,
        /// Região.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        region: Option<String>,
        /// Código postal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        postal_code: Option<String>,
    },
}

impl ShippingAddress {
    /// Confere os campos e devolve o primeiro erro (`campo`, mensagem).
    pub fn validate(&self) -> Result<(), (&'static str, &'static str)> {
        let len = |s: &str, max: usize| !s.trim().is_empty() && s.chars().count() <= max;
        match self {
            Self::Brasil {
                recipient,
                postal_code,
                street,
                number,
                city,
                state,
                ..
            } => {
                if !len(recipient, 120) {
                    return Err(("recipient", "Informe quem recebe."));
                }
                if !is_postal_code(postal_code) {
                    return Err(("postal_code", "Use os 8 dígitos do CEP."));
                }
                if !len(street, 200) {
                    return Err(("street", "Informe a rua."));
                }
                if !len(number, 20) {
                    return Err(("number", "Informe o número (ou s/n)."));
                }
                if !len(city, 120) {
                    return Err(("city", "Informe a cidade."));
                }
                if !UFS.contains(&state.as_str()) {
                    return Err(("state", "UF inválida."));
                }
                Ok(())
            }
            Self::Exterior {
                recipient,
                country,
                line1,
                city,
                ..
            } => {
                if !len(recipient, 120) {
                    return Err(("recipient", "Informe quem recebe."));
                }
                if !len(country, 80) {
                    return Err(("country", "Informe o país."));
                }
                if !len(line1, 200) {
                    return Err(("line1", "Informe o endereço."));
                }
                if !len(city, 120) {
                    return Err(("city", "Informe a cidade."));
                }
                Ok(())
            }
        }
    }

    /// "Cidade, UF" (ou "Cidade, País"): o destino que pode aparecer sem endereço.
    #[must_use]
    pub fn place(&self) -> String {
        match self {
            Self::Brasil { city, state, .. } => format!("{city}, {state}"),
            Self::Exterior { city, country, .. } => format!("{city}, {country}"),
        }
    }

    /// CEP (só no Brasil).
    #[must_use]
    pub fn postal_code(&self) -> Option<&str> {
        match self {
            Self::Brasil { postal_code, .. } => Some(postal_code),
            Self::Exterior { .. } => None,
        }
    }

    /// Quem recebe.
    #[must_use]
    pub fn recipient(&self) -> &str {
        match self {
            Self::Brasil { recipient, .. } | Self::Exterior { recipient, .. } => recipient,
        }
    }

    /// Linhas para a etiqueta e para a venda em `awaiting_shipment`.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        match self {
            Self::Brasil {
                street,
                number,
                complement,
                neighborhood,
                city,
                state,
                ..
            } => {
                let mut first = format!("{street}, {number}");
                if let Some(c) = complement.as_deref().filter(|c| !c.is_empty()) {
                    first.push_str(" — ");
                    first.push_str(c);
                }
                let mut lines = vec![first];
                if let Some(n) = neighborhood.as_deref().filter(|n| !n.is_empty()) {
                    lines.push(n.to_owned());
                }
                lines.push(format!("{city} — {state}"));
                lines
            }
            Self::Exterior {
                line1,
                line2,
                city,
                region,
                country,
                ..
            } => {
                let mut lines = vec![line1.clone()];
                if let Some(l) = line2 {
                    lines.push(l.clone());
                }
                lines.push(match region {
                    Some(r) => format!("{city}, {r}"),
                    None => city.clone(),
                });
                lines.push(country.clone());
                lines
            }
        }
    }
}

/// CEP: exatamente 8 dígitos.
#[must_use]
pub fn is_postal_code(value: &str) -> bool {
    value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit())
}

/// Cor `#RRGGBB`.
#[must_use]
pub fn is_hex_color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Nome de vitrine na URL: `^[a-z0-9]+(?:-[a-z0-9]+)*$`, 2 a 60.
#[must_use]
pub fn is_url_name(value: &str) -> bool {
    (2..=60).contains(&value.len())
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_roundtrip_and_validation() {
        let json = serde_json::json!({ "kind": "brasil", "recipient": "Ana", "postal_code": "60160230",
            "street": "Rua A", "number": "10", "city": "Fortaleza", "state": "CE" });
        let a: ShippingAddress =
            serde_json::from_value(json).unwrap_or_else(|_| ShippingAddress::Exterior {
                recipient: String::new(),
                country: String::new(),
                country_code: None,
                line1: String::new(),
                line2: None,
                city: String::new(),
                region: None,
                postal_code: None,
            });
        assert!(a.validate().is_ok());
        assert_eq!(a.place(), "Fortaleza, CE");
        assert_eq!(region_of("CE"), Some("Nordeste"));
        assert!(is_url_name("barro-vivo") && !is_url_name("Barro") && !is_url_name("a--b"));
        assert!(is_hex_color("#8B2E1F") && !is_hex_color("8B2E1F"));
    }
}
