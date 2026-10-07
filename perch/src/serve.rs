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
//! **The seams** (slice S0P-a): `/hub/runs`, `/mesh`, `/meta/..`, `/install`, `/term`, `/notes` and `/mail` go to their own module
//! with the request unread (`<mod>::route(ctx, req, rest)`), and so do the session verbs `allow`, `revoke` and `mailsend`; [`target`]
//! decides, and a test holds its order. `/hub/term/a/<verb>` and `/hub/mail/a/<verb>` take [`agent::authenticate`] in place of the master
//! check, ahead of it. Everything a later slice needs from this file is a field of [`Ctx`], an extension of the request ([`Peer`],
//! [`agent::Caller`]) or a hook in the cancel arm, so none of them edits it.
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
use crate::folders;
use crate::gate;
use crate::proxy;
use crate::sessions::{self, ExitWatch, Registry};
use crate::spawn::{self, Spawner};
use crate::titles;
use crate::tree;
use crate::watch::{self, DoorKey, Watcher};
use crate::{agent, allow, install, mail, mesh, meta, notes, runs, term};

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
    /// What `main` parsed, as given: the flags the later slices read (`--plugins-dir`, `--aoide`, the token files, the knobs).
    pub opts: Arc<crate::Options>,
    /// Cancelled first thing in the shutdown arm, before any child is signalled. Every stream and wait the perch owns itself (a
    /// terminal's SSE, an agent's long read, an answerer's request) selects on it, so none of them is what a shutdown waits for.
    pub stop: CancellationToken,
    /// One state per module a later slice fills, `Default` at start and filled by the module's own hook.
    pub runs: Arc<runs::State>,
    pub mesh: Arc<mesh::State>,
    pub meta: Arc<meta::State>,
    pub install: Arc<install::State>,
    pub term: Arc<term::State>,
    pub notes: Arc<notes::State>,
    pub mail: Arc<mail::State>,
    /// Also the [`watch::Sink`] every door's watcher is opened with.
    pub allow: Arc<allow::State>,
    pub agent: Arc<agent::State>,
}

impl Ctx {
    /// `--knob doors=off`: this perch starts no door, for a demo that must never reach a model.
    pub(crate) fn doors_off(&self) -> bool {
        self.opts.knobs.is("doors", "off")
    }
}

/// The address a request's connection came from, put into the request's extensions by the accept loop and by nothing else: a header
/// cannot set an extension. The agent gate finds the chat a call came from with it (`agent::attrib`).
#[derive(Clone, Copy, Debug)]
pub struct Peer(pub SocketAddr);

/// The sentence for a refused door start (`--knob doors=off`).
const DOORS_OFF: &str = "chats are off on this perch";

/// The sentence for a door start that arrives once the perch has begun to stop.
const SHUTTING_DOWN: &str = "the perch is shutting down";

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

/// How long a shutdown gives each of the terminals' and the runs' own hooks before it goes on to the doors. A terminal's host is
/// itself bounded (about 2.5 s) and a run's hook only waits for what the perch owns, so this is the backstop for a hook that is not.
const HOOK_BOUND: Duration = Duration::from_secs(3);

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
                let (stream, peer) = match accepted {
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
                    hyper::service::service_fn(move |mut req: Request<Incoming>| {
                        // Where the connection came from, for every request on it: the kernel's word, not a header's.
                        req.extensions_mut().insert(Peer(peer));
                        handle(ctx.clone(), req)
                    }),
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
                //
                // `ctx.stop` goes first of all: every stream and wait the perch owns itself (a terminal's SSE, an agent's long
                // read) ends on it, so none is what the steps below wait for. Then the terminals, which are the perch's own
                // children and want their sessions hung up, then the runs (D3: a detached run is left alone, the hook only waits
                // for what the perch itself owns). Only then the doors: closing the watcher before the SIGTERM is still
                // `stop_all`'s own first step, and nothing before it opens a stream to a door.
                //
                // Each of the two hooks is bounded by `HOOK_BOUND`, and a hook that runs out is warned about and left behind: a
                // terminal that will not hang up or a run that will not report must not be what keeps `stop_all` from being
                // reached, because a door is only ever stopped by `stop_all`, and a door outliving the perch is the one failure
                // this order exists to prevent.
                drop(listener);
                ctx.stop.cancel();
                if tokio::time::timeout(HOOK_BOUND, term::shutdown(&ctx)).await.is_err() {
                    tracing::warn!("perch: the terminals did not close in time at shutdown; going on to the doors");
                }
                if tokio::time::timeout(HOOK_BOUND, runs::on_shutdown(&ctx)).await.is_err() {
                    tracing::warn!("perch: the runs did not settle in time at shutdown; going on to the doors");
                }
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

async fn route(ctx: Ctx, mut req: Request<Incoming>) -> Result<Response<BoxBody>, Infallible> {
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
    //    The other exception is the agent's two namespaces (`/hub/term/a/<verb>`, `/hub/mail/a/<verb>`): their credential is a
    //    scoped token, and it is decided *before* the master check so that a master bearer or a cookie never authenticates there. The
    //    agent gate also names the chat the call came from, and hands it on in the request's extensions. Every other path keeps
    //    the gate it always had, so a scoped token anywhere else is the ordinary 401.
    let redeem = method == Method::POST && path == REDEEM;
    if gated && !redeem {
        match agent::agent_scope(&path) {
            Some(scope) => match agent::authenticate(&ctx, &req, scope) {
                Ok(caller) => {
                    req.extensions_mut().insert(caller);
                }
                Err(refusal) => return Ok(*refusal),
            },
            None if !authenticated(&ctx, &req) => return Ok(status(StatusCode::UNAUTHORIZED)),
            None => {}
        }
    }
    // Once a shutdown has begun, nothing behind the token starts anything new: a connection that was open when the listener was
    // dropped can still send requests for as long as the drain lasts, and a terminal opened or a door started then would be one
    // the shutdown steps above have already gone past. After gate 1, so a caller without a credential still learns only 401, and
    // here rather than in each module, so a module a later slice fills is covered without knowing about it.
    if gated && ctx.stop.is_cancelled() {
        return Ok(status(StatusCode::SERVICE_UNAVAILABLE));
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
    if gated && method != Method::GET && method != Method::POST && !(method == Method::DELETE && path == "/hub/projects") {
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
    //    The modules a later slice fills (`runs`, `term`, `mail`, ...) are the third kind: they take the request unread, like the
    //    proxy, because some answer with a stream and all of them know best how big their own body may be. Gate 5 is still made
    //    here for them, as far as it can be before a read: a declared length over the cap is a 413, and what a module reads it reads
    //    through [`body_of`], which is the same cap.
    //    A 405 for a static path is still the method's answer, and no static body is
    //    read; the door's statics do the same.
    if gated {
        match target(&method, &path, &query) {
            Target::Proxy => return Ok(proxy::handle(&ctx, req).await),
            Target::Seam(seam) => {
                if declared_over_cap(&req) {
                    return Ok(status(StatusCode::PAYLOAD_TOO_LARGE));
                }
                return Ok(delegate(&ctx, seam, req).await);
            }
            Target::Hub => {}
        }
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

/// A delegated route's body, read under gate 5's cap: `cap` may lower it and never raises it past [`MAX_BODY`]. `Err` is the
/// refusal to answer with, boxed as the tree's is (413 over the cap, 400 for a body that broke off). The head comes back whole,
/// extensions included.
pub async fn body_of(
    req: Request<Incoming>,
    cap: usize,
) -> std::result::Result<(hyper::http::request::Parts, Bytes), Box<Response<BoxBody>>> {
    let (parts, body) = req.into_parts();
    match capped(body, cap.min(MAX_BODY)).await {
        Ok(bytes) => Ok((parts, bytes)),
        Err(refusal) => Err(Box::new(status(refusal))),
    }
}

/// A declared `Content-Length` over [`MAX_BODY`]: refused before the module is asked, as the proxy refuses it before it connects.
fn declared_over_cap(req: &Request<Incoming>) -> bool {
    let declared = req.headers().get(hyper::header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    declared.is_some_and(|len| len > MAX_BODY as u64)
}

// ---------------------------------------------------------------------------
// The seams: the modules a later slice fills
// ---------------------------------------------------------------------------

/// Who a gated request goes to once the gates have passed, decided from the method, the path and the query alone. The order is the
/// whole point and a test holds it: the proxy's `/s`, then the modules, then the `/hub` match, whose `GET` and `POST` fallbacks
/// answer *every* path, so a module checked after them would never be reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target<'a> {
    Proxy,
    Seam(Seam<'a>),
    Hub,
}

/// The modules' arms. The `&str` is the `rest` the module's handler is told: for a namespace the path below it (`""` for the bare path),
/// for a session verb the session id, which `sessions::id_ok` has already passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seam<'a> {
    Runs(&'a str),
    Mesh(&'a str),
    Meta(&'a str),
    Install(&'a str),
    Term(&'a str),
    Notes(&'a str),
    Mail(&'a str),
    AllowList(&'a str),
    AllowCreate(&'a str),
    AllowRevoke(&'a str),
    MailOptIn(&'a str),
}

fn target<'a>(method: &Method, path: &'a str, query: &str) -> Target<'a> {
    if path == "/s" || path.starts_with("/s/") {
        return Target::Proxy;
    }
    match seam(method, path, query) {
        Some(seam) => Target::Seam(seam),
        None => Target::Hub,
    }
}

/// `/hub/term` is the namespace `term` and `/hub/term/x` is below it; `/hub/terminal` and `/hub/termx` are neither. `Some("")` is the
/// bare namespace. One slash is stripped and no more, so `/hub/term//a/list` is `/a/list`, which is nobody's agent route.
pub(crate) fn below<'a>(path: &'a str, namespace: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(namespace)?;
    if rest.is_empty() { Some("") } else { rest.strip_prefix('/') }
}

fn seam<'a>(method: &Method, path: &'a str, query: &str) -> Option<Seam<'a>> {
    if let Some(rest) = below(path, "/hub/runs") {
        return Some(Seam::Runs(rest));
    }
    if let Some(rest) = below(path, "/hub/mesh") {
        return Some(Seam::Mesh(rest));
    }
    // `/hub/meta/<what>` only: the bare `/hub/meta` has nothing to say.
    if let Some(rest) = below(path, "/hub/meta").filter(|rest| !rest.is_empty()) {
        return Some(Seam::Meta(rest));
    }
    if let Some(rest) = below(path, "/hub/install") {
        return Some(Seam::Install(rest));
    }
    if let Some(rest) = below(path, "/hub/term") {
        return Some(Seam::Term(rest));
    }
    if let Some(rest) = below(path, "/hub/notes") {
        return Some(Seam::Notes(rest));
    }
    if let Some(rest) = below(path, "/hub/mail") {
        return Some(Seam::Mail(rest));
    }
    // The session verbs of the later slices, with the same shape rules as [`session_verb`]'s: no query, a checked id, one segment of verb.
    let (id, verb) = path.strip_prefix("/hub/sessions/")?.split_once('/')?;
    if !query.is_empty() || !sessions::id_ok(id) {
        return None;
    }
    match (method.as_str(), verb) {
        ("GET", "allow") => Some(Seam::AllowList(id)),
        ("POST", "allow") => Some(Seam::AllowCreate(id)),
        ("POST", "revoke") => Some(Seam::AllowRevoke(id)),
        ("POST", "mailsend") => Some(Seam::MailOptIn(id)),
        _ => None,
    }
}

/// The module's own handler, with the request unread.
async fn delegate(ctx: &Ctx, seam: Seam<'_>, req: Request<Incoming>) -> Response<BoxBody> {
    match seam {
        Seam::Runs(rest) => runs::route(ctx, req, rest).await,
        Seam::Mesh(rest) => mesh::route(ctx, req, rest).await,
        Seam::Meta(rest) => meta::route(ctx, req, rest).await,
        Seam::Install(rest) => install::route(ctx, req, rest).await,
        Seam::Term(rest) => term::route(ctx, req, rest).await,
        Seam::Notes(rest) => notes::route(ctx, req, rest).await,
        Seam::Mail(rest) => mail::route(ctx, req, rest).await,
        Seam::AllowList(id) => allow::list(ctx, req, id).await,
        Seam::AllowCreate(id) => allow::create(ctx, req, id).await,
        Seam::AllowRevoke(id) => allow::revoke(ctx, req, id).await,
        Seam::MailOptIn(id) => mail::optin(ctx, req, id).await,
    }
}

// ---------------------------------------------------------------------------
// H1b: the session lifecycle (hub.md §3, §4)
// ---------------------------------------------------------------------------

/// The `/hub/...` routes. `/hub/sessions` is the list and the spawn; its two verbs are per
/// session, and `/hub/tree` is H2's one directory listing. Everything else behind the token
/// is a 404: `/hub/events` and `/hub/mesh` are H2/H3.
async fn hub(ctx: &Ctx, method: &Method, path: &str, query: &str, body: &Bytes, bearer: bool) -> Response<BoxBody> {
    // `--knob doors=off`: a perch that shows chats and starts none. Refused here, before any verb has done anything (a fork has
    // copied a log by the time its door is asked for), and once more in `door_up` for a start this match does not name.
    if ctx.doors_off() && opens_a_door(method, path, query) {
        return problem(StatusCode::FORBIDDEN, DOORS_OFF);
    }
    match (method.as_str(), path) {
        ("GET", "/hub/sessions") => json(StatusCode::OK, &ctx.registry.titled_list().await),
        ("POST", "/hub/sessions") => new_session(ctx, body).await,
        // Token only: a script in the page's origin cannot read the HttpOnly cookie, and must not be
        // able to turn it into a code that a second client redeems for a session of its own.
        ("POST", "/hub/code") if !bearer => status(StatusCode::FORBIDDEN),
        ("POST", "/hub/code") => json(StatusCode::OK, &serde_json::json!({ "code": ctx.logins.mint_code() })),
        ("POST", REDEEM) => redeem(ctx, body),
        ("GET", "/hub/tree") => tree(ctx, query).await,
        ("GET", "/hub/file") => crate::rawfile::serve(ctx, query).await,
        ("GET", "/hub/models") => crate::restart::models(ctx).await,
        ("GET", "/hub/projects") => folders::projects_get(ctx),
        ("POST", "/hub/projects") => folders::projects_post(ctx, body).await,
        ("DELETE", "/hub/projects") => folders::projects_delete(ctx, body),
        ("POST", "/hub/projects/rename") => folders::projects_rename(ctx, body),
        ("GET", "/hub/trash") => crate::trash::list(ctx).await,
        ("POST", "/hub/trash/empty") => crate::trash::empty(ctx, body).await,
        ("GET", "/hub/dirs") => folders::dirs_get(ctx, query).await,
        ("POST", "/hub/dirs") => folders::dirs_post(ctx, body).await,
        ("GET", _) => match (crate::images::route(path), session_verb(path, query)) {
            (Some(found), _) => crate::images::serve(ctx, found, query).await,
            (None, Some((id, Verb::Log))) => log(ctx, id).await,
            (None, Some((id, Verb::First))) => crate::pending::state(ctx, id),
            _ => status(StatusCode::NOT_FOUND),
        },
        ("POST", _) => match session_verb(path, query) {
            Some((id, Verb::Resume)) => resume(ctx, id).await,
            Some((id, Verb::Stop)) => stop(ctx, id).await,
            Some((id, Verb::Restart)) => crate::restart::restart(ctx, id, body).await,
            Some((id, Verb::Fork)) => crate::fork::fork(ctx, id, body).await,
            Some((id, Verb::First)) => crate::pending::first(ctx, id, body).await,
            Some((id, Verb::Archive)) => crate::trash::archive(ctx, id, true).await,
            Some((id, Verb::Unarchive)) => crate::trash::archive(ctx, id, false).await,
            Some((id, Verb::Pin)) => crate::trash::pin(ctx, id, true).await,
            Some((id, Verb::Unpin)) => crate::trash::pin(ctx, id, false).await,
            Some((id, Verb::Trash)) => crate::trash::trash(ctx, id, body).await,
            Some((id, Verb::Rename)) => crate::relocate::rename(ctx, id, body).await,
            Some((id, Verb::Promote)) => crate::relocate::promote(ctx, id, body).await,
            Some((_, Verb::Log)) | None => match trash_verb(path, query) {
                Some(id) => crate::trash::restore(ctx, id).await,
                None => status(StatusCode::NOT_FOUND),
            },
        },
        _ => status(StatusCode::NOT_FOUND),
    }
}

/// `GET /hub/tree?session=<id>&path=<rel>` (hub.md section 7): the query is parsed in
/// [`tree::parse_query`] (400), the session must be a live door of ours or a saved chat whose folder is known ([`tree::base`]; 404 unknown, 409
/// `{"state"}` otherwise), and everything about the path is [`tree::list`]'s one empty 404.
/// The listing runs under `spawn_blocking`, holding one of [`tree::MAX_LISTINGS`] slots (a full house
/// is a 503), and its root is re-checked against `--root` there.
async fn tree(ctx: &Ctx, query: &str) -> Response<BoxBody> {
    let Some((id, rel)) = tree::parse_query(query) else { return status(StatusCode::BAD_REQUEST) };
    if !sessions::id_ok(&id) {
        return status(StatusCode::NOT_FOUND);
    }
    let (root, saved) = match tree::base(ctx, &id) {
        Ok(tree::Base::Live(root)) => (root, false),
        Ok(tree::Base::Saved(root)) => (root, true),
        Err(refusal) => return *refusal,
    };
    // A slot before any work, and before the path is looked at, so a full house answers the same
    // whatever the path is. The permit travels into the blocking task and is dropped when the
    // listing is done, not when the client has gone.
    let Some(permit) = ctx.tree_slots.take() else {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "too many directory listings in flight; retry");
    };
    let registry = ctx.registry.clone();
    let listed = tokio::task::spawn_blocking(move || -> Result<Option<tree::Listing>, ()> {
        let _slot = permit;
        // The cwd is judged against `--root` now, whoever stored it: a resumed session's cwd is
        // what its log said, and the tree is not for `/`.
        // A saved chat whose folder is unusable now says so in a sentence; a live one keeps the empty 404.
        let Some((root, floor)) = registry.tree_cwd(&root) else { return if saved { Err(()) } else { Ok(None) } };
        Ok(tree::list(&root, &floor, &rel))
    });
    match listed.await {
        Ok(Ok(Some(listing))) => json(StatusCode::OK, &listing),
        Ok(Err(())) => problem(StatusCode::NOT_FOUND, tree::SAVED_GONE),
        _ => status(StatusCode::NOT_FOUND),
    }
}

enum Verb {
    Resume,
    Stop,
    /// `{"model"?, "yolo"?}`: stop the door and resume the same log with the new flags, between turns (restart.rs).
    Restart,
    /// `{"at"?}`: a new chat copied from this one, whole (fork.rs).
    Fork,
    /// `{"text", "images"?, "yolo"?}` runs a pending fork's first message; a `GET` is how it is going (pending.rs).
    First,
    /// `GET` only; a `POST` to it is the 404 of a route that is not there.
    Log,
    /// Into `other` by hand, and back; pinned to the top, and not (trash.rs).
    Archive,
    Unarchive,
    Pin,
    Unpin,
    /// `{"stop"?: bool}`: move the log into `<home>/trash/` (trash.rs).
    Trash,
    /// `{"title"}`: a name by hand; an inbox chat's folder follows it (relocate.rs).
    Rename,
    /// `{"parent", "name"}`: an inbox chat's folder becomes a project (relocate.rs).
    Promote,
}

/// The requests that start a door: a new chat, a resume, a fork's first message, and the restart a mode or model switch is.
/// (A fork is the copy; its door waits for `first`, but the copy is the part that is refused here, so it is named too.)
fn opens_a_door(method: &Method, path: &str, query: &str) -> bool {
    if *method != Method::POST {
        return false;
    }
    path == "/hub/sessions" || matches!(session_verb(path, query), Some((_, Verb::Resume | Verb::Restart | Verb::Fork | Verb::First)))
}

/// `POST /hub/trash/<id>/restore`: the id, when the path is exactly that.
fn trash_verb<'a>(path: &'a str, query: &str) -> Option<&'a str> {
    if !query.is_empty() {
        return None;
    }
    let (id, verb) = path.strip_prefix("/hub/trash/")?.split_once('/')?;
    (verb == "restore").then_some(id)
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
            "restart" => Verb::Restart,
            "fork" => Verb::Fork,
            "first" => Verb::First,
            "log" => Verb::Log,
            "archive" => Verb::Archive,
            "unarchive" => Verb::Unarchive,
            "pin" => Verb::Pin,
            "unpin" => Verb::Unpin,
            "trash" => Verb::Trash,
            "rename" => Verb::Rename,
            "promote" => Verb::Promote,
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
        mode: Option<String>,
    }
    const SHAPE: &str = "expected a JSON body of {\"cwd\": \"<dir>\", \"model\": \"<m>\"?, \"mode\": \"manual\"|\"auto\"|\"yolo\"?}";
    let Ok(asked) = serde_json::from_slice::<Asked>(body) else {
        return problem(StatusCode::BAD_REQUEST, SHAPE);
    };
    if asked.model.as_deref().is_some_and(|model| !crate::models::key_ok(model)) {
        return problem(StatusCode::BAD_REQUEST, "that is not a model name (letters, digits and : . _ - / @ + only)");
    }
    // No mode is the home's `default_mode` (yolo when it names none); a word that is not one of the three is a 400, not a guess.
    let mode = match asked.mode.as_deref() {
        Some(word) => match crate::mode::Mode::parse(word) {
            Some(mode) => mode,
            None => return problem(StatusCode::BAD_REQUEST, SHAPE),
        },
        None => ctx.registry.default_mode,
    };
    let (config, yolo) = match ctx.registry.door_flags(mode) {
        Ok(flags) => flags,
        Err(why) => return problem(StatusCode::SERVICE_UNAVAILABLE, &why),
    };
    // No cwd is the Minerva home's scratch dir, when the perch has a home; without one it is the
    // refusal it always was: a body that names no cwd is a 400.
    let wanted = match (&asked.cwd, &ctx.registry.scratch) {
        (Some(cwd), _) => PathBuf::from(cwd),
        (None, Some(scratch)) => match crate::names::fresh_scratch(scratch) {
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
    let argv = spawn::argv(&ctx.registry.eidolon, config, spawn::Target::New(&cwd), asked.model.as_deref(), yolo);
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
    let (model, actual) = (hello.model.clone(), mode.reported(hello.yolo));
    ctx.registry.insert(id.clone(), hello.session, cwd, pid, port, token, watcher, exit);
    ctx.registry.set_flags(&id, &model, actual);
    json(StatusCode::CREATED, &serde_json::json!({ "id": id, "mode": actual }))
}

/// `GET /hub/sessions/<id>/log`: a saved chat as eidolon's own `log --json` lines (one record per line), so the page
/// can draw it read-only without a door and without the perch ever decoding the journal. The id is [`sessions::id_ok`]
/// and resolves only to `<dir>/<id>.eid` in one of the two sources (404 otherwise, as for every unknown id); the child
/// is bounded in time and output ([`titles`]): over the size is a 413, too slow a 504, a failure a 502. A chat that is
/// running here is still readable (eidolon opens it read-only), and nothing is started or resumed.
async fn log(ctx: &Ctx, id: &str) -> Response<BoxBody> {
    if !sessions::id_ok(id) {
        return status(StatusCode::NOT_FOUND);
    }
    let Some((path, origin)) = ctx.registry.find_log(id) else { return status(StatusCode::NOT_FOUND) };
    // As for a resume: a symlink at `<id>.eid` must not name a file somewhere else.
    if ctx.registry.id_of_log(&path).as_deref() != Some(id) {
        return status(StatusCode::NOT_FOUND);
    }
    match ctx.registry.read_log(&path, origin.is_some()).await {
        Ok(lines) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", HeaderValue::from_static("application/x-ndjson"))
            .body(full(lines))
            .unwrap(),
        Err(titles::RunError::TooBig) => problem(StatusCode::PAYLOAD_TOO_LARGE, "that chat's log is too large to show here"),
        Err(titles::RunError::Timeout) => problem(StatusCode::GATEWAY_TIMEOUT, "eidolon did not read that log in time"),
        Err(e) => {
            tracing::debug!(error = %e, "perch: eidolon log failed");
            problem(StatusCode::BAD_GATEWAY, &format!("eidolon could not read that log: {e}"))
        }
    }
}

/// §4 step 2's "Resume can spend" is a UI concern and not this route's: the perch never
/// auto-restarts a door, so a door opens this log only because someone asked for exactly
/// that, here.
async fn resume(ctx: &Ctx, id: &str) -> Response<BoxBody> {
    if !sessions::id_ok(id) {
        return status(StatusCode::NOT_FOUND);
    }
    // The log has to be a real log *inside* one of the two sources (the Minerva home's sessions dir, or eidolon's own
    // under `--home`; the first wins an id both have): the id was already checked, and this is what keeps a symlink at
    // `<id>.eid` from naming a file somewhere else. A chat from the other source is resumed with the member's normal
    // config like any other, and the door then writes to that log in place.
    let Some((log, _)) = ctx.registry.find_log(id) else { return status(StatusCode::NOT_FOUND) };
    if ctx.registry.id_of_log(&log).as_deref() != Some(id) {
        return status(StatusCode::NOT_FOUND);
    }
    // A fork from a point has no door until its first message has run: a door on the copy would open at the end of the whole copy.
    if ctx.registry.forks.is_pending(id) {
        return problem(StatusCode::CONFLICT, "that fork is waiting for its first message: send one, and it opens by itself");
    }
    // §4 step 2, both halves: our own children, and the roster — upstream has no `flock`, so
    // two writers on one log is a corruption the perch can at least refuse to start.
    if ctx.registry.held(&log) || ctx.registry.roster_holds(&log) || ctx.registry.is_moving(id) {
        return problem(StatusCode::CONFLICT, "that log is already held by a live session");
    }
    // Reserve, fork, check it opened this log, go live: restart.rs, which a model switch shares.
    // The mode it last ran in, else the default: a chat that was manual is never reopened looser.
    match crate::restart::bring_up(ctx, id, &log, None, ctx.registry.mode_of(id)).await {
        Ok(_) => {
            // Continuing a chat is activity: an archived one is back among the active (with a fresh week).
            crate::trash::continued(ctx, id);
            json(StatusCode::OK, &serde_json::json!({ "id": id }))
        }
        Err(crate::restart::Failed::Held) => problem(StatusCode::CONFLICT, "that log is already held by a live session"),
        Err(crate::restart::Failed::Spawn(why)) => problem(StatusCode::SERVICE_UNAVAILABLE, &why),
    }
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
pub(crate) struct Up {
    pub(crate) pid: u32,
    pub(crate) port: u16,
    pub(crate) token: Arc<str>,
    pub(crate) exit: ExitWatch,
    pub(crate) watcher: Watcher,
    pub(crate) hello: watch::Hello,
}

/// §4 steps 3-7 for one door, new or resumed — the two differ only in the argv they are
/// given. `id` is `Some` for a resume, which is the only case where the perch knows the
/// session before the door says it; every failure leaves that row `dead` rather than
/// half-`starting`.
pub(crate) async fn door_up(ctx: &Ctx, argv: Vec<OsString>, id: Option<&str>) -> std::result::Result<Up, String> {
    // The one place every door start passes through. A start that raced a shutdown past `route`'s own check ends here, before a fork:
    // `stop_all` has already counted the doors there are, and one forked after it would outlive the perch.
    if ctx.stop.is_cancelled() {
        dead(ctx, id, 0);
        return Err(SHUTTING_DOWN.to_string());
    }
    // `--knob doors=off` holds here for a caller `hub` did not name.
    if ctx.doors_off() {
        dead(ctx, id, 0);
        return Err(DOORS_OFF.to_string());
    }
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
    // The sink is told `hello` under the door's pid and port, because no row exists yet to say which chat this is (a new chat's
    // id is the thing `hello` names); `allow` binds the pid to the chat later, through `Registry::id_of_pid`.
    let key = DoorKey { pid, port: booted.port };
    match watch::open(key, booted.token.clone(), spawn::HELLO_TIMEOUT, ctx.allow.clone()).await {
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
pub(crate) async fn give_up(registry: &Registry, watcher: Watcher, pid: u32, port: u16, token: &str, exit: ExitWatch) {
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

    // ---- the seams: which path goes to which module ------------------------------------------------------------------------

    fn seam_of<'a>(method: &Method, path: &'a str) -> Option<Seam<'a>> {
        match target(method, path, "") {
            Target::Seam(seam) => Some(seam),
            _ => None,
        }
    }

    #[test]
    fn a_module_is_its_namespace_and_what_is_below_it() {
        for (path, want) in [
            ("/hub/runs", Seam::Runs("")),
            ("/hub/runs/", Seam::Runs("")),
            ("/hub/runs/r1/log", Seam::Runs("r1/log")),
            ("/hub/mesh", Seam::Mesh("")),
            ("/hub/mesh/nodes", Seam::Mesh("nodes")),
            ("/hub/meta/health", Seam::Meta("health")),
            ("/hub/install", Seam::Install("")),
            ("/hub/install/jev", Seam::Install("jev")),
            ("/hub/term", Seam::Term("")),
            ("/hub/term/events", Seam::Term("events")),
            ("/hub/term/t1/input", Seam::Term("t1/input")),
            ("/hub/term/a/list", Seam::Term("a/list")),
            ("/hub/notes", Seam::Notes("")),
            ("/hub/notes/x", Seam::Notes("x")),
            ("/hub/mail", Seam::Mail("")),
            ("/hub/mail/a/send", Seam::Mail("a/send")),
        ] {
            for method in [Method::GET, Method::POST] {
                assert_eq!(seam_of(&method, path), Some(want), "{method} {path}");
            }
        }
    }

    /// The prefix test: a name that merely starts with a namespace is not in it. A `starts_with("/hub/term")` in place of [`below`]
    /// would send `/hub/terminal` to the terminals, and a path nobody meant for them would be gated as theirs.
    #[test]
    fn a_path_that_only_starts_like_a_namespace_is_not_in_it() {
        for path in [
            "/hub/terminal",
            "/hub/termx",
            "/hub/notesx",
            "/hub/runsx",
            "/hub/meshed",
            "/hub/mailbox",
            "/hub/installer",
            "/hub/metadata",
            "/hub/meta",
            "/hub/meta/",
            "/hub/term%2Fa/list",
            "/hub/%74erm/a/list",
            "/hub/term\\a/list",
            "/hub",
            "/hub/",
            "/hub/events",
            "/hub/sessions",
        ] {
            for method in [Method::GET, Method::POST] {
                assert_eq!(target(&method, path, ""), Target::Hub, "{method} {path}");
            }
        }
    }

    /// The order of the arms: the proxy's `/s` first, a module next, and the `/hub` match last, because that match's `GET` and `POST`
    /// fallbacks answer every path and anything after them is never asked.
    #[test]
    fn the_proxy_is_asked_first_then_a_module_then_the_hub_match() {
        for path in ["/s", "/s/", "/s/x/api/events"] {
            assert_eq!(target(&Method::POST, path, ""), Target::Proxy, "{path}");
        }
        assert_eq!(target(&Method::GET, "/hub/term/a/list", ""), Target::Seam(Seam::Term("a/list")));
        assert_eq!(target(&Method::GET, "/hub/sessions", ""), Target::Hub);
        // A query changes nothing for a namespace (the modules read their own), and everything for a session verb.
        assert_eq!(target(&Method::GET, "/hub/runs", "limit=5"), Target::Seam(Seam::Runs("")));
        assert_eq!(target(&Method::POST, "/hub/sessions/c1/allow", ""), Target::Seam(Seam::AllowCreate("c1")));
        assert_eq!(target(&Method::POST, "/hub/sessions/c1/allow", "x=1"), Target::Hub);
    }

    #[test]
    fn a_session_verb_of_a_later_slice_is_a_method_an_id_and_no_query() {
        let want = |method: Method, verb: &str| seam_of(&method, &format!("/hub/sessions/c1/{verb}")).map(|seam| seam.label());
        assert_eq!(want(Method::GET, "allow"), Some("allow-list"));
        assert_eq!(want(Method::POST, "allow"), Some("allow-create"));
        assert_eq!(want(Method::POST, "revoke"), Some("allow-revoke"));
        assert_eq!(want(Method::POST, "mailsend"), Some("mail-optin"));
        for (method, verb) in [
            (Method::GET, "revoke"),
            (Method::GET, "mailsend"),
            (Method::DELETE, "allow"),
            (Method::POST, "allowed"),
            (Method::POST, "allow/x"),
            (Method::POST, "Allow"),
        ] {
            assert_eq!(want(method.clone(), verb), None, "{method} {verb}");
        }
        for id in ["..", "-x", "", "a b", "a%2fb", "a/b"] {
            assert_eq!(seam_of(&Method::POST, &format!("/hub/sessions/{id}/allow")), None, "id {id:?}");
        }
        // Neither the lifecycle verbs nor the images routes claim these, and these claim none of theirs.
        for verb in ["allow", "revoke", "mailsend"] {
            let path = format!("/hub/sessions/c1/{verb}");
            assert!(session_verb(&path, "").is_none() && crate::images::route(&path).is_none(), "{verb}");
        }
        for verb in [
            "resume", "stop", "restart", "fork", "first", "log", "archive", "unarchive", "pin", "unpin", "trash", "rename", "promote",
            "images", "transcript", "image/1/0",
        ] {
            for method in [Method::GET, Method::POST] {
                assert_eq!(seam_of(&method, &format!("/hub/sessions/c1/{verb}")), None, "{method} {verb}");
            }
        }
    }

    impl Seam<'_> {
        fn label(&self) -> &'static str {
            match self {
                Seam::Runs(_) => "runs",
                Seam::Mesh(_) => "mesh",
                Seam::Meta(_) => "meta",
                Seam::Install(_) => "install",
                Seam::Term(_) => "term",
                Seam::Notes(_) => "notes",
                Seam::Mail(_) => "mail",
                Seam::AllowList(_) => "allow-list",
                Seam::AllowCreate(_) => "allow-create",
                Seam::AllowRevoke(_) => "allow-revoke",
                Seam::MailOptIn(_) => "mail-optin",
            }
        }
    }

    /// The agent gate's predicate and the dispatch read the same raw path: a path the gate takes as an agent's reaches `term` or `mail`
    /// as `a/<verb>`, and no other path reaches those two modules in that shape.
    #[test]
    fn the_agent_gate_and_the_dispatch_name_the_same_paths() {
        use crate::agent::{agent_scope, Scope};
        for path in [
            "/hub/term/a/list",
            "/hub/term/a/whoami",
            "/hub/term/a",
            "/hub/term/a/",
            "/hub/term/a/x/y",
            "/hub/term/a/List",
            "/hub/term/%61/list",
            "/hub/term/t1/a/list",
            "/hub/terma/list",
            "/hub/mail/a/send",
            "/hub/mail/a/",
            "/hub/mail/ab/send",
            "/hub/notes/a/list",
            "/hub/runs/a/list",
            // A doubled slash is not the agent's path for the gate, and it must not become one for the dispatch: only the one slash
            // that separates the namespace from what is below it is dropped.
            "/hub/term//a/list",
            "/hub/term/a//list",
            "/hub/term///a/list",
            "/hub/mail//a/x",
            "/hub/mail/a//x",
        ] {
            let (term, mail) = match seam_of(&Method::POST, path) {
                Some(Seam::Term(rest)) => (crate::agent::verb(rest).is_some(), false),
                Some(Seam::Mail(rest)) => (false, crate::agent::verb(rest).is_some()),
                _ => (false, false),
            };
            assert_eq!(agent_scope(path) == Some(Scope::Term), term, "{path}");
            assert_eq!(agent_scope(path) == Some(Scope::Mail), mail, "{path}");
        }
    }

    #[test]
    fn a_door_start_is_a_post_to_one_of_five_routes() {
        for path in [
            "/hub/sessions",
            "/hub/sessions/c1/resume",
            "/hub/sessions/c1/restart",
            "/hub/sessions/c1/fork",
            "/hub/sessions/c1/first",
        ] {
            assert!(opens_a_door(&Method::POST, path, ""), "POST {path}");
            assert!(!opens_a_door(&Method::GET, path, ""), "GET {path}");
        }
        for path in ["/hub/sessions/c1/stop", "/hub/sessions/c1/archive", "/hub/sessions/c1/pin", "/hub/sessions/c1/trash", "/hub/tree"] {
            assert!(!opens_a_door(&Method::POST, path, ""), "POST {path}");
        }
        // The same shape rules as the route itself: a query makes it a 404 there and so not a start here.
        assert!(!opens_a_door(&Method::POST, "/hub/sessions/c1/resume", "x=1"));
    }

    /// The cancel arm's order is the spec's and no later slice can edit this file, so it is held where it is written: the token first, the
    /// terminals, the runs, the doors, and only then the drain.
    #[test]
    fn the_shutdown_arm_runs_its_steps_in_the_order_the_spec_gives() {
        let src = include_str!("serve.rs");
        let (from, to) = (src.find("pub async fn run(").unwrap(), src.find("fn in_gated").unwrap());
        let body = &src[from..to];
        let at = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("the cancel arm no longer calls {needle}"));
        let order = [
            at("ctx.stop.cancel()"),
            at("term::shutdown(&ctx)"),
            at("runs::on_shutdown(&ctx)"),
            at("ctx.registry.stop_all()"),
            at("graceful.shutdown()"),
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "the cancel arm is out of order: {order:?}");
        // The two hooks are each inside a timeout, so a hook that never returns cannot keep `stop_all` from being reached. The call
        // is named by what stands directly before it, which is what fails when the timeout is taken off.
        for hook in ["term::shutdown(&ctx)", "runs::on_shutdown(&ctx)"] {
            let before = &body[..at(hook)];
            assert!(before.ends_with("tokio::time::timeout(HOOK_BOUND, "), "{hook} is not bounded by HOOK_BOUND");
        }
        assert!(
            HOOK_BOUND >= Duration::from_secs(1) && HOOK_BOUND <= Duration::from_secs(5),
            "a hook gets a few seconds: long enough for the terminals' own 2.5 s, short enough to leave the doors their turn"
        );
    }

    // ---- over a socket: the gates and the stubs, with no door and no eidolon ---------------------------------------------------

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    const TOKEN: &str = "a-test-token";

    /// A perch on a loopback port: the real accept loop and gates over a hand-built [`Ctx`], and an `eidolon` that is not there, so
    /// nothing a request does can start a process or reach a model.
    struct Up {
        addr: SocketAddr,
        ctx: Ctx,
        shutdown: CancellationToken,
        served: tokio::task::JoinHandle<Result<()>>,
        sessions: PathBuf,
        _dirs: (tempfile::TempDir, tempfile::TempDir),
    }

    async fn up(knobs: &[&str]) -> Up {
        let sessions = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        std::fs::write(sessions.path().join("ctf-pwn-3.eid"), "{}\n").unwrap();
        let knobs: Vec<String> = knobs.iter().map(|knob| knob.to_string()).collect();
        let opts = crate::Options {
            bind: "127.0.0.1:0".parse().unwrap(),
            ui_dir: None,
            roots: vec![sessions.path().to_path_buf()],
            eidolon: PathBuf::from("/nonexistent/eidolon"),
            sessions_dir: sessions.path().to_path_buf(),
            eidolon_config: None,
            scratch: None,
            other_sessions: None,
            modes: Default::default(),
            code_ttl: Duration::from_secs(60),
            plugins_dir: None,
            run_graphs: Vec::new(),
            jev_health: None,
            browser_health: None,
            aoide: None,
            aoide_home: None,
            aoide_daemon_socket: None,
            aoide_events: None,
            minerva: None,
            term_token_file: None,
            mail_token_file: None,
            agent_url_file: None,
            knobs: crate::Knobs::parse(&knobs).unwrap(),
        };
        let registry = Registry::new(
            sessions.path().to_path_buf(),
            runtime.path().to_path_buf(),
            opts.eidolon.clone(),
            opts.roots.clone(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ctx = Ctx {
            bound: listener.local_addr().unwrap(),
            token: Arc::from(TOKEN),
            ui: None,
            registry,
            spawner: Arc::new(Spawner::new().unwrap()),
            tree_slots: tree::Slots::new(tree::MAX_LISTINGS),
            logins: Arc::new(crate::login::Logins::new(Duration::from_secs(60))),
            opts: Arc::new(opts),
            stop: CancellationToken::new(),
            runs: Arc::default(),
            mesh: Arc::default(),
            meta: Arc::default(),
            install: Arc::default(),
            term: Arc::default(),
            notes: Arc::default(),
            mail: Arc::default(),
            allow: Arc::default(),
            agent: Arc::default(),
        };
        let shutdown = CancellationToken::new();
        let served = tokio::spawn(run(ctx.clone(), listener, shutdown.clone()));
        Up { addr: ctx.bound, ctx, shutdown, served, sessions: sessions.path().to_path_buf(), _dirs: (sessions, runtime) }
    }

    /// One request over its own connection, written exactly as given, and the status and body that came back.
    async fn raw(perch: &Up, request: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(perch.addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = Vec::new();
        // A reset after the answer (a body the perch did not read) loses nothing that was already read.
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer)).await;
        assert!(read.is_ok(), "the perch did not answer {request:?}");
        let text = String::from_utf8_lossy(&answer).into_owned();
        let code = text.split(' ').nth(1).and_then(|code| code.parse().ok()).unwrap_or_else(|| panic!("no status line in {text:?}"));
        (code, text.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or_default())
    }

    /// `credential` is a header block (or nothing), written as is.
    async fn call(perch: &Up, method: &str, path: &str, credential: &str, body: &str) -> (u16, String) {
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n{credential}Content-Length: {}\r\n\r\n{body}",
            perch.addr,
            body.len()
        );
        raw(perch, &request).await
    }

    fn bearer() -> String {
        format!("Authorization: Bearer {TOKEN}\r\n")
    }

    fn cookie(perch: &Up) -> String {
        let secret = perch.ctx.logins.redeem(&perch.ctx.logins.mint_code()).expect("a fresh code redeems");
        format!("Cookie: {}={secret}\r\nSec-Fetch-Site: same-origin\r\n", cookie_name(perch.ctx.bound))
    }

    #[tokio::test]
    async fn no_credential_a_master_bearer_and_a_cookie_are_all_a_401_on_an_agent_path() {
        let perch = up(&[]).await;
        let cookie = cookie(&perch);
        // The controls: the same credentials do open the namespaces around the agent's, so the 401 below is the agent gate's.
        assert_eq!(call(&perch, "GET", "/hub/sessions", &bearer(), "").await.0, 200);
        assert_eq!(call(&perch, "GET", "/hub/sessions", &cookie, "").await.0, 200);
        assert_eq!(call(&perch, "GET", "/hub/term", &bearer(), "").await.0, 404);
        assert_eq!(call(&perch, "GET", "/hub/term", &cookie, "").await.0, 404);
        assert_eq!(call(&perch, "GET", "/hub/term", "", "").await.0, 401);
        for path in ["/hub/term/a/list", "/hub/term/a/whoami", "/hub/mail/a/send", "/hub/mail/a/whoami"] {
            for method in ["GET", "POST"] {
                for (what, credential) in [("a master bearer", bearer()), ("a cookie", cookie.clone()), ("nothing", String::new())] {
                    let (code, body) = call(&perch, method, path, &credential, "").await;
                    assert_eq!((code, body.as_str()), (401, ""), "{what} on {method} {path}");
                }
            }
        }
        // A wrong bearer is no better than none, and the scheme is not guessed at.
        assert_eq!(call(&perch, "GET", "/hub/term/a/list", "Authorization: Bearer nope\r\n", "").await.0, 401);
        assert_eq!(call(&perch, "GET", "/hub/term/a/list", &format!("Authorization: {TOKEN}\r\n"), "").await.0, 401);
        // Only the two prefixes are the agent's: a spelling that is not exactly `a/<verb>` is today's gate and then the module's 404.
        for path in ["/hub/term/a", "/hub/term/a/", "/hub/term/a/x/y", "/hub/term/a/List", "/hub/term/%61/list", "/hub/mail/a/"] {
            assert_eq!(call(&perch, "GET", path, &bearer(), "").await.0, 404, "{path}");
            assert_eq!(call(&perch, "GET", path, "", "").await.0, 401, "{path}");
        }
        perch.shutdown.cancel();
    }

    #[tokio::test]
    async fn every_seam_is_a_404_with_the_token_and_a_401_without_it() {
        let perch = up(&[]).await;
        for (method, path) in [
            ("GET", "/hub/runs"),
            ("POST", "/hub/runs"),
            ("GET", "/hub/runs/r1/log"),
            ("GET", "/hub/mesh"),
            ("GET", "/hub/meta/health"),
            ("GET", "/hub/install"),
            ("POST", "/hub/install/jev"),
            ("GET", "/hub/term"),
            ("GET", "/hub/term/events"),
            ("POST", "/hub/term/t1/input"),
            ("GET", "/hub/notes"),
            ("POST", "/hub/notes/x"),
            ("GET", "/hub/mail"),
            ("GET", "/hub/mail/nodes"),
            ("GET", "/hub/sessions/ctf-pwn-3/allow"),
            ("POST", "/hub/sessions/ctf-pwn-3/allow"),
            ("POST", "/hub/sessions/ctf-pwn-3/revoke"),
            ("POST", "/hub/sessions/ctf-pwn-3/mailsend"),
        ] {
            let (code, body) = call(&perch, method, path, &bearer(), "{}").await;
            assert_eq!((code, body.as_str()), (404, ""), "{method} {path} with the token");
            assert_eq!(call(&perch, method, path, "", "{}").await.0, 401, "{method} {path} without it");
        }
        // The two that were already pinned stay where they were, and so does a path that only looks like a namespace.
        for path in ["/hub/events", "/hub/terminal", "/hub/termx", "/hub/notesx", "/hub/meta"] {
            assert_eq!(call(&perch, "GET", path, &bearer(), "").await.0, 404, "{path}");
        }
        perch.shutdown.cancel();
    }

    #[tokio::test]
    async fn a_declared_length_over_the_cap_is_a_413_before_a_module_is_asked() {
        let perch = up(&[]).await;
        let head = |path: &str, declared: usize| {
            format!(
                "POST {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n{}Content-Length: {declared}\r\n\r\n",
                perch.addr,
                bearer()
            )
        };
        // Only the head is sent: the answer cannot have waited for a body that never comes.
        for path in ["/hub/runs", "/hub/term/t1/input", "/hub/mail/nodes", "/hub/notes/x", "/hub/sessions/c1/allow"] {
            assert_eq!(raw(&perch, &head(path, MAX_BODY + 1)).await.0, 413, "{path}");
            assert_eq!(raw(&perch, &head(path, MAX_BODY)).await.0, 404, "{path} at the cap is the module's to read");
        }
        // Without the token the cap is not looked at: the first gate answers.
        let unauthenticated = format!(
            "POST /hub/runs HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            perch.addr,
            MAX_BODY + 1
        );
        assert_eq!(raw(&perch, &unauthenticated).await.0, 401);
        perch.shutdown.cancel();
    }

    #[tokio::test]
    async fn doors_off_refuses_every_door_start_and_lists_chats_as_before() {
        let perch = up(&["doors=off"]).await;
        let new = serde_json::json!({ "cwd": perch.sessions }).to_string();
        for (path, body) in [
            ("/hub/sessions", new.as_str()),
            ("/hub/sessions", ""),
            ("/hub/sessions/ctf-pwn-3/resume", ""),
            ("/hub/sessions/ctf-pwn-3/restart", "{\"model\":\"m\"}"),
            ("/hub/sessions/ctf-pwn-3/restart", "{\"yolo\":true}"),
            ("/hub/sessions/ctf-pwn-3/fork", "{}"),
            ("/hub/sessions/ctf-pwn-3/first", "{\"text\":\"hi\"}"),
            ("/hub/sessions/nobody/resume", ""),
        ] {
            let (code, answer) = call(&perch, "POST", path, &bearer(), body).await;
            assert_eq!(code, 403, "POST {path}");
            assert!(answer.contains("chats are off on this perch"), "POST {path}: {answer}");
        }
        // What shows chats is untouched: the list, a saved chat's id in it, and a verb that is not a start.
        let (code, listed) = call(&perch, "GET", "/hub/sessions", &bearer(), "").await;
        assert_eq!(code, 200);
        assert!(listed.contains("ctf-pwn-3"), "the saved chat is still listed: {listed}");
        assert_eq!(call(&perch, "POST", "/hub/sessions/ctf-pwn-3/stop", &bearer(), "").await.0, 409, "stop is not a start");
        perch.shutdown.cancel();
    }

    #[tokio::test]
    async fn without_the_knob_the_same_requests_are_not_refused_for_it() {
        // No door can come up here (the `eidolon` is not there), so these end in the spawn's own refusal. What matters is that it is
        // never the 403 of the knob, which a perch that forgot to read it would also not give.
        let perch = up(&[]).await;
        let new = serde_json::json!({ "cwd": perch.sessions }).to_string();
        for (path, body) in [("/hub/sessions", new.as_str()), ("/hub/sessions/ctf-pwn-3/resume", "")] {
            let (code, answer) = call(&perch, "POST", path, &bearer(), body).await;
            assert_ne!(code, 403, "POST {path}: {answer}");
            assert!(!answer.contains("chats are off"), "POST {path}: {answer}");
        }
        // And a knob that is not `doors=off` is not it: the value is compared whole.
        let other = up(&["doors=on", "doorsx=off"]).await;
        let (code, answer) = call(&other, "POST", "/hub/sessions/ctf-pwn-3/resume", &bearer(), "").await;
        assert_ne!(code, 403, "{answer}");
        other.shutdown.cancel();
        perch.shutdown.cancel();
    }

    #[tokio::test]
    async fn a_shutdown_cancels_the_stop_token_before_the_perch_returns() {
        let perch = up(&[]).await;
        assert!(!perch.ctx.stop.is_cancelled());
        assert_eq!(call(&perch, "GET", "/hub/sessions", &bearer(), "").await.0, 200);
        perch.shutdown.cancel();
        let served = tokio::time::timeout(Duration::from_secs(10), perch.served).await.expect("the perch stopped");
        assert!(served.unwrap().is_ok());
        assert!(perch.ctx.stop.is_cancelled(), "the token every stream of the perch's own waits on was cancelled by the shutdown");
    }

    /// A connection that was open when the listener was dropped still sends requests during the drain: the stop token is cancelled
    /// here and the accept loop is left running, which is that moment.
    #[tokio::test]
    async fn once_the_perch_is_stopping_nothing_behind_the_token_is_started() {
        let perch = up(&[]).await;
        let cookie = cookie(&perch);
        assert_eq!(call(&perch, "GET", "/hub/sessions", &bearer(), "").await.0, 200, "control, before the stop");
        perch.ctx.stop.cancel();
        // Every gated route, the modules' and the ones that were there before them: one answer, from one place.
        for (method, path) in [
            ("GET", "/hub/sessions"),
            ("POST", "/hub/sessions"),
            ("POST", "/hub/sessions/ctf-pwn-3/resume"),
            ("GET", "/hub/term"),
            ("POST", "/hub/term"),
            ("GET", "/hub/term/events"),
            ("GET", "/hub/runs"),
            ("POST", "/hub/runs"),
            ("POST", "/hub/mail/nodes"),
            ("GET", "/hub/tree"),
            ("GET", "/s/x/api/events"),
            ("GET", "/hub/nothing-at-all"),
        ] {
            let (code, body) = call(&perch, method, path, &bearer(), "{}").await;
            assert_eq!((code, body.as_str()), (503, ""), "{method} {path} with the token");
        }
        assert_eq!(call(&perch, "GET", "/hub/sessions", &cookie, "").await.0, 503, "a cookie is no different");
        // After gate 1, not before it: a caller with no credential, or on an agent path with the master one, still gets the 401.
        assert_eq!(call(&perch, "GET", "/hub/sessions", "", "").await.0, 401);
        assert_eq!(call(&perch, "GET", "/hub/term/a/list", &bearer(), "").await.0, 401);
        assert_eq!(call(&perch, "GET", "/hub/term/a/list", "", "").await.0, 401);
        // What is not behind the token is not this check's: the static route still answers (404, there is no UI dir here).
        assert_eq!(call(&perch, "GET", "/index.html", "", "").await.0, 404);
        perch.shutdown.cancel();
    }

    /// The same moment, one step later: a start that got past `route` (it was already running) still cannot fork once the token is
    /// cancelled, because `stop_all` has already counted the doors there are.
    #[tokio::test]
    async fn a_door_is_not_forked_once_the_perch_is_stopping() {
        let perch = up(&[]).await;
        let argv = vec![OsString::from("/nonexistent/eidolon")];
        // The control: with the token alive the start gets as far as the fork, which is what fails for a binary that is not there.
        let why = door_up(&perch.ctx, argv.clone(), None).await.err().expect("no such binary, no door");
        assert_ne!(why, SHUTTING_DOWN, "{why}");
        perch.ctx.stop.cancel();
        for id in [None, Some("ctf-pwn-3")] {
            let why = door_up(&perch.ctx, argv.clone(), id).await.err().expect("no door while stopping");
            assert_eq!(why, SHUTTING_DOWN, "{id:?}");
        }
        perch.shutdown.cancel();
    }

    /// Whether a source text collects a body the way `serve::capped` does, which is the one thing a module must not do on its own: a
    /// `collect` of an `Incoming` in a module is an uncapped read of whatever the caller sends, where [`body_of`] is gate 5's cap.
    /// Whitespace and comment lines are ignored, so a chain split over lines is still the call and a comment about it is not. The
    /// needles are assembled from pieces, so this file is not what the scan finds in it.
    fn collects_a_body(text: &str) -> bool {
        let code: String = text.lines().filter(|line| !line.trim_start().starts_with("//")).flat_map(str::split_whitespace).collect();
        code.contains(&[".col", "lect()", ".aw", "ait"].concat()) || code.contains(&["BodyExt::col", "lect("].concat())
    }

    /// `serve.rs` (which owns `capped`) and `proxy.rs` (which streams under its own `Limited`) are the two places that read a body.
    #[test]
    fn no_module_collects_a_request_body_but_through_the_cap() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src.clone()];
        let mut scanned = 0;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let exempt = path.parent() == Some(src.as_path()) && matches!(name, "serve.rs" | "proxy.rs");
                if path.extension().and_then(|e| e.to_str()) != Some("rs") || exempt {
                    continue;
                }
                scanned += 1;
                let text = std::fs::read_to_string(&path).unwrap();
                assert!(!collects_a_body(&text), "{} collects a body: read it through serve::body_of", path.display());
            }
        }
        assert!(scanned > 30, "the scan looked at {scanned} files, so it is not looking at src/");
    }

    #[test]
    fn the_collect_scan_finds_the_call_however_it_is_laid_out() {
        assert!(collects_a_body("let all = req\n    .into_body()\n    .collect()\n    .await;\n"));
        assert!(collects_a_body("let all = Limited::new(body, 9).collect().await;"));
        assert!(collects_a_body("let all = http_body_util::BodyExt::collect(body).await;"));
        assert!(!collects_a_body("// body.collect().await is what body_of is for\nlet v: Vec<_> = it.collect();\n"));
        assert!(!collects_a_body("let (parts, bytes) = body_of(req, 1024).await?;"));
    }
}
