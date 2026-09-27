//! `perch`'s argv, its signals, and the runtime [`perch::run`] needs. Everything the
//! process actually does is in the library, so the tests can drive it in-process.
//!
//! Conceptually this is `minerva perch`; there is no `minerva` multiplexer binary yet,
//! so the binary is spelled `perch` on its own. H1b adds the flags a *door* needs —
//! `--root`, `--eidolon`, `--sessions-dir` — and deliberately not `--idle`: the idle
//! reaper is H2, and a flag that parses and does nothing is worse than one that fails.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::signal::unix::{signal, Signal, SignalKind};
use tokio_util::sync::CancellationToken;

/// One loopback process in front of several `eidolon web` doors.
///
/// H1a served the UI bundle and gated `/hub` and `/s`; H1b is what is behind those gates —
/// spawn a door, resume a kept log, stop a live door, list them — plus the perch's own
/// watcher stream to each door and the ordered stop that closes it before SIGTERM. The
/// proxy and door SSE to the browser are H1c. The token is minted per start into
/// `$XDG_RUNTIME_DIR/minerva/perch-<port>.token` and is never printed.
#[derive(Parser)]
#[command(name = "perch", version, about, long_about = None)]
struct Cli {
    /// Loopback only (`127.0.0.0/8` or `::1`). Port `0` picks one; the bound address
    /// is printed at boot.
    #[arg(long, default_value = "127.0.0.1:0")]
    bind: SocketAddr,
    /// Serve the browser UI from this directory on every path outside `/hub` and `/s`;
    /// `/` is `index.html`. These files need no token.
    #[arg(long, value_name = "PATH")]
    ui_dir: Option<PathBuf>,
    /// A directory a new session's cwd must sit under; repeat for more than one.
    /// Default: `$HOME`.
    #[arg(long, value_name = "DIR")]
    root: Vec<PathBuf>,
    /// The door binary to spawn, as `eidolon web`. Default: `eidolon` on `PATH`.
    #[arg(long, value_name = "PATH", default_value = "eidolon")]
    eidolon: PathBuf,
    /// Kept session logs, as `<id>.eid`. Default: `<data dir>/eidolon/sessions`, the
    /// door's own.
    #[arg(long, value_name = "DIR")]
    sessions_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // `tracing` writes to stderr, where a diagnostic cannot land in the middle of
    // stdout's two boot lines — a supervisor reads those off a pipe and a log line
    // among them is a token file path it would parse as the wrong thing.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    // Before `perch::run`, so the handler is installed before the boot lines are printed:
    // a supervisor reads them and may send SIGTERM straight away, and the default action
    // would kill the process with its 0600 token file still on disk. Registering the
    // signal and moving the `Signal` into the task is what makes that race impossible —
    // a SIGTERM that arrives before the task first runs is held in the stream, not lost.
    let term = signal(SignalKind::terminate()).context("listening for SIGTERM")?;
    let cancel = CancellationToken::new();
    tokio::spawn(cancel_on_signals(term, cancel.clone()));

    let options = perch::Options {
        bind: cli.bind,
        ui_dir: cli.ui_dir,
        roots: roots_or_home(cli.root)?,
        eidolon: cli.eidolon,
        sessions_dir: cli.sessions_dir.unwrap_or_else(default_sessions_dir),
    };
    perch::run(options, cancel).await
}

/// `--root`'s default is `$HOME` (hub.md Q6). Resolved here rather than as a clap default,
/// so the help can say `$HOME` and a missing one is a refusal that names the variable
/// instead of a root of `/`.
fn roots_or_home(asked: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    if !asked.is_empty() {
        return Ok(asked);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
        .context("no --root and no $HOME: a new session's cwd could not be checked against anything")?;
    Ok(vec![home])
}

/// `--sessions-dir`'s default is the door's own (`config.rs::sessions_dir`):
/// `data_local_dir()/eidolon/sessions`. `dirs::data_local_dir()` is `$XDG_DATA_HOME`, or
/// `$HOME/.local/share` — spelled out here because hub.md §11's dependency table has no
/// `dirs` in it, and one default is not worth a crate.
fn default_sessions_dir() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && !path.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."));
    data.join("eidolon").join("sessions")
}

/// SIGINT and SIGTERM cancel the same token: the keystroke and the supervisor's
/// `timeout(1)` mean the same thing here, and an unhandled SIGTERM would kill the
/// process with a token file still on disk.
///
/// The `Signal` is registered by `main`, not here: this task may not have been polled yet
/// when the first SIGTERM arrives.
///
/// H1b's shutdown — close the watcher to each door, SIGTERM the children, wait, drain —
/// hangs off this one token, inside `serve::run`; this function still only cancels it.
async fn cancel_on_signals(mut term: Signal, cancel: CancellationToken) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    tracing::info!("stopping");
    cancel.cancel();
}
