//! The session logs on disk, as something to pick from.
//!
//! A session is a file, and the sessions directory is a flat pile of them
//! named by the millisecond they were created — which is the right shape
//! for an append-only log and the wrong shape for a human looking for
//! *the one about the parser*. This module is the difference: it opens the
//! logs, reads out of each one the four things that identify it (when it
//! was last written, what it was about, which model, which directory) and
//! hands them over as rows.
//!
//! The reading is the expensive part — a summary costs a full replay of
//! the log — so the list is bounded ([`SCAN`]) and taken from the newest
//! files by modification time, which is the order the picker wants anyway.
//! Sessions no one ever spoke in are skipped unless asked for: `eidolon
//! tui` writes a `SessionStart` on every launch, so the pile has more
//! empty logs in it than real ones, and a picker full of them is a picker
//! no one reads.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use eidolon_core::session::{RecordKind, Session};

/// How many logs are opened for one listing, newest first. A summary is a
/// full replay; a hundred of them is a beat, a thousand is a hang.
const SCAN: usize = 200;

/// One session log, as much of it as a picker row needs.
pub struct Summary {
    pub path: PathBuf,
    pub modified: SystemTime,
    /// The model in effect on the branch, as the catalog names it.
    pub model: String,
    /// The directory the session was started in — where its tools ran,
    /// and where they will run again if it is resumed.
    pub cwd: String,
    /// User and assistant messages on the branch: how much was said.
    pub messages: usize,
    /// The first thing the operator asked, which is what a session is
    /// remembered by.
    pub first: String,
    /// The last turn settled. A session that ends mid-turn is finished on
    /// resume, so it is worth saying which ones will do that.
    pub settled: bool,
}

impl Summary {
    /// Read one log. `None` for anything that will not open — a listing
    /// with a corrupt file in it should be a listing without that file,
    /// not an error.
    fn read(path: PathBuf, modified: SystemTime) -> Option<Summary> {
        let s = Session::open(&path).ok()?;
        let (start_model, cwd) = s.start().map(|(m, c, _)| (m.to_string(), c.to_string()))?;
        let branch = s.branch();
        let mut messages = 0usize;
        let mut first = String::new();
        for r in &branch {
            match &r.kind {
                RecordKind::UserMessage(m) => {
                    messages += 1;
                    if first.is_empty() {
                        first = one_line(&m.text());
                    }
                }
                RecordKind::AssistantMessage(_) => messages += 1,
                _ => {}
            }
        }
        Some(Summary {
            model: s.model().unwrap_or(start_model),
            settled: s.is_settled(),
            path,
            modified,
            cwd,
            messages,
            first,
        })
    }

    /// Was this session started here? What the listing sorts on, and the
    /// reason the sessions for the directory you are standing in are not
    /// buried under every other project's.
    pub fn is_local(&self, cwd: &Path) -> bool {
        Path::new(&self.cwd) == cwd
    }

    /// The row's second line: where it ran, and whether it ended cleanly.
    pub fn description(&self, cwd: &Path) -> String {
        let mut d = if self.is_local(cwd) {
            "here".to_string()
        } else {
            tilde(&self.cwd)
        };
        if !self.settled {
            d.push_str(" · unsettled");
        }
        d
    }
}

/// Every session worth showing, best first: the ones started in `cwd`,
/// most recently written first, then everything else the same way.
///
/// Two orderings rather than one because the question behind `:sessions`
/// is nearly always "what was I doing in *this* repo", and the answer to
/// the rarer one is a filter away — the picker matches on the path, the
/// model and the first message, so another project's session is found by
/// typing its name rather than by scrolling.
///
/// `all` keeps the logs nothing was ever said in.
pub fn list(dir: &Path, cwd: &Path, all: bool) -> Vec<Summary> {
    let mut files: Vec<(PathBuf, SystemTime)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "eid"))
        .map(|p| {
            let t = p
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (p, t)
        })
        .collect();
    files.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    files.truncate(SCAN);

    let mut out: Vec<Summary> = files
        .into_iter()
        .filter_map(|(p, t)| Summary::read(p, t))
        .filter(|s| all || s.messages > 0)
        .collect();
    // Stable, so the recency order survives inside each half.
    out.sort_by_key(|s| !s.is_local(cwd));
    out
}

/// Remove a log. Guarded rather than trusting: this is the one thing in
/// the harness that destroys a session, and it is reached from a picker
/// where the highlighted row moves under the fingers.
pub fn delete(path: &Path, open: &Path) -> anyhow::Result<String> {
    if path.extension().is_none_or(|x| x != "eid") {
        anyhow::bail!("{} is not a session log", path.display());
    }
    if path == open {
        anyhow::bail!("that is the session you are in; resume another one first");
    }
    std::fs::remove_file(path).map_err(|e| anyhow::anyhow!("deleting {}: {e}", path.display()))?;
    Ok(format!("deleted {}", tilde(&path.display().to_string())))
}

/// How long ago, in one column's worth of characters: `12s`, `4m`, `3h`,
/// `6d`, `8w`. Fixed width and unitless-precise, because the number is
/// there to order the rows, not to time anything.
pub fn ago(t: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s if s < 604_800 => format!("{}d", s / 86_400),
        s => format!("{}w", s / 604_800),
    }
}

/// A home-relative path, the way a shell prompt writes one.
pub fn tilde(path: &str) -> String {
    match dirs::home_dir() {
        Some(h) => match path.strip_prefix(&*h.to_string_lossy()) {
            Some(rest) => format!("~{rest}"),
            None => path.to_string(),
        },
        None => path.to_string(),
    }
}

/// The first line of a message, capped — a picker row, not a transcript.
fn one_line(text: &str) -> String {
    let flat = text.replace('\n', " ");
    let mut out: String = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > 72 {
        out = out.chars().take(71).collect::<String>() + "…";
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn create(dir: &Path, name: &str, cwd: &Path, said: &[&str]) -> PathBuf {
        let p = dir.join(name);
        let mut s = Session::create(&p, "mock", cwd, None).unwrap();
        for t in said {
            s.append(RecordKind::UserMessage(
                eidolon_core::message::Message::user_text(*t),
            ))
            .unwrap();
        }
        p
    }

    #[test]
    fn the_listing_puts_this_directory_first_and_hides_the_empty_logs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let here = dir.join("here");
        let there = dir.join("there");
        // Written oldest to newest; the listing should reverse that within
        // each half, and put `here` above `there` regardless.
        create(dir, "1.eid", &there, &["about the parser"]);
        create(dir, "2.eid", &here, &["about the log"]);
        create(dir, "3.eid", &here, &["about the keys"]);
        create(dir, "4.eid", &here, &[]);
        // A file that is not a log, and one that is not readable as one.
        std::fs::write(dir.join("notes.txt"), "not a session").unwrap();
        std::fs::write(dir.join("torn.eid"), b"\0\0garbage").unwrap();

        let l = list(dir, &here, false);
        assert_eq!(l.len(), 3, "the empty log and the two non-logs are out");
        assert!(l[0].is_local(&here) && l[1].is_local(&here));
        assert_eq!(
            l[2].first, "about the parser",
            "another directory's session comes last"
        );
        assert_eq!(
            l[0].first, "about the keys",
            "newest of this directory's first"
        );
        assert_eq!(l[0].messages, 1);
        assert!(!l[0].settled, "a branch that never settled says so");
        assert_eq!(
            l[2].description(&here),
            format!("{} · unsettled", tilde(&there.display().to_string()))
        );

        let all = list(dir, &here, true);
        assert_eq!(all.len(), 4, "`all` keeps the log nothing was said in");
    }

    #[test]
    fn delete_refuses_the_open_session_and_anything_that_is_not_a_log() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let open = create(dir, "1.eid", dir, &["hello"]);
        let other = create(dir, "2.eid", dir, &["hello"]);
        let notes = dir.join("notes.txt");
        std::fs::write(&notes, "not a session").unwrap();

        assert!(
            delete(&open, &open).is_err(),
            "the session in play is not deletable"
        );
        assert!(delete(&notes, &open).is_err(), "only logs");
        assert!(notes.exists());
        assert!(delete(&other, &open).is_ok());
        assert!(!other.exists());
    }

    #[test]
    fn ago_is_one_column_wide() {
        let now = SystemTime::now();
        assert_eq!(ago(now), "0s");
        assert_eq!(ago(now - Duration::from_secs(90)), "1m");
        assert_eq!(ago(now - Duration::from_secs(7200)), "2h");
        assert_eq!(ago(now - Duration::from_secs(3 * 86_400)), "3d");
        assert_eq!(ago(now - Duration::from_secs(21 * 86_400)), "3w");
    }
}
