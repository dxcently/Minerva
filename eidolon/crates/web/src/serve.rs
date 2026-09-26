//! The listener and the route table. The gates are the crate doc's; here they
//! sit ahead of the one `match`, so no route can skip one.
//!
//! This module never runs a tool or asks policy anything: intent goes through
//! `crate::driver::Handle` and `crate::user::WebUser`, never to the
//! `Dispatcher` directly (`AGENTS.md`'s one chokepoint).

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use eidolon_core::agent::Agent;
use eidolon_core::message::ContentBlock;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::header::HeaderValue;
use hyper::server::conn::http1;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use hyper_util::server::graceful::GracefulShutdown;
use tokio::net::TcpListener;
use tokio::task::spawn_blocking;
use tokio_util::sync::CancellationToken;

use crate::driver::{Handle, Say};
use crate::files::{self, Static};
use crate::user::{Answer, WebUser};

#[derive(Clone)]
pub struct Ctx {
    pub agent: Arc<Agent>,
    pub user: Arc<WebUser>,
    pub driver: Handle,
    pub bound: SocketAddr,
    pub cwd: String,
    pub yolo: bool,
    /// Never rendered into a response, a frame or a log line.
    pub token: Arc<str>,
    pub ui: Option<Static>,
}

/// Refused at the cap before the rest is buffered, as `eidolon_core::ipc`
/// does for a socket request. Nothing legitimate comes near it.
pub const MAX_BODY: usize = 8 * 1024 * 1024;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

fn full(bytes: impl Into<Bytes>) -> BoxBody {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed()
}

fn status(code: StatusCode) -> Response<BoxBody> {
    Response::builder().status(code).body(full(Bytes::new())).unwrap()
}

fn json_status(code: StatusCode, body: serde_json::Value) -> Response<BoxBody> {
    Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .body(full(body.to_string()))
        .unwrap()
}

/// DNS rebinding, and only that: `Host` is the target's authority, so a
/// hostile name resolving to loopback is refused, but a cross-site form
/// posting to `127.0.0.1` is not. Not a CSRF defence; the token is.
fn host_ok(req: &Request<Incoming>, bound: SocketAddr) -> bool {
    req.headers()
        .get(hyper::header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|host| host_matches(host, bound))
}

/// Compared in pieces, not parsed as a `SocketAddr`, so `[::1]:PORT` matches.
fn host_matches(host: &str, bound: SocketAddr) -> bool {
    let (h, p) = match host.rsplit_once(':') {
        Some((h, p)) => (h.trim_start_matches('[').trim_end_matches(']'), p),
        None => (host, ""),
    };
    h == bound.ip().to_string() && (p.is_empty() || p == bound.port().to_string())
}

/// The query form is for `EventSource`, which cannot set a header, and only on
/// the stream: a cross-site form can send a query but not a header. A header,
/// when present, is the only thing read.
fn presented<'a>(req: &'a Request<Incoming>, method: &Method, path: &str) -> Option<&'a str> {
    if let Some(value) = req.headers().get(hyper::header::AUTHORIZATION).and_then(|h| h.to_str().ok()) {
        return value.strip_prefix("Bearer ");
    }
    (method == Method::GET && path == "/api/events")
        .then(|| query_token(req.uri().query().unwrap_or("")))
        .flatten()
}

/// Not percent-decoded: a token is base64url, which needs no escaping.
fn query_token(query: &str) -> Option<&str> {
    query.split('&').find_map(|pair| pair.strip_prefix("token=")).filter(|t| !t.is_empty())
}

/// Constant time, so the token cannot be searched for byte by byte. The
/// length is public.
fn token_ok(presented: &str, want: &str) -> bool {
    use subtle::ConstantTimeEq as _;
    presented.as_bytes().ct_eq(want.as_bytes()).into()
}

pub async fn run(ctx: Ctx, listener: TcpListener, shutdown: CancellationToken) -> Result<()> {
    let graceful = GracefulShutdown::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _peer) = accepted.context("accept a connection")?;
                let io = TokioIo::new(stream);
                let ctx = ctx.clone();
                let conn = http1::Builder::new().serve_connection(
                    io,
                    hyper::service::service_fn(move |req| handle(ctx.clone(), req)),
                );
                // So shutdown drains an open stream instead of cutting a frame.
                let conn = graceful.watch(conn);
                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        tracing::debug!(error = %e, "web: connection ended");
                    }
                });
            }
            _ = shutdown.cancelled() => {
                drop(listener);
                graceful.shutdown().await;
                return Ok(());
            }
        }
    }
}

/// The one test the token gate and the static fall-through share, so they
/// cannot disagree.
fn in_api(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// `/api` headers in one place, so no route forgets them. `no-referrer` keeps
/// a stream URL carrying `?token=` from leaking to whatever the page links to.
async fn handle(ctx: Ctx, req: Request<Incoming>) -> Result<Response<BoxBody>, Infallible> {
    let api = in_api(req.uri().path());
    let mut response = route(ctx, req).await?;
    if api {
        let headers = response.headers_mut();
        headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
        headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    }
    Ok(response)
}

async fn route(ctx: Ctx, req: Request<Incoming>) -> Result<Response<BoxBody>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    // First, and for unserved `/api` paths too, so the route table cannot be
    // learned without the token.
    if in_api(&path) && !presented(&req, &method, &path).is_some_and(|t| token_ok(t, &ctx.token)) {
        return Ok(status(StatusCode::UNAUTHORIZED));
    }

    if method == Method::POST && !host_ok(&req, ctx.bound) {
        return Ok(json_status(
            StatusCode::FORBIDDEN,
            serde_json::json!({"error": "Host does not match the bound address"}),
        ));
    }

    match (&method, path.as_str()) {
        (&Method::GET, "/api/events") => {
            let body = crate::stream::body(
                ctx.agent.clone(),
                ctx.user.clone(),
                ctx.agent.bus().clone(),
                ctx.driver.clone(),
                ctx.cwd.clone(),
                ctx.yolo,
            );
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-store")
                .header("connection", "keep-alive")
                .body(body.boxed())
                .unwrap())
        }
        (&Method::POST, "/api/say") => post_say(ctx, req).await,
        (&Method::POST, "/api/answer") => post_answer(ctx, req).await,
        (&Method::POST, "/api/cancel") => {
            if ctx.driver.cancel().await {
                Ok(status(StatusCode::NO_CONTENT))
            } else {
                Ok(json_status(StatusCode::CONFLICT, serde_json::json!({"error": "no turn is running"})))
            }
        }
        _ => match ctx.ui.as_ref().filter(|_| !in_api(&path)) {
            Some(ui) => Ok(statics(ui, &method, &path, ctx.bound, &req).await),
            None => Ok(status(StatusCode::NOT_FOUND)),
        },
    }
}

/// `Host`-checked, since the door's own files are what a rebinding name is
/// after; no token, since the page loads before it has one. The filesystem
/// calls run under `spawn_blocking`, off the reactor.
async fn statics(ui: &Static, method: &Method, path: &str, bound: SocketAddr, req: &Request<Incoming>) -> Response<BoxBody> {
    if !host_ok(req, bound) {
        let refusal = json_status(StatusCode::FORBIDDEN, serde_json::json!({"error": "Host does not match the bound address"}));
        return static_headers(refusal);
    }
    if *method != Method::GET && *method != Method::HEAD {
        let mut refusal = status(StatusCode::METHOD_NOT_ALLOWED);
        refusal.headers_mut().insert("allow", HeaderValue::from_static(ALLOW));
        return static_headers(refusal);
    }
    let head = *method == Method::HEAD;
    let ui = ui.clone();
    let path = path.to_string();
    let filesystem = spawn_blocking(move || {
        let (file, len) = match ui.resolve(&path) {
            Ok(found) => found,
            Err(code) => return status(code),
        };
        let body = if head {
            full(Bytes::new())
        } else {
            match files::read_capped(&file) {
                Ok(bytes) => full(bytes),
                Err(e) => {
                    tracing::debug!(error = %e, path = %file.display(), "web: a static file did not read");
                    return status(StatusCode::NOT_FOUND);
                }
            }
        };
        let mut response = Response::builder()
            .status(StatusCode::OK)
            .header("content-type", files::content_type(&file))
            .body(body)
            .unwrap();
        if head {
            response.headers_mut().insert("content-length", HeaderValue::from(len));
        }
        response
    });
    let response = match filesystem.await {
        Ok(response) => response,
        Err(e) => {
            tracing::debug!(error = %e, "web: a static file's blocking task did not finish");
            status(StatusCode::INTERNAL_SERVER_ERROR)
        }
    };
    static_headers(response)
}

const ALLOW: &str = "GET, HEAD";

/// Bundles inline icons as `data:` and CSS as inline styles, but never a
/// script. `frame-ancestors 'none'`: a door a page can frame is one it can type into.
const CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";

/// On refusals as well as 200s. `no-cache` because a rebuilt bundle must not be stale.
fn static_headers(mut response: Response<BoxBody>) -> Response<BoxBody> {
    let headers = response.headers_mut();
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert("content-security-policy", HeaderValue::from_static(CSP));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("cache-control", HeaderValue::from_static("no-cache"));
    response
}

/// `mode` defaults to `send` ([`Say`]).
#[derive(serde::Deserialize)]
struct SayBody {
    text: String,
    #[serde(default)]
    mode: Option<String>,
}

/// The refusal is a pair, not a `Response`, to keep the `Err` small.
async fn json_body(req: Request<Incoming>) -> Result<Bytes, (StatusCode, String)> {
    match Limited::new(req.into_body(), MAX_BODY).collect().await {
        Ok(body) => Ok(body.to_bytes()),
        Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => {
            Err((StatusCode::PAYLOAD_TOO_LARGE, format!("a body over {MAX_BODY} bytes is not read")))
        }
        Err(_) => Err((StatusCode::BAD_REQUEST, "the body did not read".to_string())),
    }
}

async fn post_say(ctx: Ctx, req: Request<Incoming>) -> Result<Response<BoxBody>, Infallible> {
    let bytes = match json_body(req).await {
        Ok(bytes) => bytes,
        Err((code, why)) => return Ok(json_status(code, serde_json::json!({"error": why}))),
    };
    let Ok(SayBody { text, mode }) = serde_json::from_slice::<SayBody>(&bytes) else {
        return Ok(json_status(StatusCode::BAD_REQUEST, serde_json::json!({"error": "bad request body"})));
    };
    let blocks = vec![ContentBlock::text(text)];
    let say = match mode.as_deref().unwrap_or("send") {
        "steer" => Say::Steer(blocks),
        "send" => Say::Send(blocks),
        other => {
            return Ok(json_status(
                StatusCode::BAD_REQUEST,
                serde_json::json!({"error": format!("unknown mode {other:?}")}),
            ));
        }
    };
    let queued = ctx.driver.say(say).await;
    Ok(json_status(StatusCode::ACCEPTED, serde_json::json!({"queued": queued})))
}

#[derive(serde::Deserialize)]
struct AnswerBody {
    ask_id: u64,
    answer: String,
}

async fn post_answer(ctx: Ctx, req: Request<Incoming>) -> Result<Response<BoxBody>, Infallible> {
    let bytes = match json_body(req).await {
        Ok(bytes) => bytes,
        Err((code, why)) => return Ok(json_status(code, serde_json::json!({"error": why}))),
    };
    let Ok(body) = serde_json::from_slice::<AnswerBody>(&bytes) else {
        return Ok(json_status(StatusCode::BAD_REQUEST, serde_json::json!({"error": "bad request body"})));
    };
    match ctx.user.answer(body.ask_id, &body.answer) {
        Answer::Accepted => Ok(status(StatusCode::NO_CONTENT)),
        Answer::Unknown => Ok(json_status(
            StatusCode::CONFLICT,
            serde_json::json!({"error": "that ask is no longer pending"}),
        )),
        Answer::Rejected => Ok(json_status(
            StatusCode::UNPROCESSABLE_ENTITY,
            serde_json::json!({"error": "that ask does not offer that answer"}),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn host_header_pieces_match() {
        let bound = addr("127.0.0.1:4477");
        assert!(host_matches("127.0.0.1:4477", bound));
        assert!(host_matches("127.0.0.1", bound));
        assert!(host_matches("[::1]:4477", addr("[::1]:4477")));
        assert!(!host_matches("127.0.0.1:4478", bound));
    }

    #[test]
    fn host_header_mismatch_is_caught_by_the_same_comparison() {
        let bound = addr("127.0.0.1:4477");
        assert!(!host_matches("evil.example:4477", bound));
    }

    #[test]
    fn the_token_comparison_answers_yes_and_no() {
        assert!(token_ok("abc", "abc"));
        assert!(!token_ok("abc", "abd"));
        assert!(!token_ok("abc", "abcd"), "a prefix of the token is not the token");
        assert!(!token_ok("", "abc"));
        assert!(!token_ok("abc", ""));
    }

    #[test]
    fn the_query_token_parameter_is_read_and_nothing_else_is() {
        assert_eq!(query_token("token=abc"), Some("abc"));
        assert_eq!(query_token("a=1&token=abc&b=2"), Some("abc"));
        assert_eq!(query_token("xtoken=abc"), None);
        assert_eq!(query_token("token="), None, "an empty parameter carries nothing to compare");
        assert_eq!(query_token(""), None);
    }
}
