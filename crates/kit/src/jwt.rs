//! Tokens JWT ES256 (ECDSA P-256 + SHA-256), sem biblioteca de JWT.
//!
//! Usado para:
//!
//! - o **guest token** que o `checkout` emite (seção 5.7): o autorizador da
//!   plataforma confere a assinatura pela chave pública cadastrada na tabela
//!   `authz` (emissor do tipo `guest`). Na nuvem a chave privada fica no KMS e
//!   nunca sai dele (`sign_input` + [`der_to_jose`]); no ambiente local ela
//!   vem de arquivo;
//! - as sessões do servidor de desenvolvimento, que substituem o
//!   `saci-identity` enquanto ele não existe.
//!
//! Só ES256: o algoritmo é fixo e vem da chave, nunca do token.

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{
    ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair,
    UnparsedPublicKey,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

/// Falha de token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JwtError {
    /// Chave inválida.
    #[error("chave inválida")]
    Key,
    /// Token malformado.
    #[error("token malformado")]
    Malformed,
    /// Assinatura não confere.
    #[error("assinatura inválida")]
    Signature,
    /// Fora da validade.
    #[error("token vencido")]
    Expired,
}

/// Monta a parte assinada (`header.claims`) de um JWT ES256.
#[must_use]
pub fn signing_input(kid: &str, claims: &Value) -> String {
    let header = json!({ "alg": "ES256", "typ": "JWT", "kid": kid });
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

/// Junta a parte assinada e a assinatura JOSE (64 bytes `r || s`).
#[must_use]
pub fn assemble(input: &str, signature: &[u8]) -> String {
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

/// Converte uma assinatura ECDSA DER (formato do KMS) em `r || s` de 64 bytes.
pub fn der_to_jose(der: &[u8]) -> Result<[u8; 64], JwtError> {
    // SEQUENCE { INTEGER r, INTEGER s }
    let mut i = 0;
    let mut take = |expect: u8| -> Result<&[u8], JwtError> {
        if der.get(i) != Some(&expect) {
            return Err(JwtError::Malformed);
        }
        let len = usize::from(*der.get(i + 1).ok_or(JwtError::Malformed)?);
        let start = i + 2;
        let end = start + len;
        let out = der.get(start..end).ok_or(JwtError::Malformed)?;
        i = if expect == 0x30 { start } else { end };
        Ok(out)
    };
    take(0x30)?;
    let r = take(0x02)?;
    let s = take(0x02)?;
    let mut out = [0u8; 64];
    for (part, offset) in [(r, 0), (s, 32)] {
        let part = part.strip_prefix(&[0]).unwrap_or(part);
        if part.len() > 32 {
            return Err(JwtError::Malformed);
        }
        out[offset + 32 - part.len()..offset + 32].copy_from_slice(part);
    }
    Ok(out)
}

/// Chave privada ES256 em memória.
#[derive(Debug)]
pub struct Es256Key {
    pair: EcdsaKeyPair,
    kid: String,
}

impl Es256Key {
    /// Carrega uma chave PKCS#8 (DER), como as de `saci-devtools keygen`.
    pub fn from_pkcs8(der: &[u8], kid: &str) -> Result<Self, JwtError> {
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, der)
            .map_err(|_| JwtError::Key)?;
        Ok(Self {
            pair,
            kid: kid.to_owned(),
        })
    }

    /// Gera uma chave nova e devolve também o PKCS#8.
    pub fn generate(kid: &str) -> Result<(Self, Vec<u8>), JwtError> {
        let rng = SystemRandom::new();
        let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| JwtError::Key)?;
        let der = doc.as_ref().to_vec();
        Ok((Self::from_pkcs8(&der, kid)?, der))
    }

    /// Id da chave.
    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// Assina os `claims`.
    pub fn sign(&self, claims: &Value) -> Result<String, JwtError> {
        let input = signing_input(&self.kid, claims);
        let sig = self
            .pair
            .sign(&SystemRandom::new(), input.as_bytes())
            .map_err(|_| JwtError::Key)?;
        Ok(assemble(&input, sig.as_ref()))
    }

    /// Chave pública não comprimida (65 bytes), para conferir.
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.pair.public_key().as_ref().to_vec()
    }

    /// JWKS público com esta chave.
    #[must_use]
    pub fn jwks(&self) -> Value {
        let pk = self.public_key();
        json!({ "keys": [{
            "kty": "EC", "crv": "P-256", "alg": "ES256", "use": "sig", "kid": self.kid,
            "x": URL_SAFE_NO_PAD.encode(&pk[1..33]), "y": URL_SAFE_NO_PAD.encode(&pk[33..65]),
        }]})
    }
}

/// Confere assinatura e validade, e devolve os `claims`.
///
/// `public_key` é o ponto não comprimido (65 bytes). `now` em segundos.
pub fn verify(token: &str, public_key: &[u8], now: u64) -> Result<Value, JwtError> {
    let mut parts = token.split('.');
    let (Some(h), Some(c), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Malformed);
    };
    let header: Value = decode_json(h)?;
    if header.get("alg").and_then(Value::as_str) != Some("ES256") {
        return Err(JwtError::Malformed);
    }
    let signature = URL_SAFE_NO_PAD.decode(s).map_err(|_| JwtError::Malformed)?;
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, public_key)
        .verify(format!("{h}.{c}").as_bytes(), &signature)
        .map_err(|_| JwtError::Signature)?;
    let claims: Value = decode_json(c)?;
    let exp = claims
        .get("exp")
        .and_then(Value::as_u64)
        .ok_or(JwtError::Malformed)?;
    if exp + 30 < now {
        return Err(JwtError::Expired);
    }
    Ok(claims)
}

/// Lê os `claims` sem conferir (só para decidir qual chave usar).
pub fn peek(token: &str) -> Result<Value, JwtError> {
    decode_json(token.split('.').nth(1).ok_or(JwtError::Malformed)?)
}

fn decode_json(part: &str) -> Result<Value, JwtError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| JwtError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| JwtError::Malformed)
}

/// Quem assina tokens: chave em memória (local) ou KMS (nuvem, `aws::kms`).
pub trait TokenSigner: Send + Sync + std::fmt::Debug {
    /// Assina os `claims` e devolve o JWT.
    fn sign_claims<'a>(&'a self, claims: &'a Value) -> crate::BoxFut<'a, Result<String, JwtError>>;
}

impl TokenSigner for Es256Key {
    fn sign_claims<'a>(&'a self, claims: &'a Value) -> crate::BoxFut<'a, Result<String, JwtError>> {
        Box::pin(async move { self.sign(claims) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() -> Result<(), JwtError> {
        let (key, der) = Es256Key::generate("k1")?;
        let token = key.sign(&json!({ "sub": "ord_1", "exp": 2_000 }))?;
        let claims = verify(&token, &key.public_key(), 1_000)?;
        assert_eq!(claims["sub"], "ord_1");
        assert_eq!(
            verify(&token, &key.public_key(), 3_000),
            Err(JwtError::Expired)
        );
        let other = Es256Key::from_pkcs8(&Es256Key::generate("k2")?.1, "k2")?;
        assert_eq!(
            verify(&token, &other.public_key(), 1_000),
            Err(JwtError::Signature)
        );
        assert!(Es256Key::from_pkcs8(&der, "k1").is_ok());
        assert_eq!(key.jwks()["keys"][0]["kid"], "k1");
        Ok(())
    }

    #[test]
    fn converts_der_signature() -> Result<(), JwtError> {
        // r com zero à esquerda (33 bytes) e s curto (31 bytes).
        let mut der = vec![0x30, 68, 0x02, 33, 0x00];
        der.extend([0xAA; 32]);
        der.extend([0x02, 31]);
        der.extend([0x11; 31]);
        let jose = der_to_jose(&der)?;
        assert_eq!(jose[0], 0xAA);
        assert_eq!(jose[32], 0x00);
        assert_eq!(jose[63], 0x11);
        Ok(())
    }
}
