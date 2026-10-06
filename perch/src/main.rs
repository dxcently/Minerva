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
    /// door's own. Not with `--home`, which owns it (`<home>/sessions`).
    #[arg(long, value_name = "DIR", conflicts_with = "home")]
    sessions_dir: Option<PathBuf>,
    /// Run every door as `eidolon --config <FILE> web ...`. Without `--sessions-dir`, the
    /// kept logs are the file's own top-level `sessions_dir`, which must be absolute (eidolon does
    /// not expand `~` there); the door's default when it has none. The file must exist and is never
    /// written. Not with `--home`, which generates its own.
    #[arg(long, value_name = "FILE", conflicts_with = "home")]
    eidolon_config: Option<PathBuf>,
    /// The Minerva home (`~/Minerva`). The perch generates `<home>/config.toml` from the member's
    /// `~/.config/eidolon/config.toml` at every start (`sessions_dir` forced to `<home>/sessions`,
    /// `default_model` from `<home>/minerva.toml` or a default, when absent), runs every door with it
    /// and lists `<home>/sessions`. `<home>/scratch/<n>/` is where a new session that names no cwd
    /// works, and the home is one more allowed `--root`.
    #[arg(long, value_name = "DIR")]
    home: Option<PathBuf>,
    /// How long a login code lives, in milliseconds. For tests; the default is the contract.
    #[arg(long, value_name = "MS", default_value_t = 30_000, hide = true)]
    code_ttl_ms: u64,
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

    let member = perch::config::member_config_path();
    let resolved = perch::config::resolve(cli.home.as_deref(), cli.sessions_dir, cli.eidolon_config, &member)?;
    let mut roots = roots_or_home(cli.root)?;
    let scratch = resolved.home.as_ref().map(|home| {
        roots.push(home.clone());
        home.join("scratch")
    });
    // Under `--home`, eidolon's own dir is a second, read-only source (hub.md section 4); nothing in it is ever written.
    let other_sessions = resolved.home.as_ref().map(|_| perch::config::other_sessions_dir(&member));
    let modes = perch::mode::prepare(resolved.home.as_deref(), &member, &cli.eidolon)?;
    let options = perch::Options {
        bind: cli.bind,
        ui_dir: cli.ui_dir,
        roots,
        eidolon: cli.eidolon,
        sessions_dir: resolved.sessions_dir,
        eidolon_config: resolved.config,
        scratch,
        other_sessions,
        modes,
        code_ttl: std::time::Duration::from_millis(cli.code_ttl_ms),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_home_owns_the_config_and_the_sessions_dir_so_the_flags_that_name_them_conflict() {
        assert!(Cli::try_parse_from(["perch", "--home", "/h"]).is_ok());
        assert!(Cli::try_parse_from(["perch", "--eidolon-config", "/c.toml"]).is_ok());
        assert!(Cli::try_parse_from(["perch", "--home", "/h", "--eidolon-config", "/c.toml"]).is_err());
        assert!(Cli::try_parse_from(["perch", "--home", "/h", "--sessions-dir", "/s"]).is_err());
    }
}
