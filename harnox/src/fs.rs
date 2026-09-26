//! Writing secret-bearing files: atomic, `0600`, never a partial state on disk.

use std::io::Write as _;
use std::path::Path;

/// Temp file (`0600`) → fsync → rename. The parent directory is created if
/// missing. A crash mid-write leaves either the old file or the new one, never
/// a torn mix, and the file is never observable with looser permissions.
pub fn write_atomic_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    // Each writer owns its temporary inode. A shared .tmp path lets concurrent
    // writers truncate one another (even after another writer has renamed it).
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `chmod 600` on a freshly written secret file that could not go through
/// [`write_atomic_0600`] (an env file edited in place, say).
pub fn chmod_600(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Upsert the `KEY=value` line into an env-file body, preserving every other
/// line. Any prior `KEY=` line (there should be at most one) is dropped and
/// the new value appended; trailing blank lines are trimmed so they don't
/// accrete across runs. Pure, so the preserve-and-replace behaviour is
/// unit-testable.
pub fn upsert_env_line(existing: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key}=");
    let mut out: Vec<&str> =
        existing.lines().filter(|l| !l.trim_start().starts_with(&prefix)).collect();
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut body = out.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    body.push_str(&format!("{key}={value}\n"));
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_is_0600_and_replaces_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::testutil::tempdir("fs");
        let path = dir.join("nested").join("secret.bin");
        write_atomic_0600(&path, b"one").unwrap();
        write_atomic_0600(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "must be 0600");
        assert!(!path.with_extension("tmp").exists(), "temp file must not linger");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_replaces_an_existing_line_and_keeps_the_rest() {
        let existing = "PUBLIC_URL=https://x\nTOKEN=old\nOTHER=1\n";
        let out = upsert_env_line(existing, "TOKEN", "new");
        assert!(out.contains("PUBLIC_URL=https://x"), "keeps unrelated lines: {out}");
        assert!(out.contains("OTHER=1"), "keeps unrelated lines: {out}");
        assert!(out.contains("TOKEN=new"), "writes the new value: {out}");
        assert!(!out.contains("TOKEN=old"), "drops the old value: {out}");
        assert_eq!(out.matches("TOKEN=").count(), 1);
    }

    #[test]
    fn upsert_into_an_empty_file_just_writes_the_line() {
        assert_eq!(upsert_env_line("", "TOKEN", "tok"), "TOKEN=tok\n");
    }
    #[test]
    fn concurrent_atomic_writers_never_mix_payloads() {
        let dir = crate::testutil::tempdir("atomic-concurrent");
        let path = dir.join("state");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        std::thread::scope(|scope| {
            for i in 0..16u8 {
                let path = &path;
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..16 {
                        write_atomic_0600(path, &vec![i; 64 * 1024]).unwrap();
                        let body = std::fs::read(path).unwrap();
                        assert_eq!(body.len(), 64 * 1024);
                        assert!(body.iter().all(|b| *b == body[0]));
                    }
                });
            }
        });
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

}
