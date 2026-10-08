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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::aoide::{Aoide, Error};
use crate::serve::{status, BoxBody, Ctx};

/// A letter's text, in bytes: what a note may be to be sent, and what a received card keeps. It sits in argv while aoide runs.
pub const TEXT_MAX: usize = crate::aoide::TEXT_MAX;
/// A subject or a card's title is one line of at most this many characters.
pub const SUBJECT_MAX: usize = crate::aoide::SUBJECT_MAX;
/// How many sends may be running at once. A send past this is 429 at once, and the page says to wait.
pub const SENDS_AT_ONCE: usize = 2;
/// How long a schema check (`aoide schema`) is trusted, good or bad, before aoide is asked again.
const SCHEMA_TTL: Duration = Duration::from_secs(60);

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
    /// The sends running now, at most [`SENDS_AT_ONCE`] (see [`take_send_slot`]).
    in_flight: AtomicUsize,
    /// The last schema check and when it was made: whether aoide lists every command and flag the perch sends.
    schema: tokio::sync::Mutex<Option<(Instant, Result<(), String>)>>,
    /// Why mail is off in this perch, and the sentence every mail route answers with: another perch holds the folder's lock, or the folder
    /// cannot be used. Set once, by [`recv::start`].
    refusal: OnceLock<String>,
    /// This perch's hold on `<home>/mail/.lock`, kept for as long as the perch runs.
    lock: OnceLock<std::fs::File>,
    recv: recv::State,
}

/// A page that polled `GET /hub/mail` this recently is looking, and the receive task works for it (`--knob mail_watch_ms=N`).
pub const WATCH: Duration = Duration::from_secs(60);
/// The receive task's pace (`--knob mail_tick_ms=N`).
pub const TICK: Duration = Duration::from_secs(5);

/// A send's place among the [`SENDS_AT_ONCE`] that may run: held for the send's length, and given back when it is dropped.
pub(crate) struct SendSlot<'a>(&'a AtomicUsize);

impl Drop for SendSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A place for a send, or `None` when [`SENDS_AT_ONCE`] are running already.
pub(crate) fn take_send_slot(state: &State) -> Option<SendSlot<'_>> {
    if state.in_flight.fetch_add(1, Ordering::SeqCst) >= SENDS_AT_ONCE {
        state.in_flight.fetch_sub(1, Ordering::SeqCst);
        return None;
    }
    Some(SendSlot(&state.in_flight))
}

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

pub(crate) fn ours(meta: &std::fs::Metadata) -> bool {
    meta.uid() == unsafe { libc::geteuid() }
}

/// The folder exists, is a real directory of ours (not a link to one), and is `0700`: made if it is missing, tightened if it is looser.
/// Made with `mkdir` and checked after, so two creators at once (a send and the receive task) both come out with the same folder.
pub(crate) fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    if let Err(e) = std::fs::DirBuilder::new().mode(0o700).create(dir)
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(e);
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !(meta.is_dir() && ours(&meta)) {
        return Err(std::io::Error::other("the mail folder is not a plain directory of this user's"));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Whether the folder may be read: absent (nothing has been kept yet) or a real directory of this user's, never a link to one.
pub(crate) fn readable_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() && ours(&meta) => Ok(()),
        Ok(_) => Err(std::io::Error::other("the mail folder is not a plain directory of this user's")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Appends whole lines to `path` (made `0600`; a looser one is tightened) in one `write`, so a line is never torn by another append.
/// A last line with no newline (a torn one, from a write that did not finish) is ended first, so the new lines start on their own line,
/// and the file is synced before this returns. `O_NOFOLLOW | O_NONBLOCK`: a link or a FIFO planted under the name is an error and not a
/// write. The caller holds the lock.
pub(crate) fn append_lines(path: &Path, lines: &[String]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
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
    if meta.len() > 0 {
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0u8; 1];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            bytes.push(b'\n');
        }
    }
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    file.write_all(&bytes)?;
    file.sync_all()
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

/// The lines of a buffer that end in a newline. The piece after the last newline is not one: it is a line only once its newline is written,
/// so a torn last line (a write that did not finish) is never read as a record.
pub(crate) fn whole_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |at| at + 1);
    bytes[..end].split(|&b| b == b'\n').filter(|line| !line.is_empty())
}

/// The lines of a JSON-lines buffer that parse as `T`, oldest first; a torn or foreign line is passed over.
pub(crate) fn rows_of<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Vec<T> {
    whole_lines(bytes).filter_map(|line| serde_json::from_slice(line).ok()).collect()
}

/// The sentence every mail route answers with when another perch holds this home's mail folder, or `None` when this one may use it.
pub(crate) fn refused(state: &State) -> Option<String> {
    state.refusal.get().cloned()
}

/// Whether aoide is there, and lists every command and flag the perch sends (checked at most once a minute, good or bad): `Ok` for a
/// usable aoide, else the sentence for the person that says why mail is off. Runs `aoide schema` when the answer is old.
pub(crate) async fn usable(ctx: &Ctx) -> Result<Aoide, String> {
    let aoide = Aoide::from_options(&ctx.opts).map_err(|e| e.to_string())?;
    // The lock is held for the cache only. `aoide schema` can take up to its timeout, and a page's `GET` waits on this same lock
    // (`schema_refusal`), so it is never held across the call: two checks at once are two calls, and the later answer is the one kept.
    let cached = {
        let checked = ctx.mail.schema.lock().await;
        checked.as_ref().filter(|(at, _)| at.elapsed() < SCHEMA_TTL).map(|(_, outcome)| outcome.clone())
    };
    if let Some(outcome) = cached {
        return outcome.map(|()| aoide);
    }
    let outcome = match aoide.schema_check().await {
        Ok(_) => Ok(()),
        Err(Error::Refused(why)) => Err(why),
        Err(other) => Err(format!("aoide's schema could not be read: {other}")),
    };
    *ctx.mail.schema.lock().await = Some((Instant::now(), outcome.clone()));
    outcome.map(|()| aoide)
}

/// The last schema outcome, without asking aoide: `Some` only when a check was made and it refused mail.
pub(crate) async fn schema_refusal(ctx: &Ctx) -> Option<String> {
    let checked = ctx.mail.schema.lock().await;
    checked.as_ref().and_then(|(_, outcome)| outcome.clone().err())
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
    use serde_json::json;

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

    #[test]
    fn a_torn_last_line_is_never_a_record_and_the_next_append_starts_on_its_own_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("received.jsonl");
        std::fs::write(&path, "{\"n\":1}\n{\"n\":2,\"tor").unwrap();
        let rows: Vec<serde_json::Value> = rows_of(&std::fs::read(&path).unwrap());
        assert_eq!(rows, vec![json!({"n": 1})], "the torn piece is passed over");
        append_lines(&path, &["{\"n\":3}".to_string()]).unwrap();
        let rows: Vec<serde_json::Value> = rows_of(&std::fs::read(&path).unwrap());
        assert_eq!(rows, vec![json!({"n": 1}), json!({"n": 3})], "the new row is whole and nothing glued onto the torn one");
        assert_eq!(whole_lines(b"a\nb").count(), 1, "a line with no newline is not a line");
        assert_eq!(whole_lines(b"").count(), 0);
    }

    #[test]
    fn the_send_slots_are_two_and_a_drop_gives_one_back() {
        let state = State::default();
        let first = take_send_slot(&state).expect("the first");
        let second = take_send_slot(&state).expect("the second");
        assert!(take_send_slot(&state).is_none(), "the third waits");
        drop(first);
        let third = take_send_slot(&state).expect("a slot given back is taken again");
        assert!(take_send_slot(&state).is_none());
        drop((second, third));
        assert_eq!(state.in_flight.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_folder_is_made_once_and_a_link_in_its_place_is_refused_on_reads_as_well() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("mail");
        ensure_dir(&dir).unwrap();
        ensure_dir(&dir).expect("made already: that is fine");
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(readable_dir(&dir).is_ok());
        let elsewhere = base.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let linked = base.path().join("linked");
        std::os::unix::fs::symlink(&elsewhere, &linked).unwrap();
        assert!(readable_dir(&linked).is_err(), "a link is not read");
        assert!(ensure_dir(&linked).is_err(), "and is not written");
        assert!(readable_dir(&base.path().join("absent")).is_ok(), "nothing kept yet is nothing to refuse");
    }
}
