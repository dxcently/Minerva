//! The harness every perch test file shares: the perch as a child process, the hand-written HTTP/1.1 requests, the stand-in door's log and
//! the SSE pane reader. It was the first half of `tests/http.rs`, moved here unchanged (visibility and `Builder` apart) so a slice's own test
//! file starts a perch the way `http.rs` does instead of copying 600 lines.
//!
//! A test file says `mod common;` and `use common::*;`. Cargo does not build `tests/common/mod.rs` as a test of its own. Each test crate
//! compiles its own copy and uses a part of it, so dead code is allowed here; what is dead differs per crate.
//!
//! The perch under test is always a child of the test process, started on `--bind 127.0.0.1:0` (or the port a slice's live check names),
//! with its runtime dir, XDG dirs and sessions dir in fresh temp dirs, so no test ever reads or writes the operator's. `Perch::builder()`
//! is the one way to ask for anything else; `Perch::start()` and the `start_*` wrappers are what `http.rs` already called.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The door's policy, spelled out rather than imported: a perch that widened it would
/// have to fail this test, not pass it by sharing a constant with itself.
pub const CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";

pub const INDEX: &str = "<!doctype html>\n<title>perch</title>\n";

// ---------------------------------------------------------------------------
// The perch under test
// ---------------------------------------------------------------------------

pub struct Perch {
    pub child: Child,
    /// `127.0.0.1:<port>`, the bound authority every default `Host` carries.
    pub addr: String,
    pub port: u16,
    pub token: String,
    pub token_file: PathBuf,
    /// The runtime dir the child was pointed at with `XDG_RUNTIME_DIR`. Held so the
    /// tree is not deleted out from under a running perch.
    pub runtime: tempfile::TempDir,
    /// The whole directory tree: `ui/` is what `--ui-dir` names, `work/` is a directory a
    /// session may be started in, and `secret.txt` sits beside them, where a request must not
    /// be able to reach.
    pub root: tempfile::TempDir,
    pub ui: PathBuf,
    /// `--root` for a session's cwd, and `work/` inside it.
    pub work: PathBuf,
    /// `--sessions-dir`: the kept logs. A tempdir for every test, so no test ever lists (or
    /// writes) the operator's real sessions.
    pub sessions: tempfile::TempDir,
    /// `$XDG_CONFIG_HOME` of the child: where the member's `eidolon/config.toml` would be, so no
    /// test ever reads the operator's real one.
    _xdg_config: tempfile::TempDir,
    /// `$XDG_DATA_HOME` of the child: eidolon's own sessions dir (`<data>/eidolon/sessions`) is the second, read-only source
    /// under `--home`, so no test ever lists (or the fake reads) the operator's real one.
    pub data: tempfile::TempDir,
    /// H1b: the tempdir holding the stand-in door's request log. Never read — held so the
    /// tree is not deleted out from under a running door. `None` for a perch started without
    /// the hub flags.
    _hub: Option<tempfile::TempDir>,
    /// What the test handed [`Builder::hold`]: dropped with the perch, after the child is gone.
    _held: Vec<Box<dyn std::any::Any + Send + Sync>>,
    pub fake_log: Option<PathBuf>,
    pub boot: String,
    pub stdout: Arc<Mutex<String>>,
    pub stderr: Arc<Mutex<String>>,
}

/// What the harness puts at `<xdg>/eidolon/policy.rn`: the shape of the stock table, which the manual wrapper renames and wraps.
pub const STOCK_POLICY_STUB: &str = "pub fn shell_arg(tool) { \"\" }
pub fn classify_tool(tool, approval, input) { #{decision: \"allow\", reason: \"stub\", read_only: true} }
pub fn classify_program(argv, ctx) { #{decision: \"allow\", reason: \"stub\", read_only: true} }
pub fn fs_verb_tier(prog) { \"allow\" }
pub fn bash_default(posture) { posture }
";

impl Perch {
    /// The child's stdout, as it stands.
    pub fn out(&self) -> String {
        self.stdout.lock().unwrap().clone()
    }

    pub fn err(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// SIGTERM, as a supervisor stops it, and the token file must be gone.
    pub fn stop(&mut self) -> std::process::ExitStatus {
        let pid = self.child.id() as i32;
        let sent = unsafe { libc::kill(pid, libc::SIGTERM) };
        assert_eq!(sent, 0, "SIGTERM to {pid}");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "the perch did not stop within 10s");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn start() -> Self {
        Self::start_with(true)
    }

    /// A perch that was given no `--ui-dir`: nothing outside the gate is a file, so
    /// `/` is a 404 — the door's own behaviour without the flag.
    pub fn start_without_ui_dir() -> Self {
        Self::start_with(false)
    }

    pub fn start_with(ui_dir: bool) -> Self {
        Self::launch(ui_dir, None, &[])
    }

    /// A perch with H1b's flags: a `--root` a session may work in, a temp `--sessions-dir`,
    /// and — when the crate was built with `--features test-bins` — the compiled stand-in
    /// door as `--eidolon`. `knobs` are the fake's own environment (`FAKE_SILENT`,
    /// `FAKE_MODE=0644`, …), which reach it because a door's environment is inherited
    /// unchanged (hub.md §4 step 4).
    #[cfg_attr(not(feature = "test-bins"), allow(dead_code))]
    pub fn start_hub(knobs: &[(&str, &str)]) -> Self {
        Self::launch(true, Some(knobs.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect()), &[])
    }

    /// The perch binary asked to start with `args` and expected to refuse: its whole output.
    pub fn refused_start(args: &[std::ffi::OsString]) -> std::process::Output {
        let runtime = tempfile::tempdir().unwrap();
        own(runtime.path(), 0o700);
        let xdg = tempfile::tempdir().unwrap();
        Command::new(env!("CARGO_BIN_EXE_perch"))
            .arg("--bind")
            .arg("127.0.0.1:0")
            .args(args)
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("XDG_CONFIG_HOME", xdg.path())
            .stdin(Stdio::null())
            .output()
            .expect("running the perch binary")
    }

    pub fn launch(ui_dir: bool, hub: Option<Vec<(String, String)>>, extra: &[std::ffi::OsString]) -> Self {
        Self::launch_with_member(ui_dir, hub, extra, None)
    }

    /// [`Perch::start_hub`] with more perch flags (the Minerva-home ones) and the member's `~/.config/eidolon/config.toml` written first.
    #[cfg_attr(not(feature = "test-bins"), allow(dead_code))]
    pub fn start_hub_member(extra: &[std::ffi::OsString], member_config: &str) -> Self {
        Self::launch_with_member(true, Some(Vec::new()), extra, Some(member_config))
    }

    pub fn launch_with_member(ui_dir: bool, hub: Option<Vec<(String, String)>>, extra: &[std::ffi::OsString], member_config: Option<&str>) -> Self {
        let mut builder = Self::builder().ui_dir(ui_dir).args(extra.iter().cloned());
        if let Some(knobs) = hub {
            builder = builder.hub().envs(knobs);
        }
        if let Some(text) = member_config {
            builder = builder.member_config(text);
        }
        builder.start()
    }

    /// The one way to ask for anything the wrappers above do not: more flags, more environment, a port, a member config, things to keep
    /// alive as long as the perch is. `Perch::builder().hub().args([...]).env("FAKE_ASKS", path).start()`.
    pub fn builder() -> Builder {
        Builder { ui_dir: true, hub: false, args: Vec::new(), env: Vec::new(), member_config: None, port: 0, held: Vec::new() }
    }
}

/// What a test asks of the perch it is about to start. Every default is what [`Perch::start`] gave: a UI dir, no hub flags, no extra flag,
/// no extra environment, no member config, port 0 (the kernel's pick, read back from the first boot line).
pub struct Builder {
    ui_dir: bool,
    hub: bool,
    args: Vec<std::ffi::OsString>,
    env: Vec<(String, String)>,
    member_config: Option<String>,
    port: u16,
    held: Vec<Box<dyn std::any::Any + Send + Sync>>,
}

impl Builder {
    /// `--ui-dir` is on unless this says otherwise.
    pub fn ui_dir(mut self, on: bool) -> Self {
        self.ui_dir = on;
        self
    }

    /// H1b's flags: `--root` (the temp tree), `--eidolon` (the stand-in door, when the crate was built with `--features test-bins`), and
    /// the stand-in's own `FAKE_LOG` and `FAKE_SESSIONS_DIR`. `Perch::fake_*` read that log.
    pub fn hub(mut self) -> Self {
        self.hub = true;
        self
    }

    /// One more perch flag, as given.
    pub fn arg(mut self, arg: impl Into<std::ffi::OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// More perch flags, as given. A flag and its value are two items.
    pub fn args<I, A>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: Into<std::ffi::OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// One environment variable for the perch, which every door and fake it starts inherits. It cannot replace the `XDG_*` dirs or
    /// `RUST_LOG`, which the harness sets last.
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    pub fn envs(mut self, vars: impl IntoIterator<Item = (String, String)>) -> Self {
        self.env.extend(vars);
        self
    }

    /// The member's `~/.config/eidolon/config.toml`, written before the perch starts.
    pub fn member_config(mut self, text: &str) -> Self {
        self.member_config = Some(text.to_string());
        self
    }

    /// `--bind 127.0.0.1:<port>`. 0, the default, is the kernel's pick; a slice's live check names its own.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Something the test made (a temp dir the flags name, a stub server) that must live exactly as long as the perch does.
    pub fn hold(mut self, keep: impl std::any::Any + Send + Sync) -> Self {
        self.held.push(Box::new(keep));
        self
    }

    pub fn start(self) -> Perch {
        let Builder { ui_dir, hub, args: extra, env, member_config, port: bind_port, held } = self;
        let root = tempfile::tempdir().unwrap();
        let ui = root.path().join("ui");
        std::fs::create_dir(&ui).unwrap();
        std::fs::write(ui.join("index.html"), INDEX).unwrap();
        std::fs::create_dir(ui.join("assets")).unwrap();
        std::fs::write(ui.join("assets/app.js"), "let x = 1;\n").unwrap();
        // One directory above the UI dir: reachable only by an escape.
        std::fs::write(root.path().join("secret.txt"), "s3cret\n").unwrap();
        // Files that are really inside the UI dir under the two namespaces the token gate
        // covers: hub.md §3 keeps `/hub` and `/s` out of the statics, so these must be
        // unreachable by every spelling — including an escaped one.
        std::fs::create_dir(ui.join("hub")).unwrap();
        std::fs::write(ui.join("hub/name.txt"), "the hub namespace is not a static\n").unwrap();
        std::fs::create_dir(ui.join("s")).unwrap();
        std::fs::write(ui.join("s/name.txt"), "the session namespace is not a static\n").unwrap();
        // A symlink out of the UI dir, and one that stays inside.
        std::os::unix::fs::symlink(root.path().join("secret.txt"), ui.join("escape.txt")).unwrap();
        std::os::unix::fs::symlink(ui.join("index.html"), ui.join("again.html")).unwrap();
        // The directory a session may be started in, inside `--root`.
        let work = root.path().join("work");
        std::fs::create_dir(&work).unwrap();

        let runtime = tempfile::tempdir().unwrap();
        // `tempfile` makes `0755`; a perch runtime dir is `0700`, and the perch refuses
        // anything looser, so the child would never print a boot line.
        own(runtime.path(), 0o700);
        let sessions = tempfile::tempdir().unwrap();
        let xdg_config = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        // A stock policy table for the manual wrapper to take over (the fake has no tables to unpack): the four entry points, nothing more.
        std::fs::create_dir_all(xdg_config.path().join("eidolon")).unwrap();
        std::fs::write(xdg_config.path().join("eidolon/policy.rn"), STOCK_POLICY_STUB).unwrap();
        if let Some(text) = member_config {
            std::fs::create_dir_all(xdg_config.path().join("eidolon")).unwrap();
            std::fs::write(xdg_config.path().join("eidolon/config.toml"), text).unwrap();
        }
        // With `--home` the home owns the sessions dir (`<home>/sessions`), and `--sessions-dir` conflicts; an
        // explicit `--eidolon-config` names it too, and the tests of that want the perch to read it from there.
        let home_sessions = extra.windows(2).find(|pair| pair[0] == "--home").map(|pair| PathBuf::from(&pair[1]).join("sessions"));

        let mut command = Command::new(env!("CARGO_BIN_EXE_perch"));
        command.arg("--bind").arg(format!("127.0.0.1:{bind_port}"));
        // Always, so no test ever reads the operator's real sessions dir.
        let owns_sessions = extra.iter().any(|arg| arg == "--home" || arg == "--eidolon-config");
        if !owns_sessions {
            command.arg("--sessions-dir").arg(sessions.path());
        }
        if ui_dir {
            command.arg("--ui-dir").arg(&ui);
        }
        command.args(&extra);
        let mut hub_dir = None;
        let mut fake_log = None;
        if hub {
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("fake.log");
            command.arg("--root").arg(root.path());
            if let Some(fake) = fake_eidolon() {
                command.arg("--eidolon").arg(fake);
            }
            // The fake's own contract: where it writes its log and where a new session's log
            // goes (the perch names that path in no argv — the door names it itself).
            command.env("FAKE_LOG", &log).env("FAKE_SESSIONS_DIR", home_sessions.as_deref().unwrap_or(sessions.path()));
            hub_dir = Some(dir);
            fake_log = Some(log);
        }
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("XDG_CONFIG_HOME", xdg_config.path())
            .env("XDG_DATA_HOME", data.path())
            // At trace for every test, not just the secrets one: the loud path is the
            // one a token could leak through, so every test is run on it.
            .env("RUST_LOG", "trace")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawning the perch binary");

        let stdout = Arc::new(Mutex::new(String::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        pump(child.stdout.take().unwrap(), stdout.clone());
        pump(child.stderr.take().unwrap(), stderr.clone());
        let boot = wait_for_lines(&mut child, &stdout, 2, Duration::from_secs(20));

        // Line 1 names the bound port, line 2 names the file the token was written to.
        let first = boot.lines().next().unwrap();
        let addr = first
            .strip_prefix("listening on http://")
            .and_then(|rest| rest.strip_suffix('/'))
            .unwrap_or_else(|| panic!("the first boot line is the URL, got {boot:?}"))
            .to_string();
        let port = addr.rsplit_once(':').unwrap().1.parse().unwrap();
        let token_file = PathBuf::from(
            boot.lines()
                .nth(1)
                .and_then(|line| line.strip_prefix("token file: "))
                .unwrap_or_else(|| panic!("the second boot line is the token file, got {boot:?}")),
        );
        let token = std::fs::read_to_string(&token_file).expect("the token file the perch printed");
        assert!(!token.trim().is_empty(), "the token file is not empty");

        Perch {
            child,
            addr,
            port,
            token,
            token_file,
            runtime,
            root,
            ui,
            work,
            sessions,
            _xdg_config: xdg_config,
            data,
            _hub: hub_dir,
            _held: held,
            fake_log,
            boot,
            stdout,
            stderr,
        }
    }
}

/// The compiled stand-in door, when the crate was built with `--features test-bins`.
pub fn fake_eidolon() -> Option<&'static str> {
    option_env!("CARGO_BIN_EXE_fake_eidolon")
}

/// The compiled stand-in `aoide` (a stub until slice R2a), when built with `--features test-bins`.
pub fn fake_aoide() -> Option<&'static str> {
    option_env!("CARGO_BIN_EXE_fake_aoide")
}

/// The compiled stand-in `minerva` (a stub until slice R4), when built with `--features test-bins`.
pub fn fake_minerva() -> Option<&'static str> {
    option_env!("CARGO_BIN_EXE_fake_minerva")
}

impl Drop for Perch {
    fn drop(&mut self) {
        // A test that already stopped it cleanly finds it gone; the rest are killed so
        // one failing assertion does not leave a server behind.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `chmod`, spelled once: `tempfile`'s directories are the umask's mode, and every
/// directory the perch is pointed at has to be `0700` before it will start.
pub fn own(dir: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
}

// ---------------------------------------------------------------------------
// H1b: the session lifecycle, over the same socket
// ---------------------------------------------------------------------------

impl Perch {
    /// The header every H1b request but a gate's own carries.
    pub fn auth(&self) -> String {
        format!("Bearer {}", self.token.trim())
    }

    pub fn apireq(&self, method: &str, path: &str) -> Request {
        Request::new(&self.addr, method, path).bearer(self.auth())
    }

    pub fn get(&self, path: &str) -> Answer {
        self.apireq("GET", path).send(&self.addr)
    }

    pub fn post(&self, path: &str) -> Answer {
        self.apireq("POST", path).send(&self.addr)
    }

    pub fn post_json(&self, path: &str, body: serde_json::Value) -> Answer {
        self.apireq("POST", path).body(serde_json::to_vec(&body).unwrap()).send(&self.addr)
    }

    /// The pin a seam gets until the slice that owns it builds it: the gate is in front of the route (no credential is the empty 401) and
    /// behind the gate the route is the empty 404 of a path nobody serves yet. The owning slice deletes the call and tests the route.
    pub fn assert_not_built(&self, method: &str, path: &str) {
        let bare = Request::new(&self.addr, method, path).send(&self.addr);
        assert_eq!(bare.status, 401, "{method} {path} with no credential: {}", bare.head);
        assert_eq!(bare.body, "", "{method} {path}: the 401 has no body");
        let gated = self.apireq(method, path).send(&self.addr);
        assert_eq!(gated.status, 404, "{method} {path} with the token: {}", gated.head);
        assert_eq!(gated.body, "", "{method} {path}: a route nobody serves has no body");
    }

    /// `GET /hub/sessions`, as the page reads it.
    pub fn list(&self) -> Vec<serde_json::Value> {
        let answer = self.get("/hub/sessions");
        assert_eq!(answer.status, 200, "{}", answer.head);
        assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{}", answer.head);
        serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("the list is a JSON array: {e}: {:?}", answer.body))
    }

    pub fn row(&self, id: &str) -> Option<serde_json::Value> {
        self.list().into_iter().find(|row| row["id"] == id)
    }

    pub fn state_of(&self, id: &str) -> Option<String> {
        self.row(id).and_then(|row| row["state"].as_str().map(str::to_string))
    }

    /// The list, polled. A state that is reached in the background — a child that exits, a
    /// door that is stopped — has no answer of its own to wait on, and a bounded poll is the
    /// only honest way to look for it.
    pub fn wait_state(&self, id: &str, want: &str, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if self.state_of(id).as_deref() == Some(want) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A kept session, as `eidolon sessions` would leave one: a log with that stem.
    pub fn keep_a_log(&self, id: &str) -> String {
        std::fs::write(self.sessions.path().join(format!("{id}.eid")), "{\"fake\":true}\n").unwrap();
        id.to_string()
    }

    /// A roster row for another consumer, in the swarm's own directory layout
    /// (`$XDG_RUNTIME_DIR/eidolon/<name>/meta.json`).
    pub fn roster_row(&self, name: &str, log: &std::path::Path, pid: u32) {
        let dir = self.runtime.path().join("eidolon").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let row = serde_json::json!({ "id": name, "pid": pid, "log": log.display().to_string(), "cwd": "/tmp", "title": "a peer" });
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&row).unwrap()).unwrap();
    }

    /// Every `web-*.token` a door left in the runtime dir. A stop must leave none: the door
    /// removes its own on the clean path, and the perch removes it after a SIGKILL.
    pub fn door_token_files(&self) -> Vec<PathBuf> {
        let dir = self.runtime.path().join("eidolon");
        let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.file_name().and_then(|n| n.to_str()).is_some_and(|name| name.starts_with("web-") && name.ends_with(".token")))
            .collect()
    }

    /// Where a door's token file goes: `<runtime dir>/eidolon/web-<port>.token` (hub.md §4
    /// step 6, the door's own `Presence::root()`).
    pub fn door_token_file(&self, port: u16) -> PathBuf {
        self.runtime.path().join("eidolon").join(format!("web-{port}.token"))
    }

    /// The door token itself, read the way the perch read it — out of the `0600` file, not
    /// out of anything the door printed.
    pub fn door_token(&self, port: u16) -> String {
        std::fs::read_to_string(self.door_token_file(port)).expect("the door's token file").trim().to_string()
    }

    /// What the stand-in door recorded about doors: its `start`/`argv`/`model` lines and the requests it served. The
    /// perch's own questions to eidolon (`sessions`, `log --json`) are `helper` lines, in [`Perch::fake_helpers`].
    pub fn fake_records(&self) -> Vec<String> {
        let log = self.fake_log.as_ref().expect("a perch started with the hub flags");
        std::fs::read_to_string(log).unwrap_or_default().lines().filter(|line| !line.starts_with("helper ")).map(str::to_string).collect()
    }

    /// Every time the perch ran `eidolon sessions` or `eidolon log --json`, as `helper <argv> config=<bool>`.
    pub fn fake_helpers(&self) -> Vec<String> {
        let log = self.fake_log.as_ref().expect("a perch started with the hub flags");
        std::fs::read_to_string(log).unwrap_or_default().lines().filter(|line| line.starts_with("helper ")).map(str::to_string).collect()
    }

    /// The door the test just asked for: the last `start` line's pid and port.
    pub fn fake_last_start(&self) -> (u32, u16) {
        let records = self.fake_records();
        let last = records.iter().rev().find(|line| line.starts_with("start ")).unwrap_or_else(|| panic!("no door started yet: {records:?}"));
        let field = |name: &str| -> u32 {
            last.split_whitespace()
                .find_map(|word| word.strip_prefix(name))
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| panic!("{name} in {last:?}"))
        };
        (field("pid="), field("port=") as u16)
    }

    /// The argv the stand-in door was given, as one line.
    pub fn fake_argv(&self) -> String {
        let records = self.fake_records();
        records.iter().rev().find(|line| line.starts_with("argv ")).unwrap_or_else(|| panic!("no argv recorded: {records:?}")).clone()
    }
}

/// `/proc/<pid>/cmdline` and `/proc/<pid>/environ`, NUL-separated and read lossily, as
/// hub.md §10.4 row 1 says to read them.
pub fn proc_of(pid: u32) -> (String, String) {
    let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_else(|e| panic!("/proc/{pid}/cmdline: {e}"));
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_else(|e| panic!("/proc/{pid}/environ: {e}"));
    (String::from_utf8_lossy(&argv).to_string(), String::from_utf8_lossy(&environ).to_string())
}

/// A process that is really gone, not merely signalled: the perch reaps its children, so a
/// `/proc` entry that is still there is an orphan (or a zombie the perch never waited on).
pub fn is_gone(pid: u32) -> bool {
    !std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Waits for a process to be gone, bounded, because a stop is asynchronous: `/stop` answers
/// once the door is signalled, and the child's exit is the kernel's business after that.
pub fn wait_gone(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if is_gone(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    is_gone(pid)
}

/// The `id` out of an answer that just made a session.
pub fn body_id(answer: &Answer) -> String {
    let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON answer: {e}: {:?}", answer.body));
    body["id"].as_str().unwrap_or_else(|| panic!("an id in {:?}", answer.body)).to_string()
}

/// Reads a pipe into a shared buffer, so a test can look at the child's output while
/// it is still running.
pub fn pump(mut pipe: impl Read + Send + 'static, into: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => into.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
    });
}

pub fn wait_for_lines(child: &mut Child, into: &Arc<Mutex<String>>, want: usize, within: Duration) -> String {
    let deadline = Instant::now() + within;
    loop {
        let seen = into.lock().unwrap().clone();
        if seen.lines().count() >= want {
            return seen;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("the perch exited ({status}) before printing {want} lines: {seen:?}");
        }
        if Instant::now() >= deadline {
            panic!("the perch printed {seen:?}, not {want} lines, within {within:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// One request as it goes on the wire, defaulting to the bound authority and no token.
pub struct Request {
    method: String,
    path: String,
    host: String,
    auth: Option<String>,
    extra: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    pub fn new(addr: &str, method: &str, path: &str) -> Self {
        Self { method: method.into(), path: path.into(), host: addr.into(), auth: None, extra: Vec::new(), body: Vec::new() }
    }

    pub fn bearer(mut self, auth: impl Into<String>) -> Self {
        self.auth = Some(auth.into());
        self
    }

    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.extra.push((name.into(), value.into()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    pub fn head(&self) -> String {
        let mut head = format!("{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n", self.method, self.path, self.host);
        if let Some(auth) = &self.auth {
            head.push_str(&format!("Authorization: {auth}\r\n"));
        }
        for (name, value) in &self.extra {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        if !self.body.is_empty() {
            head.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", self.body.len()));
        }
        head.push_str("\r\n");
        head
    }

    /// Sends it and reads the whole answer. Every request asks for
    /// `Connection: close`, so a response that never ends cannot hang a test.
    pub fn send(self, addr: &str) -> Answer {
        let mut sock = TcpStream::connect(addr).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let mut bytes = self.head().into_bytes();
        bytes.extend_from_slice(&self.body);
        sock.write_all(&bytes).unwrap();
        let mut raw = Vec::new();
        sock.read_to_end(&mut raw).unwrap();
        let answer = Answer::parse(&raw);
        // The one global assertion: no response this suite ever looks at carries a CORS
        // header, because nothing here answers a preflight and nothing here may invite
        // an origin that cannot hold the token.
        assert!(!answer.has_cors(), "a CORS header on {} {}: {}", self.method, self.path, answer.head);
        answer
    }
}

pub struct Answer {
    pub status: u16,
    pub head: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Answer {
    pub fn parse(raw: &[u8]) -> Self {
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n");
        let (head_bytes, body_bytes) = match split {
            Some(at) => (&raw[..at], &raw[at + 4..]),
            None => (raw, &raw[raw.len()..]),
        };
        let head = String::from_utf8_lossy(head_bytes).to_string();
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line in {head:?}"));
        let mut headers = Vec::new();
        for line in head.lines().skip(1) {
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
            }
        }
        // A proxied answer is chunked when the door's length was not known; a page reads the
        // decoded bytes, and so does this.
        let chunked = headers.iter().any(|(name, value)| name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked"));
        let body = if chunked { dechunk(body_bytes).0 } else { body_bytes.to_vec() };
        Self { status, head, headers, body: String::from_utf8_lossy(&body).to_string() }
    }

    /// Case-insensitive, as a browser reads them.
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    }

    pub fn has_cors(&self) -> bool {
        self.headers.iter().any(|(name, _)| name.starts_with("access-control-"))
    }

    /// The four headers every static response carries, 200 or refusal.
    pub fn assert_static_headers(&self, what: &str) {
        assert_eq!(self.header("x-content-type-options").as_deref(), Some("nosniff"), "{what}");
        assert_eq!(self.header("referrer-policy").as_deref(), Some("no-referrer"), "{what}");
        assert_eq!(self.header("cache-control").as_deref(), Some("no-cache"), "{what}");
        assert_eq!(self.header("content-security-policy").as_deref(), Some(CSP), "{what}");
    }
}

/// Sends a body of `len` bytes and reads the answer, with the body going out on
/// another thread: a refused body stops being read, so an inline write would block
/// before the status ever came back. A write error is the refusal, not a failure.
pub fn send_oversize(addr: &str, method: &str, path: &str, auth: Option<&str>, len: usize) -> Answer {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some(auth) = auth {
        head.push_str(&format!("Authorization: {auth}\r\n"));
    }
    head.push_str(&format!("Content-Type: application/json\r\nContent-Length: {len}\r\n\r\n"));

    let mut sock = TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    sock.write_all(head.as_bytes()).unwrap();
    let mut writer = sock.try_clone().unwrap();
    writer.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let body = std::thread::spawn(move || {
        let chunk = vec![b'x'; 64 * 1024];
        let mut written = 0;
        while written < len {
            let take = chunk.len().min(len - written);
            if writer.write_all(&chunk[..take]).is_err() {
                return written;
            }
            written += take;
        }
        written
    });
    let mut raw = Vec::new();
    let _ = sock.read_to_end(&mut raw);
    let written = body.join().unwrap();
    let answer = Answer::parse(&raw);
    assert!(!answer.has_cors(), "a CORS header on {method} {path}: {}", answer.head);
    assert!(written <= len);
    answer
}


// ---------------------------------------------------------------------------
// Chunked bodies and the SSE pane (moved from the H1c section of http.rs)
// ---------------------------------------------------------------------------

/// Decode as many whole chunks as `raw` holds: the bytes, and whether the terminal chunk has
/// been seen. A page reads a proxied stream through this, a chunk at a time.
pub fn dechunk(raw: &[u8]) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let Some(eol) = raw[at..].windows(2).position(|w| w == b"\r\n") else { return (out, false) };
        let size_text = String::from_utf8_lossy(&raw[at..at + eol]).to_string();
        let Ok(size) = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16) else { return (out, true) };
        let start = at + eol + 2;
        if size == 0 {
            return (out, true);
        }
        if raw.len() < start + size + 2 {
            return (out, false);
        }
        out.extend_from_slice(&raw[start..start + size]);
        at = start + size + 2;
    }
}

/// One open `GET /s/<id>/api/events`, read incrementally the way a page's fetch would: the
/// chunked body is decoded as it arrives, and every read is bounded by a deadline so a
/// stream that never delivers fails a test instead of hanging it.
#[cfg(feature = "test-bins")]
pub struct Pane {
    pub sock: TcpStream,
    /// The chunked body as received, undecoded.
    pub raw: Vec<u8>,
    pub head: String,
    /// The socket reached end-of-file (the perch closed it).
    pub eof: bool,
}

#[cfg(feature = "test-bins")]
impl Pane {
    pub fn open(perch: &Perch, id: &str) -> Self {
        Self::adopt(Self::begin(perch, id))
    }

    /// The request, sent, and nothing read: a pane the perch may still be setting up.
    pub fn begin(perch: &Perch, id: &str) -> TcpStream {
        Self::begin_path(perch, &format!("/s/{id}/api/events"))
    }

    /// [`Pane::open`] for any gated GET that streams, with the bearer: `/hub/term/events?since=t1:0`, `/hub/mesh/events`.
    pub fn open_path(perch: &Perch, path: &str) -> Self {
        Self::adopt(Self::begin_path(perch, path))
    }

    /// [`Pane::begin`] for any gated GET that streams.
    pub fn begin_path(perch: &Perch, path: &str) -> TcpStream {
        let mut sock = TcpStream::connect(&perch.addr).unwrap();
        let head = format!("GET {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: {}\r\nAccept: text/event-stream\r\n\r\n", perch.addr, perch.auth());
        sock.write_all(head.as_bytes()).unwrap();
        sock
    }

    /// [`Pane::begin`] with another credential line (`Cookie: ...`) in place of the bearer.
    pub fn begin_with(perch: &Perch, id: &str, credential: &str) -> TcpStream {
        let mut sock = TcpStream::connect(&perch.addr).unwrap();
        let head = format!("GET /s/{id}/api/events HTTP/1.1\r\nHost: {}\r\n{credential}\r\nAccept: text/event-stream\r\n\r\n", perch.addr);
        sock.write_all(head.as_bytes()).unwrap();
        sock
    }

    /// The response head of a pane that [`Pane::begin`] sent for, read.
    pub fn adopt(mut sock: TcpStream) -> Self {
        sock.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut raw = Vec::new();
        let end = loop {
            if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break at;
            }
            assert!(Instant::now() < deadline, "no response head for the pane: {:?}", String::from_utf8_lossy(&raw));
            let mut buf = [0u8; 4096];
            match sock.read(&mut buf) {
                Ok(0) => panic!("the perch closed the pane before its head: {:?}", String::from_utf8_lossy(&raw)),
                Ok(n) => raw.extend_from_slice(&buf[..n]),
                Err(_) => {}
            }
        };
        let head = String::from_utf8_lossy(&raw[..end]).to_string();
        assert!(head.starts_with("HTTP/1.1 200"), "the pane was refused: {head}");
        let rest = raw[end + 4..].to_vec();
        Self { sock, raw: rest, head, eof: false }
    }

    pub fn pump(&mut self) {
        let mut buf = [0u8; 16 * 1024];
        match self.sock.read(&mut buf) {
            Ok(0) => self.eof = true,
            Ok(n) => self.raw.extend_from_slice(&buf[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => self.eof = true,
        }
    }

    /// What the page has been handed so far, and whether the stream has ended cleanly.
    pub fn data(&self) -> (String, bool) {
        let (bytes, finished) = dechunk(&self.raw);
        (String::from_utf8_lossy(&bytes).to_string(), finished)
    }

    /// Read until `needle` is in the stream, the stream ends, or `within` passes. `true` only
    /// for the first: a stream that is still open and silent is a `false`, not a hang.
    pub fn read_until(&mut self, needle: &str, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            let (text, finished) = self.data();
            if text.contains(needle) {
                return true;
            }
            if finished || self.eof || Instant::now() >= deadline {
                return false;
            }
            self.pump();
        }
    }

    /// Read until the stream ends: `true` if it did within `within`.
    pub fn read_to_end(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            let (_, finished) = self.data();
            if finished || self.eof {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            self.pump();
        }
    }

    /// Every `data: {json}` frame so far that parses; the rest (a half frame) is ignored, as
    /// a page ignores it.
    pub fn frames(&self) -> Vec<serde_json::Value> {
        self.data().0.lines().filter_map(|line| line.strip_prefix("data: ")).filter_map(|json| serde_json::from_str(json).ok()).collect()
    }

    /// The perch's own last frame, and the assertion that there is exactly one of it: a stream
    /// that ends gets one `hub-stream-end`, not one per path that noticed the end.
    pub fn end_frame(&self) -> serde_json::Value {
        let mut ends: Vec<serde_json::Value> = self.frames().into_iter().filter(|f| f["type"] == "hub-stream-end").collect();
        assert_eq!(ends.len(), 1, "exactly one hub-stream-end in {:?}", self.data().0);
        ends.remove(0)
    }
}

/// A live session under the stand-in door: its id, pid and port.
#[cfg(feature = "test-bins")]
pub fn live(perch: &Perch) -> (String, u32, u16) {
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let (pid, port) = perch.fake_last_start();
    (body_id(&answer), pid, port)
}

/// The requests the stand-in door has recorded since `before` lines ago.
#[cfg(feature = "test-bins")]
pub fn since(perch: &Perch, before: usize) -> Vec<String> {
    perch.fake_records().into_iter().skip(before).collect()
}

/// A bounded poll of the door's log, because a request the perch has made is a fact that
/// reaches the file a moment later.
#[cfg(feature = "test-bins")]
pub fn wait_record(perch: &Perch, within: Duration, wanted: impl Fn(&[String]) -> bool) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if wanted(&perch.fake_records()) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
