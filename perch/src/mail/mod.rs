//! `/hub/mail`: Aoide mail, sent from a note and received as cards (slices M2, M3).
//!
//! Mail goes through the Aoide CLI only ([`crate::aoide`], an explicit binary and a closed set of argvs), never a transport of the
//! perch's own, and it is the operator's: every route here is behind the master token, and the agent's own are `agent`'s (M3).
//! A perch started without `--home` has no mail (no place for the files below): every route is the empty 404, as notes are.
//!
//! **Files**, all under `<home>/mail/` (a directory made `0700`, never a link), each written `0600` and opened `O_NOFOLLOW`:
//!
//! ```text
//!   sent.jsonl      one line per send, appended: {ts, origin, from, note, subject, sha256, to, mesh?, msgid, disposition, status}
//!                   origin is `operator` (this slice) or `agent:<chat>` (M3); the text is never kept, only its sha256
//!   received.jsonl  one line per letter read from the mailbox, appended, deduped by msgid, text at most 64 KiB
//!   cards.json      msgid -> {state new|placed|dismissed, chat?, at}, and sent msgid -> when a receipt came back
//! ```
//!
//! The routes are split by who fills them:
//!
//! ```text
//!   GET  /hub/mail/nodes            send::nodes       the nodes a note can be sent to                         (M2)
//!   POST /hub/mail/a/<verb>         agent::route      the agent's `mail_send` and whoami: scoped token        (M3)
//!   everything else                 recv::route       GET /hub/mail, POST /hub/mail/<msgid>/place|dismiss|save (M2)
//!
//!   POST /hub/notes/<n>/send        mail::send_note   called by `notes`, which owns the route                 (M2)
//!   POST /hub/sessions/<id>/mailsend mail::optin      a chat's opt-in to agent mail, operator only            (M3)
//! ```
//!
//! [`pick`] is the whole dispatch below `/hub/mail` and is a plain function so a test can reach it. A message id is 64 hex digits, so
//! `nodes` and `a` are never ids. As for `term`, an agent path is routed as one only if the gate took it as one: `serve` asks
//! [`crate::agent::agent_scope`] before the master check and [`pick`] asks the same predicate ([`crate::agent::verb`]).
//!
//! Every handler has one signature, `(ctx, req, rest) -> Response`, where `rest` is the part of the path below the handler's own
//! prefix: `""` for `nodes`, the verb for `agent`, the whole rest for `recv`; for `send_note` the note's name and for `optin` the
//! chat's id (already checked by `sessions::id_ok`).

pub mod agent;
pub mod recv;
pub mod send;

use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// A letter's text, in bytes: what a note may be to be sent, and what a received card keeps. It sits in argv while aoide runs.
pub const TEXT_MAX: usize = crate::aoide::TEXT_MAX;
/// A subject or a card's title is one line of at most this many characters.
pub const SUBJECT_MAX: usize = crate::aoide::SUBJECT_MAX;

/// What the mail side keeps: when a page last looked, the lock every `sent.jsonl` append takes, and the receive side's store.
/// Filled by [`start`] and the handlers; `Default` is how [`Ctx`] gets one before any hook runs. M3 adds the agent opt-ins here.
#[derive(Default)]
pub struct State {
    /// When `GET /hub/mail` was last asked. The receive task works only for [`WATCH`] after it.
    looked: Mutex<Option<Instant>>,
    /// Wakes the receive task when a page looks after a quiet spell, so the first card does not wait a whole tick.
    wake: tokio::sync::Notify,
    /// Held across an append to `sent.jsonl`, so two sends never interleave a line.
    sent: Mutex<()>,
    recv: recv::State,
}

/// A page that polled `GET /hub/mail` this recently is looking, and the receive task works for it (`--knob mail_watch_ms=N`).
pub const WATCH: Duration = Duration::from_secs(60);
/// The receive task's pace (`--knob mail_tick_ms=N`).
pub const TICK: Duration = Duration::from_secs(5);

impl State {
    /// A page asked for the cards. The first look after a quiet spell wakes the task.
    pub(crate) fn look(&self, within: Duration) {
        let mut looked = self.looked.lock().unwrap_or_else(|e| e.into_inner());
        let quiet = looked.is_none_or(|at| at.elapsed() >= within);
        *looked = Some(Instant::now());
        drop(looked);
        if quiet {
            self.wake.notify_one();
        }
    }

    /// Whether a page looked within `within`.
    pub(crate) fn looked_within(&self, within: Duration) -> bool {
        self.looked.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|at| at.elapsed() < within)
    }
}

/// A `--knob` that is a number of milliseconds, else `default`.
pub(crate) fn knob_ms(ctx: &Ctx, key: &str, default: Duration) -> Duration {
    ctx.opts.knobs.get(key).and_then(|ms| ms.parse().ok()).map(Duration::from_millis).unwrap_or(default)
}

// ---------------------------------------------------------------------------
// The mail folder
// ---------------------------------------------------------------------------

/// `<home>/mail`, or `None` for a perch with no home.
pub(crate) fn mail_dir(ctx: &Ctx) -> Option<PathBuf> {
    ctx.registry.home().map(|home| home.join("mail"))
}

fn ours(meta: &std::fs::Metadata) -> bool {
    meta.uid() == unsafe { libc::geteuid() }
}

/// The folder exists, is a real directory of ours (not a link to one), and is `0700`: made if it is missing, tightened if it is looser.
pub(crate) fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() && ours(&meta) => {
            if meta.permissions().mode() & 0o077 != 0 {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
            Ok(())
        }
        Ok(_) => Err(std::io::Error::other("the mail folder is not a plain directory of this user's")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new().mode(0o700).create(dir),
        Err(e) => Err(e),
    }
}

/// Appends whole lines to `path` (made `0600`; a looser one is tightened) in one `write`, so a line is never torn by another append.
/// `O_NOFOLLOW | O_NONBLOCK`: a link or a FIFO planted under the name is an error and not a write. The caller holds the lock.
pub(crate) fn append_lines(path: &Path, lines: &[String]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || !ours(&meta) {
        return Err(std::io::Error::other("a mail file is not a plain file of this user's"));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    file.write_all(&bytes)
}

/// The last `window` bytes of `path`, from the first whole line in them. A file that is not there is empty. Read through
/// [`crate::rawfile::open_regular`]: a link or a FIFO is an error, and the size judged is the open file's.
pub(crate) fn read_tail(path: &Path, window: u64) -> std::io::Result<Vec<u8>> {
    let (mut file, len) = match crate::rawfile::open_regular(path) {
        Ok(opened) => opened,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let start = len.saturating_sub(window);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    (&file).take(window).read_to_end(&mut bytes)?;
    if start > 0 {
        // The window's edge is most likely the middle of a line.
        let skip = bytes.iter().position(|&b| b == b'\n').map_or(bytes.len(), |at| at + 1);
        bytes.drain(..skip);
    }
    Ok(bytes)
}

/// The lines of a JSON-lines buffer that parse as `T`, oldest first; a torn or foreign line is passed over.
pub(crate) fn rows_of<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Vec<T> {
    bytes.split(|&b| b == b'\n').filter(|line| !line.is_empty()).filter_map(|line| serde_json::from_slice(line).ok()).collect()
}

pub(crate) fn now() -> u64 {
    harnox::time::now_secs()
}

/// A message id: 64 lower-case hex digits (a sha256). The only spelling a route, a row or a receipt's text is matched by.
pub(crate) fn hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A status or a disposition aoide printed: `^[a-z][a-z_-]{0,31}$`. Anything else is not passed on.
pub(crate) fn word_ok(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase()) && text.len() <= 32 && chars.all(|c| c.is_ascii_lowercase() || c == '-' || c == '_')
}

/// One line of printable text for a header: control characters, line and paragraph separators and the bidirectional controls (which
/// reorder what is drawn around them) are dropped, it is trimmed, and it is cut at `max` characters.
pub(crate) fn line_safe(text: &str, max: usize) -> String {
    let drop = |c: &char| {
        c.is_control()
            || matches!(*c, '\u{2028}' | '\u{2029}' | '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{feff}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    };
    let line: String = text.chars().filter(|c| !drop(c)).collect();
    line.trim().chars().take(max).collect::<String>().trim_end().to_string()
}

/// Where a request under `/hub/mail/` goes, and what it is told is its `rest`.
#[derive(Debug, PartialEq, Eq)]
enum Pick<'a> {
    Nodes,
    Agent(&'a str),
    Cards(&'a str),
    /// `a` or `a/...` that is not exactly `a/<verb>`: nobody's route.
    Nobody,
}

fn pick(rest: &str) -> Pick<'_> {
    if rest == "nodes" {
        return Pick::Nodes;
    }
    if let Some(verb) = crate::agent::verb(rest) {
        return Pick::Agent(verb);
    }
    if rest == "a" || rest.starts_with("a/") {
        return Pick::Nobody;
    }
    Pick::Cards(rest)
}

/// `/hub/mail` and everything under it. `rest` is `""` for the bare path.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    match pick(rest) {
        Pick::Nodes => send::nodes(ctx, req, "").await,
        Pick::Agent(verb) => agent::route(ctx, req, verb).await,
        Pick::Cards(rest) => recv::route(ctx, req, rest).await,
        Pick::Nobody => status(StatusCode::NOT_FOUND),
    }
}

/// `POST /hub/notes/<name>/send {node, mesh?, hold?}`: the notes route delegates here. `name` is the note's.
pub async fn send_note(ctx: &Ctx, req: Request<Incoming>, name: &str) -> Response<BoxBody> {
    send::send_note(ctx, req, name).await
}

/// `POST /hub/sessions/<id>/mailsend {on}`: whether this chat's agent may send mail. `id` is a checked session id.
pub async fn optin(ctx: &Ctx, req: Request<Incoming>, id: &str) -> Response<BoxBody> {
    agent::optin(ctx, req, id).await
}

/// After the bind (lib.rs): the receive task, which only works while a page is looking.
pub fn start(ctx: &Ctx) {
    recv::start(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{agent_scope, Scope};

    #[test]
    fn the_dispatch_names_one_handler_per_shape() {
        assert_eq!(pick("nodes"), Pick::Nodes);
        assert_eq!(pick("a/send"), Pick::Agent("send"));
        assert_eq!(pick("a/whoami"), Pick::Agent("whoami"));
        let msgid = "0123456789abcdef".repeat(4);
        let place = format!("{msgid}/place");
        for rest in ["", msgid.as_str(), place.as_str(), "nodes/x", "nodesx", "ab/send", "b/send"] {
            assert_eq!(pick(rest), Pick::Cards(rest), "{rest:?}");
        }
        for rest in ["a", "a/", "a/x/y", "a/Send", "a/%73end"] {
            assert_eq!(pick(rest), Pick::Nobody, "{rest:?}");
        }
    }

    #[test]
    fn a_path_is_an_agent_route_exactly_when_the_gate_took_it_as_one() {
        for path in [
            "/hub/mail",
            "/hub/mail/",
            "/hub/mail/nodes",
            "/hub/mail/a",
            "/hub/mail/a/",
            "/hub/mail/a/send",
            "/hub/mail/a/x/y",
            "/hub/mail/a/%73end",
            "/hub/mail/%61/send",
            "/hub/mail/t1/a/send",
            "/hub/mail//a/send",
            "/hub/mail/a//send",
            "/hub/mail//a/x",
        ] {
            let rest = crate::serve::below(path, "/hub/mail").expect("every path in the table is in the namespace");
            let dispatched = matches!(pick(rest), Pick::Agent(_));
            assert_eq!(dispatched, agent_scope(path) == Some(Scope::Mail), "{path}");
        }
    }
}
