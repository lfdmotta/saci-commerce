//! `saci-checkout`: as funções do domínio `checkout` (ADR 0003, ADR 0007).
//!
//! | `SACI_FUNCTION` | Gatilho | O que roda |
//! | --- | --- | --- |
//! | `api` | API Gateway (`POST /v1/checkout` público com conta opcional; `GET` misto) | a saga |
//! | `payments` | Fila de `payments.*` | Pix pago ou vencido, cobrança falha |
//! | `sweeper` | Agendamento (a cada 5 minutos) | compensa sagas paradas |
//! | `relay` | DynamoDB Streams | *outbox* → EventBridge |
//!
//! Variáveis: `TABLE`, `AUTHZ_TABLE` e `TRUSTED_ISSUERS` (conta opcional),
//! `GUEST_ISSUER`, `GUEST_AUDIENCE`, `GUEST_TTL_SECS`, e a chave do guest
//! token: `GUEST_KMS_KEY_ID` (nuvem) ou `GUEST_KEY_PARAM` (PKCS#8 em base64, só local),
//! com `GUEST_KID`. `INTERNAL_<ALVO>_FUNCTION`, `EVENT_BUS`.

use std::sync::Arc;

use base64_free::decode;
use lambda_runtime::Error;
use saci_checkout_saga::{Checkout, PaymentEvents};
use saci_commerce_kit::aws::dynamodb::DynamoStore;
use saci_commerce_kit::aws::kms::KmsSigner;
use saci_commerce_kit::aws::lambda::{
    WithOptionalAuth, init_telemetry, run_consumer, run_http, run_relay, run_scheduled,
};
use saci_commerce_kit::aws::optional_auth::OptionalAuth;
use saci_commerce_kit::aws::{sdk_config, ssm};
use saci_commerce_kit::clock::{Clock, SystemClock};
use saci_commerce_kit::jwt::{Es256Key, TokenSigner};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let function = saci_commerce_kit::env("SACI_FUNCTION")?;
    init_telemetry(&format!("checkout-{function}"));
    let config = sdk_config().await;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let store = Arc::new(DynamoStore::from_env(&config, "TABLE")?);
    let kid = saci_commerce_kit::env_or("GUEST_KID", "guest-1");
    let guest_signer: Arc<dyn TokenSigner> = if let Ok(key) = std::env::var("GUEST_KMS_KEY_ID") {
        Arc::new(KmsSigner::new(&config, key, kid))
    } else {
        let pkcs8 = ssm::secret(&config, "GUEST_KEY").await?;
        Arc::new(
            Es256Key::from_pkcs8(&decode(pkcs8.trim()).ok_or("GUEST_KEY não é base64")?, &kid)
                .map_err(|e| e.to_string())?,
        )
    };
    let checkout = Checkout {
        store: store.clone(),
        internal: saci_commerce_sandbox::boot::internal_client(&config)?,
        clock: Arc::clone(&clock),
        guest_signer,
        guest_issuer: saci_commerce_kit::env("GUEST_ISSUER")?,
        guest_audience: saci_commerce_kit::env_or("GUEST_AUDIENCE", "saci-api"),
        guest_ttl_secs: saci_commerce_kit::env_or("GUEST_TTL_SECS", "86400")
            .parse()
            .unwrap_or(86_400),
    };
    match function.as_str() {
        "api" => {
            run_http(Arc::new(WithOptionalAuth {
                inner: Arc::new(checkout),
                auth: OptionalAuth::from_env(&config)?,
            }))
            .await
        }
        "payments" => run_consumer(store, vec![Arc::new(PaymentEvents(checkout))], clock).await,
        "sweeper" => {
            let checkout = Arc::new(checkout);
            run_scheduled(move || {
                let checkout = Arc::clone(&checkout);
                async move {
                    let n = checkout
                        .sweep()
                        .await
                        .map_err(|p| format!("{} {}", p.status, p.code))?;
                    tracing::info!(compensadas = n, "varredura concluída");
                    Ok(())
                }
            })
            .await
        }
        "relay" => run_relay(&config).await,
        other => Err(format!("SACI_FUNCTION desconhecida: {other}").into()),
    }
}

/// Base64 padrão sem dependência extra.
mod base64_free {
    /// Decodifica base64 padrão (com ou sem `=`).
    pub(crate) fn decode(input: &str) -> Option<Vec<u8>> {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0;
        for c in input
            .bytes()
            .filter(|c| *c != b'=' && !c.is_ascii_whitespace())
        {
            let v = u32::try_from(T.iter().position(|x| *x == c)?).ok()?;
            buf = (buf << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(u8::try_from((buf >> bits) & 0xFF).ok()?);
            }
        }
        Some(out)
    }
}
