//! `GET /hub/tree?session=<id>&path=<rel>`: one directory under a live session's cwd
//! (hub.md section 7). Names and kinds only. **No file contents, ever**, no link target and no
//! absolute path in any answer: the files tab is a map, and reading a file is the door's job.
//!
//! The rules, in the order they are applied (the numbers are hub.md section 7's steps):
//!
//! ```text
//!   1  query: exactly `session` and `path`, each at most once, decoded once    -> 400
//!   2  path: no NUL, no `\`, not absolute, no ``/`.`/`..` segment, <= 4096 B   -> 404
//!   3  every segment against the deny list (case-insensitive, whole name) and
//!      against the 8.3 short-name shape (`~` then a digit)                     -> 404
//!      (the same two tests run on the cwd's own segments below its `--root`)
//!   4  root.join(path), canonicalize, must start_with(root)                    -> 404
//!      (a symlink out of the tree dies here; the tests of step 3 are run again
//!       on the canonical remainder, so an inside link cannot launder `.git`)
//!   5  must be a directory; read_dir; entry kinds from `d_type` (never followed)
//!   6  denied names dropped; dirs first, then by name; cap 2000                -> truncated
//!      only the kept files are `lstat`ed, for their size
//! ```
//!
//! Every refusal after the query is the same empty 404, so a denied name cannot be told from a
//! missing one. The decode is [`crate::files::decode`], the one the statics use.
//!
//! **Cost.** A directory is scanned for at most [`SCAN_MAX`] entries; past that the listing is
//! `truncated` whatever was kept. The caller also holds a [`Slots`] permit (at most
//! [`MAX_LISTINGS`] listings at once) around the blocking call.
//!
//! **Why `~<digit>`.** On NTFS (`/mnt/c`) a directory has a second spelling, its 8.3 short name:
//! `GIT~1` is `.git` and `SSH~1` is `.ssh`, and `realpath` keeps the spelling it was given, so
//! a deny list that compares spellings is blind to it. Refusing the alias shape is the cheap,
//! spelling-level answer; a real name like `backup~1` cannot be opened as a `path` either.

use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Entries in one answer. Past it the rest are not sent and `truncated` says so.
pub const MAX_ENTRIES: usize = 2000;

/// Entries read from one directory before the scan stops and the answer says `truncated`. The
/// bound is on work (a `node_modules`, or a cache dir of millions), not on the answer, which
/// [`MAX_ENTRIES`] caps.
pub const SCAN_MAX: usize = 100_000;

/// Listings running at once. A listing is a blocking-pool thread that may sit on a slow
/// filesystem; the pool's default cap is 512, which a retrying page could use up.
pub const MAX_LISTINGS: usize = 4;

/// `path` longer than this is not a path anyone typed.
const MAX_PATH: usize = 4096;

/// The concurrency cap around listings: [`Slots::take`] is a permit or `None`, never a wait.
#[derive(Clone)]
pub struct Slots(Arc<Semaphore>);

impl Slots {
    pub fn new(n: usize) -> Self {
        Self(Arc::new(Semaphore::new(n)))
    }

    /// A free slot, held until the permit is dropped. `None` is the 503.
    pub fn take(&self) -> Option<OwnedSemaphorePermit> {
        self.0.clone().try_acquire_owned().ok()
    }
}

/// The query, decoded once: `(session, path)`. `None` is a 400: an unknown key, a repeated
/// key, a pair with no `=`, a missing `session`, or a malformed escape.
pub fn parse_query(query: &str) -> Option<(String, String)> {
    let (mut session, mut path) = (None, None);
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        let slot = match crate::files::decode(key)?.as_str() {
            "session" => &mut session,
            "path" => &mut path,
            _ => return None,
        };
        if slot.is_some() {
            return None;
        }
        *slot = Some(crate::files::decode(value)?);
    }
    Some((session?, path.unwrap_or_default()))
}

/// Step 3's list. Whole names, case-insensitive, except `credentials`, which is a substring.
/// A config knob may one day add names; it never removes these.
pub fn denied(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == ".git"
        || n == ".env"
        || n.starts_with(".env.")
        || [".key", ".pem", ".p12", ".pfx", ".kdbx", ".token"].iter().any(|ext| n.ends_with(ext))
        || n.starts_with("id_")
        || n.contains("secret")
        || [".netrc", ".ssh", ".gnupg", ".aoide", ".envrc", ".pgpass", ".npmrc", ".pypirc", "auth.json", "creds.json"].contains(&n.as_str())
        || n.contains("credentials")
}

/// The NTFS 8.3 alias shape: a `~` followed by an ASCII digit anywhere in the name. Applied to
/// the segments of `path` and of the cwd, not to the entries of a listing (`notes~1.txt` is a
/// plausible editor backup and is only a name there).
fn short_name(name: &str) -> bool {
    name.as_bytes().windows(2).any(|w| w[0] == b'~' && w[1].is_ascii_digit())
}

/// A name the tree will not open or pass through.
pub fn refused(name: &str) -> bool {
    denied(name) || short_name(name)
}

#[derive(Serialize, Debug)]
pub struct Entry {
    pub name: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Serialize, Debug)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<Entry>,
    pub skipped: u32,
    pub truncated: bool,
}

/// Where a tree or file request starts: the cwd of a live door, or the remembered folder of a saved (kept) chat.
/// A saved chat is served read-only through exactly the same checks; only the words of a refusal differ.
pub enum Base {
    Live(std::path::PathBuf),
    Saved(std::path::PathBuf),
}

/// The 404 sentence of a saved chat whose folder cannot be used now (moved, deleted, or no longer under a `--root`).
pub const SAVED_GONE: &str = "this chat's folder has moved, been deleted, or is no longer allowed";

/// The one question both `/hub/tree` and `/hub/file` ask first: which cwd, or which refusal. A live door's cwd as ever
/// (409 `{"state"}` when it is between states); else a kept row whose folder is known ([`Base::Saved`]); a kept row with no
/// known folder is a 409 with a sentence; a row that is live elsewhere keeps its 409 state; anything else (a trashed id
/// is no row at all) is 404. The cwd is re-judged against `--root` by the caller, per request.
pub fn base(ctx: &crate::serve::Ctx, id: &str) -> Result<Base, Box<hyper::Response<crate::serve::BoxBody>>> {
    use crate::serve::{json, status};
    use crate::sessions::{State, TreeRoot};
    use hyper::StatusCode;
    match ctx.registry.tree_root(id) {
        TreeRoot::Live(root) => Ok(Base::Live(root)),
        TreeRoot::Not(state) => Err(Box::new(json(StatusCode::CONFLICT, &serde_json::json!({ "state": state })))),
        TreeRoot::NotOurs => match ctx.registry.list().into_iter().find(|row| row.id == id) {
            Some(row) if row.state == State::Kept => match row.cwd {
                Some(cwd) => Ok(Base::Saved(cwd.into())),
                None => Err(Box::new(json(StatusCode::CONFLICT, &serde_json::json!({ "state": row.state, "error": "this chat's folder isn't known" })))),
            },
            Some(row) => Err(Box::new(json(StatusCode::CONFLICT, &serde_json::json!({ "state": row.state })))),
            None => Err(Box::new(status(StatusCode::NOT_FOUND))),
        },
    }
}

/// Steps 2-6, blocking: the caller runs it under `spawn_blocking`. `None` is the 404.
///
/// `floor` is the `--root` that contains `root` (the cwd): the cwd's own segments below it are
/// judged like a `path` is, so a session whose cwd is `~/.ssh` lists nothing.
pub fn list(root: &Path, floor: &Path, rel: &str) -> Option<Listing> {
    list_bounded(root, floor, rel, SCAN_MAX)
}

/// Steps 2-4, blocking: the canonical path `rel` names under `root` (the cwd), or `None`, which is every
/// refusal. Whether it is a directory or a file is the caller's question; [`list`] and the file route share it.
pub fn resolve(root: &Path, floor: &Path, rel: &str) -> Option<std::path::PathBuf> {
    let segments = segments(rel)?;
    if segments.iter().any(|s| refused(s)) {
        return None;
    }
    // The root is the door's cwd and is canonicalized now, when the request is served.
    let root = root.canonicalize().ok()?;
    if root.strip_prefix(floor).ok()?.components().any(|c| refused(&c.as_os_str().to_string_lossy())) {
        return None;
    }
    let mut named = root.clone();
    named.extend(&segments);
    let real = named.canonicalize().ok()?;
    if !real.starts_with(&root) {
        return None;
    }
    // A link that stays inside the tree is followed by `canonicalize`, so what it reached is
    // judged by name too: `ln -s .git history` must not list `.git`.
    let inside = real.strip_prefix(&root).unwrap_or(&real);
    if inside.components().any(|c| refused(&c.as_os_str().to_string_lossy())) {
        return None;
    }
    Some(real)
}

fn list_bounded(root: &Path, floor: &Path, rel: &str, scan_max: usize) -> Option<Listing> {
    let real = resolve(root, floor, rel)?;
    if !std::fs::metadata(&real).ok()?.is_dir() {
        return None;
    }

    // Names and kinds only, from `d_type`: no `lstat` per entry, and a link is a link whatever
    // it points at. The scan stops at `scan_max` entries read.
    let (mut skipped, mut scanned, mut cut) = (0u32, 0usize, false);
    let mut rows: Vec<(String, &'static str)> = Vec::new();
    for entry in std::fs::read_dir(&real).ok()? {
        if scanned >= scan_max {
            cut = true;
            break;
        }
        scanned += 1;
        let Ok(entry) = entry else { continue };
        let Ok(name) = entry.file_name().into_string() else {
            skipped += 1;
            continue;
        };
        if denied(&name) {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        let kind = if kind.is_symlink() {
            "link"
        } else if kind.is_dir() {
            "dir"
        } else if kind.is_file() {
            "file"
        } else {
            "other"
        };
        rows.push((name, kind));
    }
    rows.sort_by(|a, b| ((a.1 != "dir"), &a.0).cmp(&((b.1 != "dir"), &b.0)));
    let truncated = cut || rows.len() > MAX_ENTRIES;
    rows.truncate(MAX_ENTRIES);

    // `lstat` only what is kept, and only for the size of a regular file. An entry that vanished
    // since the scan is dropped.
    let entries = rows
        .into_iter()
        .filter_map(|(name, kind)| {
            if kind != "file" {
                return Some(Entry { name, kind, size: None });
            }
            let meta = std::fs::symlink_metadata(real.join(&name)).ok()?;
            Some(Entry { name, kind, size: Some(meta.len()) })
        })
        .collect();
    Some(Listing { path: rel.to_string(), entries, skipped, truncated })
}

/// Step 2. Empty is the root (no segments).
fn segments(rel: &str) -> Option<Vec<&str>> {
    if rel.len() > MAX_PATH || rel.contains('\0') || rel.contains('\\') || rel.starts_with('/') {
        return None;
    }
    if rel.is_empty() {
        return Some(Vec::new());
    }
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return None;
    }
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_is_exactly_session_and_path_decoded_once() {
        assert_eq!(parse_query("session=a"), Some(("a".into(), String::new())));
        assert_eq!(parse_query("session=a&path=src%2Fx"), Some(("a".into(), "src/x".into())));
        assert_eq!(parse_query("path=b&session=a"), Some(("a".into(), "b".into())));
        assert_eq!(parse_query("session=a&path=%2541"), Some(("a".into(), "%41".into())), "one pass, not two");
        for bad in [
            "",
            "path=x",
            "session=a&x=1",
            "session=a&session=b",
            "session=a&path=1&path=2",
            "session",
            "session=a&",
            "session=a&path=%zz",
            "session=a&path=%ff",
        ] {
            assert_eq!(parse_query(bad), None, "{bad:?}");
        }
    }

    /// Mutation: drop any one name from the list (or turn `contains("credentials")` back into
    /// `starts_with`) and this goes red.
    #[test]
    fn the_deny_list_is_whole_names_and_case_insensitive() {
        for n in [
            ".git",
            ".GIT",
            ".env",
            ".Env",
            ".env.local",
            "a.key",
            "A.PEM",
            "x.p12",
            "x.pfx",
            "x.kdbx",
            "id_rsa",
            "ID_ed25519",
            "secret.txt",
            "my-Secret-notes",
            "a.token",
            ".netrc",
            ".ssh",
            ".gnupg",
            ".aoide",
            "credentials",
            "credentials.json",
            ".credentials.json",
            ".git-credentials",
            "mycredentials",
            "AWS-Credentials.csv",
            ".envrc",
            ".ENVRC",
            ".pgpass",
            ".npmrc",
            ".pypirc",
            "auth.json",
            "Creds.json",
        ] {
            assert!(denied(n), "{n}");
        }
        for n in [
            "src", ".gitignore", ".github", "environment", "env", "idea", "monkey", "key", "pem", "main.rs", "creds", "auth",
            "pgpass", ".envrc.bak",
        ] {
            assert!(!denied(n), "{n}");
        }
    }

    #[test]
    fn a_path_names_segments_or_nothing() {
        assert_eq!(segments(""), Some(vec![]));
        assert_eq!(segments("a/b"), Some(vec!["a", "b"]));
        assert_eq!(segments("..."), Some(vec!["..."]));
        for bad in ["..", ".", "/etc", "a//b", "a/", "a/../b", "a\\b", "a\0b", &"a/".repeat(2049)] {
            assert_eq!(segments(bad), None, "{bad:?}");
        }
    }

    /// The 8.3 alias shape is refused as a path segment, wherever it sits, in any case.
    /// Mutation: make `short_name` return false and this goes red.
    #[test]
    fn an_ntfs_short_name_is_refused_in_a_path() {
        let root = tempfile::tempdir().unwrap();
        // They exist, so it is the shape that is refused and not a missing entry.
        for sub in ["GIT~1", "a/SSH~1/x", "git~1", "AOIDE~2", "x~9y", "a/b/CREDEN~1"] {
            std::fs::create_dir_all(root.path().join(sub)).unwrap();
        }
        for sub in ["GIT~1", "a/SSH~1/x", "git~1", "AOIDE~2", "x~9y", "a/b/CREDEN~1"] {
            assert!(list(root.path(), root.path(), sub).is_none(), "{sub}");
        }
        // A tilde that is not followed by a digit is a name.
        std::fs::create_dir(root.path().join("a~b")).unwrap();
        std::fs::create_dir(root.path().join("~")).unwrap();
        assert!(list(root.path(), root.path(), "a~b").is_some());
        assert!(list(root.path(), root.path(), "~").is_some());
        // The listing itself still shows a real file that has the shape: it is a name there.
        std::fs::write(root.path().join("notes~1.txt"), "x").unwrap();
        let names: Vec<String> = list(root.path(), root.path(), "").unwrap().entries.into_iter().map(|e| e.name).collect();
        assert!(names.contains(&"notes~1.txt".to_string()), "{names:?}");
    }

    /// An alias reached through a link inside the tree is judged on the canonical remainder.
    #[test]
    fn a_link_to_a_short_name_is_refused() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("GIT~1")).unwrap();
        std::os::unix::fs::symlink("GIT~1", root.path().join("h")).unwrap();
        assert!(list(root.path(), root.path(), "h").is_none());
    }

    /// A cwd that is a denied name, or an alias, or sits under one, below its `--root`, lists
    /// nothing: not even its root. Segments above the `--root` are not judged.
    /// Mutation: drop the root-segment check in `list_bounded` and this goes red.
    #[test]
    fn a_cwd_that_is_a_denied_name_is_not_listable() {
        let floor = tempfile::tempdir().unwrap();
        for cwd in [".ssh", "GIT~1", ".git/sub", "a/.gnupg/b", "SECRETS", ".aoide"] {
            let dir = floor.path().join(cwd);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("known_hosts"), "x").unwrap();
            assert!(list(&dir, floor.path(), "").is_none(), "root {cwd}");
            assert!(list(&dir, floor.path(), "known_hosts").is_none(), "root {cwd}");
        }
        // The same names *above* the floor are the operator's choice of `--root`.
        let outer = tempfile::tempdir().unwrap();
        let above = outer.path().join(".ssh").join("work");
        std::fs::create_dir_all(&above).unwrap();
        assert!(list(&above, &above, "").is_some());
        // A cwd outside its floor is not a listing at all.
        assert!(list(outer.path(), floor.path(), "").is_none());
    }

    /// A FIFO is `other` and listing it does not block, a dangling link is a `link`, a regular
    /// file has a size, a `.git` *file* (worktree, submodule) is hidden by name.
    /// Mutation: drop the `denied(&name)` filter and the `.git` file shows up.
    #[test]
    fn edge_kinds_list_as_what_they_are() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;
        let dir = tempfile::tempdir().unwrap();
        let fifo = CString::new(dir.path().join("pipe").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        std::os::unix::fs::symlink("nowhere", dir.path().join("dangling")).unwrap();
        std::fs::write(dir.path().join("f"), "abc").unwrap();
        std::fs::write(dir.path().join(".git"), "gitdir: ../x/.git/worktrees/w\n").unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        let got = list(dir.path(), dir.path(), "").unwrap();
        let rows: Vec<(&str, &str, Option<u64>)> = got.entries.iter().map(|e| (e.name.as_str(), e.kind, e.size)).collect();
        assert_eq!(rows, vec![("d", "dir", None), ("dangling", "link", None), ("f", "file", Some(3)), ("pipe", "other", None)]);
        // A FIFO as a path is not a directory.
        assert!(list(dir.path(), dir.path(), "pipe").is_none());
        assert!(list(dir.path(), dir.path(), ".git").is_none());
    }

    /// A directory that cannot be read is the 404, not a 500; so is a cwd that was deleted.
    #[test]
    fn an_unreadable_directory_or_a_deleted_cwd_is_a_404() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads anything; the check is only meaningful for an ordinary user.
        if unsafe { libc::geteuid() } != 0 {
            assert!(list(dir.path(), dir.path(), "locked").is_none());
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        let gone = dir.path().join("gone");
        std::fs::create_dir(&gone).unwrap();
        std::fs::remove_dir(&gone).unwrap();
        assert!(list(&gone, dir.path(), "").is_none());
    }

    /// The scan stops at its bound and says so, even when the kept part is under the cap.
    /// Mutation: drop the `scanned >= scan_max` break and this goes red.
    #[test]
    fn the_scan_is_bounded_and_says_truncated() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..80 {
            std::fs::write(dir.path().join(format!("f{n:02}")), "").unwrap();
        }
        let whole = list_bounded(dir.path(), dir.path(), "", 80).unwrap();
        assert_eq!((whole.entries.len(), whole.truncated), (80, false), "exactly at the bound is not past it");
        let cut = list_bounded(dir.path(), dir.path(), "", 50).unwrap();
        assert_eq!((cut.entries.len(), cut.truncated), (50, true));
        // The bound counts what was read, denied names included.
        std::fs::write(dir.path().join(".env"), "").unwrap();
        let more = list_bounded(dir.path(), dir.path(), "", 80).unwrap();
        assert!(more.truncated && more.entries.len() <= 80);
    }

    /// Mutation: have `take` wait instead of refusing, or ignore the permit, and this goes red.
    #[test]
    fn the_listing_slots_refuse_instead_of_queueing() {
        let slots = Slots::new(2);
        let (a, b) = (slots.take().unwrap(), slots.take().unwrap());
        assert!(slots.take().is_none(), "the third is refused");
        drop(a);
        let c = slots.take().expect("a freed slot is reusable");
        assert!(slots.take().is_none());
        drop((b, c));
        assert!(slots.take().is_some());
    }
}
