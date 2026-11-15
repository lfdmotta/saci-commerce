//! Limites de negócio por janela de tempo (seção 6.4), guardados na tabela do
//! próprio serviço: "5 conferências de identidade por conta em 24 horas", "20
//! convites por vitrine por dia".
//!
//! Os limites por IP e por rota ficam no WAF da borda; estes são os que só o
//! serviço sabe contar (por conta, por vitrine).

use saci_problem::Problem;
use serde::{Deserialize, Serialize};

use crate::problem::problem;
use crate::store::{DocumentStore, Expect, Item, StoreError, WriteOp};

#[derive(Debug, Serialize, Deserialize)]
struct Counter {
    count: u32,
}

/// Conta uma tentativa em `scope` na janela de `window_secs` que contém `now`.
/// Passou de `max`: devolve a recusa `code` (ex.: `attempts-exceeded`).
pub async fn hit(
    store: &dyn DocumentStore,
    scope: &str,
    window_secs: u64,
    max: u32,
    now: u64,
    code: &str,
) -> Result<(), Problem> {
    let window = now / window_secs.max(1);
    let pk = format!("LIMIT#{scope}");
    let sk = window.to_string();
    for _ in 0..5 {
        let current = store.get(&pk, &sk).await?;
        let (count, expect, version) = match &current {
            Some(item) => (
                item.parse::<Counter>()?.count,
                Expect::Version(item.version),
                item.version + 1,
            ),
            None => (0, Expect::Absent, 1),
        };
        if count >= max {
            return Err(problem(code));
        }
        let item = Item::new(pk.clone(), sk.clone(), &Counter { count: count + 1 })?
            .with_version(version)
            .expiring((window + 1) * window_secs);
        match store.write(vec![WriteOp::Put { item, expect }]).await {
            Ok(()) => return Ok(()),
            Err(StoreError::Conflict) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(problem("rate-limited"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use std::sync::Arc;

    #[tokio::test]
    async fn counts_per_window() -> Result<(), Problem> {
        let store = MemoryStore::new().with_clock(Arc::new(crate::clock::FixedClock::at(100)));
        for _ in 0..3 {
            hit(
                &store,
                "acc_1:identity",
                86_400,
                3,
                100,
                "attempts-exceeded",
            )
            .await?;
        }
        let refused = hit(
            &store,
            "acc_1:identity",
            86_400,
            3,
            100,
            "attempts-exceeded",
        )
        .await;
        assert_eq!(refused.map_err(|p| p.code), Err("attempts-exceeded".into()));
        hit(
            &store,
            "acc_2:identity",
            86_400,
            3,
            100,
            "attempts-exceeded",
        )
        .await?;
        Ok(())
    }
}
