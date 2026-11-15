//! Ponte opcional com o servidor de desenvolvimento do `saci-identity`
//! (`SACI_DEV_IDENTITY_URL=http://localhost:8788`).
//!
//! Sem a variável, nada muda: `identity` e `kyc` são o sandbox. Com ela:
//!
//! | O quê | Como |
//! | --- | --- |
//! | `/v1/identity/*` | repassado ao `saci-identity-dev` (método, caminho, cabeçalhos, cookie e corpo); o guest token conferido aqui segue como `x-saci-dev-guest-order` |
//! | Alvos internos `identity` e `kyc` | `POST /internal/{alvo}` no `saci-identity-dev`, no mesmo formato `{op, payload}` das Lambdas |
//! | `Authorization` | conferido com a chave pública das sessões de lá (`/.well-known/jwks.json`) |
//!
//! Só HTTP em `localhost` (é desenvolvimento); cliente HTTP/1.1 mínimo, sem
//! dependência nova.

use std::fmt::Write as _;
use std::io::{Read, Write};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use saci_commerce_kit::http::{HttpService, Request, Response};
use saci_commerce_kit::internal::{
    InternalHandler, InternalRequest, InternalResponse, LocalInternal,
};
use saci_commerce_kit::problem::{internal, unavailable};
use saci_commerce_kit::{BoxFut, Problem};
use serde_json::Value;

/// A ponte.
#[derive(Debug, Clone)]
pub(crate) struct Bridge {
    host: String,
}

/// Resposta crua.
struct Raw {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn request_bytes(
    method: &str,
    host: &str,
    target: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Vec<u8> {
    let mut out = format!(
        "{method} {target} HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\ncontent-length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        if !matches!(
            k.as_str(),
            "host" | "connection" | "content-length" | "transfer-encoding"
        ) {
            let _ = write!(out, "{k}: {v}\r\n");
        }
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn parse(raw: &[u8]) -> Option<Raw> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;
    let mut lines = head.split("\r\n");
    let status = lines.next()?.split(' ').nth(1)?.parse().ok()?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Some(Raw {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    })
}

impl Bridge {
    /// Lê `SACI_DEV_IDENTITY_URL`. Ausente: sem ponte.
    pub(crate) fn from_env() -> Result<Option<Self>, String> {
        let Ok(url) = std::env::var("SACI_DEV_IDENTITY_URL") else {
            return Ok(None);
        };
        let host = url
            .strip_prefix("http://")
            .ok_or("SACI_DEV_IDENTITY_URL deve ser http://localhost:<porta>")?
            .trim_end_matches('/')
            .to_owned();
        Ok(Some(Self { host }))
    }

    /// A chave pública (ponto não comprimido) das sessões do `saci-identity-dev`.
    pub(crate) fn session_public_key(&self) -> Result<Vec<u8>, String> {
        let mut stream = std::net::TcpStream::connect(&self.host).map_err(|e| {
            format!("saci-identity-dev não responde em {}: {e} (suba-o antes: `task dev` no saci-identity)", self.host)
        })?;
        stream
            .write_all(&request_bytes(
                "GET",
                &self.host,
                "/.well-known/jwks.json",
                &[],
                &[],
            ))
            .map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;
        let jwks: Value = parse(&raw)
            .and_then(|r| serde_json::from_slice(&r.body).ok())
            .ok_or("JWKS do saci-identity-dev inválido")?;
        let key = &jwks["keys"][0];
        let coord = |k: &str| {
            key[k]
                .as_str()
                .and_then(|v| URL_SAFE_NO_PAD.decode(v).ok())
                .ok_or_else(|| format!("JWKS sem {k}"))
        };
        let mut point = vec![4u8];
        point.extend(coord("x")?);
        point.extend(coord("y")?);
        Ok(point)
    }

    async fn send(
        &self,
        method: &str,
        target: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Raw, String> {
        // Cliente bloqueante numa thread própria: sem dependência nova no tokio.
        let host = self.host.clone();
        let bytes = request_bytes(method, &host, target, headers, body);
        tokio::task::spawn_blocking(move || {
            let mut stream = std::net::TcpStream::connect(&host)
                .map_err(|e| format!("saci-identity-dev: {e}"))?;
            stream.write_all(&bytes).map_err(|e| e.to_string())?;
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;
            parse(&raw).ok_or_else(|| "resposta HTTP inválida do saci-identity-dev".to_owned())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Troca os alvos `identity` e `kyc` do sandbox pelos de verdade.
    pub(crate) fn register(&self, local: &LocalInternal) {
        for target in ["identity", "kyc"] {
            local.register(
                target,
                std::sync::Arc::new(Remote {
                    bridge: self.clone(),
                    target,
                }),
            );
        }
    }
}

/// Um alvo interno do `saci-identity-dev`.
#[derive(Debug)]
struct Remote {
    bridge: Bridge,
    target: &'static str,
}

impl InternalHandler for Remote {
    fn call<'a>(&'a self, op: &'a str, payload: Value) -> BoxFut<'a, Result<Value, Problem>> {
        Box::pin(async move {
            let body = serde_json::to_vec(&InternalRequest {
                op: op.to_owned(),
                payload,
            })
            .map_err(|e| internal(&e))?;
            let headers = [("content-type".to_owned(), "application/json".to_owned())];
            let raw = self
                .bridge
                .send(
                    "POST",
                    &format!("/internal/{}", self.target),
                    &headers,
                    &body,
                )
                .await
                .map_err(|e| unavailable(&e))?;
            match serde_json::from_slice::<InternalResponse>(&raw.body).map_err(|e| internal(&e))? {
                InternalResponse::Ok(v) => Ok(v),
                InternalResponse::Error(p) => Err(p),
            }
        })
    }
}

impl HttpService for Bridge {
    fn handle(&self, req: Request) -> BoxFut<'_, Response> {
        Box::pin(async move {
            let mut target = req.path.clone();
            if !req.query.is_empty() {
                let q: Vec<String> = req.query.iter().map(|(k, v)| format!("{k}={v}")).collect();
                target = format!("{target}?{}", q.join("&"));
            }
            let mut headers: Vec<(String, String)> = req
                .headers
                .iter()
                .filter(|(k, _)| k.as_str() != "x-guest-token")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if let Some(ip) = &req.source_ip {
                headers.push(("x-forwarded-for".into(), ip.clone()));
            }
            if let Some(order) = req.auth.as_ref().and_then(|a| a.order_id.clone()) {
                headers.push(("x-saci-dev-guest-order".into(), order));
            }
            match self.send(&req.method, &target, &headers, &req.body).await {
                Ok(raw) => {
                    let mut r = Response {
                        status: raw.status,
                        headers: Vec::new(),
                        body: String::from_utf8_lossy(&raw.body).into_owned(),
                    };
                    for (k, v) in raw.headers {
                        if !matches!(
                            k.as_str(),
                            "content-length" | "connection" | "date" | "transfer-encoding"
                        ) {
                            r = r.header(&k, &v);
                        }
                    }
                    r
                }
                Err(e) => Response::problem(unavailable(&e)),
            }
        })
    }
}
