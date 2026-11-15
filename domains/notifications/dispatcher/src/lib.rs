//! `dispatcher` e `push-sender` (proposta 9.6).
//!
//! - **dispatcher** (consumidor de eventos): decide quem avisar e com que
//!   texto, respeita as preferências e enfileira um aviso por aparelho;
//! - **push-sender** (outra Lambda, lendo a fila em lote): envia ao Expo Push
//!   em paralelo e esquece tokens recusados.
//!
//! O aviso não leva dado pessoal: título genérico, corpo curto e um link
//! interno (`saci://...`) que o app abre com a sessão.
//!
//! | Evento | Quem recebe | Link |
//! | --- | --- | --- |
//! | `checkout.order.confirmed.v1` | quem comprou; a equipe com `pedidos` de cada vitrine | compra; vendas |
//! | `orders.sale.shipped.v1`, `orders.sale.delivered.v1` | quem comprou | compra |
//! | `support.ticket.opened.v1` | a equipe com `atendimento` | chamado |
//! | `finance.payout.sent.v1` | a pessoa responsável | financeiro |

use std::fmt;
use std::sync::Arc;

use saci_commerce_kit::events::{ConsumeError, Consumer, Event, data};
use saci_commerce_kit::internal::{self, InternalClient};
use saci_commerce_kit::{BoxFut, Problem};
use saci_notifications_api::Notifications;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Um aviso para um aparelho.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Push {
    /// Conta.
    pub account_id: String,
    /// Instalação.
    pub installation_id: String,
    /// Token do Expo.
    pub token: String,
    /// Título.
    pub title: String,
    /// Corpo.
    pub body: String,
    /// Link interno.
    pub link: String,
}

/// Fila de avisos (SQS na nuvem; em processo no local).
pub trait PushQueue: Send + Sync + fmt::Debug {
    /// Enfileira.
    fn enqueue(&self, pushes: Vec<Push>) -> BoxFut<'_, Result<(), String>>;
}

/// Resultado do envio de um aviso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Entregue ao Expo.
    Sent,
    /// Token não vale mais: esquecer.
    Unregistered,
    /// Falha passageira: tentar de novo.
    Retry,
}

/// Quem envia (Expo Push; *sandbox* que só registra).
pub trait PushSender: Send + Sync + fmt::Debug {
    /// Envia um lote.
    fn send<'a>(&'a self, pushes: &'a [Push]) -> BoxFut<'a, Vec<Delivery>>;
}

/// *Sandbox*: registra o aviso (sem conteúdo pessoal) e não envia nada.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogSender;

impl PushSender for LogSender {
    fn send<'a>(&'a self, pushes: &'a [Push]) -> BoxFut<'a, Vec<Delivery>> {
        Box::pin(async move {
            for p in pushes {
                tracing::info!(title = %p.title, link = %p.link, "aviso push (sandbox)");
            }
            vec![Delivery::Sent; pushes.len()]
        })
    }
}

/// O despachante.
#[derive(Debug, Clone)]
pub struct Dispatcher {
    /// Aparelhos e preferências.
    pub notifications: Notifications,
    /// `orders` e `sellers` (quem avisar).
    pub internal: Arc<dyn InternalClient>,
    /// Fila do `push-sender`.
    pub queue: Arc<dyn PushQueue>,
}

impl Dispatcher {
    async fn pushes_for(
        &self,
        account: &str,
        kind: &str,
        title: &str,
        body: &str,
        link: &str,
    ) -> Result<Vec<Push>, Problem> {
        let prefs = self.notifications.preferences(account).await?;
        let wanted = match kind {
            "messages" => prefs.messages,
            "marketing" => prefs.marketing,
            _ => prefs.orders,
        };
        if !wanted {
            return Ok(Vec::new());
        }
        Ok(self
            .notifications
            .devices(account)
            .await?
            .into_iter()
            .map(|d| Push {
                account_id: account.to_owned(),
                installation_id: d.installation_id,
                token: d.push_token,
                title: title.to_owned(),
                body: body.to_owned(),
                link: link.to_owned(),
            })
            .collect())
    }

    /// Contas da vitrine com a seção (a pessoa responsável sempre).
    async fn team(&self, storefront: &str, section: &str) -> Result<Vec<String>, Problem> {
        let members: Value = internal::call(
            self.internal.as_ref(),
            "sellers",
            "storefront_members",
            &json!({ "storefront_id": storefront }),
        )
        .await?;
        Ok(members["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| {
                m["role"] == "owner"
                    || m["permissions"]
                        .as_array()
                        .is_some_and(|p| p.iter().any(|x| x == section))
            })
            .filter_map(|m| m["account_id"].as_str().map(str::to_owned))
            .collect())
    }

    async fn dispatch(&self, event: &Event) -> Result<(), Problem> {
        let mut pushes = Vec::new();
        match event.event_type.as_str() {
            "checkout.order.confirmed.v1" => {
                #[derive(Deserialize)]
                struct Confirmed {
                    order_id: String,
                    storefront_ids: Vec<String>,
                    #[serde(default)]
                    account_id: Option<String>,
                }
                let e: Confirmed =
                    data(event).map_err(|e| saci_commerce_kit::problem::internal(&e))?;
                if let Some(a) = &e.account_id {
                    pushes.extend(
                        self.pushes_for(
                            a,
                            "orders",
                            "Pedido confirmado",
                            "Seu pagamento foi aprovado.",
                            &format!("saci://compras/{}", e.order_id),
                        )
                        .await?,
                    );
                }
                for s in &e.storefront_ids {
                    for a in self.team(s, "pedidos").await? {
                        pushes.extend(
                            self.pushes_for(
                                &a,
                                "orders",
                                "Venda nova",
                                "Uma peça sua foi vendida.",
                                "saci://painel/pedidos",
                            )
                            .await?,
                        );
                    }
                }
            }
            "orders.sale.shipped.v1" | "orders.sale.delivered.v1" => {
                #[derive(Deserialize)]
                struct Sale {
                    sale_id: String,
                    order_id: String,
                }
                let e: Sale = data(event).map_err(|e| saci_commerce_kit::problem::internal(&e))?;
                let parties: Value = internal::call(
                    self.internal.as_ref(),
                    "orders",
                    "sale_parties",
                    &json!({ "sale_id": e.sale_id }),
                )
                .await?;
                if let Some(a) = parties["account_id"].as_str() {
                    let (title, body) = if event.event_type.ends_with("shipped.v1") {
                        ("Pacote a caminho", "Uma parte da sua compra foi postada.")
                    } else {
                        ("Pacote entregue", "Conte como foi: avalie a vitrine.")
                    };
                    pushes.extend(
                        self.pushes_for(
                            a,
                            "orders",
                            title,
                            body,
                            &format!("saci://compras/{}", e.order_id),
                        )
                        .await?,
                    );
                }
            }
            "support.ticket.opened.v1" => {
                #[derive(Deserialize)]
                struct Ticket {
                    storefront_id: String,
                }
                let e: Ticket =
                    data(event).map_err(|e| saci_commerce_kit::problem::internal(&e))?;
                for a in self.team(&e.storefront_id, "atendimento").await? {
                    pushes.extend(
                        self.pushes_for(
                            &a,
                            "messages",
                            "Chamado novo",
                            "A Caipora passou uma conversa para a sua vitrine.",
                            "saci://painel/atendimento",
                        )
                        .await?,
                    );
                }
            }
            "finance.payout.sent.v1" => {
                #[derive(Deserialize)]
                struct Payout {
                    storefront_id: String,
                }
                let e: Payout =
                    data(event).map_err(|e| saci_commerce_kit::problem::internal(&e))?;
                let summary: Value = internal::call(
                    self.internal.as_ref(),
                    "sellers",
                    "storefront_summary",
                    &json!({ "storefront_id": e.storefront_id }),
                )
                .await?;
                if let Some(owner) = summary["owner_account_id"].as_str() {
                    pushes.extend(
                        self.pushes_for(
                            owner,
                            "orders",
                            "Repasse enviado",
                            "O valor das suas vendas foi enviado.",
                            "saci://painel/financeiro",
                        )
                        .await?,
                    );
                }
            }
            _ => {}
        }
        if !pushes.is_empty() {
            self.queue
                .enqueue(pushes)
                .await
                .map_err(|e| saci_commerce_kit::problem::unavailable(&e))?;
        }
        Ok(())
    }
}

impl Consumer for Dispatcher {
    fn name(&self) -> &'static str {
        "notifications-dispatcher"
    }

    fn accepts(&self) -> &'static [&'static str] {
        &[
            "checkout.order.confirmed.v1",
            "orders.sale.shipped.v1",
            "orders.sale.delivered.v1",
            "support.ticket.opened.v1",
            "finance.payout.sent.v1",
        ]
    }

    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
        Box::pin(async move { self.dispatch(event).await.map_err(ConsumeError::from) })
    }
}

/// O enviador: manda um lote e esquece tokens recusados. Devolve os que devem
/// voltar à fila.
pub async fn send_batch(
    sender: &dyn PushSender,
    notifications: &Notifications,
    pushes: &[Push],
) -> Vec<usize> {
    let results = sender.send(pushes).await;
    let mut retry = Vec::new();
    for (i, (push, result)) in pushes.iter().zip(results).enumerate() {
        match result {
            Delivery::Sent => {}
            Delivery::Unregistered => {
                let _ = notifications
                    .forget_token(&push.account_id, &push.installation_id, &push.token)
                    .await;
            }
            Delivery::Retry => retry.push(i),
        }
    }
    retry
}

/// Fila em processo (servidor de desenvolvimento): envia na hora.
#[derive(Debug, Clone)]
pub struct InlineQueue {
    /// Enviador.
    pub sender: Arc<dyn PushSender>,
    /// Aparelhos.
    pub notifications: Notifications,
}

impl PushQueue for InlineQueue {
    fn enqueue(&self, pushes: Vec<Push>) -> BoxFut<'_, Result<(), String>> {
        Box::pin(async move {
            send_batch(self.sender.as_ref(), &self.notifications, &pushes).await;
            Ok(())
        })
    }
}
