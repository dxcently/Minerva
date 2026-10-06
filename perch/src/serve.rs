//! The listener, the gate order, and the one `match` on `(Method, path)`.
//!
//! Every refusal in hub.md §10.2 is made here, ahead of the routing `match`, so no
//! route can skip a gate — and so the order in which they fire is readable in one
//! function ([`route`]) rather than spread across handlers.
//!
//! **H1b is the session lifecycle behind the token**: `GET`/`POST /hub/sessions` and
//! `POST /hub/sessions/<id>/{resume,stop}` (hub.md §3, §4). **H1c is the proxy**: everything
//! under `/s` is handed to [`proxy::handle`] with its body still unread (hub.md §5), after the
//! same gates. `GET /hub/tree` is H2's directory listing ([`tree`]). `/hub/events` and
//! `/hub/mesh` are H2/H3: 404s here, after
//! the same gates, so an authenticated caller can tell "not built yet" from "not for you" on
//! every one of them.
//!
//! **The shutdown arm closes streams before it signals children** (hub.md §2 "Shutdown",
//! §4 "Stop"): the watcher the perch holds to each door ends first, because a door with an
//! open SSE stream runs hyper's `GracefulShutdown` and ignores SIGTERM for minutes. The one
//! `CancellationToken` from H1a is what both a stop and a shutdown hang off.
//!
//! **No CORS header is written anywhere, and `OPTIONS` is 405.** There is nothing here
//! to preflight, and a page that holds the token does not need one; a CORS header could
//! only ever admit a page that cannot hold it. The crate's `src/` is scanned for one by
//! a test in this file, so a later route cannot quietly add one.

use std::convert::Infallible;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
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
use crate::proxy;
use crate::sessions::{self, ExitWatch, Registry, TreeRoot};
use crate::spawn::{self, Spawner};
use crate::tree;
use crate::watch::{self, Watcher};

#[derive(Clone)]
pub struct Ctx {
    /// The bound authority: what `Host` is checked against.
    pub bound: SocketAddr,
    /// Never rendered into a response, a frame or a log line.
    pub token: Arc<str>,
    /// `--ui-dir`, when there is one.
    pub ui: Option<Static>,
    /// The doors this perch owns, and the paths every spawn is judged against (H1b).
    pub registry: Arc<Registry>,
    /// The one thread that forks doors, so `PR_SET_PDEATHSIG` fires when the perch dies
    /// and not when a pool thread goes idle (hub.md §2).
    pub spawner: Arc<Spawner>,
    /// At most [`tree::MAX_LISTINGS`] directory listings at once; a fifth is a 503.
    pub tree_slots: tree::Slots,
    /// Login codes and the session secrets they are traded for (hub.md section 10.5).
    pub logins: Arc<crate::login::Logins>,
}

/// Refused at the cap before the rest is buffered. The door caps its own bodies at the same
/// number (its `MAX_BODY`), and a `say` may legitimately carry up to 8 MiB of base64 images, so
/// this is never stricter than the door: a body of exactly this size reaches it.
pub const MAX_BODY: usize = 8 * 1024 * 1024;

/// `Unsync` because the proxy's body holds a future, which is `Send` and not `Sync`; hyper's
/// server needs the former only.
pub(crate) type BoxBody = http_body_util::combinators::UnsyncBoxBody<Bytes, Infallible>;

fn full(bytes: impl Into<Bytes>) -> BoxBody {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed_unsync()
}

/// Every refusal body is empty: the status is the whole answer, and an empty body
/// cannot leak a token, a path or a route name.
pub(crate) fn status(code: StatusCode) -> Response<BoxBody> {
    Response::builder().status(code).body(full(Bytes::new())).unwrap()
}

/// How long a shutdown waits for responses in flight to the pages, after every door is stopped.
const RESPONSE_DRAIN: Duration = Duration::from_secs(3);

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
                // Stop accepting first, then the Stop finding for every door this perch
                // owns, then the drain. The order matters and is hub.md §2's "Shutdown" row:
                // the watcher stream to each door is closed *before* the child is signalled,
                // because a door holding an open `/api/events` stream is running hyper's
                // `GracefulShutdown` and ignores SIGTERM until that stream ends — which a
                // SIGKILL then cuts short, leaving a `0600` token file behind. It all
                // happens ahead of `lib.rs` removing the perch's own token file, so a door
                // can never outlive the token that let the page reach it.
                drop(listener);
                ctx.registry.stop_all().await;
                // Bounded, because the drain waits for every response hyper is still writing, and
                // hyper never finishes writing to a page that has stopped reading (a frozen tab
                // on a pane). The doors are already stopped by now, so what is left is only
                // pages' own connections, and they are cut when the process ends. The bound is
                // longer than a pane's own drain (`PANE_DRAIN`, 2 s) so an ordinary response gets
                // its moment, and far shorter than a door's (10 s). Returning, even on the
                // timeout, is what lets `lib.rs` remove the perch's token file.
                if tokio::time::timeout(RESPONSE_DRAIN, graceful.shutdown()).await.is_err() {
                    tracing::warn!("perch: a connection did not finish in time at shutdown; leaving it");
                }
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

/// The open login route and the largest body it will read: a code is 43 characters.
const REDEEM: &str = "/hub/redeem";
const REDEEM_BODY: usize = 1024;

/// The cookie's name: the port is in it because cookies are not isolated by port, so two perches
/// on one host (the operator's and a test's) would otherwise overwrite each other's login.
fn cookie_name(bound: SocketAddr) -> String {
    format!("minerva_{}", bound.port())
}

/// The token as a bearer header: the launcher, the `#token=` path, curl. Constant time.
fn bearer_ok(ctx: &Ctx, req: &Request<Incoming>) -> bool {
    presented(req).is_some_and(|presented| gate::token_ok(presented, &ctx.token))
}

/// The credential, from either place a browser can hold it: the token as a bearer header, or the
/// session cookie [`redeem`] set. Both are constant time; neither is read from a query or a body.
///
/// The bearer keeps gate 3's "absent `Sec-Fetch-Site` passes" (curl sends none, and a page cannot
/// attach the header). The cookie is attached by the browser itself, and a browser without fetch
/// metadata on another port of this host is same-site, so the cookie counts only on a request that
/// says it is `same-origin`: the page's own `fetch` and `EventSource` always do.
fn authenticated(ctx: &Ctx, req: &Request<Incoming>) -> bool {
    if bearer_ok(ctx, req) {
        return true;
    }
    if sec_fetch(req) != Some("same-origin") {
        return false;
    }
    let name = cookie_name(ctx.bound);
    req.headers()
        .get_all(hyper::header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .filter_map(|header| crate::login::cookie_value(header, &name))
        .fold(false, |ok, value| ok | ctx.logins.session_ok(value))
}

/// `POST /hub/redeem {"code": "..."}`: a valid code is consumed and answered with a session
/// cookie (204); anything else, a malformed body included, is the same empty 403.
///
/// `HttpOnly` keeps it from page script, `SameSite=Strict` from cross-site requests, and there
/// is no `Secure` because the origin is plain-http loopback. No `Max-Age`: it ends with the
/// browser session, and with the perch, whose restart forgets every secret.
fn redeem(ctx: &Ctx, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize)]
    struct Asked {
        code: String,
    }
    let secret = serde_json::from_slice::<Asked>(body).ok().and_then(|asked| ctx.logins.redeem(&asked.code));
    let Some(secret) = secret else { return status(StatusCode::FORBIDDEN) };
    let cookie = format!("{}={secret}; HttpOnly; SameSite=Strict; Path=/", cookie_name(ctx.bound));
    let mut value = HeaderValue::from_str(&cookie).expect("a base64url secret is a header value");
    value.set_sensitive(true);
    let mut answer = status(StatusCode::NO_CONTENT);
    answer.headers_mut().insert(hyper::header::SET_COOKIE, value);
    answer
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
    } else {
        // What the token or the cookie opens answers JSON and event streams, never a page: a
        // same-origin navigation to one (the cookie now authenticates it) renders nothing.
        headers.insert("content-security-policy", HeaderValue::from_static(GATED_CSP));
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
    //    The one route left open is `POST /hub/redeem`: it is how a browser gets a credential at
    //    all. Its other gates (host, fetch metadata, a tiny body) still apply, and any other
    //    method on that path is an ordinary gated 401. The credential is the token as a bearer
    //    header or the session cookie that redeem set.
    let redeem = method == Method::POST && path == REDEEM;
    if gated && !redeem && !authenticated(&ctx, &req) {
        return Ok(status(StatusCode::UNAUTHORIZED));
    }
    // `POST /hub/code` is the launcher's: the bearer token only, never the cookie (below).
    let bearer = bearer_ok(&ctx, &req);

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
    //    an unbounded PUT must not be read at all. The proxy is what reads a POST's body,
    //    as a stream.
    if gated && method != Method::GET && method != Method::POST {
        return Ok(status(StatusCode::METHOD_NOT_ALLOWED));
    }

    // 5. The body cap, on every request behind the token (hub.md §10.2, gate 5): the body
    //    goes through `Limited`, so the first frame past the cap is refused while it
    //    arrives and nothing over the cap is ever held.
    //    It is its own gate rather than a route's, which is also why it is checked before
    //    the routing `match`: an 8 MiB + 1 POST to `/hub/...` must be a 413, not the 404
    //    that a missing route would otherwise answer with before the body was looked at.
    //    The two namespaces read their bodies differently, so they branch here, before any
    //    body is read: `/hub` bodies are small JSON that a route parses, so they are collected
    //    (below); `/s` bodies are a door's, and a `say` may carry several MiB of images, so
    //    they are handed to the proxy unread and streamed through their own `Limited`.
    //    A 405 for a static path is still the method's answer, and no static body is
    //    read; the door's statics do the same.
    if gated && (path == "/s" || path.starts_with("/s/")) {
        return Ok(proxy::handle(&ctx, req).await);
    }
    if gated {
        let (_parts, body) = req.into_parts();
        // The open route is capped tight: nothing unauthenticated gets to make the perch hold 8 MiB.
        let body = match capped(body, if redeem { REDEEM_BODY } else { MAX_BODY }).await {
            Ok(body) => body,
            Err(refusal) => return Ok(status(refusal)),
        };
        // 6. Per-route. The `/hub` routes are H1b's and `/hub/tree`; `/hub/events` and
        //    `/hub/mesh` are H2/H3, so they are the 404 of a route that does not exist —
        //    which is what makes a 401 or a 403 the only thing an unauthenticated caller can
        //    tell apart. (`/s` never gets here: it is the proxy's, above. What does is a
        //    percent-encoded spelling of either namespace, which is a 404.)
        return Ok(hub(&ctx, &method, &path, &query, &body, bearer).await);
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

/// On every answer behind the gate: no script, no frame, nothing loaded.
const GATED_CSP: &str = "default-src 'none'; sandbox";

/// The bundle's policy: its own origin for everything, inline styles because the
/// bundle draws with them, `data:` icons, and never a frame — a page that can frame
/// the perch can type into every session behind it.
const CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";

/// The cap, proven at the cap: `Limited` refuses the first frame past [`MAX_BODY`], so a
/// body over it is never held whole. `Err` is the refusal to answer with.
///
/// For `/hub` bodies only: H1b's routes read a small JSON body out of what is collected here.
/// The proxy (`/s`) never calls this. It builds its own `Limited` over the unread body and
/// streams it to the door, because collecting a `say`'s images first would hold the whole body
/// in memory and the door waiting (H1b review finding 3).
async fn capped(body: Incoming, cap: usize) -> std::result::Result<Bytes, StatusCode> {
    match Limited::new(body, cap).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => Err(StatusCode::PAYLOAD_TOO_LARGE),
        Err(_) => Err(StatusCode::BAD_REQUEST),
    }
}

// ---------------------------------------------------------------------------
// H1b: the session lifecycle (hub.md §3, §4)
// ---------------------------------------------------------------------------

/// The `/hub/...` routes. `/hub/sessions` is the list and the spawn; its two verbs are per
/// session, and `/hub/tree` is H2's one directory listing. Everything else behind the token
/// is a 404: `/hub/events` and `/hub/mesh` are H2/H3.
async fn hub(ctx: &Ctx, method: &Method, path: &str, query: &str, body: &Bytes, bearer: bool) -> Response<BoxBody> {
    match (method.as_str(), path) {
        ("GET", "/hub/sessions") => json(StatusCode::OK, &ctx.registry.list()),
        ("POST", "/hub/sessions") => new_session(ctx, body).await,
        // Token only: a script in the page's origin cannot read the HttpOnly cookie, and must not be
        // able to turn it into a code that a second client redeems for a session of its own.
        ("POST", "/hub/code") if !bearer => status(StatusCode::FORBIDDEN),
        ("POST", "/hub/code") => json(StatusCode::OK, &serde_json::json!({ "code": ctx.logins.mint_code() })),
        ("POST", REDEEM) => redeem(ctx, body),
        ("GET", "/hub/tree") => tree(ctx, query).await,
        ("POST", _) => match session_verb(path, query) {
            Some((id, Verb::Resume)) => resume(ctx, id).await,
            Some((id, Verb::Stop)) => stop(ctx, id).await,
            None => status(StatusCode::NOT_FOUND),
        },
        _ => status(StatusCode::NOT_FOUND),
    }
}

/// `GET /hub/tree?session=<id>&path=<rel>` (hub.md section 7): the query is parsed in
/// [`tree::parse_query`] (400), the session must be a live door of ours (404 unknown, 409
/// `{"state"}` otherwise), and everything about the path is [`tree::list`]'s one empty 404.
/// The listing runs under `spawn_blocking`, holding one of [`tree::MAX_LISTINGS`] slots (a full house
/// is a 503), and its root is re-checked against `--root` there.
async fn tree(ctx: &Ctx, query: &str) -> Response<BoxBody> {
    let Some((id, rel)) = tree::parse_query(query) else { return status(StatusCode::BAD_REQUEST) };
    if !sessions::id_ok(&id) {
        return status(StatusCode::NOT_FOUND);
    }
    let root = match ctx.registry.tree_root(&id) {
        TreeRoot::Live(root) => root,
        TreeRoot::Not(state) => return json(StatusCode::CONFLICT, &serde_json::json!({ "state": state })),
        // Kept and live-elsewhere rows are not served (Q9): they get their state, and an id
        // that is no row at all is a 404.
        TreeRoot::NotOurs => {
            return match ctx.registry.list().into_iter().find(|row| row.id == id) {
                Some(row) => json(StatusCode::CONFLICT, &serde_json::json!({ "state": row.state })),
                None => status(StatusCode::NOT_FOUND),
            };
        }
    };
    // A slot before any work, and before the path is looked at, so a full house answers the same
    // whatever the path is. The permit travels into the blocking task and is dropped when the
    // listing is done, not when the client has gone.
    let Some(permit) = ctx.tree_slots.take() else {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "too many directory listings in flight; retry");
    };
    let registry = ctx.registry.clone();
    let listed = tokio::task::spawn_blocking(move || {
        let _slot = permit;
        // The cwd is judged against `--root` now, whoever stored it: a resumed session's cwd is
        // what its log said, and the tree is not for `/`.
        let (root, floor) = registry.tree_cwd(&root)?;
        tree::list(&root, &floor, &rel)
    });
    match listed.await {
        Ok(Some(listing)) => json(StatusCode::OK, &listing),
        _ => status(StatusCode::NOT_FOUND),
    }
}

enum Verb {
    Resume,
    Stop,
}

/// `<id>/resume` and `<id>/stop`, and the id is handed back exactly as it was spelled: it is
/// checked by [`sessions::id_ok`] and then resolved only as `<sessions dir>/<id>.eid` (§4).
/// A query string is not part of either route (`/hub/sessions` itself ignores one), and a
/// path that is not exactly one of these is a 404 — never a 400, so a bad id cannot be told
/// from a route that does not exist.
fn session_verb<'a>(path: &'a str, query: &str) -> Option<(&'a str, Verb)> {
    if !query.is_empty() {
        return None;
    }
    let (id, verb) = path.strip_prefix("/hub/sessions/")?.split_once('/')?;
    Some((
        id,
        match verb {
            "resume" => Verb::Resume,
            "stop" => Verb::Stop,
            _ => return None,
        },
    ))
}

/// §4's spawn sequence, steps 1-7, for a **new** session: `{"cwd": "<dir>", "model": "<m>"?}`.
///
/// The 201 carries the id the door named in `hello` (§4 step 7) — a new session's log is
/// named by the door, so this request does not answer until the door has said which log it
/// opened. A door that never says is a 503, and the child is stopped first.
async fn new_session(ctx: &Ctx, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize)]
    struct Asked {
        cwd: Option<String>,
        model: Option<String>,
    }
    const SHAPE: &str = "expected a JSON body of {\"cwd\": \"<dir>\", \"model\": \"<m>\"?}";
    let Ok(asked) = serde_json::from_slice::<Asked>(body) else {
        return problem(StatusCode::BAD_REQUEST, SHAPE);
    };
    // No cwd is the Minerva home's scratch dir, when the perch has a home; without one it is the
    // refusal it always was: a body that names no cwd is a 400.
    let wanted = match (&asked.cwd, &ctx.registry.scratch) {
        (Some(cwd), _) => PathBuf::from(cwd),
        (None, Some(scratch)) => match fresh_scratch(scratch) {
            Ok(dir) => dir,
            Err(e) => {
                tracing::warn!(error = %e, path = %scratch.display(), "perch: a scratch dir could not be created");
                return problem(StatusCode::SERVICE_UNAVAILABLE, "a scratch directory could not be created under the Minerva home");
            }
        },
        (None, None) => return problem(StatusCode::BAD_REQUEST, SHAPE),
    };
    // §4 step 1, and the reason the perch token is not a licence to start an agent in `/`.
    let cwd = match ctx.registry.cwd_under_a_root(&wanted) {
        Ok(cwd) => cwd,
        Err(_) => return problem(StatusCode::UNPROCESSABLE_ENTITY, "cwd is not a directory under a --root"),
    };
    // §4 step 3: the door's own argv, with no token in it and no `--ui-dir`.
    let argv = spawn::argv(&ctx.registry.eidolon, ctx.registry.eidolon_config.as_deref(), spawn::Target::New(&cwd), asked.model.as_deref());
    let up = match door_up(ctx, argv, None).await {
        Ok(up) => up,
        Err(why) => return problem(StatusCode::SERVICE_UNAVAILABLE, &why),
    };

    let Up { pid, port, token, exit, watcher, hello } = up;
    // A door whose log is not in our sessions dir names no session this perch can list or
    // resume, so it is not adopted.
    let Some(id) = sessions::id_from_log(&ctx.registry.sessions_dir, &hello.session) else {
        let why = format!("the door's session log {} is not in the sessions dir", hello.session.display());
        // No row was ever written for this door, so the reap task's `mark_exited` finds nothing
        // by pid and removes nothing: the signal path owns the file here (H1b review finding 1).
        give_up(&ctx.registry, watcher, pid, port, &token, exit).await;
        return problem(StatusCode::SERVICE_UNAVAILABLE, &why);
    };
    // The cwd that was checked against `--root` at step 1, not the one `hello` names: they are the
    // same directory from an honest door, and the row must hold the checked one.
    ctx.registry.insert(id.clone(), hello.session, cwd, pid, port, token, watcher, exit);
    json(StatusCode::CREATED, &serde_json::json!({ "id": id }))
}

/// A new directory for one folderless chat: `<scratch>/<YYYYMMDD-HHMMSS>`, then `-2`, `-3`, ... when that is
/// taken. `create_dir` (not `create_dir_all`) is what makes the name this chat's alone, as eidolon's own
/// `create_session` does for a log.
fn fresh_scratch(scratch: &std::path::Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(scratch)?;
    let stamp = utc_stamp(std::time::SystemTime::now());
    for n in 1..1000 {
        let dir = scratch.join(if n == 1 { stamp.clone() } else { format!("{stamp}-{n}") });
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free scratch name"))
}

/// `YYYYMMDD-HHMMSS` in UTC (days-to-civil, Hinnant's algorithm): a name, not a clock, so no time crate.
fn utc_stamp(at: std::time::SystemTime) -> String {
    let secs = at.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// §4 step 2's "Resume can spend" is a UI concern and not this route's: the perch never
/// auto-restarts a door, so a door opens this log only because someone asked for exactly
/// that, here.
async fn resume(ctx: &Ctx, id: &str) -> Response<BoxBody> {
    if !sessions::id_ok(id) {
        return status(StatusCode::NOT_FOUND);
    }
    let log = sessions::log_path(&ctx.registry.sessions_dir, id);
    // The log has to be a real log *inside* the sessions dir: the id was already checked, and
    // this is what keeps a symlink at `<id>.eid` from naming a file somewhere else.
    if !log.is_file() || sessions::id_from_log(&ctx.registry.sessions_dir, &log).as_deref() != Some(id) {
        return status(StatusCode::NOT_FOUND);
    }
    // §4 step 2, both halves: our own children, and the roster — upstream has no `flock`, so
    // two writers on one log is a corruption the perch can at least refuse to start.
    if ctx.registry.held(&log) || ctx.registry.roster_holds(&log) {
        return problem(StatusCode::CONFLICT, "that log is already held by a live session");
    }
    // Reserved before the fork, so a second resume in flight cannot pass the check above too.
    if !ctx.registry.reserve(id, &log) {
        return problem(StatusCode::CONFLICT, "that log is already held by a live session");
    }
    let argv = spawn::argv(&ctx.registry.eidolon, ctx.registry.eidolon_config.as_deref(), spawn::Target::Kept(&log), None);
    let up = match door_up(ctx, argv, Some(id)).await {
        Ok(up) => up,
        Err(why) => return problem(StatusCode::SERVICE_UNAVAILABLE, &why),
    };
    // The door must have opened the log we asked it to. A door that opened another one is
    // exactly the two-writers case above, wearing a hat.
    if sessions::id_from_log(&ctx.registry.sessions_dir, &up.hello.session).as_deref() != Some(id) {
        let why = format!("the door opened {}, not the log we asked for", up.hello.session.display());
        let Up { pid, port, token, exit, watcher, .. } = up;
        ctx.registry.dead(id, pid);
        give_up(&ctx.registry, watcher, pid, port, &token, exit).await;
        return problem(StatusCode::SERVICE_UNAVAILABLE, &why);
    }
    // The cwd the log named is kept as the checked path when it is under a `--root`; when it is
    // not, it is kept as given (the list still shows it) and `/hub/tree` refuses it, because the
    // tree re-checks its root against `--root` on every request.
    let cwd = ctx.registry.cwd_under_a_root(&up.hello.cwd).unwrap_or_else(|_| up.hello.cwd.clone());
    ctx.registry.mark_live(id, &up.hello.session, &cwd);
    json(StatusCode::OK, &serde_json::json!({ "id": id }))
}

/// `/hub/sessions/<id>/stop`: 202 once the door has been signalled, and the signals are
/// [`sessions::Registry::stop`]'s business — the watcher is closed first, because a door with
/// an open stream does not die on SIGTERM.
async fn stop(ctx: &Ctx, id: &str) -> Response<BoxBody> {
    if !sessions::id_ok(id) {
        return status(StatusCode::NOT_FOUND);
    }
    match ctx.registry.stop(id).await {
        sessions::Stop::Signalled => status(StatusCode::ACCEPTED),
        sessions::Stop::NotLive => problem(StatusCode::CONFLICT, "that session is not live under this perch"),
    }
}

/// A door that reached `live` inside a spawn request, whole. [`Ctx::registry`] takes these
/// one at a time: see the two callers for why a new session's row is written last and a
/// resume's as soon as each fact is known.
struct Up {
    pid: u32,
    port: u16,
    token: Arc<str>,
    exit: ExitWatch,
    watcher: Watcher,
    hello: watch::Hello,
}

/// §4 steps 3-7 for one door, new or resumed — the two differ only in the argv they are
/// given. `id` is `Some` for a resume, which is the only case where the perch knows the
/// session before the door says it; every failure leaves that row `dead` rather than
/// half-`starting`.
async fn door_up(ctx: &Ctx, argv: Vec<OsString>, id: Option<&str>) -> std::result::Result<Up, String> {
    let mut forked = match spawn::fork_door(&ctx.spawner, &ctx.registry, argv).await {
        Ok(forked) => forked,
        Err(e) => {
            dead(ctx, id, 0);
            return Err(format!("{e:#}"));
        }
    };
    let (pid, exit) = (forked.pid, forked.exit.clone());
    if let Some(id) = id {
        ctx.registry.attach(id, pid, exit.clone());
    }
    // Steps 5 and 6: the two boot lines, then the token file. A door that never prints them,
    // or whose token file is not an owner-only regular file where it should be, is stopped
    // before this returns.
    let booted = match spawn::boot(&mut forked, &ctx.registry.runtime_dir).await {
        Ok(booted) => booted,
        Err(e) => {
            dead(ctx, id, pid);
            return Err(format!("{e:#}"));
        }
    };
    if let Some(id) = id {
        ctx.registry.ready(id, booted.port, booted.token.clone());
    }
    // Step 7: the watcher's `hello`. Opening it is also what closes it on this path.
    match watch::open(booted.port, booted.token.clone(), spawn::HELLO_TIMEOUT).await {
        Ok((watcher, hello)) => Ok(Up { pid, port: booted.port, token: booted.token, exit, watcher, hello }),
        Err(e) => {
            sessions::signal_and_wait(pid, exit, spawn::GIVE_UP_GRACE).await;
            // As in `give_up`: the signal path owns the file, and a new session has no row to find it by.
            sessions::drop_token_file(&spawn::door_token_path(&ctx.registry.runtime_dir, booted.port), &booted.token);
            dead(ctx, id, pid);
            Err(format!("{e:#}"))
        }
    }
}

/// A spawn that will not be adopted: the watcher is closed first, then the door is stopped —
/// the same order as any stop, and for the same reason.
///
/// The door's token file is removed last. The path that signals a door owns its file (H1b
/// review findings 1 and 2): a door that ignores SIGTERM is SIGKILLed here and never runs its
/// own `remove_file`, and on the new-session path there is no registry row for the reap task to
/// find it by. Only a file still holding this door's token is removed.
async fn give_up(registry: &Registry, watcher: Watcher, pid: u32, port: u16, token: &str, exit: ExitWatch) {
    watcher.close().await;
    sessions::signal_and_wait(pid, exit, spawn::GIVE_UP_GRACE).await;
    sessions::drop_token_file(&spawn::door_token_path(&registry.runtime_dir, port), token);
}

/// Marks the row of the door `pid` (0: the fork never happened) as `dead`; a row that has
/// since been taken over by a newer door of the same id is left alone.
fn dead(ctx: &Ctx, id: Option<&str>, pid: u32) {
    if let Some(id) = id {
        ctx.registry.dead(id, pid);
    }
}

/// Every JSON answer this crate writes, so the content type is decided in one place. The
/// body is never a secret: `Entry` has no token field, and a spawn answer is an id.
pub(crate) fn json<T: serde::Serialize>(code: StatusCode, value: &T) -> Response<BoxBody> {
    let body = match serde_json::to_vec(value) {
        Ok(body) => body,
        // Serializing our own types cannot fail; an empty object is the honest answer rather
        // than a panic in a request handler.
        Err(e) => {
            tracing::warn!(error = %e, "perch: an answer did not serialize");
            b"{}".to_vec()
        }
    };
    Response::builder().status(code).header("content-type", HeaderValue::from_static("application/json")).body(full(body)).unwrap()
}

/// A refusal with a reason a caller can act on. Reasons name paths and states, never a
/// token: nothing in this crate has one to name but [`Ctx::token`] and the doors' own.
pub(crate) fn problem(code: StatusCode, why: &str) -> Response<BoxBody> {
    json(code, &serde_json::json!({ "error": why }))
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
    fn a_scratch_stamp_is_the_utc_date_and_time() {
        let at = |secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        assert_eq!(utc_stamp(at(0)), "19700101-000000");
        assert_eq!(utc_stamp(at(1_709_209_599)), "20240229-122639", "a leap day");
        assert_eq!(utc_stamp(at(1_791_158_400 + 3_661)), "20261005-010101");
    }

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
        // Every `.rs` under `src/`, `src/bin/` included: a test double may write no CORS
        // header either, and a handler added in a new subdirectory is still scanned.
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap().to_ascii_lowercase();
                assert!(!text.contains(needle), "{} writes an {needle} header", path.display());
            }
        }
    }
}
