//! Sandbox de `kyc`, `finance` e `assistant`.
//!
//! **KYC** (o `kyc-gateway` do `saci-identity`, ADR 0010): as conferências
//! respondem na hora e a divergência com a Receita só baixa a nota de fraude.
//! Aqui a nota é gravada na hora, com estas regras:
//!
//! | Entrada | Resultado |
//! | --- | --- |
//! | Nascimento com menos de 18 anos | `underage` (recusa na hora) |
//! | Conta sem CPF | `422 document-required` |
//! | Nascimento em 1º de janeiro de qualquer ano | `ok`, com `birth_date` 0 na nota |
//! | Outro nascimento | `ok`; o nome que a pessoa usa vira o de registro |
//! | CNPJ começando por `00` | `ok`, com `cnpj_active` 0 |
//! | CNPJ começando por `99` | `ok`, com `qsa_name` 0 |
//! | CNPJ começando por `88` | `ok`, com `address` 0 |
//! | Outro CNPJ válido | `ok`, nota cheia |
//!
//! **Finance**: comissão fixa de 12% e repasse 7 dias depois da entrega.
//!
//! **Assistant**: os detalhes de um repasse da Caipora registrado pelo
//! servidor de desenvolvimento (`POST /dev/assistant/handoffs`).

use saci_commerce_kit::clock::parse_date;
use saci_commerce_kit::internal::{InternalHandler, payload, reply, unknown_op};
use saci_commerce_kit::problem::{invalid, problem};
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

impl Kyc {
    /// Grava as dimensões de uma conferência, como o `kyc-gateway` faria depois
    /// da Receita (aqui, na hora).
    async fn record(&self, account: &str, sk: &str, doc: &Value) -> Result<(), Problem> {
        self.0
            .store
            .write(vec![WriteOp::put(Item::new(
                format!("ACC#{account}"),
                sk,
                doc,
            )?)])
            .await?;
        Ok(())
    }
}

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
                        return Err(invalid("birth_date", "Use a data no formato AAAA-MM-DD."));
                    };
                    let today = self.0.clock.today();
                    let mut age = today.year() - birth.year();
                    if (u8::from(today.month()), today.day())
                        < (u8::from(birth.month()), birth.day())
                    {
                        age -= 1;
                    }
                    if birth > today || age > 130 {
                        return Err(invalid("birth_date", "Confira a data de nascimento."));
                    }
                    if age < 18 {
                        return reply(&json!({ "result": "underage" }));
                    }
                    let (mut a, v) = self.0.account(&q.account_id).await?;
                    if a.document.is_none() {
                        return Err(problem("document-required"));
                    }
                    let matches = !(u8::from(birth.month()) == 1 && birth.day() == 1);
                    // Como o cofre: o nome de registro só fica quando CPF e nascimento conferem.
                    if matches && a.name.is_none() {
                        a.name = a.preferred_name.clone();
                        self.0
                            .store
                            .write(vec![WriteOp::replace(
                                Item::new(format!("ACC#{}", a.id), "META", &a)?,
                                v,
                            )])
                            .await?;
                    }
                    let birth_score = if matches { 100 } else { 0 };
                    self.record(
                        &q.account_id,
                        "KYC#IDENTITY",
                        &json!({ "status": "done", "dimensions": { "cpf_active": 100, "birth_date": birth_score, "name": 100 } }),
                    )
                    .await?;
                    reply(&json!({ "result": "ok", "status": "pending" }))
                }
                "check_business" => {
                    #[derive(Deserialize)]
                    struct Q {
                        account_id: String,
                        cnpj: String,
                    }
                    let q: Q = payload(body)?;
                    let flag = |p: &str| if q.cnpj.starts_with(p) { 0 } else { 100 };
                    self.record(
                        &q.account_id,
                        "KYC#BUSINESS",
                        &json!({ "status": "done", "cnpj_masked": mask_cnpj(&q.cnpj), "dimensions": {
                            "cnpj_active": flag("00"), "qsa_name": flag("99"), "address": flag("88") } }),
                    )
                    .await?;
                    reply(
                        &json!({ "result": "ok", "status": "pending", "cnpj_masked": mask_cnpj(&q.cnpj) }),
                    )
                }
                "fraud_score" => {
                    #[derive(Deserialize)]
                    struct Q {
                        account_id: String,
                    }
                    let q: Q = payload(body)?;
                    let pk = format!("ACC#{}", q.account_id);
                    let mut out = json!({ "account_id": q.account_id, "status": "none" });
                    let mut values: Vec<u64> = Vec::new();
                    for (sk, field) in [("KYC#IDENTITY", "identity"), ("KYC#BUSINESS", "business")]
                    {
                        if let Some((doc, _)) = self.0.store.load::<Value>(&pk, sk).await? {
                            if let Some(d) = doc["dimensions"].as_object() {
                                values.extend(d.values().filter_map(Value::as_u64));
                            }
                            out[field] = doc;
                        }
                    }
                    if !values.is_empty() {
                        let n = values.len() as u64;
                        out["status"] = json!("done");
                        out["score"] = json!((values.iter().sum::<u64>() + n / 2) / n);
                    }
                    reply(&out)
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
