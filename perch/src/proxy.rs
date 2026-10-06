//! The proxy: `GET|POST /s/<id>/api/<route>`, forwarded to that session's door (hub.md §5, §6).
//!
//! This is the browser-facing half of the perch. The page holds the perch's token and nothing
//! else; this module holds each door's token (in the registry, in memory) and writes it into
//! exactly one place, the `Authorization` header of the request it sends *to that door*.
//!
//! **What crosses, and what does not.** The request is rebuilt, not forwarded: method, the
//! URI `/api/<route>`, the door's Bearer, a `Host` of `127.0.0.1:<door port>` (the door checks
//! it on POSTs, so the rewrite is load-bearing), and `Content-Type` and `Accept` if the page
//! sent them. Nothing else is copied: no `Cookie`, `Origin`, `Referer`, `Sec-*`,
//! `Last-Event-ID`, `X-Forwarded-*`, and not the perch's own `Authorization`. The response is
//! rebuilt the same way: the door's status, its `content-type`, and `cache-control: no-store`.
//! A door's 401 becomes the perch's 502, so the page never concludes its own token is bad.
//!
//! **The path is a shape, not an allowlist.** `<route>` must match
//! `^[a-z][a-z0-9-]{0,31}(/[a-z0-9-]{1,32})*$`, so `/api/launch` (and a future `/api/commands`)
//! pass with no change here. Nothing in this module names a route, a content type, or a field
//! of a body or a frame: bodies and frames are bytes. The one exception is the route `events`,
//! because a GET of it is the one request that becomes a *pane*.
//!
//! **Bodies stream.** A POST's body goes to the door through `Limited` at [`MAX_BODY`] without
//! being collected first: a `say` may carry several images, so multi-MiB bodies are the normal
//! case, and buffering one would also hold the connection up until it had all arrived. When the
//! limit trips the upstream body errors, the door sees a truncated request, and the answer is a
//! 413. A declared `Content-Length` over the cap is a 413 before any connection is made.
//!
//! **A pane is a proxied `/api/events`.** It is counted per door while its body lives, it ends
//! when the door's stream ends or a stop cancels it, and it always ends with one frame of the
//! perch's own, `hub-stream-end`, so the page can tell "the door ended this stream" (`live`:
//! reconnect), "the perch is stopping the door" (`stopping`) and "the door is gone" (`dead`)
//! apart. The frame starts with a blank line to terminate any half-written door frame.
//!
//! **Transport.** One fresh TCP connection per request, loopback only, no pool: a pool would
//! outlive a dead door. The connection's task is tied to the response body and is aborted
//! *and awaited* when the body ends or is dropped, as the watcher's is, so "the pane closed"
//! means the socket really is closed before the SIGTERM that a stop sends next. A pane's connection
//! belongs to a task of the pane's own, so a page that has stopped reading cannot hold a stop up.
//!
//! Not here, on purpose: the `goodbye` frame is not parsed, so a door that says `goodbye` and
//! exits is classified `dead` with `code:0`, not `stopping` (telling them apart needs frame
//! parsing, which §5 rules out; H2 can revisit).

use std::convert::Infallible;
use std::future::Future;
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt as _, Empty, LengthLimitError, Limited};
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::client::conn::http1::SendRequest;
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard, WaitForCancellationFutureOwned};

use crate::serve::{BoxBody, Ctx, MAX_BODY, json, problem, status};
use crate::sessions::{self, ExitWatch, Lookup, PaneGuard, Panes};

/// §5: this is a socket to a process on this box.
const CONNECT: Duration = Duration::from_secs(2);
/// §5: a POST's response head. `/api/launch` spawns under `spawn_blocking`, so a POST is not
/// instant; an SSE stream has no such bound because its head is the first thing the door sends
/// and its body is the part that never ends.
const HEAD: Duration = Duration::from_secs(60);
/// How long a stream that has ended waits to learn whether the door has too. The stream's
/// end and the reap task's report of the exit are two events, and `dead` vs `live` is read
/// off whichever has happened, so the wait is for the exit to be *published*, not slept.
const SETTLE: Duration = Duration::from_secs(1);

type Failure = Box<dyn std::error::Error + Send + Sync>;
/// What goes to the door: a POST's `Limited` stream, or nothing for a GET. One type, so one
/// `SendRequest<_>` type serves both.
type Upstream = UnsyncBoxBody<Bytes, Failure>;

/// `/s/<id>/api/<route>` out of a raw path, or `None`. The raw path: any `%` is a refusal, and
/// so is anything else that is not exactly the shape, so a spelling the door would decode
/// differently never reaches it.
fn parse(path: &str) -> Option<(&str, &str)> {
    if path.contains('%') {
        return None;
    }
    let (id, rest) = path.strip_prefix("/s/")?.split_once('/')?;
    if !sessions::id_ok(id) {
        return None;
    }
    let route = rest.strip_prefix("api/")?;
    route_ok(route).then_some((id, route))
}

/// §5's regex, spelled out: `^[a-z][a-z0-9-]{0,31}(/[a-z0-9-]{1,32})*$`. No `.`, so no `..`; no
/// empty segment, so no `//`; lower case only, so `Say` is not `say`.
fn route_ok(route: &str) -> bool {
    let mut segments = route.split('/');
    let first = segments.next().unwrap_or("");
    let first_ok = first.len() <= 32
        && first.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && first.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    first_ok
        && segments.all(|segment| {
            (1..=32).contains(&segment.len()) && segment.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// The whole proxy, steps a-g of the H1c task in the order they happen. The request body is
/// still unread when this is called.
pub async fn handle(ctx: &Ctx, req: Request<hyper::body::Incoming>) -> Response<BoxBody> {
    // a. A declared length over the cap is refused before anything else, and before any
    //    connection: the door would refuse it too, and there is nothing to learn by asking.
    let declared = req.headers().get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|len| len > MAX_BODY as u64) {
        return status(StatusCode::PAYLOAD_TOO_LARGE);
    }

    // b. The shape. Every malformed path is the same empty 404, as for any route that is not
    //    there: no oracle for which part of the shape failed.
    let Some((id, route)) = parse(req.uri().path()).map(|(id, route)| (id.to_string(), route.to_string())) else {
        return status(StatusCode::NOT_FOUND);
    };
    let (id, route) = (id.as_str(), route.as_str());

    // c. The session, in one lock: whether it is ours, whether it is live, and where it is.
    let target = match ctx.registry.lookup(id) {
        Lookup::Live(target) => target,
        Lookup::Dead => return dead(),
        Lookup::Between(state) => return json(StatusCode::CONFLICT, &serde_json::json!({ "state": state })),
        // Not a door of ours: a kept log is a session the page can resume (409 `kept`, and the
        // same for a log some other consumer holds, which is never adopted: Q5); anything else
        // is not a session at all.
        Lookup::NotOurs => {
            let kept = ctx.registry.find_log(id).is_some(); // either source: the other one is readable and resumable too
            return if kept { json(StatusCode::CONFLICT, &serde_json::json!({ "state": "kept" })) } else { status(StatusCode::NOT_FOUND) };
        }
    };

    // d. The upstream request: rebuilt from five facts, copied from nowhere else.
    let method = req.method().clone();
    let (parts, incoming) = req.into_parts();
    let limit_hit = Arc::new(AtomicBool::new(false));
    let body: Upstream = if method == Method::POST {
        let flag = limit_hit.clone();
        Limited::new(incoming, MAX_BODY)
            .map_err(move |e| {
                if e.downcast_ref::<LengthLimitError>().is_some() {
                    flag.store(true, Ordering::SeqCst);
                }
                e
            })
            .boxed_unsync()
    } else {
        // A GET sends no body, whatever the page sent with it.
        Empty::<Bytes>::new().map_err(|never| match never {}).boxed_unsync()
    };
    let Ok(mut bearer) = HeaderValue::from_str(&format!("Bearer {}", target.token)) else {
        tracing::warn!("perch: a door's token is not a header value");
        return problem(StatusCode::BAD_GATEWAY, "the door did not answer");
    };
    // Never shown by a debug print of the request, the way a token must not be.
    bearer.set_sensitive(true);
    let mut upstream = Request::builder()
        .method(method.clone())
        .uri(format!("/api/{route}"))
        .header(AUTHORIZATION, bearer)
        .header(HOST, format!("127.0.0.1:{}", target.port));
    for name in [CONTENT_TYPE, ACCEPT] {
        if let Some(value) = parts.headers.get(&name) {
            upstream = upstream.header(name, value.clone());
        }
    }
    let Ok(upstream) = upstream.body(body) else { return problem(StatusCode::BAD_GATEWAY, "the door did not answer") };

    // g (first half). A GET of `events` is a pane. It is counted from *before* the connect, so a
    //    stop that races a pane being set up waits for it as well; a door that answers anything
    //    but 200 does not make one, and the guard goes with it.
    let pane = (method == Method::GET && route == "events").then(|| target.panes.open());

    // e. Transport. A POST gets a deadline for the response head; a GET (a stream) does not.
    let exchange = exchange(target.port, upstream);
    let exchanged = if method == Method::POST {
        tokio::time::timeout(HEAD, exchange).await.unwrap_or(Err(Exchange::Timeout))
    } else {
        exchange.await
    };
    let (response, conn) = match exchanged {
        Ok(done) => done,
        Err(why) => {
            tracing::debug!(session = id, ?why, "perch: the door did not take the request");
            // The limit tripping is the page's fault and has a body-less answer of its own. Anything
            // else is the door: gone (the state says so) or just not answering.
            if limit_hit.load(Ordering::SeqCst) {
                return status(StatusCode::PAYLOAD_TOO_LARGE);
            }
            return match ctx.registry.lookup(id) {
                Lookup::Dead => dead(),
                _ => problem(StatusCode::BAD_GATEWAY, "the door did not answer"),
            };
        }
    };

    // f. The response. A door's 401 is the perch's problem and not the page's.
    let (head, incoming) = response.into_parts();
    if head.status == StatusCode::UNAUTHORIZED {
        drop(incoming);
        conn.close().await;
        return problem(StatusCode::BAD_GATEWAY, "door refused the perch");
    }
    let mut answer = Response::builder().status(head.status);
    if let Some(kind) = head.headers.get(CONTENT_TYPE) {
        answer = answer.header(CONTENT_TYPE, kind.clone());
    }
    // The door's `cache-control` is not copied: whatever the door says, a proxied answer is
    // never cached, and an SSE stream least of all.
    answer = answer.header("cache-control", "no-store");

    // A pane's connection belongs to the pane's own task (see [`Pane`]); any other response keeps
    // it in the body.
    let pane = if head.status == StatusCode::OK { pane } else { None };
    let (conn, pane) = match pane {
        Some(guard) => (None, Some(Pane::new(id, guard, &target.panes, target.exit.clone(), conn))),
        None => (Some(conn), None),
    };
    let body = Proxied::new(incoming, conn, pane);
    match answer.body(body.boxed_unsync()) {
        Ok(response) => response,
        Err(_) => problem(StatusCode::BAD_GATEWAY, "the door did not answer"),
    }
}

/// The 503 of a door that is gone: no connection was made.
fn dead() -> Response<BoxBody> {
    json(StatusCode::SERVICE_UNAVAILABLE, &serde_json::json!({ "state": "dead" }))
}

#[derive(Debug)]
enum Exchange {
    Connect,
    Send,
    Timeout,
}

/// A fresh connection to the door, the request on it, and the response head. Everything after
/// the head is the body's, and the connection's task goes with it.
async fn exchange(port: u16, request: Request<Upstream>) -> Result<(Response<Incoming>, Conn), Exchange> {
    let stream = match tokio::time::timeout(CONNECT, TcpStream::connect((Ipv4Addr::LOCALHOST, port))).await {
        Ok(Ok(stream)) => stream,
        _ => return Err(Exchange::Connect),
    };
    let (mut sender, connection) = hyper::client::conn::http1::handshake::<_, Upstream>(TokioIo::new(stream)).await.map_err(|_| Exchange::Connect)?;
    // Hyper's connection moves nothing unless it is polled. Held by [`Conn`], which aborts
    // it on drop, so a timeout or an error here cannot leave a socket behind.
    let task = tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::debug!(error = %e, "perch: a proxied connection to a door ended");
        }
    });
    let mut conn = Conn { sender: None, task: Some(task) };
    match sender.send_request(request).await {
        Ok(response) => {
            // Kept alive for as long as the response body is: a sender dropped early would
            // let hyper close the connection under a stream that is still being read.
            conn.sender = Some(sender);
            Ok((response, conn))
        }
        Err(e) => {
            tracing::debug!(error = %e, "perch: a door did not answer a proxied request");
            Err(Exchange::Send)
        }
    }
}

/// The connection to one door, as long as one response lives.
struct Conn {
    sender: Option<SendRequest<Upstream>>,
    task: Option<JoinHandle<()>>,
}

impl Conn {
    /// Abort the connection's task and wait until it is gone. The socket is closed when the
    /// task's future is *dropped*, and awaiting the handle is what makes that have happened
    /// before this returns: the watcher's trick, for the same reason.
    async fn close(mut self) {
        self.sender = None;
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// The pane half of a response body: what its last frame will say, and the task that owns the
/// door-side half of it.
///
/// **The pane's own task.** hyper does not poll a response body while its write buffer to the
/// page is full, so a page that has stopped reading (a frozen or background tab on a busy
/// session) would never let `Proxied::poll_frame` see a stop's cancellation. So the cancellation
/// is not left to the body: each pane has a task that owns the upstream [`Conn`] and the count's
/// [`PaneGuard`], waits for the stop (or for the body to say it is over), then closes the socket
/// *and waits for it*, and only then lets the count fall. A pane nobody is reading is therefore
/// still closed, and its `hub-stream-end` frame simply goes undelivered: that page is not
/// listening.
struct Pane {
    session: String,
    panes: Arc<Panes>,
    exit: ExitWatch,
    closing: Pin<Box<WaitForCancellationFutureOwned>>,
    /// Dropping this (the body is over, or the page went away) tells the task to finish.
    ended: DropGuard,
    task: JoinHandle<()>,
}

impl Pane {
    fn new(session: &str, guard: PaneGuard, panes: &Arc<Panes>, exit: ExitWatch, conn: Conn) -> Self {
        let ended = CancellationToken::new();
        let (stop, over) = (panes.closing(), ended.clone());
        let task = tokio::spawn(async move {
            tokio::select! {
                _ = stop => {}
                _ = over.cancelled() => {}
            }
            // The socket first, and really closed; then the count.
            conn.close().await;
            drop(guard);
        });
        Self { session: session.to_string(), panes: panes.clone(), exit, closing: Box::pin(panes.closing()), ended: ended.drop_guard(), task }
    }
}

/// hub.md §5's last frame. `cancelled` is "this pane was told to end by a stop".
///
/// | `door`     | when |
/// |------------|------|
/// | `stopping` | the perch was asked to stop this door (a stop, a shutdown, a cancelled pane) |
/// | `dead`     | otherwise, if the door's exit is published within [`SETTLE`] |
/// | `live`     | otherwise: the door ended the stream and is still there (`lagged`) |
async fn end_frame(session: &str, panes: &Panes, exit: &ExitWatch, cancelled: bool) -> Bytes {
    let (door, exit) = if cancelled || panes.stop_asked() {
        ("stopping", *exit.borrow())
    } else {
        let mut exit = exit.clone();
        match tokio::time::timeout(SETTLE, sessions::wait_exit(&mut exit)).await {
            Ok(seen) => ("dead", Some(seen)),
            Err(_) => ("live", None),
        }
    };
    let exit = exit.filter(|exit| exit.code.is_some() || exit.signal.is_some()).map(|exit| exit.to_string());
    let frame = serde_json::json!({ "type": "hub-stream-end", "session": session, "door": door, "exit": exit });
    // The leading blank line ends a door frame that was cut in half; the trailing one ends this.
    Bytes::from(format!("\n\ndata: {frame}\n\n"))
}

/// A door's response, handed to the page as it arrives.
///
/// A hand-written body rather than a stream combinator because it has three jobs no combinator
/// has together: pass each upstream frame on untouched (no buffering, no re-chunking, no
/// parsing), stop at once when a stop cancels the pane, and, after the upstream is over, close
/// the upstream socket *and wait for it* before emitting the one extra frame. Upstream errors
/// are end-of-stream, never a panic and never an error to hyper (`Error = Infallible`).
struct Proxied {
    upstream: Option<Incoming>,
    conn: Option<Conn>,
    pane: Option<Pane>,
    phase: Phase,
}

enum Phase {
    Streaming,
    /// The upstream is over; the future closes its socket and yields the final frame, if any.
    Finishing(Pin<Box<dyn Future<Output = Option<Bytes>> + Send>>),
    Done,
}

impl Proxied {
    fn new(upstream: Incoming, conn: Option<Conn>, pane: Option<Pane>) -> Self {
        Self { upstream: Some(upstream), conn, pane, phase: Phase::Streaming }
    }

    fn finish(&mut self, cancelled: bool) {
        // The body first, then the connection: the order a socket is closed in.
        self.upstream = None;
        let conn = self.conn.take();
        let pane = self.pane.take();
        self.phase = Phase::Finishing(Box::pin(async move {
            if let Some(conn) = conn {
                conn.close().await;
            }
            let Pane { session, panes, exit, ended, task, .. } = pane?;
            // The pane's task closes the socket and then lets the count fall, which is what a stop
            // waits for; working out what to say about the door can take up to SETTLE, and must not.
            drop(ended);
            let _ = task.await;
            Some(end_frame(&session, &panes, &exit, cancelled).await)
        }));
    }
}

impl Body for Proxied {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = &mut *self;
        loop {
            match &mut this.phase {
                Phase::Streaming => {
                    // The cancellation first, so a stop is not held up behind a busy stream.
                    if this.pane.as_mut().is_some_and(|pane| pane.closing.as_mut().poll(cx).is_ready()) {
                        this.finish(true);
                        continue;
                    }
                    let Some(upstream) = this.upstream.as_mut() else {
                        this.phase = Phase::Done;
                        continue;
                    };
                    match Pin::new(upstream).poll_frame(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Some(Ok(frame))) => return Poll::Ready(Some(Ok(frame))),
                        // The end of the door's stream, or an error on it: the same thing to a page.
                        Poll::Ready(Some(Err(_))) | Poll::Ready(None) => this.finish(false),
                    }
                }
                Phase::Finishing(closing) => match closing.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Some(last)) => {
                        this.phase = Phase::Done;
                        return Poll::Ready(Some(Ok(Frame::data(last))));
                    }
                    Poll::Ready(None) => {
                        this.phase = Phase::Done;
                        return Poll::Ready(None);
                    }
                },
                Phase::Done => return Poll::Ready(None),
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.phase {
            Phase::Done => true,
            // An answer with no body (a 204, a `Content-Length: 0`) is over before it starts.
            Phase::Streaming => self.pane.is_none() && self.upstream.as_ref().is_some_and(|upstream| upstream.is_end_stream()),
            Phase::Finishing(_) => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        // The door's own length for an ordinary answer, so a page sees a `Content-Length` and
        // not chunking it did not ask for; a pane's length is never known.
        match (&self.upstream, &self.pane) {
            (Some(upstream), None) => upstream.size_hint(),
            _ => SizeHint::default(),
        }
    }
}

impl Drop for Proxied {
    /// The page went away mid-body (a closed tab, a cancelled fetch). The upstream socket is
    /// closed and awaited from a task, and the pane count falls only once it is: a count of
    /// zero must mean the door has no stream of this pane's left open.
    fn drop(&mut self) {
        self.upstream = None;
        // A pane's task is told by dropping the pane and does its own closing; only a plain
        // response's connection is closed from here.
        self.pane = None;
        let Some(conn) = self.conn.take() else { return };
        let cleanup = async move { conn.close().await };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(cleanup);
            }
            // No runtime left to wait on: the drops that follow abort the task and release the count.
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_route_is_a_shape_and_not_a_list_of_names() {
        // The five the door has today, the one it will grow, and two that nothing names.
        for route in ["events", "say", "answer", "cancel", "launch", "commands", "a", "a-b", "a1/b-2/c3", &format!("a{}", "b".repeat(31))] {
            assert!(route_ok(route), "{route} is a route");
        }
        for route in [
            "",
            "/",
            "Say",
            "1a",
            "-a",
            "a_b",
            "a.b",
            "a//b",
            "a/",
            "/a",
            "a/../b",
            "a/./b",
            "a b",
            "é",
            &format!("a{}", "b".repeat(32)),
            &format!("a/{}", "b".repeat(33)),
        ] {
            assert!(!route_ok(route), "{route:?} is not a route");
        }
    }

    #[test]
    fn only_the_exact_path_shape_names_a_session_route() {
        assert_eq!(parse("/s/ctf-pwn-3/api/events"), Some(("ctf-pwn-3", "events")));
        assert_eq!(parse("/s/a.b_c/api/x/y-z"), Some(("a.b_c", "x/y-z")));
        for path in [
            "/s",
            "/s/",
            "/s/x",
            "/s/x/",
            "/s/x/api",
            "/s/x/api/",
            "/s/x/x/events",
            "/s/x/api/Say",
            "/s/x/api/a//b",
            "/s/x/api/a/../b",
            "/s//api/events",
            "/s/../api/events",
            "/s/-x/api/events",
            "/s/x/api/%65vents",
            "/s/%78/api/events",
            "/%73/x/api/events",
            "/hub/x/api/events",
        ] {
            assert!(parse(path).is_none(), "{path} is not a session route");
        }
    }

    #[tokio::test]
    async fn the_last_frame_is_a_blank_line_one_json_line_and_a_blank_line() {
        let panes = Panes::new();
        let (publish, exit) = tokio::sync::watch::channel(None);
        // A door that is still there when its stream ends: `live`, no exit.
        let frame = end_frame("s1", &panes, &exit, false).await;
        let text = std::str::from_utf8(&frame).unwrap();
        assert!(text.starts_with("\n\ndata: {") && text.ends_with("}\n\n"), "{text:?}");
        let json: serde_json::Value = serde_json::from_str(text.trim().strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!((json["type"].as_str(), json["session"].as_str(), json["door"].as_str()), (Some("hub-stream-end"), Some("s1"), Some("live")));
        assert!(json["exit"].is_null());
        assert_eq!(text.trim().lines().count(), 1, "one line of JSON");

        // An exit published before the settle ends: `dead`, with its code, or its signal.
        publish.send(Some(sessions::Exit { code: Some(1), signal: None })).unwrap();
        let json = last_json(&end_frame("s1", &panes, &exit, false).await);
        assert_eq!((json["door"].as_str(), json["exit"].as_str()), (Some("dead"), Some("code:1")));
        publish.send(Some(sessions::Exit { code: None, signal: Some(9) })).unwrap();
        let json = last_json(&end_frame("s1", &panes, &exit, false).await);
        assert_eq!((json["door"].as_str(), json["exit"].as_str()), (Some("dead"), Some("signal:9")));

        // A cancelled pane is `stopping` whatever the exit says, and so is a stop that was asked.
        let json = last_json(&end_frame("s1", &panes, &exit, true).await);
        assert_eq!(json["door"], "stopping");
    }

    fn last_json(frame: &Bytes) -> serde_json::Value {
        serde_json::from_str(std::str::from_utf8(frame).unwrap().trim().strip_prefix("data: ").unwrap()).unwrap()
    }
}
