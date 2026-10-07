//! `minerva status --json`: what this install knows about itself, for the perch's start screen. Read-only and network-free: it
//! reads four small files under `<prefix>/state/minerva` and the jev marker, and never asks the network, never updates, never
//! rewrites the cache the daily notice keeps. Every file is the member's own; a missing or unreadable one is an absent answer
//! (`null`, `[]`, `false`), never an error, so the page can always draw something.
//!
//! ```text
//! { "version": "0.1.0-gabc1234",
//!   "notice":  { "line": "Minerva 0.2.0 available -- run: hoot update" | null, "latest": "0.2.0" | null,
//!                "stale": "release info is stale ..." | null, "checked": 1791269513 } | null,
//!   "plugins": [ { "name": "browser", "version": "0.3.0", "how": "stock" } ],
//!   "jev":     { "set_up": true, "running": true, "pid": 4242 } }          (pid only while it runs)
//! ```

use std::fs;
use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::install::Layout;
use crate::{jev, plugins, version};

/// The most of the update-check cache that is read. The daily notice writes a few dozen bytes.
const CACHE_CAP: u64 = 64 * 1024;
/// The most of the plugin record that is read (a few hundred bytes per plugin, a handful of plugins).
const RECORD_CAP: u64 = 256 * 1024;

pub struct Opts<'a> {
    pub layout: &'a Layout,
    /// The version installed here (what `current` points at, else this program's own).
    pub version: &'a str,
    /// The command a person is told to run for an update: `hoot` (under WSL) or `minerva`.
    pub who: &'a str,
}

/// The same fields `notice.rs` writes (its own type is private to it). Unknown fields are ignored, as the writer's reader does.
#[derive(Deserialize)]
struct Cached {
    checked: u64,
    #[serde(default)]
    latest: Option<String>,
    #[serde(default)]
    stale: Option<String>,
}

pub fn report(o: &Opts) -> Value {
    json!({
        "version": o.version,
        "notice": notice_of(o),
        "plugins": plugins_of(o.layout),
        "jev": jev_of(o.layout),
    })
}

/// A file as text, at most `cap` bytes of it: a file past the cap is not a file this program wrote, so it reads as absent. So is
/// anything that is not a regular file (a FIFO would block the open until someone wrote to it), checked before the open.
pub fn read_capped(path: &Path, cap: u64) -> Option<String> {
    if !fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    fs::File::open(path).ok()?.take(cap + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= cap).then_some(text)
}

/// The cached once-a-day answer, worded the way `notice::check` words it. `null` when there is no cache or it does not parse.
fn notice_of(o: &Opts) -> Value {
    let Some(c) = read_capped(&o.layout.state_dir().join("update-check"), CACHE_CAP).and_then(|t| serde_json::from_str::<Cached>(&t).ok()) else {
        return Value::Null;
    };
    let newer = c.latest.as_deref().is_some_and(|l| version::is_newer(l, o.version).unwrap_or(false));
    let mut line = match (&c.latest, newer) {
        (Some(l), true) => format!("Minerva {l} available -- run: {} update", o.who),
        _ => String::new(),
    };
    if let Some(w) = &c.stale {
        if !line.is_empty() {
            line.push_str("; ");
        }
        line.push_str(w);
    }
    json!({
        "line": (!line.is_empty()).then_some(line),
        "latest": c.latest,
        "stale": c.stale,
        "checked": c.checked,
    })
}

/// What `minerva plugins sync` recorded as installed by Minerva (sorted by name; none when there is no record). Trust and service
/// state are not in the record and not here.
fn plugins_of(l: &Layout) -> Value {
    // The record is read here, capped, rather than through `plugins::load_state` (which reads whatever file is there, however large
    // or whatever kind); it is parsed as that module's own type.
    let st: plugins::State = read_capped(&l.state_dir().join("plugins.json"), RECORD_CAP).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    Value::Array(st.plugins.iter().map(|(name, p)| json!({ "name": name, "version": p.version, "how": p.how })).collect())
}

/// Set up (the venv marker) and running (the pid file names a live process whose command line is the service's), like
/// `jev::status` words it; that function returns a sentence, so the two answers are read here and a test holds them together.
fn jev_of(l: &Layout) -> Value {
    let set_up = jev::is_set_up(l);
    let pid = running_pid(l);
    let mut v = json!({ "set_up": set_up, "running": set_up && pid.is_some() });
    if let (true, Some(pid)) = (set_up, pid) {
        v["pid"] = json!(pid);
    }
    v
}

/// The same check as `jev::pid_alive`: `<state>/jev.pid` holds a pid whose `/proc/<pid>/cmdline` mentions `server.py`.
fn running_pid(l: &Layout) -> Option<u32> {
    let pid: u32 = read_capped(&l.state_dir().join("jev.pid"), 32)?.trim().parse().ok()?;
    let cmd = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    String::from_utf8_lossy(&cmd).contains("server.py").then_some(pid)
}

#[cfg(test)]
mod tests;
