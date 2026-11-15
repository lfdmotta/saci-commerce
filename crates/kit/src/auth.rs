//! Níveis de acesso da proposta (seção 4.2) sobre o contexto do autorizador.
//!
//! O autorizador da plataforma diz **quem é** (conta, vitrine, papel, seções);
//! estas funções decidem se a rota aceita essa pessoa, e devolvem o que a rota
//! precisa (o `account_id` ou o `storefront_id`), **sempre do token, nunca da URL**.
//! A posse de cada objeto é conferida no serviço, com 404 para objeto alheio.

use std::collections::HashMap;

pub use saci_authz::{AuthContext, PrincipalKind, Role, Section};
use saci_problem::Problem;
use serde_json::Value;

use crate::http::Request;
use crate::problem::{normalize, problem};

/// Janela de `USER+`: autenticação de até 5 minutos (seção 4.2).
pub const RECENT_AUTH_SECS: u64 = 300;

/// `USER`: conta com contato verificado. Devolve o `account_id`.
pub fn user(req: &Request) -> Result<&str, Problem> {
    req.auth()?.require_user().map_err(normalize)
}

/// `USER` opcional: a conta, quando há token de conta; `None` sem token.
#[must_use]
pub fn optional_user(req: &Request) -> Option<&str> {
    req.auth.as_ref().and_then(|a| a.require_user().ok())
}

/// `USER+`: conta reautenticada há no máximo 5 minutos. `now` em segundos.
pub fn recent_user(req: &Request, now: u64) -> Result<&str, Problem> {
    req.auth()?
        .require_recent_auth(now, RECENT_AUTH_SECS)
        .map_err(normalize)
}

/// `SELLER:<seção>`: responsável ou equipe com a seção. Devolve a vitrine.
pub fn section(req: &Request, section: Section) -> Result<&str, Problem> {
    req.auth()?.require_section(section).map_err(normalize)
}

/// `OWNER`: só a pessoa responsável. Devolve a vitrine.
pub fn owner(req: &Request) -> Result<&str, Problem> {
    req.auth()?.require_owner().map_err(normalize)
}

/// `OWNER+`: responsável e reautenticada. Devolve a vitrine.
pub fn recent_owner(req: &Request, now: u64) -> Result<&str, Problem> {
    recent_user(req, now)?;
    owner(req)
}

/// `GUEST`: o pedido do guest token.
pub fn guest_order(req: &Request) -> Result<&str, Problem> {
    let auth = req.auth()?;
    match (auth.kind, auth.order_id.as_deref()) {
        (PrincipalKind::Guest, Some(order)) => Ok(order),
        _ => Err(problem("unauthorized")),
    }
}

/// O papel da pessoa na vitrine do token, quando há.
#[must_use]
pub fn role(req: &Request) -> Option<Role> {
    req.auth.as_ref().and_then(|a| a.role)
}

/// Monta um contexto de conta (servidor de desenvolvimento e testes).
#[must_use]
pub fn user_context(
    account_id: &str,
    auth_time: u64,
    grant: Option<(&str, &str, &[&str])>,
) -> AuthContext {
    let mut fields: HashMap<String, Value> = HashMap::from([
        ("kind".into(), Value::from("user")),
        ("account_id".into(), Value::from(account_id)),
        ("auth_time".into(), Value::from(auth_time.to_string())),
    ]);
    if let Some((storefront, role, sections)) = grant {
        fields.insert("storefront_id".into(), Value::from(storefront));
        fields.insert("role".into(), Value::from(role));
        fields.insert("sections".into(), Value::from(sections.join(",")));
    }
    // Os campos acima sempre formam um contexto válido.
    AuthContext::from_fields(&fields).unwrap_or_else(|_| guest_context(""))
}

/// Monta um contexto de guest token (servidor de desenvolvimento e testes).
#[must_use]
pub fn guest_context(order_id: &str) -> AuthContext {
    let fields: HashMap<String, Value> = HashMap::from([
        ("kind".into(), Value::from("guest")),
        ("order_id".into(), Value::from(order_id)),
    ]);
    AuthContext {
        kind: PrincipalKind::Guest,
        account_id: None,
        auth_time: None,
        scopes: std::collections::BTreeSet::new(),
        storefront_id: None,
        role: None,
        sections: std::collections::BTreeSet::new(),
        order_id: fields
            .get("order_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        conversation_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_map_to_contract_codes() {
        let anon = Request::new("GET", "/v1/customers/me");
        assert_eq!(user(&anon).map_err(|p| p.code), Err("unauthorized".into()));

        let old = Request::new("POST", "/x").with_auth(user_context("acc_1", 100, None));
        assert_eq!(
            recent_user(&old, 1_000).map_err(|p| p.code),
            Err("reauth-required".into())
        );
        assert_eq!(recent_user(&old, 300).ok(), Some("acc_1"));

        let member = Request::new("GET", "/x").with_auth(user_context(
            "acc_2",
            100,
            Some(("stf_1", "member", &["pedidos"])),
        ));
        assert_eq!(section(&member, Section::Pedidos).ok(), Some("stf_1"));
        assert_eq!(
            section(&member, Section::Vitrine).map_err(|p| p.status),
            Err(403)
        );
        assert!(owner(&member).is_err());
    }
}
