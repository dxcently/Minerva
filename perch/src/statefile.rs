//! Reading the perch's small state files (`names.json`, `archive.json`, `forks.json`, `chat-cwds.json`, `projects.json`).
//!
//! A file that is not there is empty. A file that is there and cannot be read or does not parse is **set aside** as
//! `<name>.bad-<unix seconds>` (`-1`, `-2` on a clash) with a warning, and the store starts empty: the next write then creates a
//! new file and the bad one is still on disk to read, instead of being overwritten without a word.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

/// The file's value, or the default when it is absent, or was set aside.
pub(crate) fn load<T: DeserializeOwned + Default>(file: Option<&Path>) -> T {
    let Some(file) = file else { return T::default() };
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(e) => {
            set_aside(file, &format!("could not be read: {e}"));
            return T::default();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(e) => {
            set_aside(file, &format!("does not parse: {e}"));
            T::default()
        }
    }
}

/// Renames `file` to `<file>.bad-<unix seconds>`; where that is not possible the file stays and the warning says so.
fn set_aside(file: &Path, why: &str) {
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    for n in 0u32..100 {
        let suffix = if n == 0 { format!("{name}.bad-{ts}") } else { format!("{name}.bad-{ts}-{n}") };
        let to: PathBuf = file.with_file_name(suffix);
        if std::fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        match std::fs::rename(file, &to) {
            Ok(()) => {
                tracing::warn!(path = %file.display(), kept_as = %to.display(), "perch: {name} {why}; it was set aside and the list starts empty");
                return;
            }
            Err(e) => {
                tracing::warn!(path = %file.display(), error = %e, "perch: {name} {why}, and it could not be set aside; the next write will replace it");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn a_corrupt_file_is_set_aside_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("names.json");
        assert!(load::<BTreeMap<String, String>>(Some(&file)).is_empty(), "absent is empty and nothing is created");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        std::fs::write(&file, b"{ not json").unwrap();
        assert!(load::<BTreeMap<String, String>>(Some(&file)).is_empty());
        assert!(!file.exists(), "the corrupt file is out of the way before any write");
        let kept: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(kept.len(), 1);
        assert!(kept[0].file_name().unwrap().to_str().unwrap().starts_with("names.json.bad-"), "{kept:?}");
        assert_eq!(std::fs::read(&kept[0]).unwrap(), b"{ not json", "its bytes are all still there");
        // A second corrupt file in the same second does not clobber the first set-aside.
        std::fs::write(&file, b"[2").unwrap();
        assert!(load::<BTreeMap<String, String>>(Some(&file)).is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        // A good file is read as it is and left where it is.
        std::fs::write(&file, br#"{"a":"b"}"#).unwrap();
        assert_eq!(load::<BTreeMap<String, String>>(Some(&file)).get("a").map(String::as_str), Some("b"));
        assert!(file.exists());
    }
}
