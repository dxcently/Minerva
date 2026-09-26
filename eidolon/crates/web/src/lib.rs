//! The web door: `eidolon web` serves one session's event stream and its
//! operator verbs over plain HTTP/1.1 on loopback, so any browser can draw
//! the transcript and type into the turn. Four API routes, plus the UI files
//! `--ui-dir` names:
//!
//! ```text
//!   GET  /api/events   hello, the branch replayed, every pending ask,
//!                      caught-up, then the bus live — and, when this
//!                      session quiesced, one last `goodbye` frame before
//!                      the stream ends (see `crate::wake`)
//!   POST /api/say      {"text": "...", "mode": "send"|"steer"}
//!   POST /api/cancel   the running turn, or 409 when the session is idle
//!   POST /api/answer   {"ask_id": N, "answer": "..."} for a pending ask —
//!                      204, or 409 when nothing is pending under that id,
//!                      or 422 when the answer is not one that ask offers
//!                      (it stays pending)
//!   GET  /other        with --ui-dir, a file from that directory: `/` is
//!   HEAD               index.html, a directory names nothing, and any other
//!                      method is 405. Without the flag, 404.
//! ```
//!
//! **The gates, in the order a request meets them.**
//!
//! 1. The token, on all of `/api` (the bare `/api` too), before anything else,
//!    so an unauthenticated caller cannot map the routes by status.
//!    `Authorization: Bearer`, or `?token=` on `GET /api/events` alone because
//!    `EventSource` cannot set a header. 401 with an empty body; constant-time.
//! 2. `Host` against the bound address, on the POSTs and static GET/HEAD:
//!    DNS rebinding. `/api/events` needs none; a cross-site page has no token.
//! 3. POST bodies over [`serve::MAX_BODY`] are 413, never buffered past it.
//!
//! The bind is loopback-only ([`loopback_only`]) and no flag widens it.
//!
//! **Static files need no token**: the page loads before it has one. The
//! launcher hands it over in the URL fragment, which a browser never sends;
//! the door never puts it in a served file or prints it. Path rules are in
//! `crate::files`.
//!
//! **The token** is 256 random bits per start, written `0600` to
//! `<runtime dir>/web-<port>.token`; [`run`] refuses to start unless the file
//! is owner-only, and removes it on clean shutdown.
//!
//! [`PROTOCOL`] rides `hello`. A client must ignore frame types and fields it
//! does not know, which is what keeps an additive change additive; a breaking
//! one bumps it.

pub mod driver;
pub mod files;
pub mod serve;
pub mod stream;
pub mod user;
pub mod wake;
pub mod wire;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use eidolon_core::agent::Agent;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub use user::WebUser;

/// The wire version, carried by `hello`. See the module doc.
pub const PROTOCOL: u32 = 1;

/// What `crates/cli`'s `Cmd::Web` arm hands to [`run`].
pub struct WebOptions {
    pub bind: SocketAddr,
    pub cwd: PathBuf,
    /// Where the token file goes; the CLI passes `eidolon_swarm::Presence::root`.
    /// Not read from the environment, so a test can name its own.
    pub runtime_dir: PathBuf,
    /// `--ui-dir`, served outside `/api`; see [`files::Static`].
    pub ui_dir: Option<PathBuf>,
    pub yolo: eidolon_core::yolo::Switch,
    /// Rings on peer mail or a quiesce request. `None` when the swarm is off:
    /// only the waking is lost; the inbox still drains at turn boundaries.
    pub doorbell: Option<tokio::sync::mpsc::UnboundedReceiver<()>>,
    /// This session's roster entry, when the swarm is on.
    pub presence: Option<Arc<dyn driver::Presence>>,
}

/// Refuses anything but `127.0.0.0/8` and `::1`: the token is a file on this
/// box, not a credential for a network.
pub fn loopback_only(bind: SocketAddr) -> Result<()> {
    if bind.ip().is_loopback() {
        return Ok(());
    }
    anyhow::bail!("refusing to bind {bind}: the web door serves loopback only (127.0.0.0/8 or ::1)")
}

/// Named for the port bound, so two doors on one box never share a file.
fn token_path(runtime_dir: &Path, port: u16) -> PathBuf {
    runtime_dir.join(format!("web-{port}.token"))
}

/// Split from [`write_token`] so a test can reach it: the write always comes
/// out `0600` on a filesystem that keeps its promises.
fn owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(path)
        .with_context(|| format!("checking {}", path.display()))?
        .permissions()
        .mode();
    anyhow::ensure!(mode & 0o077 == 0, "{} came out {mode:o}, not owner-only", path.display());
    Ok(())
}

/// The mode is checked after the write, not assumed, as
/// `eidolon_core::ipc::bind_private` does for a socket.
fn write_token(path: &Path, token: &str) -> Result<()> {
    harnox::fs::write_atomic_0600(path, token.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    owner_only(path)
}

/// Bind, write the token file, print the URL and the file's path (never the
/// token), and serve until `cancel`.
///
/// The first frame never waits on the network, as the TUI's first draw does
/// not: the replay walks a session already in memory.
pub async fn run(agent: Arc<Agent>, user: Arc<WebUser>, mut opts: WebOptions, cancel: CancellationToken) -> Result<()> {
    loopback_only(opts.bind)?;
    let ui = opts.ui_dir.as_deref().map(files::Static::new).transpose()?;
    let listener = TcpListener::bind(opts.bind).await.with_context(|| format!("bind {}", opts.bind))?;
    let bound = listener.local_addr().context("read the bound address")?;

    // After the bind: `--bind 127.0.0.1:0` has no port to name the file for until now.
    let token = harnox::crypto::random_token();
    let token_file = token_path(&opts.runtime_dir, bound.port());
    write_token(&token_file, &token)?;

    println!("listening on http://{bound}/");
    println!("token file: {}", token_file.display());
    eidolon_core::boot::mark("listen");

    let driver = driver::Handle::new(agent.clone(), opts.presence.clone());
    // Before the resume, so the wake task sees the resumed turn end and
    // collects mail that landed during it.
    let ended = driver.subscribe_ended();
    // Result unused: nothing is accepting requests yet, so nothing can race it.
    driver.finish_unsettled().await;

    let doorbell = opts.doorbell.take();
    let watched = agent.clone();
    let door = driver.clone();
    let presence = opts.presence.clone();
    let close = cancel.clone();
    tokio::spawn(wake::watch(watched, door, presence, doorbell, ended, close));

    let ctx = serve::Ctx {
        agent,
        user,
        driver,
        bound,
        cwd: opts.cwd.display().to_string(),
        yolo: opts.yolo.armed(),
        token: Arc::from(token.as_str()),
        ui,
    };
    let served = serve::run(ctx, listener, cancel).await;
    if let Err(e) = std::fs::remove_file(&token_file) {
        tracing::debug!(error = %e, path = %token_file.display(), "web: token file not removed");
    }
    served
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_addresses_are_served() {
        for ok in ["127.0.0.1:4477", "127.5.5.5:1", "[::1]:4477"] {
            assert!(loopback_only(ok.parse().unwrap()).is_ok(), "{ok} is loopback");
        }
        for no in ["0.0.0.0:4477", "[::]:4477", "192.168.1.5:4477", "10.0.0.1:80"] {
            let addr: SocketAddr = no.parse().unwrap();
            let err = loopback_only(addr).unwrap_err().to_string();
            assert!(err.contains(&addr.to_string()), "the refusal names the address: {err}");
        }
    }

    #[test]
    fn the_token_file_is_named_for_the_port_in_the_runtime_dir() {
        let path = token_path(Path::new("/run/user/1000/eidolon"), 4477);
        assert_eq!(path, Path::new("/run/user/1000/eidolon/web-4477.token"));
    }

    #[test]
    fn a_written_token_reads_back_at_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        // The write makes the missing directory, as on a first run.
        let path = dir.path().join("nested").join("web-1.token");
        write_token(&path, "s3cret").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "s3cret");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0, "owner-only");
        owner_only(&path).unwrap();
    }

    #[test]
    fn a_token_file_that_is_not_owner_only_fails_the_check() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-1.token");
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
        let path = locked.join("web-1.token");
        let err = write_token(&path, "s3cret").unwrap_err();
        assert!(err.to_string().contains("writing"), "the refusal names the write it could not make: {err}");
        assert!(!path.exists(), "no token file half-way there");
        // So `TempDir` can clean up.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
