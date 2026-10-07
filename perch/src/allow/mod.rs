//! "Allow for the rest of this chat": rules the operator makes from a pending ask, and the answerer that applies them with no page open
//! (slice A1b; the watcher that feeds it is A1a's).
//!
//! The watcher reads every door's asks ([`crate::watch`]) and tells a [`watch::Sink`] about them; [`State`] is that sink, and
//! `serve::door_up` hands `ctx.allow` to `watch::open` for every door the perch starts, so no hook was needed anywhere else. The three
//! routes are session verbs, handed over by `serve` before the body is read, each `(ctx, req, id) -> Response` with `id` already passed
//! by `sessions::id_ok`:
//!
//! ```text
//!   GET  /hub/sessions/<id>/allow          200 {mode, rules: [{rid, tool, origin, command?, cwd?, added_ms, uses, last_ms?}]}
//!   POST /hub/sessions/<id>/allow {ask_id} 201 {rule, answered: "yes" | "lost"}  (200 when the same rule was there already)
//!   POST /hub/sessions/<id>/revoke {rid}   204
//! ```
//!
//! **Every refusal**, so a review can test each one:
//!
//! ```text
//!   404 (empty)   the chat is not a live door of this perch (kept, starting, stopping, dead, unknown, a bad id); POST /revoke: no such rid
//!   400           a body that is not {ask_id: u64} / {rid: u64}
//!   413           a body over 4 KiB (the gate's own 413 for a declared length over 8 MiB comes first)
//!   409           the ask is not pending (settled, never read, too long to have been read, or the watcher has ended); it is a question; the
//!                 chat is yolo (the door answers itself); 32 rules already; another answer for this ask is already on its way
//!   422           an approval no rule may cover: eidolon asks about the call's shape and the rule would be by name; a bash call that is not
//!                 plainly in the foreground; a call whose origin is unknown; a bash call with a field beyond command/timeout_s/background;
//!                 an ask that names no tool or whose command is not a string
//!   502           the door did not take the answer (unreachable, a status other than 204/409, no reply in 5 s): no rule is kept
//!   503           the perch is stopping
//! ```
//!
//! **What a request can shape.** `POST /allow` reads `ask_id` and nothing else: any other field in the body is ignored, so a page (or
//! anything holding the master token) cannot make a rule for a tool, a command or an origin the door did not ask about. The rule is built
//! from the watcher's own copy of that ask ([`watch::AskView::ask`]): its tool, its input, the origin joined from the call's own frame.
//! The perch then answers that ask `yes` itself, with [`RULE_NOTE`] as the note, so the settled row in a chat says why.
//!
//! **Lifetime** (D10 a). Rules are in memory ([`store`]), each under the [`DoorKey`] alive when it was made. A stop, a model or mode
//! switch (a stop and a start), a crash or a goodbye ends the door's watcher, the sink is told `closed`, and the rules go with it. A
//! perch restart starts with none. Nothing is read from or written to a disk, a test holds it, and a fork is a new door with none.
//!
//! **Matching** is [`matcher`]'s: pure, byte-exact, fail closed. This module's part is to feed it only live facts: the ask re-read from
//! the watcher at the moment of deciding (never the note the sink was told), the chat's mode now, and the door's cwd from `hello`. An ask
//! is answered once: [`store::Door::claim`] keeps a door from handing the same ask to two answerers, whatever the watcher tells.
//!
//! **The answerer** is one `POST /api/answer {ask_id, answer: "yes", note}` to the door, with the door's Bearer, on a task of its own
//! that selects on the perch's stop (`ctx.stop`), so a shutdown never waits on it, and is bounded in time. `204` is done; `409` means the
//! page (or another rule) won and is not an error. The door's token is read from the registry per answer and is in no log line, no
//! error and no `Debug`; the log names the ask id, the rule id and the tool, never the command.
//!
//! **Binding a key to a chat.** The sink is told a [`DoorKey`], never a chat id (the watcher opens before the registry row exists). The
//! registry is reached through the first `POST /allow` (`State::host`): there is no rule before one, so there is nothing to match
//! before it, and the sink needs no hook anywhere. `sessions::Registry::id_of_pid` binds a key to its chat for the mode and the token.

pub mod matcher;
pub mod store;

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use self::matcher::DoorFacts;
use self::store::{Added, New, Refused, Rule, Store};
use crate::archive::now_ms;
use crate::mode::Mode;
use crate::serve::{BoxBody, Ctx, body_of, json, problem, status};
use crate::sessions::{Lookup, Registry};
use crate::watch::{self, AskNote, AskView, DoorKey};

/// The note a rule's answer carries. The model sees it (it rides in the tool result), so it says what happened and nothing else. ASCII,
/// well under the door's 1024-byte limit. The page recognises a settled row by this exact text.
pub const RULE_NOTE: &str = "allowed by the operator's rule for this chat (Minerva)";

/// A request body: `{ask_id}` or `{rid}` and nothing a person would write more of.
const BODY_CAP: usize = 4 * 1024;

/// How long the perch waits to connect to a door, and for the whole answer. A door is a process on this box.
const CONNECT: Duration = Duration::from_secs(2);
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

const SHAPE_ALLOW: &str = "expected a JSON body of {\"ask_id\": <the ask's number>}";
const SHAPE_REVOKE: &str = "expected a JSON body of {\"rid\": <the rule's number>}";
const NOT_PENDING: &str = "that ask is not pending (it was settled, or never read)";
const NOT_WATCHED: &str = "that door is not being watched";
const YOLO: &str = "a yolo chat answers every ask itself; there is nothing to allow";
const FULL: &str = "this chat has 32 rules already; revoke one first";
const TAKEN: &str = "that ask is already being answered";
const NOT_TAKEN: &str = "the door did not take the answer, so no rule was made";
const NOT_TAKEN_SAME: &str = "the door did not take the answer";

/// What the sink needs from the rest of the perch, and gets from the first `POST /allow`: before one there is no rule, so no ask is ever
/// matched without it. `registry` binds a key to its chat and gives the token; `stop` is what the answerer selects on.
struct Host {
    registry: Arc<Registry>,
    stop: CancellationToken,
}

/// The rules, and the watch sink that applies them. `Default` is how [`Ctx`] gets one before any hook runs.
#[derive(Default)]
pub struct State {
    /// Shared with the answerer tasks, which count the rule's use when the door has taken the answer. One short lock, never held across an
    /// await or while the registry is asked anything.
    store: Arc<Mutex<Store>>,
    host: OnceLock<Host>,
}

fn locked(store: &Mutex<Store>) -> MutexGuard<'_, Store> {
    // A panic in a sink method is contained by the watcher (it loses that one event); the store is plain data and stays usable.
    store.lock().unwrap_or_else(PoisonError::into_inner)
}

impl State {
    fn store(&self) -> MutexGuard<'_, Store> {
        locked(&self.store)
    }

    /// An ask was told to the sink: answer it if a rule says so. Runs on a blocking thread off the watcher's read loop.
    fn decide(&self, key: DoorKey, ask_id: u64) {
        // No `POST /allow` has run, so no rule exists.
        let Some(host) = self.host.get() else { return };
        // A blocking thread of the runtime that spawned it: the answer is async work, started here and not waited for.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        let (view, cwd, yolo) = {
            let store = self.store();
            let Some(door) = store.door(key) else { return };
            if door.rules().is_empty() {
                return;
            }
            let Some(view) = door.view().cloned() else { return };
            (view, door.cwd().to_path_buf(), door.yolo())
        };
        if view.is_closed() {
            return;
        }
        // The watcher's copy now, with the origin it has now: not the note the sink was told.
        let Some(ask) = view.ask(ask_id) else { return };
        let Some((token, registry_mode)) = door_of_key(&host.registry, key) else { return };
        let facts = DoorFacts { key, mode: effective(yolo, registry_mode), cwd: &cwd };
        let rid = {
            let mut store = self.store();
            let Some(door) = store.door_mut(key) else { return };
            let Some(rid) = matcher::find(door.rules(), &ask, &facts) else { return };
            if !door.claim(ask_id) {
                return;
            }
            rid
        };
        if view.is_closed() {
            return;
        }
        // Ids, the rule and the tool's name only: the call's input is the operator's command line, which can hold a secret.
        tracing::info!(pid = key.pid, port = key.port, ask_id, rid, tool = ask.tool.as_deref().unwrap_or("-"), "perch: a rule answers an ask");
        let (store, stop) = (self.store.clone(), host.stop.clone());
        runtime.spawn(async move {
            if answer(key.port, &token, ask_id, &stop).await == Answered::Done {
                locked(&store).used(key, rid, now_ms());
            }
        });
    }

    /// `POST /allow`: the checks, the rule, and the answer to the ask it was made from.
    async fn make(&self, ctx: &Ctx, live: Live, ask_id: u64) -> Response<BoxBody> {
        let host = self.host.get_or_init(|| Host { registry: ctx.registry.clone(), stop: ctx.stop.clone() });
        let key = live.key;
        let (view, cwd, yolo) = {
            let store = self.store();
            let Some(door) = store.door(key) else { return problem(StatusCode::CONFLICT, NOT_WATCHED) };
            let Some(view) = door.view().cloned() else { return problem(StatusCode::CONFLICT, NOT_WATCHED) };
            (view, door.cwd().to_path_buf(), door.yolo())
        };
        if !matches!(effective(yolo, live.mode), Mode::Auto | Mode::Manual) {
            return problem(StatusCode::CONFLICT, YOLO);
        }
        if view.is_closed() {
            return problem(StatusCode::CONFLICT, NOT_PENDING);
        }
        let Some(ask) = view.ask(ask_id) else { return problem(StatusCode::CONFLICT, NOT_PENDING) };
        let pattern = match matcher::pattern_of(&ask, &cwd) {
            Ok(pattern) => pattern,
            Err(why) => return problem(StatusCode::from_u16(why.status()).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY), why.why()),
        };
        // Read after the ask, so a stamp is never lower than the ask's own: an ask that opens in between is left to the person.
        let new = New { mode: effective(yolo, live.mode), stamp: view.seq(), max_ask: view.max_ask_id(), pattern, now_ms: now_ms() };
        let added = self.store().add(key, new, ask_id);
        let added = match added {
            Ok(added) => added,
            Err(Refused::NoDoor) => return problem(StatusCode::CONFLICT, NOT_WATCHED),
            Err(Refused::Full) => return problem(StatusCode::CONFLICT, FULL),
            Err(Refused::Taken) => return problem(StatusCode::CONFLICT, TAKEN),
        };
        let (rule, created) = (added.rule().clone(), added.made());
        // On a task of its own, so a page that hangs up mid-request cannot leave a rule made and its ask half answered: the task finishes
        // and tidies up either way. It selects on the perch's stop, so the shutdown does not wait for it.
        let (store, stop, token, rid) = (self.store.clone(), host.stop.clone(), live.token, rule.rid);
        let task = tokio::spawn(async move {
            let outcome = answer(key.port, &token, ask_id, &stop).await;
            if matches!(outcome, Answered::Failed | Answered::Stopped) {
                // The ask is still open, so it goes back to the person, and a rule made for it is not kept.
                let mut store = locked(&store);
                if created {
                    store.revoke(key, rid);
                }
                if let Some(door) = store.door_mut(key) {
                    door.release(ask_id);
                }
            }
            outcome
        });
        let outcome = task.await.unwrap_or(Answered::Failed);
        match reply_of(&added, outcome) {
            (code, Some(body)) => json(code, &body),
            (code, None) => status(code),
        }
    }
}

/// The chat's mode as the matcher needs it: the door's own word on `--yolo` beats the registry's, so a door that says it is yolo never
/// has a rule applied, whatever the registry thinks it was started as.
fn effective(door_yolo: bool, registry: Mode) -> Mode {
    if door_yolo { Mode::Yolo } else { registry }
}

/// What a `POST /allow` came to, as `(status, body)`. The body is `None` for a bare status.
fn reply_of(added: &Added, outcome: Answered) -> (StatusCode, Option<Value>) {
    let rule = added.rule();
    let code = if added.made() { StatusCode::CREATED } else { StatusCode::OK };
    let said = |answered: &str| Some(serde_json::json!({ "rule": rule.json(), "answered": answered }));
    match outcome {
        Answered::Done => (code, said("yes")),
        Answered::Lost => (code, said("lost")),
        Answered::Failed => {
            let why = if added.made() { NOT_TAKEN } else { NOT_TAKEN_SAME };
            (StatusCode::BAD_GATEWAY, Some(serde_json::json!({ "error": why })))
        }
        Answered::Stopped => (StatusCode::SERVICE_UNAVAILABLE, None),
    }
}

impl watch::Sink for State {
    fn hello(&self, key: DoorKey, hello: &watch::Hello) {
        self.store().bind(key, hello.cwd.clone(), hello.yolo);
    }

    fn watching(&self, key: DoorKey, asks: AskView) {
        self.store().watch(key, asks);
    }

    fn ask(&self, key: DoorKey, ask: &AskNote) {
        self.decide(key, ask.ask_id);
    }

    fn closed(&self, key: DoorKey) {
        let ended = self.store().end(key);
        if ended > 0 {
            tracing::info!(pid = key.pid, port = key.port, rules = ended, "perch: a door ended; its allow rules ended with it");
        }
    }
}

// ---------------------------------------------------------------------------
// The registry's side: a chat and its door
// ---------------------------------------------------------------------------

/// A chat's live door as the routes need it.
struct Live {
    key: DoorKey,
    token: Arc<str>,
    /// The mode the registry has for the chat (the door's own `yolo` word is [`store::Door::yolo`]).
    mode: Mode,
}

/// The live door of chat `id`, or `None` when the chat is not one (kept, starting, stopping, dead, unknown). Two reads of the registry:
/// if the door changed between them the key names no door in the store, and the caller's lookup there comes back empty.
fn door_of_chat(registry: &Registry, id: &str) -> Option<Live> {
    let Lookup::Live(target) = registry.lookup(id) else { return None };
    let pid = registry.door_pids().into_iter().find_map(|(chat, pid)| (chat == id).then_some(pid))?;
    Some(Live { key: DoorKey { pid, port: target.port }, token: target.token, mode: registry.known_mode(id)? })
}

/// The token and registry mode of the live door a key names, or `None` when that door is no longer the chat's (stopping, gone, or a later
/// door on another port).
fn door_of_key(registry: &Registry, key: DoorKey) -> Option<(Arc<str>, Mode)> {
    let id = registry.id_of_pid(key.pid)?;
    let Lookup::Live(target) = registry.lookup(&id) else { return None };
    if target.port != key.port {
        return None;
    }
    Some((target.token, registry.known_mode(&id)?))
}

// ---------------------------------------------------------------------------
// The answerer
// ---------------------------------------------------------------------------

/// How an answer came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answered {
    /// `204`: the door took the answer.
    Done,
    /// `409`: the ask was already settled; the page, or another rule, won. Not an error.
    Lost,
    /// The door could not be reached, said anything else, or did not reply in time. The ask is still open.
    Failed,
    /// The perch is stopping, and the answer was dropped.
    Stopped,
}

/// `POST /api/answer {ask_id, answer: "yes", note: RULE_NOTE}` to the door on `port`. Selects on `stop`, and is over in
/// [`ANSWER_WITHIN`] whatever the door does.
pub async fn answer(port: u16, token: &str, ask_id: u64, stop: &CancellationToken) -> Answered {
    answer_within(port, token, ask_id, stop, ANSWER_WITHIN).await
}

async fn answer_within(port: u16, token: &str, ask_id: u64, stop: &CancellationToken, within: Duration) -> Answered {
    let posted = tokio::select! {
        // A stop that has happened wins, so an answer is not even tried once the perch is stopping.
        biased;
        _ = stop.cancelled() => return Answered::Stopped,
        posted = tokio::time::timeout(within, post_answer(port, token, ask_id)) => posted,
    };
    match posted {
        Ok(Ok(code)) if code == StatusCode::NO_CONTENT || code == StatusCode::OK => Answered::Done,
        Ok(Ok(StatusCode::CONFLICT)) => {
            tracing::debug!(port, ask_id, "perch: an answer for a rule came second; the ask was settled already");
            Answered::Lost
        }
        Ok(Ok(code)) => {
            tracing::warn!(port, ask_id, status = code.as_u16(), "perch: a door refused the answer a rule sent");
            Answered::Failed
        }
        // The error says what went wrong on the socket; the request, with the door's token in it, is not part of it.
        Ok(Err(e)) => {
            tracing::warn!(port, ask_id, error = format!("{e:#}").as_str(), "perch: a door could not be asked to take an answer");
            Answered::Failed
        }
        Err(_) => {
            tracing::warn!(port, ask_id, "perch: a door did not answer an answer in time");
            Answered::Failed
        }
    }
}

/// One request on one connection, dropped when this returns (or is dropped), so no socket outlives the answer: a door with a connection
/// open runs its graceful shutdown and does not end on SIGTERM until it closes.
async fn post_answer(port: u16, token: &str, ask_id: u64) -> anyhow::Result<StatusCode> {
    let socket = match tokio::time::timeout(CONNECT, TcpStream::connect((Ipv4Addr::LOCALHOST, port))).await {
        Ok(socket) => socket.with_context(|| format!("connecting to the door on 127.0.0.1:{port}"))?,
        Err(_) => anyhow::bail!("the door on 127.0.0.1:{port} did not accept a connection within {CONNECT:?}"),
    };
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket)).await.context("the HTTP/1.1 handshake with the door")?;
    tokio::pin!(connection);
    let mut bearer = HeaderValue::from_str(&format!("Bearer {token}")).context("the door's token is not a header value")?;
    // Never shown by a debug print of the request, the way a token must not be.
    bearer.set_sensitive(true);
    let body = serde_json::to_vec(&serde_json::json!({ "ask_id": ask_id, "answer": "yes", "note": RULE_NOTE })).context("the answer is JSON")?;
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/answer")
        .header(AUTHORIZATION, bearer)
        .header(HOST, format!("127.0.0.1:{port}"))
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .context("building the answer")?;
    let sending = sender.send_request(request);
    tokio::pin!(sending);
    // The connection has to be polled for the response to arrive at all. When it ends first (a door that closes right behind its reply),
    // whatever it delivered on the way out is still in `sending`: asking it again is the answer, an error if there was none.
    let response = tokio::select! {
        response = &mut sending => response,
        _ = &mut connection => (&mut sending).await,
    };
    Ok(response.context("asking the door to take the answer")?.status())
}

// ---------------------------------------------------------------------------
// The routes
// ---------------------------------------------------------------------------

/// The unsigned number a body's `key` holds, and nothing else of the body: the body must be a JSON object, so `[1]` (which serde would
/// read as a struct's one field) is a refusal, and so is a number that is negative, fractional, huge or in a string.
fn number_in(body: &[u8], key: &str) -> Option<u64> {
    serde_json::from_slice::<Value>(body).ok()?.as_object()?.get(key)?.as_u64()
}

/// `GET /hub/sessions/<id>/allow`.
pub async fn list(ctx: &Ctx, _req: Request<Incoming>, id: &str) -> Response<BoxBody> {
    let Some(live) = door_of_chat(&ctx.registry, id) else { return status(StatusCode::NOT_FOUND) };
    let shown = {
        let store = ctx.allow.store();
        let Some(door) = store.door(live.key) else { return status(StatusCode::NOT_FOUND) };
        let rules: Vec<Value> = door.rules().iter().map(Rule::json).collect();
        serde_json::json!({ "mode": effective(door.yolo(), live.mode).as_str(), "rules": rules })
    };
    json(StatusCode::OK, &shown)
}

/// `POST /hub/sessions/<id>/allow {ask_id}`. The chat is judged before the body is read, so a request for a chat that has no door costs
/// nothing.
pub async fn create(ctx: &Ctx, req: Request<Incoming>, id: &str) -> Response<BoxBody> {
    let Some(live) = door_of_chat(&ctx.registry, id) else { return status(StatusCode::NOT_FOUND) };
    let bytes = match body_of(req, BODY_CAP).await {
        Ok((_, bytes)) => bytes,
        Err(refusal) => return *refusal,
    };
    // Read for `ask_id` only: every other field is ignored, so nothing else in the body reaches the rule.
    let Some(ask_id) = number_in(&bytes, "ask_id") else { return problem(StatusCode::BAD_REQUEST, SHAPE_ALLOW) };
    ctx.allow.make(ctx, live, ask_id).await
}

/// `POST /hub/sessions/<id>/revoke {rid}`. A revoke stops a rule from answering anything it has not answered yet; an answer already on its
/// way is not recalled.
pub async fn revoke(ctx: &Ctx, req: Request<Incoming>, id: &str) -> Response<BoxBody> {
    let Some(live) = door_of_chat(&ctx.registry, id) else { return status(StatusCode::NOT_FOUND) };
    let bytes = match body_of(req, BODY_CAP).await {
        Ok((_, bytes)) => bytes,
        Err(refusal) => return *refusal,
    };
    let Some(rid) = number_in(&bytes, "rid") else { return problem(StatusCode::BAD_REQUEST, SHAPE_REVOKE) };
    if ctx.allow.store().revoke(live.key, rid) {
        tracing::info!(pid = live.key.pid, port = live.key.port, rid, "perch: an allow rule was revoked");
        status(StatusCode::NO_CONTENT)
    } else {
        status(StatusCode::NOT_FOUND)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use super::matcher::{Pattern, Shape};
    use super::*;
    use crate::watch::Origin;

    /// What a body is read for: one unsigned number under one key of a JSON object. A list is not an object (serde would have read `[7]` as
    /// a struct's one field), and a number that is signed, fractional, an exponent, too big or quoted is not a number the door issued.
    #[test]
    fn a_body_is_read_for_one_unsigned_number_in_an_object() {
        for ok in [r#"{"ask_id":7}"#, r#"{"ask_id":7,"tool":"write","command":"rm -rf /"}"#, r#" { "ask_id" : 18446744073709551615 } "#] {
            assert!(number_in(ok.as_bytes(), "ask_id").is_some(), "{ok}");
        }
        assert_eq!(number_in(br#"{"ask_id":7,"rid":9}"#, "rid"), Some(9));
        assert_eq!(number_in(br#"{"ask_id":1,"ask_id":2}"#, "ask_id"), Some(2), "the last of a repeated key, as every JSON reader has it");
        let bad = [
            "", "7", "[7]", "[7,8]", "null", r#"{"ask_id":"7"}"#, r#"{"ask_id":-7}"#, r#"{"ask_id":7.5}"#, r#"{"ask_id":7.0}"#, r#"{"ask_id":1e3}"#,
            r#"{"ask_id":null}"#, r#"{"ask_id":18446744073709551616}"#, r#"{"rid":7}"#, r#"{"ask_id":[7]}"#, r#"{"ask_id":{"ask_id":7}}"#, "{",
            r#"{"ask_id":7}x"#,
        ];
        for body in bad {
            assert_eq!(number_in(body.as_bytes(), "ask_id"), None, "{body}");
        }
    }

    #[test]
    fn the_note_is_the_one_the_page_looks_for() {
        assert_eq!(RULE_NOTE, "allowed by the operator's rule for this chat (Minerva)");
        assert!(RULE_NOTE.is_ascii() && RULE_NOTE.len() <= 1024);
    }

    fn made(command: &str) -> Added {
        let mut store = Store::default();
        let key = DoorKey { pid: 1, port: 2 };
        store.bind(key, "/w".into(), false);
        let pattern = Pattern { tool: "bash".into(), origin: Origin::Model, shape: Shape::Command { command: command.into(), cwd: "/w".into() } };
        store.add(key, New { mode: Mode::Auto, stamp: 0, max_ask: 0, pattern, now_ms: 5 }, 1).unwrap()
    }

    #[test]
    fn each_way_an_answer_can_come_out_has_its_own_reply() {
        let added = made("ls");
        let (code, body) = reply_of(&added, Answered::Done);
        assert_eq!(code, StatusCode::CREATED);
        let body = body.unwrap();
        assert_eq!(body["answered"], "yes");
        assert_eq!(body["rule"]["command"], "ls");
        let (code, body) = reply_of(&added, Answered::Lost);
        assert_eq!((code, body.unwrap()["answered"].clone()), (StatusCode::CREATED, serde_json::json!("lost")));
        let (code, body) = reply_of(&added, Answered::Failed);
        assert_eq!(code, StatusCode::BAD_GATEWAY);
        assert!(body.unwrap()["error"].as_str().unwrap().contains("no rule was made"));
        assert_eq!(reply_of(&added, Answered::Stopped), (StatusCode::SERVICE_UNAVAILABLE, None));
        // A rule that was there already is a 200, and a failed answer does not say it was not made.
        let same = Added::Same(added.rule().clone());
        assert_eq!(reply_of(&same, Answered::Done).0, StatusCode::OK);
        assert!(!reply_of(&same, Answered::Failed).1.unwrap()["error"].as_str().unwrap().contains("no rule was made"));
    }

    #[test]
    fn the_mode_the_door_reports_beats_the_registry() {
        assert_eq!(effective(true, Mode::Auto), Mode::Yolo);
        assert_eq!(effective(true, Mode::Manual), Mode::Yolo);
        assert_eq!(effective(false, Mode::Manual), Mode::Manual);
        assert_eq!(effective(false, Mode::Yolo), Mode::Yolo, "the registry's yolo is not softened");
    }

    // The source of this module is read, with comments and test code left out, for anything that touches a file.
    #[test]
    fn the_rules_are_never_written_anywhere() {
        for (name, source) in [("mod.rs", include_str!("mod.rs")), ("store.rs", include_str!("store.rs")), ("matcher.rs", include_str!("matcher.rs"))] {
            let source = source.replace("\r\n", "\n");
            let code = source.split("#[cfg(test)]").next().unwrap();
            for line in code.lines().filter(|line| !line.trim_start().starts_with("//")) {
                for needle in ["std::fs", "tokio::fs", "harnox::fs", "File::", "OpenOptions", "write_atomic", "allow.json", "create_dir", "std::env", "persist"] {
                    assert!(!line.contains(needle), "{name} touches a file or the environment ({needle}): {line}");
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // The answerer, against a door that is a socket
    // -----------------------------------------------------------------------

    fn complete(seen: &[u8]) -> bool {
        let Some(head_end) = seen.windows(4).position(|w| w == b"\r\n\r\n") else { return false };
        let head = String::from_utf8_lossy(&seen[..head_end]).to_ascii_lowercase();
        let length = head.lines().find_map(|line| line.strip_prefix("content-length:")).and_then(|n| n.trim().parse::<usize>().ok()).unwrap_or(0);
        seen.len() >= head_end + 4 + length
    }

    /// A door that reads one request and writes `reply` (`None`: never answers, and holds the connection open). The task returns the
    /// request as it arrived.
    async fn door(reply: Option<&'static str>) -> (u16, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (mut seen, mut buf) = (Vec::new(), [0u8; 4096]);
            while !complete(&seen) {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0, "the perch closed before it finished the request");
                seen.extend_from_slice(&buf[..n]);
            }
            match reply {
                Some(reply) => socket.write_all(reply.as_bytes()).await.unwrap(),
                None => tokio::time::sleep(Duration::from_secs(30)).await,
            }
            String::from_utf8_lossy(&seen).to_string()
        });
        (port, task)
    }

    const NO_CONTENT: &str = "HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n";
    const CONFLICT: &str = "HTTP/1.1 409 Conflict\r\ncontent-length: 0\r\n\r\n";
    const OOPS: &str = "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\n\r\n";
    const UNOFFERED: &str = "HTTP/1.1 422 Unprocessable Entity\r\ncontent-length: 0\r\n\r\n";

    #[tokio::test]
    async fn an_answer_is_one_post_with_the_door_s_bearer_and_the_note() {
        let (port, door) = door(Some(NO_CONTENT)).await;
        let stop = CancellationToken::new();
        assert_eq!(answer(port, "door-secret-token", 7, &stop).await, Answered::Done);
        let request = door.await.unwrap();
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        let head = head.to_ascii_lowercase();
        assert!(head.starts_with("post /api/answer http/1.1"), "{head}");
        assert!(head.contains("authorization: bearer door-secret-token"), "{head}");
        assert!(head.contains(&format!("host: 127.0.0.1:{port}")), "{head}");
        assert!(head.contains("content-type: application/json"), "{head}");
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(body, serde_json::json!({ "ask_id": 7, "answer": "yes", "note": RULE_NOTE }));
    }

    #[tokio::test]
    async fn what_the_door_says_decides_whether_the_answer_stands() {
        for (reply, want) in [(NO_CONTENT, Answered::Done), (CONFLICT, Answered::Lost), (OOPS, Answered::Failed), (UNOFFERED, Answered::Failed)] {
            let (port, door) = door(Some(reply)).await;
            assert_eq!(answer(port, "t", 1, &CancellationToken::new()).await, want, "{reply}");
            door.await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_door_that_is_not_there_is_a_failed_answer_and_not_a_hang() {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let began = std::time::Instant::now();
        assert_eq!(answer(port, "t", 1, &CancellationToken::new()).await, Answered::Failed);
        assert!(began.elapsed() < Duration::from_secs(3), "{:?}", began.elapsed());
    }

    #[tokio::test]
    async fn a_door_that_never_replies_costs_the_time_it_is_given_and_a_stop_costs_nothing() {
        let (port, _door) = door(None).await;
        let began = std::time::Instant::now();
        assert_eq!(answer_within(port, "t", 1, &CancellationToken::new(), Duration::from_millis(300)).await, Answered::Failed);
        let waited = began.elapsed();
        assert!((Duration::from_millis(250)..Duration::from_secs(2)).contains(&waited), "{waited:?}");

        let (port, _door) = door(None).await;
        let stop = CancellationToken::new();
        let canceller = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            canceller.cancel();
        });
        let began = std::time::Instant::now();
        assert_eq!(answer(port, "t", 1, &stop).await, Answered::Stopped, "the real bound is 5 s; the stop ended it");
        assert!(began.elapsed() < Duration::from_secs(2), "{:?}", began.elapsed());
        // Already stopped: not even a connection is tried.
        assert_eq!(answer(1, "t", 1, &stop).await, Answered::Stopped);
    }

    #[tokio::test]
    async fn the_door_s_token_is_in_no_error_and_no_debug_print() {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let err = post_answer(port, "door-secret-token", 1).await.unwrap_err();
        assert!(!format!("{err:#}").contains("door-secret-token") && !format!("{err:?}").contains("door-secret-token"));
        let mut bearer = HeaderValue::from_str("Bearer door-secret-token").unwrap();
        bearer.set_sensitive(true);
        assert!(!format!("{bearer:?}").contains("door-secret-token"));
    }
}
