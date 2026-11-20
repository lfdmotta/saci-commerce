//! `saci-commerce-dev`: o back-end do `saci-commerce` inteiro na máquina, para
//! a interface (`saci-interface`) testar contra o código real.
//!
//! - Os **mesmos** crates de serviço das Lambdas, com os adaptadores em
//!   memória: armazenamento, arquivos, barramento de eventos (entregue aos
//!   mesmos consumidores) e chamadas internas em processo.
//! - O papel da **borda**: CORS para o site do Expo e autenticação equivalente
//!   ao autorizador da plataforma (JWT ES256, papéis de vitrine, guest token).
//! - O **sandbox** dos domínios ainda sem repositório (`/v1/identity`,
//!   `/v1/payments`, KYC, Caipora), com as contas de exemplo das sementes;
//!   `identity` e `payments` podem ser os servidores de verdade (pontes).
//! - O **CDN** e o `files.` locais: envio direto de imagens, fotos publicadas,
//!   etiquetas assinadas e os layouts de `domains/content/layouts`.
//!
//! ```text
//! cargo run -p saci-commerce-dev            # http://localhost:8787
//! ```
//!
//! | Variável | Padrão | Para |
//! | --- | --- | --- |
//! | `SACI_DEV_PORT` | `8787` | Porta (só em 127.0.0.1) |
//! | `SACI_DEV_PUBLIC_URL` | `http://localhost:8787` | Base das URLs devolvidas (troque pelo IP da máquina para o app no celular) |
//! | `SACI_DEV_ORIGINS` | `http://localhost:8081,http://localhost:19006` | Origens aceitas no CORS |
//! | `SACI_DEV_BIND` | `127.0.0.1` | Interface de rede (use `0.0.0.0` para o celular na mesma rede) |
//! | `SACI_DEV_IDENTITY_URL` | — | Usa o `saci-identity-dev` (`http://localhost:8788`) no lugar do sandbox de `identity` e `kyc` (`identity_bridge.rs`) |
//! | `SACI_DEV_PAYMENTS_URL` | — | Usa o `saci-payments-dev` (`http://localhost:8789`) no lugar do sandbox de `payments`: `/v1/payments`, cobrança, Pix e eventos (`payments_bridge.rs`) |
//! | `RUST_LOG` | `info` | Nível de log |
//!
//! Detalhes e roteiro de teste em `local-env/README.md`. **Só para
//! desenvolvimento**: chaves geradas a cada início, dados em memória.

mod identity_bridge;
mod payments_bridge;
mod wiring;

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use saci_commerce_kit::auth::{guest_context, user_context};
use saci_commerce_kit::http::{Request, Response, finish, parse_query, percent_decode};
use saci_commerce_kit::jwt;
use saci_commerce_kit::objects::ObjectStore;
use saci_commerce_kit::problem::{not_found, problem};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

use crate::wiring::App;

#[tokio::main]
async fn main() -> Result<(), String> {
    saci_telemetry::init("saci-commerce-dev");
    let port: u16 = saci_commerce_kit::env_or("SACI_DEV_PORT", "8787")
        .parse()
        .map_err(|_| "SACI_DEV_PORT inválida")?;
    let public_url =
        saci_commerce_kit::env_or("SACI_DEV_PUBLIC_URL", &format!("http://localhost:{port}"));
    let bind = saci_commerce_kit::env_or("SACI_DEV_BIND", "127.0.0.1");
    let origins: Vec<String> = saci_commerce_kit::env_or(
        "SACI_DEV_ORIGINS",
        "http://localhost:8081,http://localhost:19006",
    )
    .split(',')
    .map(|o| o.trim().trim_end_matches('/').to_owned())
    .filter(|o| !o.is_empty())
    .collect();

    let (app, mut worker) = wiring::build(&public_url)?;
    let seeds = worker.seeds.take();
    tokio::spawn(worker.run());
    if let Some(seeds) = seeds {
        seeds.run().await?;
    }
    let app = Arc::new(app);
    let origins = Arc::new(origins);

    let addr: SocketAddr = format!("{bind}:{port}")
        .parse()
        .map_err(|e| format!("endereço inválido: {e}"))?;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("não consegui abrir {addr}: {e}"))?;
    tracing::info!(%addr, %public_url, "saci-commerce-dev pronto");
    print_banner(&public_url);

    loop {
        let (stream, peer) = listener.accept().await.map_err(|e| e.to_string())?;
        let (app, origins) = (Arc::clone(&app), Arc::clone(&origins));
        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let (app, origins) = (Arc::clone(&app), Arc::clone(&origins));
                async move { Ok::<_, Infallible>(serve(&app, &origins, req, peer).await) }
            });
            if let Err(e) = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                tracing::debug!(error = %e, "conexão encerrada");
            }
        });
    }
}

#[allow(clippy::print_stdout)]
fn print_banner(public_url: &str) {
    println!(
        "\n  saci-commerce-dev em {public_url}\n\
         \n  Na interface (saci-interface/.env):\
         \n    EXPO_PUBLIC_USE_MOCKS=false\
         \n    EXPO_PUBLIC_API_URL={public_url}\
         \n    EXPO_PUBLIC_PAY_URL={public_url}\
         \n    EXPO_PUBLIC_LAYOUT_BASE_URL={public_url}/layouts\
         \n\n  Contas de exemplo (senha feira-de-exemplo; código de verificação 123456):"
    );
    // CPFs calculados aqui: no repositório ficam só os 9 primeiros dígitos.
    for (who, base) in [
        ("Marina, quem compra", "529982247"),
        ("Helena, Ateliê Barro Vivo", "390533447"),
        ("Joana, equipe do Barro Vivo", "714602380"),
    ] {
        println!(
            "    {} — {who}",
            saci_commerce_sandbox::identity::cpf_from_base(base)
        );
    }
    println!();
}

type HyperResponse = hyper::Response<Full<Bytes>>;

async fn serve(
    app: &App,
    origins: &[String],
    req: hyper::Request<Incoming>,
    peer: SocketAddr,
) -> HyperResponse {
    let origin = req
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let allowed = origin.as_deref().filter(|o| {
        origins.iter().any(|x| x == o)
            || o.starts_with("http://localhost:")
            || o.starts_with("http://127.0.0.1:")
    });
    let mut response = if req.method() == hyper::Method::OPTIONS {
        Response::no_content()
    } else {
        handle(app, req, peer).await
    };
    if let Some(o) = allowed {
        // CORS como a borda faria (só o site; o app nativo não usa CORS).
        response = response
            .header("access-control-allow-origin", o)
            .header("access-control-allow-credentials", "true")
            .header("access-control-allow-methods", "GET, POST, PUT, PATCH, DELETE, OPTIONS")
            .header(
                "access-control-allow-headers",
                "Authorization, Content-Type, Idempotency-Key, If-Match, X-Client, X-Guest-Token, X-Conversation-Token, x-amz-checksum-sha256",
            )
            .header("access-control-expose-headers", "ETag, X-Request-Id, Location")
            .header("access-control-max-age", "600")
            .header("vary", "Origin");
    }
    to_hyper(response)
}

fn to_hyper(r: Response) -> HyperResponse {
    let mut builder = hyper::Response::builder().status(r.status);
    let binary = r.header_value("x-binary").is_some();
    for (k, v) in r.headers.iter().filter(|(k, _)| k != "x-binary") {
        builder = builder.header(k, v);
    }
    // Arquivos viajam no `Response` de texto como latin-1 (um char por byte).
    let body: Vec<u8> = if binary {
        r.body
            .chars()
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .collect()
    } else {
        r.body.into_bytes()
    };
    builder
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_else(|_| hyper::Response::new(Full::new(Bytes::new())))
}

async fn handle(app: &App, req: hyper::Request<Incoming>, peer: SocketAddr) -> Response {
    let method = req.method().as_str().to_owned();
    let path = percent_decode(req.uri().path());
    let query_string = req.uri().query().unwrap_or_default().to_owned();
    let headers: HashMap<String, String> = req
        .headers()
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|v| (k.as_str().to_ascii_lowercase(), v.to_owned()))
        })
        .collect();
    let body = match req.into_body().collect().await {
        Ok(b) => b.to_bytes().to_vec(),
        Err(_) => Vec::new(),
    };
    let request_id = saci_ids::Id::new(saci_ids::prefix::REQUEST).to_string();
    let mut request = Request {
        method,
        path: path.clone(),
        query: parse_query(&query_string),
        headers,
        body,
        auth: None,
        request_id: request_id.clone(),
        source_ip: Some(peer.ip().to_string()),
    };

    // Arquivos do CDN, de `files.` e o envio direto.
    if let Some(rest) = path.strip_prefix("/dev/") {
        return dev_routes(app, &request, rest).await;
    }
    if let Some(screen) = path.strip_prefix("/layouts/") {
        return layout(screen).await;
    }

    // O papel do autorizador da plataforma.
    if let Err(r) = authenticate(app, &mut request) {
        return finish(Err(r), &request_id);
    }
    let segments = request.segments();
    let service = match segments.as_slice() {
        [
            "v1",
            "catalog",
            "categories" | "search" | "artisans" | "storefronts",
            ..,
        ]
        | ["v1", "catalog", "listings"] => &app.search,
        ["v1", "catalog", ..] => &app.catalog,
        ["v1", "sellers", ..] => &app.sellers,
        ["v1", "customers", ..] => &app.customers,
        ["v1", "reviews", ..] => &app.reviews,
        ["v1", "shipping", "hooks", ..] => &app.carrier_hooks,
        ["v1", "shipping", ..] => &app.shipping,
        ["v1", "media", ..] => &app.media,
        ["v1", "checkout", ..] => &app.checkout,
        ["v1", "orders", ..] => &app.orders,
        ["v1", "support", ..] => &app.support,
        ["v1", "notifications", ..] => &app.notifications,
        ["v1", "identity", ..] => &app.identity,
        ["v1", "payments", ..] => &app.payments,
        _ => return finish(Err(not_found()), &request_id),
    };
    let started = std::time::Instant::now();
    let response = service.handle(request).await;
    tracing::info!(status = response.status, path = %path, ms = started.elapsed().as_millis(), "requisição");
    response
}

/// Confere o `Authorization` (sessão do sandbox) e o `X-Guest-Token`, como o
/// autorizador da plataforma, e monta o contexto com o papel da vitrine.
fn authenticate(app: &App, req: &mut Request) -> Result<(), saci_commerce_kit::Problem> {
    let now = app.clock.epoch();
    if let Some(bearer) = req
        .header("authorization")
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::to_owned)
    {
        let claims = jwt::verify(&bearer, &app.session_public_key, now)
            .map_err(|_| problem("unauthorized"))?;
        if claims["exp"].as_u64().unwrap_or(0) < now {
            return Err(problem("session-expired"));
        }
        let account = claims["account_id"]
            .as_str()
            .ok_or_else(|| problem("unauthorized"))?;
        let auth_time = claims["auth_time"].as_u64().unwrap_or(0);
        let grant = app.grants.lookup(account);
        let sections: Vec<&str> = grant
            .as_ref()
            .map(|g| g.sections.iter().map(String::as_str).collect())
            .unwrap_or_default();
        req.auth = Some(user_context(
            account,
            auth_time,
            grant.as_ref().map(|g| {
                (
                    g.storefront_id.as_str(),
                    g.role.as_str(),
                    sections.as_slice(),
                )
            }),
        ));
    } else if let Some(token) = req.header("x-guest-token").map(str::to_owned) {
        let claims = jwt::verify(&token, &app.guest_key.public_key(), now)
            .map_err(|_| problem("unauthorized"))?;
        req.auth = Some(guest_context(claims["sub"].as_str().unwrap_or_default()));
    }
    Ok(())
}

async fn dev_routes(app: &App, req: &Request, rest: &str) -> Response {
    let now = app.clock.epoch();
    let signed = |prefix: &str| -> Option<String> {
        let key = rest.strip_prefix(prefix)?.to_owned();
        let expires: u64 = req.query("expires")?.parse().ok()?;
        let signature = req.query("signature")?;
        app.dev_signer
            .clone()
            .with_prefix(prefix.trim_end_matches('/'))
            .verify(&key, expires, signature, now)
            .then_some(key)
    };
    match (req.method.as_str(), rest) {
        ("GET", "health") => Response::ok(&json!({ "status": "ok" })),
        ("PUT", r) if r.starts_with("uploads/") => {
            // O PUT direto que o S3 faria, com as mesmas conferências da assinatura.
            let Some(key) = signed("uploads/") else {
                return Response::problem(problem("forbidden"));
            };
            let digest = STANDARD.encode(Sha256::digest(&req.body));
            if req.header("x-amz-checksum-sha256") != Some(digest.as_str()) {
                return Response::problem(
                    problem("forbidden")
                        .with_message("O hash do arquivo não confere com o assinado."),
                );
            }
            let content_type = req
                .header("content-type")
                .unwrap_or("application/octet-stream")
                .to_owned();
            if app
                .quarantine
                .put(&key, req.body.clone(), &content_type)
                .await
                .is_err()
            {
                return Response::problem(problem("service-unavailable"));
            }
            // O gatilho da esteira (evento do S3, na nuvem).
            let pipeline = app.pipeline.clone();
            tokio::spawn(async move {
                if let Err(e) = pipeline.process(&key, None).await {
                    tracing::warn!(code = %e.code, "esteira de imagens falhou");
                }
            });
            Response::text(200, "")
        }
        ("GET", r) if r.starts_with("cdn/") => match app.cdn.get(&r["cdn/".len()..]).await {
            Ok(Some(o)) => file_response(
                o.content_type,
                &o.bytes,
                "public, max-age=31536000, immutable",
            ),
            _ => Response::problem(not_found()),
        },
        ("GET", r) if r.starts_with("files/") => {
            let Some(key) = signed("files/") else {
                return Response::problem(problem("forbidden"));
            };
            match app.files.get(&key).await {
                Ok(Some(o)) => file_response(o.content_type, &o.bytes, "private, no-store"),
                _ => Response::problem(not_found()),
            }
        }
        ("POST", "assistant/handoffs") => handoff(app, req).await,
        // Eventos de outro servidor de desenvolvimento (o saci-payments-dev publica
        // payments.* aqui), entregues aos consumidores como o barramento faria.
        ("POST", "events") => external_event(app, req),
        // A tokenização do PSP do sandbox (a interface chama direto, como faria com o PSP).
        ("POST", "psp/tokens") => app.payments.handle(req.clone()).await,
        ("POST", r) if r.starts_with("carrier/") => {
            // Simula a transportadora: assina o corpo e entrega ao webhook.
            let mut hook = req.clone();
            hook.path = format!("/v1/shipping/hooks/carrier/{}", &r["carrier/".len()..]);
            hook.headers.insert(
                "x-saci-signature".into(),
                saci_carrier_webhooks::sign(&app.carrier_secret, &req.body),
            );
            app.carrier_hooks.handle(hook).await
        }
        _ => Response::problem(not_found()),
    }
}

/// Publica no barramento em processo um CloudEvent vindo de outro servidor de
/// desenvolvimento. Só `payments.*` (o que o saci-payments-dev publica).
fn external_event(app: &App, req: &Request) -> Response {
    use saci_commerce_kit::store::EventSink;
    let Ok(event) = serde_json::from_slice::<saci_commerce_kit::events::Event>(&req.body) else {
        return Response::problem(saci_commerce_kit::problem::invalid(
            "event",
            "CloudEvent esperado.",
        ));
    };
    if app.payments_bridge.is_none() || !event.event_type.starts_with("payments.") {
        return Response::problem(problem("forbidden"));
    }
    app.bus.publish(vec![event]);
    Response::json(202, &json!({ "accepted": true }))
}

/// Resposta de arquivo: bytes em latin-1 dentro do `Response` de texto (ver `to_hyper`).
fn file_response(content_type: String, bytes: &[u8], cache: &str) -> Response {
    Response {
        status: 200,
        headers: vec![
            ("content-type".into(), content_type),
            ("cache-control".into(), cache.into()),
            ("x-binary".into(), "1".into()),
        ],
        body: bytes_to_latin1(bytes),
    }
}

fn bytes_to_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// Simula um repasse da Caipora para uma vitrine (`assistant.handoff.requested.v1`).
async fn handoff(app: &App, req: &Request) -> Response {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
    let Some(storefront) = body["storefront_id"].as_str() else {
        return Response::problem(saci_commerce_kit::problem::invalid(
            "storefront_id",
            "Informe a vitrine.",
        ));
    };
    let conversation = saci_ids::Id::new(saci_ids::prefix::CONVERSATION).to_string();
    let now = app.clock.rfc3339();
    let details = json!({
        "subject": body["subject"].as_str().unwrap_or("Dúvida sobre uma peça"),
        "summary": body["summary"].as_str().unwrap_or("A pessoa quer falar com a vitrine."),
        "customer": { "first_name": body["first_name"].as_str().unwrap_or("Cliente"), "city": body["city"], "state": body["state"] },
        "messages": [
            { "id": "m1", "author": "customer", "author_name": body["first_name"].as_str().unwrap_or("Cliente"), "text": body["message"].as_str().unwrap_or("Olá!"), "created_at": now },
            { "id": "m2", "author": "caipora", "text": "Vou passar para a vitrine, que responde por aqui mesmo.", "created_at": now },
        ],
    });
    if app
        .sandbox
        .register_handoff(&conversation, &details)
        .await
        .is_err()
    {
        return Response::problem(problem("service-unavailable"));
    }
    match saci_commerce_kit::events::event(
        "assistant",
        "assistant.handoff.requested.v1",
        &conversation,
        &json!({ "conversation_id": conversation, "storefront_id": storefront }),
    ) {
        Ok(e) => {
            use saci_commerce_kit::store::EventSink;
            app.bus.publish(vec![e]);
            Response::json(202, &json!({ "conversation_id": conversation }))
        }
        Err(p) => Response::problem(p),
    }
}

/// Os layouts publicados (`domains/content/layouts`), como o CDN os serve.
///
/// As imagens do layout (`layouts/media/*`) também saem daqui, e o host do CDN
/// de produção no JSON é trocado pelo deste servidor, como o publicador troca
/// pelo CDN de cada ambiente.
async fn layout(path: &str) -> Response {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../domains/content/layouts");
    let ext = |p: &str, e: &str| {
        std::path::Path::new(p)
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case(e))
    };
    if path.contains("..") || path.contains('\\') {
        return Response::problem(not_found());
    }
    if let Some(file) = path
        .strip_prefix("media/")
        .filter(|f| !f.contains('/') && ext(f, "jpg"))
    {
        return match tokio::fs::read(dir.join("media").join(file)).await {
            Ok(bytes) => file_response("image/jpeg".into(), &bytes, "public, max-age=3600"),
            Err(_) => Response::problem(not_found()),
        };
    }
    if !ext(path, "json") || path.contains('/') {
        return Response::problem(not_found());
    }
    let public = saci_commerce_kit::env_or("SACI_DEV_PUBLIC_URL", "http://localhost:8787");
    match tokio::fs::read_to_string(dir.join(path)).await {
        Ok(text) => Response::text(
            200,
            &text.replace(
                "https://cdn.saci.art.br/layouts/",
                &format!("{public}/layouts/"),
            ),
        )
        .header("content-type", "application/json")
        .header("cache-control", "public, max-age=60"),
        Err(_) => Response::problem(not_found()),
    }
}
