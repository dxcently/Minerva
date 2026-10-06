//! Which saved chats sit in the sidebar's `other` group, which are pinned, and the trash they can be deleted into.
//!
//! `<home>/archive.json` is `{ "<id>": {"archived": bool, "at": ms, "unarchived_at": ms?, "pinned": bool?} }`, written
//! atomically with mode `0600`. A chat is **auto-other** when it is not live, not pinned, not archived by hand, and it has been idle
//! for [`IDLE_MS`]: idle is `max(log mtime, unarchived_at)`, so un-archiving a chat gives it a fresh week. That is computed at
//! listing time ([`Archive::flags`]); no file is ever moved for it.
//!
//! Delete is a move into `<home>/trash/` (same filesystem, so an atomic rename, keeping the file name). Nothing here ever
//! touches a path outside `<home>/sessions` and `<home>/trash`, and an id reaches a path only through
//! [`crate::sessions::log_path`] after [`crate::sessions::id_ok`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A chat with no activity for this long drops into `other` by itself: 7 days.
pub const IDLE_MS: u64 = 7 * 24 * 3600 * 1000;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Mark {
    #[serde(default)]
    pub archived: bool,
    /// When it was last archived, unix ms.
    #[serde(default)]
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unarchived_at: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// What the list says of one chat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub archived: bool,
    pub auto_other: bool,
    pub pinned: bool,
}

pub struct Archive {
    file: Option<PathBuf>,
    map: Mutex<BTreeMap<String, Mark>>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl Archive {
    /// Reads `file` (absent is empty; a corrupt one is set aside as `archive.json.bad-<ts>`, see [`crate::statefile`]).
    pub fn open(file: Option<PathBuf>) -> Self {
        let map = crate::statefile::load(file.as_deref());
        Self { file, map: Mutex::new(map) }
    }

    /// The flags of one chat at `now` (ms). `live` is any state in which a door holds the log: a live chat is never
    /// archived or auto-other. `mtime` is the log's, in seconds.
    pub fn flags(&self, id: &str, mtime: Option<u64>, live: bool, now: u64) -> Flags {
        let mark = self.map.lock().unwrap().get(id).cloned().unwrap_or_default();
        if live {
            return Flags { pinned: mark.pinned, ..Flags::default() };
        }
        if mark.archived {
            return Flags { archived: true, ..Flags::default() };
        }
        if mark.pinned {
            return Flags { pinned: true, ..Flags::default() };
        }
        let last = mtime.map(|s| s * 1000).into_iter().chain(mark.unarchived_at).max();
        let idle = last.is_some_and(|last| last.saturating_add(IDLE_MS) < now);
        Flags { auto_other: idle, ..Flags::default() }
    }

    /// Edits one entry and writes the file, pruning entries whose log is gone (`exists`). The memory changes only if
    /// the write succeeded.
    fn edit(&self, id: &str, exists: &dyn Fn(&str) -> bool, change: impl FnOnce(&mut Mark)) -> Result<()> {
        let Some(file) = &self.file else { anyhow::bail!("no Minerva home") };
        let mut map = self.map.lock().unwrap();
        let mut next = map.clone();
        change(next.entry(id.to_string()).or_default());
        next.retain(|k, mark| (k == id || exists(k)) && *mark != Mark::default());
        write(file, &next)?;
        *map = next;
        Ok(())
    }

    /// Archive: into `other` now. It also unpins.
    pub fn archive(&self, id: &str, now: u64, exists: &dyn Fn(&str) -> bool) -> Result<()> {
        self.edit(id, exists, |m| {
            m.archived = true;
            m.at = now;
            m.pinned = false;
        })
    }

    /// Unarchive: back out of `other`, and idle for a fresh [`IDLE_MS`] from `now`.
    pub fn unarchive(&self, id: &str, now: u64, exists: &dyn Fn(&str) -> bool) -> Result<()> {
        self.edit(id, exists, |m| {
            m.archived = false;
            m.unarchived_at = Some(now);
        })
    }

    pub fn set_pinned(&self, id: &str, pinned: bool, exists: &dyn Fn(&str) -> bool) -> Result<()> {
        self.edit(id, exists, |m| {
            m.pinned = pinned;
            // A pinned chat is in one place: pinning one that was put in `other` brings it out.
            m.archived &= !pinned;
        })
    }

    pub fn is_archived(&self, id: &str) -> bool {
        self.map.lock().unwrap().get(id).is_some_and(|m| m.archived)
    }

    pub fn is_pinned(&self, id: &str) -> bool {
        self.map.lock().unwrap().get(id).is_some_and(|m| m.pinned)
    }

    /// Drops the entry of a chat that left the sessions dir (trashed): a restored chat is a fresh, unpinned, unarchived one.
    pub fn forget(&self, id: &str, exists: &dyn Fn(&str) -> bool) -> Result<()> {
        let Some(file) = &self.file else { return Ok(()) };
        let mut map = self.map.lock().unwrap();
        let mut next = map.clone();
        next.remove(id);
        next.retain(|k, _| exists(k));
        write(file, &next)?;
        *map = next;
        Ok(())
    }
}

fn write(file: &Path, map: &BTreeMap<String, Mark>) -> Result<()> {
    let body = serde_json::to_vec_pretty(map).context("serializing the archive")?;
    harnox::fs::write_atomic_0600(file, &body).with_context(|| format!("writing {}", file.display()))
}

/// `<home>/trash`.
pub fn trash_dir(home: &Path) -> PathBuf {
    home.join("trash")
}

/// Creates the trash dir `0700` on first use.
pub fn ensure_trash(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 3600 * 1000;
    const NOW: u64 = 100 * DAY;
    fn all(_: &str) -> bool {
        true
    }

    #[test]
    fn a_chat_idle_a_week_is_auto_other_unless_live_pinned_or_fresh() {
        let a = Archive::open(None);
        let secs = |days_ago: u64| Some((NOW - days_ago * DAY) / 1000);
        assert!(!a.flags("x", secs(6), false, NOW).auto_other);
        assert!(a.flags("x", secs(8), false, NOW).auto_other);
        assert!(!a.flags("x", secs(8), true, NOW).auto_other, "a live chat never ages out");
        assert!(!a.flags("x", None, false, NOW).auto_other, "no mtime: not hidden");
    }

    #[test]
    fn archive_unarchive_pin_and_the_grace_after_unarchiving() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("archive.json");
        let a = Archive::open(Some(file.clone()));
        let old = Some((NOW - 30 * DAY) / 1000);
        a.archive("x", NOW, &all).unwrap();
        assert!(a.flags("x", old, false, NOW).archived);
        assert!(!a.flags("x", old, true, NOW).archived, "live overrides");
        a.unarchive("x", NOW, &all).unwrap();
        let f = a.flags("x", old, false, NOW + DAY);
        assert!(!f.archived && !f.auto_other, "a fresh week after unarchive: {f:?}");
        assert!(a.flags("x", old, false, NOW + 8 * DAY).auto_other, "and idle again after it");
        a.set_pinned("x", true, &all).unwrap();
        let f = a.flags("x", old, false, NOW + 80 * DAY);
        assert!(f.pinned && !f.auto_other, "pinned never ages out");
        a.archive("x", NOW, &all).unwrap();
        assert!(!a.is_pinned("x"), "archiving unpins");
        // Persisted, 0600, and pruned of ids with no log.
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(Archive::open(Some(file.clone())).is_archived("x"));
        a.set_pinned("y", true, &|id| id != "x").unwrap();
        assert!(!a.is_archived("x") && a.is_pinned("y"), "x has no log: pruned");
        a.forget("y", &all).unwrap();
        assert!(!a.is_pinned("y"));
    }
}
