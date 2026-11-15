//! Pedidos e vendas.
//!
//! Um **pedido** é a compra inteira; cada vitrine do pedido tem a sua
//! **venda** (um pacote). Tabela `saci-<env>-orders`:
//!
//! | `pk` | `sk` | Documento |
//! | --- | --- | --- |
//! | `ORD#<pedido>` | `META` | [`OrderRecord`] |
//! | `SAL#<venda>` | `META` | [`SaleRecord`] |
//! | `ACC#<conta>` | `ORD#<data>#<pedido>` | índice das compras da conta |
//! | `STF#<vitrine>` | `SAL#<venda>` | [`SaleIndex`]: situação e prazo, para grupos e contagens do painel |
//! | `LST#<anúncio>` | `SAL#<venda>` | venda em aberto com a peça (impede retirar o anúncio) |
//! | `SEQ` | `ORDER` | número legível do próximo pedido |
//!
//! A proposta (5.8, 3.1) prevê Aurora PostgreSQL para relatórios; o ADR 0006
//! explica por que o começo usa a mesma porta de documentos e quando trocar.
//! Valores das compras são C2 aqui; comissão, líquido e repasse são do
//! `finance` (9.9).

use saci_commerce_kit::types::Money;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Chaves.
pub mod keys {
    /// Pedido.
    #[must_use]
    pub fn order(id: &str) -> String {
        format!("ORD#{id}")
    }
    /// Venda.
    #[must_use]
    pub fn sale(id: &str) -> String {
        format!("SAL#{id}")
    }
    /// Compras de uma conta.
    #[must_use]
    pub fn account(id: &str) -> String {
        format!("ACC#{id}")
    }
    /// Vendas de uma vitrine.
    #[must_use]
    pub fn storefront(id: &str) -> String {
        format!("STF#{id}")
    }
    /// Vendas em aberto de um anúncio.
    #[must_use]
    pub fn listing(id: &str) -> String {
        format!("LST#{id}")
    }
    /// Documento único.
    pub const META: &str = "META";
}

/// Prazo em dias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Days {
    /// Mínimo.
    pub min: u32,
    /// Máximo.
    pub max: u32,
}

/// Uma linha do pedido.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderLine {
    /// Anúncio.
    pub listing_id: String,
    /// Nome da peça na hora da compra.
    pub title: String,
    /// Vitrine.
    pub storefront_id: String,
    /// Nome da vitrine.
    pub storefront_name: String,
    /// Quantidade.
    pub quantity: u32,
    /// Preço unitário do catálogo na hora da compra.
    pub unit_price: Money,
    /// Capa.
    #[serde(default)]
    pub photo_url: Option<String>,
    /// Dias de produção.
    #[serde(default)]
    pub production_days: u32,
}

/// O pedido guardado.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderRecord {
    /// Id (`ord_`).
    pub id: String,
    /// Número legível (só leitura humana; nunca dá acesso).
    pub number: String,
    /// Conta, quando a compra foi com conta.
    #[serde(default)]
    pub account_id: Option<String>,
    /// `awaiting-payment`, `paid` ou `cancelled` (o resto vem das vendas).
    pub status: String,
    /// Linhas.
    pub lines: Vec<OrderLine>,
    /// Soma das peças.
    pub subtotal: Money,
    /// Soma dos fretes.
    pub shipping: Money,
    /// Total.
    pub total: Money,
    /// Prazo de entrega (o maior entre as vitrines).
    pub delivery_days: Days,
    /// Referência do endereço de entrega no cofre.
    #[serde(default)]
    pub address_ref: Option<String>,
    /// "Cidade, UF".
    pub destination: String,
    /// `pix`, `credito` ou `debito`.
    pub payment_method: String,
    /// Vendas (uma por vitrine).
    pub sale_ids: Vec<String>,
    /// Criação.
    pub created_at: String,
    /// Última mudança.
    pub updated_at: String,
}

/// Situação de uma venda. `pending_payment` e `cancelled` são internas: a
/// vitrine só vê a venda depois do pagamento.
pub mod sale_status {
    /// Esperando o pagamento (invisível para a vitrine).
    pub const PENDING_PAYMENT: &str = "pending_payment";
    /// Em produção.
    pub const IN_PRODUCTION: &str = "in_production";
    /// Peça pronta, etiqueta e endereço liberados.
    pub const AWAITING_SHIPMENT: &str = "awaiting_shipment";
    /// Postada.
    pub const SHIPPED: &str = "shipped";
    /// Entregue.
    pub const DELIVERED: &str = "delivered";
    /// Repasse feito.
    pub const INVOICED: &str = "invoiced";
    /// Devolução aberta.
    pub const RETURN_OPENED: &str = "return_opened";
    /// Devolvida.
    pub const RETURNED: &str = "returned";
    /// Cancelada (pagamento recusado ou Pix vencido).
    pub const CANCELLED: &str = "cancelled";
}

/// Grupo do painel de uma situação (`utils/sellerOrders.ts` da interface).
#[must_use]
pub fn group_of(status: &str) -> Option<&'static str> {
    use sale_status::{
        AWAITING_SHIPMENT, DELIVERED, IN_PRODUCTION, INVOICED, RETURN_OPENED, RETURNED, SHIPPED,
    };
    match status {
        IN_PRODUCTION | AWAITING_SHIPMENT => Some("a-fazer"),
        SHIPPED => Some("a-caminho"),
        DELIVERED | INVOICED => Some("concluidos"),
        RETURN_OPENED | RETURNED => Some("devolucoes"),
        _ => None,
    }
}

/// Uma peça da venda.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaleItem {
    /// Anúncio.
    pub listing_id: String,
    /// Nome.
    pub title: String,
    /// Foto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo_url: Option<String>,
    /// Quantidade.
    pub quantity: u32,
    /// Preço unitário.
    pub unit_price: Money,
    /// Personalização combinada.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customization: Option<Value>,
}

/// Frete da venda.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaleShipping {
    /// Serviço.
    pub service: String,
    /// Preço.
    pub price: Money,
    /// Prazo da transportadora.
    pub days: Days,
    /// Etiqueta comprada.
    #[serde(default)]
    pub label_available: bool,
    /// Rastreio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_code: Option<String>,
    /// Postagem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipped_at: Option<String>,
    /// Entrega.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    /// Previsão.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_delivery: Option<String>,
}

/// A venda guardada.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaleRecord {
    /// Id (`sal_`).
    pub id: String,
    /// Pedido.
    pub order_id: String,
    /// Número legível.
    pub number: String,
    /// Vitrine.
    pub storefront_id: String,
    /// Nome da vitrine.
    pub storefront_name: String,
    /// Conta de quem comprou (para a avaliação e a notificação).
    #[serde(default)]
    pub account_id: Option<String>,
    /// Situação.
    pub status: String,
    /// Quando o pedido entrou.
    pub placed_at: String,
    /// Última mudança.
    pub updated_at: String,
    /// O mínimo de quem comprou (seção 5.8): primeiro nome, cidade, UF.
    pub buyer: Value,
    /// Peças.
    pub items: Vec<SaleItem>,
    /// Prazo de postagem.
    pub deadline: Value,
    /// Frete.
    pub shipping: SaleShipping,
    /// Referência do endereço de entrega no cofre.
    #[serde(default)]
    pub address_ref: Option<String>,
    /// Endereço de exemplo das sementes (só fora de produção; nunca dado real).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_fixture: Option<Value>,
    /// Nota fiscal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoice: Option<Value>,
    /// Devolução.
    #[serde(default, rename = "return", skip_serializing_if = "Option::is_none")]
    pub return_info: Option<Value>,
    /// Chamado ligado.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_protocol: Option<String>,
    /// Histórico.
    pub history: Vec<Value>,
}

impl SaleRecord {
    /// Soma das peças.
    #[must_use]
    pub fn items_total(&self) -> Money {
        Money::brl(
            self.items
                .iter()
                .map(|i| i.unit_price.amount * i64::from(i.quantity))
                .sum(),
        )
    }
}

/// Índice do painel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaleIndex {
    /// Venda.
    pub sale_id: String,
    /// Situação.
    pub status: String,
    /// Prazo de postagem (ordem do grupo "a fazer").
    pub ship_by: String,
    /// Última mudança (ordem dos outros grupos).
    pub updated_at: String,
}

impl From<&SaleRecord> for SaleIndex {
    fn from(s: &SaleRecord) -> Self {
        Self {
            sale_id: s.id.clone(),
            status: s.status.clone(),
            ship_by: s.deadline["ship_by"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            updated_at: s.updated_at.clone(),
        }
    }
}
