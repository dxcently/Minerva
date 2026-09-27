//! `perch`'s argv, its signals, and the runtime [`perch::run`] needs. Everything the
//! process actually does is in the library, so the tests can drive it in-process.
//!
//! Conceptually this is `minerva perch`; there is no `minerva` multiplexer binary yet,
//! so the binary is spelled `perch` on its own. The other flags the design names —
//! `--root`, `--idle`, `--eidolon`, `--sessions-dir` — belong to H1b/H1c and are not
//! accepted here rather than accepted and ignored: a flag that parses and does nothing
//! is worse than one that fails.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::signal::unix::{signal, Signal, SignalKind};
use tokio_util::sync::CancellationToken;

/// One loopback process in front of several `eidolon web` doors.
///
/// H1a serves the UI bundle and gates `/hub` and `/s`; spawn, proxy and the streams
/// are not built yet. The token is minted per start into
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
    perch::run(perch::Options { bind: cli.bind, ui_dir: cli.ui_dir }, cancel).await
}

/// SIGINT and SIGTERM cancel the same token: the keystroke and the supervisor's
/// `timeout(1)` mean the same thing here, and an unhandled SIGTERM would kill the
/// process with a token file still on disk.
///
/// The `Signal` is registered by `main`, not here: this task may not have been polled yet
/// when the first SIGTERM arrives.
///
/// H1b extends the shutdown this token drives — close the streams, signal the
/// children, drain — but not this function: the token is what every step hangs off.
async fn cancel_on_signals(mut term: Signal, cancel: CancellationToken) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    tracing::info!("stopping");
    cancel.cancel();
}
