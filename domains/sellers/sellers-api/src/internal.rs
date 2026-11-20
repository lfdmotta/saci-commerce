//! Operações internas das vitrines (função `sellers-internal`) e o consumidor
//! que mantém o painel.
//!
//! | Operação | Quem chama | Devolve |
//! | --- | --- | --- |
//! | `storefront_profile` | `catalog` | Nome, endereço, cidade e identidade da banca |
//! | `storefront_by_url` | `reviews` | A vitrine de um endereço |
//! | `storefront_summary` | `orders`, `notifications`, `support` | Nome, endereço e responsável |
//! | `storefront_members` | `notifications` | Contas com acesso à vitrine |
//!
//! O consumidor `sellers-workspace` atualiza as contagens do painel (peças,
//! vendas a fazer, chamados abertos), tira a vitrine do rascunho na primeira
//! peça publicada e ativa convites de quem acabou de abrir a conta
//! (`identity.account.verified.v1`).

use saci_commerce_kit::events::{self, ConsumeError, Consumer, Event, data};
use saci_commerce_kit::internal::{self, InternalHandler, payload, reply, unknown_op};
use saci_commerce_kit::problem::not_found;
use saci_commerce_kit::store::{Item, StoreExt, WriteOp};
use saci_commerce_kit::{BoxFut, Problem};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::Sellers;
use crate::model::{Counts, MemberRecord, UrlRecord, keys};

#[derive(Debug, Deserialize)]
struct StorefrontRef {
    storefront_id: String,
}

#[derive(Debug, Deserialize)]
struct UrlRef {
    url_name: String,
}

impl InternalHandler for Sellers {
    fn call<'a>(&'a self, op: &'a str, body: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            match op {
                "storefront_profile" => {
                    let (s, _) = self
                        .storefront(&payload::<StorefrontRef>(body)?.storefront_id)
                        .await?;
                    reply(&json!({
                        "storefront_id": s.id, "brand_name": s.brand_name, "url_name": s.url_name, "slogan": s.slogan,
                        "city": s.location.city, "state": s.location.state, "theme": self.theme(&s).await?,
                    }))
                }
                "storefront_by_url" => {
                    let name = payload::<UrlRef>(body)?.url_name;
                    let (url, _) = self
                        .store
                        .load::<UrlRecord>(&keys::url(&name), keys::META)
                        .await?
                        .ok_or_else(not_found)?;
                    let target = url.redirect_to.unwrap_or(name);
                    let (s, _) = self.storefront(&url.storefront_id).await?;
                    reply(
                        &json!({ "storefront_id": s.id, "brand_name": s.brand_name, "url_name": target }),
                    )
                }
                "storefront_summary" => {
                    let (s, _) = self
                        .storefront(&payload::<StorefrontRef>(body)?.storefront_id)
                        .await?;
                    reply(&json!({
                        "storefront_id": s.id, "brand_name": s.brand_name, "url_name": s.url_name,
                        "owner_account_id": s.owner_account_id, "logo_url": s.logo_url,
                    }))
                }
                "storefront_members" => {
                    let id = payload::<StorefrontRef>(body)?.storefront_id;
                    let (s, _) = self.storefront(&id).await?;
                    let members = self
                        .store
                        .query_all(keys::storefront(&id), "MEMBER#".into())
                        .await?;
                    let mut accounts = vec![
                        json!({ "account_id": s.owner_account_id, "role": "owner", "permissions": [] }),
                    ];
                    for m in members.iter().map(Item::parse::<MemberRecord>) {
                        let m = m?;
                        if let (Some(a), "active") = (m.account_id, m.status.as_str()) {
                            accounts.push(json!({ "account_id": a, "role": "member", "permissions": m.permissions }));
                        }
                    }
                    reply(&json!({ "accounts": accounts }))
                }
                _ => Err(unknown_op("sellers", op)),
            }
        })
    }
}

/// O consumidor do painel.
#[derive(Debug, Clone)]
pub struct WorkspaceProjector(pub Sellers);

#[derive(Debug, Deserialize)]
struct Count {
    count: u32,
}

impl WorkspaceProjector {
    /// Monta a projeção inteira de uma vitrine, perguntando as três contagens
    /// aos donos delas. Usado quando a projeção ainda não existe: vitrines
    /// semeadas por migração (as sementes de `catalog`, `orders` e `support`
    /// não publicam eventos) ou anteriores ao consumidor.
    ///
    /// # Errors
    ///
    /// A chamada interna a um dos domínios falhou.
    pub async fn rebuild(&self, storefront: &str) -> Result<Counts, Problem> {
        for what in ["listings", "orders", "tickets"] {
            self.refresh(storefront, what).await?;
        }
        Ok(self
            .0
            .store
            .load::<Counts>(&keys::storefront(storefront), keys::COUNTS)
            .await?
            .map(|(c, _)| c)
            .unwrap_or_default())
    }

    async fn refresh(&self, storefront: &str, what: &str) -> Result<(), Problem> {
        let s = &self.0;
        let (target, op) = match what {
            "listings" => ("catalog", "active_listings"),
            "orders" => ("orders", "storefront_to_do"),
            _ => ("support", "open_tickets"),
        };
        let count: Count = internal::call(
            s.internal.as_ref(),
            target,
            op,
            &json!({ "storefront_id": storefront }),
        )
        .await?;
        let pk = keys::storefront(storefront);
        let current = s.store.load::<Counts>(&pk, keys::COUNTS).await?;
        let mut counts = current.as_ref().map(|(c, _)| c.clone()).unwrap_or_default();
        match what {
            "listings" => counts.active_listings = count.count,
            "orders" => counts.orders_to_do = count.count,
            _ => counts.open_tickets = count.count,
        }
        let mut ops = vec![WriteOp::put(Item::new(pk, keys::COUNTS, &counts)?)];
        // A vitrine sai do rascunho na primeira peça publicada.
        if what == "listings"
            && count.count > 0
            && let Ok((mut record, version)) = s.storefront(storefront).await
            && record.status == "draft"
        {
            record.status = "published".into();
            ops.push(WriteOp::replace(
                Item::new(keys::storefront(storefront), keys::META, &record)?,
                version,
            ));
            ops.push(WriteOp::Publish(events::event(
                "sellers",
                "sellers.storefront.updated.v1",
                storefront,
                &json!({ "storefront_id": storefront }),
            )?));
        }
        Ok(s.store.write(ops).await?)
    }

    async fn activate_invitations(&self, account: &str) -> Result<(), Problem> {
        let s = &self.0;
        #[derive(Deserialize)]
        struct Fingerprint {
            fingerprint: String,
        }
        let fp: Fingerprint = internal::call(
            s.internal.as_ref(),
            "identity",
            "cpf_fingerprint_of",
            &json!({ "account_id": account }),
        )
        .await?;
        for pending in s
            .store
            .query_all(keys::pending(&fp.fingerprint), String::new())
            .await?
        {
            let inv: Value = pending.parse()?;
            let storefront = inv["storefront_id"].as_str().unwrap_or_default().to_owned();
            let Some((mut m, version)) = s
                .store
                .load::<MemberRecord>(
                    &keys::storefront(&storefront),
                    &format!("MEMBER#{}", pending.sk),
                )
                .await?
            else {
                continue;
            };
            m.account_id = Some(account.to_owned());
            s.store
                .write(vec![
                    WriteOp::replace(Item::new(keys::storefront(&storefront), format!("MEMBER#{}", m.id), &m)?, version),
                    WriteOp::put(Item::new(keys::invitations(account), m.id.clone(), &inv)?),
                    WriteOp::delete(pending.pk.clone(), pending.sk.clone()),
                    WriteOp::Publish(events::event(
                        "sellers",
                        "sellers.team.member_changed.v1",
                        &storefront,
                        &json!({ "storefront_id": storefront, "member_account_id": account, "change": "invited" }),
                    )?),
                ])
                .await?;
        }
        Ok(())
    }
}

impl Consumer for WorkspaceProjector {
    fn name(&self) -> &'static str {
        "sellers-workspace"
    }

    fn accepts(&self) -> &'static [&'static str] {
        &[
            "catalog.listing.published.v1",
            "catalog.listing.paused.v1",
            "catalog.listing.withdrawn.v1",
            "catalog.stock.changed.v1",
            "checkout.order.confirmed.v1",
            "orders.sale.ready.v1",
            "orders.sale.shipped.v1",
            "orders.sale.delivered.v1",
            "orders.return.opened.v1",
            "support.ticket.opened.v1",
            "support.ticket.resolved.v1",
            "identity.account.verified.v1",
        ]
    }

    fn handle<'a>(&'a self, event: &'a Event) -> BoxFut<'a, Result<(), ConsumeError>> {
        Box::pin(async move {
            let t = event.event_type.as_str();
            let result = if t.starts_with("catalog.") {
                self.refresh(&data::<StorefrontRef>(event)?.storefront_id, "listings")
                    .await
            } else if t == "checkout.order.confirmed.v1" {
                #[derive(Deserialize)]
                struct Confirmed {
                    storefront_ids: Vec<String>,
                }
                for id in data::<Confirmed>(event)?.storefront_ids {
                    self.refresh(&id, "orders").await?;
                }
                Ok(())
            } else if t.starts_with("orders.") {
                self.refresh(&data::<StorefrontRef>(event)?.storefront_id, "orders")
                    .await
            } else if t.starts_with("support.") {
                self.refresh(&data::<StorefrontRef>(event)?.storefront_id, "tickets")
                    .await
            } else {
                #[derive(Deserialize)]
                struct Verified {
                    account_id: String,
                }
                self.activate_invitations(&data::<Verified>(event)?.account_id)
                    .await
            };
            result.map_err(ConsumeError::from)
        })
    }
}
