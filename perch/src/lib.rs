//! The perch: one loopback HTTP server that will front several `eidolon web` doors
//! so the browser UI (`webui/term`) can tile them without touching upstream.
//!
//! This is slice **H1c** on top of H1a's front door and H1b's session lifecycle. H1a was the
//! process, the token and the gates; H1b is the **session lifecycle** behind them; H1c is the
//! **proxy**, which is what makes a door reachable from a page:
//!
//! ```text
//!   perch [--bind 127.0.0.1:0] [--ui-dir DIR] [--root DIR]... [--eidolon PATH] [--sessions-dir DIR]
//!         [--eidolon-config FILE] [--home DIR]
//!
//!   /                    the UI bundle from --ui-dir, no token, `Host` checked
//!   /hub/sessions        GET the list, POST to spawn one          (H1b)
//!   /hub/sessions/<id>/resume, /stop                              (H1b)
//!   /hub/sessions/<id>/log  a saved chat as eidolon's JSON lines                (titles slice)
//!   /hub/models          GET the models eidolon lists (cached 10 min)           (composer)
//!   /hub/sessions/<id>/restart  stop + resume the same log with -m / a mode (manual, auto, yolo), between turns   (composer)
//!   /s/<id>/api/<route>  GET|POST to that session's door, its Bearer injected,
//!                        SSE passed through, `hub-stream-end` appended   (H1c, this slice)
//!   /hub/tree?session=<id>&path=<rel>  one directory under a live cwd   (H2)
//!   /hub/file?session=<id>&path=<rel>  one image file under a live cwd (rawfile.rs)
//!   /hub/sessions/<id>/rename, /promote, /hub/projects/rename      (names, and an inbox chat's folder: relocate.rs)
//!   /hub/events                                                   (H2/H3: still 404)
//!   /hub/runs /hub/mesh /hub/meta /hub/install /hub/term /hub/notes /hub/mail      (seams: one module each, 404 until their slice)
//!   /hub/sessions/<id>/allow|revoke|mailsend                      (seams: 404 until their slice)
//!   /hub/term/a/<verb> /hub/mail/a/<verb>   the agent's own routes, behind a scoped token and never the master one
//! ```
//!
//! **The seams.** Each of those modules is a folder with a `README.md` and a stub for every handler a later slice fills, all with one
//! signature, `(ctx, req, rest) -> Response`, and all answering the empty 404 today. `serve` hands a request to its module before
//! the body is read (the module reads its own, under the same cap: [`serve::body_of`]), and the module's own `pick` decides which
//! handler gets it. A slice fills bodies; it adds no `mod` line and no route arm. [`serve::Ctx`] carries each module's `State`
//! (`Default`, filled by the module's hook), the parsed [`Options`], and `stop`, the token that ends every stream and wait the perch owns.
//!
//! So an **unauthenticated** request to `/hub` or `/s` is a 401 and an authenticated one to
//! a route that is not built yet is a 404: the route does not exist yet, but the gate in
//! front of it does. Being able to tell those two apart is what H1a proved and what every
//! later slice keeps.
//!
//! **The gates, in the order a request meets them** (hub.md §10.2). 0: this bind,
//! refused unless it is loopback, before a listener exists. 1: the token, on `/hub`,
//! `/s` and everything under them, before the path is parsed. 2: `Host` against the
//! bound authority, on every request. 3: `Sec-Fetch-Site`, when present. 4: method and
//! path shape. 5: the body cap. 6: per-route: hub.md §4's id regex, a cwd under a
//! `--root`, and the session's state. The refusals are 401, 403, 404/405, 413, 409 (two
//! doors on one log), 422 (a cwd outside every root) and 503 (a door that did not come up);
//! they are in [`serve`] beside the one `match` that routes, so no route can skip a gate.
//!
//! **A door is a child process.** It is spawned with no token in its argv or environment —
//! it mints its own, prints the path, and the perch reads it from a `0600` file it refuses
//! to trust without checking owner and mode (§4 steps 3-6, §10.4 row 1). The perch holds its
//! own `/api/events` stream to each door it owns ([`watch`]), which is how a door becomes
//! `live` and the stream a stop closes *before* it signals. A browser's proxied
//! `/api/events` (a pane, [`proxy`]) is the same kind of stream and is closed in the same
//! place, first.
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

pub mod agent;
pub mod allow;
pub mod aoide;
pub mod archive;
pub mod config;
pub mod dirs;
pub mod files;
pub mod fork;
pub mod folders;
pub mod gate;
pub mod guard;
pub mod images;
pub mod install;
pub mod login;
pub mod mail;
pub mod mesh;
pub mod meta;
pub mod mode;
pub mod models;
pub mod notes;
pub mod pending;
pub mod names;
pub mod projects;
pub mod proxy;
pub mod rawfile;
pub mod relocate;
pub mod restart;
pub mod runs;
pub mod serve;
pub(crate) mod statefile;
pub mod sessions;
pub mod spawn;
pub mod term;
pub mod titles;
pub mod trash;
pub mod tree;
pub mod watch;

use std::collections::BTreeMap;
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
    /// `--root`, repeatable: a new session's cwd must sit under one (hub.md Q6, default
    /// `$HOME`). Canonicalized and checked by [`sessions::Registry::new`], before the bind.
    pub roots: Vec<PathBuf>,
    /// `--eidolon`: the door binary to spawn. `eidolon` on `PATH` by default; the tests point
    /// it at the compiled `fake_eidolon`.
    pub eidolon: PathBuf,
    /// `--sessions-dir`: kept logs live here as `<id>.eid`. The door's own default is
    /// `data_local_dir()/eidolon/sessions`.
    pub sessions_dir: PathBuf,
    /// `--eidolon-config`: passed to every door as eidolon's global `--config`.
    pub eidolon_config: Option<PathBuf>,
    /// `--home`'s `scratch/`: the cwd of a new session that names none.
    pub scratch: Option<PathBuf>,
    /// With `--home`: eidolon's own sessions dir, listed read-only as `origin: "other"` ([`sessions::Registry::other`]).
    pub other_sessions: Option<PathBuf>,
    /// The manual config (or why there is none) and the default mode of a new chat ([`mode::prepare`]).
    pub modes: mode::ModeSetup,
    /// How long a login code (`POST /hub/code`) can be redeemed. [`login::CODE_TTL`] outside tests.
    pub code_ttl: std::time::Duration,
    /// `--plugins-dir`: the eidolon-plugins collection (`<dir>/jev`, `<dir>/browser`, ...). Graph runs read `<dir>/jev/workflows/<g>.rn`.
    pub plugins_dir: Option<PathBuf>,
    /// `--run-graphs`: the graph names a page may start, default `wiki-hop`. Each is a bare name (see [`graph_name_ok`]), never a path.
    pub run_graphs: Vec<String>,
    /// `--jev-health`: the URL the perch probes for jev's `/health` (a loopback address). `None` probes nothing.
    pub jev_health: Option<String>,
    /// `--browser-health`: the same for the browser service.
    pub browser_health: Option<String>,
    /// `--aoide`: the binary every Aoide call runs, else `~/.local/bin/aoide` when that is executable. Never a `PATH` lookup.
    pub aoide: Option<PathBuf>,
    /// `--aoide-home`: Aoide's own home (`~/.aoide`), read for the node registry and the mail cursor.
    pub aoide_home: Option<PathBuf>,
    /// `--aoide-daemon-socket`: the daemon's socket, which is only ever asked to `ping`.
    pub aoide_daemon_socket: Option<PathBuf>,
    /// `--aoide-events`: the audit log the mesh view tails.
    pub aoide_events: Option<PathBuf>,
    /// `--minerva`: the `minerva` binary the install routes ask, when it is not beside the perch's own.
    pub minerva: Option<PathBuf>,
    /// `--term-token-file`, `--mail-token-file`, `--agent-url-file`: where the scoped agent tokens and the perch's URL are written.
    /// All three are off unless given, so a test never touches `$HOME`.
    pub term_token_file: Option<PathBuf>,
    pub mail_token_file: Option<PathBuf>,
    pub agent_url_file: Option<PathBuf>,
    /// The hidden, repeatable `--knob k=v`: how a slice adds a test setting without touching `main.rs`.
    pub knobs: Knobs,
}

/// `--knob k=v`, kept as given. A key is `[a-z][a-z0-9_]{0,31}`; the value is anything, empty included. An unknown key is not an
/// error (a later slice adds its own), and a key given twice is, because "which one wins" is not an answer a test wants to guess.
#[derive(Clone, Debug, Default)]
pub struct Knobs(BTreeMap<String, String>);

impl Knobs {
    /// Parses every `k=v` the flag was given.
    pub fn parse(pairs: &[String]) -> Result<Self> {
        let mut knobs = BTreeMap::new();
        for pair in pairs {
            let (key, value) = pair.split_once('=').with_context(|| format!("--knob {pair:?} is not k=v"))?;
            anyhow::ensure!(
                knob_key_ok(key),
                "--knob {key:?}: a key is a lower case letter, then lower case letters, digits and _ (32 at most)"
            );
            // A knob this crate reads has a closed set of values, and a word outside it refuses the start: `doors=OFF` that quietly
            // meant "on" would leave a demo perch with its doors open, which is the thing the knob is for preventing.
            if let Some(allowed) = knob_values(key) {
                anyhow::ensure!(allowed.contains(&value), "--knob {key}={value:?}: the value is {}", allowed.join(" or "));
            }
            anyhow::ensure!(knobs.insert(key.to_string(), value.to_string()).is_none(), "--knob {key} is given twice");
        }
        Ok(Self(knobs))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// Whether the knob is set, to exactly this value.
    pub fn is(&self, key: &str, value: &str) -> bool {
        self.get(key) == Some(value)
    }
}

/// The values of a knob that this crate itself reads and that takes a closed set: `doors` is `on` or `off`. A key that is not here
/// takes any value, because it is a later slice's and that slice reads it.
fn knob_values(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "doors" => Some(&["on", "off"]),
        _ => None,
    }
}

fn knob_key_ok(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && key.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A graph name that `--run-graphs` may carry: letters, digits, `-` and `_`, starting with a letter or digit, at most 64. It is joined
/// into a path (`workflows/<g>.rn`), so it is a bare name and never a path, and a dot is not in it.
pub fn graph_name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
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
    // The registry and the thread that forks doors, before the bind for the same reason the
    // token's directory is: a `--root` that does not exist, or a spawn thread that cannot be
    // started, must refuse the start rather than the first spawn.
    let registry = sessions::Registry::with_home(
        opts.sessions_dir.clone(),
        runtime.clone(),
        opts.eidolon.clone(),
        opts.roots.clone(),
        opts.eidolon_config.clone(),
        opts.scratch.clone(),
        opts.other_sessions.clone(),
        opts.modes.clone(),
    )?;
    let spawner = Arc::new(spawn::Spawner::new()?);
    let listener = TcpListener::bind(opts.bind).await.with_context(|| format!("bind {}", opts.bind))?;
    let bound = listener.local_addr().context("read the bound address")?;

    // After the bind: `--bind 127.0.0.1:0` has no port to name the file for until now.
    let token = harnox::crypto::random_token();
    let token_file = token_path(&runtime, bound.port())?;
    write_token(&token_file, &token)?;

    let tree_slots = tree::Slots::new(tree::MAX_LISTINGS);
    let logins = Arc::new(login::Logins::new(opts.code_ttl));
    // Every module's state starts `Default`: what needs the options or the home is filled by that module's own hook below.
    let ctx = serve::Ctx {
        bound,
        token: Arc::from(token.as_str()),
        ui,
        registry,
        spawner,
        tree_slots,
        logins,
        opts: Arc::new(opts),
        stop: CancellationToken::new(),
        runs: Arc::default(),
        mesh: Arc::default(),
        meta: Arc::default(),
        install: Arc::default(),
        term: Arc::default(),
        notes: Arc::default(),
        mail: Arc::default(),
        allow: Arc::default(),
        agent: Arc::default(),
    };
    // The one hook that can refuse the start, so it runs before the boot lines: a supervisor never reads them from a perch that then
    // dies for a token file it could not write. It also leaves nothing behind: what it wrote and the perch's own token go.
    if let Err(e) = agent::start(&ctx) {
        agent::on_exit(&ctx);
        remove_token(&token_file, &token);
        return Err(e);
    }

    // Exactly two lines, and never the token. A supervisor reads these off the pipe;
    // `tracing` writes to stderr, nowhere near them.
    println!("listening on http://{bound}/");
    println!("token file: {}", token_file.display());

    // An inbox chat's folder follows its title between turns (relocate.rs); it stops with the perch, before the doors are stopped.
    let tidy = tokio::spawn(relocate::tidy_loop(ctx.clone(), cancel.clone()));
    // A fork's first message that was running when the perch last stopped (`forks.json` says): held off now, settled in the background.
    let orphans = pending::adopt(&ctx);
    tokio::spawn(pending::recover(ctx.clone(), orphans));
    // Runs a previous perch left detached are taken up again (D3); the mail task reads the mailbox while a page is looking.
    runs::adopt(&ctx);
    mail::start(&ctx);
    let served = serve::run(ctx.clone(), listener, cancel).await;
    tidy.abort();

    // Last, after the drain. The doors' shutdown — close the watcher to each door, SIGTERM
    // it, wait, SIGKILL what is left and remove its token file — has already happened inside
    // `serve::run`'s cancel arm, so no door outlives the token that let the page reach it. The agent's token files go with the perch's
    // own: a scoped token that outlived the listener that honoured it would be a credential for nothing.
    agent::on_exit(&ctx);
    remove_token(&token_file, &token);
    served
}

/// The perch's own token file, removed only while it still holds this perch's token: a perch that started on the same port after this
/// one let go of its listener has written its own token there, and that file is not this perch's to take. Gone already, someone else's
/// or unreadable: a debug line, nothing removed.
fn remove_token(token_file: &Path, token: &str) {
    match spawn::read_token_file(token_file) {
        Ok(found) if found == token => {
            if let Err(e) = std::fs::remove_file(token_file) {
                tracing::debug!(error = %e, path = %token_file.display(), "perch: token file not removed");
            }
        }
        Ok(_) => tracing::debug!(path = %token_file.display(), "perch: the token file holds another perch's token; left in place"),
        Err(e) => tracing::debug!(error = %e, path = %token_file.display(), "perch: token file not read; left in place"),
    }
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

    /// A perch that stops late must not take the token file a newer perch on the same port has written: only its own token goes.
    #[test]
    fn remove_token_takes_only_the_file_that_holds_this_perchs_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("perch-1.token");
        write_token(&path, "newer-perch").unwrap();
        remove_token(&path, "older-perch");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "newer-perch", "another perch's token file is left in place");
        remove_token(&path, "newer-perch");
        assert!(!path.exists(), "its own token file goes");
        // Already gone: nothing to do, nothing to panic about.
        remove_token(&path, "newer-perch");
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

    fn pairs(of: &[&str]) -> Vec<String> {
        of.iter().map(|pair| pair.to_string()).collect()
    }

    #[test]
    fn a_knob_is_a_key_and_a_value_and_a_key_is_not_given_twice() {
        let knobs = Knobs::parse(&pairs(&["doors=off", "term_ring=4096", "empty="])).unwrap();
        assert_eq!(knobs.get("doors"), Some("off"));
        assert!(knobs.is("doors", "off") && !knobs.is("doors", "on") && !knobs.is("door", "off"));
        assert_eq!(knobs.get("term_ring"), Some("4096"));
        assert_eq!(knobs.get("empty"), Some(""), "a value may be empty");
        assert_eq!(knobs.get("nothing"), None, "an unknown key is simply absent");
        assert!(Knobs::parse(&[]).unwrap().get("doors").is_none());
        // A value keeps a second `=`.
        assert_eq!(Knobs::parse(&pairs(&["a=b=c"])).unwrap().get("a"), Some("b=c"));
        for bad in ["doors", "=off", "Doors=off", "1a=b", "a-b=c", "doors =off", &format!("{}=x", "k".repeat(33))] {
            assert!(Knobs::parse(&pairs(&[bad])).is_err(), "{bad:?} is not a knob");
        }
        let twice = Knobs::parse(&pairs(&["doors=off", "doors=on"])).unwrap_err().to_string();
        assert!(twice.contains("doors") && twice.contains("twice"), "{twice}");
    }

    /// `doors` is `on` or `off`: any other spelling refuses the start, with a message that names the knob, the word and the choices,
    /// instead of being read as "not off".
    #[test]
    fn the_doors_knob_is_on_or_off_and_nothing_else() {
        assert!(Knobs::parse(&pairs(&["doors=on"])).unwrap().is("doors", "on"));
        assert!(Knobs::parse(&pairs(&["doors=off"])).unwrap().is("doors", "off"));
        for bad in ["doors=OFF", "doors=Off", "doors=nope", "doors=", "doors=off ", "doors= off", "doors=0", "doors=false", "doors=off=on"] {
            let why = Knobs::parse(&pairs(&[bad])).unwrap_err().to_string();
            assert!(why.contains("doors") && why.contains("on or off"), "{bad:?}: {why}");
        }
        // Only the knob this crate reads is closed: another key takes what its own slice reads, and the value is not looked at.
        assert!(Knobs::parse(&pairs(&["term_ring=anything at all", "x1="])).is_ok());
    }

    #[test]
    fn a_graph_name_is_a_bare_name_that_cannot_leave_its_directory() {
        for ok in ["wiki-hop", "a", "Graph_2", "0day", &"x".repeat(64)] {
            assert!(graph_name_ok(ok), "{ok:?}");
        }
        for no in ["", "-lead", "_lead", ".hidden", "..", "a/b", "../x", "a.rn", "a b", "é", "a\0", &"x".repeat(65)] {
            assert!(!graph_name_ok(no), "{no:?}");
        }
    }
}
