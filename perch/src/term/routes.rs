//! The operator's terminal routes (slice T1b): `GET|POST /hub/term` and `/hub/term/<t>/input|resize|hold|close|text`.
//!
//! The operator is the master bearer, or a same-origin login cookie: gate 1 has passed by the time a request is here. `term::route` is a
//! seam, so the body arrives unread and is read here through [`body_of`], under each route's own cap. Every answer is decided in this
//! file, and none carries the environment, a token or the perch's argv: a refusal names a state or a reason, never a path it was not
//! given. The agent's routes (`/hub/term/a/...`) are [`super::agent`]'s and never come through here.
//!
//! `rest` is `""` or `<t>/<verb>` (see [`super::pick`]). An id that is not `^t[0-9]{1,6}$`, and a path that is no route, are the empty
//! 404. A route with the wrong method is a 405 that names the method it takes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use hyper::body::Incoming;
use hyper::header::{HeaderValue, ALLOW, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};

use super::screen::{Cursor, MAX_TEXT_CHARS, MAX_TEXT_LINES};
use super::{valid_id, By, InputError, Missing, OpenError, OpenSpec, Owner, Terminal, Terms, MAX_INPUT, MAX_TERMS};
use crate::serve::{body_of, json, problem, status, BoxBody, Ctx};
use crate::sessions;

/// The lines a text read gives when the request names none.
const TEXT_LINES: usize = 100;
/// The most a create, resize or hold body may be: a few fields of JSON, so this is generous.
const JSON_BODY: usize = 16 * 1024;
/// The media type `input` takes. Parameters after `;` are allowed; a second `Content-Type` header is not, since it is ambiguous.
const OCTET_STREAM: &str = "application/octet-stream";

const ENDED: &str = "that terminal's shell has ended";
const CREATE_SHAPE: &str = "expected a JSON object of {\"chat\"?, \"cwd\"?, \"cols\"?, \"rows\"?, \"title\"?}, or no body";
const SIZE_SHAPE: &str = "expected a JSON object of {\"cols\": <number>, \"rows\": <number>}";
const HOLD_SHAPE: &str = "expected a JSON object of {\"hold\": true or false}";
const LINES_SHAPE: &str = "lines is a number of lines: digits only, given once";

/// `rest` is `""` (the list and the create) or `<t>/<verb>`. The id is checked before anything else, so no other spelling reaches a
/// terminal.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    let terms = ctx.term.terms(ctx);
    if rest.is_empty() {
        if *req.method() == Method::GET {
            return json(StatusCode::OK, &terms.list());
        }
        if *req.method() == Method::POST {
            return create(ctx, terms, req).await;
        }
        return method_refused("GET, POST");
    }
    let Some((id, verb)) = rest.split_once('/') else { return status(StatusCode::NOT_FOUND) };
    if !valid_id(id) {
        return status(StatusCode::NOT_FOUND);
    }
    let takes = match verb {
        "text" => Method::GET,
        "input" | "resize" | "hold" | "close" => Method::POST,
        _ => return status(StatusCode::NOT_FOUND),
    };
    if *req.method() != takes {
        return method_refused(if takes == Method::GET { "GET" } else { "POST" });
    }
    if terms.info(id).is_none() {
        return status(StatusCode::NOT_FOUND);
    }
    match verb {
        "text" => text(terms, id, req.uri().query()).await,
        "input" => input(terms, id, req).await,
        "resize" => resize(terms, id, req).await,
        "hold" => hold(terms, id, req).await,
        _ => close(terms, id),
    }
}

/// `POST /hub/term {chat?, cwd?, cols?, rows?, title?}`: 201 `{id}`. The shell starts in `cwd` when one is given (it must pass
/// `--root`, [`crate::sessions::Registry::cwd_under_a_root`]); else in the folder of `chat` when one is given (an unknown chat is a 404);
/// else in `$HOME`, which is not root-checked: the check bounds only where a shell starts, and the operator's shell can `cd` anywhere.
async fn create(ctx: &Ctx, terms: &Terms, req: Request<Incoming>) -> Response<BoxBody> {
    #[derive(Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Asked {
        chat: Option<String>,
        cwd: Option<String>,
        cols: Option<u32>,
        rows: Option<u32>,
        title: Option<String>,
    }
    let bytes = match read_body(req, JSON_BODY).await {
        Ok(bytes) => bytes,
        Err(refusal) => return *refusal,
    };
    // No body is the default terminal; anything else must be the object.
    let asked = if bytes.is_empty() {
        Asked::default()
    } else {
        match serde_json::from_slice::<Asked>(&bytes) {
            Ok(asked) => asked,
            Err(_) => return problem(StatusCode::BAD_REQUEST, CREATE_SHAPE),
        }
    };
    let (chat, folder) = match asked.chat.clone() {
        None => (None, None),
        Some(chat) => {
            // The remembered folder is read from a file and the list from the logs: off the runtime, as the other file reads are.
            let registry = ctx.registry.clone();
            let asked_chat = chat.clone();
            match tokio::task::spawn_blocking(move || chat_folder(&registry, &asked_chat)).await {
                Ok(Chat::Unknown) => return problem(StatusCode::NOT_FOUND, "no such chat"),
                Ok(Chat::NoFolder) => (Some(chat), None),
                Ok(Chat::Folder(dir)) => (Some(chat), Some(dir)),
                Err(_) => return problem(StatusCode::INTERNAL_SERVER_ERROR, "the chat could not be read"),
            }
        }
    };
    let cwd = match asked.cwd.as_deref() {
        Some(asked_cwd) => match ctx.registry.cwd_under_a_root(Path::new(asked_cwd)) {
            Ok(path) => Some(path),
            Err(why) => return problem(StatusCode::UNPROCESSABLE_ENTITY, &format!("{why:#}")),
        },
        None => folder,
    };
    let spec = OpenSpec {
        owner: Owner { by: By::Operator, chat },
        cwd,
        cols: asked.cols,
        rows: asked.rows,
        title: asked.title,
        chat_cap: None,
    };
    match terms.open(spec).await {
        Ok(id) => json(StatusCode::CREATED, &serde_json::json!({ "id": id })),
        Err(why) => open_refused(why),
    }
}

/// What a chat's id is to the perch: not one it knows, one it knows with no folder remembered, or one with the folder it was started in.
enum Chat {
    Unknown,
    NoFolder,
    Folder(PathBuf),
}

/// Whether `chat` is one the perch knows, and where it was started. The remembered folder (a live door's, or one kept in the perch's
/// own `chat-cwds.json`) is asked first; a chat with none is looked for in the list, the question `tree` asks, which reads the logs.
fn chat_folder(registry: &sessions::Registry, chat: &str) -> Chat {
    if !sessions::id_ok(chat) {
        return Chat::Unknown;
    }
    if let Some(dir) = registry.known_cwd(chat) {
        return Chat::Folder(dir);
    }
    if registry.list().iter().any(|row| row.id == chat) {
        return Chat::NoFolder;
    }
    Chat::Unknown
}

fn open_refused(why: OpenError) -> Response<BoxBody> {
    match why {
        OpenError::Cap | OpenError::ChatCap => {
            problem(StatusCode::CONFLICT, &format!("the perch holds {MAX_TERMS} terminals; close one first"))
        }
        OpenError::Cwd => problem(StatusCode::UNPROCESSABLE_ENTITY, "that folder is not a directory"),
        OpenError::Stopping => problem(StatusCode::SERVICE_UNAVAILABLE, "the perch is shutting down"),
        // An operator's terminal names no chat, so this is only reachable by a change that gives one: a 400, not a guess.
        OpenError::NoChat => problem(StatusCode::BAD_REQUEST, "that terminal needs a chat"),
        OpenError::Spawn(why) => {
            tracing::warn!(error = %why, "perch: a terminal's shell could not be started");
            problem(StatusCode::INTERNAL_SERVER_ERROR, "the shell could not be started")
        }
    }
}

/// `POST /hub/term/<t>/input`: the operator's bytes for the shell, as they are (204). The `Content-Type` is judged before the body is
/// read (415), and the size before it too when the length is declared (413).
async fn input(terms: &Terms, id: &str, req: Request<Incoming>) -> Response<BoxBody> {
    if !is_octet_stream(&req) {
        return problem(StatusCode::UNSUPPORTED_MEDIA_TYPE, "input is sent as application/octet-stream");
    }
    let bytes = match read_body(req, MAX_INPUT).await {
        Ok(bytes) => bytes,
        Err(refusal) => return *refusal,
    };
    match terms.input(id, &bytes) {
        Ok(()) => status(StatusCode::NO_CONTENT),
        Err(why) => input_refused(why),
    }
}

fn input_refused(why: InputError) -> Response<BoxBody> {
    match why {
        InputError::NotFound => status(StatusCode::NOT_FOUND),
        InputError::TooBig => status(StatusCode::PAYLOAD_TOO_LARGE),
        InputError::Ended => problem(StatusCode::CONFLICT, ENDED),
        InputError::Full => problem(StatusCode::SERVICE_UNAVAILABLE, "the shell is not reading its input; try again"),
        // The operator's input is never held: `Held` is an agent's refusal (T2's), and this lane cannot produce it.
        InputError::Held => problem(StatusCode::CONFLICT, "the operator has this terminal on hold"),
    }
}

/// `POST /hub/term/<t>/resize {cols, rows}`: 200 `{cols, rows}`, the size now. T1a clamps it (20-400 columns, 5-200 rows).
async fn resize(terms: &Terms, id: &str, req: Request<Incoming>) -> Response<BoxBody> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Asked {
        cols: u32,
        rows: u32,
    }
    #[derive(Serialize)]
    struct Now {
        cols: u16,
        rows: u16,
    }
    let bytes = match read_body(req, JSON_BODY).await {
        Ok(bytes) => bytes,
        Err(refusal) => return *refusal,
    };
    let Ok(asked) = serde_json::from_slice::<Asked>(&bytes) else { return problem(StatusCode::BAD_REQUEST, SIZE_SHAPE) };
    let term = match live(terms, id) {
        Ok(term) => term,
        Err(why) => return missing(why),
    };
    // The resize takes the screen's lock, which the thread parsing the program's output holds for a moment: off the runtime.
    match tokio::task::spawn_blocking(move || term.resize(asked.cols, asked.rows)).await {
        Ok((cols, rows)) => json(StatusCode::OK, &Now { cols, rows }),
        Err(_) => problem(StatusCode::INTERNAL_SERVER_ERROR, "the terminal did not take the size"),
    }
}

/// `POST /hub/term/<t>/hold {hold}`: the operator's stop for an agent (T2 refuses an agent's input while it is on). 200 `{hold}`, the
/// state now. The operator's own typing is never held.
async fn hold(terms: &Terms, id: &str, req: Request<Incoming>) -> Response<BoxBody> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Asked {
        hold: bool,
    }
    let bytes = match read_body(req, JSON_BODY).await {
        Ok(bytes) => bytes,
        Err(refusal) => return *refusal,
    };
    let Ok(asked) = serde_json::from_slice::<Asked>(&bytes) else { return problem(StatusCode::BAD_REQUEST, HOLD_SHAPE) };
    match terms.set_hold(id, asked.hold) {
        Ok(_) => json(StatusCode::OK, &serde_json::json!({ "hold": asked.hold })),
        Err(why) => missing(why),
    }
}

/// `POST /hub/term/<t>/close`: the terminal leaves the list now and its session is hung up in the background. 202, without a wait.
fn close(terms: &Terms, id: &str) -> Response<BoxBody> {
    match terms.close(id) {
        // The hang-up runs on its own thread, and this answer does not wait for it.
        Some(_hangup) => status(StatusCode::ACCEPTED),
        None => status(StatusCode::NOT_FOUND),
    }
}

/// `GET /hub/term/<t>/text?lines=N`: the screen as text, its last `N` lines (100 when none is named, 400 at most) and at most
/// [`MAX_TEXT_CHARS`] characters, the end kept. 200 `{text, cursor, alt_screen}`; `cursor` is `null` for an ended terminal.
async fn text(terms: &Terms, id: &str, query: Option<&str>) -> Response<BoxBody> {
    #[derive(Serialize)]
    struct TextAnswer {
        text: String,
        cursor: Option<Cursor>,
        alt_screen: bool,
    }
    let lines = match lines_of(query) {
        Ok(lines) => lines,
        Err(refusal) => return *refusal,
    };
    let read = match terms.get(id) {
        // A live screen's read waits for the lock the output parser holds: off the runtime.
        Some(term) => match tokio::task::spawn_blocking(move || term.text(lines, MAX_TEXT_CHARS)).await {
            Ok(read) => read,
            Err(_) => return problem(StatusCode::INTERNAL_SERVER_ERROR, "the screen could not be read"),
        },
        // Not live: an ended terminal keeps its last screen, which needs no lock.
        None => match terms.text(id, lines, MAX_TEXT_CHARS) {
            Some(read) => read,
            None => return status(StatusCode::NOT_FOUND),
        },
    };
    json(StatusCode::OK, &TextAnswer { text: read.text, cursor: read.cursor, alt_screen: read.alt_screen })
}

/// `?lines=N`: digits and nothing else, given once. Absent is [`TEXT_LINES`]; a number over [`MAX_TEXT_LINES`] is that many; `0` is one.
fn lines_of(query: Option<&str>) -> Result<usize, Box<Response<BoxBody>>> {
    let mut given = None;
    for pair in query.unwrap_or("").split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key != "lines" {
            continue;
        }
        if given.replace(value).is_some() {
            return Err(Box::new(problem(StatusCode::BAD_REQUEST, LINES_SHAPE)));
        }
    }
    match given {
        None => Ok(TEXT_LINES),
        Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
            Ok(digits.parse::<usize>().unwrap_or(usize::MAX).min(MAX_TEXT_LINES))
        }
        Some(_) => Err(Box::new(problem(StatusCode::BAD_REQUEST, LINES_SHAPE))),
    }
}

/// The running terminal `id`. One that is not in the table but is listed (an ended one, read back from `terms.json`) is `Ended`.
fn live(terms: &Terms, id: &str) -> Result<Arc<Terminal>, Missing> {
    match terms.get(id) {
        Some(term) => Ok(term),
        None if terms.info(id).is_some() => Err(Missing::Ended),
        None => Err(Missing::NotFound),
    }
}

fn missing(why: Missing) -> Response<BoxBody> {
    match why {
        Missing::NotFound => status(StatusCode::NOT_FOUND),
        Missing::Ended => problem(StatusCode::CONFLICT, ENDED),
    }
}

/// The body of a route that takes one, at most `cap` bytes. A declared length over the cap is refused before a byte is read; a body
/// without one stops at the first frame past the cap ([`body_of`]).
async fn read_body(req: Request<Incoming>, cap: usize) -> Result<Bytes, Box<Response<BoxBody>>> {
    if declared_over(&req, cap) {
        return Err(Box::new(status(StatusCode::PAYLOAD_TOO_LARGE)));
    }
    match body_of(req, cap).await {
        Ok((_, bytes)) => Ok(bytes),
        Err(refusal) => Err(refusal),
    }
}

fn declared_over(req: &Request<Incoming>, cap: usize) -> bool {
    let declared = req.headers().get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    declared.is_some_and(|len| len > cap as u64)
}

/// `Content-Type` is `application/octet-stream`: the one header, its media type exactly that (parameters after `;` allowed, the case
/// not judged). Anything else, a repeated header included, is not.
fn is_octet_stream(req: &Request<Incoming>) -> bool {
    let mut values = req.headers().get_all(CONTENT_TYPE).iter();
    let (Some(value), None) = (values.next(), values.next()) else { return false };
    value.to_str().ok().and_then(|text| text.split(';').next()).is_some_and(|media| media.trim().eq_ignore_ascii_case(OCTET_STREAM))
}

fn method_refused(allow: &'static str) -> Response<BoxBody> {
    let mut refusal = status(StatusCode::METHOD_NOT_ALLOWED);
    refusal.headers_mut().insert(ALLOW, HeaderValue::from_static(allow));
    refusal
}
