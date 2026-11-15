//! Parâmetros do SSM lidos no início da função (fora do caminho quente).
//!
//! Segredos nunca vão em variável de ambiente (convenção da plataforma): a
//! variável traz o **nome** do parâmetro `SecureString`, e a função lê o valor
//! uma vez, no início.

use aws_sdk_ssm::Client;

/// Lê um parâmetro (decifrado). Erro é texto, para o `main` abortar a função.
pub async fn parameter(config: &aws_config::SdkConfig, name: &str) -> Result<String, String> {
    Client::new(config)
        .get_parameter()
        .name(name)
        .with_decryption(true)
        .send()
        .await
        .map_err(|e| format!("SSM {name}: {}", e.into_service_error()))?
        .parameter
        .and_then(|p| p.value)
        .ok_or_else(|| format!("SSM {name} vazio"))
}

/// Lê o valor de `var` direto (local) ou, se `var_PARAM` existir, do SSM.
pub async fn secret(config: &aws_config::SdkConfig, var: &str) -> Result<String, String> {
    if let Ok(name) = std::env::var(format!("{var}_PARAM")) {
        return parameter(config, &name).await;
    }
    crate::env(var)
}
