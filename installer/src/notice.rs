//! The once-a-day "a newer Minerva exists" notice. Notice only: nothing is ever downloaded for install or installed
//! from here. Silent on any failure. Cached in `<prefix>/state/minerva/update-check` (`~/.local/state/...`).

use std::fs;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::fetch::Source;
use crate::install::Layout;
use crate::keys::Trust;
use crate::update::fetch_verified;
use crate::version::is_newer;

pub const EVERY_SECS: u64 = 24 * 3600;
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    /// Unix seconds of the last attempt, successful or not.
    checked: u64,
    /// The newest version seen with a valid signature.
    latest: Option<String>,
    /// The "release info is stale" warning of the last verified manifest, if it had one.
    #[serde(default)]
    stale: Option<String>,
}

/// `Some("Minerva 0.2.0 available -- run: hoot update")` when a verified newer release is known. `cmd` is the name to
/// tell the user to run (`hoot` or `minerva`). `now` is Unix seconds (a parameter so tests need no clock).
pub fn check(layout: &Layout, base: &str, trust: &Trust, current: &str, cmd: &str, now: u64) -> Option<String> {
    let file = layout.state_dir().join("update-check");
    let mut cache: Cache = fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if now.saturating_sub(cache.checked) >= EVERY_SECS || cache.checked > now {
        cache.checked = now;
        if let Ok((v, stale)) = fetch_latest(base, trust, now) {
            cache.latest = Some(v);
            cache.stale = stale;
        }
        // Written on failure too: an offline machine is asked once a day, not on every command.
        if fs::create_dir_all(layout.state_dir()).is_ok()
            && let Ok(t) = serde_json::to_string(&cache)
        {
            let _ = fs::write(&file, t);
        }
    }
    let latest = cache.latest?;
    let newer = is_newer(&latest, current).ok()?;
    let mut line = if newer { format!("Minerva {latest} available -- run: {cmd} update") } else { String::new() };
    if let Some(w) = cache.stale {
        if !line.is_empty() {
            line.push_str("; ");
        }
        line.push_str(&w);
    }
    (!line.is_empty()).then_some(line)
}

/// Judged exactly like `update --check`: the release's `keys.json` is checked and applied IN MEMORY (never written), then
/// the manifest is verified with the resulting keys. A release signed by a key only its own rotation introduces is
/// therefore noticed, and a rotation that fails the rules gives no notice at all.
fn fetch_latest(base: &str, trust: &Trust, now: u64) -> Result<(String, Option<String>)> {
    let src = Source::new(base);
    let v = fetch_verified(&src, trust, now, false, Some(TIMEOUT))?;
    Ok((v.manifest.version, v.stale))
}

#[cfg(test)]
mod tests;
