//! The models eidolon can run, for the composer's `model` menu: `eidolon [--config C] models`, parsed, cached.
//!
//! The listing is eidolon's own words and the perch decodes nothing else: a row is `<key>  <name>  [<weight>]  <note>`,
//! where `<key>` is `provider:model`. The provider table and the notices printed above the rows have no such first word
//! and are skipped, the same rule `setup/src/lib.rs` (`parse_models`) uses for the wizard.
//!
//! **Cached ten minutes**, one answer for the whole process: the command reads eidolon's catalog cache, but it still starts a
//! process and a menu that is opened ten times should not. The cache lock is held across the run, so two menus opened at once
//! cost one command. A failure is never cached (the next open asks again), and the run is bounded in time and size by
//! [`titles::run`], which kills what is still running when it returns.

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::titles::{self, RunError};

/// How long an answer is trusted.
pub const TTL: Duration = Duration::from_secs(600);
/// `eidolon models` prints in tens of milliseconds from its cache; a wedged one is not waited on longer than this.
const WITHIN: Duration = Duration::from_secs(20);
/// The test knob: `PERCH_MODELS_TIMEOUT_MS` shortens [`WITHIN`] so a hung command can be proven without a 20 s test.
const KNOB: &str = "PERCH_MODELS_TIMEOUT_MS";
/// Thirty-odd short lines; a megabyte is a command that is not `models`.
const CAP: usize = 1024 * 1024;

/// One row of the menu.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Model {
    /// What `-m` takes.
    pub key: String,
    /// Its display name, when eidolon prints one that differs from the key.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The last column: provider, context, price.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

static CACHE: tokio::sync::Mutex<Option<(Instant, Vec<Model>)>> = tokio::sync::Mutex::const_new(None);

/// The listing, from the cache when it is under [`TTL`] old, else from one run of the command.
pub async fn list(eidolon: &Path, config: Option<&Path>) -> Result<Vec<Model>, RunError> {
    let mut cache = CACHE.lock().await;
    if let Some((at, models)) = cache.as_ref()
        && at.elapsed() < TTL
    {
        return Ok(models.clone());
    }
    let within = std::env::var(KNOB).ok().and_then(|ms| ms.parse().ok()).map(Duration::from_millis).unwrap_or(WITHIN);
    let args = [OsString::from("models")];
    let out = titles::run(eidolon, config, &args, CAP, within).await?;
    let models = parse(&String::from_utf8_lossy(&out));
    *cache = Some((Instant::now(), models.clone()));
    Ok(models)
}

/// Whether `key` is something `-m` may be handed: nothing that could read as a flag, no whitespace or control character, and
/// a bounded length. The perch passes it as one argv element and never through a shell; this keeps it one *value*.
pub fn key_ok(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 200
        && !key.starts_with('-')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || ":._-/@+".contains(c))
}

/// The rows of `eidolon models`: a first word that holds a `:` (`provider:model`) with more text after it. Duplicates are
/// dropped, order is kept (eidolon sorts heaviest first, which is how the menu should read).
pub fn parse(text: &str) -> Vec<Model> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(key), Some(_)) = (words.next(), words.next()) else { continue };
        let ok = key.contains(':') && !key.starts_with(':') && !key.ends_with(':') && !key.contains("//") && key_ok(key);
        if !ok || !seen.insert(key.to_string()) {
            continue;
        }
        // Columns are separated by two or more spaces (the key column is padded to 40); a single space is inside a name.
        let rest = line.trim_start()[key.len()..].trim();
        let cols: Vec<&str> = rest.split("  ").map(str::trim).filter(|col| !col.is_empty()).collect();
        let name = cols.first().copied().unwrap_or_default();
        let note = if cols.len() > 1 { cols[cols.len() - 1] } else { "" };
        let weighted = name.starts_with(['+', '-']) && name[1..].chars().all(|c| c.is_ascii_digit());
        out.push(Model { key: key.to_string(), name: if weighted || name == key { String::new() } else { name.to_string() }, note: note.to_string() });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rows_are_the_keys_with_a_name_and_a_note_and_nothing_above_them() {
        let text = "deepseek\tAnthropic\thttps://api.deepseek.com/anthropic\t(built-in)\n\
                    nothing cached for deepseek -- declared floor only\n\
                    \n\
                    deepseek:deepseek-flash                  DeepSeek Flash                                deepseek · 1000k ctx · $0.3/1.2 per M\n\
                    zai:glm-5.3                              GLM 5.3                                   +2  zai · 1048k ctx\n\
                    zai:glm-5.3                              GLM 5.3                                       zai · 1048k ctx\n\
                    ollama:qwen3.5:397b                      ollama:qwen3.5:397b                           ollama · 262k ctx\n\
                    https://x.y/z  a b\n";
        let rows = parse(text);
        let keys: Vec<&str> = rows.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, ["deepseek:deepseek-flash", "zai:glm-5.3", "ollama:qwen3.5:397b"]);
        assert_eq!(rows[0].name, "DeepSeek Flash");
        assert_eq!(rows[0].note, "deepseek · 1000k ctx · $0.3/1.2 per M");
        assert_eq!(rows[1].name, "GLM 5.3", "the weight column is not the name");
        assert_eq!(rows[2].name, "", "a name that is the key is not repeated");
    }

    #[test]
    fn a_key_is_one_plain_value_and_never_a_flag() {
        for good in ["mock", "chatgpt:gpt-5.6-luna", "ollama:qwen3.5:397b", "a/b@c+d"] {
            assert!(key_ok(good), "{good}");
        }
        for bad in ["", "-m", "--yolo", "a b", "a\nb", "a;b", "$(x)", &"x".repeat(201)] {
            assert!(!key_ok(bad), "{bad:?}");
        }
    }
}
