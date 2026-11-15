//! Cliente interno das Lambdas, com o sandbox quando configurado (feature `aws`).
//!
//! Cada alvo vem de `INTERNAL_<ALVO>_FUNCTION`. O valor `sandbox` (só fora de
//! produção) atende o alvo em processo, com o estado do sandbox na tabela
//! `SANDBOX_TABLE`, dividida por todas as funções do ambiente (o cofre de
//! endereços gravado pelo `checkout` é lido pelo `orders`).

use std::sync::Arc;

use saci_commerce_kit::aws::dynamodb::DynamoStore;
use saci_commerce_kit::aws::invoke::LambdaInternal;
use saci_commerce_kit::clock::SystemClock;
use saci_commerce_kit::internal::{InternalClient, LocalInternal};
use saci_commerce_kit::jwt::Es256Key;

use crate::Sandbox;

/// Todos os alvos internos conhecidos.
pub const TARGETS: &[&str] = &[
    "catalog",
    "search",
    "sellers",
    "shipping",
    "media",
    "orders",
    "support",
    "identity",
    "kyc",
    "payments",
    "finance",
    "assistant",
];

/// Monta o cliente interno da função.
pub fn internal_client(config: &aws_config::SdkConfig) -> Result<Arc<dyn InternalClient>, String> {
    let client = LambdaInternal::from_env(config, TARGETS);
    let uses_sandbox = TARGETS.iter().any(|t| {
        std::env::var(format!("INTERNAL_{}_FUNCTION", t.to_ascii_uppercase()))
            .is_ok_and(|v| v == "sandbox")
    });
    if !uses_sandbox {
        return Ok(Arc::new(client));
    }
    let table = saci_commerce_kit::env("SANDBOX_TABLE")?;
    let store = Arc::new(DynamoStore::new(aws_sdk_dynamodb_client(config), table));
    // As Lambdas não emitem sessões: a chave aqui só existe para montar o sandbox.
    let key = Arc::new(Es256Key::generate("unused").map_err(|e| e.to_string())?.0);
    let sandbox = Sandbox::new(
        store,
        Arc::new(SystemClock),
        key,
        "https://auth.local.saci",
        "saci-local",
        "http://localhost:8787",
    )?;
    let local = Arc::new(LocalInternal::new());
    sandbox.register(&local);
    Ok(Arc::new(client.with_sandbox(local)?))
}

fn aws_sdk_dynamodb_client(
    config: &aws_config::SdkConfig,
) -> saci_commerce_kit::aws::dynamodb::Client {
    saci_commerce_kit::aws::dynamodb::Client::new(config)
}
