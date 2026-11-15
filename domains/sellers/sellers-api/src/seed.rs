//! Migrações de dados das vitrines.
//!
//! | Migração | O que faz | Ambientes |
//! | --- | --- | --- |
//! | `0001-launch-fonts` | Pares de letras da personalização | todos |
//! | `0002-launch-storefronts` | Vitrines de lançamento, endereços (no cofre), responsáveis e a equipe de exemplo | `local`, `dev`, `staging` |

use saci_commerce_kit::Problem;
use saci_commerce_kit::grants::Grant;
use saci_commerce_kit::internal;
use saci_commerce_kit::store::{Item, WriteOp};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::Sellers;
use crate::model::{
    Location, MemberRecord, Membership, Palette, StorefrontRecord, UrlRecord, keys,
};

const LAUNCH: &str = include_str!("../migrations/0001-launch-storefronts.json");

#[derive(Debug, Deserialize)]
struct Launch {
    font_options: Vec<Value>,
    storefronts: Vec<SeedStorefront>,
    team: Vec<MemberRecord>,
}

#[derive(Debug, Deserialize)]
struct SeedStorefront {
    id: String,
    owner_account_id: String,
    brand_name: String,
    #[serde(default)]
    slogan: Option<String>,
    url_name: String,
    sells_with: String,
    status: String,
    location: Location,
    business_address: Value,
    #[serde(default)]
    palette: Option<Palette>,
    #[serde(default)]
    font_option_id: Option<String>,
    #[serde(default)]
    logo_url: Option<String>,
    #[serde(default)]
    hero_image_url: Option<String>,
    created_at: String,
}

/// Aplica as migrações pendentes.
pub async fn migrate(sellers: &Sellers, allow_fixtures: bool) -> Result<Vec<String>, Problem> {
    let data: Launch =
        serde_json::from_str(LAUNCH).map_err(|e| saci_commerce_kit::problem::internal(&e))?;
    let mut applied = Vec::new();
    if sellers
        .store
        .get("MIGRATION", "0001-launch-fonts")
        .await?
        .is_none()
    {
        let mut ops: Vec<WriteOp> = data
            .font_options
            .iter()
            .enumerate()
            .map(|(i, f)| {
                Item::new(
                    keys::FONTS,
                    format!("{:03}#{}", i + 1, f["id"].as_str().unwrap_or_default()),
                    f,
                )
                .map(WriteOp::put)
            })
            .collect::<Result<_, _>>()?;
        ops.push(WriteOp::create(Item::new(
            "MIGRATION",
            "0001-launch-fonts",
            &json!({ "at": sellers.clock.rfc3339() }),
        )?));
        sellers.store.write(ops).await?;
        applied.push("0001-launch-fonts".to_owned());
    }
    if !allow_fixtures
        || sellers
            .store
            .get("MIGRATION", "0002-launch-storefronts")
            .await?
            .is_some()
    {
        return Ok(applied);
    }
    for s in data.storefronts {
        #[derive(Deserialize)]
        struct Ref {
            #[serde(rename = "ref")]
            reference: String,
        }
        let address: Ref = internal::call(
            sellers.internal.as_ref(),
            "identity",
            "put_address",
            &json!({ "owner_account_id": s.owner_account_id, "purpose": "business", "address": s.business_address }),
        )
        .await?;
        let record = StorefrontRecord {
            id: s.id.clone(),
            owner_account_id: s.owner_account_id.clone(),
            brand_name: s.brand_name,
            slogan: s.slogan,
            url_name: s.url_name.clone(),
            sells_with: s.sells_with,
            status: s.status,
            location: s.location,
            business_address_ref: Some(address.reference),
            cnpj_masked: None,
            palette: s.palette,
            font_option_id: s.font_option_id,
            logo_url: s.logo_url,
            hero_image_url: s.hero_image_url,
            accepted_seller_terms_version: Some("2026-01".into()),
            created_at: s.created_at,
            updated_at: None,
        };
        let membership = Membership {
            storefront_id: s.id.clone(),
            role: "owner".into(),
            permissions: Vec::new(),
        };
        sellers
            .store
            .write(vec![
                WriteOp::put(Item::new(keys::storefront(&s.id), keys::META, &record)?),
                WriteOp::put(Item::new(
                    keys::url(&s.url_name),
                    keys::META,
                    &UrlRecord {
                        storefront_id: s.id.clone(),
                        redirect_to: None,
                    },
                )?),
                WriteOp::put(Item::new(
                    keys::account(&s.owner_account_id),
                    keys::MEMBERSHIP,
                    &membership,
                )?),
            ])
            .await?;
        sellers
            .grants
            .put(
                &s.owner_account_id,
                &Grant {
                    storefront_id: s.id,
                    role: "owner".into(),
                    sections: Vec::new(),
                },
            )
            .await?;
    }
    for m in data.team {
        let mut ops = vec![WriteOp::put(Item::new(
            keys::storefront(&m.storefront_id),
            format!("MEMBER#{}", m.id),
            &m,
        )?)];
        if let (Some(account), "active") = (&m.account_id, m.status.as_str()) {
            let membership = Membership {
                storefront_id: m.storefront_id.clone(),
                role: "member".into(),
                permissions: m.permissions.clone(),
            };
            ops.push(WriteOp::put(Item::new(
                keys::account(account),
                keys::MEMBERSHIP,
                &membership,
            )?));
            sellers
                .grants
                .put(
                    account,
                    &Grant {
                        storefront_id: m.storefront_id.clone(),
                        role: "member".into(),
                        sections: m.permissions.clone(),
                    },
                )
                .await?;
        }
        sellers.store.write(ops).await?;
    }
    sellers
        .store
        .write(vec![WriteOp::create(Item::new(
            "MIGRATION",
            "0002-launch-storefronts",
            &json!({ "at": sellers.clock.rfc3339() }),
        )?)])
        .await?;
    applied.push("0002-launch-storefronts".to_owned());
    Ok(applied)
}

/// Ids das vitrines de lançamento.
pub fn launch_storefront_ids() -> Vec<String> {
    serde_json::from_str::<Launch>(LAUNCH)
        .map(|d| d.storefronts.into_iter().map(|s| s.id).collect())
        .unwrap_or_default()
}
