//! HTTP sem framework.
//!
//! [`Request`] e [`Response`] são estruturas simples: o adaptador da Lambda
//! (`aws::lambda_http`) e o servidor de desenvolvimento convertem de e para
//! eles. Os serviços roteiam com *slice patterns* sobre [`Request::segments`],
//! sem macro nem tabela de rotas:
//!
//! ```
//! use saci_commerce_kit::http::{Request, Response};
//!
//! let req = Request::new("GET", "/v1/catalog/listings/lst_1");
//! let out = match (req.method.as_str(), req.segments().as_slice()) {
//!     ("GET", ["v1", "catalog", "listings", id]) => Response::text(200, id),
//!     _ => Response::text(404, "?"),
//! };
//! assert_eq!(out.body, "lst_1");
//! ```
//!
//! Convenções da proposta (4.1) aplicadas por [`finish`]: `X-Request-Id` em toda
//! resposta, erros em `application/problem+json` com `request_id`, e
//! `Cache-Control: no-store` em tudo o que não declarou cache.

use std::collections::HashMap;

use saci_authz::AuthContext;
use saci_problem::Problem;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::problem::{invalid, normalize, problem};

/// Requisição HTTP já autenticada pela borda.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// Método em maiúsculas (`GET`).
    pub method: String,
    /// Caminho completo, sem o estágio do API Gateway (`/v1/catalog/listings`).
    pub path: String,
    /// Parâmetros de query, na ordem recebida.
    pub query: Vec<(String, String)>,
    /// Cabeçalhos, com o nome em minúsculas.
    pub headers: HashMap<String, String>,
    /// Corpo cru.
    pub body: Vec<u8>,
    /// Quem chama, segundo o autorizador. `None` em rota pública.
    pub auth: Option<AuthContext>,
    /// Id da requisição (`X-Request-Id`).
    pub request_id: String,
    /// IP de origem, para limites por IP.
    pub source_ip: Option<String>,
}

impl Request {
    /// Requisição mínima (testes e chamadas internas).
    #[must_use]
    pub fn new(method: &str, path_and_query: &str) -> Self {
        let (path, query) = path_and_query
            .split_once('?')
            .unwrap_or((path_and_query, ""));
        Self {
            method: method.to_ascii_uppercase(),
            path: path.to_owned(),
            query: parse_query(query),
            request_id: "req_local".to_owned(),
            ..Self::default()
        }
    }

    /// Acrescenta um cabeçalho (nome em minúsculas).
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers
            .insert(name.to_ascii_lowercase(), value.to_owned());
        self
    }

    /// Define o corpo JSON.
    #[must_use]
    pub fn with_json(mut self, body: &serde_json::Value) -> Self {
        self.body = body.to_string().into_bytes();
        self.headers
            .insert("content-type".into(), "application/json".into());
        self
    }

    /// Define o contexto de quem chama.
    #[must_use]
    pub fn with_auth(mut self, auth: AuthContext) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Segmentos do caminho, já decodificados (`%20` → espaço).
    #[must_use]
    pub fn segments(&self) -> Vec<&str> {
        self.path.split('/').filter(|s| !s.is_empty()).collect()
    }

    /// Valor de um parâmetro de query (o primeiro, vazio conta como ausente).
    #[must_use]
    pub fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, v)| k == name && !v.is_empty())
            .map(|(_, v)| v.as_str())
    }

    /// Lista separada por vírgula (`ids=a,b`), ou repetida (`ids=a&ids=b`).
    #[must_use]
    pub fn query_list(&self, name: &str) -> Vec<String> {
        self.query
            .iter()
            .filter(|(k, _)| k == name)
            .flat_map(|(_, v)| v.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Inteiro opcional com limites; fora deles é `validation-failed`.
    pub fn query_int(&self, name: &str, min: i64, max: i64) -> Result<Option<i64>, Problem> {
        let Some(raw) = self.query(name) else {
            return Ok(None);
        };
        match raw.parse::<i64>() {
            Ok(v) if (min..=max).contains(&v) => Ok(Some(v)),
            _ => Err(invalid(
                name,
                &format!("Use um número inteiro entre {min} e {max}."),
            )),
        }
    }

    /// Texto opcional com tamanho máximo.
    pub fn query_text(&self, name: &str, max_len: usize) -> Result<Option<&str>, Problem> {
        match self.query(name) {
            Some(v) if v.chars().count() > max_len => Err(invalid(
                name,
                &format!("Use no máximo {max_len} caracteres."),
            )),
            other => Ok(other),
        }
    }

    /// Valor opcional dentre os aceitos.
    pub fn query_enum(&self, name: &str, allowed: &[&str]) -> Result<Option<&str>, Problem> {
        match self.query(name) {
            Some(v) if !allowed.contains(&v) => Err(invalid(name, "Valor não aceito.")),
            other => Ok(other),
        }
    }

    /// Booleano opcional (`true`/`false`).
    pub fn query_bool(&self, name: &str) -> Result<Option<bool>, Problem> {
        match self.query(name) {
            None => Ok(None),
            Some("true" | "1") => Ok(Some(true)),
            Some("false" | "0") => Ok(Some(false)),
            Some(_) => Err(invalid(name, "Use true ou false.")),
        }
    }

    /// Cabeçalho (nome sem diferença de caixa).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// Corpo JSON tipado. Corpo ausente ou fora do formato é `validation-failed`.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, Problem> {
        let body: &[u8] = if self.body.is_empty() {
            b"{}"
        } else {
            &self.body
        };
        serde_json::from_slice(body).map_err(|e| {
            // A mensagem do serde diz o campo, mas não o valor: segura para a resposta.
            let detail = e.to_string();
            let field = detail
                .split('`')
                .nth(1)
                .filter(|f| f.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
                .unwrap_or("body")
                .to_owned();
            invalid(&field, "Campo ausente ou em formato inválido.")
        })
    }

    /// Contexto de quem chama; ausente é `unauthorized`.
    pub fn auth(&self) -> Result<&AuthContext, Problem> {
        self.auth.as_ref().ok_or_else(|| problem("unauthorized"))
    }
}

/// Resposta HTTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Código HTTP.
    pub status: u16,
    /// Cabeçalhos, nome em minúsculas.
    pub headers: Vec<(String, String)>,
    /// Corpo (sempre texto: JSON ou vazio).
    pub body: String,
}

impl Response {
    /// Resposta JSON. Falha de serialização vira 500 (nunca acontece com os tipos da API).
    #[must_use]
    pub fn json<T: Serialize + ?Sized>(status: u16, body: &T) -> Self {
        match serde_json::to_string(body) {
            Ok(text) => Self {
                status,
                headers: vec![("content-type".into(), "application/json".into())],
                body: text,
            },
            Err(e) => Self::problem(crate::problem::internal(&e)),
        }
    }

    /// `200` com JSON.
    #[must_use]
    pub fn ok<T: Serialize + ?Sized>(body: &T) -> Self {
        Self::json(200, body)
    }

    /// Texto simples (testes, saúde).
    #[must_use]
    pub fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            body: body.to_owned(),
        }
    }

    /// `204` sem corpo.
    #[must_use]
    pub fn no_content() -> Self {
        Self {
            status: 204,
            headers: Vec::new(),
            body: String::new(),
        }
    }

    /// Recusa em `application/problem+json`, já com os códigos do contrato.
    #[must_use]
    pub fn problem(p: Problem) -> Self {
        let p = normalize(p);
        Self {
            status: p.status,
            headers: vec![
                ("content-type".into(), "application/problem+json".into()),
                ("cache-control".into(), "no-store".into()),
            ],
            body: p.to_json(),
        }
    }

    /// Acrescenta ou troca um cabeçalho.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        let name = name.to_ascii_lowercase();
        self.headers.retain(|(k, _)| *k != name);
        self.headers.push((name, value.to_owned()));
        self
    }

    /// Política de cache pública (`public, max-age=N`), só para dado público.
    #[must_use]
    pub fn public(self, max_age_secs: u32) -> Self {
        self.header(
            "cache-control",
            &format!("public, max-age={max_age_secs}, stale-while-revalidate=60"),
        )
    }

    /// Valor de um cabeçalho da resposta.
    #[must_use]
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Resultado de um tratador.
pub type HttpResult = Result<Response, Problem>;

/// Fecha uma resposta: `X-Request-Id`, `request_id` nas recusas e `no-store`
/// em tudo o que não declarou cache (seção 4.1: "toda resposta sob `/me`, ou
/// com dado pessoal, leva `Cache-Control: no-store`").
#[must_use]
pub fn finish(result: HttpResult, request_id: &str) -> Response {
    let mut response = match result {
        Ok(r) => r,
        Err(p) => {
            if p.status >= 500 {
                tracing::warn!(status = p.status, code = %p.code, "recusa do servidor");
            }
            Response::problem(p.with_request_id(request_id))
        }
    };
    if response.header_value("cache-control").is_none() {
        response = response.header("cache-control", "no-store");
    }
    response.header("x-request-id", request_id)
}

/// Decodifica `%XX` e `+` de um componente de URL.
#[must_use]
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                    .ok_or(())
                {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(()) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Codifica um componente de URL (só o necessário para ids e nomes de vitrine).
#[must_use]
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&hex::encode_upper([b]));
        }
    }
    out
}

/// Interpreta uma query string (`a=1&b=x%20y`).
#[must_use]
pub fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

/// ETag fraco de uma versão de objeto (`W/"7"`).
#[must_use]
pub fn etag(version: u64) -> String {
    format!("W/\"{version}\"")
}

/// Confere `If-Match` contra a versão atual: ausente é `validation-failed`
/// (o contrato o exige); diferente é `precondition-failed` (412).
pub fn require_if_match(req: &Request, current_version: u64) -> Result<(), Problem> {
    let Some(value) = req.header("if-match") else {
        return Err(invalid("if_match", "Envie o ETag lido antes de editar."));
    };
    let wanted = value.trim().trim_start_matches("W/").trim_matches('"');
    if wanted == current_version.to_string() || value.trim() == "*" {
        Ok(())
    } else {
        Err(problem("precondition-failed"))
    }
}

/// Um serviço HTTP: o que a Lambda e o servidor de desenvolvimento chamam.
pub trait HttpService: Send + Sync + std::fmt::Debug {
    /// Trata a requisição. Nunca falha: recusas viram [`Response::problem`].
    fn handle(&self, req: Request) -> crate::BoxFut<'_, Response>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_query_lists_and_decodes() {
        let req = Request::new("get", "/v1/x?ids=a,b&ids=c&q=barro%20vivo&empty=");
        assert_eq!(req.method, "GET");
        assert_eq!(req.query_list("ids"), ["a", "b", "c"]);
        assert_eq!(req.query("q"), Some("barro vivo"));
        assert_eq!(req.query("empty"), None);
    }

    #[test]
    fn validates_query_ints() {
        let req = Request::new("GET", "/v1/x?limit=99");
        assert_eq!(
            req.query_int("limit", 1, 48).map_err(|p| p.status),
            Err(422)
        );
        assert_eq!(req.query_int("page", 1, 100).ok(), Some(None));
    }

    #[test]
    fn finish_sets_request_id_and_no_store() {
        let r = finish(Ok(Response::ok(&serde_json::json!({}))), "req_1");
        assert_eq!(r.header_value("x-request-id"), Some("req_1"));
        assert_eq!(r.header_value("cache-control"), Some("no-store"));
        let r = finish(Ok(Response::ok(&1).public(60)), "req_1");
        assert!(
            r.header_value("cache-control")
                .is_some_and(|v| v.starts_with("public"))
        );
        let r = finish(Err(Problem::unauthenticated()), "req_2");
        assert_eq!(r.status, 401);
        assert!(r.body.contains("\"code\":\"unauthorized\""));
        assert!(r.body.contains("req_2"));
    }

    #[test]
    fn if_match_rules() {
        let req = Request::new("PUT", "/x").with_header("If-Match", "W/\"3\"");
        assert!(require_if_match(&req, 3).is_ok());
        assert_eq!(require_if_match(&req, 4).map_err(|p| p.status), Err(412));
        let missing = Request::new("PUT", "/x");
        assert_eq!(
            require_if_match(&missing, 1).map_err(|p| p.status),
            Err(422)
        );
    }
}
