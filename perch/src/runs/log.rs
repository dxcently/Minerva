//! `GET /hub/runs/<rid>/log?after=N` (slice R1b): the run's journal as `eidolon log --json --after N` lines, for the graph tab's poll.
//!
//! ```text
//!   GET /hub/runs/<rid>/log             200 application/x-ndjson: every record of the run's journal, one JSON line each
//!   GET /hub/runs/<rid>/log?after=N     200 the records whose id is above N, in order (eidolon's own `--after`: a poller's cursor)
//!       200 with an empty body: nothing above N yet, or the run has no journal yet (the child has not written one)
//!       400 {error, why} a query that is not exactly `after=<digits>` once         404 (empty) not a run this perch knows
//!       405 (empty) not a GET     413 a log over 16 MiB     502 eidolon failed     504 eidolon did not answer in 15 s
//! ```
//!
//! The lines are eidolon's, unread: the perch never decodes the journal and never reads a line of it here. The command is
//! `eidolon --config <the run's config.toml> log --json [--after N] <the run's .eid>` through [`crate::titles::run`] (time and size
//! bounded, started from an argv array, no secret anywhere near it), and `N` in the argv is the number parsed from the query, never its text.
//!
//! **The cache** is one answer per journal, keyed on what the journal was when eidolon was asked: its mtime (ns), its length, and `after`.
//! The same question of an unchanged journal is answered from memory and starts no child (the page asks every second while a run steps and
//! every five while it is parked, and a parked or ended run does not change). It is the *answer* that is kept, not "nothing new": a poll
//! whose response was lost and is asked again with the same `after` gets the same lines, not an empty body that would make it skip them.
//! The answer is kept only when the journal had not moved by the time eidolon had finished (so it is exactly what the stamp says) and when
//! it is at most 1 MiB. The slot of a journal is held across the child, so two polls of one run at once make one child.
//!
//! Every poll of a journal that did change starts one `eidolon log`, which rewrites `<config>/eidolon/system` in the run's own config dir;
//! the page's lane keeps that to one in flight, about one a second while a run is active.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::header::HeaderValue;
use hyper::{Method, Request, Response, StatusCode};
use serde_json::Value;

use super::state::{self, refuse};
use crate::serve::{BoxBody, Ctx, status};
use crate::titles::{self, LOG_CAP, LOG_TIMEOUT, RunError};

/// An answer bigger than this is given and not kept.
const KEPT_BYTES: usize = 1024 * 1024;
/// How many journals are remembered at once. One run is active at a time; the rest is somebody reading an old one.
const SLOTS_MAX: usize = 8;

fn full(bytes: impl Into<Bytes>) -> BoxBody {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed_unsync()
}

fn lines(body: Bytes) -> Response<BoxBody> {
    Response::builder().status(StatusCode::OK).header("content-type", HeaderValue::from_static("application/x-ndjson")).body(full(body)).unwrap()
}

/// `after`, from a query that is empty or exactly `after=<digits>`: a record id is a plain number, and nothing else reaches the argv.
fn parse_query(query: &str) -> Result<Option<u64>, &'static str> {
    if query.is_empty() {
        return Ok(None);
    }
    let mut after = None;
    for pair in query.split('&') {
        let Some(("after", value)) = pair.split_once('=') else { return Err("the only query is after=<a record id>") };
        if after.is_some() {
            return Err("after is given once");
        }
        if value.is_empty() || value.len() > 20 || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err("after is a record id: digits only");
        }
        after = Some(value.parse::<u64>().map_err(|_| "after is not a record id this large")?);
    }
    Ok(after)
}

/// What a journal looked like when it was asked about: its modification time in nanoseconds, and its length.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    mtime_ns: u128,
    len: u64,
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ns = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    Some(Stamp { mtime_ns, len: meta.len() })
}

/// The last answer for a journal: the question (`after`), what the journal was, and the lines.
struct Seen {
    stamp: Stamp,
    after: Option<u64>,
    lines: Bytes,
}

type Slot = Arc<tokio::sync::Mutex<Option<Seen>>>;

/// The slots by journal path. The outer lock is never held across an await; a slot's own lock is, and is what makes one child of two polls.
static SLOTS: Mutex<Vec<(PathBuf, Slot)>> = Mutex::new(Vec::new());

fn slot(journal: &Path) -> Slot {
    let mut slots = SLOTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, slot)) = slots.iter().find(|(path, _)| path == journal) {
        return slot.clone();
    }
    // Over the limit, one that nobody is using goes (a slot a request holds has another owner).
    if slots.len() >= SLOTS_MAX
        && let Some(idle) = slots.iter().position(|(_, slot)| Arc::strong_count(slot) == 1)
    {
        slots.remove(idle);
    }
    let fresh: Slot = Arc::default();
    slots.push((journal.to_path_buf(), fresh.clone()));
    fresh
}

fn refused(why: RunError) -> Response<BoxBody> {
    match why {
        RunError::TooBig => refuse(StatusCode::PAYLOAD_TOO_LARGE, "that run's log is too large to show here", Value::Null),
        RunError::Timeout => refuse(StatusCode::GATEWAY_TIMEOUT, "eidolon did not read that log in time", Value::Null),
        RunError::Failed(why) => {
            tracing::debug!(%why, "perch: eidolon log of a run failed");
            refuse(StatusCode::BAD_GATEWAY, &format!("eidolon could not read that log: {why}"), Value::Null)
        }
    }
}

/// `rest` is the run id, one path segment: `runs::pick` hands over `<rid>` of `<rid>/log` and nothing nested.
pub async fn get(ctx: &Ctx, req: Request<Incoming>, rid: &str) -> Response<BoxBody> {
    let (method, query) = (req.method().clone(), req.uri().query().unwrap_or("").to_string());
    drop(req);
    // A run this perch does not know (or an id that is not one) is nobody's: the empty 404 of every unknown run.
    let Some(config) = state::config_of(ctx, rid) else { return status(StatusCode::NOT_FOUND) };
    if method != Method::GET {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    }
    let after = match parse_query(&query) {
        Ok(after) => after,
        Err(why) => return refuse(StatusCode::BAD_REQUEST, why, Value::Null),
    };
    // The run exists and eidolon has not made its journal yet (the first instant of a start), or never did (a refusal): nothing is above any id.
    let Some(journal) = state::journal_of(ctx, rid) else { return lines(Bytes::new()) };
    let slot = slot(&journal);
    let mut seen = slot.lock().await;
    let before = stamp_of(&journal);
    if let (Some(now), Some(kept)) = (before, seen.as_ref())
        && kept.stamp == now
        && kept.after == after
    {
        return lines(kept.lines.clone());
    }
    let mut args: Vec<OsString> = vec!["log".into(), "--json".into()];
    if let Some(after) = after {
        args.extend(["--after".into(), after.to_string().into()]);
    }
    args.push(journal.as_os_str().to_os_string());
    let out = match titles::run(&ctx.registry.eidolon, Some(&config), &args, LOG_CAP, LOG_TIMEOUT).await {
        Ok(out) => out,
        Err(why) => {
            *seen = None;
            return refused(why);
        }
    };
    // Kept only if the journal had not moved while eidolon read it, so that what is kept is exactly what its stamp says.
    *seen = match (before, stamp_of(&journal)) {
        (Some(was), Some(is)) if was == is && out.len() <= KEPT_BYTES => Some(Seen { stamp: was, after, lines: Bytes::from(out.clone()) }),
        _ => None,
    };
    lines(Bytes::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_is_nothing_or_one_after_of_digits() {
        assert_eq!(parse_query(""), Ok(None));
        assert_eq!(parse_query("after=0"), Ok(Some(0)));
        assert_eq!(parse_query("after=0042"), Ok(Some(42)));
        assert_eq!(parse_query("after=18446744073709551615"), Ok(Some(u64::MAX)));
        for bad in ["after", "after=", "after=-1", "after=+1", "after=1.5", "after=1e3", "after=0x10", "after=%31", "after= 1", "After=1", "after=--json"] {
            assert!(parse_query(bad).is_err(), "{bad:?}");
        }
        // Twice, with company, with a stray separator, and one past u64 (and one that is merely long).
        for bad in ["after=1&after=2", "after=1&x=2", "x=1", "&after=1", "after=1&", "after=18446744073709551616", "after=111111111111111111111"] {
            assert!(parse_query(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_journal_has_one_slot_and_a_full_table_drops_an_idle_one() {
        let first = slot(Path::new("/unit/slots/first.eid"));
        assert!(Arc::ptr_eq(&first, &slot(Path::new("/unit/slots/first.eid"))), "the same journal is the same slot");
        let other = slot(Path::new("/unit/slots/other.eid"));
        assert!(!Arc::ptr_eq(&first, &other));
        // Fill the table well past its limit while `first` is in use: it is never the one dropped, however many come.
        for n in 0..(SLOTS_MAX * 3) {
            slot(Path::new(&format!("/unit/slots/n{n}.eid")));
        }
        assert!(Arc::ptr_eq(&first, &slot(Path::new("/unit/slots/first.eid"))), "a slot in use is kept");
        assert!(SLOTS.lock().unwrap().len() <= SLOTS_MAX, "the table is bounded");
        drop(other);
    }
}
