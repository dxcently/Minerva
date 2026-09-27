//! The listener, the gate order, and the one `match` on `(Method, path)`.
//!
//! Every refusal in hub.md §10.2 is made here, ahead of the routing `match`, so no
//! route can skip a gate — and so the order in which they fire is readable in one
//! function ([`route`]) rather than spread across handlers.
//!
//! **H1a has no route behind the token.** `/hub/...` is H1b (sessions, spawn, stop,
//! tree) and `/s/<id>/api/...` is H1c (the proxy and its SSE). What exists here is the
//! gate in front of them: an authenticated request to either gets a 404, and an
//! unauthenticated one gets a 401 before the path is even looked at.
//!
//! **No CORS header is written anywhere, and `OPTIONS` is 405.** There is nothing here
//! to preflight, and a page that holds the token does not need one; a CORS header could
//! only ever admit a page that cannot hold it. The crate's `src/` is scanned for one by
//! a test in this file, so a later route cannot quietly add one.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::header::{HeaderValue, AUTHORIZATION, HOST};
use hyper::server::conn::http1;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use hyper_util::server::graceful::GracefulShutdown;
use tokio::net::TcpListener;
use tokio::task::spawn_blocking;
use tokio_util::sync::CancellationToken;

use crate::files::{self, Static};
use crate::gate;

#[derive(Clone)]
pub struct Ctx {
    /// The bound authority: what `Host` is checked against.
    pub bound: SocketAddr,
    /// Never rendered into a response, a frame or a log line.
    pub token: Arc<str>,
    /// `--ui-dir`, when there is one.
    pub ui: Option<Static>,
}

/// Refused at the cap before the rest is buffered. Nothing legitimate to a door comes
/// near it: the door caps its own bodies at the same number.
pub const MAX_BODY: usize = 8 * 1024 * 1024;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

fn full(bytes: impl Into<Bytes>) -> BoxBody {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed()
}

/// Every refusal body is empty: the status is the whole answer, and an empty body
/// cannot leak a token, a path or a route name.
fn status(code: StatusCode) -> Response<BoxBody> {
    Response::builder().status(code).body(full(Bytes::new())).unwrap()
}

pub async fn run(ctx: Ctx, listener: TcpListener, shutdown: CancellationToken) -> Result<()> {
    let graceful = GracefulShutdown::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                // A refused connection is not the perch's end. `EMFILE`/`ENFILE` (the
                // process's fd table full, which many open SSE streams can reach) and
                // `ECONNABORTED` on some kernels are per-connection failures: a `?` here
                // would end the run, and a perch that stopped accepting is every door
                // behind it invisible. So it is logged and the loop goes round again; only
                // the cancel arm below leaves the loop.
                let (stream, _peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(e) => {
                        tracing::warn!(error = %e, "perch: accept failed; the listener stays up");
                        // The fd table is what is full; a moment's pause keeps a failing
                        // accept from spinning this thread until one frees.
                        if matches!(e.raw_os_error(), Some(libc::EMFILE) | Some(libc::ENFILE)) {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        continue;
                    }
                };
                let io = TokioIo::new(stream);
                let ctx = ctx.clone();
                let conn = http1::Builder::new().serve_connection(
                    io,
                    hyper::service::service_fn(move |req| handle(ctx.clone(), req)),
                );
                // So shutdown drains a response in flight instead of cutting it.
                let conn = graceful.watch(conn);
                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        tracing::debug!(error = %e, "perch: connection ended");
                    }
                });
            }
            _ = shutdown.cancelled() => {
                // The drain is the last step of a shutdown that H1b and H1c will
                // lengthen: this arm grows "close the watcher and pane streams, SIGTERM
                // each child, wait, SIGKILL what is left" *before* these two lines,
                // because a door holding an open `/api/events` stream ignores SIGTERM
                // for minutes and a SIGKILL leaves its token file behind. A stream is
                // closed by this same token reaching its body — `shutdown` is what every
                // stream body should hold — not by a second mechanism. The listener is
                // dropped either way, and first: stop accepting, then drain.
                drop(listener);
                graceful.shutdown().await;
                return Ok(());
            }
        }
    }
}

/// The one test the token gate and the static fall-through share, so they cannot
/// disagree about which paths are gated.
///
/// The bare `/hub` and `/s` are gated too (hub.md §3): a caller without the token
/// must not be able to tell "no such route" from "not for you", not even at the root
/// of the namespace.
fn in_gated(path: &str) -> bool {
    path == "/hub" || path.starts_with("/hub/") || path == "/s" || path.starts_with("/s/")
}

/// [`in_gated`] on the raw path *and* on the decoded one. The static route decodes once
/// before it names a file ([`files::decode`]), so `/%68ub/x` is `hub/x` to it: a gate
/// that read only the raw string would be skipped by any spelling of `/hub` or `/s`, and
/// the namespace would be reachable — and its files served — without a token. A path
/// whose escapes do not decode is refused by the static route anyway, and is judged here
/// on the raw string alone.
fn gated_path(raw: &str) -> bool {
    in_gated(raw) || files::decode(raw).is_some_and(|decoded| in_gated(&decoded))
}

/// `Authorization: Bearer` and nothing else. A query parameter is never a credential
/// here (hub.md §10.4): a cross-site page can put one in a URL, and cannot put a
/// header on a simple request. The scheme is spelled as `fetch` writes it.
fn presented(req: &Request<Incoming>) -> Option<&str> {
    req.headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn host_header(req: &Request<Incoming>) -> Option<&str> {
    req.headers().get(HOST).and_then(|h| h.to_str().ok())
}

/// The fetch-metadata header is not one of hyper's constants (it is not in
/// `hyper::header`), so it is named here once.
const SEC_FETCH_SITE: &str = "sec-fetch-site";

fn sec_fetch(req: &Request<Incoming>) -> Option<&str> {
    req.headers().get(SEC_FETCH_SITE).and_then(|h| h.to_str().ok())
}

/// The headers every response carries, and the two more that a static response
/// carries on top. Applied by path, so a refusal gets them too: a 404 for a file that
/// is not there is as much "this origin's answer" as a 200 is, and a browser that got
/// one without a policy would still be running the bundle that asked for it.
///
/// `nosniff` is what makes [`files::content_type`] the only thing a browser believes;
/// `no-referrer` keeps the URL the page was reached by from leaking onward. `no-cache`
/// is on the static bundle because a rebuilt bundle must never be served stale, and
/// the CSP is the bundle's own policy — see [`CSP`].
async fn handle(ctx: Ctx, req: Request<Incoming>) -> Result<Response<BoxBody>, Infallible> {
    // Before `route` consumes the request for its body: the same decision [`gated_path`]
    // makes for the token gate, so a path cannot be gated by one spelling and answered as
    // a static under another (`/%68ub/x` is `/hub/x` here as it is there).
    let static_route = !gated_path(req.uri().path());
    let mut response = route(ctx, req).await?;
    let headers = response.headers_mut();
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if static_route {
        headers.insert("content-security-policy", HeaderValue::from_static(CSP));
        headers.insert("cache-control", HeaderValue::from_static("no-cache"));
    }
    Ok(response)
}

async fn route(ctx: Ctx, req: Request<Incoming>) -> Result<Response<BoxBody>, Infallible> {
    // Read before anything consumes the request: gate 1 is decided on the path, and
    // nothing here parses it beyond the one decode the static route would do itself.
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let gated = gated_path(&path);
    let query = req.uri().query().unwrap_or("").to_string();

    // 1. The token, on `/hub`, `/s` and everything under them, before path parsing, so
    //    an unauthenticated caller cannot map the route table by status codes: 401 for
    //    a route that exists, 401 for one that does not. Empty body, constant time.
    if gated && !presented(&req).is_some_and(|presented| gate::token_ok(presented, &ctx.token)) {
        return Ok(status(StatusCode::UNAUTHORIZED));
    }

    // 2. DNS rebinding. On every request, statics included: the bundle is served
    //    without a token, so this is the only thing standing in front of it for a name
    //    that resolves to loopback.
    if !gate::host_ok(host_header(&req), ctx.bound) {
        return Ok(status(StatusCode::FORBIDDEN));
    }

    // 3. A page from another origin. After the token, because a cross-site page has no
    //    token and the 401 is the answer it deserves. Absent passes: curl sends none.
    if !gate::sec_fetch_ok(sec_fetch(&req)) {
        return Ok(status(StatusCode::FORBIDDEN));
    }

    // 4. Method and path shape. `OPTIONS` is 405 everywhere: no CORS preflight is ever
    //    answered, and it is checked here so a preflight to a gated path cannot learn
    //    the route table without a token — the token gate above already refused it.
    if method == Method::OPTIONS {
        let mut refusal = status(StatusCode::METHOD_NOT_ALLOWED);
        if !gated {
            refusal.headers_mut().insert("allow", HeaderValue::from_static(ALLOW));
        }
        return Ok(refusal);
    }

    //    …and the same gate's path shape, which for `/s` includes the query (hub.md
    //    §5): the door's `?token=` form is never used, and no session route takes a
    //    query string. `/hub` keeps its queries — `/hub/tree?session=<id>&path=<rel>` is
    //    one — which is H1b's to parse. A `?token=` anywhere is still nothing: gate 1
    //    read no query at all.
    if gated && path.starts_with("/s") && !query.is_empty() {
        return Ok(status(StatusCode::BAD_REQUEST));
    }

    //    …and the same gate's shape for a method: only GET and POST have a route behind
    //    the token (hub.md §3 — the proxy takes those two and nothing else), so every
    //    other gated method is a 405. It is here, ahead of the body step, on purpose: a
    //    method with no route must not be answered as though its body had been read, and
    //    an unbounded PUT must not be read at all. H1c's proxy is what reads a GET's or a
    //    POST's body, as a stream.
    if gated && method != Method::GET && method != Method::POST {
        return Ok(status(StatusCode::METHOD_NOT_ALLOWED));
    }

    // 5. The body cap, on every request behind the token (hub.md §10.2, gate 5), and read
    //    as a stream: `Limited` wraps hyper's body, so a body over the cap is refused
    //    while it arrives, and nothing over the cap is ever buffered or collected. The
    //    frames are dropped as they come — H1c's proxy is what will hand this stream to a
    //    door — so a body at the cap is never held whole here.
    //    It is its own gate rather than a route's, which is also why it is checked before
    //    the routing `match`: an 8 MiB + 1 POST to `/hub/...` must be a 413, not the 404
    //    that a missing route would otherwise answer with before the body was looked at.
    //    A 405 for a static path is still the method's answer, and no static body is
    //    read; the door's statics do the same.
    if gated {
        // `_parts` is what a rebuilt request needs, and the limited body is the stream a
        // proxy forwards; H1a has no route to give either to, so the cap is the whole of
        // what this step decides.
        let (_parts, body) = req.into_parts();
        if let Some(refusal) = capped(body).await {
            return Ok(status(refusal));
        }
    }

    // 6. Per-route. Behind the gate: nothing yet.
    if gated {
        // `/hub/...` is H1b and `/s/<id>/api/...` is H1c. 404 is the honest answer for
        // a route that does not exist, and the point of H1a is that it comes *after*
        // the 401 — an authenticated caller can tell the difference, an unauthenticated
        // one cannot.
        return Ok(status(StatusCode::NOT_FOUND));
    }

    // The static UI. No token (the page loads before it has one, so it cannot present
    // one), the `Host` check above, and GET/HEAD only.
    if method != Method::GET && method != Method::HEAD {
        let mut refusal = status(StatusCode::METHOD_NOT_ALLOWED);
        refusal.headers_mut().insert("allow", HeaderValue::from_static(ALLOW));
        return Ok(refusal);
    }
    match ctx.ui {
        Some(ui) => Ok(statics(&ui, &method, &path).await),
        // Without `--ui-dir` there is no bundle to serve, as at the door.
        None => Ok(status(StatusCode::NOT_FOUND)),
    }
}

const ALLOW: &str = "GET, HEAD";

/// The bundle's policy: its own origin for everything, inline styles because the
/// bundle draws with them, `data:` icons, and never a frame — a page that can frame
/// the perch can type into every session behind it.
const CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";

/// The cap, proven as a stream: every frame is dropped as it arrives, so a body at the cap
/// is never held whole, and the first frame past it is the refusal. `None` means the body
/// stayed within [`MAX_BODY`].
///
/// H1c's proxy holds this same `Limited` stream and hands it to a door unread; nothing in
/// H1a has a route to pass a body to, so the only question here is whether the cap holds.
async fn capped(body: Incoming) -> Option<StatusCode> {
    let mut body = Limited::new(body, MAX_BODY);
    while let Some(frame) = body.frame().await {
        match frame {
            Ok(_frame) => {}
            Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => return Some(StatusCode::PAYLOAD_TOO_LARGE),
            Err(_) => return Some(StatusCode::BAD_REQUEST),
        }
    }
    None
}

/// The filesystem calls run under `spawn_blocking`, off the reactor. The containment
/// rules are [`files::Static::resolve`]'s; this function only turns its answer into a
/// response.
async fn statics(ui: &Static, method: &Method, path: &str) -> Response<BoxBody> {
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
                    tracing::debug!(error = %e, path = %file.display(), "perch: a static file did not read");
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
    match filesystem.await {
        Ok(response) => response,
        Err(e) => {
            tracing::debug!(error = %e, "perch: a static file's blocking task did not finish");
            status(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gated_namespaces_are_the_two_the_route_table_names() {
        for gated in ["/hub", "/hub/", "/hub/sessions", "/hub/tree", "/s", "/s/", "/s/x/api/events"] {
            assert!(in_gated(gated), "{gated} is behind the token");
        }
        // A prefix is not the namespace: `/hubris` is a static path, and a route that
        // forgot this would serve the bundle's own file rules to a path meant to be
        // gated, or the reverse.
        for open in ["/", "/index.html", "/hubris", "/sx", "/assets/app.js"] {
            assert!(!in_gated(open), "{open} is not behind the token");
        }
    }

    #[test]
    fn a_percent_encoded_spelling_of_a_gated_namespace_is_still_gated() {
        // `files` decodes once before it names a file, so these reach `hub/...` and `s/...`
        // as statics: without the decode in `gated_path` an unauthenticated caller would be
        // served them (and a 404 where hub.md §3 promises a 401 for the namespace).
        for gated in ["/%68ub", "/%68ub/sessions", "/hub/%73/x", "/%73", "/%73/x/api/events", "/%73%2Fx"] {
            assert!(gated_path(gated), "{gated} is a spelling of the namespace");
        }
        // Decoding is not a widening: a path that is not the namespace either way, a
        // malformed escape (refused by the static route itself), and `..` stay static.
        for open in ["/", "/index.html", "/hubris", "/sx", "/%68ubris", "/%73x", "/a%zz", "/%2e%2e/secret.txt", "//hub/x"] {
            assert!(!gated_path(open), "{open} is not behind the token");
        }
    }

    #[test]
    fn a_bearer_header_is_the_only_credential_and_no_cors_header_is_written() {
        // `presented` reads a header off a real `Request<Incoming>`, which cannot be
        // built outside a connection: both of these are proven over a socket in
        // `tests/http.rs`. What is checked here is the shape of the source, because a
        // CORS header or a query token would be *added* in a future route's file — the
        // one thing this test must not do is match itself, so the needle is assembled
        // from pieces.
        let needle = concat!("access", "-", "control");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(&src).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap().to_ascii_lowercase();
            assert!(!text.contains(needle), "{} writes an {needle} header", path.display());
        }
    }
}
