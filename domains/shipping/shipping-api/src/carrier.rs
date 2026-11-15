//! Portas das integrações externas do frete, e as implementações de tabela
//! (*sandbox*) usadas até a transportadora e a base de CEP serem contratadas.
//!
//! | Porta | O que faz | Implementação aqui |
//! | --- | --- | --- |
//! | [`PostalDirectory`] | CEP → cidade e UF | [`TableDirectory`]: CEPs conhecidos e faixas por UF (sem saída para a internet, como a rede da etapa 0) |
//! | [`Carrier`] | Cotação e compra de etiqueta | [`TableCarrier`]: a mesma regra do mock da interface, e uma etiqueta em PDF simples |
//!
//! Uma transportadora real (Correios, Melhor Envio...) entra como outro
//! adaptador destas portas, com a saída liberada no firewall da plataforma.

use std::fmt;

use saci_commerce_kit::BoxFut;
use saci_commerce_kit::types::{Money, region_index, region_of};
use serde::{Deserialize, Serialize};

/// Um CEP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostalCodeInfo {
    /// CEP.
    pub postal_code: String,
    /// Cidade.
    pub city: String,
    /// UF.
    pub state: String,
    /// Bairro.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub district: Option<String>,
    /// Logradouro.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street: Option<String>,
}

/// Base de CEPs.
pub trait PostalDirectory: Send + Sync + fmt::Debug {
    /// Procura um CEP (8 dígitos).
    fn lookup<'a>(&'a self, postal_code: &'a str) -> BoxFut<'a, Option<PostalCodeInfo>>;
}

/// Um pacote a cotar: uma vitrine.
#[derive(Debug, Clone)]
pub struct Parcel {
    /// Vitrine (id do artesão).
    pub artisan_id: String,
    /// Região de origem.
    pub origin_region: String,
    /// Quantas peças.
    pub pieces: u32,
}

/// Uma cotação.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    /// Vitrine.
    pub artisan_id: String,
    /// Serviço.
    pub service: String,
    /// Preço.
    pub price: Money,
    /// Prazo em dias úteis.
    pub days: Days,
}

/// Prazo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Days {
    /// Mínimo.
    pub min: u32,
    /// Máximo.
    pub max: u32,
}

/// O que vai impresso na etiqueta.
#[derive(Debug, Clone)]
pub struct LabelRequest {
    /// Venda.
    pub sale_id: String,
    /// Número legível da venda.
    pub sale_number: String,
    /// Remetente (nome da vitrine, cidade e UF).
    pub sender: Vec<String>,
    /// Destinatário (nome e linhas do endereço).
    pub recipient: Vec<String>,
    /// CEP de destino.
    pub postal_code: String,
    /// Serviço.
    pub service: String,
}

/// Etiqueta comprada.
#[derive(Debug, Clone)]
pub struct Label {
    /// Id da etiqueta na transportadora.
    pub label_id: String,
    /// Código de rastreio já reservado, quando a transportadora dá.
    pub tracking_code: Option<String>,
    /// PDF.
    pub pdf: Vec<u8>,
}

/// Transportadora.
pub trait Carrier: Send + Sync + fmt::Debug {
    /// Cota cada pacote para o destino.
    fn quote<'a>(
        &'a self,
        destination_state: &'a str,
        parcels: &'a [Parcel],
    ) -> BoxFut<'a, Vec<Quote>>;
    /// Compra a etiqueta.
    fn buy_label<'a>(&'a self, request: &'a LabelRequest) -> BoxFut<'a, Result<Label, String>>;
}

/// CEPs conhecidos e faixas por UF (o mesmo conjunto do mock da interface).
#[derive(Debug, Default, Clone, Copy)]
pub struct TableDirectory;

/// CEP, cidade, UF, bairro e rua.
type Known = (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
);

const KNOWN: &[Known] = &[
    ("12530000", "Cunha", "SP", None, None),
    (
        "55002000",
        "Caruaru",
        "PE",
        Some("Nossa Senhora das Dores"),
        None,
    ),
    ("39650000", "Minas Novas", "MG", None, None),
    ("66010000", "Belém", "PA", Some("Campina"), None),
    ("83350000", "Morretes", "PR", None, None),
    ("59300000", "Caicó", "RN", None, None),
    (
        "01310100",
        "São Paulo",
        "SP",
        Some("Bela Vista"),
        Some("Avenida Paulista"),
    ),
    (
        "60175047",
        "Fortaleza",
        "CE",
        Some("Meireles"),
        Some("Rua Professor Dias da Rocha"),
    ),
];

/// Início da faixa (5 primeiros dígitos), UF e cidade de referência.
const RANGES: &[(u32, &str, &str)] = &[
    (1000, "SP", "São Paulo"),
    (20000, "RJ", "Rio de Janeiro"),
    (29000, "ES", "Vitória"),
    (30000, "MG", "Belo Horizonte"),
    (40000, "BA", "Salvador"),
    (49000, "SE", "Aracaju"),
    (50000, "PE", "Recife"),
    (57000, "AL", "Maceió"),
    (58000, "PB", "João Pessoa"),
    (59000, "RN", "Natal"),
    (60000, "CE", "Fortaleza"),
    (64000, "PI", "Teresina"),
    (65000, "MA", "São Luís"),
    (66000, "PA", "Belém"),
    (68900, "AP", "Macapá"),
    (69000, "AM", "Manaus"),
    (69300, "RR", "Boa Vista"),
    (69400, "AM", "Manaus"),
    (69900, "AC", "Rio Branco"),
    (70000, "DF", "Brasília"),
    (72800, "GO", "Goiânia"),
    (73000, "DF", "Brasília"),
    (73700, "GO", "Goiânia"),
    (76800, "RO", "Porto Velho"),
    (77000, "TO", "Palmas"),
    (78000, "MT", "Cuiabá"),
    (79000, "MS", "Campo Grande"),
    (80000, "PR", "Curitiba"),
    (88000, "SC", "Florianópolis"),
    (90000, "RS", "Porto Alegre"),
];

impl PostalDirectory for TableDirectory {
    fn lookup<'a>(&'a self, postal_code: &'a str) -> BoxFut<'a, Option<PostalCodeInfo>> {
        Box::pin(async move {
            if let Some((cep, city, state, district, street)) =
                KNOWN.iter().find(|k| k.0 == postal_code)
            {
                return Some(PostalCodeInfo {
                    postal_code: (*cep).to_owned(),
                    city: (*city).to_owned(),
                    state: (*state).to_owned(),
                    district: district.map(str::to_owned),
                    street: street.map(str::to_owned),
                });
            }
            // CEP "00000000" (e tudo abaixo de 01000) não existe: serve para testar o 404.
            let prefix: u32 = postal_code.get(..5)?.parse().ok()?;
            let (_, state, city) = RANGES.iter().rev().find(|(start, _, _)| prefix >= *start)?;
            Some(PostalCodeInfo {
                postal_code: postal_code.to_owned(),
                city: (*city).to_owned(),
                state: (*state).to_owned(),
                district: None,
                street: None,
            })
        })
    }
}

/// Cotação por distância entre regiões (a regra do mock) e etiqueta em PDF.
#[derive(Debug, Default, Clone, Copy)]
pub struct TableCarrier;

impl Carrier for TableCarrier {
    fn quote<'a>(
        &'a self,
        destination_state: &'a str,
        parcels: &'a [Parcel],
    ) -> BoxFut<'a, Vec<Quote>> {
        Box::pin(async move {
            let destination = region_of(destination_state).unwrap_or("Sudeste");
            parcels
                .iter()
                .map(|p| {
                    let far = u32::try_from(
                        region_index(destination).abs_diff(region_index(&p.origin_region)),
                    )
                    .unwrap_or(4);
                    Quote {
                        artisan_id: p.artisan_id.clone(),
                        service: "Correios PAC".into(),
                        price: Money::brl(
                            1_890
                                + i64::from(far) * 600
                                + i64::from(p.pieces.saturating_sub(1)) * 450,
                        ),
                        days: Days {
                            min: 3 + far * 2,
                            max: 5 + far * 3,
                        },
                    }
                })
                .collect()
        })
    }

    fn buy_label<'a>(&'a self, request: &'a LabelRequest) -> BoxFut<'a, Result<Label, String>> {
        Box::pin(async move {
            let mut lines = vec![
                format!("SACI — {} — venda {}", request.service, request.sale_number),
                String::new(),
                "REMETENTE".into(),
            ];
            lines.extend(request.sender.iter().cloned());
            lines.push(String::new());
            lines.push("DESTINATÁRIO".into());
            lines.extend(request.recipient.iter().cloned());
            lines.push(format!("CEP {}", request.postal_code));
            Ok(Label {
                label_id: format!("lbl_{}", request.sale_id.trim_start_matches("sal_")),
                tracking_code: None,
                pdf: pdf(&lines),
            })
        })
    }
}

/// Um PDF de uma página com texto (Helvetica, WinAnsi). Sem dependência.
#[must_use]
pub fn pdf(lines: &[String]) -> Vec<u8> {
    let escape = |s: &str| -> Vec<u8> {
        s.chars()
            .flat_map(|c| match c {
                '(' | ')' | '\\' => vec![b'\\', c as u8],
                c if (c as u32) < 0x80 => vec![c as u8],
                // Latin-1 cobre os acentos do português (WinAnsiEncoding).
                c if (c as u32) <= 0xFF => vec![c as u32 as u8],
                _ => vec![b'?'],
            })
            .collect()
    };
    let mut content: Vec<u8> = b"BT /F1 12 Tf 40 780 Td 16 TL\n".to_vec();
    for line in lines {
        content.push(b'(');
        content.extend(escape(line));
        content.extend(b") Tj T*\n");
    }
    content.extend(b"ET");
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 595] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        [format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(), content, b"\nendstream".to_vec()].concat(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").into_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .into_bytes(),
    );
    out
}
