//! Cursor de página opaco e assinado (seção 4.1: "Cursor assinado e opaco").
//!
//! O cursor carrega a última chave lida e fica **preso ao escopo** da consulta
//! (quem chama + qual lista). Um cursor alterado ou trazido de outra lista é
//! recusado (`validation-failed`), então ele nunca serve para ler fora do que a
//! própria consulta já permite.
//!
//! Formato: `base64url(valor) "." base64url(HMAC-SHA256(chave, escopo "\0" valor))[..16]`.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use saci_problem::Problem;
use sha2::{Digest, Sha256};

use crate::problem::invalid;

/// Assina e confere cursores.
#[derive(Clone)]
pub struct CursorSigner {
    key: Vec<u8>,
}

impl std::fmt::Debug for CursorSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CursorSigner(..)")
    }
}

impl CursorSigner {
    /// Cria com a chave (32 bytes ou mais; em nuvem vem do SSM, nunca do código).
    #[must_use]
    pub fn new(key: &[u8]) -> Self {
        Self { key: key.to_vec() }
    }

    /// Codifica `value` para o escopo dado.
    #[must_use]
    pub fn encode(&self, scope: &str, value: &str) -> String {
        let mac = hmac_sha256(&self.key, &[scope.as_bytes(), b"\0", value.as_bytes()]);
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(value),
            URL_SAFE_NO_PAD.encode(&mac[..16])
        )
    }

    /// Confere e decodifica. Cursor inválido é `validation-failed` em `cursor`.
    pub fn decode(&self, scope: &str, cursor: &str) -> Result<String, Problem> {
        let bad = || invalid("cursor", "Cursor inválido. Volte para a primeira página.");
        let (value, mac) = cursor.split_once('.').ok_or_else(bad)?;
        let value = URL_SAFE_NO_PAD.decode(value).map_err(|_| bad())?;
        let mac = URL_SAFE_NO_PAD.decode(mac).map_err(|_| bad())?;
        let expected = hmac_sha256(&self.key, &[scope.as_bytes(), b"\0", &value]);
        if !constant_time_eq(&expected[..16], &mac) {
            return Err(bad());
        }
        String::from_utf8(value).map_err(|_| bad())
    }
}

/// HMAC-SHA256 (RFC 2104) sobre partes concatenadas.
#[must_use]
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    for p in parts {
        inner.update(p);
    }
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// Comparação em tempo constante.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_scope_binding() {
        let signer = CursorSigner::new(b"chave-de-teste-com-32-bytes-ok!!");
        let c = signer.encode("acc_1:/me/listings", "LST#lst_9");
        assert_eq!(
            signer.decode("acc_1:/me/listings", &c).ok().as_deref(),
            Some("LST#lst_9")
        );
        assert!(signer.decode("acc_2:/me/listings", &c).is_err());
        assert!(signer.decode("acc_1:/me/listings", "lixo").is_err());
    }

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", &[b"what do ya want ", b"for nothing?"]);
        assert_eq!(
            hex::encode(mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
