//! `FAKE_ASKS`, `FAKE_LAG`, and the door's rules for an answer: the door's open asks, for the watcher and the allow rules (slice A1a).
//!
//! `fake_eidolon.rs` only calls the three hooks below, at the places named on each; the state they share is the `static` in this file, and
//! `Fake`'s fields are reachable from here (`fake.frames` is the broadcast every open `/api/events` stream relays).
//!
//! - `FAKE_ASKS=<file>`: after a `say` is accepted, the file is read and its frames are emitted. A line is one frame, written to every open
//!   stream as `data: <line>\n\n` byte for byte (so a test can make a line 100 KiB long, or not JSON at all). Two kinds of line are not
//!   frames: `#sleep <ms>` pauses the emission, and any other line starting with `#` is a comment.
//!
//!   ```text
//!   {"type":"tool-call-started","id":"c1","name":"bash","input":{"command":"ls"},"origin":"model"}
//!   {"type":"ask","kind":"approval","ask_id":1,"call_id":"c1","tool":"bash","input":{"command":"ls"},"prompt":"bash. Run it?",
//!    "reason":"outside-cwd","structural":false,"judged":null,"yolo":false,"answers":["yes","no"]}
//!   #sleep 200
//!   {"type":"turn-state","running":true}
//!   ```
//!
//!   Ask ids are unique per process, as the real door's are (they only go up): the first `say` emits the file as written, and each later
//!   `say` has the `ask_id` of its `ask` and `ask-settled` frames raised by the highest id emitted before it, so a script that is read
//!   again opens new asks and an id that was settled is never open again. (A line that is not JSON, and any other kind of frame, is
//!   emitted as written; `says` that overlap in time are not offset against each other.)
//!
//!   Like the real door, the fake remembers what it has emitted: an `ask` stays open until it is answered (or a scripted `ask-settled`
//!   names it), and a stream that opens later, a reconnect or a pane, is given a replay after its `hello`: the `tool-call-started` frames
//!   whose origin is `model` (a branch replay knows no other) and the turn markers `user-message`, `turn-settled` and `cancelled`, all in
//!   the order they were emitted, then the asks still open, then `caught-up`. Nothing is replayed before anything was emitted, so a door
//!   that was never scripted behaves exactly as it did.
//! - `FAKE_LAG=<n>`: after each of the first `n` says' frames, a `lagged` frame goes to every stream. The real door sends one to a
//!   subscriber that fell behind and then ends the stream; this one leaves the connection to the reader, who is expected to drop it
//!   on seeing `lagged` (the watcher does) and come back, so what a lag exercises is the reconnect and the replay above. It does not
//!   model loss: no frame is dropped (the real door's lag means frames were missed, and the replay is how a client finds out what), so a
//!   reader that only works because nothing was missed is not tested by it.
//! - `POST /api/answer`, as the real door answers it: `204` and an `ask-settled` frame when the ask is open and the answer is one of
//!   those it offered (an approval's `answers`, or the `label` of one of a question's `options`; an ask that offers neither takes any);
//!   `409` for an ask that was never opened or is already settled (with `FAKE_ASKS` set, an ask the script never opened is a `409`; without
//!   it any id is accepted once, as before); `422` for an answer that was not offered, and the ask stays open. Every answer is written to
//!   `$FAKE_LOG` as `answer ask_id=<n> answer=<a> note=<"text" or ->` (a refusal as `answer-refused ask_id=<n> status=<409|422>`), which is
//!   the answer log the allow tests read.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::Value;

use super::Fake;

/// What a new `/api/events` stream is given after `hello`, before it settles into relaying the broadcast.
#[derive(Default)]
pub struct Opening {
    /// Raw SSE bytes (`data: {...}\n\n`, any number of frames) written at once: what the real door replays to a client that connects late.
    pub replay: Vec<u8>,
    /// End the stream after `replay`. Nothing here asks for it (a lag is a frame the reader acts on, see the module doc).
    pub end: bool,
}

/// An ask that is open: its frame as written, and what it offers to be answered with.
struct Pending {
    frame: Vec<u8>,
    /// The answers it offers, when it says: an approval's `answers`, or the labels of a question's `options`. `None` takes any answer.
    offered: Option<Vec<String>>,
}

#[derive(Default)]
struct State {
    open: BTreeMap<u64, Pending>,
    /// The frames a replay carries before the open asks, as written, in the order they were emitted: model `tool-call-started` frames and
    /// the turn markers.
    history: Vec<Vec<u8>>,
    /// Asks that have been answered, or settled by the script.
    settled: BTreeSet<u64>,
    /// How many says have been accepted.
    says: u32,
    /// The highest ask id emitted so far, the offsets included.
    top: u64,
}

static STATE: Mutex<State> = Mutex::new(State { open: BTreeMap::new(), history: Vec::new(), settled: BTreeSet::new(), says: 0, top: 0 });

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One frame as the stream carries it.
fn sse(payload: &[u8]) -> Vec<u8> {
    let mut frame = b"data: ".to_vec();
    frame.extend_from_slice(payload);
    frame.extend_from_slice(b"\n\n");
    frame
}

/// The line with its `ask_id` raised by `base`, when it is an `ask` or an `ask-settled` frame and `base` is not nothing; any other line is
/// returned as it is, byte for byte.
fn offset(line: &[u8], base: u64) -> Cow<'_, [u8]> {
    if base == 0 {
        return Cow::Borrowed(line);
    }
    let Ok(mut value) = serde_json::from_slice::<Value>(line) else { return Cow::Borrowed(line) };
    let named = matches!(value.get("type").and_then(Value::as_str), Some("ask" | "ask-settled"));
    let id = value.get("ask_id").and_then(Value::as_u64);
    match (named, id) {
        (true, Some(id)) => {
            value["ask_id"] = Value::from(id + base);
            Cow::Owned(value.to_string().into_bytes())
        }
        _ => Cow::Borrowed(line),
    }
}

/// `POST /api/say` was accepted and answered 202. Emit what `FAKE_ASKS` names onto `fake.frames`; spawn rather than block, because this
/// runs on the connection that carried the `say`.
pub fn after_say(fake: &Arc<Fake>) {
    let path = std::env::var_os("FAKE_ASKS");
    let lag: u32 = std::env::var("FAKE_LAG").ok().and_then(|n| n.parse().ok()).unwrap_or(0);
    if path.is_none() && lag == 0 {
        return;
    }
    let (nth, base) = {
        let mut state = state();
        state.says += 1;
        (state.says, state.top)
    };
    let fake = fake.clone();
    tokio::spawn(async move {
        let script = match &path {
            None => Vec::new(),
            Some(path) => match std::fs::read(path) {
                Ok(script) => script,
                Err(e) => {
                    fake.record(&format!("asks-file-unreadable {e}"));
                    return;
                }
            },
        };
        for line in script.split(|&byte| byte == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                continue;
            }
            if let Some(directive) = line.strip_prefix(b"#") {
                let text = std::str::from_utf8(directive).unwrap_or_default();
                if let Some(ms) = text.strip_prefix("sleep ").and_then(|ms| ms.trim().parse::<u64>().ok()) {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                }
                continue;
            }
            let line = offset(line, base);
            let frame = sse(&line);
            note(&line, &frame);
            let _ = fake.frames.send(frame);
            // The broadcast keeps 64 frames per stream; a script longer than that must not outrun the readers.
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        if nth <= lag {
            let _ = fake.frames.send(sse(br#"{"type":"lagged","dropped":3}"#));
        }
    });
}

/// What an `ask` frame offers: its `answers` when they are strings, else the `label` of each of its `options`; `None` when it names
/// none (a question with no options is free text).
fn offered(frame: &Value) -> Option<Vec<String>> {
    fn texts(items: &[Value], key: Option<&str>) -> Vec<String> {
        let text = |item: &Value| match key {
            None => item.as_str().map(str::to_string),
            Some(key) => item.get(key).and_then(Value::as_str).map(str::to_string),
        };
        items.iter().filter_map(text).collect()
    }
    let named = match (frame.get("answers").and_then(Value::as_array), frame.get("options").and_then(Value::as_array)) {
        (Some(answers), _) => texts(answers, None),
        (None, Some(options)) => texts(options, Some("label")),
        (None, None) => Vec::new(),
    };
    (!named.is_empty()).then_some(named)
}

/// What a frame just emitted does to what the fake remembers.
fn note(line: &[u8], frame: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(line) else { return };
    let mut state = state();
    match value.get("type").and_then(Value::as_str) {
        Some("tool-call-started") if value.get("origin").and_then(Value::as_str) == Some("model") => state.history.push(frame.to_vec()),
        Some("user-message" | "turn-settled" | "cancelled") => state.history.push(frame.to_vec()),
        Some("ask") => {
            if let Some(id) = value.get("ask_id").and_then(Value::as_u64) {
                state.top = state.top.max(id);
                state.open.insert(id, Pending { frame: frame.to_vec(), offered: offered(&value) });
            }
        }
        Some("ask-settled") => {
            if let Some(id) = value.get("ask_id").and_then(Value::as_u64) {
                state.top = state.top.max(id);
                state.open.remove(&id);
                state.settled.insert(id);
            }
        }
        _ => {}
    }
}

/// `POST /api/answer {ask_id, answer, note?}` passed the token, `Host` and shape checks; `body` is the parsed JSON. `Some((status line,
/// body))` is sent instead of the usual `204` and the `ask-settled` frame (a second answer: `("409 Conflict", ...)`, an answer the ask did
/// not offer: `("422 Unprocessable Entity", ...)`, and the ask stays open); `None` carries on. The answer log the allow tests read is
/// written here too.
pub fn on_answer(fake: &Fake, body: &Value) -> Option<(&'static str, &'static str)> {
    let scripted = std::env::var_os("FAKE_ASKS").is_some();
    let ask_id = body.get("ask_id").and_then(Value::as_u64);
    let answer = body.get("answer").and_then(Value::as_str);
    let refused = {
        let mut state = state();
        match ask_id {
            Some(id) if state.settled.contains(&id) || (scripted && !state.open.contains_key(&id)) => Some(409),
            Some(id) => {
                let offers = state.open.get(&id).and_then(|pending| pending.offered.as_ref());
                if offers.is_some_and(|offers| !answer.is_some_and(|answer| offers.iter().any(|offer| offer == answer))) {
                    Some(422)
                } else {
                    state.settled.insert(id);
                    state.open.remove(&id);
                    None
                }
            }
            // Not an unsigned integer: whatever the fake did before it had asks, which is to accept it.
            None => None,
        }
    };
    let id = ask_id.map_or_else(|| "-".to_string(), |id| id.to_string());
    if let Some(status) = refused {
        fake.record(&format!("answer-refused ask_id={id} status={status}"));
        return Some(match status {
            409 => ("409 Conflict", "{\"error\":\"no such ask, or it was already settled\"}"),
            _ => ("422 Unprocessable Entity", "{\"error\":\"not one of the answers the ask offered\"}"),
        });
    }
    let answer = answer.unwrap_or("-");
    let note = body.get("note").and_then(Value::as_str).map_or_else(|| "-".to_string(), |note| format!("{note:?}"));
    fake.record(&format!("answer ask_id={id} answer={answer} note={note}"));
    None
}

/// The `nth` `/api/events` stream (0 is the perch's watcher) has just been sent `hello`. Once anything has been emitted, a stream is given
/// what a real door gives a late client: the model's calls and the turn markers in order, the asks still open, `caught-up`.
pub fn on_stream(_fake: &Fake, _nth: usize) -> Opening {
    let state = state();
    if state.history.is_empty() && state.open.is_empty() {
        return Opening::default();
    }
    let mut replay = Vec::new();
    for frame in state.history.iter().chain(state.open.values().map(|pending| &pending.frame)) {
        replay.extend_from_slice(frame);
    }
    replay.extend_from_slice(&sse(br#"{"type":"caught-up"}"#));
    Opening { replay, end: false }
}
