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
    /// The eidolon-plugins collection (`<dir>/jev`, `<dir>/browser`, ...): where a graph run finds the jev workflows.
    #[arg(long, value_name = "DIR")]
    plugins_dir: Option<PathBuf>,
    /// The graphs a page may start as a run, comma separated or repeated. Bare names only: each is joined into a path.
    #[arg(long, value_name = "NAME", value_delimiter = ',', default_value = "wiki-hop")]
    run_graphs: Vec<String>,
    /// jev's health URL, probed before a run starts (`http://127.0.0.1:<port>/health`).
    #[arg(long, value_name = "URL")]
    jev_health: Option<String>,
    /// The browser service's health URL, probed before a run starts.
    #[arg(long, value_name = "URL")]
    browser_health: Option<String>,
    /// The `aoide` binary, as an absolute path. Default: `~/.local/bin/aoide` when it is executable.
    /// Never found on `PATH`, which finds a stale one first.
    #[arg(long, value_name = "PATH")]
    aoide: Option<PathBuf>,
    /// Aoide's home (`~/.aoide`), which the mesh view reads and never writes.
    #[arg(long, value_name = "DIR")]
    aoide_home: Option<PathBuf>,
    /// Aoide's daemon socket, which is only ever asked to `ping`.
    #[arg(long, value_name = "PATH")]
    aoide_daemon_socket: Option<PathBuf>,
    /// Aoide's audit log, which the mesh view tails.
    #[arg(long, value_name = "FILE")]
    aoide_events: Option<PathBuf>,
    /// The `minerva` binary the install routes ask, as an absolute path. Default: the one beside this perch, when there is one.
    #[arg(long, value_name = "PATH")]
    minerva: Option<PathBuf>,
    /// Write the terminal tools' scoped token here (`0600`, rotated every start, removed on exit). Off unless given.
    #[arg(long, value_name = "FILE")]
    term_token_file: Option<PathBuf>,
    /// Write the mail tool's scoped token here (`0600`, rotated every start, removed on exit). Off unless given.
    #[arg(long, value_name = "FILE")]
    mail_token_file: Option<PathBuf>,
    /// Write this perch's URL here for the agent tools (removed on exit). Off unless given.
    #[arg(long, value_name = "FILE")]
    agent_url_file: Option<PathBuf>,
    /// A test setting, `k=v`, repeatable (`doors=off` refuses every door start). For tests and demos; unknown keys are ignored.
    #[arg(long = "knob", value_name = "K=V", hide = true)]
    knob: Vec<String>,
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

    // The flags that can be refused on their own are, before `--home` is created and a config generated for a start that will not happen.
    for graph in &cli.run_graphs {
        anyhow::ensure!(perch::graph_name_ok(graph), "--run-graphs {graph:?} is not a bare graph name (letters, digits, - and _)");
    }
    let knobs = perch::Knobs::parse(&cli.knob)?;
    absolute_binary("--aoide", cli.aoide.as_deref())?;
    absolute_binary("--minerva", cli.minerva.as_deref())?;

    let member = perch::config::member_config_path();
    let resolved = perch::config::resolve(cli.home.as_deref(), cli.sessions_dir, cli.eidolon_config, &member)?;
    let mut roots = roots_or_home(cli.root)?;
    let scratch = resolved.home.as_ref().map(|home| {
        roots.push(home.clone());
        home.join("scratch")
    });
    // Under `--home`, eidolon's own dir is a second, read-only source (hub.md section 4); nothing in it is ever written.
    let other_sessions = resolved.home.as_ref().map(|_| perch::config::other_sessions_dir(&member));
    let files = perch::mode::AgentFiles { term: cli.term_token_file.as_deref(), mail: cli.mail_token_file.as_deref() };
    let modes = perch::mode::prepare_with(resolved.home.as_deref(), &member, &cli.eidolon, files)?;
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
        plugins_dir: cli.plugins_dir,
        run_graphs: cli.run_graphs,
        jev_health: cli.jev_health,
        browser_health: cli.browser_health,
        aoide: aoide_or_default(cli.aoide, std::env::var_os("HOME")),
        aoide_home: cli.aoide_home,
        aoide_daemon_socket: cli.aoide_daemon_socket,
        aoide_events: cli.aoide_events,
        minerva: cli.minerva,
        term_token_file: cli.term_token_file,
        mail_token_file: cli.mail_token_file,
        agent_url_file: cli.agent_url_file,
        knobs,
    };
    perch::run(options, cancel).await
}

/// `--aoide` as given, else `$HOME/.local/bin/aoide` when that is an executable file, else nothing. Never a `PATH` lookup: `PATH` finds
/// a stale `aoide` first, and a binary the operator did not name is not one the perch should run. An explicit path is not checked here;
/// the runner that uses it says what is wrong with it. `home` is the value of `$HOME`, handed in so a test need not set it.
fn aoide_or_default(asked: Option<PathBuf>, home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    if asked.is_some() {
        return asked;
    }
    let home = PathBuf::from(home.filter(|home| !home.is_empty())?);
    let default = home.join(".local").join("bin").join("aoide");
    // A `$HOME` that is not absolute would make this a path relative to wherever the perch was started.
    (default.is_absolute() && executable_file(&default)).then_some(default)
}

/// `--aoide` and `--minerva` name a binary the perch will run, and are an absolute path or not given: a bare `aoide` handed to
/// `Command` is looked up on `PATH`, which is the lookup this flag exists to avoid, and a relative `./aoide` is a different binary in
/// every directory the perch could be started from. Refused at the start, before anything is created, and said in a sentence.
fn absolute_binary(flag: &str, asked: Option<&std::path::Path>) -> Result<()> {
    if let Some(path) = asked {
        anyhow::ensure!(
            path.is_absolute(),
            "{flag} {} is not an absolute path (a bare name would be looked up on PATH, and the perch never runs a binary it found there)",
            path.display()
        );
    }
    Ok(())
}

/// A regular file (a link is followed) with an execute bit.
fn executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
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

    #[test]
    fn the_slice_flags_parse_and_default_to_nothing_but_the_one_graph() {
        let bare = Cli::try_parse_from(["perch"]).unwrap();
        assert_eq!(bare.run_graphs, ["wiki-hop"], "the default graph");
        assert!(bare.plugins_dir.is_none() && bare.jev_health.is_none() && bare.browser_health.is_none());
        assert!(bare.aoide.is_none() && bare.aoide_home.is_none() && bare.aoide_daemon_socket.is_none() && bare.aoide_events.is_none());
        assert!(bare.minerva.is_none() && bare.term_token_file.is_none() && bare.mail_token_file.is_none() && bare.agent_url_file.is_none());
        assert!(bare.knob.is_empty());

        let all = Cli::try_parse_from([
            "perch", "--plugins-dir", "/p", "--run-graphs", "a,b", "--jev-health", "http://127.0.0.1:8091/health", "--browser-health",
            "http://127.0.0.1:8090/health", "--aoide", "/bin/aoide", "--aoide-home", "/h/.aoide", "--aoide-daemon-socket", "/s.sock",
            "--aoide-events", "/e.log", "--minerva", "/bin/minerva", "--term-token-file", "/t", "--mail-token-file", "/m",
            "--agent-url-file", "/u", "--knob", "doors=off", "--knob", "term_ring=4096",
        ])
        .unwrap();
        assert_eq!(all.run_graphs, ["a", "b"], "given graphs replace the default");
        assert_eq!(all.plugins_dir.as_deref(), Some(std::path::Path::new("/p")));
        assert_eq!(all.aoide.as_deref(), Some(std::path::Path::new("/bin/aoide")));
        assert_eq!(all.knob, ["doors=off", "term_ring=4096"], "the knob repeats");
        // Repeated, not comma separated, is the same list.
        let again = Cli::try_parse_from(["perch", "--run-graphs", "a", "--run-graphs", "b"]).unwrap();
        assert_eq!(again.run_graphs, ["a", "b"]);
    }

    /// Never a `PATH` lookup, and never a binary that is not executable. Mutation: look the name up on `PATH`, or skip the mode check,
    /// and the matching case goes red.
    #[test]
    fn the_default_aoide_is_the_one_in_the_home_when_it_is_executable_and_nothing_else() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = tempfile::tempdir().unwrap();
        let home_os = || Some(home.path().as_os_str().to_os_string());
        assert_eq!(aoide_or_default(None, home_os()), None, "no file");
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let aoide = bin.join("aoide");
        std::fs::write(&aoide, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&aoide, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(aoide_or_default(None, home_os()), None, "a file that cannot run is not a default");
        std::fs::set_permissions(&aoide, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(aoide_or_default(None, home_os()), Some(aoide.clone()));
        // A directory by that name is not it either.
        std::fs::remove_file(&aoide).unwrap();
        std::fs::create_dir(&aoide).unwrap();
        assert_eq!(aoide_or_default(None, home_os()), None);
        // An explicit path is taken as given, existing or not; no home and an empty home find nothing.
        assert_eq!(aoide_or_default(Some(PathBuf::from("/nowhere/aoide")), home_os()), Some(PathBuf::from("/nowhere/aoide")));
        assert_eq!(aoide_or_default(None, None), None);
        assert_eq!(aoide_or_default(None, Some(std::ffi::OsString::new())), None);
    }

    /// No `PATH` lookup, ever: a name that is not an absolute path is refused at the start, for both binaries the perch is told of.
    /// Mutation: take a relative path (or only refuse the empty one) and the cases below go red.
    #[test]
    fn a_binary_the_perch_is_told_of_is_an_absolute_path_or_it_is_refused() {
        for flag in ["--aoide", "--minerva"] {
            assert!(absolute_binary(flag, None).is_ok(), "not given is fine");
            for ok in ["/bin/aoide", "/home/x/.local/bin/aoide", "/opt/x/../aoide", "/aoide"] {
                assert!(absolute_binary(flag, Some(std::path::Path::new(ok))).is_ok(), "{flag} {ok}");
            }
            for bad in ["aoide", "minerva", "./aoide", "../bin/aoide", "bin/aoide", "", "~/bin/aoide", " /bin/aoide", "$HOME/bin/aoide"] {
                let why = absolute_binary(flag, Some(std::path::Path::new(bad))).unwrap_err().to_string();
                assert!(why.contains(flag) && why.contains("absolute"), "{flag} {bad:?}: {why}");
            }
        }
        // Through the parser, which is the path an operator takes.
        let cli = Cli::try_parse_from(["perch", "--aoide", "aoide", "--minerva", "/bin/minerva"]).unwrap();
        assert!(absolute_binary("--aoide", cli.aoide.as_deref()).is_err());
        assert!(absolute_binary("--minerva", cli.minerva.as_deref()).is_ok());
        // …and the default one is not found through a `$HOME` that is not absolute either.
        assert_eq!(aoide_or_default(None, Some("relative/home".into())), None);
    }
}
