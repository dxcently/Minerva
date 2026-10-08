//! The agent's terminal routes, `POST /hub/term/a/<verb>`: list, open, read, send and close (slice T2).
//!
//! Behind the agent gate ([`crate::agent`]): by the time a request is here, its scoped term token and its socket were checked, and the
//! chat it came from is in the request's extensions as a [`Caller`]. No route reads a chat id from a body, a query or a header.
//!
//! ```text
//!   list   any web terminal                              200 [{id, title, fg, by, chat, mine, alive, hold}]
//!   open   a new one, for this chat, in its folder        201 {id}                    at most 4 per chat (D5)
//!   read   any web terminal, after its wait (below)       200 {text, cursor, seq, alive, exit?, alt_screen}
//!   send   any web terminal, typed as the operator would  200 {seq}                   423 held; 403 the guard (D9)
//!   close  only a terminal this chat opened (D5)          202 {id}                    403 another chat's or the operator's
//! ```
//!
//! **Reach (ruling 2).** `list`, `read` and `send` work on any web terminal, whoever opened it. The operator's stop is the hold
//! ([`super::Terms::set_hold`]): while it is on, an agent's `send` is refused with `423`, and its reads still answer.
//!
//! **Bodies.** One JSON object of named fields, at most [`JSON_BODY`] bytes. A field it does not name is a `400`, so a `cwd` or a `chat`
//! in a body is refused and never honoured. An empty body is `{}`, and the verbs that name no field take nothing else.
//!
//! **The wait (`read`).** The answer is held until one of these, whichever comes first: the text it names is on the screen; the screen
//! has been quiet for `idle_ms` (output and input both count as activity); the time is up; the shell has ended. That is what the tool's
//! own description promises. A wait is woken by output and by its own quiet timer, and a perch stop ends it with a `503`.
//!
//! **The guard (D9).** A `send` whose text names what an agent may not run is refused before a byte is typed, with the guard's sentence
//! and a `403`. [`crate::guard::refuses_term_text`] is a hint, not a gate, and it does not depend on whether the policy rows are on.
//!
//! What a read gives back is untrusted terminal output, and it goes to the model provider. The tool's note says so; nothing here changes it.

use std::time::{Duration, Instant};

use hyper::body::Incoming;
use hyper::header::{HeaderValue, ALLOW, CONTENT_LENGTH};
use hyper::{Method, Request, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::screen::{Cursor, Text, MAX_TEXT_CHARS, MAX_TEXT_LINES};
use super::{now_ms, valid_id, By, Exit, InputError, OpenError, OpenSpec, Owner, Terms, AGENT_CHAT_CAP, MAX_TERMS};
use crate::agent::Caller;
use crate::guard;
use crate::serve::{body_of, json, problem, status, BoxBody, Ctx};

/// The most a call's JSON body may be: a few named fields, so this is generous, and a body over it is a 413 before it is read.
const JSON_BODY: usize = 16 * 1024;
/// The most `send` types, in bytes (the same bound `term_send` states).
const TEXT_BYTES: usize = 4 * 1024;
/// The most named keys one `send` presses.
const KEYS_MAX: usize = 32;
/// The most characters a `wait.for` may name.
const FOR_CHARS: usize = 200;
/// `wait.idle_ms` when none is named, and the most it may be.
const IDLE_MS: u64 = 500;
const IDLE_MAX_MS: u64 = 60_000;
/// `wait.timeout_s` when none is named, and the most it may be (the tool says 60).
const TIMEOUT_S: u64 = 30;
const TIMEOUT_MAX_S: u64 = 60;
/// The lines a read gives when none is named.
const READ_LINES: usize = 100;

const NOT_A_DOOR: &str = "term tools work in chats this Minerva opened";
const NO_TERMINAL: &str = "no such terminal";
const ENDED: &str = "that terminal's shell has ended";
const HELD: &str = "the operator has this terminal on hold: stop and tell them, do not retry";
const STOPPING: &str = "the perch is shutting down";
const NOT_YOURS: &str = "only a terminal this chat opened can be closed";
const KEY_NAMES: &str = "enter, tab, esc, backspace, up, down, left, right, home, end, pgup, pgdn, or ctrl-a to ctrl-z";
const LIST_SHAPE: &str = "expected a JSON object with no fields, or no body";
const OPEN_SHAPE: &str = "expected a JSON object of {\"title\"?, \"cols\"?, \"rows\"?}, or no body";
const READ_SHAPE: &str = "expected a JSON object of {\"id\", \"lines\"?, \"max_chars\"?, \"wait\"?: {\"for\"?, \"idle_ms\"?, \"timeout_s\"?}}";
const SEND_SHAPE: &str = "expected a JSON object of {\"id\", \"text\"?, \"keys\"?}";
const CLOSE_SHAPE: &str = "expected a JSON object of {\"id\"}";

/// A body that names no field (`list`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoFields {}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAsk {
    title: Option<String>,
    cols: Option<u32>,
    rows: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadAsk {
    id: String,
    lines: Option<u64>,
    max_chars: Option<u64>,
    wait: Option<WaitAsk>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitAsk {
    #[serde(rename = "for")]
    for_text: Option<String>,
    idle_ms: Option<u64>,
    timeout_s: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendAsk {
    id: String,
    text: Option<String>,
    keys: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdAsk {
    id: String,
}

/// One row of `list`. `mine` is whether this chat opened the terminal.
#[derive(Serialize)]
struct Row {
    id: String,
    title: String,
    fg: String,
    by: By,
    chat: Option<String>,
    mine: bool,
    alive: bool,
    hold: bool,
}

/// What `read` answers. `exit` is named only once the shell has ended.
#[derive(Serialize)]
struct ReadAnswer {
    text: String,
    cursor: Option<Cursor>,
    seq: u64,
    alive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit: Option<Exit>,
    alt_screen: bool,
}

/// `rest` is the verb, already checked by [`crate::agent::verb`]: lower case, no `/`. The chat comes from the request's extensions,
/// where the gate put it; a call without one is refused as the gate refuses a socket that is no door's.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    let Some(chat) = req.extensions().get::<Caller>().map(|caller| caller.chat.clone()) else {
        return problem(StatusCode::FORBIDDEN, NOT_A_DOOR);
    };
    if *req.method() != Method::POST {
        let mut refusal = status(StatusCode::METHOD_NOT_ALLOWED);
        refusal.headers_mut().insert(ALLOW, HeaderValue::from_static("POST"));
        return refusal;
    }
    let terms = ctx.term.terms(ctx);
    match rest {
        "list" => list(terms, &chat, req).await,
        "open" => open(ctx, terms, &chat, req).await,
        "read" => read(ctx, terms, req).await,
        "send" => send(terms, &chat, req).await,
        "close" => close(terms, &chat, req).await,
        _ => status(StatusCode::NOT_FOUND),
    }
}

/// `POST /hub/term/a/list`: every web terminal, oldest first, ended ones included. 200.
async fn list(terms: &Terms, chat: &str, req: Request<Incoming>) -> Response<BoxBody> {
    if let Err(refusal) = fields::<NoFields>(req, LIST_SHAPE).await {
        return *refusal;
    }
    let rows: Vec<Row> = terms
        .list()
        .into_iter()
        .map(|info| Row {
            mine: Owner { by: info.by, chat: info.chat.clone() }.is_agents_of(chat),
            id: info.id,
            title: info.title,
            fg: info.fg,
            by: info.by,
            chat: info.chat,
            alive: info.alive,
            hold: info.hold,
        })
        .collect();
    json(StatusCode::OK, &rows)
}

/// `POST /hub/term/a/open {title?, cols?, rows?}`: a terminal for this chat, started in the folder the chat was started in (`$HOME`
/// when the perch remembers none). At most [`AGENT_CHAT_CAP`] of the chat's stay open: an ended one that was not closed counts until
/// the chat wants another, and then its own oldest is let go. 201 `{id}`. A `cwd` is a 400: the field is not named.
async fn open(ctx: &Ctx, terms: &Terms, chat: &str, req: Request<Incoming>) -> Response<BoxBody> {
    let asked = match fields::<OpenAsk>(req, OPEN_SHAPE).await {
        Ok(asked) => asked,
        Err(refusal) => return *refusal,
    };
    // The remembered folder is read off the runtime, as the operator's create reads a chat's (routes.rs).
    let registry = ctx.registry.clone();
    let folder_of = chat.to_string();
    let cwd = tokio::task::spawn_blocking(move || registry.known_cwd(&folder_of)).await.ok().flatten();
    let spec = OpenSpec {
        owner: Owner { by: By::Agent, chat: Some(chat.to_string()) },
        cwd,
        cols: asked.cols,
        rows: asked.rows,
        title: asked.title,
        // None is [`AGENT_CHAT_CAP`], the D5 cap, counted under the table's lock with the table's own cap.
        chat_cap: None,
    };
    match terms.open(spec).await {
        Ok(id) => json(StatusCode::CREATED, &serde_json::json!({ "id": id })),
        Err(why) => open_refused(why),
    }
}

fn open_refused(why: OpenError) -> Response<BoxBody> {
    match why {
        OpenError::ChatCap => problem(StatusCode::CONFLICT, &format!("this chat has {AGENT_CHAT_CAP} terminals open; close one first")),
        OpenError::Cap => problem(StatusCode::CONFLICT, &format!("the perch holds {MAX_TERMS} terminals; close one first")),
        OpenError::Cwd => problem(StatusCode::UNPROCESSABLE_ENTITY, "the chat's folder is not a directory any more"),
        OpenError::Stopping => problem(StatusCode::SERVICE_UNAVAILABLE, STOPPING),
        // An agent's terminal always names its chat (`open` names it), so this is unreachable; a 400 says so rather than guessing.
        OpenError::NoChat => problem(StatusCode::BAD_REQUEST, "that terminal needs a chat"),
        OpenError::Spawn(why) => {
            tracing::warn!(error = %why, "perch: an agent's terminal could not be started");
            problem(StatusCode::INTERNAL_SERVER_ERROR, "the shell could not be started")
        }
    }
}

/// The JSON body of a call, as the named fields of `T`, read under [`JSON_BODY`]: a declared length over it is refused before a byte is
/// read, and one that breaks the cap is refused as the body is read. An empty body counts as `{}`. `Err` is the answer to send.
async fn fields<T: DeserializeOwned>(req: Request<Incoming>, shape: &str) -> Result<T, Box<Response<BoxBody>>> {
    if declared_over(&req) {
        return Err(Box::new(status(StatusCode::PAYLOAD_TOO_LARGE)));
    }
    let (_, bytes) = body_of(req, JSON_BODY).await?;
    let text: &[u8] = if bytes.is_empty() { &b"{}"[..] } else { &bytes[..] };
    serde_json::from_slice(text).map_err(|_| Box::new(problem(StatusCode::BAD_REQUEST, shape)))
}

/// A declared `Content-Length` over [`JSON_BODY`].
fn declared_over(req: &Request<Incoming>) -> bool {
    let declared = req.headers().get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    declared.is_some_and(|len| len > JSON_BODY as u64)
}

/// `POST /hub/term/a/read {id, lines?, max_chars?, wait?}`: a terminal's screen, after its wait. 200 `{text, cursor, seq, alive, exit?,
/// alt_screen}`. Any web terminal can be read. An ended one answers from the screen it kept, with `alive` false and `exit` named.
async fn read(ctx: &Ctx, terms: &Terms, req: Request<Incoming>) -> Response<BoxBody> {
    let asked = match fields::<ReadAsk>(req, READ_SHAPE).await {
        Ok(asked) => asked,
        Err(refusal) => return *refusal,
    };
    if !valid_id(&asked.id) || terms.info(&asked.id).is_none() {
        return problem(StatusCode::NOT_FOUND, NO_TERMINAL);
    }
    let lines = match bounded(asked.lines, READ_LINES, MAX_TEXT_LINES, "lines") {
        Ok(lines) => lines,
        Err(refusal) => return *refusal,
    };
    let max_chars = match bounded(asked.max_chars, MAX_TEXT_CHARS, MAX_TEXT_CHARS, "max_chars") {
        Ok(max_chars) => max_chars,
        Err(refusal) => return *refusal,
    };
    let wait = match asked.wait.map(wait_of).transpose() {
        Ok(wait) => wait,
        Err(refusal) => return *refusal,
    };
    if let Some(wait) = &wait
        && settle(ctx, terms, &asked.id, wait, lines, max_chars).await == Settled::Stopping
    {
        return problem(StatusCode::SERVICE_UNAVAILABLE, STOPPING);
    }
    let Some(info) = terms.info(&asked.id) else { return problem(StatusCode::NOT_FOUND, NO_TERMINAL) };
    let Some(screen) = screen_of(terms, &asked.id, lines, max_chars).await else {
        return problem(StatusCode::NOT_FOUND, NO_TERMINAL);
    };
    let answer = ReadAnswer {
        text: screen.text,
        cursor: screen.cursor,
        seq: screen.seq,
        alive: info.alive,
        exit: info.exit,
        alt_screen: screen.alt_screen,
    };
    json(StatusCode::OK, &answer)
}

/// What a read's wait came to: the answer is due, or the perch is stopping and the wait ends without one.
#[derive(PartialEq, Eq)]
enum Settled {
    Answer,
    Stopping,
}

/// A read's `wait`: the text it names, the quiet it needs, and how long it may take.
struct Wait {
    for_text: Option<String>,
    idle_ms: u64,
    timeout: Duration,
}

/// The wait a read asks for, with its bounds checked: a bound that is not met is a 400 that names it.
fn wait_of(asked: WaitAsk) -> Result<Wait, Box<Response<BoxBody>>> {
    if asked.for_text.as_deref().is_some_and(|text| text.is_empty() || text.chars().count() > FOR_CHARS) {
        return Err(Box::new(problem(StatusCode::BAD_REQUEST, "wait.for is a text of 1 to 200 characters")));
    }
    let idle_ms = asked.idle_ms.unwrap_or(IDLE_MS);
    if idle_ms > IDLE_MAX_MS {
        return Err(Box::new(problem(StatusCode::BAD_REQUEST, "wait.idle_ms is 0 to 60000")));
    }
    let timeout_s = asked.timeout_s.unwrap_or(TIMEOUT_S);
    if timeout_s > TIMEOUT_MAX_S {
        return Err(Box::new(problem(StatusCode::BAD_REQUEST, "wait.timeout_s is 0 to 60")));
    }
    Ok(Wait { for_text: asked.for_text, idle_ms, timeout: Duration::from_secs(timeout_s) })
}

/// Holds a read until its wait is over (see the module's notes). The quiet clock is the terminal's last output or input, so an agent's
/// own typing starts it. The text is looked at again only when the terminal has written since the last look (its `seq` moved), so a
/// busy perch does not read a screen for every other terminal's output.
async fn settle(ctx: &Ctx, terms: &Terms, id: &str, wait: &Wait, lines: usize, max_chars: usize) -> Settled {
    let deadline = Instant::now() + wait.timeout;
    let mut ticks = terms.ticks();
    let mut looked_at: Option<u64> = None;
    loop {
        let Some(info) = terms.info(id) else { return Settled::Answer };
        if !info.alive {
            return Settled::Answer;
        }
        let now = now_ms();
        let quiet_at = info.last_ms.saturating_add(wait.idle_ms);
        let left = deadline.saturating_duration_since(Instant::now());
        if now >= quiet_at || left.is_zero() {
            return Settled::Answer;
        }
        if let Some(needle) = &wait.for_text
            && looked_at != Some(info.seq)
        {
            looked_at = Some(info.seq);
            if screen_of(terms, id, lines, max_chars).await.is_some_and(|screen| screen.text.contains(needle.as_str())) {
                return Settled::Answer;
            }
        }
        let nap = left.min(Duration::from_millis(quiet_at - now));
        tokio::select! {
            _ = tokio::time::sleep(nap) => {}
            changed = ticks.changed() => {
                // The channel is gone only when the terminals are: nothing more will come, so the wait answers now.
                if changed.is_err() {
                    return Settled::Answer;
                }
            }
            _ = ctx.stop.cancelled() => return Settled::Stopping,
        }
    }
}

/// The screen a read answers with: a live terminal's on a blocking thread (its lock is the output parser's), an ended one's saved text.
async fn screen_of(terms: &Terms, id: &str, lines: usize, max_chars: usize) -> Option<Text> {
    match terms.get(id) {
        Some(term) => tokio::task::spawn_blocking(move || term.text(lines, max_chars)).await.ok(),
        None => terms.text(id, lines, max_chars),
    }
}

/// A count the call names, which must be from 1 to `max`; absent, it is `default`.
fn bounded(given: Option<u64>, default: usize, max: usize, name: &str) -> Result<usize, Box<Response<BoxBody>>> {
    match given {
        None => Ok(default),
        Some(n) if (1..=max as u64).contains(&n) => Ok(n as usize),
        Some(_) => Err(Box::new(problem(StatusCode::BAD_REQUEST, &format!("{name} is 1 to {max}")))),
    }
}

/// `POST /hub/term/a/send {id, text?, keys?}`: types `text` as it is (a newline in it is typed too), then presses `keys` in order. 200
/// `{seq}`. Refused with `423` while the operator holds the terminal, and with `403` and the guard's sentence when the text names what
/// an agent may not run (D9). The guard is judged before anything is typed.
async fn send(terms: &Terms, chat: &str, req: Request<Incoming>) -> Response<BoxBody> {
    let asked = match fields::<SendAsk>(req, SEND_SHAPE).await {
        Ok(asked) => asked,
        Err(refusal) => return *refusal,
    };
    if !valid_id(&asked.id) || terms.info(&asked.id).is_none() {
        return problem(StatusCode::NOT_FOUND, NO_TERMINAL);
    }
    let text = asked.text.unwrap_or_default();
    let keys = asked.keys.unwrap_or_default();
    if text.is_empty() && keys.is_empty() {
        return problem(StatusCode::BAD_REQUEST, "send needs text, keys, or both");
    }
    if text.len() > TEXT_BYTES {
        return problem(StatusCode::BAD_REQUEST, "text is at most 4 KiB");
    }
    if keys.len() > KEYS_MAX {
        return problem(StatusCode::BAD_REQUEST, "keys is at most 32 names");
    }
    let mut bytes = text.as_bytes().to_vec();
    for name in &keys {
        match key_bytes(name) {
            Some(press) => bytes.extend_from_slice(&press),
            None => return problem(StatusCode::BAD_REQUEST, &format!("an unknown key; the keys are {KEY_NAMES}")),
        }
    }
    if let Some(reason) = guard::refuses_term_text(&text) {
        return problem(StatusCode::FORBIDDEN, reason);
    }
    match terms.agent_input(&asked.id, chat, &bytes) {
        Ok(()) => json(StatusCode::OK, &serde_json::json!({ "seq": terms.info(&asked.id).map_or(0, |info| info.seq) })),
        Err(why) => send_refused(why),
    }
}

/// The bytes a named key presses. Cursor keys are the normal-mode forms: whether the program asked for application cursor keys is not
/// read here, so a full-screen program that did may not take an arrow sent this way.
fn key_bytes(name: &str) -> Option<Vec<u8>> {
    let press: &[u8] = match name {
        "enter" => b"\r",
        "tab" => b"\t",
        "esc" => b"\x1b",
        "backspace" => b"\x7f",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "pgup" => b"\x1b[5~",
        "pgdn" => b"\x1b[6~",
        _ => {
            return match name.strip_prefix("ctrl-").map(str::as_bytes) {
                Some([letter @ b'a'..=b'z']) => Some(vec![letter - b'a' + 1]),
                _ => None,
            };
        }
    };
    Some(press.to_vec())
}

fn send_refused(why: InputError) -> Response<BoxBody> {
    match why {
        InputError::NotFound => problem(StatusCode::NOT_FOUND, NO_TERMINAL),
        InputError::TooBig => status(StatusCode::PAYLOAD_TOO_LARGE),
        InputError::Ended => problem(StatusCode::CONFLICT, ENDED),
        InputError::Full => problem(StatusCode::SERVICE_UNAVAILABLE, "the shell is not reading its input; try again"),
        InputError::Held => problem(StatusCode::LOCKED, HELD),
    }
}

/// `POST /hub/term/a/close {id}`: ends a terminal this chat opened (D5). The hang-up runs in the background and this does not wait for
/// it: 202 `{id}`. Another chat's or the operator's terminal is a 403, and an id that is no terminal a 404.
async fn close(terms: &Terms, chat: &str, req: Request<Incoming>) -> Response<BoxBody> {
    let asked = match fields::<IdAsk>(req, CLOSE_SHAPE).await {
        Ok(asked) => asked,
        Err(refusal) => return *refusal,
    };
    if !valid_id(&asked.id) {
        return problem(StatusCode::NOT_FOUND, NO_TERMINAL);
    }
    let Some(info) = terms.info(&asked.id) else { return problem(StatusCode::NOT_FOUND, NO_TERMINAL) };
    if !(Owner { by: info.by, chat: info.chat }).is_agents_of(chat) {
        return problem(StatusCode::FORBIDDEN, NOT_YOURS);
    }
    match terms.close(&asked.id) {
        // The hang-up runs on its own thread, and this answer does not wait for it (routes.rs's close does the same).
        Some(_hangup) => json(StatusCode::ACCEPTED, &serde_json::json!({ "id": asked.id })),
        None => problem(StatusCode::NOT_FOUND, NO_TERMINAL),
    }
}
