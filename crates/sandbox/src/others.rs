//! Sandbox de `kyc`, `finance` e `assistant`.
//!
//! **KYC** (conferência na Receita pelo `kyc-gateway`):
//!
//! | Entrada | Resultado |
//! | --- | --- |
//! | Nascimento com menos de 18 anos | `underage` |
//! | Nascimento em 1º de janeiro de qualquer ano | `mismatch` (para testar a recusa) |
//! | Outro nascimento | `ok`, com o nome de registro da conta |
//! | CNPJ começando por `00` | `inactive` |
//! | CNPJ começando por `99` | `not_partner` |
//! | Outro CNPJ válido | `ok` |
//!
//! **Finance**: comissão fixa de 12% e repasse 7 dias depois da entrega.
//!
//! **Assistant**: os detalhes de um repasse da Caipora registrado pelo
//! servidor de desenvolvimento (`POST /dev/assistant/handoffs`).

use saci_commerce_kit::clock::parse_date;
use saci_commerce_kit::internal::{InternalHandler, payload, reply, unknown_op};
use saci_commerce_kit::store::{Item, StoreExt, WriteOp};
use saci_commerce_kit::text::mask_cnpj;
use saci_commerce_kit::types::Money;
use saci_commerce_kit::{BoxFut, Problem};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Sandbox;

/// KYC.
#[derive(Debug, Clone)]
pub struct Kyc(pub Sandbox);

impl InternalHandler for Kyc {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            match op {
                "check_identity" => {
                    #[derive(Deserialize)]
                    struct Q {
                        account_id: String,
                        birth_date: String,
                    }
                    let q: Q = payload(body)?;
                    let Some(birth) = parse_date(&q.birth_date) else {
                        return reply(&json!({ "result": "mismatch" }));
                    };
                    let today = self.0.clock.today();
                    let mut age = today.year() - birth.year();
                    if (u8::from(today.month()), today.day())
                        < (u8::from(birth.month()), birth.day())
                    {
                        age -= 1;
                    }
                    if age < 18 {
                        return reply(&json!({ "result": "underage" }));
                    }
                    if u8::from(birth.month()) == 1 && birth.day() == 1 {
                        return reply(&json!({ "result": "mismatch" }));
                    }
                    let name = self
                        .0
                        .account(&q.account_id)
                        .await
                        .ok()
                        .and_then(|(a, _)| a.name)
                        .unwrap_or_else(|| "Pessoa de Exemplo da Silva".into());
                    // Guarda o nome de registro na conta, como o cofre faria depois da conferência.
                    if let Ok((mut a, v)) = self.0.account(&q.account_id).await
                        && a.name.is_none()
                    {
                        a.name = Some(name.clone());
                        self.0
                            .store
                            .write(vec![WriteOp::replace(
                                Item::new(format!("ACC#{}", a.id), "META", &a)?,
                                v,
                            )])
                            .await?;
                    }
                    reply(&json!({ "result": "ok", "official_name": name }))
                }
                "check_business" => {
                    #[derive(Deserialize)]
                    struct Q {
                        cnpj: String,
                    }
                    let q: Q = payload(body)?;
                    let result = if q.cnpj.starts_with("00") {
                        "inactive"
                    } else if q.cnpj.starts_with("99") {
                        "not_partner"
                    } else {
                        "ok"
                    };
                    reply(
                        &json!({ "result": result, "cnpj_masked": mask_cnpj(&q.cnpj), "legal_name": "Ateliê de Exemplo Artesanato Ltda", "trade_name": "Ateliê de Exemplo" }),
                    )
                }
                _ => Err(unknown_op("kyc", op)),
            }
        })
    }
}

/// Finance.
#[derive(Debug, Clone, Copy)]
pub struct Finance;

impl InternalHandler for Finance {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            match op {
                "sale_payment" => {
                    let total = body["items_total"]["amount"].as_i64().unwrap_or(0);
                    let fee = (total * 12 + 50) / 100;
                    reply(
                        &json!({ "fee": Money::brl(fee), "net": Money::brl(total - fee), "payout_date": Value::Null }),
                    )
                }
                _ => Err(unknown_op("finance", op)),
            }
        })
    }
}

/// Assistant.
#[derive(Debug, Clone)]
pub struct Assistant(pub Sandbox);

impl Sandbox {
    /// Registra os detalhes de um repasse (servidor de desenvolvimento).
    pub async fn register_handoff(
        &self,
        conversation_id: &str,
        details: &Value,
    ) -> Result<(), Problem> {
        Ok(self
            .store
            .write(vec![WriteOp::put(Item::new(
                format!("HANDOFF#{conversation_id}"),
                "META",
                details,
            )?)])
            .await?)
    }
}

impl InternalHandler for Assistant {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            match op {
                "handoff_details" => {
                    let id = body["conversation_id"].as_str().unwrap_or_default();
                    let details = self.0.store.load::<Value>(&format!("HANDOFF#{id}"), "META").await?.map_or_else(|| {
                        json!({ "subject": "Conversa da Caipora", "summary": "A pessoa pediu para falar com a vitrine.",
                                "customer": { "first_name": "Cliente" }, "messages": [] })
                    }, |(v, _)| v);
                    reply(&details)
                }
                _ => Err(unknown_op("assistant", op)),
            }
        })
    }
}
