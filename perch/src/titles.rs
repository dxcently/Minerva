//! Chat titles, and the two things the perch asks eidolon for them: `eidolon sessions` (a
//! one-line-per-log listing whose last column is the first message) and `eidolon log --json`
//! (one JSON line per record, for a reader that cannot decode the journal).
//!
//! **The perch never decodes the `EIDO` log.** A title is whatever eidolon says it is: the roster's
//! `meta.json` title while a door is live (read in `sessions.rs`), and the fourth column of
//! `eidolon sessions` for a saved one. This file runs that command, parses its lines, and caches the
//! answer.
//!
//! **The cache** is keyed by the log's path. A log that has a title is never asked about again (the
//! first message of a chat does not change); one without a title is asked again only when its mtime has
//! moved, because that is the one thing that can give it one. [`Cache::stale`] says whether any log of a
//! directory needs the command, so a list that changes nothing runs nothing, and a list that changes
//! something runs it once for the whole directory (it prints ~200 logs in ~35 ms).
//!
//! **The fallback** for a chat with no user message yet is `<project> · HH:MM`: the folder name
//! when the perch knows the cwd (a door it holds, a roster row, or one it started and remembers), the birth
//! name (`quiet-owl-lantern`, no time) for an inbox chat, and `chat` when it has no way to know (a log from an earlier perch run: the cwd is
//! inside the journal, which is exactly what is not decoded here). The time is the log's mtime, local.
//!
//! Every child is spawned from an argv array (no shell), with a time bound and a cap on what it may print,
//! and nothing secret is ever put in its argv or environment: the perch has nothing to pass.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;

use crate::spawn::name_config;

/// `eidolon sessions` for ~200 logs is ~35 ms; this is for a wedged or a very large directory.
pub const SESSIONS_TIMEOUT: Duration = Duration::from_secs(10);
/// The listing is one short line per log.
pub const SESSIONS_CAP: usize = 4 * 1024 * 1024;
/// `eidolon log --json` of one chat. A chat that does not fit is a 413, not a truncated transcript.
pub const LOG_TIMEOUT: Duration = Duration::from_secs(15);
pub const LOG_CAP: usize = 16 * 1024 * 1024;
/// After a failed `sessions`, how long before the same directory is asked again.
const RETRY: Duration = Duration::from_secs(30);

/// Why a run of eidolon gave no output.
#[derive(Debug)]
pub enum RunError {
    /// It printed more than the cap.
    TooBig,
    /// It did not finish in time (and was killed).
    Timeout,
    /// It could not be started, or exited non-zero.
    Failed(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooBig => write!(f, "the output is over the size limit"),
            Self::Timeout => write!(f, "eidolon did not answer in time"),
            Self::Failed(why) => write!(f, "{why}"),
        }
    }
}

/// The command for one read: `eidolon --config <config> <args...>`, with that config named in its environment too (`EIDOLON_CONFIG`, SA2),
/// and removed from it when there is none.
fn read_command(eidolon: &Path, config: Option<&Path>, args: &[OsString]) -> Command {
    let mut command = Command::new(eidolon);
    name_config(command.as_std_mut(), config);
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    command.args(args);
    command
}

/// `<eidolon> [--config <config>] <args...>`, run to the end with its stdout capped at `cap` bytes
/// and the whole thing bounded by `within`. stderr is not read (it goes nowhere), and a child still running
/// when this returns is killed.
pub async fn run(eidolon: &Path, config: Option<&Path>, args: &[OsString], cap: usize, within: Duration) -> std::result::Result<Vec<u8>, RunError> {
    let mut command = read_command(eidolon, config, args);
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = command.spawn().map_err(|e| RunError::Failed(format!("starting {}: {e}", eidolon.display())))?;
    let mut stdout = child.stdout.take().ok_or_else(|| RunError::Failed("no stdout".into()))?;
    let work = async {
        let mut out = Vec::new();
        // One byte past the cap is how "over" is told from "exactly".
        (&mut stdout).take(cap as u64 + 1).read_to_end(&mut out).await.map_err(|e| RunError::Failed(format!("reading eidolon: {e}")))?;
        if out.len() > cap {
            return Err(RunError::TooBig);
        }
        let status = child.wait().await.map_err(|e| RunError::Failed(format!("waiting for eidolon: {e}")))?;
        if !status.success() {
            return Err(RunError::Failed(format!("eidolon exited with {status}")));
        }
        Ok(out)
    };
    // On a timeout or a cap the future is dropped, and `kill_on_drop` ends the child.
    tokio::time::timeout(within, work).await.map_err(|_| RunError::Timeout)?
}

/// `eidolon sessions`' lines: `<path>\t<model>\t<records>\t<first message, 60 chars>`; an unreadable log
/// is `<path>\t(unreadable: ...)`. The result is `stem -> first message`, empty ones left out.
pub fn parse_sessions(out: &[u8]) -> HashMap<String, String> {
    let mut found = HashMap::new();
    for line in String::from_utf8_lossy(out).lines() {
        let mut cols = line.split('\t');
        let (Some(path), Some(_model), Some(_records), Some(first)) = (cols.next(), cols.next(), cols.next(), cols.next()) else { continue };
        let Some(stem) = Path::new(path).file_stem().and_then(|s| s.to_str()) else { continue };
        // A tab inside the message is more columns, not a new field: eidolon only flattens newlines.
        let first = std::iter::once(first).chain(cols).flat_map(str::split_whitespace).collect::<Vec<_>>().join(" ");
        if !first.is_empty() {
            found.insert(stem.to_string(), first);
        }
    }
    found
}

/// `eidolon sessions`' second column: `stem -> model`, for the rows that have one.
pub fn parse_models(out: &[u8]) -> HashMap<String, String> {
    let mut found = HashMap::new();
    for line in String::from_utf8_lossy(out).lines() {
        let mut cols = line.split('\t');
        let (Some(path), Some(model), Some(_records)) = (cols.next(), cols.next(), cols.next()) else { continue };
        let Some(stem) = Path::new(path).file_stem().and_then(|s| s.to_str()) else { continue };
        let model = model.trim();
        if !model.is_empty() && !model.starts_with('(') {
            found.insert(stem.to_string(), model.to_string());
        }
    }
    found
}

struct Seen {
    mtime: Option<u64>,
    title: Option<String>,
}

/// What the perch has learned about titles. Behind a `Mutex` in the registry, never held across an await.
#[derive(Default)]
pub struct Cache {
    seen: HashMap<PathBuf, Seen>,
    /// A directory whose `sessions` failed, and when: not asked again for [`RETRY`].
    failed: HashMap<PathBuf, Instant>,
    /// The cwd a door of this perch's was started in, by id: what names a chat's folder after the door is gone.
    cwds: HashMap<String, PathBuf>,
    /// The model `eidolon sessions` named for a chat, by id: what a fork of a saved chat starts with when nothing fresher is known.
    models: HashMap<String, String>,
}

impl Cache {
    /// Whether any of `logs` (path, mtime) needs `eidolon sessions` to know its title.
    pub fn stale(&self, dir: &Path, logs: &[(PathBuf, Option<u64>)]) -> bool {
        if self.failed.get(dir).is_some_and(|at| at.elapsed() < RETRY) {
            return false;
        }
        logs.iter().any(|(path, mtime)| match self.seen.get(path) {
            Some(seen) => seen.title.is_none() && seen.mtime != *mtime,
            None => true,
        })
    }

    /// Record one directory's answer: every log in `logs` is now known, with or without a title.
    pub fn learn(&mut self, dir: &Path, logs: &[(PathBuf, Option<u64>)], firsts: &HashMap<String, String>) {
        self.failed.remove(dir);
        for (path, mtime) in logs {
            let title = path.file_stem().and_then(|s| s.to_str()).and_then(|stem| firsts.get(stem)).cloned();
            self.seen.insert(path.clone(), Seen { mtime: *mtime, title });
        }
    }

    pub fn learn_models(&mut self, models: &HashMap<String, String>) {
        self.models.extend(models.iter().map(|(stem, model)| (stem.clone(), model.clone())));
    }

    pub fn model(&self, id: &str) -> Option<&str> {
        self.models.get(id).map(String::as_str)
    }

    pub fn failed(&mut self, dir: &Path) {
        self.failed.insert(dir.to_path_buf(), Instant::now());
    }

    pub fn title(&self, path: &Path) -> Option<&str> {
        self.seen.get(path).and_then(|seen| seen.title.as_deref())
    }

    pub fn remember_cwd(&mut self, id: &str, cwd: &Path) {
        self.cwds.insert(id.to_string(), cwd.to_path_buf());
    }

    pub fn cwd(&self, id: &str) -> Option<&Path> {
        self.cwds.get(id).map(PathBuf::as_path)
    }
}

/// The chat's birth name (`quiet-owl-lantern`, no time) for an inbox chat, `<project> · HH:MM` for a chat in a named project,
/// the folder's name for any other folder, and `chat · HH:MM` (no cwd known). `scratch` is the perch's scratch directory: a cwd
/// under it. A scratch folder from before birth names, a bare timestamp, is never a name: it reads `new chat · HH:MM`.
pub fn fallback(cwd: Option<&Path>, scratch: Option<&Path>, project: Option<&str>, mtime: Option<u64>) -> String {
    let birth = cwd
        .zip(scratch)
        .and_then(|(cwd, scratch)| cwd.strip_prefix(scratch).ok())
        .and_then(|rest| rest.components().next())
        .and_then(|first| first.as_os_str().to_str());
    if let Some(birth) = birth.filter(|name| name.chars().any(|c| c.is_ascii_alphabetic())) {
        return birth.to_string();
    }
    let name = match cwd {
        Some(cwd) if scratch.is_some_and(|scratch| cwd.starts_with(scratch)) => "new chat".to_string(),
        Some(_) if project.is_some() => project.unwrap_or_default().to_string(),
        Some(cwd) => cwd.file_name().and_then(|n| n.to_str()).map(str::to_string).unwrap_or_else(|| "chat".into()),
        None => "chat".to_string(),
    };
    match mtime.and_then(hhmm) {
        Some(at) => format!("{name} · {at}"),
        None => name,
    }
}

/// Local `HH:MM` of a unix time in seconds.
fn hhmm(secs: u64) -> Option<String> {
    let t = libc::time_t::try_from(secs).ok()?;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    Some(format!("{:02}:{:02}", tm.tm_hour, tm.tm_min))
}

pub fn mtime_secs(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    modified.duration_since(SystemTime::UNIX_EPOCH).ok().map(|since| since.as_secs())
}

/// The `*.eid` logs in `dir` as (path, mtime), by the path the registry would give them: `<dir>/<stem>.eid`.
pub fn logs_in(dir: &Path) -> Result<Vec<(PathBuf, Option<u64>)>> {
    let mut logs = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("eid") {
            let mtime = mtime_secs(&path);
            logs.push((path, mtime));
        }
    }
    Ok(logs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_lines_give_the_first_message_by_stem() {
        let out = b"/s/a.eid\tmock\t4\thello   there\n/s/b.eid\tmock\t1\t\n/s/c.eid\t(unreadable: bad)\n/s/d.eid\tm\t2\tx\ty\n";
        let got = parse_sessions(out);
        assert_eq!(got.get("a").map(String::as_str), Some("hello there"), "whitespace is flattened");
        assert!(!got.contains_key("b"), "an empty first message is no title");
        assert!(!got.contains_key("c"), "an unreadable row is skipped");
        assert_eq!(got.get("d").map(String::as_str), Some("x y"), "a tab inside the message does not shift the columns");
    }

    #[test]
    fn a_titled_log_is_never_asked_again_and_an_untitled_one_only_when_it_moved() {
        let dir = Path::new("/d");
        let (a, b) = (PathBuf::from("/d/a.eid"), PathBuf::from("/d/b.eid"));
        let mut cache = Cache::default();
        let logs = vec![(a.clone(), Some(1)), (b.clone(), Some(1))];
        assert!(cache.stale(dir, &logs), "nothing is known yet");
        cache.learn(dir, &logs, &HashMap::from([("a".to_string(), "first".to_string())]));
        assert!(!cache.stale(dir, &logs), "a is titled, b is untitled and unmoved");
        assert!(!cache.stale(dir, &[(a.clone(), Some(99))]), "a titled log that moved is still titled");
        assert!(cache.stale(dir, &[(b.clone(), Some(2))]), "an untitled log that moved may have a title now");
        assert!(cache.stale(dir, &[(PathBuf::from("/d/new.eid"), Some(1))]), "a log never seen");
        cache.failed(dir);
        assert!(!cache.stale(dir, &[(PathBuf::from("/d/new.eid"), Some(1))]), "a failed directory is left alone for a while");
    }

    #[test]
    fn the_fallback_names_the_folder_scratch_or_nothing() {
        assert!(fallback(Some(Path::new("/h/proj")), None, None, None) == "proj");
        assert!(fallback(Some(Path::new("/h/proj/sub")), None, Some("My App"), Some(0)).starts_with("My App · "));
        assert!(fallback(Some(Path::new("/h/Minerva/scratch/20261005-1")), Some(Path::new("/h/Minerva/scratch")), None, Some(0)).starts_with("new chat · "));
        let scratch = Some(Path::new("/h/Minerva/scratch"));
        assert_eq!(fallback(Some(Path::new("/h/Minerva/scratch/quiet-owl-lantern")), scratch, None, Some(0)), "quiet-owl-lantern", "the birth name, no time");
        assert_eq!(fallback(Some(Path::new("/h/Minerva/scratch/quiet-owl-lantern/sub")), scratch, None, None), "quiet-owl-lantern");
        assert!(fallback(None, None, None, Some(0)).starts_with("chat · "));
        assert_eq!(hhmm(0).map(|t| t.len()), Some(5));
    }

    #[test]
    fn a_read_names_its_config_in_the_environment_and_removes_the_name_without_one() {
        use crate::spawn::eidolon_config_entry;
        let eidolon = Path::new("/usr/bin/eidolon");
        let args = [OsString::from("sessions")];
        let with = read_command(eidolon, Some(Path::new("/h/minerva.toml")), &args);
        assert_eq!(eidolon_config_entry(with.as_std()), Some(Some(OsString::from("/h/minerva.toml"))));
        assert_eq!(eidolon_config_entry(read_command(eidolon, None, &args).as_std()), Some(None));
    }
}
