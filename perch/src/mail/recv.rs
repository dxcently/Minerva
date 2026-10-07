//! Receiving mail as cards (slice M2): `GET /hub/mail`, `POST /hub/mail/<msgid>/place|dismiss|save`, and the task that reads the
//! Aoide mailbox while a page is looking.
//!
//! ```text
//!   GET  /hub/mail                       {available, received:[card], more, sent:[row], error}
//!   POST /hub/mail/<msgid>/place {chat}  200 {msgid, state:"placed", chat}   the card is shown above that chat's composer; nothing is sent
//!   POST /hub/mail/<msgid>/dismiss       200 {msgid, state:"dismissed"}
//!   POST /hub/mail/<msgid>/save          201 {name, sha, mtime}              a note made from the card (`notes::create`)
//! ```
//!
//! **Nothing here ever posts a card to a door.** A card is data in this perch's files; `place` only records which chat the page should
//! show it with, and the page puts the quoted text into a composer for the person to send or not. `tests/mail.rs` holds the fake door
//! to that: no `say` reaches it for any card.
//!
//! **The receive task** ([`start`]) reads the mailbox only while a page is looking: `GET /hub/mail` within the last 60 s. Every 5 s it
//! stats `<aoide-home>/state/mail/base.jsonl` (inode, size, mtime) and runs `mail read --for=minerva --json` (as the reader
//! `minerva-perch`, so only Minerva's own cursor moves) when that changed; an unchanged file costs no call at all. The first look after
//! the perch started also runs `--reread` once (4 MiB at most), which recovers a letter that aoide handed over, advanced its cursor past,
//! and the perch never wrote down. A plain read that may have advanced the cursor without the letters being kept (a timeout, too much
//! output, output that is not JSON, a write that failed) is followed by a `--reread` too, after a pause of 30 s. `mail export` is not
//! used: it is read-only, but it returns Markdown threads for every mailbox and no receipts.
//!
//! **A letter** is decoded as `AOIDE-LETTER/1` (a marker line, then a JSON object with `subject` and `body`) when it parses and is raw
//! text otherwise (a marker that does not parse is `broken`, shown raw). The header fields a card keeps are clamped to their grammars:
//! a node or mailbox is `^[a-z0-9][a-z0-9-]*$` or nothing; a subject is one printable line of 120 characters; a thread or reply id is
//! 64 hex digits or nothing; a time that is not a time is the moment the perch read the letter. An entry whose message id is not 64 hex
//! digits is not kept (nothing could be asked about it). The sender's mailbox is kept as `minerva`, `minerva-agent` or `other` (`sender`),
//! beside the name itself (`mailbox`): `--from` is attribution that the sender chose, and not proof.
//!
//! **Receipts.** An entry of type `receipt` whose text is the message id of a row of `sent.jsonl`, and which came from the node that row
//! went to, marks that row `delivered`. The status of any other row comes from `mail outbox --json`, run at most once a minute and only
//! while a row is waiting on it, never by `GET /hub/mail` itself: a row that left the outbox after it was sent is `accepted`.
//!
//! **Files and bounds.** `received.jsonl` is append-only (a letter once, by message id, text cut at 64 KiB), `cards.json` holds one slot
//! per message id ever read (`new`, `placed{chat}`, `dismissed`: the dedupe and the state) and the receipts. The task stops reading at
//! 5000 cards or 32 MiB of letters and says so in `error`: aoide keeps the letters it was not asked for, so nothing is lost, and
//! moving `received.jsonl` and `cards.json` aside starts a fresh store. `GET` shows the 25 newest cards that are not dismissed (`more`
//! counts the rest) and the 50 newest sent rows, and reads only the last 2 MiB of letters and 256 KiB of rows.

use std::collections::{BTreeMap, HashSet};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::aoide::{self, Aoide, Error, Limits, Shape};
use crate::serve::{body_of, json, problem, status, BoxBody, Ctx};
use crate::{mail, mesh, notes, sessions};

/// The cards a `GET` shows, and the sent rows.
const CARDS_SHOWN: usize = 25;
const SENT_SHOWN: usize = 50;
/// How much of the end of each file a `GET` reads.
const RECEIVED_WINDOW: u64 = 2 * 1024 * 1024;
const SENT_WINDOW: u64 = 256 * 1024;
/// How much of `sent.jsonl` a receipt is matched against.
const MATCH_WINDOW: u64 = 1024 * 1024;
/// The task stops reading at this many cards, or this many bytes of letters.
const MAX_CARDS: usize = 5000;
const RECEIVED_CAP: u64 = 32 * 1024 * 1024;
/// What `--reread` may print. A plain read has the runner's 1 MiB.
const REREAD_CAP: usize = 4 * 1024 * 1024;
/// At most this many entries of one read are looked at.
const ENTRIES_MAX: usize = 2000;
/// `cards.json` is read only when it is at most this big.
const CARDS_FILE_CAP: u64 = 16 * 1024 * 1024;
/// Receipts kept in `cards.json`.
const DELIVERED_KEEP: usize = 1000;
/// A row is waited on for this long.
const WAIT_ROW: u64 = 7 * 24 * 3600;
/// The body of `place`, `dismiss` and `save`.
const BODY_CAP: usize = 1024;
/// How long the task rests after a failure (`--knob mail_pause_ms=N`), and how long `mail outbox --json` is trusted (`mail_ttl_ms=N`).
const PAUSE: Duration = Duration::from_secs(30);
const OUTBOX_TTL: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// What is kept
// ---------------------------------------------------------------------------

/// What the receive side keeps in memory: the cards (loaded when first needed; the lock is also the one every writer of `received.jsonl`
/// and `cards.json` takes), the last `mail outbox` answer, and the task's last complaint.
#[derive(Default)]
pub struct State {
    cards: Mutex<Option<Cards>>,
    outbox: Mutex<Option<Arc<Outbox>>>,
    notice: Mutex<Option<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mood {
    New,
    Placed,
    Dismissed,
}

/// One card's state: where it is in `new -> placed{chat} -> dismissed`, and when it got there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Slot {
    state: Mood,
    #[serde(default)]
    chat: Option<String>,
    at: u64,
}

/// `cards.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Cards {
    #[serde(default)]
    v: u32,
    #[serde(default)]
    cards: BTreeMap<String, Slot>,
    /// Sent message id -> when its receipt was read.
    #[serde(default)]
    delivered: BTreeMap<String, u64>,
}

/// A letter as `received.jsonl` keeps it and a card shows it. Everything in it is clamped or checked by [`parse_entry`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Row {
    msgid: String,
    #[serde(default)]
    seq: Option<u64>,
    /// When aoide received it (its `receivedAt`), or when this perch read it if that was not a time.
    at: u64,
    /// When this perch read it.
    seen: u64,
    #[serde(default)]
    node: Option<String>,
    #[serde(default)]
    mailbox: Option<String>,
    /// `minerva`, `minerva-agent` or `other`.
    sender: String,
    #[serde(default)]
    via: Option<String>,
    subject: String,
    text: String,
    /// `letter` (an AOIDE-LETTER/1 that parsed) or `raw`.
    format: String,
    #[serde(default)]
    broken: bool,
    #[serde(default)]
    clipped: bool,
    #[serde(default)]
    thread: Option<String>,
    #[serde(default)]
    reply_to: Option<String>,
}

/// A line of `sent.jsonl` (`send.rs` writes it).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SentRow {
    ts: u64,
    origin: String,
    from: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    subject: String,
    sha256: String,
    to: String,
    #[serde(default)]
    mesh: Option<String>,
    #[serde(default)]
    msgid: Option<String>,
    disposition: String,
    status: String,
}

/// What `mail outbox --json` said, and when.
#[derive(Debug, Default)]
struct Outbox {
    at: Option<Instant>,
    /// Unix seconds when it was fetched: a row sent after that has not been looked for yet.
    fetched: u64,
    entries: BTreeMap<String, OutboxEntry>,
}

#[derive(Debug, Clone, PartialEq)]
struct OutboxEntry {
    status: String,
    reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Reading a letter
// ---------------------------------------------------------------------------

/// What one entry of `mail read` is to this perch.
#[derive(Debug, PartialEq)]
enum Item {
    Letter(Box<Row>),
    /// A receipt: the message id it names, and the node it came from.
    Receipt { of: String, from_node: Option<String> },
}

/// A receipt's evidence: the message id it names, and the node it came from (when that is a name).
type Receipt = (String, Option<String>);

/// A name that fits aoide's grammar, else nothing.
fn name_of(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).filter(|name| aoide::name_ok(name)).map(str::to_string)
}

fn id_of(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).filter(|id| mail::hex64(id)).map(str::to_string)
}

/// `text` cut to 64 KiB at a character boundary, with a NUL (which a note cannot hold) made a replacement character.
fn clip_text(text: &str) -> (String, bool) {
    let clean = text.replace('\0', "\u{fffd}");
    if clean.len() <= mail::TEXT_MAX {
        return (clean, false);
    }
    let mut end = mail::TEXT_MAX;
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    (clean[..end].to_string(), true)
}

/// The first non-empty line of `text`, as a header.
fn first_line(text: &str) -> String {
    text.lines().map(|line| mail::line_safe(line, mail::SUBJECT_MAX)).find(|line| !line.is_empty()).unwrap_or_default()
}

/// The letter a text is: its subject, its body, and whether it was structured (`letter`) or raw, broken, with its thread.
struct Decoded {
    subject: String,
    body: String,
    format: &'static str,
    broken: bool,
    thread: Option<String>,
    reply_to: Option<String>,
}

const MARKER: &str = "AOIDE-LETTER/1";

/// `AOIDE-LETTER/1`, a newline and a JSON object with a string `body` is structured; anything else is raw, and a text that starts with the
/// marker and still does not parse is raw and `broken`. Nothing in it is trusted: the subject is one printable line, the ids are hex.
fn decode(text: &str) -> Decoded {
    let raw = |broken: bool| Decoded {
        subject: if broken { "(an unreadable letter)".to_string() } else { first_line(text) },
        body: text.to_string(),
        format: "raw",
        broken,
        thread: None,
        reply_to: None,
    };
    // A letter is meant to be structured when its first line is exactly the marker; a note that merely starts with those words is not.
    let (head, json) = text.split_once('\n').unwrap_or((text, ""));
    if head.trim_end_matches('\r') != MARKER {
        return raw(false);
    }
    match serde_json::from_str::<Value>(json) {
        Ok(Value::Object(letter)) if letter.get("body").is_some_and(Value::is_string) => {
            let body = letter["body"].as_str().unwrap_or_default().to_string();
            let subject = letter.get("subject").and_then(Value::as_str).map(|s| mail::line_safe(s, mail::SUBJECT_MAX)).filter(|s| !s.is_empty());
            Decoded {
                subject: subject.unwrap_or_else(|| first_line(&body)),
                body,
                format: "letter",
                broken: false,
                thread: id_of(letter.get("threadId")),
                reply_to: id_of(letter.get("replyTo")),
            }
        }
        _ => raw(true),
    }
}

/// One entry of `data.entries`. `seen` is now, for a time that is not one. `None` for what is not a letter or a receipt, or has no
/// message id of the right shape.
fn parse_entry(entry: &Value, seen: u64) -> Option<Item> {
    let envelope = entry.get("envelope")?;
    let kind = entry.get("type").or_else(|| envelope.pointer("/header/type")).and_then(Value::as_str)?;
    let msgid = id_of(envelope.get("msgid"))?;
    let text = envelope.get("text").and_then(Value::as_str)?;
    let from = envelope.pointer("/header/from");
    let node = name_of(from.and_then(|from| from.get("node")));
    let mailbox = name_of(from.and_then(|from| from.get("name")));
    match kind {
        "receipt" => mail::hex64(text.trim()).then(|| Item::Receipt { of: text.trim().to_string(), from_node: node }),
        "letter" => {
            let decoded = decode(text);
            let (body, clipped) = clip_text(&decoded.body);
            let sender = match mailbox.as_deref() {
                Some("minerva") => "minerva",
                Some("minerva-agent") => "minerva-agent",
                _ => "other",
            };
            Some(Item::Letter(Box::new(Row {
                msgid,
                seq: entry.get("seq").and_then(Value::as_u64),
                at: entry.get("receivedAt").and_then(Value::as_str).and_then(parse_rfc3339).unwrap_or(seen),
                seen,
                node,
                mailbox,
                sender: sender.to_string(),
                via: name_of(entry.get("via")),
                subject: decoded.subject,
                text: body,
                format: decoded.format.to_string(),
                broken: decoded.broken,
                clipped,
                thread: decoded.thread,
                reply_to: decoded.reply_to,
            })))
        }
        _ => None,
    }
}

/// `data.entries` of a `mail read`, at most [`ENTRIES_MAX`] of them: the letters (deduped within the read) and the receipts.
fn parse_entries(data: &Value, seen: u64) -> (Vec<Row>, Vec<Receipt>, usize) {
    let (mut letters, mut receipts, mut skipped) = (Vec::new(), Vec::new(), 0);
    let mut ids = HashSet::new();
    let entries = data.get("entries").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    skipped += entries.len().saturating_sub(ENTRIES_MAX);
    for entry in entries.iter().take(ENTRIES_MAX) {
        match parse_entry(entry, seen) {
            Some(Item::Letter(row)) if ids.insert(row.msgid.clone()) => letters.push(*row),
            Some(Item::Letter(_)) => {}
            Some(Item::Receipt { of, from_node }) => receipts.push((of, from_node)),
            None => skipped += 1,
        }
    }
    (letters, receipts, skipped)
}

/// `2026-10-07T09:00:00Z`, with a fraction and/or a `+HH:MM` offset, as seconds since the epoch. Anything else is `None`.
fn parse_rfc3339(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() < 20 || b.len() > 40 || !text.is_ascii() {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let digits = &text[from..to];
        digits.bytes().all(|d| d.is_ascii_digit()).then(|| digits.parse().ok()).flatten()
    };
    if b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (year, month, day, hour, minute, second) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut rest = &text[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &fraction[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        zone if zone.len() == 6 && matches!(zone.as_bytes()[0], b'+' | b'-') && zone.as_bytes()[3] == b':' => {
            let sign = if zone.as_bytes()[0] == b'-' { -1 } else { 1 };
            let (h, m) = (zone[1..3].parse::<i64>().ok()?, zone[4..6].parse::<i64>().ok()?);
            if h > 23 || m > 59 {
                return None;
            }
            sign * (h * 3600 + m * 60)
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    u64::try_from(days * 86400 + hour * 3600 + minute * 60 + second - offset).ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

// ---------------------------------------------------------------------------
// The status of a sent row
// ---------------------------------------------------------------------------

/// `mail outbox --json`'s `data.entries`: message id to the status aoide reports and its reason. Read defensively.
fn outbox_of(data: &Value, fetched: u64) -> Outbox {
    let mut entries = BTreeMap::new();
    for entry in data.get("entries").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default().iter().take(ENTRIES_MAX) {
        let Some(msgid) = id_of(entry.get("msgid")) else { continue };
        let delivery = entry.get("delivery");
        let reported = delivery.and_then(|d| d.get("status")).and_then(Value::as_str).filter(|word| mail::word_ok(word));
        let refused = entry.get("refused").and_then(Value::as_bool).unwrap_or(false);
        let status = reported.map(str::to_string).unwrap_or_else(|| if refused { "refused" } else { "queued" }.to_string());
        let reason = delivery.and_then(|d| d.get("reason")).and_then(Value::as_str).map(|r| mail::line_safe(r, 200)).filter(|r| !r.is_empty());
        entries.insert(msgid, OutboxEntry { status, reason });
    }
    Outbox { at: Some(Instant::now()), fetched, entries }
}

/// The status a sent row has now, and the reason when there is one. `delivered`: a receipt came back, or the letter was filed on this
/// machine. Otherwise what the outbox says; a row that is not in an outbox fetched after it was sent has left it: `accepted`. Without
/// an outbox answer, the status the send reported.
fn current(row: &SentRow, delivered: &BTreeMap<String, u64>, outbox: Option<&Outbox>) -> (String, Option<String>) {
    let Some(msgid) = row.msgid.as_deref() else { return (row.status.clone(), None) };
    if delivered.contains_key(msgid) || row.disposition == "filed" {
        return ("delivered".to_string(), None);
    }
    if let Some(outbox) = outbox {
        if let Some(entry) = outbox.entries.get(msgid) {
            return (entry.status.clone(), entry.reason.clone());
        }
        if outbox.fetched > row.ts {
            return ("accepted".to_string(), None);
        }
    }
    (row.status.clone(), None)
}

/// Whether `mail outbox --json` has something to tell about this row that the cached answer does not.
fn waiting(row: &SentRow, delivered: &BTreeMap<String, u64>, outbox: Option<&Outbox>, now: u64) -> bool {
    let Some(msgid) = row.msgid.as_deref() else { return false };
    if delivered.contains_key(msgid) || row.disposition == "filed" || now.saturating_sub(row.ts) > WAIT_ROW {
        return false;
    }
    match outbox.map(|outbox| (outbox, outbox.entries.get(msgid))) {
        Some((_, Some(entry))) => entry.status != "refused",
        Some((outbox, None)) => outbox.fetched <= row.ts,
        None => true,
    }
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Paths {
    dir: PathBuf,
    received: PathBuf,
    cards: PathBuf,
    sent: PathBuf,
}

fn paths(ctx: &Ctx) -> Option<Paths> {
    let dir = mail::mail_dir(ctx)?;
    Some(Paths { received: dir.join("received.jsonl"), cards: dir.join("cards.json"), sent: dir.join("sent.jsonl"), dir })
}

/// A file's trouble as a sentence; the error itself goes to the log, where a path is no harm.
fn io_sentence(what: &str, e: &std::io::Error) -> String {
    tracing::warn!(error = %e, what, "perch: a mail file was not used");
    format!("{what} could not be used as a plain file")
}

/// `cards.json`, or an empty set when it is not there. A file that does not parse is set aside by `statefile::load` (and the cards
/// start empty); one that is not a plain file or is far too big is not touched, and mail says so.
fn load_cards(path: &Path) -> Result<Cards, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= CARDS_FILE_CAP => Ok(crate::statefile::load::<Cards>(Some(path))),
        Ok(_) => Err("cards.json is not a plain file of a size the perch reads".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Cards::default()),
        Err(e) => Err(io_sentence("cards.json", &e)),
    }
}

/// Reads the cards (loading them first if this is the first need) under the lock.
fn with_cards<T>(state: &State, paths: &Paths, work: impl FnOnce(&Cards) -> T) -> Result<T, String> {
    let mut held = state.cards.lock().unwrap_or_else(|e| e.into_inner());
    if held.is_none() {
        *held = Some(load_cards(&paths.cards)?);
    }
    held.as_ref().map(work).ok_or_else(|| "the cards could not be loaded".to_string())
}

fn persist(paths: &Paths, cards: &mut Cards) -> std::io::Result<()> {
    cards.v = 1;
    mail::ensure_dir(&paths.dir)?;
    let bytes = serde_json::to_vec(cards).map_err(std::io::Error::other)?;
    harnox::fs::write_atomic_0600(&paths.cards, &bytes)
}

/// A refusal of a card route: a status and its sentence.
type Fail = (StatusCode, String);

fn fail(code: StatusCode, why: &str) -> Fail {
    (code, why.to_string())
}

/// Changes the cards: `work` runs on a copy, the copy is written, and only a copy that was written becomes the cards.
fn change<T>(state: &State, paths: &Paths, work: impl FnOnce(&mut Cards) -> Result<T, Fail>) -> Result<T, Fail> {
    let mut held = state.cards.lock().unwrap_or_else(|e| e.into_inner());
    if held.is_none() {
        *held = Some(load_cards(&paths.cards).map_err(|why| (StatusCode::SERVICE_UNAVAILABLE, why))?);
    }
    let mut next = held.clone().unwrap_or_default();
    let done = work(&mut next)?;
    persist(paths, &mut next).map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, io_sentence("cards.json", &e)))?;
    *held = Some(next);
    Ok(done)
}

/// Work on a blocking thread. The join failing is the only error of its own.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, Fail> {
    tokio::task::spawn_blocking(work).await.map_err(|e| {
        tracing::warn!(error = %e, "perch: a mail call did not finish");
        fail(StatusCode::INTERNAL_SERVER_ERROR, "that did not finish")
    })
}

/// The newest row of `received.jsonl` with this message id, found in its last 36 MiB (the file is held to 32 MiB plus one read).
fn find_row(path: &Path, msgid: &str) -> Result<Option<Row>, String> {
    let bytes = mail::read_tail(path, RECEIVED_CAP + REREAD_CAP as u64).map_err(|e| io_sentence("received.jsonl", &e))?;
    let needle = msgid.as_bytes();
    let mut found = None;
    for line in bytes.split(|&b| b == b'\n') {
        // A parse is only for a line that names the id at all.
        if line.windows(needle.len()).any(|window| window == needle)
            && let Ok(row) = serde_json::from_slice::<Row>(line)
            && row.msgid == msgid
        {
            found = Some(row);
        }
    }
    Ok(found)
}

/// The nodes the rows of `sent.jsonl` went to, by message id: what a receipt is held to.
fn sent_nodes(path: &Path) -> BTreeMap<String, String> {
    let bytes = mail::read_tail(path, MATCH_WINDOW).unwrap_or_default();
    mail::rows_of::<SentRow>(&bytes)
        .into_iter()
        .filter_map(|row| Some((row.msgid?, row.to.split('/').next()?.to_string())))
        .collect()
}

/// Records what a read found: new letters at the end of `received.jsonl` and in the cards, receipts that name a sent row from the node
/// it went to. The cards are written last, so a letter that is in the file and not in the cards is simply read again and shown once
/// (the listing keeps one row per message id). Returns how many letters were new.
fn apply(state: &State, paths: &Paths, letters: &[Row], receipts: Vec<Receipt>) -> Result<usize, String> {
    let mut held = state.cards.lock().unwrap_or_else(|e| e.into_inner());
    if held.is_none() {
        *held = Some(load_cards(&paths.cards)?);
    }
    let current = held.clone().unwrap_or_default();
    let fresh: Vec<&Row> = letters.iter().filter(|row| !current.cards.contains_key(&row.msgid)).collect();
    let mut next = current.clone();
    let now = mail::now();
    for row in &fresh {
        next.cards.insert(row.msgid.clone(), Slot { state: Mood::New, chat: None, at: now });
    }
    if !receipts.is_empty() {
        let sent = sent_nodes(&paths.sent);
        for (of, from_node) in receipts {
            let to = sent.get(&of).filter(|to| to.as_str() != "self");
            if to.is_some() && to.map(String::as_str) == from_node.as_deref() {
                next.delivered.entry(of).or_insert(now);
            }
        }
        while next.delivered.len() > DELIVERED_KEEP {
            let Some(oldest) = next.delivered.iter().min_by_key(|(_, at)| **at).map(|(id, _)| id.clone()) else { break };
            next.delivered.remove(&oldest);
        }
    }
    if next.cards == current.cards && next.delivered == current.delivered {
        return Ok(0);
    }
    if !fresh.is_empty() {
        let lines: Vec<String> = fresh.iter().filter_map(|row| serde_json::to_string(row).ok()).collect();
        mail::ensure_dir(&paths.dir).map_err(|e| io_sentence("the mail folder", &e))?;
        mail::append_lines(&paths.received, &lines).map_err(|e| io_sentence("received.jsonl", &e))?;
    }
    persist(paths, &mut next).map_err(|e| io_sentence("cards.json", &e))?;
    let added = fresh.len();
    *held = Some(next);
    Ok(added)
}

/// Whether the task may read: fewer than [`MAX_CARDS`] cards and less than [`RECEIVED_CAP`] of letters.
fn room(state: &State, paths: &Paths) -> Result<bool, String> {
    let cards = with_cards(state, paths, |cards| cards.cards.len())?;
    let len = std::fs::symlink_metadata(&paths.received).map(|meta| meta.len()).unwrap_or(0);
    Ok(cards < MAX_CARDS && len < RECEIVED_CAP)
}

// ---------------------------------------------------------------------------
// GET /hub/mail
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Snapshot {
    received: Vec<Value>,
    more: usize,
    sent: Vec<Value>,
}

/// A card as the page gets it: the row, its state and the chat it was placed with.
fn card(row: &Row, slot: &Slot) -> Value {
    let mut card = serde_json::to_value(row).unwrap_or_else(|_| json!({}));
    card["state"] = json!(slot.state);
    card["chat"] = json!(slot.chat);
    card
}

/// A sent row as the page gets it: what was written, with the status it has now and why.
fn sent_view(row: &SentRow, delivered: &BTreeMap<String, u64>, outbox: Option<&Outbox>) -> Value {
    let (status, reason) = current(row, delivered, outbox);
    let mut view = serde_json::to_value(row).unwrap_or_else(|_| json!({}));
    view["status"] = json!(status);
    view["reason"] = json!(reason);
    view
}

/// The newest cards that are not dismissed, and the newest sent rows. Reads two files' ends and the cards, and runs nothing.
fn snapshot(state: &State, paths: &Paths, outbox: Option<&Outbox>) -> Result<Snapshot, String> {
    let letters = mail::read_tail(&paths.received, RECEIVED_WINDOW).map_err(|e| io_sentence("received.jsonl", &e))?;
    let sent = mail::read_tail(&paths.sent, SENT_WINDOW).map_err(|e| io_sentence("sent.jsonl", &e))?;
    let rows: Vec<Row> = mail::rows_of(&letters);
    let sent_rows: Vec<SentRow> = mail::rows_of(&sent);
    with_cards(state, paths, |cards| {
        let mut snap = Snapshot::default();
        let mut seen = HashSet::new();
        for row in rows.iter().rev() {
            if !seen.insert(row.msgid.as_str()) {
                continue;
            }
            // A row with no slot is one whose cards were never written (the crash window): the next read puts it right.
            let Some(slot) = cards.cards.get(&row.msgid).filter(|slot| slot.state != Mood::Dismissed) else { continue };
            if snap.received.len() < CARDS_SHOWN {
                snap.received.push(card(row, slot));
            } else {
                snap.more += 1;
            }
        }
        snap.sent = sent_rows.iter().rev().take(SENT_SHOWN).map(|row| sent_view(row, &cards.delivered, outbox)).collect();
        snap
    })
}

const NO_AOIDE: &str = "Aoide is not available here, so nothing can be sent or read: start the perch with --aoide <absolute path>";

/// `GET /hub/mail`: also the page saying it is looking, which is what lets the task read.
async fn list(ctx: &Ctx, paths: Paths) -> Response<BoxBody> {
    ctx.mail.look(mail::knob_ms(ctx, "mail_watch_ms", mail::WATCH));
    let available = Aoide::from_options(&ctx.opts).is_ok();
    let outbox = ctx.mail.recv.outbox.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let notice = ctx.mail.recv.notice.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let held = ctx.clone();
    let read = blocking(move || snapshot(&held.mail.recv, &paths, outbox.as_deref())).await;
    let (snap, error) = match read {
        Ok(Ok(snap)) => (snap, notice),
        Ok(Err(why)) | Err((_, why)) => (Snapshot::default(), Some(why)),
    };
    let error = error.or_else(|| (!available).then(|| NO_AOIDE.to_string()));
    json(
        StatusCode::OK,
        &json!({ "available": available, "received": snap.received, "more": snap.more, "sent": snap.sent, "error": error }),
    )
}

// ---------------------------------------------------------------------------
// POST /hub/mail/<msgid>/place|dismiss|save
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Verb {
    Place,
    Dismiss,
    Save,
}

#[derive(Debug, PartialEq, Eq)]
enum Target<'a> {
    List,
    Card(&'a str, Verb),
    Nobody,
}

/// `""` is the list; `<msgid>/<verb>` is a card (a message id is 64 lower-case hex digits and a verb is one of three); the rest is nobody's.
fn target_of(rest: &str) -> Target<'_> {
    if rest.is_empty() {
        return Target::List;
    }
    let Some((msgid, verb)) = rest.split_once('/') else { return Target::Nobody };
    let verb = match verb {
        "place" => Verb::Place,
        "dismiss" => Verb::Dismiss,
        "save" => Verb::Save,
        _ => return Target::Nobody,
    };
    if mail::hex64(msgid) { Target::Card(msgid, verb) } else { Target::Nobody }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Place {
    chat: String,
}

/// A JSON object of `T` under [`BODY_CAP`]. Only an object, and only its fields.
async fn asked<T: serde::de::DeserializeOwned>(req: Request<Incoming>, shape: &str) -> Result<T, Box<Response<BoxBody>>> {
    let (_, bytes) = body_of(req, BODY_CAP).await?;
    let refused = || Box::new(problem(StatusCode::BAD_REQUEST, shape));
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    if !value.is_object() {
        return Err(refused());
    }
    serde_json::from_value(value).map_err(|_| refused())
}

/// `dismiss` and `save` take nothing: an empty body, or an empty object.
async fn nothing(req: Request<Incoming>) -> Result<(), Box<Response<BoxBody>>> {
    let (_, bytes) = body_of(req, BODY_CAP).await?;
    if bytes.is_empty() || serde_json::from_slice::<Value>(&bytes).is_ok_and(|value| value.as_object().is_some_and(|object| object.is_empty())) {
        Ok(())
    } else {
        Err(Box::new(problem(StatusCode::BAD_REQUEST, "expected no body, or {}")))
    }
}

/// Whether `id` is a chat this perch knows: a saved log in either source, or a door it holds.
fn chat_known(ctx: &Ctx, id: &str) -> bool {
    sessions::id_ok(id) && (ctx.registry.find_log(id).is_some() || !matches!(ctx.registry.lookup(id), sessions::Lookup::NotOurs))
}

fn failed(fail: Fail) -> Response<BoxBody> {
    problem(fail.0, &fail.1)
}

/// `rest` is `""` or `<msgid>/<verb>`; `nodes` and `a/...` never get here (`mail::pick`). Every method but the one a route takes is the
/// 404 of a route that is not there, as the mesh's are.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    let Some(paths) = paths(ctx) else { return status(StatusCode::NOT_FOUND) };
    let (method, query) = (req.method().clone(), req.uri().query().unwrap_or("").to_string());
    match target_of(rest) {
        Target::List if method == Method::GET => {
            drop(req);
            if !query.is_empty() {
                return problem(StatusCode::BAD_REQUEST, "expected no query");
            }
            list(ctx, paths).await
        }
        Target::Card(msgid, verb) if method == Method::POST => {
            if !query.is_empty() {
                return problem(StatusCode::BAD_REQUEST, "expected no query");
            }
            card_verb(ctx, req, paths, msgid, verb).await
        }
        _ => status(StatusCode::NOT_FOUND),
    }
}

async fn card_verb(ctx: &Ctx, req: Request<Incoming>, paths: Paths, msgid: &str, verb: Verb) -> Response<BoxBody> {
    let held = ctx.clone();
    let id = msgid.to_string();
    match verb {
        Verb::Place => {
            let ask = match asked::<Place>(req, "expected {\"chat\": the id of a chat}").await {
                Ok(ask) => ask,
                Err(refusal) => return *refusal,
            };
            if !chat_known(ctx, &ask.chat) {
                return problem(StatusCode::NOT_FOUND, "no chat has that id");
            }
            let chat = ask.chat;
            let placed = blocking(move || {
                change(&held.mail.recv, &paths, |cards| {
                    let slot = cards.cards.get_mut(&id).ok_or_else(|| fail(StatusCode::NOT_FOUND, "no such card"))?;
                    if slot.state == Mood::Dismissed {
                        return Err(fail(StatusCode::CONFLICT, "that card was dismissed"));
                    }
                    *slot = Slot { state: Mood::Placed, chat: Some(chat.clone()), at: mail::now() };
                    Ok(chat)
                })
            })
            .await;
            match placed.and_then(|done| done) {
                Ok(chat) => json(StatusCode::OK, &json!({ "msgid": msgid, "state": "placed", "chat": chat })),
                Err(fail) => failed(fail),
            }
        }
        Verb::Dismiss => {
            if let Err(refusal) = nothing(req).await {
                return *refusal;
            }
            let dismissed = blocking(move || {
                change(&held.mail.recv, &paths, |cards| {
                    let slot = cards.cards.get_mut(&id).ok_or_else(|| fail(StatusCode::NOT_FOUND, "no such card"))?;
                    *slot = Slot { state: Mood::Dismissed, chat: None, at: mail::now() };
                    Ok(())
                })
            })
            .await;
            match dismissed.and_then(|done| done) {
                Ok(()) => json(StatusCode::OK, &json!({ "msgid": msgid, "state": "dismissed" })),
                Err(fail) => failed(fail),
            }
        }
        Verb::Save => {
            if let Err(refusal) = nothing(req).await {
                return *refusal;
            }
            let found = blocking(move || {
                let known = with_cards(&held.mail.recv, &paths, |cards| cards.cards.contains_key(&id))?;
                if !known {
                    return Ok(None);
                }
                find_row(&paths.received, &id).map(Some)
            })
            .await;
            let row = match found {
                Ok(Ok(Some(Some(row)))) => row,
                Ok(Ok(Some(None))) => return problem(StatusCode::NOT_FOUND, "the text of that card is not on file"),
                Ok(Ok(None)) => return problem(StatusCode::NOT_FOUND, "no such card"),
                Ok(Err(why)) => return problem(StatusCode::SERVICE_UNAVAILABLE, &why),
                Err(fail) => return failed(fail),
            };
            // A letter's note starts with its subject. A raw one has no subject of its own to put there: its first line is the title.
            let (title, text) = if row.format == "letter" { (row.subject.as_str(), row.text.as_str()) } else { ("", row.text.as_str()) };
            match notes::create(ctx, title, text).await {
                Ok(created) => json(StatusCode::CREATED, &created),
                Err(refusal) => *refusal,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The receive task
// ---------------------------------------------------------------------------

/// The receive task, started after the bind. It does nothing until a page has looked, and nothing at all for a perch with no home.
pub fn start(ctx: &Ctx) {
    if paths(ctx).is_some() {
        tokio::spawn(task(ctx.clone()));
    }
}

/// What the task remembers between ticks.
struct Task {
    /// The stat of `base.jsonl` when it was last read: `None` before the first look, `Some(None)` for a file that is not there.
    last: Option<Option<Stat>>,
    /// A `--reread` is due: the first time, and after a read that may have lost letters.
    reread: bool,
    /// Reads rest until then (after a failure).
    quiet_until: Option<Instant>,
    outbox_pause: Option<Instant>,
}

/// What the gate compares: the file as it is. Not its content, which is aoide's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stat {
    ino: u64,
    len: u64,
    mtime: i64,
    nsec: i64,
}

fn stat_of(path: &Path) -> Option<Stat> {
    let meta = std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file)?;
    Some(Stat { ino: meta.ino(), len: meta.len(), mtime: meta.mtime(), nsec: meta.mtime_nsec() })
}

async fn task(ctx: Ctx) {
    let Some(paths) = paths(&ctx) else { return };
    let tick = mail::knob_ms(&ctx, "mail_tick_ms", mail::TICK);
    let watch = mail::knob_ms(&ctx, "mail_watch_ms", mail::WATCH);
    let mut task = Task { last: None, reread: true, quiet_until: None, outbox_pause: None };
    loop {
        tokio::select! {
            _ = ctx.stop.cancelled() => return,
            _ = tokio::time::sleep(tick) => {}
            _ = ctx.mail.wake.notified() => {}
        }
        if !ctx.mail.looked_within(watch) {
            continue;
        }
        // A tick that is in the middle of a call when the perch stops is dropped, and the runner's child with it.
        tokio::select! {
            _ = ctx.stop.cancelled() => return,
            _ = tick_once(&ctx, &paths, &mut task) => {}
        }
    }
}

fn set_notice(ctx: &Ctx, why: Option<String>) {
    *ctx.mail.recv.notice.lock().unwrap_or_else(|e| e.into_inner()) = why;
}

/// One look: the outbox if a row waits on it, then the gate, then (only when the mailbox changed) the read.
async fn tick_once(ctx: &Ctx, paths: &Paths, task: &mut Task) {
    let Ok(aoide) = Aoide::from_options(&ctx.opts) else { return };
    let Some(home) = mesh::home(&ctx.opts) else { return };
    refresh_outbox(ctx, &aoide, paths, task).await;
    if task.quiet_until.is_some_and(|until| Instant::now() < until) {
        return;
    }
    let stat = stat_of(&home.join("state").join("mail").join("base.jsonl"));
    if task.last == Some(stat) && !task.reread {
        return;
    }
    if stat.is_none() {
        // No mailbox yet: nothing to read, and so nothing to ask aoide.
        task.last = Some(None);
        return;
    }
    let pause = mail::knob_ms(ctx, "mail_pause_ms", PAUSE);
    let (held, kept) = (ctx.clone(), paths.clone());
    match blocking(move || room(&held.mail.recv, &kept)).await {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            set_notice(ctx, Some("the card store is full (5000 cards or 32 MiB of letters): move received.jsonl and cards.json aside to go on".to_string()));
            return;
        }
        Ok(Err(why)) | Err((_, why)) => {
            set_notice(ctx, Some(why));
            task.quiet_until = Some(Instant::now() + pause);
            return;
        }
    }
    if task.reread {
        task.reread = false;
        if let Err(failure) = read(ctx, &aoide, paths, true).await {
            set_notice(ctx, Some(failure.why));
            task.quiet_until = Some(Instant::now() + pause);
            return;
        }
    }
    match read(ctx, &aoide, paths, false).await {
        Ok(()) => {
            task.last = Some(stat);
            set_notice(ctx, None);
        }
        Err(failure) => {
            // A read that may have moved the cursor without the letters being kept is made up for by a reread, after the pause.
            task.reread = failure.lost;
            set_notice(ctx, Some(failure.why));
            task.quiet_until = Some(Instant::now() + pause);
        }
    }
}

struct Failure {
    why: String,
    /// The cursor may have moved without the letters being written down.
    lost: bool,
}

/// One `mail read` and what it found, written down. The log line has counts and never a word of a letter.
async fn read(ctx: &Ctx, aoide: &Aoide, paths: &Paths, reread: bool) -> Result<(), Failure> {
    let aoide = if reread { aoide.clone().limits(Limits { stdout_cap: REREAD_CAP, ..Limits::default() }) } else { aoide.clone() };
    let reply = aoide.run(&Shape::MailRead { reread }).await.map_err(|e| Failure {
        lost: !reread && matches!(e, Error::Timeout(_) | Error::TooBig(_) | Error::NotJson),
        why: format!("mail could not be read: {e}"),
    })?;
    let Some(data) = reply.data().cloned() else {
        return Err(Failure { why: "mail could not be read: aoide printed no data".to_string(), lost: !reread });
    };
    let (held, kept) = (ctx.clone(), paths.clone());
    let applied = blocking(move || {
        let (letters, receipts, skipped) = parse_entries(&data, mail::now());
        apply(&held.mail.recv, &kept, &letters, receipts).map(|added| (added, letters.len(), skipped))
    })
    .await;
    match applied {
        Ok(Ok((added, letters, skipped))) => {
            tracing::info!(reread, letters, added, skipped, "perch: mail was read");
            Ok(())
        }
        Ok(Err(why)) | Err((_, why)) => Err(Failure { why, lost: !reread }),
    }
}

/// `mail outbox --json`, when a row of `sent.jsonl` waits on it and the last answer is older than a minute. Never from a `GET`.
async fn refresh_outbox(ctx: &Ctx, aoide: &Aoide, paths: &Paths, task: &mut Task) {
    let ttl = mail::knob_ms(ctx, "mail_ttl_ms", OUTBOX_TTL);
    let held = ctx.mail.recv.outbox.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if held.as_ref().and_then(|outbox| outbox.at).is_some_and(|at| at.elapsed() < ttl) || task.outbox_pause.is_some_and(|until| Instant::now() < until) {
        return;
    }
    let (state, kept, known) = (ctx.clone(), paths.clone(), held);
    let waits = blocking(move || {
        let rows: Vec<SentRow> = mail::rows_of(&mail::read_tail(&kept.sent, SENT_WINDOW).unwrap_or_default());
        let now = mail::now();
        with_cards(&state.mail.recv, &kept, |cards| rows.iter().any(|row| waiting(row, &cards.delivered, known.as_deref(), now))).unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    if !waits {
        return;
    }
    // When the run began, not when it ended: a row sent while it ran is not in its answer.
    let began = mail::now();
    match aoide.run(&Shape::MailOutboxJson).await {
        Ok(reply) => {
            if let Some(data) = reply.data() {
                *ctx.mail.recv.outbox.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(outbox_of(data, began)));
            }
        }
        Err(e) => {
            tracing::warn!(why = %e, "perch: the outbox was not read");
            task.outbox_pause = Some(Instant::now() + mail::knob_ms(ctx, "mail_pause_ms", PAUSE));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, from_node: &str, from_name: &str, msgid: &str, text: &str) -> Value {
        json!({
            "seq": 3,
            "receivedAt": "2026-10-07T09:10:00Z",
            "type": kind,
            "via": "osaka",
            "envelope": {
                "header": { "version": "1", "from": { "node": from_node, "name": from_name }, "type": kind },
                "text": text,
                "sig": "ab",
                "msgid": msgid,
            },
        })
    }

    fn letter(item: Option<Item>) -> Row {
        match item {
            Some(Item::Letter(row)) => *row,
            other => panic!("not a letter: {other:?}"),
        }
    }

    const ID: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    #[test]
    fn a_structured_letter_is_decoded_and_a_broken_one_is_kept_raw() {
        let thread = "c".repeat(64);
        let text = format!(
            "AOIDE-LETTER/1\n{{\"subject\":\"Plan: ship\\nthe mesh view\",\"to\":[],\"cc\":[],\"body\":\"One.\\nTwo \\\"q\\\" 世界\",\"threadId\":\"{thread}\",\"replyTo\":\"not-hex\"}}"
        );
        let text = text.as_str();
        let row = letter(parse_entry(&entry("letter", "osaka", "minerva-agent", ID, text), 99));
        assert_eq!(row.format, "letter");
        assert_eq!(row.subject, "Plan: shipthe mesh view", "a subject is one printable line");
        assert_eq!(row.text, "One.\nTwo \"q\" 世界");
        assert_eq!((row.sender.as_str(), row.node.as_deref(), row.mailbox.as_deref()), ("minerva-agent", Some("osaka"), Some("minerva-agent")));
        assert_eq!(row.thread.as_deref(), Some("c".repeat(64).as_str()));
        assert_eq!(row.reply_to, None, "an id that is not 64 hex digits is not kept");
        assert_eq!((row.at, row.seen, row.via.as_deref()), (parse_rfc3339("2026-10-07T09:10:00Z").unwrap(), 99, Some("osaka")));
        assert!(!row.broken);

        let cut = "AOIDE-LETTER/1\n{\"subject\":\"half a letter\",\"to\":[],\"cc\":[],\"bod";
        let row = letter(parse_entry(&entry("letter", "osaka", "minerva", ID, cut), 99));
        assert_eq!((row.format.as_str(), row.broken, row.subject.as_str()), ("raw", true, "(an unreadable letter)"));
        assert_eq!(row.text, cut, "the text is kept whole");
        // The marker alone, and a marker with no body, are broken too; a note that merely begins with the words is not a letter at all.
        for text in ["AOIDE-LETTER/1", "AOIDE-LETTER/1\n{\"subject\":\"s\"}", "AOIDE-LETTER/1\n[1,2]", "AOIDE-LETTER/1\r\nnot json"] {
            assert!(letter(parse_entry(&entry("letter", "osaka", "minerva", ID, text), 1)).broken, "{text:?}");
        }
        let prose = letter(parse_entry(&entry("letter", "osaka", "minerva", ID, "AOIDE-LETTER/1 is the name of the format\nsecond"), 1));
        assert_eq!((prose.broken, prose.format.as_str(), prose.subject.as_str()), (false, "raw", "AOIDE-LETTER/1 is the name of the format"));
    }

    #[test]
    fn hostile_text_and_headers_are_clamped_and_the_text_is_kept_as_text() {
        let hostile = "<script>alert(1)</script></script>\u{1b}[31mred\u{1b}[0m\u{1b}]0;pwned\u{7} {{7*7}} ${x} \u{202e} rtl\n\n```\nfence\n````";
        let row = letter(parse_entry(&entry("letter", "<b>evil</b>\u{1b}[1m", "minerva-agent\n--hold", ID, hostile), 5));
        assert_eq!(row.text, hostile, "the body is not touched");
        assert_eq!((row.node, row.mailbox, row.sender.as_str()), (None, None, "other"), "a name outside the grammar is nothing");
        // The subject of a raw letter is its first line, with the escape characters, the bell and the bidi override dropped.
        assert_eq!(row.subject, "<script>alert(1)</script></script>[31mred[0m]0;pwned {{7*7}} ${x}  rtl");
        let by_name = |from_name: &str| letter(parse_entry(&entry("letter", "osaka", from_name, ID, "x"), 5)).sender;
        let senders = [by_name("minerva"), by_name("minerva-agent"), by_name("Minerva"), by_name("minerva-agent2"), by_name("conductor")];
        assert_eq!(senders, ["minerva", "minerva-agent", "other", "other", "other"]);
        // A time that is not a time is when the perch read it; an id that is not an id is no card.
        let mut odd = entry("letter", "osaka", "minerva", ID, "x");
        odd["receivedAt"] = json!("yesterday-ish");
        assert_eq!(letter(parse_entry(&odd, 77)).at, 77);
        for bad in ["not-hex", "", &"A".repeat(64), &"a".repeat(63), &"a".repeat(65)] {
            assert_eq!(parse_entry(&entry("letter", "osaka", "minerva", bad, "x"), 1), None, "{bad:?}");
        }
        assert_eq!(parse_entry(&entry("note-to-self", "osaka", "minerva", ID, "x"), 1), None);
        assert_eq!(parse_entry(&json!({ "type": "letter", "envelope": { "msgid": ID } }), 1), None, "no text");
        assert_eq!(parse_entry(&json!("a string"), 1), None);
    }

    #[test]
    fn a_big_text_is_cut_at_a_character_and_a_nul_is_not_kept() {
        let long = "世".repeat(mail::TEXT_MAX); // 3 bytes each
        let row = letter(parse_entry(&entry("letter", "osaka", "minerva", ID, &long), 1));
        assert!(row.clipped && row.text.len() <= mail::TEXT_MAX && row.text.len() > mail::TEXT_MAX - 3, "{}", row.text.len());
        assert!(row.text.chars().all(|c| c == '世'));
        let row = letter(parse_entry(&entry("letter", "osaka", "minerva", ID, "a\0b"), 1));
        assert_eq!((row.text.as_str(), row.clipped), ("a\u{fffd}b", false));
    }

    #[test]
    fn a_receipt_names_the_message_it_is_for() {
        let of = "6".repeat(64);
        let want = Some(Item::Receipt { of: of.clone(), from_node: Some("osaka".to_string()) });
        assert_eq!(parse_entry(&entry("receipt", "osaka", "minerva", ID, &format!(" {of}\n")), 1), want);
        for text in ["not an id", &"6".repeat(63), ""] {
            assert_eq!(parse_entry(&entry("receipt", "osaka", "minerva", ID, text), 1), None, "{text:?}");
        }
        let data = json!({ "entries": [
            entry("letter", "osaka", "minerva", ID, "one"),
            entry("letter", "osaka", "minerva", ID, "one again"),
            entry("receipt", "osaka", "minerva", &"2".repeat(64), &of),
            entry("letter", "osaka", "minerva", "bad", "x"),
        ]});
        let (letters, receipts, skipped) = parse_entries(&data, 1);
        assert_eq!((letters.len(), receipts.len(), skipped), (1, 1, 1), "a repeat in one read is one letter");
        assert_eq!(letters[0].text, "one");
    }

    #[test]
    fn times_are_read_as_utc_seconds_and_nothing_else_is() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2001-09-09T01:46:40Z"), Some(1_000_000_000));
        assert_eq!(parse_rfc3339("2001-09-09T01:46:40.123456Z"), Some(1_000_000_000));
        assert_eq!(parse_rfc3339("2001-09-09T03:46:40+02:00"), Some(1_000_000_000));
        assert_eq!(parse_rfc3339("2001-09-08T20:46:40-05:00"), Some(1_000_000_000));
        assert_eq!(parse_rfc3339("2024-02-29T12:00:00Z"), Some(1_709_208_000));
        assert_eq!(parse_rfc3339("2026-10-07T09:00:00Z"), Some(1_791_363_600));
        for bad in [
            "", "yesterday-ish", "2026-10-07", "2026-10-07T09:00:00", "2026-13-07T09:00:00Z", "2026-10-32T09:00:00Z", "2026-10-07T24:00:00Z",
            "2026-10-07T09:60:00Z", "2026-10-07T09:00:00.Z", "2026-10-07T09:00:00+0200", "2026-10-07T09:00:00+24:00", "２０２６-10-07T09:00:00Z",
            "1960-01-01T00:00:00Z", "2026-10-07T09:00:00Zjunk",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad:?}");
        }
    }

    fn sent(msgid: Option<&str>, disposition: &str, status: &str, ts: u64) -> SentRow {
        SentRow {
            ts,
            origin: "operator".to_string(),
            from: "minerva".to_string(),
            note: None,
            subject: "s".to_string(),
            sha256: "a".repeat(64),
            to: "osaka/minerva".to_string(),
            mesh: None,
            msgid: msgid.map(str::to_string),
            disposition: disposition.to_string(),
            status: status.to_string(),
        }
    }

    #[test]
    fn a_sent_rows_status_comes_from_a_receipt_then_the_outbox() {
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        let row = sent(Some(&a), "spooled", "queued", 100);
        let none = BTreeMap::new();
        assert_eq!(current(&row, &none, None).0, "queued", "no outbox answer: what the send said");
        let mut delivered = BTreeMap::new();
        delivered.insert(a.clone(), 150);
        assert_eq!(current(&row, &delivered, None).0, "delivered");
        assert_eq!(current(&sent(Some(&a), "filed", "delivered", 100), &none, None).0, "delivered", "filed on this machine");
        assert_eq!(current(&sent(None, "unknown", "unknown", 100), &delivered, None).0, "unknown", "no id, no way to follow it");

        let mut entries = BTreeMap::new();
        entries.insert(a.clone(), OutboxEntry { status: "retrying".to_string(), reason: Some("connection refused".to_string()) });
        let outbox = Outbox { at: None, fetched: 200, entries };
        assert_eq!(current(&row, &none, Some(&outbox)), ("retrying".to_string(), Some("connection refused".to_string())));
        let gone = sent(Some(&b), "spooled", "queued", 100);
        assert_eq!(current(&gone, &none, Some(&outbox)).0, "accepted", "it left the outbox after it was sent");
        let after = sent(Some(&b), "spooled", "queued", 200);
        assert_eq!(current(&after, &none, Some(&outbox)).0, "queued", "a row sent after the answer was fetched is not in it yet");
        assert_eq!(current(&gone, &delivered, Some(&outbox)).0, "accepted");
        delivered.insert(b.clone(), 1);
        assert_eq!(current(&gone, &delivered, Some(&outbox)).0, "delivered", "a receipt wins");
    }

    #[test]
    fn the_outbox_is_asked_about_only_while_a_row_waits_on_it() {
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        let none = BTreeMap::new();
        let now = 1_000_000;
        let row = sent(Some(&a), "spooled", "queued", now - 10);
        assert!(waiting(&row, &none, None, now), "no answer yet");
        let mut entries = BTreeMap::new();
        entries.insert(a.clone(), OutboxEntry { status: "retrying".to_string(), reason: None });
        entries.insert(b.clone(), OutboxEntry { status: "refused".to_string(), reason: None });
        let outbox = Outbox { at: None, fetched: now - 5, entries };
        assert!(waiting(&row, &none, Some(&outbox), now), "still in the outbox: it can change");
        assert!(!waiting(&sent(Some(&b), "spooled", "queued", now - 10), &none, Some(&outbox), now), "refused is the end of it");
        assert!(!waiting(&sent(Some(&"c".repeat(64)), "spooled", "queued", now - 50), &none, Some(&outbox), now), "gone from the outbox: accepted");
        assert!(waiting(&sent(Some(&"c".repeat(64)), "spooled", "queued", now - 2), &none, Some(&outbox), now), "sent after the answer");
        let mut delivered = BTreeMap::new();
        delivered.insert(a.clone(), 1);
        assert!(!waiting(&row, &delivered, None, now));
        assert!(!waiting(&sent(Some(&a), "filed", "delivered", now), &none, None, now));
        assert!(!waiting(&sent(None, "unknown", "unknown", now), &none, None, now));
        assert!(!waiting(&sent(Some(&a), "spooled", "queued", now - WAIT_ROW - 1), &none, None, now), "a week is long enough to wait");
    }

    #[test]
    fn the_outbox_is_read_defensively() {
        let data = json!({ "entries": [
            { "msgid": "a".repeat(64), "delivery": { "status": "retrying", "reason": "bad\u{1b}[0m reason" } },
            { "msgid": "b".repeat(64), "refused": true },
            { "msgid": "c".repeat(64), "delivery": { "status": "Not A Word" } },
            { "msgid": "short", "delivery": { "status": "queued" } },
            "junk",
        ]});
        let outbox = outbox_of(&data, 42);
        assert_eq!(outbox.fetched, 42);
        assert_eq!(outbox.entries.len(), 3);
        assert_eq!(outbox.entries[&"a".repeat(64)], OutboxEntry { status: "retrying".to_string(), reason: Some("bad[0m reason".to_string()) });
        assert_eq!(outbox.entries[&"b".repeat(64)].status, "refused");
        assert_eq!(outbox.entries[&"c".repeat(64)].status, "queued");
        assert!(outbox_of(&json!(null), 1).entries.is_empty());
    }

    #[test]
    fn a_card_path_is_a_message_id_and_one_of_three_verbs() {
        assert_eq!(target_of(""), Target::List);
        assert_eq!(target_of(&format!("{ID}/place")), Target::Card(ID, Verb::Place));
        assert_eq!(target_of(&format!("{ID}/dismiss")), Target::Card(ID, Verb::Dismiss));
        assert_eq!(target_of(&format!("{ID}/save")), Target::Card(ID, Verb::Save));
        for rest in [
            ID.to_string(),
            format!("{ID}/"),
            format!("{ID}/Place"),
            format!("{ID}/place/x"),
            format!("{ID}//place"),
            format!("{}/place", "A".repeat(64)),
            format!("{}/place", &ID[1..]),
            format!("{ID}x/place"),
            "nodes/place".to_string(),
            "/place".to_string(),
            format!("%31{}/place", &ID[1..]),
            "a/place".to_string(),
        ] {
            assert_eq!(target_of(&rest), Target::Nobody, "{rest:?}");
        }
    }
}
