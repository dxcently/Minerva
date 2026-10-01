//! Chrome profile directories: the one this process makes, and the ones an
//! earlier process left behind when it was killed before it could clean up.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const PREFIX: &str = "eidolon-browser-profile-";

/// A new, empty profile directory under `tmp`.
pub fn make_profile(tmp: &Path) -> std::io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = tmp.join(format!("{PREFIX}{}-{nanos:x}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Remove every profile directory under `tmp` older than `min_age` whose
/// Chrome is not still running. Returns the names removed. Errs toward
/// keeping a directory whenever it cannot tell.
pub fn sweep(tmp: &Path, min_age: Duration, now: SystemTime) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(tmp) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if !name.starts_with(PREFIX) || !path.is_dir() {
            continue;
        }
        let Ok(modified) = path.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if now.duration_since(modified).unwrap_or_default() < min_age {
            continue;
        }
        if locked(&path) {
            continue;
        }
        let _ = std::fs::remove_dir_all(&path);
        if !path.exists() {
            removed.push(name);
        }
    }
    removed
}

/// Chrome holds a profile with a `SingletonLock` symlink to `host-pid`.
/// Locked means that pid is still alive.
fn locked(dir: &Path) -> bool {
    let Ok(target) = std::fs::read_link(dir.join("SingletonLock")) else {
        return false;
    };
    let target = target.to_string_lossy();
    let pid = target.rsplit('-').next().unwrap_or_default();
    if pid.is_empty() || !pid.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    Path::new("/proc").join(pid).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "eidolon-browser-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn old_unlocked_profiles_go_and_everything_else_stays() {
        let tmp = scratch("sweep");
        let old = tmp.join(format!("{PREFIX}old"));
        std::fs::create_dir(&old).unwrap();
        std::fs::write(tmp.join(format!("{PREFIX}file")), b"x").unwrap();
        std::fs::create_dir(tmp.join("unrelated")).unwrap();

        let later = SystemTime::now() + Duration::from_secs(120);
        assert_eq!(
            sweep(&tmp, Duration::from_secs(60), later),
            vec![format!("{PREFIX}old")]
        );
        assert!(tmp.join(format!("{PREFIX}file")).exists());
        assert!(tmp.join("unrelated").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn young_profiles_stay() {
        let tmp = scratch("young");
        std::fs::create_dir(tmp.join(format!("{PREFIX}young"))).unwrap();
        assert!(sweep(&tmp, Duration::from_secs(60), SystemTime::now()).is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn a_profile_whose_chrome_is_alive_stays() {
        let tmp = scratch("locked");
        let live = tmp.join(format!("{PREFIX}live"));
        let dead = tmp.join(format!("{PREFIX}dead"));
        std::fs::create_dir(&live).unwrap();
        std::fs::create_dir(&dead).unwrap();
        std::os::unix::fs::symlink(
            format!("host-{}", std::process::id()),
            live.join("SingletonLock"),
        )
        .unwrap();
        std::os::unix::fs::symlink("host-999999999", dead.join("SingletonLock")).unwrap();

        let later = SystemTime::now() + Duration::from_secs(120);
        assert_eq!(
            sweep(&tmp, Duration::from_secs(60), later),
            vec![format!("{PREFIX}dead")]
        );
        assert!(live.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
