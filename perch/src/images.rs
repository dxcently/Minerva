//! Images in a chat, shown to the page: `GET /hub/sessions/<id>/images`, `.../image/<record>/<n>` and the
//! image-free `.../transcript`.
//!
//! **Where the bytes are.** The door never sends them (`wire.rs`: a frame carries a count). They live in the
//! log, and `eidolon log --json` is the one export door for the log, so this module asks it, exactly as the
//! `log` route does, and never decodes the journal itself. Verified against eidolon `0.1.0`: a user message
//! prints as `{"id":1,"kind":{"UserMessage":{"role":"user","content":[{"type":"image","source":{"type":
//! "base64","media_type":"image/png","data":"<base64>"},"alt":"dot.png"},{"type":"text",...}]}}}`.
//! A tool result is `{"ToolResult":{"tool_use_id","content": <string>,"is_error"}}`: text only, so today a tool
//! produces no image bytes in a log at all. A model-made image would be an `AssistantMessage` block of the same
//! shape; a user message that carries a `tool_result` block next to an image is the one place a tool's image
//! could arrive, so it is listed as `where: "tool"` with the call it answers. None of the last two occurs in a
//! log today; they are read because the shape allows them, and covered by a synthetic test.
//!
//! **One run per change.** The run's output is parsed once into [`Read`]: the decoded images and a *lean*
//! transcript (the same lines with every image's `data` emptied, which is what lets a chat with a few
//! megabytes of pictures still fit the 16 MiB the page's log read allows). It is kept per log path and
//! `(mtime, length)`: ten thumbnails are one eidolon run, and a live chat that grew is read again on the next
//! request. At most [`KEEP`] logs are held, the oldest dropped first.
//!
//! **What an image answer is.** Only `image/png|jpeg|gif|webp` are served (anything else is a 415), with the
//! type the log declared, `nosniff` and a `default-src 'none'; sandbox` policy, so the bytes can only ever be
//! drawn. A record is immutable once written, so a hit may be cached for an hour; a miss is never cached.
//! Read-only for every chat, live, kept or `other`: nothing is started, resumed or written.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::HeaderValue;
use hyper::{Response, StatusCode};
use serde_json::Value;

use crate::serve::{BoxBody, Ctx, json, problem, status};
use crate::sessions::id_ok;
use crate::titles::{self, RunError};

/// What one `eidolon log --json` may print before the read is refused. Images are most of a log that has any:
/// 8 per message at up to 4 MiB each, so this is far above the 16 MiB the plain log route allows.
pub const RUN_CAP: usize = 128 * 1024 * 1024;

/// The page's transcript, images emptied, is held to the plain log route's bound.
pub const LEAN_CAP: usize = titles::LOG_CAP;

/// Logs whose decoded images are held at once.
const KEEP: usize = 3;

/// The types a browser is told these bytes are. The door admits the same four.
const TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// A path this module answers.
#[derive(Debug, PartialEq, Eq)]
pub enum Route<'a> {
    List(&'a str),
    Transcript(&'a str),
    /// `<id>`, `<record>`, `<n>` as the URL spelled them: parsed by the handler, where a bad one is a 404.
    Bytes(&'a str, &'a str, &'a str),
}

/// Which of this module's routes a path is, if any.
pub fn route(path: &str) -> Option<Route<'_>> {
    let mut parts = path.strip_prefix("/hub/sessions/")?.split('/');
    let id = parts.next()?;
    let found = match (parts.next()?, parts.next(), parts.next(), parts.next()) {
        ("images", None, ..) => Route::List(id),
        ("transcript", None, ..) => Route::Transcript(id),
        ("image", Some(record), Some(n), None) => Route::Bytes(id, record, n),
        _ => return None,
    };
    Some(found)
}

/// One image block of a log.
struct Image {
    record: u64,
    /// Its position among the image blocks of that record's message.
    n: usize,
    media_type: String,
    place: &'static str,
    tool_call: Option<String>,
    alt: Option<String>,
    bytes: Vec<u8>,
}

/// What one run of `eidolon log --json` was turned into.
struct Read {
    /// `(mtime in ns, length)` of the log when the run began: the cache's whole notion of "changed".
    stamp: (u128, u64),
    images: Vec<Image>,
    lean: Vec<u8>,
}

type Slot = Arc<tokio::sync::Mutex<Option<Arc<Read>>>>;

/// Per log path, oldest first. One slot lock per log, so a slow read of one chat does not hold up another's.
fn slots() -> &'static Mutex<Vec<(PathBuf, Slot)>> {
    static SLOTS: OnceLock<Mutex<Vec<(PathBuf, Slot)>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(Vec::new()))
}

fn slot_for(path: &PathBuf) -> Slot {
    let mut all = slots().lock().unwrap();
    if let Some((_, slot)) = all.iter().find(|(p, _)| p == path) {
        return slot.clone();
    }
    // Drop the oldest slot nobody is using; a read in flight keeps its own.
    while all.len() >= KEEP {
        match all.iter().position(|(_, s)| Arc::strong_count(s) == 1) {
            Some(i) => {
                all.remove(i);
            }
            None => break,
        }
    }
    let slot: Slot = Arc::new(tokio::sync::Mutex::new(None));
    all.push((path.clone(), slot.clone()));
    slot
}

/// The log `id` names, resolved the way the `log` route resolves it, and whether it is the other source's.
#[allow(clippy::result_large_err)] // the Err is the response to send, as in trash.rs
fn resolve(ctx: &Ctx, id: &str) -> std::result::Result<(PathBuf, bool), Response<BoxBody>> {
    if !id_ok(id) {
        return Err(status(StatusCode::NOT_FOUND));
    }
    let Some((path, origin)) = ctx.registry.find_log(id) else { return Err(status(StatusCode::NOT_FOUND)) };
    // As for a resume: a symlink at `<id>.eid` must not name a file somewhere else.
    if ctx.registry.id_of_log(&path).as_deref() != Some(id) {
        return Err(status(StatusCode::NOT_FOUND));
    }
    Ok((path, origin.is_some()))
}

fn stamp_of(path: &std::path::Path) -> Result<(u128, u64)> {
    let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let modified = meta.modified().context("mtime")?.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    Ok((modified, meta.len()))
}

/// How many `eidolon log --json` reads (up to [`RUN_CAP`] each) run at once.
const LOG_READS: usize = 2;

fn log_reads() -> &'static tokio::sync::Semaphore {
    static READS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    READS.get_or_init(|| tokio::sync::Semaphore::new(LOG_READS))
}

/// The log, read (or the cached read of it, if it has not changed).
#[allow(clippy::result_large_err)]
async fn read(ctx: &Ctx, id: &str) -> std::result::Result<Arc<Read>, Response<BoxBody>> {
    let (path, other) = resolve(ctx, id)?;
    let Ok(stamp) = stamp_of(&path) else { return Err(status(StatusCode::NOT_FOUND)) };
    let slot = slot_for(&path);
    let mut held = slot.lock().await;
    if let Some(have) = held.as_ref()
        && have.stamp == stamp
    {
        return Ok(have.clone());
    }
    let config = if other { None } else { ctx.registry.eidolon_config.as_deref() };
    let args = ["log".into(), "--json".into(), path.clone().into()];
    // At most a couple of these at once, for the whole perch: each prints up to RUN_CAP into memory. (Held after the per-log slot, so a
    // waiter for another log's read does not hold this one up while it holds nothing.)
    let _reading = match log_reads().acquire().await {
        Ok(permit) => permit,
        Err(_) => return Err(status(StatusCode::SERVICE_UNAVAILABLE)),
    };
    let out = match titles::run(&ctx.registry.eidolon, config, &args, RUN_CAP, titles::LOG_TIMEOUT).await {
        Ok(out) => out,
        Err(RunError::TooBig) => return Err(problem(StatusCode::PAYLOAD_TOO_LARGE, "that chat's log is too large to read its images")),
        Err(RunError::Timeout) => return Err(problem(StatusCode::GATEWAY_TIMEOUT, "eidolon did not read that log in time")),
        Err(e) => {
            tracing::debug!(error = %e, "perch: eidolon log failed");
            return Err(problem(StatusCode::BAD_GATEWAY, &format!("eidolon could not read that log: {e}")));
        }
    };
    let scanned = tokio::task::spawn_blocking(move || scan(&out)).await;
    let Ok((images, lean)) = scanned else { return Err(problem(StatusCode::INTERNAL_SERVER_ERROR, "reading that log failed")) };
    let fresh = Arc::new(Read { stamp, images, lean });
    *held = Some(fresh.clone());
    Ok(fresh)
}

/// The chat's records as eidolon's own JSON lines with every image's bytes emptied (what the transcript route serves, whole), or the
/// refusal to answer with. For fork.rs/pending.rs, which read the shape of a chat and nothing else.
#[allow(clippy::result_large_err)]
pub(crate) async fn lean(ctx: &Ctx, id: &str) -> std::result::Result<Vec<u8>, Response<BoxBody>> {
    let done = read(ctx, id).await?;
    if done.lean.len() > LEAN_CAP {
        return Err(problem(StatusCode::PAYLOAD_TOO_LARGE, "that chat's log is too large to fork from a point"));
    }
    Ok(done.lean.clone())
}

/// The log's lines, once: the images found (decoded) and the lines with every image's `data` emptied. A line
/// with no image block is copied as it came, so everything else stays eidolon's own bytes.
fn scan(out: &[u8]) -> (Vec<Image>, Vec<u8>) {
    const MARK: &[u8] = b"\"type\":\"image\"";
    let mut images = Vec::new();
    let mut lean = Vec::with_capacity(out.len().min(LEAN_CAP + 1));
    for line in out.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        if line.windows(MARK.len()).any(|w| w == MARK)
            && let Ok(mut record) = serde_json::from_slice::<Value>(line)
            && lift(&mut record, &mut images)
            && let Ok(text) = serde_json::to_vec(&record)
        {
            lean.extend_from_slice(&text);
        } else {
            lean.extend_from_slice(line);
        }
        lean.push(b'\n');
    }
    (images, lean)
}

/// The images of one record, appended to `into` and emptied in place. Whether the record had any.
fn lift(record: &mut Value, into: &mut Vec<Image>) -> bool {
    let Some(id) = record.get("id").and_then(Value::as_u64) else { return false };
    let Some(kind) = record.get_mut("kind").and_then(Value::as_object_mut) else { return false };
    let place = if kind.contains_key("UserMessage") {
        "UserMessage"
    } else if kind.contains_key("AssistantMessage") {
        "AssistantMessage"
    } else {
        return false;
    };
    let Some(message) = kind.get_mut(place) else { return false };
    let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else { return false };
    // A user message that answers tool calls is where a tool's image would ride.
    let answered = content
        .iter()
        .rev()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .and_then(|b| b.get("tool_use_id").and_then(Value::as_str))
        .map(str::to_string);
    let place: &'static str = match (place, &answered) {
        ("AssistantMessage", _) => "assistant",
        (_, Some(_)) => "tool",
        _ => "user",
    };
    let mut n = 0;
    let mut any = false;
    for block in content.iter_mut() {
        if block.get("type").and_then(Value::as_str) != Some("image") {
            continue;
        }
        let at = n;
        n += 1;
        any = true;
        let alt = block.get("alt").and_then(Value::as_str).map(str::to_string);
        let Some(source) = block.get_mut("source").and_then(Value::as_object_mut) else { continue };
        if source.get("type").and_then(Value::as_str) != Some("base64") {
            continue;
        }
        let media_type = source.get("media_type").and_then(Value::as_str).unwrap_or_default().to_string();
        let data = source.get("data").and_then(Value::as_str).unwrap_or_default();
        let Some(bytes) = decode(data) else { continue };
        source.insert("data".into(), Value::String(String::new()));
        into.push(Image { record: id, n: at, media_type, place, tool_call: answered.clone().filter(|_| place == "tool"), alt, bytes });
    }
    any
}

/// Strict standard padded base64, the alphabet the door checks on the way in. No dependency: this is the one
/// decode the perch does, and the input is a log eidolon wrote.
pub(crate) fn decode(s: &str) -> Option<Vec<u8>> {
    let s = s.as_bytes();
    if s.is_empty() || !s.len().is_multiple_of(4) {
        return None;
    }
    let pad = s.iter().rev().take_while(|&&c| c == b'=').count();
    if pad > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (quads, _) = s.as_chunks::<4>();
    for (i, quad) in quads.iter().enumerate() {
        let last = i + 1 == s.len() / 4;
        let mut n: u32 = 0;
        let mut have = 0;
        let mut padded = false;
        for &c in quad {
            // Padding is only ever the tail of the last quad.
            if padded && c != b'=' {
                return None;
            }
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' if last => {
                    padded = true;
                    n <<= 6;
                    continue;
                }
                _ => return None,
            };
            n = n << 6 | u32::from(v);
            have += 1;
        }
        let triple = n.to_be_bytes();
        out.extend_from_slice(&triple[1..1 + (have * 6 / 8)]);
    }
    Some(out)
}

fn body(bytes: impl Into<Bytes>) -> BoxBody {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed_unsync()
}

/// Answer one of this module's routes. A query string is a route that is not there, as for `log`.
pub async fn serve(ctx: &Ctx, found: Route<'_>, query: &str) -> Response<BoxBody> {
    if !query.is_empty() {
        return status(StatusCode::NOT_FOUND);
    }
    match found {
        Route::List(id) => match read(ctx, id).await {
            Ok(done) => json(StatusCode::OK, &done.images.iter().map(listed).collect::<Vec<_>>()),
            Err(refusal) => refusal,
        },
        Route::Transcript(id) => match read(ctx, id).await {
            Ok(done) if done.lean.len() > LEAN_CAP => problem(StatusCode::PAYLOAD_TOO_LARGE, "that chat's log is too large to show here"),
            Ok(done) => Response::builder()
                .status(StatusCode::OK)
                .header("content-type", HeaderValue::from_static("application/x-ndjson"))
                // A chat made by fork-from-here shows its own branch, not the whole copy (pending.rs).
                .body(body(crate::pending::shown(ctx, id, &done.lean).unwrap_or_else(|| done.lean.clone())))
                .unwrap(),
            Err(refusal) => refusal,
        },
        Route::Bytes(id, record, n) => {
            let (Some(record), Some(n)) = (digits(record), digits(n)) else { return status(StatusCode::NOT_FOUND) };
            if !id_ok(id) {
                return status(StatusCode::NOT_FOUND);
            }
            let done = match read(ctx, id).await {
                Ok(done) => done,
                Err(refusal) => return refusal,
            };
            let Some(image) = done.images.iter().find(|i| i.record == record && i.n as u64 == n) else {
                return status(StatusCode::NOT_FOUND);
            };
            let Some(kind) = TYPES.iter().find(|t| **t == image.media_type) else {
                return problem(StatusCode::UNSUPPORTED_MEDIA_TYPE, "that block is not a png, jpeg, gif or webp image");
            };
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", HeaderValue::from_static(kind))
                .header("content-length", image.bytes.len())
                .header("x-content-type-options", HeaderValue::from_static("nosniff"))
                .header("cache-control", HeaderValue::from_static("private, max-age=3600"))
                .header("content-security-policy", HeaderValue::from_static("default-src 'none'; sandbox"))
                .body(body(image.bytes.clone()))
                .unwrap()
        }
    }
}

/// An unsigned integer spelled only with digits: `+3`, `-0`, ` 3` and `3.0` are not records.
fn digits(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 19 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn listed(i: &Image) -> Value {
    let mut row = serde_json::json!({ "record": i.record, "n": i.n, "media_type": i.media_type, "where": i.place, "bytes": i.bytes.len() });
    if let Some(call) = &i.tool_call {
        row["tool_call"] = Value::String(call.clone());
    }
    if let Some(alt) = &i.alt {
        row["alt"] = Value::String(alt.clone());
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOT: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn line(id: u64, kind: Value) -> String {
        format!("{}\n", serde_json::json!({ "id": id, "parent": id.saturating_sub(1), "ts_ms": 1, "kind": kind }))
    }

    fn block(media: &str, data: &str, alt: Option<&str>) -> Value {
        let mut b = serde_json::json!({ "type": "image", "source": { "type": "base64", "media_type": media, "data": data } });
        if let Some(alt) = alt {
            b["alt"] = alt.into();
        }
        b
    }

    #[test]
    fn routes_are_exactly_three() {
        assert_eq!(route("/hub/sessions/a1/images"), Some(Route::List("a1")));
        assert_eq!(route("/hub/sessions/a1/transcript"), Some(Route::Transcript("a1")));
        assert_eq!(route("/hub/sessions/a1/image/3/0"), Some(Route::Bytes("a1", "3", "0")));
        for no in ["/hub/sessions/a1/image/3", "/hub/sessions/a1/image/3/0/x", "/hub/sessions/a1/images/x", "/hub/sessions/a1/log", "/hub/sessions/a1"] {
            assert_eq!(route(no), None, "{no}");
        }
    }

    #[test]
    fn base64_is_strict_and_right() {
        assert_eq!(decode("QUJD").unwrap(), b"ABC");
        assert_eq!(decode("QUI=").unwrap(), b"AB");
        assert_eq!(decode("QQ==").unwrap(), b"A");
        assert_eq!(&decode(DOT).unwrap()[..4], b"\x89PNG");
        for bad in ["", "QUJ", "QU=D", "Q===", "QUJD\n", "QU Jd", "QUJ*"] {
            assert!(decode(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn digits_are_digits() {
        assert_eq!(digits("12"), Some(12));
        for bad in ["", "+1", "-1", "1.0", " 1", "1 ", "0x1", "99999999999999999999"] {
            assert_eq!(digits(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn scan_lifts_user_images_and_empties_their_data() {
        let log = line(0, serde_json::json!({ "SessionStart": { "model": "mock", "cwd": "/", "system": null } }))
            + &line(1, serde_json::json!({ "UserMessage": { "role": "user", "content": [
                block("image/png", DOT, Some("dot.png")), block("image/jpeg", "QUJD", None), { "type": "text", "text": "hi" }] } }));
        let (images, lean) = scan(log.as_bytes());
        assert_eq!(images.len(), 2);
        assert_eq!((images[0].record, images[0].n, images[0].place, images[0].alt.as_deref()), (1, 0, "user", Some("dot.png")));
        assert_eq!((images[1].record, images[1].n, images[1].media_type.as_str(), images[1].bytes.as_slice()), (1, 1, "image/jpeg", &b"ABC"[..]));
        let text = String::from_utf8(lean).unwrap();
        assert!(!text.contains(DOT) && !text.contains("QUJD"), "the bytes are gone from the transcript: {text}");
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("\"alt\":\"dot.png\"") && text.contains("\"text\":\"hi\""), "everything else stays: {text}");
    }

    #[test]
    fn scan_places_tool_and_assistant_images() {
        let log = line(5, serde_json::json!({ "UserMessage": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "call_9", "content": "shot" }, block("image/png", DOT, Some("shot.png"))] } }))
            + &line(6, serde_json::json!({ "AssistantMessage": { "role": "assistant", "content": [block("image/webp", "QUJD", None)] } }));
        let (images, _) = scan(log.as_bytes());
        assert_eq!((images[0].place, images[0].tool_call.as_deref()), ("tool", Some("call_9")));
        assert_eq!((images[1].place, images[1].tool_call.as_deref(), images[1].record), ("assistant", None, 6));
    }

    #[test]
    fn scan_keeps_a_url_image_and_a_broken_one_out_of_the_list_but_in_the_numbering() {
        let url = serde_json::json!({ "type": "image", "source": { "type": "url", "url": "https://x/a.png" } });
        let log = line(1, serde_json::json!({ "UserMessage": { "role": "user", "content": [url, block("image/png", "!!!!", None), block("image/png", DOT, None)] } }));
        let (images, lean) = scan(log.as_bytes());
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].n, 2, "its position in the message, not in the list");
        assert!(String::from_utf8(lean).unwrap().contains("https://x/a.png"));
    }

    #[test]
    fn a_line_without_images_is_copied_as_it_came() {
        let raw = "{\"id\":3,\"parent\":2,\"ts_ms\":1,\"kind\":{\"UserMessage\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"the word image\"}]}}}\n";
        let (images, lean) = scan(raw.as_bytes());
        assert!(images.is_empty());
        assert_eq!(lean, raw.as_bytes());
    }
}
