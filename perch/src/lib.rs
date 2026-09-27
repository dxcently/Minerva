//! The perch: one loopback HTTP server that will front several `eidolon web` doors
//! so the browser UI (`webui/term`) can tile them without touching upstream.
//!
//! This is slice **H1a**, and H1a is only the front door:
//!
//! ```text
//!   perch [--bind 127.0.0.1:0] [--ui-dir DIR]
//!
//!   /            the UI bundle from --ui-dir, no token, `Host` checked
//!   /hub/...     token-gated, no handler yet  -> 404 (H1b: sessions, spawn, stop)
//!   /s/...       token-gated, no handler yet  -> 404 (H1c: the proxy, SSE)
//! ```
//!
//! So an **unauthenticated** request to `/hub` or `/s` is a 401 and an authenticated
//! one is a 404: the route does not exist yet, but the gate in front of it does. Being
//! able to tell those two apart is the whole point of this slice.
//!
//! **The gates, in the order a request meets them** (hub.md §10.2). 0: this bind,
//! refused unless it is loopback, before a listener exists. 1: the token, on `/hub`,
//! `/s` and everything under them, before the path is parsed. 2: `Host` against the
//! bound authority, on every request. 3: `Sec-Fetch-Site`, when present. 4: method and
//! path shape. 5: the body cap. The refusals are 401, 403, 404/405 and 413; they are
//! in [`serve`] beside the one `match` that routes, so no route can skip one.
//!
//! **The token** is 256 random bits per start, written `0600` to
//! `$XDG_RUNTIME_DIR/minerva/perch-<port>.token`. It is never in argv, never in the
//! environment, never in a URL (the gate reads `Authorization: Bearer` only), never in
//! a log and never on stdout — [`run`] prints the two boot lines and no third. It is
//! removed on clean shutdown, because the file is the page's only credential and it
//! must not outlive the listener that honoured it.
//!
//! **Runtime dir.** Unlike the door, which falls back to the shared temp dir
//! (`swarm::Presence::root`) and ignores a failed `chmod`, the perch refuses to start
//! without `$XDG_RUNTIME_DIR` and refuses a directory that is not ours and `0700`: a
//! temp-dir fallback lets another user pre-create the directory the token goes in.
//!
//! Unix only, like the door: it needs `0600` files, an XDG runtime dir and signals.

#[cfg(not(unix))]
compile_error!("the perch is Unix only: it needs a 0600 token file, $XDG_RUNTIME_DIR and SIGTERM");

pub mod files;
pub mod gate;
pub mod serve;

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// What `main` hands to [`run`]. No field is a secret, and none is an argv string
/// that could carry one.
pub struct Options {
    pub bind: SocketAddr,
    /// `--ui-dir`, served outside `/hub` and `/s`; see [`files::Static`]. `None`
    /// serves no statics, as the door without the flag does.
    pub ui_dir: Option<PathBuf>,
}

/// The perch's own directory under `$XDG_RUNTIME_DIR`, so its token sits beside the
/// doors' `eidolon/` directory and never inside a door's.
const RUNTIME_SUBDIR: &str = "minerva";

/// Bind, mint the token, print the URL and the token file's path (never the token),
/// and serve until `cancel`.
///
/// The order is deliberate: every refusal that can be made before the process
/// announces itself is made first, so a supervisor never reads a boot line from a
/// perch that then dies.
pub async fn run(opts: Options, cancel: CancellationToken) -> Result<()> {
    gate::loopback_only(opts.bind)?;
    // The secret's home, checked before the bind and long before anything is minted
    // into it: a token written into a directory another user could have made is not
    // a secret.
    let runtime = runtime_dir()?;
    // A UI dir that cannot be served does not come up, as the door's does not.
    let ui = opts.ui_dir.as_deref().map(files::Static::new).transpose()?;
    let listener = TcpListener::bind(opts.bind).await.with_context(|| format!("bind {}", opts.bind))?;
    let bound = listener.local_addr().context("read the bound address")?;

    // After the bind: `--bind 127.0.0.1:0` has no port to name the file for until now.
    let token = harnox::crypto::random_token();
    let token_file = token_path(&runtime, bound.port())?;
    write_token(&token_file, &token)?;

    // Exactly two lines, and never the token. A supervisor reads these off the pipe;
    // `tracing` writes to stderr, nowhere near them.
    println!("listening on http://{bound}/");
    println!("token file: {}", token_file.display());

    let ctx = serve::Ctx { bound, token: Arc::from(token.as_str()), ui };
    let served = serve::run(ctx, listener, cancel).await;

    // Last, after the drain. H1b's shutdown — close the watcher and pane streams,
    // SIGTERM each child, wait, then drain — happens *inside* `serve::run`'s cancel
    // arm, ahead of this line, so a door can never outlive the token that let the
    // page reach it.
    if let Err(e) = std::fs::remove_file(&token_file) {
        tracing::debug!(error = %e, path = %token_file.display(), "perch: token file not removed");
    }
    served
}

/// `$XDG_RUNTIME_DIR`, which the perch requires rather than falling back to the
/// shared temp dir the door uses. The value is read here, once, so every check
/// downstream is on the same path.
fn runtime_dir() -> Result<PathBuf> {
    runtime_dir_from(std::env::var_os("XDG_RUNTIME_DIR"))
}

/// Split from [`runtime_dir`] so a test can hand in a value instead of mutating the
/// process environment, which every other test in the binary shares.
fn runtime_dir_from(value: Option<OsString>) -> Result<PathBuf> {
    let dir = value
        .map(PathBuf::from)
        .context("refusing to start: $XDG_RUNTIME_DIR is not set, and the token file must not fall back to a shared directory")?;
    owner_dir(&dir)?;
    Ok(dir)
}

/// Owner and mode of a directory we are about to trust with a secret. `0700` or
/// tighter is the requirement; `metadata` follows a symlink, which is what we want —
/// we are checking the directory the token will actually land in.
fn owner_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(dir).with_context(|| format!("checking runtime dir {}", dir.display()))?;
    anyhow::ensure!(meta.is_dir(), "runtime dir {} is not a directory", dir.display());
    let ours = unsafe { libc::getuid() };
    owned_by(meta.uid(), ours).with_context(|| format!("checking runtime dir {}", dir.display()))?;
    let mode = kept_mode(&meta);
    anyhow::ensure!(mode & 0o077 == 0, "runtime dir {} is mode {mode:o}, not owner-only", dir.display());
    Ok(())
}

/// The uid half of [`owner_dir`], split out so it can be tested as a comparison: `chown`
/// is the only way to stage a real directory that is ours in mode but another uid's, and
/// that needs root. What the check is, is this comparison, so this is what a test can
/// make fail.
fn owned_by(meta_uid: u32, ours: u32) -> Result<()> {
    anyhow::ensure!(meta_uid == ours, "owned by uid {meta_uid}, not ours ({ours})");
    Ok(())
}

/// Named for the port bound, so two perches on one box never share a file, and under
/// its own subdirectory so a perch token is never mistakable for a door's.
fn token_path(runtime: &Path, port: u16) -> Result<PathBuf> {
    let dir = runtime.join(RUNTIME_SUBDIR);
    // `write_atomic_0600` will `create_dir_all` this anyway, but at the umask's mode
    // (`0755` on the usual box). The token inside is `0600` either way; the directory
    // is tightened first so that nothing but its owner can so much as list it.
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    set_owner_only(&dir)?;
    Ok(dir.join(format!("perch-{port}.token")))
}

/// `0700`, checked after the call rather than assumed: on a filesystem that does not
/// keep permissions (a 9p/drvfs mount, say) `chmod` succeeds and changes nothing, and
/// a perch whose secrets directory is world-readable should refuse to start, not
/// serve. The door's version of this is `let _ = set_permissions(...)`, which is
/// exactly the silence hub.md §2 rejects.
fn set_owner_only(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).with_context(|| format!("chmod 0700 {}", dir.display()))?;
    owner_only(dir)
}

/// Split from [`write_token`] so a test can reach it: the write always comes out
/// `0600` on a filesystem that keeps its promises.
fn owner_only(path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path).with_context(|| format!("checking {}", path.display()))?;
    let mode = kept_mode(&meta);
    anyhow::ensure!(mode & 0o077 == 0, "{} came out {mode:o}, not owner-only", path.display());
    Ok(())
}

/// The permission bits alone. `st_mode` carries the file type too — `0755` reads as
/// `40755` for a directory — and a refusal that printed that would name a number
/// nobody can `chmod`.
fn kept_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o777
}

/// The mode is checked after the write, not assumed. A file that is not owner-only
/// refuses the start rather than being chmod'ed into shape: a token that was readable
/// for however long the write took has already leaked.
fn write_token(path: &Path, token: &str) -> Result<()> {
    harnox::fs::write_atomic_0600(path, token.as_bytes()).with_context(|| format!("writing {}", path.display()))?;
    owner_only(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tempfile::tempdir` makes a directory at the umask's mode — `0755` on this box —
    /// and a runtime dir the perch accepts is `0700`. Every test that means to pass the
    /// check has to say so first.
    fn own(dir: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn the_token_file_is_named_for_the_port_in_the_runtime_dir() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        own(dir.path());
        let path = token_path(dir.path(), 4477).unwrap();
        assert_eq!(path, dir.path().join("minerva").join("perch-4477.token"));
        // …and the directory the token will be written into is already owner-only, which
        // is what `token_path` calls `set_owner_only` for.
        let mode = std::fs::metadata(dir.path().join("minerva")).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the token's directory came out {:o}, not owner-only", mode & 0o777);
    }

    #[test]
    fn the_runtime_dir_must_be_named_or_the_start_is_refused() {
        let err = runtime_dir_from(None).unwrap_err().to_string();
        assert!(err.contains("XDG_RUNTIME_DIR"), "the refusal names the variable: {err}");
    }

    #[test]
    fn the_runtime_dir_must_be_a_directory_that_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        own(dir.path());
        runtime_dir_from(Some(dir.path().as_os_str().to_os_string())).unwrap();

        // A file named as the runtime dir.
        let file = dir.path().join("a-file");
        std::fs::write(&file, "not a directory").unwrap();
        let err = runtime_dir_from(Some(file.as_os_str().to_os_string())).unwrap_err().to_string();
        assert!(err.contains("not a directory"), "{err}");

        // A path that is not there at all: the door's temp-dir fallback would have
        // kept this going; the perch will not.
        let missing = dir.path().join("nowhere");
        let err = runtime_dir_from(Some(missing.as_os_str().to_os_string())).unwrap_err().to_string();
        assert!(err.contains(&missing.display().to_string()), "the refusal names the path: {err}");

        // A directory another user could read: group or other, any bit.
        for loose in [0o755, 0o750, 0o701, 0o777] {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(loose)).unwrap();
            let err = runtime_dir_from(Some(dir.path().as_os_str().to_os_string())).unwrap_err().to_string();
            assert!(err.contains(&format!("{loose:o}")), "the refusal names the mode it found: {err}");
            assert!(err.contains(&dir.path().display().to_string()), "…and the path: {err}");
        }
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// The uid comparison, made to fail. `chown` is the only way to make a real directory
    /// ours in mode but another uid's, and that needs root; what the check *is* is the
    /// comparison, so that is what is tested here. That `owner_dir` makes the comparison
    /// at all is covered by the mode tests above, which go through it.
    #[test]
    fn the_runtime_dir_must_be_ours() {
        let ours = unsafe { libc::getuid() };
        owned_by(ours, ours).unwrap();
        let err = owned_by(ours + 1, ours).unwrap_err().to_string();
        assert!(err.contains("not ours"), "the refusal says whose it is not: {err}");
        assert!(err.contains(&(ours + 1).to_string()), "the refusal names the uid it found: {err}");
        assert!(err.contains(&ours.to_string()), "…and the uid of ours it compared with: {err}");
    }

    #[test]
    fn a_written_token_reads_back_at_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        // The write makes the missing directory, as on a first run.
        let path = dir.path().join("nested").join("perch-1.token");
        write_token(&path, "s3cret").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "s3cret");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0, "owner-only");
        owner_only(&path).unwrap();
    }

    #[test]
    fn a_token_file_that_is_not_owner_only_fails_the_check() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("perch-1.token");
        std::fs::write(&path, "s3cret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = owner_only(&path).unwrap_err().to_string();
        assert!(err.contains("644"), "the refusal names the mode it found: {err}");
        assert!(err.contains(&path.display().to_string()), "…and the file: {err}");
        // The rename replaces it with a fresh `0600` file.
        write_token(&path, "s3cret").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
    }

    #[test]
    fn write_token_fails_when_the_file_cannot_be_created() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let path = locked.join("perch-1.token");
        let err = write_token(&path, "s3cret").unwrap_err();
        assert!(err.to_string().contains("writing"), "the refusal names the write it could not make: {err}");
        assert!(!path.exists(), "no token file half-way there");
        // So `TempDir` can clean up.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// A directory is tightened, and a `chmod` that cannot be made refuses the start
    /// instead of leaving the directory as it found it. What the re-read inside
    /// [`set_owner_only`] catches — a filesystem where `chmod` succeeds and changes
    /// nothing — cannot be staged on ext4; the re-read itself is proven by
    /// [`owner_only`]'s test above, which is the function it calls.
    #[test]
    fn a_directory_we_cannot_tighten_refuses_the_start() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o755)).unwrap();
        set_owner_only(&nested).unwrap();
        assert_eq!(std::fs::metadata(&nested).unwrap().permissions().mode() & 0o077, 0, "0755 -> 0700");

        let missing = dir.path().join("nowhere");
        let err = set_owner_only(&missing).unwrap_err().to_string();
        assert!(err.contains(&missing.display().to_string()), "the refusal names the path: {err}");
        assert!(err.contains("chmod 0700"), "…and the call it could not make: {err}");
    }
}
