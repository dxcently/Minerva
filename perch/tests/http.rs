//! The perch over a real socket.
//!
//! The binary is spawned as a child with `--bind 127.0.0.1:0`, so what is tested is
//! the argv, the two boot lines and the `0600` token file a supervisor actually gets;
//! the token is then read out of the file the child printed, never out of its output.
//! The requests are hand-written HTTP/1.1, because `Request<Incoming>` cannot be built
//! outside a connection — which is also why every gate has a case here (hub.md §10.2).
//!
//! Every request carries the token unless the test is about the token, and every
//! response is checked for a `Access-Control-*` header before the test sees it.
//!
//! The H1b tests drive a real child process, so they need the stand-in door
//! (`src/bin/fake_eidolon.rs`) and are `#[cfg(feature = "test-bins")]`: `cargo test` alone
//! still builds and passes everything that needs no door, and the helpers only those tests
//! call are allowed to be unused there.
#![cfg_attr(not(feature = "test-bins"), allow(dead_code))]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The door's policy, spelled out rather than imported: a perch that widened it would
/// have to fail this test, not pass it by sharing a constant with itself.
const CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";

const INDEX: &str = "<!doctype html>\n<title>perch</title>\n";

// ---------------------------------------------------------------------------
// The perch under test
// ---------------------------------------------------------------------------

struct Perch {
    child: Child,
    /// `127.0.0.1:<port>`, the bound authority every default `Host` carries.
    addr: String,
    port: u16,
    token: String,
    token_file: PathBuf,
    /// The runtime dir the child was pointed at with `XDG_RUNTIME_DIR`. Held so the
    /// tree is not deleted out from under a running perch.
    runtime: tempfile::TempDir,
    /// The whole directory tree: `ui/` is what `--ui-dir` names, `work/` is a directory a
    /// session may be started in, and `secret.txt` sits beside them, where a request must not
    /// be able to reach.
    root: tempfile::TempDir,
    ui: PathBuf,
    /// `--root` for a session's cwd, and `work/` inside it.
    work: PathBuf,
    /// `--sessions-dir`: the kept logs. A tempdir for every test, so no test ever lists (or
    /// writes) the operator's real sessions.
    sessions: tempfile::TempDir,
    /// `$XDG_CONFIG_HOME` of the child: where the member's `eidolon/config.toml` would be, so no
    /// test ever reads the operator's real one.
    _xdg_config: tempfile::TempDir,
    /// `$XDG_DATA_HOME` of the child: eidolon's own sessions dir (`<data>/eidolon/sessions`) is the second, read-only source
    /// under `--home`, so no test ever lists (or the fake reads) the operator's real one.
    data: tempfile::TempDir,
    /// H1b: the tempdir holding the stand-in door's request log. Never read — held so the
    /// tree is not deleted out from under a running door. `None` for a perch started without
    /// the hub flags.
    _hub: Option<tempfile::TempDir>,
    fake_log: Option<PathBuf>,
    boot: String,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
}

impl Perch {
    /// The child's stdout, as it stands.
    fn out(&self) -> String {
        self.stdout.lock().unwrap().clone()
    }

    fn err(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// SIGTERM, as a supervisor stops it, and the token file must be gone.
    fn stop(&mut self) -> std::process::ExitStatus {
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

    fn start() -> Self {
        Self::start_with(true)
    }

    /// A perch that was given no `--ui-dir`: nothing outside the gate is a file, so
    /// `/` is a 404 — the door's own behaviour without the flag.
    fn start_without_ui_dir() -> Self {
        Self::start_with(false)
    }

    fn start_with(ui_dir: bool) -> Self {
        Self::launch(ui_dir, None, &[])
    }

    /// A perch with H1b's flags: a `--root` a session may work in, a temp `--sessions-dir`,
    /// and — when the crate was built with `--features test-bins` — the compiled stand-in
    /// door as `--eidolon`. `knobs` are the fake's own environment (`FAKE_SILENT`,
    /// `FAKE_MODE=0644`, …), which reach it because a door's environment is inherited
    /// unchanged (hub.md §4 step 4).
    #[cfg_attr(not(feature = "test-bins"), allow(dead_code))]
    fn start_hub(knobs: &[(&str, &str)]) -> Self {
        Self::launch(true, Some(knobs.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect()), &[])
    }

    /// The perch binary asked to start with `args` and expected to refuse: its whole output.
    fn refused_start(args: &[std::ffi::OsString]) -> std::process::Output {
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

    fn launch(ui_dir: bool, hub: Option<Vec<(String, String)>>, extra: &[std::ffi::OsString]) -> Self {
        Self::launch_with_member(ui_dir, hub, extra, None)
    }

    /// [`Perch::start_hub`] with more perch flags (the Minerva-home ones) and the member's `~/.config/eidolon/config.toml` written first.
    #[cfg_attr(not(feature = "test-bins"), allow(dead_code))]
    fn start_hub_member(extra: &[std::ffi::OsString], member_config: &str) -> Self {
        Self::launch_with_member(true, Some(Vec::new()), extra, Some(member_config))
    }

    fn launch_with_member(ui_dir: bool, hub: Option<Vec<(String, String)>>, extra: &[std::ffi::OsString], member_config: Option<&str>) -> Self {
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
        if let Some(text) = member_config {
            std::fs::create_dir_all(xdg_config.path().join("eidolon")).unwrap();
            std::fs::write(xdg_config.path().join("eidolon/config.toml"), text).unwrap();
        }
        // With `--home` the home owns the sessions dir (`<home>/sessions`), and `--sessions-dir` conflicts; an\n        // explicit `--eidolon-config` names it too, and the tests of that want the perch to read it from there.
        let home_sessions = extra.windows(2).find(|pair| pair[0] == "--home").map(|pair| PathBuf::from(&pair[1]).join("sessions"));

        let mut command = Command::new(env!("CARGO_BIN_EXE_perch"));
        command.arg("--bind").arg("127.0.0.1:0");
        // Always, so no test ever reads the operator's real sessions dir.
        let owns_sessions = extra.iter().any(|arg| arg == "--home" || arg == "--eidolon-config");
        if !owns_sessions {
            command.arg("--sessions-dir").arg(sessions.path());
        }
        if ui_dir {
            command.arg("--ui-dir").arg(&ui);
        }
        command.args(extra);
        let mut hub_dir = None;
        let mut fake_log = None;
        if let Some(knobs) = hub {
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("fake.log");
            command.arg("--root").arg(root.path());
            if let Some(fake) = option_env!("CARGO_BIN_EXE_fake_eidolon") {
                command.arg("--eidolon").arg(fake);
            }
            // The fake's own contract: where it writes its log and where a new session's log
            // goes (the perch names that path in no argv — the door names it itself).
            command.env("FAKE_LOG", &log).env("FAKE_SESSIONS_DIR", home_sessions.as_deref().unwrap_or(sessions.path()));
            for (name, value) in knobs {
                command.env(name, value);
            }
            hub_dir = Some(dir);
            fake_log = Some(log);
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

        Self { child, addr, port, token, token_file, runtime, root, ui, work, sessions, _xdg_config: xdg_config, data, _hub: hub_dir, fake_log, boot, stdout, stderr }
    }
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
fn own(dir: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
}

// ---------------------------------------------------------------------------
// H1b: the session lifecycle, over the same socket
// ---------------------------------------------------------------------------

impl Perch {
    /// The header every H1b request but a gate's own carries.
    fn auth(&self) -> String {
        format!("Bearer {}", self.token.trim())
    }

    fn apireq(&self, method: &str, path: &str) -> Request {
        Request::new(&self.addr, method, path).bearer(self.auth())
    }

    fn get(&self, path: &str) -> Answer {
        self.apireq("GET", path).send(&self.addr)
    }

    fn post(&self, path: &str) -> Answer {
        self.apireq("POST", path).send(&self.addr)
    }

    fn post_json(&self, path: &str, body: serde_json::Value) -> Answer {
        self.apireq("POST", path).body(serde_json::to_vec(&body).unwrap()).send(&self.addr)
    }

    /// `GET /hub/sessions`, as the page reads it.
    fn list(&self) -> Vec<serde_json::Value> {
        let answer = self.get("/hub/sessions");
        assert_eq!(answer.status, 200, "{}", answer.head);
        assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{}", answer.head);
        serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("the list is a JSON array: {e}: {:?}", answer.body))
    }

    fn row(&self, id: &str) -> Option<serde_json::Value> {
        self.list().into_iter().find(|row| row["id"] == id)
    }

    fn state_of(&self, id: &str) -> Option<String> {
        self.row(id).and_then(|row| row["state"].as_str().map(str::to_string))
    }

    /// The list, polled. A state that is reached in the background — a child that exits, a
    /// door that is stopped — has no answer of its own to wait on, and a bounded poll is the
    /// only honest way to look for it.
    fn wait_state(&self, id: &str, want: &str, within: Duration) -> bool {
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
    fn keep_a_log(&self, id: &str) -> String {
        std::fs::write(self.sessions.path().join(format!("{id}.eid")), "{\"fake\":true}\n").unwrap();
        id.to_string()
    }

    /// A roster row for another consumer, in the swarm's own directory layout
    /// (`$XDG_RUNTIME_DIR/eidolon/<name>/meta.json`).
    fn roster_row(&self, name: &str, log: &std::path::Path, pid: u32) {
        let dir = self.runtime.path().join("eidolon").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let row = serde_json::json!({ "id": name, "pid": pid, "log": log.display().to_string(), "cwd": "/tmp", "title": "a peer" });
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&row).unwrap()).unwrap();
    }

    /// Every `web-*.token` a door left in the runtime dir. A stop must leave none: the door
    /// removes its own on the clean path, and the perch removes it after a SIGKILL.
    fn door_token_files(&self) -> Vec<PathBuf> {
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
    fn door_token_file(&self, port: u16) -> PathBuf {
        self.runtime.path().join("eidolon").join(format!("web-{port}.token"))
    }

    /// The door token itself, read the way the perch read it — out of the `0600` file, not
    /// out of anything the door printed.
    fn door_token(&self, port: u16) -> String {
        std::fs::read_to_string(self.door_token_file(port)).expect("the door's token file").trim().to_string()
    }

    /// What the stand-in door recorded about doors: its `start`/`argv`/`model` lines and the requests it served. The
    /// perch's own questions to eidolon (`sessions`, `log --json`) are `helper` lines, in [`Perch::fake_helpers`].
    fn fake_records(&self) -> Vec<String> {
        let log = self.fake_log.as_ref().expect("a perch started with the hub flags");
        std::fs::read_to_string(log).unwrap_or_default().lines().filter(|line| !line.starts_with("helper ")).map(str::to_string).collect()
    }

    /// Every time the perch ran `eidolon sessions` or `eidolon log --json`, as `helper <argv> config=<bool>`.
    fn fake_helpers(&self) -> Vec<String> {
        let log = self.fake_log.as_ref().expect("a perch started with the hub flags");
        std::fs::read_to_string(log).unwrap_or_default().lines().filter(|line| line.starts_with("helper ")).map(str::to_string).collect()
    }

    /// The door the test just asked for: the last `start` line's pid and port.
    fn fake_last_start(&self) -> (u32, u16) {
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
    fn fake_argv(&self) -> String {
        let records = self.fake_records();
        records.iter().rev().find(|line| line.starts_with("argv ")).unwrap_or_else(|| panic!("no argv recorded: {records:?}")).clone()
    }
}

/// `/proc/<pid>/cmdline` and `/proc/<pid>/environ`, NUL-separated and read lossily, as
/// hub.md §10.4 row 1 says to read them.
fn proc_of(pid: u32) -> (String, String) {
    let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_else(|e| panic!("/proc/{pid}/cmdline: {e}"));
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_else(|e| panic!("/proc/{pid}/environ: {e}"));
    (String::from_utf8_lossy(&argv).to_string(), String::from_utf8_lossy(&environ).to_string())
}

/// A process that is really gone, not merely signalled: the perch reaps its children, so a
/// `/proc` entry that is still there is an orphan (or a zombie the perch never waited on).
fn is_gone(pid: u32) -> bool {
    !std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Waits for a process to be gone, bounded, because a stop is asynchronous: `/stop` answers
/// once the door is signalled, and the child's exit is the kernel's business after that.
fn wait_gone(pid: u32, within: Duration) -> bool {
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
fn body_id(answer: &Answer) -> String {
    let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON answer: {e}: {:?}", answer.body));
    body["id"].as_str().unwrap_or_else(|| panic!("an id in {:?}", answer.body)).to_string()
}

/// Reads a pipe into a shared buffer, so a test can look at the child's output while
/// it is still running.
fn pump(mut pipe: impl Read + Send + 'static, into: Arc<Mutex<String>>) {
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

fn wait_for_lines(child: &mut Child, into: &Arc<Mutex<String>>, want: usize, within: Duration) -> String {
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
struct Request {
    method: String,
    path: String,
    host: String,
    auth: Option<String>,
    extra: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn new(addr: &str, method: &str, path: &str) -> Self {
        Self { method: method.into(), path: path.into(), host: addr.into(), auth: None, extra: Vec::new(), body: Vec::new() }
    }

    fn bearer(mut self, auth: impl Into<String>) -> Self {
        self.auth = Some(auth.into());
        self
    }

    fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.extra.push((name.into(), value.into()));
        self
    }

    fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    fn head(&self) -> String {
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
    fn send(self, addr: &str) -> Answer {
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

struct Answer {
    status: u16,
    head: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answer {
    fn parse(raw: &[u8]) -> Self {
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
    fn header(&self, name: &str) -> Option<String> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    }

    fn has_cors(&self) -> bool {
        self.headers.iter().any(|(name, _)| name.starts_with("access-control-"))
    }

    /// The four headers every static response carries, 200 or refusal.
    fn assert_static_headers(&self, what: &str) {
        assert_eq!(self.header("x-content-type-options").as_deref(), Some("nosniff"), "{what}");
        assert_eq!(self.header("referrer-policy").as_deref(), Some("no-referrer"), "{what}");
        assert_eq!(self.header("cache-control").as_deref(), Some("no-cache"), "{what}");
        assert_eq!(self.header("content-security-policy").as_deref(), Some(CSP), "{what}");
    }
}

/// Sends a body of `len` bytes and reads the answer, with the body going out on
/// another thread: a refused body stops being read, so an inline write would block
/// before the status ever came back. A write error is the refusal, not a failure.
fn send_oversize(addr: &str, method: &str, path: &str, auth: Option<&str>, len: usize) -> Answer {
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
// The process: argv, the two boot lines, the token file
// ---------------------------------------------------------------------------

#[test]
fn stdout_is_the_two_boot_lines_and_the_token_is_not_in_them() {
    let perch = Perch::start();
    let lines: Vec<&str> = perch.boot.lines().collect();
    assert_eq!(lines.len(), 2, "stdout is the two boot lines, nothing else: {:?}", perch.boot);
    assert_eq!(lines[0], format!("listening on http://{}/", perch.addr));
    assert_eq!(lines[1], format!("token file: {}", perch.token_file.display()));
    assert!(!perch.boot.contains(perch.token.trim()), "the token is never printed: {:?}", perch.boot);
}

#[test]
fn the_token_file_is_owner_only_and_named_for_the_bound_port() {
    use std::os::unix::fs::PermissionsExt as _;
    let perch = Perch::start();
    let mode = std::fs::metadata(&perch.token_file).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "the token file is owner-only, got {mode:o}");
    assert_eq!(mode & 0o777, 0o600, "…and only the owner reads it, got {mode:o}");
    assert_eq!(perch.token_file, perch.runtime.path().join("minerva").join(format!("perch-{}.token", perch.port)));
    // What `random_token` mints: 43 base64url characters.
    let token = perch.token.trim();
    assert_eq!(token.len(), 43, "{}", perch.token);
    assert!(token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{token}");
}

#[test]
fn sigterm_removes_the_token_file_and_stops_the_listener() {
    let mut perch = Perch::start();
    assert_eq!(Request::new(&perch.addr, "GET", "/").send(&perch.addr).status, 200);
    let status = perch.stop();
    assert!(status.success(), "a signalled stop is a clean exit: {status}");
    assert!(!perch.token_file.exists(), "the token file outlived the listener that honoured it");
    assert!(TcpStream::connect(&perch.addr).is_err(), "nothing is listening any more");
}

#[test]
fn a_bind_that_is_not_loopback_is_refused_before_anything_listens() {
    // Gate 0: the refusal is the process's, not a request's.
    let runtime = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_perch"))
        .arg("--bind")
        .arg("0.0.0.0:0")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "a perch tried to bind 0.0.0.0");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("loopback only"), "the refusal says why: {err}");
    assert!(out.stdout.is_empty(), "nothing was printed as a boot line: {:?}", out.stdout);
    assert!(std::fs::read_dir(&runtime.path()).unwrap().next().is_none(), "no token file was minted for a refused bind");
}

#[test]
fn a_missing_runtime_dir_refuses_the_start() {
    // The door falls back to the shared temp dir (`swarm::Presence::root`); the perch
    // requires the variable, because a shared directory is one another user can
    // pre-create for the token.
    let ui = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_perch"))
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--ui-dir")
        .arg(ui.path())
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "a perch without a runtime dir came up");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("XDG_RUNTIME_DIR"), "the refusal names the variable: {err}");
    assert!(out.stdout.is_empty(), "no boot line from a refused start: {:?}", out.stdout);
    assert!(std::fs::read_dir(ui.path()).unwrap().next().is_none(), "nothing was written into --ui-dir");
}

#[test]
fn without_a_ui_dir_every_static_path_is_404() {
    let perch = Perch::start_without_ui_dir();
    for path in ["/", "/index.html", "/assets/app.js"] {
        let answer = Request::new(&perch.addr, "GET", path).send(&perch.addr);
        assert_eq!(answer.status, 404, "{path} without --ui-dir: {}", answer.head);
        answer.assert_static_headers("a 404 without a bundle");
    }
}

#[test]
fn a_runtime_dir_that_is_not_owner_only_refuses_the_start() {
    let runtime = tempfile::tempdir().unwrap();
    own(runtime.path(), 0o755);
    let out = Command::new(env!("CARGO_BIN_EXE_perch"))
        .arg("--bind")
        .arg("127.0.0.1:0")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "a world-readable runtime dir was accepted");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("755"), "the refusal names the mode it found: {err}");
    assert!(out.stdout.is_empty(), "no boot line from a refused start: {:?}", out.stdout);
}

// ---------------------------------------------------------------------------
// Statics: no token, `Host` checked, headers on every response
// ---------------------------------------------------------------------------

#[test]
fn a_static_file_is_served_without_a_token_with_the_bundle_headers() {
    let perch = Perch::start();
    let answer = Request::new(&perch.addr, "GET", "/").send(&perch.addr);
    assert_eq!(answer.status, 200, "{}", answer.head);
    assert_eq!(answer.body, std::fs::read_to_string(perch.ui.join("index.html")).unwrap(), "`/` is index.html");
    assert_eq!(answer.header("content-type").as_deref(), Some("text/html; charset=utf-8"));
    answer.assert_static_headers("the index");
    assert!(!answer.has_cors());

    let asset = Request::new(&perch.addr, "GET", "/assets/app.js").send(&perch.addr);
    assert_eq!(asset.status, 200);
    assert_eq!(asset.header("content-type").as_deref(), Some("text/javascript; charset=utf-8"));
    asset.assert_static_headers("an asset");
}

#[test]
fn the_static_route_answers_head_405_and_404_as_the_route_table_says() {
    let perch = Perch::start();
    let head = Request::new(&perch.addr, "HEAD", "/").send(&perch.addr);
    assert_eq!(head.status, 200);
    assert_eq!(head.header("content-length").as_deref(), Some(INDEX.len().to_string().as_str()));
    assert_eq!(head.body, "", "HEAD carries no body");
    head.assert_static_headers("a HEAD");

    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        let refusal = Request::new(&perch.addr, method, "/").send(&perch.addr);
        assert_eq!(refusal.status, 405, "{method} / is not a static method");
        assert_eq!(refusal.header("allow").as_deref(), Some("GET, HEAD"));
        refusal.assert_static_headers(&format!("a {method} refusal"));
    }

    // A 404 carries the static headers too: the browser that asked is still running
    // the bundle, and a refusal without a policy is a page without one.
    for path in ["/missing.js", "/assets", "/assets/", "/again.html%"] {
        let missing = Request::new(&perch.addr, "GET", path).send(&perch.addr);
        assert_eq!(missing.status, 404, "{path}");
        missing.assert_static_headers(&format!("the 404 for {path}"));
    }

    // A link that stays inside the UI dir is a file; the door's rules, over the wire.
    assert_eq!(Request::new(&perch.addr, "GET", "/again.html").send(&perch.addr).status, 200);
}

#[test]
fn every_escape_from_the_ui_dir_is_refused_over_the_wire() {
    let perch = Perch::start();
    // The secret the escapes below must not reach, named on disk so a 404 for it means
    // something: it is one directory above `--ui-dir`.
    assert_eq!(std::fs::read_to_string(perch.root.path().join("secret.txt")).unwrap().trim(), "s3cret");
    for path in [
        // the ported files.rs cases, asked of a real server
        "/../secret.txt",
        "/%2e%2e/secret.txt",
        "/..%2fsecret.txt",
        "/%2e%2e%2fsecret.txt",
        "/assets/../../secret.txt",
        "/assets/..%2f..%2fsecret.txt",
        "/..",
        "/.",
        "/assets/..",
        "//secret.txt",
        "/assets//secret.txt",
        "/..\\secret.txt",
        "/%5c..%5csecret.txt",
        "/assets\\app.js",
        "/%00",
        "/assets/%00.js",
        "/secret.txt%",
        "/a%zz",
        // absolute-looking paths resolve *inside* the UI dir, and these are not there
        "/etc/passwd",
        "/%2e%2e%2f%2e%2e%2fetc%2fpasswd",
        // a symlink out, and the file beside the UI dir by its plain name
        "/escape.txt",
        "/secret.txt",
    ] {
        let answer = Request::new(&perch.addr, "GET", path).send(&perch.addr);
        assert_eq!(answer.status, 404, "{path}: {}", answer.head);
        assert!(!answer.body.contains("s3cret"), "{path} served a file outside the UI dir");
    }
    // …and the regular file under the root is still served, so the refusals above are
    // the containment rule and not a server that serves nothing.
    assert_eq!(Request::new(&perch.addr, "GET", "/index.html").send(&perch.addr).status, 200);
}

// ---------------------------------------------------------------------------
// Gate 1: the token
// ---------------------------------------------------------------------------

#[test]
fn a_gated_route_without_a_token_is_401_with_an_empty_body() {
    let perch = Perch::start();
    // The last two are the same namespaces spelled with a percent escape. The static route
    // decodes once before it names a file, so `/%68ub/x` is `hub/x` to it; the gate decodes
    // too, or the spelling would reach the UI dir (and answer 404 where §3 promises 401).
    for path in ["/hub", "/hub/", "/hub/sessions", "/hub/tree", "/s", "/s/", "/s/x/api/events", "/%68ub/x", "/%73/x"] {
        for method in ["GET", "POST"] {
            let answer = Request::new(&perch.addr, method, path).send(&perch.addr);
            assert_eq!(answer.status, 401, "{method} {path} without a token: {}", answer.head);
            assert_eq!(answer.body, "", "a 401 says nothing: {path}");
        }
    }
}

#[test]
fn a_file_under_the_gated_namespaces_is_not_a_static_in_any_spelling() {
    let perch = Perch::start();
    // Both of these exist on disk, under `--ui-dir` (`ui/hub/name.txt`, `ui/s/name.txt`).
    // hub.md §3 keeps `/hub` and `/s` out of the statics, and the gate decodes the path the
    // way the static route does, so no spelling of either namespace serves them.
    assert!(perch.ui.join("hub/name.txt").is_file(), "the fixture file is really there");
    for path in ["/hub/name.txt", "/%68ub/name.txt", "/%68ub%2fname.txt", "/s/name.txt", "/%73/name.txt"] {
        let answer = Request::new(&perch.addr, "GET", path).send(&perch.addr);
        assert_eq!(answer.status, 401, "{path} without a token: {}", answer.head);
        assert_eq!(answer.body, "", "{path} reached a file under the gated namespace");
    }
    // With the token it is the 404 of a route that does not exist, not the file either.
    let auth = format!("Bearer {}", perch.token.trim());
    for path in ["/%68ub/name.txt", "/%73/name.txt"] {
        let answer = Request::new(&perch.addr, "GET", path).bearer(auth.clone()).send(&perch.addr);
        assert_eq!(answer.status, 404, "{path} with the token: {}", answer.head);
        assert_eq!(answer.body, "", "{path} served a file under the gated namespace");
    }
}

#[test]
fn a_gated_route_with_a_wrong_token_is_401() {
    let perch = Perch::start();
    let token = perch.token.trim().to_string();
    for wrong in [
        "Bearer wrong".to_string(),
        format!("Bearer {}", &token[..token.len() - 1]),
        format!("Bearer {token}x"),
        format!("Bearer {}", token.to_uppercase()),
        token.clone(),
        format!("bearer {token}"),
    ] {
        let answer = Request::new(&perch.addr, "GET", "/hub/anything").bearer(wrong.clone()).send(&perch.addr);
        assert_eq!(answer.status, 401, "{wrong:?} is not the token: {}", answer.head);
        assert_eq!(answer.body, "");
    }
}

#[test]
fn a_gated_route_that_is_not_built_yet_is_404_with_the_right_token() {
    // The point of H1a, and it still holds for every route a later slice owns: the gate
    // passed, and what is behind it is not built (`/hub/events`, `/hub/mesh`: H2/H3; `GET /hub/tree`
    // is built since H2 and has its own tests at the end of this file).
    // `/s` is built since H1c, but `x` is no session of this perch, so the proxy answers the
    // same empty 404 for it: a session route for a session that is not there.
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    for (method, path) in [
        ("GET", "/hub"),
        ("POST", "/hub/tree"),
        ("GET", "/hub/events"),
        ("GET", "/hub/mesh"),
        ("GET", "/hub/mesh/events"),
        ("GET", "/s"),
        ("GET", "/s/x/api/events"),
        ("POST", "/s/x/api/say"),
    ] {
        let answer = Request::new(&perch.addr, method, path).bearer(auth.clone()).send(&perch.addr);
        assert_eq!(answer.status, 404, "{method} {path} with the token: {}", answer.head);
        assert_eq!(answer.body, "", "no unbuilt route has a body: {path}");
    }
    // …and the routes that are built are not among them: they answer, which is what makes
    // the 404s above "not built yet" rather than "not for you".
    let sessions = Request::new(&perch.addr, "GET", "/hub/sessions").bearer(auth).send(&perch.addr);
    assert_eq!(sessions.status, 200, "{}", sessions.head);
    assert_eq!(sessions.body, "[]", "a perch with an empty sessions dir lists nothing");
}

#[test]
fn a_query_token_is_not_a_credential() {
    let perch = Perch::start();
    let token = perch.token.trim();
    for path in [
        format!("/hub/anything?token={token}"),
        format!("/hub/sessions?token={token}"),
        format!("/s?token={token}"),
        format!("/s/x/api/events?token={token}"),
    ] {
        let answer = Request::new(&perch.addr, "GET", &path).send(&perch.addr);
        assert_eq!(answer.status, 401, "{path} was taken as a credential: {}", answer.head);
    }
    // The header is still the header, query or not.
    let answer = Request::new(&perch.addr, "GET", &format!("/hub/anything?token={token}"))
        .bearer(format!("Bearer {token}"))
        .send(&perch.addr);
    assert_eq!(answer.status, 404, "{}", answer.head);
}

#[test]
fn a_query_string_on_a_session_route_is_400() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    for (method, path) in [
        ("GET", "/s/x/api/y?session=1"),
        ("POST", "/s/x/api/y?a=1"),
        ("GET", "/s/x/api/y?token=1"),
        ("GET", "/s?x=1"),
    ] {
        let answer = Request::new(&perch.addr, method, path).bearer(auth.clone()).send(&perch.addr);
        assert_eq!(answer.status, 400, "{method} {path} carries a query: {}", answer.head);
    }
    // Without the query it is the 404 of a route that does not exist, so the 400 above
    // is the query refusal and not a blanket refusal of the path.
    let plain = Request::new(&perch.addr, "GET", "/s/x/api/y").bearer(auth).send(&perch.addr);
    assert_eq!(plain.status, 404, "{}", plain.head);
    // `/hub` keeps its query strings: `/hub/tree?session=…&path=…` is one (H1b).
    // It is built since H2, so the query reaches the route: an unknown session is its 404.
    let hub = |path: &str| Request::new(&perch.addr, "GET", path).bearer(format!("Bearer {}", perch.token.trim())).send(&perch.addr).status;
    assert_eq!(hub("/hub/tree?session=nobody&path=/tmp"), 404);
    assert_eq!(hub("/hub/sessions?x=1"), 200);
}

// ---------------------------------------------------------------------------
// Gate 2 and 3: Host, Sec-Fetch-Site
// ---------------------------------------------------------------------------

#[test]
fn a_bad_host_is_403_on_both_a_static_and_a_gated_request() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    let port = perch.port;

    // The static route first: no token needed, and the `Host` check is all there is. The
    // last two are forms the piece-wise comparison admitted — a stray bracket and a
    // bracketed v4 — and neither is a string the bound authority renders.
    for host in [
        format!("evil.example:{port}"),
        format!("127.0.0.1:{}", port + 1),
        "127.0.0.1".to_string(),
        "perch.example".to_string(),
        format!("[127.0.0.1]:{port}"),
        format!("127.0.0.1]:{port}"),
    ] {
        let answer = Request::new(&perch.addr, "GET", "/").host(host.clone()).send(&perch.addr);
        assert_eq!(answer.status, 403, "Host: {host} on a static: {}", answer.head);
        // A refusal is still an answer from this origin, so it carries the policy.
        answer.assert_static_headers(&format!("the 403 for Host: {host}"));
    }

    // Then behind the token gate: the gate order means the 403 needs the token, and a
    // caller without one gets the 401 that hides the route table.
    for (method, path) in [("GET", "/hub/anything"), ("POST", "/s/x/api/y")] {
        let answer = Request::new(&perch.addr, method, path).bearer(auth.clone()).host(format!("evil.example:{port}")).send(&perch.addr);
        assert_eq!(answer.status, 403, "{method} {path} with a bad Host: {}", answer.head);
        let no_token = Request::new(&perch.addr, method, path).host(format!("evil.example:{port}")).send(&perch.addr);
        assert_eq!(no_token.status, 401, "the token gate comes first: {}", no_token.head);
    }
}

#[test]
fn localhost_is_the_one_other_name_the_host_may_carry() {
    let perch = Perch::start();
    for host in [format!("localhost:{}", perch.port), format!("127.0.0.1:{}", perch.port)] {
        let answer = Request::new(&perch.addr, "GET", "/").host(host.clone()).send(&perch.addr);
        assert_eq!(answer.status, 200, "Host: {host}: {}", answer.head);
    }
    for host in [format!("localhost.localdomain:{}", perch.port), format!("localhost:{}", perch.port + 1), "localhost".to_string()] {
        let answer = Request::new(&perch.addr, "GET", "/").host(host.clone()).send(&perch.addr);
        assert_eq!(answer.status, 403, "Host: {host} is another name or port: {}", answer.head);
    }
}

#[test]
fn a_cross_site_sec_fetch_site_is_403_and_an_absent_one_passes() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    // A page from another origin, on a static path and behind the token.
    for (method, path, bearer) in [
        ("GET", "/", None),
        ("GET", "/hub/anything", Some(auth.clone())),
        ("POST", "/s/x/api/y", Some(auth.clone())),
    ] {
        let mut request = Request::new(&perch.addr, method, path).header("Sec-Fetch-Site", "cross-site");
        if let Some(bearer) = bearer {
            request = request.bearer(bearer);
        }
        let answer = request.send(&perch.addr);
        assert_eq!(answer.status, 403, "{method} {path} cross-site: {}", answer.head);
    }
    // Same-origin and `none` (a typed URL), and no header at all: all pass.
    for site in ["same-origin", "none"] {
        let answer = Request::new(&perch.addr, "GET", "/").header("Sec-Fetch-Site", site).send(&perch.addr);
        assert_eq!(answer.status, 200, "Sec-Fetch-Site: {site}: {}", answer.head);
    }
    assert_eq!(Request::new(&perch.addr, "GET", "/").send(&perch.addr).status, 200, "curl sends none");
    // `same-site` is another origin on this host, which is not ours.
    let answer = Request::new(&perch.addr, "GET", "/").header("Sec-Fetch-Site", "same-site").send(&perch.addr);
    assert_eq!(answer.status, 403, "{}", answer.head);
}

// ---------------------------------------------------------------------------
// Gate 4 and 5: method shape, and the body cap
// ---------------------------------------------------------------------------

#[test]
fn options_is_405_and_no_cors_header_answers_a_preflight() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    // On a static path, with no token: 405, and GET/HEAD are what it would answer.
    let static_options = Request::new(&perch.addr, "OPTIONS", "/").send(&perch.addr);
    assert_eq!(static_options.status, 405, "{}", static_options.head);
    assert_eq!(static_options.header("allow").as_deref(), Some("GET, HEAD"));
    static_options.assert_static_headers("an OPTIONS on a static");
    // The preflight headers a browser would send; none of them is answered.
    let preflight = Request::new(&perch.addr, "OPTIONS", "/hub/sessions")
        .header("Origin", "https://evil.example")
        .header("Access-Control-Request-Method", "POST")
        .header("Access-Control-Request-Headers", "authorization")
        .send(&perch.addr);
    assert_eq!(preflight.status, 401, "the token gate is ahead of the method: {}", preflight.head);
    let preflight = Request::new(&perch.addr, "OPTIONS", "/hub/sessions").bearer(auth.clone()).send(&perch.addr);
    assert_eq!(preflight.status, 405, "{}", preflight.head);

    for answer in [&static_options, &preflight] {
        assert!(!answer.has_cors(), "a preflight was answered with CORS: {}", answer.head);
        assert!(answer.header("allow-origin").is_none());
        assert!(answer.header("access-control-allow-origin").is_none());
        assert!(answer.header("access-control-allow-headers").is_none());
    }
}

#[test]
fn a_gated_method_that_is_not_get_or_post_is_405_before_its_body_is_read() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    // hub.md §3: only GET and POST have a route behind the token — the proxy takes those
    // two and nothing else — and the route table says 404/405 after the token for the rest.
    for method in ["PUT", "PATCH", "DELETE", "HEAD"] {
        let answer = Request::new(&perch.addr, method, "/hub/anything").bearer(auth.clone()).body(vec![b'x'; 16]).send(&perch.addr);
        assert_eq!(answer.status, 405, "{method} /hub/anything with the token: {}", answer.head);
        assert_eq!(answer.body, "", "a 405 says nothing: {method}");
    }
    // Over the cap it is still the method's answer, which is what "before the body step"
    // means: a PUT's body is never read, by the cap or by a route.
    let over = send_oversize(&perch.addr, "PUT", "/hub/anything", Some(&auth), 8 * 1024 * 1024 + 1);
    assert_eq!(over.status, 405, "a PUT over the cap is refused as a method: {}", over.head);
    // Without a token the method is not looked at at all: gate 1 is ahead of gate 4.
    let no_token = Request::new(&perch.addr, "PUT", "/hub/anything").send(&perch.addr);
    assert_eq!(no_token.status, 401, "the token gate is ahead of the method: {}", no_token.head);
}

#[test]
fn a_body_over_the_cap_behind_the_gate_is_413() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    // One byte over 8 MiB: refused before it is buffered, so the answer is the cap's
    // 413 and not the 404 a missing route would give.
    let over = send_oversize(&perch.addr, "POST", "/hub/anything", Some(&auth), 8 * 1024 * 1024 + 1);
    assert_eq!(over.status, 413, "{}", over.head);
    assert_eq!(over.body, "");
    // At the cap it is read and then the route that does not exist answers, which is
    // what makes the 413 above a boundary and not a blanket refusal of POSTs.
    let under = Request::new(&perch.addr, "POST", "/hub/anything").bearer(auth).body(vec![b'x'; 1024]).send(&perch.addr);
    assert_eq!(under.status, 404, "{}", under.head);
}

// ---------------------------------------------------------------------------
// Secrets: nothing anywhere ever prints the token
// ---------------------------------------------------------------------------

#[test]
fn the_token_never_appears_in_the_output_at_trace() {
    let mut perch = Perch::start();
    let token = perch.token.trim().to_string();
    let auth = format!("Bearer {token}");
    // Every way a token could reach a log line: a good header, a wrong one, a query
    // parameter, and a body over the cap. What the log holds is perch's own lines — hyper 1
    // emits no `tracing` events unless it is built with `--cfg hyper_unstable_tracing` — and
    // the run is at `RUST_LOG=trace`, so none of perch's lines is filtered out of it.
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/anything").bearer(auth.clone()).send(&perch.addr).status, 404);
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/anything").bearer("Bearer wrong".to_string()).send(&perch.addr).status, 401);
    assert_eq!(Request::new(&perch.addr, "GET", &format!("/hub/anything?token={token}")).send(&perch.addr).status, 401);
    assert_eq!(send_oversize(&perch.addr, "POST", "/hub/anything", Some(&auth), 8 * 1024 * 1024 + 1).status, 413);
    assert_eq!(Request::new(&perch.addr, "GET", "/").send(&perch.addr).status, 200);

    // Stop it, so the shutdown path's own lines are in the output too.
    let status = perch.stop();
    assert!(status.success(), "{status}");
    // The pump threads read the pipes to EOF once the child is gone.
    let deadline = Instant::now() + Duration::from_secs(10);
    while (!perch.err().contains("stopping") || perch.out().lines().count() < 2) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    let stdout = perch.out();
    let stderr = perch.err();
    assert_eq!(stdout.lines().count(), 2, "stdout is only the two boot lines: {stdout:?}");
    // Non-empty, which "stopping" on its own satisfies: a guard against a log this test
    // never sees, not evidence that a request was logged.
    assert!(stderr.len() > 0, "the trace output is not empty, so this test looks at something");
    assert!(!stdout.contains(&token), "the token is in stdout: {stdout:?}");
    assert!(!stderr.contains(&token), "the token is in the stderr log: {stderr:?}");
    // …and the request that carried it as a header, a query and a body left no trace of
    // the header itself either.
    assert!(!stdout.contains("Authorization"), "{stdout:?}");
    assert!(!stderr.contains("Authorization"), "{stderr:?}");
    assert!(!perch.boot.contains(&token), "the boot lines carry the token: {:?}", perch.boot);
}

/// hub.md §10.4 row 1 names this test: "read `/proc/<pid>/cmdline` and `environ` of hub and
/// children". The perch hands its token to nothing and takes one from nothing, so its own
/// is the only token `/proc` could show — and if it were passed as an argument or a
/// variable, the 0600 file would no longer be the only place it lives.
#[test]
fn the_token_is_not_in_the_childs_argv_or_environment() {
    let perch = Perch::start();
    let token = perch.token.trim().to_string();
    let pid = perch.child.id();
    let argv = std::fs::read(format!("/proc/{pid}/cmdline")).expect("the child's cmdline");
    let environ = std::fs::read(format!("/proc/{pid}/environ")).expect("the child's environ");
    // `cmdline` and `environ` are NUL-separated byte strings, so the token is a substring of
    // them if it is in them at all; read lossily, every entry is visible and a byte that is
    // not UTF-8 does not fail the read.
    let argv = String::from_utf8_lossy(&argv).to_string();
    let environ = String::from_utf8_lossy(&environ).to_string();
    // What was read is this test's own spawn: an empty or wrong read cannot pass.
    assert!(argv.contains("--bind"), "the child's argv was read: {argv:?}");
    assert!(environ.contains("XDG_RUNTIME_DIR"), "the child's environment was read");
    assert!(!argv.contains(&token), "the token is in the child's argv: {argv:?}");
    assert!(!environ.contains(&token), "the token is in the child's environment");
    // …nor the header a caller would present it in, which is a spelling a leak could use.
    assert!(!argv.contains("Authorization"), "the child's argv names the header: {argv:?}");
    assert!(!environ.contains("Authorization"), "the child's environment names the header");
}

// ---------------------------------------------------------------------------
// H1b: spawn, list, resume, stop — against the stand-in door (hub.md §4, §12)
// ---------------------------------------------------------------------------

/// The four routes H1b adds are behind the same gates as the namespace they live in, and the
/// cap still answers before any route does.
#[test]
fn the_session_routes_are_behind_the_same_gates_as_the_namespace() {
    let perch = Perch::start_hub(&[]);
    for (method, path) in [
        ("GET", "/hub/sessions"),
        ("POST", "/hub/sessions"),
        ("POST", "/hub/sessions/x/resume"),
        ("POST", "/hub/sessions/x/stop"),
    ] {
        // Gate 1: no token, and the answer is empty — a route that exists and one that does
        // not are the same 401.
        let answer = Request::new(&perch.addr, method, path).send(&perch.addr);
        assert_eq!(answer.status, 401, "{method} {path} without a token: {}", answer.head);
        assert_eq!(answer.body, "");
        // Gate 1 again: a wrong token is not a token.
        let wrong = Request::new(&perch.addr, method, path).bearer("Bearer wrong").send(&perch.addr);
        assert_eq!(wrong.status, 401, "{method} {path} with a wrong token: {}", wrong.head);
        // Gate 2: DNS rebinding, on the routes that spawn and stop processes too.
        let rebound = Request::new(&perch.addr, method, path).bearer(perch.auth()).host("evil.example").send(&perch.addr);
        assert_eq!(rebound.status, 403, "{method} {path} with a bad Host: {}", rebound.head);
        // Gate 3.
        let cross = Request::new(&perch.addr, method, path).bearer(perch.auth()).header("Sec-Fetch-Site", "cross-site").send(&perch.addr);
        assert_eq!(cross.status, 403, "{method} {path} cross-site: {}", cross.head);
    }
    // Gate 4: only POST reaches a verb, and a gated method that is neither GET nor POST is
    // 405 before its body is read.
    assert_eq!(perch.get("/hub/sessions/x/stop").status, 404, "a verb is POST only");
    assert_eq!(perch.apireq("DELETE", "/hub/sessions").send(&perch.addr).status, 405);
    // …and a query string is not part of either verb.
    assert_eq!(perch.post("/hub/sessions/x/stop?now=1").status, 404);
    // Gate 5: a body past the cap is still 413 on a route that exists.
    let over = send_oversize(&perch.addr, "POST", "/hub/sessions", Some(&perch.auth()), 8 * 1024 * 1024 + 1);
    assert_eq!(over.status, 413, "{}", over.head);
    assert!(perch.fake_records().is_empty(), "nothing was spawned for a refused body");
}

/// A new session's cwd is checked against `--root` (hub.md §4 step 1) before anything is
/// forked, and a body that is not the shape the route takes is refused before that.
#[test]
fn a_cwd_outside_every_root_is_422() {
    let perch = Perch::start_hub(&[]);
    let outside = tempfile::tempdir().unwrap();
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": outside.path().display().to_string() }));
    assert_eq!(answer.status, 422, "{}", answer.head);
    assert!(answer.body.contains("error"), "a refusal says why: {}", answer.body);
    // Not there at all, and not a directory: the same 422, because neither can be under a
    // root.
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({ "cwd": "/nowhere/at/all" })).status, 422);
    // Inside the root but not a directory: the same 422, and nothing is forked for it. (A
    // *directory* inside the root is exactly what this route is for; the UI dir is one.)
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.ui.join("index.html").display().to_string() })).status, 422);
    // A body that is not the shape the route takes, and one that is not JSON at all: 400.
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({})).status, 400);
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({ "cwd": 7 })).status, 400);
    assert_eq!(perch.apireq("POST", "/hub/sessions").body(b"not json".to_vec()).send(&perch.addr).status, 400);
    // Nothing was forked for any of them.
    assert!(perch.fake_records().is_empty(), "{:?}", perch.fake_records());
}

/// The log's stem, and nothing else: a bad id is a 404 (never a 400, so it cannot be told
/// from a route that does not exist) and never a path.
#[test]
fn a_bad_id_on_resume_or_stop_is_404_and_never_a_path() {
    let perch = Perch::start_hub(&[]);
    for id in ["..%2Fescape", "../escape", ".hidden", "-lead", "a%20b", "%2e%2e", &"x".repeat(129)] {
        for verb in ["resume", "stop"] {
            let path = format!("/hub/sessions/{id}/{verb}");
            let answer = perch.post(&path);
            assert_eq!(answer.status, 404, "POST {path}: {}", answer.head);
        }
    }
    // `..` as a whole path segment collapses the route itself, which is still a 404.
    assert_eq!(perch.post("/hub/sessions/../escape/resume").status, 404);
    assert_eq!(perch.post("/hub/sessions/ctf-pwn-3/../ctf-pwn-3/resume").status, 404);
    // An id that is well-formed but names no log is a 404 too, and one that is well-formed
    // and not live is a 409: the two are different answers on purpose.
    assert_eq!(perch.post("/hub/sessions/ctf-pwn-3/resume").status, 404);
    assert_eq!(perch.post("/hub/sessions/ctf-pwn-3/stop").status, 409);
}

/// The kept half of the list: `*.eid` in the sessions dir, by stem, with an mtime and no
/// cwd (the perch has not read the log, and reading it is not H1b's business).
#[test]
fn the_list_shows_kept_logs_and_nothing_else_in_the_directory() {
    let perch = Perch::start_hub(&[]);
    perch.keep_a_log("ctf-pwn-3");
    std::fs::write(perch.sessions.path().join("notes.txt"), "not a log\n").unwrap();
    std::fs::write(perch.sessions.path().join("-leading-dash.eid"), "{}\n").unwrap();
    let list = perch.list();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["id"], "ctf-pwn-3");
    assert_eq!(list[0]["state"], "kept");
    assert_eq!(list[0]["panes"], 0, "nothing proxied is open, so no pane is");
    assert!(list[0]["mtime"].as_u64().is_some(), "a kept log has an mtime: {}", list[0]);
    assert!(list[0].get("cwd").is_none(), "a kept log's cwd is not read: {}", list[0]);
}

/// A log the roster says another consumer holds is `live-elsewhere` in the list, and a
/// resume of it is refused (hub.md §4 step 2's roster half) — the perch never adopts a door
/// it did not start (Q5).
#[test]
fn a_log_another_consumer_holds_is_listed_and_not_resumable() {
    let perch = Perch::start_hub(&[]);
    let id = perch.keep_a_log("ctf-pwn-3");
    let log = perch.sessions.path().join("ctf-pwn-3.eid");
    perch.roster_row("eidolon-9f2c", &log, std::process::id());
    let row = perch.row(&id).expect("a kept log is listed");
    assert_eq!(row["state"], "live-elsewhere", "{row}");
    assert_eq!(row["title"], "a peer", "{row}");
    let refused = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(refused.status, 409, "{}", refused.head);
    assert!(perch.fake_records().is_empty(), "no door was forked for a log someone else holds");
}

// ---------------------------------------------------------------------------
// H1b: the lifecycle against the real stand-in door (spawn -> live -> stop -> resume)
// ---------------------------------------------------------------------------

/// The whole of hub.md §4 in one test, because the steps are only meaningful in order:
/// spawn a new session, see it `live` through the perch's *own* watcher, stop it cleanly,
/// and resume the kept log.
#[cfg(feature = "test-bins")]
#[test]
fn a_new_session_spawns_reaches_live_and_stops_clean() {
    let perch = Perch::start_hub(&[]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let id = body_id(&answer);
    assert!(!id.is_empty() && !id.contains('/'), "the answer's id is a log stem: {id}");

    // (a) §4 step 3: the argv is the door's own, with `--cwd` and no token anywhere in it.
    let (pid, port) = perch.fake_last_start();
    let argv = perch.fake_argv();
    assert!(argv.contains("--cwd"), "the door is told where to work: {argv}");
    assert!(argv.contains(&perch.work.display().to_string()), "{argv}");
    assert!(!argv.contains("--session"), "a new session is not a resume: {argv}");
    assert!(!argv.contains("--ui-dir"), "the door serves no statics under the perch: {argv}");
    assert!(!argv.contains(&perch.token.trim()), "the perch's token is not in the argv: {argv}");
    assert!(!argv.contains("Bearer") && !argv.contains("Authorization"), "…nor a header: {argv}");
    // …and the same for the child's bytes, which is hub.md §10.4 row 1's own test.
    let (cmdline, environ) = proc_of(pid);
    assert!(cmdline.contains("--cwd"), "the child's argv was read: {cmdline:?}");
    assert!(environ.contains("XDG_RUNTIME_DIR"), "the child's environment was read");
    assert!(!cmdline.contains(&perch.token.trim()), "the perch's token is in the child's argv");
    assert!(!environ.contains(&perch.token.trim()), "the perch's token is in the child's environment");
    assert!(!cmdline.contains("Authorization") && !environ.contains("Authorization"));

    // The door's own token: minted by the door, read from its `0600` file, and in the
    // watcher's `Authorization` header — which is the only place it ever goes.
    let door_token = perch.door_token(port);
    assert!(door_token.len() >= 20, "the door minted a real token: {door_token:?}");
    assert!(!cmdline.contains(&door_token), "the door's token is not in its own argv either");
    assert!(!environ.contains(&door_token), "…nor its environment");
    let records = perch.fake_records();
    assert!(
        records.iter().any(|line| line == &format!("GET /api/events auth=Bearer {door_token}")),
        "the watcher asked for the door's own stream with the door's own token: {records:?}"
    );

    // (b) §4 step 7: `hello` was read, so the door is live and its log is a kept session.
    let row = perch.row(&id).expect("the new session is listed");
    assert_eq!(row["state"], "live", "{row}");
    assert_eq!(row["cwd"], perch.work.canonicalize().unwrap().display().to_string(), "hello's cwd is the row's: {row}");
    assert_eq!(row["panes"], 0, "nothing proxied is open, so no pane is");
    assert!(perch.sessions.path().join(format!("{id}.eid")).is_file(), "the door wrote the log it named");

    // (c) The Stop finding: the watcher is closed *before* the door is signalled, so the
    // door is not holding a stream open and dies at once.
    let started = Instant::now();
    let stopped = perch.post(&format!("/hub/sessions/{id}/stop"));
    let took = started.elapsed();
    assert_eq!(stopped.status, 202, "{}", stopped.head);
    assert!(took < Duration::from_secs(5), "a clean stop does not wait the grace out: {took:?}");
    assert!(wait_gone(pid, Duration::from_secs(5)), "the door is gone, and reaped");
    assert!(!perch.door_token_file(port).exists(), "the door removed its own token file");
    assert!(perch.door_token_files().is_empty(), "no orphan token file: {:?}", perch.door_token_files());
    let records = perch.fake_records();
    let closed = records.iter().position(|line| line == "stream-closed").expect("the watcher was closed");
    let term = records.iter().position(|line| line.starts_with("sigterm ")).expect("the door was signalled");
    assert!(closed < term, "close the watcher, then signal: {records:?}");
    assert!(records.iter().any(|line| line == "sigterm streams=0"), "…with no stream left open: {records:?}");

    // (d) The log is kept, so the session is still there — and resumable.
    let row = perch.row(&id).expect("a stopped session is still listed");
    assert_eq!(row["state"], "kept", "a cleanly stopped session is kept (hub.md section 4), not dead: {row}");
    let resumed = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(resumed.status, 200, "{}{}", resumed.head, resumed.body);
    assert_eq!(body_id(&resumed), id, "a resume answers with the id it resumed");
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));
    let (again_pid, again_port) = perch.fake_last_start();
    assert_ne!(again_pid, pid, "a resume is a new door, not the old one");
    let argv = perch.fake_argv();
    assert!(argv.contains("--session"), "{argv}");
    assert!(argv.contains(&format!("{id}.eid")), "on the log it was asked for: {argv}");
    assert!(perch.door_token(again_port).len() >= 20, "the new door minted a token of its own");

    // Leave nothing running: the same ordered stop, once more.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(wait_gone(again_pid, Duration::from_secs(5)));
    assert!(perch.door_token_files().is_empty());
}

/// The Minerva home: the perch generates `<home>/config.toml` from the member's config, every door's
/// argv carries it (new and resumed), a new session with no cwd works in its own `<home>/scratch/<stamp>`,
/// and without `--home` that same body is the 400 it always was. Reverting any of these fails here.
#[cfg(feature = "test-bins")]
#[test]
fn the_minerva_home_gives_every_door_its_config_and_a_cwd_less_session_its_scratch() {
    // Created inside `--root` (the hub harness's), where the perch can canonicalize it at start.
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("minerva.toml"), "default_model = \"mock\"\n").unwrap();
    let home = home.canonicalize().unwrap();
    let config = home.join("config.toml");
    // A stale hand-written config.toml is regenerated away, not honoured.
    std::fs::write(&config, "sessions_dir = \"/hand/edited\"\n").unwrap();
    let member = "sessions_dir = \"~/elsewhere\"\nthinking_budget = 4000\n\n[swarm]\nenabled = false\n\n[[providers]]\nname = \"mine\"\n";
    let perch = Perch::start_hub_member(
        &["--root".into(), base.path().canonicalize().unwrap().into(), "--home".into(), home.clone().into()],
        member,
    );
    let generated: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
    assert_eq!(generated["sessions_dir"].as_str(), Some(home.join("sessions").to_str().unwrap()), "forced, absolute");
    assert_eq!(generated["default_model"].as_str(), Some("mock"), "from minerva.toml, the member names none");
    assert_eq!(generated["thinking_budget"].as_integer(), Some(4000), "the member's keys survive");
    assert_eq!(generated["swarm"]["enabled"].as_bool(), Some(false));
    assert_eq!(generated["providers"][0]["name"].as_str(), Some("mine"));

    let answer = perch.post_json("/hub/sessions", serde_json::json!({}));
    assert_eq!(answer.status, 201, "no cwd + a home is a scratch dir: {}{}", answer.head, answer.body);
    let id = body_id(&answer);
    let argv = perch.fake_argv();
    let scratch = home.join("scratch");
    assert!(argv.starts_with(&format!("argv --config {} web ", config.display())), "--config leads the door's argv: {argv}");
    let row = perch.list().into_iter().find(|row| row["id"] == id.as_str()).expect("the new session is listed");
    let cwd = PathBuf::from(row["cwd"].as_str().unwrap());
    assert_eq!(cwd.parent(), Some(scratch.as_path()), "a fresh directory under <home>/scratch: {cwd:?}");
    assert!(cwd.is_dir() && argv.contains(&format!("--cwd {}", cwd.display())), "{argv}");

    // A second chat gets its own directory, so two chats never share files.
    let other = perch.post_json("/hub/sessions", serde_json::json!({}));
    assert_eq!(other.status, 201, "{}{}", other.head, other.body);
    let other_id = body_id(&other);
    let other_row = perch.list().into_iter().find(|row| row["id"] == other_id.as_str()).unwrap();
    let other_cwd = PathBuf::from(other_row["cwd"].as_str().unwrap());
    assert_ne!(other_cwd, cwd, "one scratch directory per chat");
    assert_eq!(other_cwd.parent(), Some(scratch.as_path()));
    assert_eq!(perch.post(&format!("/hub/sessions/{other_id}/stop")).status, 202);

    // A named cwd still wins, and is still judged against the roots.
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({ "cwd": "/nowhere/at/all" })).status, 422);

    // Resume: the same config on the reopened door.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    let (pid, _) = perch.fake_last_start();
    assert!(wait_gone(pid, Duration::from_secs(5)));
    let resumed = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(resumed.status, 200, "{}{}", resumed.head, resumed.body);
    let argv = perch.fake_argv();
    assert!(argv.starts_with(&format!("argv --config {} web ", config.display())), "a resume carries the config too: {argv}");
    assert!(argv.contains("--session"), "{argv}");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// The listing follows the config the doors run with. With `--home` that is `<home>/sessions` whatever
/// the member's config says; with an explicit `--eidolon-config` and no `--sessions-dir` it is the file's own
/// (absolute) `sessions_dir`. Mutations: list the shared default dir instead (the `a` row is missing).
#[test]
fn the_listing_follows_the_config_the_doors_run_with() {
    // --home: a log in <home>/sessions is listed, and the member's own sessions_dir is not.
    let base = tempfile::tempdir().unwrap();
    let theirs = base.path().join("theirs");
    std::fs::create_dir(&theirs).unwrap();
    std::fs::write(theirs.join("t.eid"), "{}\n").unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(home.join("sessions/a.eid"), "{}\n").unwrap();
    let member = format!("sessions_dir = \"{}\"\n", theirs.display());
    let perch = Perch::launch_with_member(true, None, &["--home".into(), home.clone().into(), "--root".into(), base.path().into()], Some(&member));
    let ids: Vec<String> = perch.list().iter().map(|row| row["id"].as_str().unwrap().to_string()).collect();
    // The member's own dir is eidolon's: the read-only second source, listed and flagged, never the home's.
    assert_eq!(ids, ["a", "t"], "<home>/sessions, then the member's directory as the second source");
    let rows = perch.list();
    assert!(rows.iter().find(|r| r["id"] == "a").unwrap().get("origin").is_none());
    assert_eq!(rows.iter().find(|r| r["id"] == "t").unwrap()["origin"], "other");
    drop(perch);

    // An explicit config, no home: its absolute sessions_dir is the listing.
    let config = base.path().join("c.toml");
    std::fs::write(&config, format!("sessions_dir = '''{}'''\n", theirs.display())).unwrap();
    let mut args: Vec<std::ffi::OsString> = vec!["--eidolon-config".into(), config.clone().into()];
    let perch = Perch::launch(true, None, &args);
    let ids: Vec<String> = perch.list().iter().map(|row| row["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids, ["t"], "the explicit config's own sessions_dir");
    drop(perch);

    // A `~` or relative sessions_dir would be a different directory to the door: the perch refuses to start.
    for bad in ["~/x", "rel/x"] {
        std::fs::write(&config, format!("sessions_dir = \"{bad}\"\n")).unwrap();
        args = vec!["--eidolon-config".into(), config.clone().into()];
        let out = Perch::refused_start(&args);
        assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("not an absolute path"), "{bad}: {:?}", out);
    }
}

/// Without `--home` and `--eidolon-config` nothing changes: no `--config` in the argv, and a
/// body with no cwd is refused.
#[cfg(feature = "test-bins")]
#[test]
fn without_a_home_the_door_argv_has_no_config_and_a_cwd_is_required() {
    let perch = Perch::start_hub(&[]);
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({})).status, 400);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let argv = perch.fake_argv();
    assert!(argv.starts_with("argv web "), "no --config: {argv}");
    let id = body_id(&answer);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// The second half of the Stop finding: a door that ignores SIGTERM is killed after the
/// grace, and the token file it could not remove is the perch's to remove.
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_ignores_sigterm_is_killed_and_the_perch_removes_its_token_file() {
    let perch = Perch::start_hub(&[("FAKE_IGNORE_SIGTERM", "1")]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let id = body_id(&answer);
    let (pid, port) = perch.fake_last_start();
    assert!(perch.door_token_file(port).exists(), "the door wrote its token file");

    let started = Instant::now();
    let stopped = perch.post(&format!("/hub/sessions/{id}/stop"));
    let took = started.elapsed();
    assert_eq!(stopped.status, 202, "{}", stopped.head);
    // hub.md §4 "Stop": SIGTERM, then 10 s, then SIGKILL. The wait is the perch's, and the
    // record of it is this duration.
    assert!(took >= Duration::from_secs(10), "the perch waited its grace out: {took:?}");
    assert!(took < Duration::from_secs(20), "…and not longer: {took:?}");
    assert!(wait_gone(pid, Duration::from_secs(5)), "the SIGKILL landed");
    assert!(
        perch.fake_records().iter().any(|line| line == "sigterm-ignored"),
        "the door ignored the SIGTERM it was sent: {:?}",
        perch.fake_records()
    );
    // The door never got to run its own `remove_file`, so this is the perch's work.
    assert!(!perch.door_token_file(port).exists(), "an orphaned door token file was left behind");
    assert!(perch.door_token_files().is_empty(), "{:?}", perch.door_token_files());
    // A SIGKILL is not a clean exit: the row stays `dead` and the proxy answers 503.
    assert_eq!(perch.state_of(&id).as_deref(), Some("dead"));
    let gone = perch.apireq("GET", &format!("/s/{id}/api/events")).send(&perch.addr);
    assert_eq!((gone.status, gone.body.as_str()), (503, "{\"state\":\"dead\"}"), "{}", gone.head);
}

/// A door that dies by itself is a `dead` row, and its token file does not outlive it.
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_exits_on_its_own_leaves_a_dead_row() {
    let perch = Perch::start_hub(&[("FAKE_EXIT_AFTER_MS", "2000")]);
    let id = perch.keep_a_log("crashy");
    let resumed = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(resumed.status, 200, "the door reached hello first: {}", resumed.head);
    let (pid, port) = perch.fake_last_start();
    assert!(perch.wait_state(&id, "dead", Duration::from_secs(15)), "the child exited: {:?}", perch.state_of(&id));
    assert!(wait_gone(pid, Duration::from_secs(5)));
    assert!(!perch.door_token_file(port).exists(), "a door that died left its token file to the perch");
    // The log is still there, so a resume is still allowed after the crash.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// hub.md §4 step 2: two writers on one log are unguarded upstream, so the perch refuses the
/// second one — ours, and someone else's.
#[cfg(feature = "test-bins")]
#[test]
fn a_second_writer_on_one_log_is_409() {
    let perch = Perch::start_hub(&[]);
    let id = perch.keep_a_log("one-writer");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    let (pid, _port) = perch.fake_last_start();
    let held = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(held.status, 409, "{}", held.head);
    assert!(held.body.contains("error"), "{}", held.body);
    // Only one door was forked for the two requests.
    let starts = perch.fake_records().iter().filter(|line| line.starts_with("start ")).count();
    assert_eq!(starts, 1, "{:?}", perch.fake_records());
    // A live-elsewhere log is refused as well, before anything is forked.
    perch.roster_row("eidolon-9f2c", &perch.sessions.path().join("one-writer.eid"), std::process::id());
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(wait_gone(pid, Duration::from_secs(5)));
    let after = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(after.status, 409, "the roster still holds the log: {}", after.head);
    // With the roster gone (the peer stopped), the log is resumable again.
    std::fs::remove_dir_all(perch.runtime.path().join("eidolon/eidolon-9f2c")).unwrap();
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// §4 step 5: a door that never prints its boot lines is killed at the startup timeout, the
/// spawn is a 503, and the row it reserved is `dead` rather than stuck `starting`.
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_never_boots_is_503_and_dead() {
    let perch = Perch::start_hub(&[("FAKE_SILENT", "1")]);
    let id = perch.keep_a_log("silent-session");
    let started = Instant::now();
    let answer = perch.post(&format!("/hub/sessions/{id}/resume"));
    let took = started.elapsed();
    assert_eq!(answer.status, 503, "{}", answer.head);
    assert!(took >= Duration::from_secs(20), "the perch waited its startup timeout: {took:?}");
    assert!(took < Duration::from_secs(40), "…and not longer: {took:?}");
    assert_eq!(perch.state_of(&id).as_deref(), Some("dead"), "a door that never came up is dead, not starting");
    assert!(perch.door_token_files().is_empty(), "the silent door removed its own token file: {:?}", perch.door_token_files());
}

/// The same startup timeout on the *new session* path, where there is no id to reserve: the
/// answer is a 503 and the list is unchanged (nothing names a session that never was).
#[cfg(feature = "test-bins")]
#[test]
fn a_new_session_that_never_boots_is_503() {
    let perch = Perch::start_hub(&[("FAKE_SILENT", "1")]);
    let started = Instant::now();
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 503, "{}", answer.head);
    assert!(started.elapsed() >= Duration::from_secs(20), "the startup timeout, again");
    // No session was *invented*: a silent door did write a log of its own (a real one does,
    // before it prints), but nothing is live or starting, because nothing reached hello.
    let live: Vec<_> = perch.list().into_iter().filter(|row| row["state"] == "live" || row["state"] == "starting").collect();
    assert!(live.is_empty(), "nothing was adopted: {live:?}");
    assert!(perch.door_token_files().is_empty(), "{:?}", perch.door_token_files());
}

/// §4 step 6: a door token that is world-readable is refused, not read. The door ignores
/// SIGTERM here, so the file's removal is the perch's and not the door's.
#[cfg(feature = "test-bins")]
#[test]
fn a_door_token_that_is_not_owner_only_is_refused() {
    let perch = Perch::start_hub(&[("FAKE_MODE", "0644"), ("FAKE_IGNORE_SIGTERM", "1")]);
    let id = perch.keep_a_log("loose-token");
    let answer = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(answer.status, 503, "{}", answer.head);
    assert_eq!(perch.state_of(&id).as_deref(), Some("dead"));
    let (_pid, port) = perch.fake_last_start();
    let token_file = perch.door_token_file(port);
    if token_file.exists() {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&token_file).unwrap().permissions().mode() & 0o777, 0o644, "the fake really wrote a loose file");
    }
    assert!(!token_file.exists(), "the perch removed the token file it refused to read");
}

/// §4 step 6: a token file outside `<runtime>/eidolon` is refused, and the path is not one
/// the perch will touch either — a path a child printed is not a file to delete.
#[cfg(feature = "test-bins")]
#[test]
fn a_token_file_outside_the_runtime_dir_is_refused() {
    let perch = Perch::start_hub(&[("FAKE_TOKEN_ELSEWHERE", "1")]);
    let id = perch.keep_a_log("elsewhere-token");
    let answer = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(answer.status, 503, "{}", answer.head);
    assert!(answer.body.contains("eidolon"), "the refusal names what it expected: {}", answer.body);
    assert_eq!(perch.state_of(&id).as_deref(), Some("dead"));
    let (_pid, port) = perch.fake_last_start();
    assert!(!perch.door_token_file(port).exists(), "nothing was adopted into the runtime dir");
    let _ = std::fs::remove_file(std::env::temp_dir().join(format!("fake-door-{port}.token")));
}

/// hub.md §2's "Shutdown" row: SIGTERM the perch and its doors go with it — the watcher is
/// closed first, so a door that would otherwise wait its stream out exits at once.
#[cfg(feature = "test-bins")]
#[test]
fn a_perch_shutdown_stops_its_doors_and_leaves_no_token_file() {
    let mut perch = Perch::start_hub(&[]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let (pid, port) = perch.fake_last_start();
    assert!(perch.door_token_file(port).exists());

    let started = Instant::now();
    let status = perch.stop();
    assert!(status.success(), "a signalled stop is a clean exit: {status}");
    assert!(started.elapsed() < Duration::from_secs(5), "the perch does not wait its children out: {:?}", started.elapsed());
    assert!(wait_gone(pid, Duration::from_secs(5)), "the door did not outlive the perch");
    assert!(!perch.door_token_file(port).exists(), "the door's token file outlived the listener");
    assert!(!perch.token_file.exists(), "the perch's own token file outlived the listener");
    let records = perch.fake_records();
    assert!(records.iter().any(|line| line == "sigterm streams=0"), "the shutdown closed the watcher first: {records:?}");
}

/// hub.md §2's PDEATHSIG hypothesis, measured the way the row says to: `kill -9` the perch
/// and the doors must not be left behind. The child is forked by the perch's one long-lived
/// thread precisely so this signal arrives when the *process* dies.
#[cfg(feature = "test-bins")]
#[test]
fn kill_minus_nine_on_the_perch_leaves_no_door_behind() {
    let perch = Perch::start_hub(&[]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let (pid, port) = perch.fake_last_start();
    let perch_pid = perch.child.id() as i32;
    assert_eq!(unsafe { libc::kill(perch_pid, libc::SIGKILL) }, 0, "kill -9 the perch");
    assert!(wait_gone(pid, Duration::from_secs(10)), "PDEATHSIG did not reach the door");
    // A door that dies of SIGTERM removes its own token file, so nothing is orphaned here.
    let deadline = Instant::now() + Duration::from_secs(5);
    while perch.door_token_file(port).exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!perch.door_token_file(port).exists(), "the door never removed its token file");
}

/// hub.md §10.4: neither token — the perch's or a door's — reaches the perch's output, and
/// no answer body carries one. Run with a whole lifecycle so every path's log lines exist.
#[cfg(feature = "test-bins")]
#[test]
fn neither_token_reaches_the_output_or_an_answer() {
    // `FAKE_REFUSE` makes `cancel` a door 401, so a 502 is among the proxied answers below.
    let mut perch = Perch::start_hub(&[("FAKE_REFUSE", "/api/cancel")]);
    let perch_token = perch.token.trim().to_string();
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let id = body_id(&answer);
    let (pid, port) = perch.fake_last_start();
    let door_token = perch.door_token(port);

    // H1c: proxied traffic of every kind — a say, an answer, an open stream, and a 502 — and
    // neither token in any head or body the page was handed. (The door's token is in exactly
    // one header, the perch -> door one; the page's is in the one it sent.)
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));
    let said = perch.post_json(&format!("/s/{id}/api/say"), serde_json::json!({ "text": "hi" }));
    let answered = perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": 1, "answer": "ok", "note": "n" }));
    let refused = perch.post_json(&format!("/s/{id}/api/cancel"), serde_json::json!({}));
    assert_eq!((said.status, answered.status, refused.status), (202, 204, 502), "{}{}{}", said.head, answered.head, refused.head);
    assert!(pane.read_until("ask-settled", Duration::from_secs(5)));
    for (what, head, body) in [
        ("a say", said.head.as_str(), said.body.as_str()),
        ("an answer", answered.head.as_str(), answered.body.as_str()),
        ("a 502", refused.head.as_str(), refused.body.as_str()),
        ("a pane", pane.head.as_str(), pane.data().0.as_str()),
    ] {
        for token in [&door_token, &perch_token] {
            assert!(!head.contains(token.as_str()) && !body.contains(token.as_str()), "a token is in {what}: {head} {body}");
        }
        assert!(!head.to_ascii_lowercase().contains("authorization"), "{what} names the header: {head}");
    }
    drop(pane);

    // Every answer a page can read, including the refusals.
    let list = perch.get("/hub/sessions");
    assert_eq!(list.status, 200);
    assert!(!list.body.contains(&door_token), "a door token is in the list: {}", list.body);
    assert!(!list.body.contains(&perch_token), "the perch token is in the list");
    let conflict = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(conflict.status, 409, "{}", conflict.head);
    assert!(!conflict.body.contains(&door_token) && !conflict.body.contains(&perch_token));
    let refused = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": "/nowhere" }));
    assert_eq!(refused.status, 422);
    assert!(!refused.body.contains(&door_token) && !refused.body.contains(&perch_token));
    assert!(!perch.get("/hub/sessions/../x/resume").body.contains(&door_token));

    // …and the process's own output. The door is stopped first, so the shutdown path's own
    // lines are in it too, and then the perch.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(wait_gone(pid, Duration::from_secs(5)));
    let status = perch.stop();
    assert!(status.success(), "{status}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !perch.err().contains("stopping") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let stdout = perch.out();
    let stderr = perch.err();
    assert_eq!(stdout.lines().count(), 2, "stdout is only the two boot lines: {stdout:?}");
    assert!(stderr.len() > 0, "the trace output is not empty, so this test looks at something");
    for (what, token) in [("the perch's token", &perch_token), ("a door's token", &door_token)] {
        assert!(!stdout.contains(token.as_str()), "{what} is in stdout: {stdout:?}");
        assert!(!stderr.contains(token.as_str()), "{what} is in stderr");
        assert!(!perch.boot.contains(token.as_str()), "{what} is in the boot lines");
    }
    assert!(!stdout.contains("Authorization") && !stderr.contains("Authorization"), "{stderr}");
}

// ---------------------------------------------------------------------------
// H1c: the proxy, `/s/<id>/api/<route>` (hub.md §5, §6)
// ---------------------------------------------------------------------------

/// Decode as many whole chunks as `raw` holds: the bytes, and whether the terminal chunk has
/// been seen. A page reads a proxied stream through this, a chunk at a time.
fn dechunk(raw: &[u8]) -> (Vec<u8>, bool) {
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
struct Pane {
    sock: TcpStream,
    /// The chunked body as received, undecoded.
    raw: Vec<u8>,
    head: String,
    /// The socket reached end-of-file (the perch closed it).
    eof: bool,
}

#[cfg(feature = "test-bins")]
impl Pane {
    fn open(perch: &Perch, id: &str) -> Self {
        Self::adopt(Self::begin(perch, id))
    }

    /// The request, sent, and nothing read: a pane the perch may still be setting up.
    fn begin(perch: &Perch, id: &str) -> TcpStream {
        let mut sock = TcpStream::connect(&perch.addr).unwrap();
        let head = format!("GET /s/{id}/api/events HTTP/1.1\r\nHost: {}\r\nAuthorization: {}\r\nAccept: text/event-stream\r\n\r\n", perch.addr, perch.auth());
        sock.write_all(head.as_bytes()).unwrap();
        sock
    }

    /// [`Pane::begin`] with another credential line (`Cookie: ...`) in place of the bearer.
    fn begin_with(perch: &Perch, id: &str, credential: &str) -> TcpStream {
        let mut sock = TcpStream::connect(&perch.addr).unwrap();
        let head = format!("GET /s/{id}/api/events HTTP/1.1\r\nHost: {}\r\n{credential}\r\nAccept: text/event-stream\r\n\r\n", perch.addr);
        sock.write_all(head.as_bytes()).unwrap();
        sock
    }

    /// The response head of a pane that [`Pane::begin`] sent for, read.
    fn adopt(mut sock: TcpStream) -> Self {
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

    fn pump(&mut self) {
        let mut buf = [0u8; 16 * 1024];
        match self.sock.read(&mut buf) {
            Ok(0) => self.eof = true,
            Ok(n) => self.raw.extend_from_slice(&buf[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => self.eof = true,
        }
    }

    /// What the page has been handed so far, and whether the stream has ended cleanly.
    fn data(&self) -> (String, bool) {
        let (bytes, finished) = dechunk(&self.raw);
        (String::from_utf8_lossy(&bytes).to_string(), finished)
    }

    /// Read until `needle` is in the stream, the stream ends, or `within` passes. `true` only
    /// for the first: a stream that is still open and silent is a `false`, not a hang.
    fn read_until(&mut self, needle: &str, within: Duration) -> bool {
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
    fn read_to_end(&mut self, within: Duration) -> bool {
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
    fn frames(&self) -> Vec<serde_json::Value> {
        self.data().0.lines().filter_map(|line| line.strip_prefix("data: ")).filter_map(|json| serde_json::from_str(json).ok()).collect()
    }

    /// The perch's own last frame, and the assertion that there is exactly one of it: a stream
    /// that ends gets one `hub-stream-end`, not one per path that noticed the end.
    fn end_frame(&self) -> serde_json::Value {
        let mut ends: Vec<serde_json::Value> = self.frames().into_iter().filter(|f| f["type"] == "hub-stream-end").collect();
        assert_eq!(ends.len(), 1, "exactly one hub-stream-end in {:?}", self.data().0);
        ends.remove(0)
    }
}

/// A live session under the stand-in door: its id, pid and port.
#[cfg(feature = "test-bins")]
fn live(perch: &Perch) -> (String, u32, u16) {
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let (pid, port) = perch.fake_last_start();
    (body_id(&answer), pid, port)
}

/// The requests the stand-in door has recorded since `before` lines ago.
#[cfg(feature = "test-bins")]
fn since(perch: &Perch, before: usize) -> Vec<String> {
    perch.fake_records().into_iter().skip(before).collect()
}

/// A bounded poll of the door's log, because a request the perch has made is a fact that
/// reaches the file a moment later.
#[cfg(feature = "test-bins")]
fn wait_record(perch: &Perch, within: Duration, wanted: impl Fn(&[String]) -> bool) -> bool {
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

/// A body of base64-looking characters, as a `say` with two images carries: deterministic, and
/// not a repeating byte, so a body that was truncated, reordered or re-chunked wrongly differs.
fn images_body(total: usize) -> Vec<u8> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut seed: u32 = 0x1234_5678;
    let mut image = |len: usize| -> String {
        (0..len)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                alphabet[(seed >> 24) as usize % alphabet.len()] as char
            })
            .collect()
    };
    let (a, b) = (image(total / 2), image(total / 2));
    format!("{{\"text\":\"look\",\"images\":[{{\"mime\":\"image/png\",\"data\":\"{a}\"}},{{\"mime\":\"image/png\",\"data\":\"{b}\"}}]}}").into_bytes()
}

/// A chunked POST of `len` bytes with the body going out on another thread, like
/// [`send_oversize`] but with no `Content-Length` for anything to refuse up front: only the
/// cap on the stream can stop it. A write error is the refusal, not a failure.
fn send_chunked(addr: &str, path: &str, auth: &str, len: usize) -> Answer {
    let head = format!("POST {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n");
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
            let framed = [format!("{take:x}\r\n").as_bytes(), &chunk[..take], b"\r\n"].concat();
            if writer.write_all(&framed).is_err() {
                return;
            }
            written += take;
        }
        let _ = writer.write_all(b"0\r\n\r\n");
    });
    let mut raw = Vec::new();
    let _ = sock.read_to_end(&mut raw);
    let _ = body.join();
    Answer::parse(&raw)
}

/// 1. The rewrite table (hub.md §5). A page request with every header a browser would add is
///    turned into a door request with five facts and nothing else.
///
/// Mutations: drop the `Authorization` rewrite (the stand-in 401s, the perch answers 502);
/// forward `Cookie` (the `cookie=` field is not `-`); drop the `Host` rewrite (the stand-in
/// 403s a POST whose `Host` is not its own).
#[cfg(feature = "test-bins")]
#[test]
fn a_proxied_request_is_rebuilt_and_carries_the_doors_bearer_and_nothing_of_the_pages() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, port) = live(&perch);
    let door_token = perch.door_token(port);
    let before = perch.fake_records().len();

    let answer = perch
        .apireq("POST", &format!("/s/{id}/api/say"))
        .header("Cookie", "sid=abc")
        .header("Origin", "https://evil.example")
        .header("Referer", "https://evil.example/x")
        .header("Sec-Fetch-Site", "same-origin")
        .header("Last-Event-ID", "42")
        .header("Accept", "application/json")
        .body(b"{\"text\":\"hi\"}".to_vec())
        .send(&perch.addr);
    assert_eq!(answer.status, 202, "{}{}", answer.head, answer.body);
    assert_eq!(answer.body, "{\"queued\":false}", "the door's body, unchanged");

    let records = since(&perch, before);
    let at = records.iter().position(|line| line == &format!("POST /api/say auth=Bearer {door_token}")).unwrap_or_else(|| panic!("the door's own Bearer: {records:?}"));
    assert_eq!(
        records[at + 1],
        format!("hdr host=127.0.0.1:{port} content-type=application/json accept=application/json cookie=- origin=- referer=- sec-fetch-site=- last-event-id=-"),
        "five facts, and nothing of the page's: {records:?}"
    );
    let perch_token = perch.token.trim();
    assert!(records.iter().all(|line| !line.contains(perch_token)), "the perch's token reached the door: {records:?}");
    assert!(perch.fake_records().iter().all(|line| !line.contains(perch_token)), "…or its log");

    // The response: the door's status and content type, `no-store`, and nothing that sets state.
    assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{}", answer.head);
    assert_eq!(answer.header("cache-control").as_deref(), Some("no-store"), "{}", answer.head);
    assert_eq!(answer.header("x-content-type-options").as_deref(), Some("nosniff"));
    assert_eq!(answer.header("referrer-policy").as_deref(), Some("no-referrer"));
    assert!(answer.header("set-cookie").is_none(), "nothing that sets state: {}", answer.head);
    assert!(!answer.head.contains(&door_token) && !answer.body.contains(&door_token), "a door token reached the page");
}

/// 2. The door's contract at 0194f2b passes through with no route-specific code: notes on an
///    answer, `launch`, a multi-MiB `say` byte for byte, a refused `documents`, a route the
///    perch has never heard of, and every malformed path shape.
///
/// Mutations: an allowlist of route names turns (e) red; touching the body turns (c) red;
/// parsing or rewriting a frame turns (a) red.
#[cfg(feature = "test-bins")]
#[test]
fn the_doors_current_contract_passes_through_with_no_route_specific_code() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);

    // (a) An ask note round-trips: opaque to the perch, and byte for byte on the pane.
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)), "{:?}", pane.data());
    let note = "because \"quotes\", a \\ backslash, a newline\n and é ☃";
    let answered = perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": 7, "answer": "yes", "note": note }));
    assert_eq!(answered.status, 204, "{}{}", answered.head, answered.body);
    assert_eq!(answered.body, "");
    assert!(pane.read_until("ask-settled", Duration::from_secs(5)), "{:?}", pane.data());
    let settled = pane.frames().into_iter().find(|f| f["type"] == "ask-settled").expect("the ask-settled frame");
    assert_eq!((settled["ask_id"].as_u64(), settled["answer"].as_str(), settled["how"].as_str()), (Some(7), Some("yes"), Some("answered")));
    assert_eq!(settled["note"].as_str(), Some(note), "the note, byte for byte");
    // …and without a note the frame has none.
    let bare = perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": 8, "answer": "no" }));
    assert_eq!(bare.status, 204);
    assert!(pane.read_until("\"ask_id\":8", Duration::from_secs(5)));
    assert!(pane.frames().iter().find(|f| f["ask_id"] == 8).is_some_and(|f| f.get("note").is_none()));
    // A body the door refuses is the door's 400, passed through.
    assert_eq!(perch.apireq("POST", &format!("/s/{id}/api/answer")).body(b"not json".to_vec()).send(&perch.addr).status, 400);

    // (b) launch: 200, JSON, and the door's own body.
    let launched = perch.post_json(&format!("/s/{id}/api/launch"), serde_json::json!({ "window": "editor" }));
    assert_eq!(launched.status, 200, "{}{}", launched.head, launched.body);
    assert_eq!(launched.header("content-type").as_deref(), Some("application/json"), "{}", launched.head);
    let opened: serde_json::Value = serde_json::from_str(&launched.body).unwrap();
    assert_eq!(opened["opened"], "fake editor");

    // (c) A `say` with images totalling ~6 MiB of base64: the door got every byte.
    let body = images_body(6 * 1024 * 1024);
    assert!(body.len() > 6 * 1024 * 1024, "a multi-MiB say: {}", body.len());
    let said = perch.apireq("POST", &format!("/s/{id}/api/say")).body(body.clone()).send(&perch.addr);
    assert_eq!(said.status, 202, "{}{}", said.head, said.body);
    let received = std::fs::read(format!("{}.body", perch.fake_log.as_ref().unwrap().display())).expect("the door's copy of the body");
    assert_eq!(received.len(), body.len(), "the door got the whole body");
    assert!(received == body, "…and it is byte-identical");

    // (d) `documents` is refused by name, and the door's reason is the page's.
    let refused = perch.post_json(&format!("/s/{id}/api/say"), serde_json::json!({ "text": "x", "documents": [{ "name": "a.pdf" }] }));
    assert_eq!(refused.status, 422, "{}{}", refused.head, refused.body);
    assert!(refused.body.contains("this door takes images only"), "{}", refused.body);
    assert_eq!(perch.post_json(&format!("/s/{id}/api/cancel"), serde_json::json!({})).status, 409);

    // (e) A route the perch has never heard of reaches the door, and the door's 404 comes back.
    let before = perch.fake_records().len();
    for method in ["GET", "POST"] {
        let unknown = perch.apireq(method, &format!("/s/{id}/api/commands")).send(&perch.addr);
        assert_eq!(unknown.status, 404, "{method}: {}", unknown.head);
        assert_eq!(unknown.body, "no such route", "{method}: the door's 404, not the perch's empty one");
    }
    assert!(since(&perch, before).iter().any(|line| line.starts_with("GET /api/commands ")), "{:?}", since(&perch, before));
    assert!(since(&perch, before).iter().any(|line| line.starts_with("POST /api/commands ")));
    let nested = perch.apireq("GET", &format!("/s/{id}/api/commands/list-all")).send(&perch.addr);
    assert_eq!((nested.status, nested.body.as_str()), (404, "no such route"), "a nested route is a shape too");

    // (f) Malformed shapes never leave the perch: an empty 404, and the door sees nothing.
    let before = perch.fake_records().len();
    for path in [
        format!("/s/{id}/x/events"),
        format!("/s/{id}/api/"),
        format!("/s/{id}/api"),
        format!("/s/{id}/api/Say"),
        format!("/s/{id}/api/a//b"),
        format!("/s/{id}/api/a/../b"),
        format!("/s/{id}/api/%65vents"),
        format!("/s/{id}"),
        format!("/s/{id}/"),
        "/s".to_string(),
        "/s/".to_string(),
    ] {
        for method in ["GET", "POST"] {
            let answer = perch.apireq(method, &path).send(&perch.addr);
            assert_eq!(answer.status, 404, "{method} {path}: {}", answer.head);
            assert_eq!(answer.body, "", "{method} {path} is the perch's own empty 404");
        }
    }
    assert_eq!(since(&perch, before), Vec::<String>::new(), "no malformed path reached the door");
}

/// 3. The body cap, where it is and is not. Never stricter than the door (exactly 8 MiB gets
///    through), never looser (one byte over, declared or streamed, is refused), and a
///    declared length over the cap makes no connection at all.
///
/// Mutations: remove `Limited` from the stream path (the chunked case reaches the stand-in's
/// own cap, which records `body-refused`); a cap of `MAX_BODY - 1` (the exact case goes red);
/// check the declared length after connecting (the first case sees a request).
#[cfg(feature = "test-bins")]
#[test]
fn the_body_cap_is_the_doors_cap_and_holds_on_a_stream() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    let path = format!("/s/{id}/api/say");
    const CAP: usize = 8 * 1024 * 1024;

    // A declared length one byte over: 413, empty, and the door was never asked.
    let before = perch.fake_records().len();
    let over = send_oversize(&perch.addr, "POST", &path, Some(&perch.auth()), CAP + 1);
    assert_eq!(over.status, 413, "{}", over.head);
    assert_eq!(over.body, "");
    assert_eq!(since(&perch, before), Vec::<String>::new(), "no upstream request for a declared body over the cap");

    // Exactly the cap: the door gets all of it.
    let exact = perch.apireq("POST", &path).body(vec![b'x'; CAP]).send(&perch.addr);
    assert_eq!(exact.status, 202, "{}{}", exact.head, exact.body);
    assert!(since(&perch, before).iter().any(|line| line == "body-len=8388608"), "{:?}", since(&perch, before));

    // A chunked body over the cap has no length to refuse up front: only the cap on the
    // stream stops it, and it stops it before the door's own cap would (`body-refused`).
    let before = perch.fake_records().len();
    let chunked = send_chunked(&perch.addr, &path, &perch.auth(), CAP + 64 * 1024);
    assert_eq!(chunked.status, 413, "{}", chunked.head);
    let records = since(&perch, before);
    assert!(records.iter().any(|line| line.starts_with("POST /api/say ")), "the body was streamed to the door until the cap: {records:?}");
    assert!(!records.iter().any(|line| line.starts_with("body-len=")), "a body over the cap was not completed at the door: {records:?}");
    assert!(!records.iter().any(|line| line == "body-refused"), "the door never saw a body over its own cap: {records:?}");
    // The perch is fine afterwards.
    assert_eq!(perch.post_json(&path, serde_json::json!({ "text": "again" })).status, 202);
}

/// 4. Streaming, not collecting. The door sees the request head while the page is still
///    sending the body.
///
/// Mutation: collect the body before connecting (the head never reaches the stand-in until the
/// whole body has arrived, so the bounded poll below fails).
#[cfg(feature = "test-bins")]
#[test]
fn a_body_is_streamed_to_the_door_while_it_is_still_arriving() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    let before = perch.fake_records().len();
    const WHOLE: usize = 1024 * 1024;

    let head = format!(
        "POST /s/{id}/api/say HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAuthorization: {}\r\nContent-Type: application/json\r\nContent-Length: {WHOLE}\r\n\r\n",
        perch.addr,
        perch.auth()
    );
    let mut sock = TcpStream::connect(&perch.addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    sock.write_all(head.as_bytes()).unwrap();
    sock.write_all(&vec![b'x'; WHOLE / 2]).unwrap();

    // Half the body is out and the other half is held back: the door must already have the head.
    let arrived = wait_record(&perch, Duration::from_secs(5), |records| records.iter().skip(before).any(|line| line.starts_with("POST /api/say ")));
    assert!(arrived, "the door never saw the request while the body was still arriving: {:?}", since(&perch, before));
    assert!(!since(&perch, before).iter().any(|line| line.starts_with("body-len=")), "the body was not complete yet");

    sock.write_all(&vec![b'y'; WHOLE / 2]).unwrap();
    let mut raw = Vec::new();
    let _ = sock.read_to_end(&mut raw);
    let answer = Answer::parse(&raw);
    assert_eq!(answer.status, 202, "{}{}", answer.head, answer.body);
    assert!(since(&perch, before).iter().any(|line| line == &format!("body-len={WHOLE}")), "{:?}", since(&perch, before));
}

/// 5. A door's 401 is the perch's 502: the page never concludes its own token is bad.
///
/// Mutation: pass the status through (the page sees 401).
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_refuses_the_perch_is_502_and_never_401() {
    let perch = Perch::start_hub(&[("FAKE_REFUSE", "/api/say")]);
    let (id, _pid, _port) = live(&perch);
    let refused = perch.post_json(&format!("/s/{id}/api/say"), serde_json::json!({ "text": "x" }));
    assert_eq!(refused.status, 502, "{}{}", refused.head, refused.body);
    assert_eq!(refused.body, "{\"error\":\"door refused the perch\"}");
    assert_eq!(refused.header("content-type").as_deref(), Some("application/json"));
    // Only that path: the rest of the door answers the same Bearer.
    assert_eq!(perch.post_json(&format!("/s/{id}/api/cancel"), serde_json::json!({})).status, 409);
}

/// 6. SSE passes through unbuffered: `hello` arrives while the stream is still open, and a
///    frame the door broadcasts afterwards arrives on the same pane.
///
/// Mutation: buffer the body (collect it, or hold frames until the stream ends): `hello` never
/// arrives while the stream is open.
#[cfg(feature = "test-bins")]
#[test]
fn a_pane_gets_frames_as_they_happen_and_the_stream_stays_open() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    let mut pane = Pane::open(&perch, &id);
    assert_eq!(pane.head.lines().find(|l| l.to_ascii_lowercase().starts_with("content-type:")).map(str::to_ascii_lowercase).as_deref(), Some("content-type: text/event-stream"));
    assert!(pane.head.to_ascii_lowercase().contains("cache-control: no-store"), "{}", pane.head);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)), "hello did not arrive while the stream was open: {:?}", pane.data());
    assert!(!pane.eof && !pane.data().1, "…and the stream is still open");
    // A frame the door produces now reaches the same pane, live.
    assert_eq!(perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": 1, "answer": "go" })).status, 204);
    assert!(pane.read_until("ask-settled", Duration::from_secs(5)), "{:?}", pane.data());
    assert!(!pane.eof, "still open after a live frame");
}

/// 7. Mirrors: several panes on one door, each its own stream, each counted; closing a tab
///    brings the count down.
///
/// Mutation: never decrement the count (the drop-one case stays at 2).
#[cfg(feature = "test-bins")]
#[test]
fn several_panes_on_one_session_are_counted_and_a_closed_tab_is_not() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    assert_eq!(perch.row(&id).unwrap()["panes"], 0);
    let mut one = Pane::open(&perch, &id);
    let mut two = Pane::open(&perch, &id);
    assert!(one.read_until("\"hello\"", Duration::from_secs(5)) && two.read_until("\"hello\"", Duration::from_secs(5)), "both panes get a hello of their own");
    assert_eq!(perch.row(&id).unwrap()["panes"], 2, "{:?}", perch.row(&id));

    // One answer reaches both mirrors.
    assert_eq!(perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": 3, "answer": "ok" })).status, 204);
    assert!(one.read_until("ask-settled", Duration::from_secs(5)) && two.read_until("ask-settled", Duration::from_secs(5)));

    let panes_within = |want: u64, within: Duration| {
        let deadline = Instant::now() + within;
        loop {
            if perch.row(&id).unwrap()["panes"] == want {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    drop(one);
    assert!(panes_within(1, Duration::from_secs(2)), "a closed tab is one pane fewer: {:?}", perch.row(&id));
    drop(two);
    assert!(panes_within(0, Duration::from_secs(2)), "{:?}", perch.row(&id));
}

/// 8. The Stop finding for panes: a proxied `/api/events` is an open stream exactly like the
///    watcher, so a stop closes it BEFORE it signals the door.
///
/// Two panes: one read normally, and one the perch is still setting up when the stop lands (the
/// stand-in holds its head back 400 ms with the stream already counted open). The second is what
/// makes "before" provable: the stop must WAIT for it, so the stand-in sees no open stream the
/// moment the signal lands (`sigterm-received streams=0`).
///
/// Mutation: do not cancel the panes in `Registry::stop`: the stand-in (which, like a real door,
/// holds SIGTERM while a stream is open) logs `sigterm streams=1`, the stop waits out the 10 s
/// grace, and the time and the log assertions below both go red.
/// Mutation: remove `panes.drained(PANE_DRAIN)` from `Registry::stop`: the signal lands while the
/// late pane's stream is open, `sigterm-received streams=1`, and the last assertion goes red.
#[cfg(feature = "test-bins")]
#[test]
fn stopping_a_session_closes_its_panes_before_it_signals_the_door() {
    let perch = Perch::start_hub(&[("FAKE_LATE_PANE_MS", "400")]);
    let (id, pid, port) = live(&perch);
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));
    let late = Pane::begin(&perch, &id);
    assert!(wait_record(&perch, Duration::from_secs(5), |lines| lines.iter().any(|l| l == "late-stream-held")), "{:?}", perch.fake_records());
    assert_eq!(perch.row(&id).unwrap()["panes"], 2);

    let started = Instant::now();
    let stopped = perch.post(&format!("/hub/sessions/{id}/stop"));
    let took = started.elapsed();
    assert_eq!(stopped.status, 202, "{}", stopped.head);
    assert!(took < Duration::from_secs(5), "a stop with a pane open does not wait the grace out: {took:?}");

    assert!(pane.read_to_end(Duration::from_secs(5)), "the pane was ended: {:?}", pane.data());
    let end = pane.end_frame();
    assert_eq!((end["session"].as_str(), end["door"].as_str()), (Some(id.as_str()), Some("stopping")), "{end}");
    let (text, finished) = pane.data();
    assert!(finished && text.ends_with("}\n\n"), "the page's stream ended cleanly after the frame: {text:?}");
    // The pane that was still being set up is ended the same way, once.
    let mut late = Pane::adopt(late);
    assert!(late.read_to_end(Duration::from_secs(5)), "{:?}", late.data());
    assert_eq!(late.end_frame()["door"], "stopping");

    assert!(wait_gone(pid, Duration::from_secs(5)));
    let records = perch.fake_records();
    assert!(records.iter().any(|line| line == "sigterm-received streams=0"), "every stream was closed BEFORE the signal landed: {records:?}");
    assert!(records.iter().any(|line| line == "sigterm streams=0"), "{records:?}");
    assert!(perch.door_token_files().is_empty(), "no token file left: {:?}", perch.door_token_files());
    assert!(!perch.door_token_file(port).exists());
    assert_eq!(perch.state_of(&id).as_deref(), Some("kept"), "a requested clean stop leaves the log kept (hub.md section 4)");
}

/// 9. A door that dies mid-frame: the page gets the half frame, then the perch's own frame (its
///    leading blank line terminates the half one), then EOF; and the dead door's port is never
///    connected to again.
///
/// Mutation: look the port up without the state check (a connect to the dead port arrives at
/// the listener this test binds there).
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_dies_mid_frame_ends_the_pane_as_dead_and_is_never_connected_to_again() {
    let perch = Perch::start_hub(&[("FAKE_EXIT_AFTER_MS", "3000")]);
    let id = perch.keep_a_log("crashy");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    let (pid, port) = perch.fake_last_start();
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));

    assert!(pane.read_to_end(Duration::from_secs(15)), "the door's death ended the pane: {:?}", pane.data());
    let (text, finished) = pane.data();
    assert!(finished, "EOF after the frame");
    // The key order of the perch's JSON is not part of the contract; the blank lines around it are.
    assert!(text.contains("data: {\"type\":\"turn-st\n\ndata: {"), "the half frame, then a blank line, then the perch's frame: {text:?}");
    assert!(text.contains("\"type\":\"hub-stream-end\""), "{text:?}");
    assert!(text.ends_with("}\n\n"), "{text:?}");
    let end = pane.end_frame();
    assert_eq!((end["door"].as_str(), end["exit"].as_str(), end["session"].as_str()), (Some("dead"), Some("code:1"), Some(id.as_str())), "{end}");
    assert!(pane.frames().iter().all(|f| f["type"] != "turn-st"), "the half frame did not parse, and a page drops it");

    assert!(perch.wait_state(&id, "dead", Duration::from_secs(10)), "{:?}", perch.state_of(&id));
    assert!(wait_gone(pid, Duration::from_secs(5)));
    // The door's port is free now. Hold it, and prove the perch never connects to it.
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            listener.set_nonblocking(true).unwrap();
            let again = perch.apireq("POST", &format!("/s/{id}/api/say")).body(b"{}".to_vec()).send(&perch.addr);
            assert_eq!(again.status, 503, "{}{}", again.head, again.body);
            assert_eq!(again.body, "{\"state\":\"dead\"}");
            let get = perch.apireq("GET", &format!("/s/{id}/api/events")).send(&perch.addr);
            assert_eq!((get.status, get.body.as_str()), (503, "{\"state\":\"dead\"}"));
            match listener.accept() {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                other => panic!("the perch connected to a dead door's port: {:?}", other.map(|(_, peer)| peer)),
            }
        }
        Err(e) => {
            // Some other process took the port in the meantime; the 503 is still checked.
            eprintln!("could not bind the dead door's port {port}: {e}; checking the 503 only");
            let again = perch.apireq("POST", &format!("/s/{id}/api/say")).body(b"{}".to_vec()).send(&perch.addr);
            assert_eq!(again.status, 503, "{}", again.head);
        }
    }
}

/// 10. The perch's own shutdown with a pane open: every pane is closed, and ended with
///     `stopping`, before any door is signalled, so the quit is fast and leaves no token file.
///
/// Mutation: as test 8, through `stop_all` (both of its mutations).
#[cfg(feature = "test-bins")]
#[test]
fn a_perch_shutdown_with_a_pane_open_ends_the_pane_and_leaves_no_token_file() {
    let mut perch = Perch::start_hub(&[("FAKE_LATE_PANE_MS", "400")]);
    let (id, pid, port) = live(&perch);
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));
    let late = Pane::begin(&perch, &id);
    assert!(wait_record(&perch, Duration::from_secs(5), |lines| lines.iter().any(|l| l == "late-stream-held")), "{:?}", perch.fake_records());

    let started = Instant::now();
    let status = perch.stop();
    assert!(status.success(), "{status}");
    assert!(started.elapsed() < Duration::from_secs(5), "a quit with a pane open does not wait the grace out: {:?}", started.elapsed());
    assert!(pane.read_to_end(Duration::from_secs(5)), "{:?}", pane.data());
    let end = pane.end_frame();
    assert_eq!((end["door"].as_str(), end["session"].as_str()), (Some("stopping"), Some(id.as_str())), "{end}");
    let mut late = Pane::adopt(late);
    assert!(late.read_to_end(Duration::from_secs(5)), "{:?}", late.data());
    assert_eq!(late.end_frame()["door"], "stopping");
    assert!(wait_gone(pid, Duration::from_secs(5)));
    assert!(!perch.door_token_file(port).exists(), "the door's token file");
    assert!(!perch.token_file.exists(), "the perch's own token file");
    let records = perch.fake_records();
    assert!(records.iter().any(|line| line == "sigterm-received streams=0"), "every stream was closed BEFORE the signal landed: {records:?}");
    assert!(records.iter().any(|line| line == "sigterm streams=0"), "{records:?}");
}

/// A page that has stopped reading: it said `GET events`, read `hello`, and then nothing, while
/// the door broadcasts many MiB of frames, so the perch's write to it backs up and hyper stops
/// polling the response body. Returns the pane (kept open by the caller: the socket is the page).
#[cfg(feature = "test-bins")]
fn a_pane_that_stopped_reading(perch: &Perch, id: &str) -> Pane {
    let mut pane = Pane::open(perch, id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));
    // 1000 `ask-settled` frames of 32 KiB each, ~32 MB: far past what the sockets between
    // the door and the page can hold. Many small frames, not a few huge ones: hyper only stops
    // polling the body once it has a queue of buffers it cannot write, and a handful of 4 MiB
    // frames did not get it there.
    let note = "n".repeat(32 * 1024);
    for n in 0..1000 {
        let sent = perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": n, "answer": "go", "note": note }));
        assert_eq!(sent.status, 204, "{}", sent.head);
    }
    // Time for the backlog to build and the page's socket to fill. The page does not read.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(perch.row(id).unwrap()["panes"], 1);
    pane
}

/// H1c review finding 1: a pane that has stopped reading must still be cancellable. hyper does not
/// poll a response body while its write buffer is full, so the stop's cancellation cannot rely on
/// the body being polled: the pane's own task closes the door's stream, and the stop takes no
/// longer than its pane drain (2 s), where it took the door's 10 s grace and a SIGKILL before.
///
/// Mutation: give the pane no task of its own (cancel only from `Proxied::poll_frame`): the stop
/// takes ~12 s, the stand-in logs `sigterm streams=1` and the time assertion goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_stop_does_not_wait_for_a_pane_that_has_stopped_reading() {
    let perch = Perch::start_hub(&[]);
    let (id, pid, port) = live(&perch);
    let pane = a_pane_that_stopped_reading(&perch, &id);

    let started = Instant::now();
    let stopped = perch.post(&format!("/hub/sessions/{id}/stop"));
    let took = started.elapsed();
    assert_eq!(stopped.status, 202, "{}", stopped.head);
    assert!(took < Duration::from_secs(2), "a stop is not held up by a page that is not reading: {took:?}");
    assert!(wait_gone(pid, Duration::from_secs(5)));
    let records = perch.fake_records();
    assert!(records.iter().any(|line| line == "sigterm-received streams=0"), "the pane's stream was closed before SIGTERM: {records:?}");
    assert!(!records.iter().any(|line| line == "sigterm-ignored"), "{records:?}");
    assert!(perch.door_token_files().is_empty(), "{:?}", perch.door_token_files());
    assert!(!perch.door_token_file(port).exists());
    assert_eq!(perch.state_of(&id).as_deref(), Some("kept"), "a requested clean stop leaves the log kept (hub.md section 4)");
    assert!(!perch.err().contains("a pane did not close in time"), "{}", perch.err());
    drop(pane);
}

/// H1c review finding 1, the other half: the perch's own shutdown with a page that is not reading
/// must not hang on hyper's graceful drain. It exits within `serve::RESPONSE_DRAIN` (3 s) of the
/// door being stopped, with the page's socket still open, and leaves no token file.
///
/// Mutation: an unbounded `graceful.shutdown().await` in `serve::run`: the perch is still running
/// when `Perch::stop`'s own 10 s deadline passes, and it panics. (Without the pane's own task the
/// door stop takes ~12 s first, which the time assertion also catches.)
#[cfg(feature = "test-bins")]
#[test]
fn a_perch_shutdown_is_not_held_up_by_a_pane_that_has_stopped_reading() {
    let mut perch = Perch::start_hub(&[]);
    let (id, pid, port) = live(&perch);
    let pane = a_pane_that_stopped_reading(&perch, &id);

    let started = Instant::now();
    let status = perch.stop();
    let took = started.elapsed();
    assert!(status.success(), "{status}");
    assert!(took < Duration::from_secs(6), "the quit is bounded though a page is not reading: {took:?}");
    assert!(wait_gone(pid, Duration::from_secs(5)));
    assert!(!perch.door_token_file(port).exists(), "the door's token file");
    assert!(!perch.token_file.exists(), "the perch's own token file");
    drop(pane);
}

/// 11. Session states that are not live: each refusal is its own answer, and none of them
///     opens a connection to a door.
#[cfg(feature = "test-bins")]
#[test]
fn a_session_that_is_not_live_is_refused_without_a_connection() {
    let perch = Perch::start_hub(&[]);
    let (alive, _pid, _port) = live(&perch);
    let kept = perch.keep_a_log("kept-one");
    let held = perch.keep_a_log("held-elsewhere");
    perch.roster_row("eidolon-9f2c", &perch.sessions.path().join("held-elsewhere.eid"), std::process::id());
    let before = perch.fake_records().len();

    for (method, path) in [("GET", "events"), ("POST", "say")] {
        let unknown = perch.apireq(method, &format!("/s/nobody/api/{path}")).send(&perch.addr);
        assert_eq!((unknown.status, unknown.body.as_str()), (404, ""), "{method} unknown: {}", unknown.head);
        // A kept log, and one a peer holds (never adopted: Q5): the page can offer a resume.
        for id in [&kept, &held] {
            let refused = perch.apireq(method, &format!("/s/{id}/api/{path}")).send(&perch.addr);
            assert_eq!(refused.status, 409, "{method} {id}: {}", refused.head);
            assert_eq!(refused.body, "{\"state\":\"kept\"}");
            assert_eq!(refused.header("content-type").as_deref(), Some("application/json"));
        }
        // Not ids at all: never a path, never a connection.
        for id in ["..", "-x", ".hidden", "a%2Fb"] {
            let bad = perch.apireq(method, &format!("/s/{id}/api/{path}")).send(&perch.addr);
            assert_eq!((bad.status, bad.body.as_str()), (404, ""), "{method} {id}: {}", bad.head);
        }
    }
    assert_eq!(since(&perch, before), Vec::<String>::new(), "no refusal opened a connection to a door");
    // The live one was untouched by any of it.
    assert_eq!(perch.apireq("POST", &format!("/s/{alive}/api/cancel")).body(b"{}".to_vec()).send(&perch.addr).status, 409);

    // A cleanly stopped session is `kept` again (hub.md section 4): 409, and no connection.
    assert_eq!(perch.post(&format!("/hub/sessions/{alive}/stop")).status, 202);
    let gone = perch.apireq("GET", &format!("/s/{alive}/api/events")).send(&perch.addr);
    assert_eq!((gone.status, gone.body.as_str()), (409, "{\"state\":\"kept\"}"), "{}", gone.head);
}

/// A clean stop lists the session `kept`, answers 409 `kept` on the proxy and connects to
/// nothing; a resume then works. (A SIGKILLed or crashed door stays `dead`: see
/// `a_door_that_ignores_sigterm_is_killed_and_the_perch_removes_its_token_file` and
/// `a_door_that_exits_on_its_own_leaves_a_dead_row`.)
///
/// Mutation: drop the clean-exit rule (`Exit::clean` always false) and the first assertion goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_clean_stop_lists_the_session_kept_and_connects_to_nothing() {
    let perch = Perch::start_hub(&[]);
    let (id, pid, port) = live(&perch);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(wait_gone(pid, Duration::from_secs(5)));
    assert_eq!(perch.state_of(&id).as_deref(), Some("kept"));
    let row = perch.row(&id).unwrap();
    assert!(row["mtime"].as_u64().is_some(), "the kept row carries the log's mtime: {row}");

    let before = perch.fake_records().len();
    let refused = perch.apireq("GET", &format!("/s/{id}/api/events")).send(&perch.addr);
    assert_eq!((refused.status, refused.body.as_str()), (409, "{\"state\":\"kept\"}"), "{}", refused.head);
    assert_eq!(since(&perch, before), Vec::<String>::new(), "no connection to a stopped door");
    // The port is free: if the perch connected to it, a listener here would see it.
    if let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port)) {
        listener.set_nonblocking(true).unwrap();
        let _ = perch.apireq("GET", &format!("/s/{id}/api/events")).send(&perch.addr);
        assert!(listener.accept().is_err(), "the perch connected to a door that exited cleanly");
    }

    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200, "a clean stop is resumable");
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// A crashed door's leftover `meta.json` (its pid is gone) does not make the log
/// `live-elsewhere`, and the resume is not a 409; a live pid still does.
///
/// Mutation: drop the `pid_alive` check in `Registry::roster` and this goes red at the first resume.
#[cfg(feature = "test-bins")]
#[test]
fn a_stale_roster_row_does_not_block_a_resume() {
    let perch = Perch::start_hub(&[]);
    let id = perch.keep_a_log("stale-roster");
    let log = perch.sessions.path().join("stale-roster.eid");
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = gone.id();
    gone.wait().unwrap();
    perch.roster_row("web-stale", &log, dead_pid);
    assert_eq!(perch.state_of(&id).as_deref(), Some("kept"), "a dead pid's row is not live-elsewhere");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(perch.runtime.path().join("eidolon/web-stale/meta.json").is_file(), "someone else's row is left alone");
}

/// 13. H1b review finding 1: a new session whose door names a log outside the sessions dir is
///     rejected, the door that ignores SIGTERM is killed, and its token file does not outlive
///     it. No row was ever written for it, so the reap task has nothing to find it by: the
///     signal path owns the file.
///
/// Mutation: drop `drop_token_file` from `serve::give_up` (the file stays, for good).
#[cfg(feature = "test-bins")]
#[test]
fn a_rejected_new_session_leaves_no_door_token_file_behind() {
    let elsewhere = tempfile::tempdir().unwrap();
    let perch = Perch::start_hub(&[("FAKE_IGNORE_SIGTERM", "1"), ("FAKE_SESSIONS_DIR", elsewhere.path().to_str().unwrap())]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 503, "{}{}", answer.head, answer.body);
    assert!(answer.body.contains("not in the sessions dir"), "{}", answer.body);
    let (pid, port) = perch.fake_last_start();
    assert!(wait_gone(pid, Duration::from_secs(5)), "the door that ignored SIGTERM was killed");
    // The token file is the perch's to remove, and it is removed by the time the 503 is out.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !perch.door_token_files().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(perch.door_token_files().is_empty(), "an orphaned door token file: {:?}", perch.door_token_files());
    assert!(!perch.door_token_file(port).exists());
}

// ---------------------------------------------------------------------------
// H2: GET /hub/tree (hub.md section 7)
// ---------------------------------------------------------------------------

#[cfg(feature = "test-bins")]
impl Perch {
    fn tree(&self, id: &str, path: &str) -> Answer {
        self.get(&format!("/hub/tree?session={id}&path={path}"))
    }

    /// The names in a 200 listing, in the order sent.
    fn tree_names(answer: &Answer) -> Vec<String> {
        let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON listing: {e}: {:?}", answer.body));
        body["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect()
    }
}

/// The fixture under the session's cwd: what a project looks like, plus everything the
/// listing must hide or refuse.
#[cfg(feature = "test-bins")]
fn tree_fixture(work: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt as _;
    std::fs::create_dir(work.join("src")).unwrap();
    std::fs::write(work.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(work.join("src/.env"), "KEY=1\n").unwrap();
    std::fs::write(work.join("src/Secret.TXT"), "s\n").unwrap();
    std::fs::create_dir(work.join("out")).unwrap();
    std::fs::write(work.join("a.txt"), "12345").unwrap();
    std::fs::write(work.join(".env"), "TOKEN=1\n").unwrap();
    std::fs::write(work.join("id_rsa"), "k\n").unwrap();
    std::fs::write(work.join("secret.txt"), "s\n").unwrap();
    std::fs::write(work.join("x.pem"), "p\n").unwrap();
    std::fs::create_dir(work.join(".git")).unwrap();
    std::fs::write(work.join(".git/config"), "[core]\n").unwrap();
    std::os::unix::fs::symlink("..", work.join("up")).unwrap();
    std::os::unix::fs::symlink("/etc", work.join("etc")).unwrap();
    std::os::unix::fs::symlink(".git", work.join("history")).unwrap();
    std::os::unix::fs::symlink("src", work.join("inside")).unwrap();
    std::fs::write(work.join(std::ffi::OsStr::from_bytes(b"bad-\xff-name")), "x").unwrap();
}

/// The root listing: kinds and sizes, dirs first, denied names absent, links without a
/// target, and a non-UTF-8 name counted rather than sent.
///
/// Mutation: drop the `denied(&name)` filter in `tree::list` and the hidden-names line goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_lists_names_and_kinds_and_hides_what_the_deny_list_names() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    tree_fixture(&perch.work);

    let answer = perch.tree(&id, "");
    assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
    assert_eq!(answer.header("content-type").as_deref(), Some("application/json"));
    let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(body["path"], "");
    assert_eq!(body["skipped"], 1, "the non-UTF-8 name is counted: {body}");
    assert_eq!(body["truncated"], false);
    let rows: Vec<(String, String, Option<u64>)> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["name"].as_str().unwrap().to_string(), e["kind"].as_str().unwrap().to_string(), e["size"].as_u64()))
        .collect();
    let want: Vec<(String, String, Option<u64>)> = [
        ("out", "dir", None),
        ("src", "dir", None),
        ("a.txt", "file", Some(5)),
        ("etc", "link", None),
        ("history", "link", None),
        ("inside", "link", None),
        ("up", "link", None),
    ]
    .into_iter()
    .map(|(n, k, s)| (n.to_string(), k.to_string(), s))
    .collect();
    // `history` is a link named like nothing on the list, so it is listed (as a link); it is
    // asking to open it that is refused (see the escape test).
    assert_eq!(rows, want, "dirs first, then by name: {body}");
    for hidden in [".env", "id_rsa", "secret.txt", "x.pem", ".git"] {
        assert!(!rows.iter().any(|(name, _, _)| name == hidden), "{hidden} must not be listed: {body}");
    }
    // A link shows no target, and nothing in the answer is an absolute path or a file's text.
    for entry in body["entries"].as_array().unwrap() {
        assert!(entry.get("target").is_none() && entry.get("path").is_none(), "{entry}");
    }
    for leak in ["/etc", "KEY=1", "TOKEN=1", "fn main", &perch.work.display().to_string()] {
        assert!(!answer.body.contains(leak), "{leak} leaked: {}", answer.body);
    }

    // A subdirectory, and the deny list inside it (case-insensitive, by name).
    let sub = perch.tree(&id, "src");
    assert_eq!(sub.status, 200, "{}", sub.body);
    assert_eq!(Perch::tree_names(&sub), vec!["main.rs"], "{}", sub.body);
    assert!(sub.body.contains("\"path\":\"src\""), "{}", sub.body);
    // A link that stays inside the tree is followed when it is asked for.
    let inside = perch.tree(&id, "inside");
    assert_eq!((inside.status, Perch::tree_names(&inside)), (200, vec!["main.rs".to_string()]), "{}", inside.body);
}

/// Every way out of the cwd, and every way to a denied name, is the same empty 404.
///
/// Mutations: remove the `starts_with(root)` check in `tree::list` and `up`/`etc` go red;
/// remove the canonical-remainder deny check and `history` goes red; remove the per-segment
/// deny check and `.env`/`.git` go red.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_path_cannot_leave_the_cwd_or_name_a_denied_entry() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    tree_fixture(&perch.work);

    for path in [
        "up",
        "etc",
        "..",
        "%2e%2e",
        "%2E%2e",
        ".",
        "/etc",
        "a%00b",
        "a%5Cb",
        "src//x",
        "src/",
        "src/..",
        "src/../..",
        ".git",
        ".GIT",
        ".env",
        "history",
        "src/.env",
        "src/.ENV",
        "src/Secret.TXT",
        "id_rsa",
        "x.pem",
        "secret.txt",
        "a.txt",
        "nowhere",
        "src/main.rs",
        "up/ui",
        "up/secret.txt",
        "etc/passwd",
    ] {
        let answer = perch.tree(&id, path);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "path={path}: {}", answer.head);
    }
    // 4098 bytes is not a path.
    let long = "a/".repeat(2049);
    assert_eq!(perch.tree(&id, &long).status, 404);
    // …and a good one still works, so the 404s above are the rules and not a broken route.
    assert_eq!(perch.tree(&id, "src").status, 200);
}

/// 2 001 entries come back as 2 000 and `truncated`; exactly 2 000 do not.
///
/// Mutation: raise `MAX_ENTRIES` or drop the `truncate` and this goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_of_2001_entries_is_cut_at_2000() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    let many = perch.work.join("many");
    std::fs::create_dir(&many).unwrap();
    for n in 0..2000 {
        std::fs::write(many.join(format!("f{n:04}")), "").unwrap();
    }
    let exact: serde_json::Value = serde_json::from_str(&perch.tree(&id, "many").body).unwrap();
    assert_eq!(exact["entries"].as_array().unwrap().len(), 2000);
    assert_eq!(exact["truncated"], false);
    std::fs::write(many.join("f2000"), "").unwrap();
    let cut: serde_json::Value = serde_json::from_str(&perch.tree(&id, "many").body).unwrap();
    assert_eq!(cut["entries"].as_array().unwrap().len(), 2000);
    assert_eq!(cut["truncated"], true);
}

/// The session and query rules: only a live door of ours is listed, and the query is
/// exactly `session` and `path`.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_is_for_a_live_session_with_a_strict_query_behind_the_token() {
    let perch = Perch::start_hub(&[]);
    let (id, pid, _port) = live(&perch);
    let kept = perch.keep_a_log("kept-one");
    let held = perch.keep_a_log("held-elsewhere");
    perch.roster_row("eidolon-9f2c", &perch.sessions.path().join("held-elsewhere.eid"), std::process::id());

    let unknown = perch.tree("nobody", "");
    assert_eq!((unknown.status, unknown.body.as_str()), (404, ""), "{}", unknown.head);
    for bad in ["..", "-x", "a%2Fb"] {
        assert_eq!(perch.tree(bad, "").status, 404, "{bad}");
    }
    let refused = perch.tree(&kept, "");
    assert_eq!((refused.status, refused.body.as_str()), (409, "{\"state\":\"kept\"}"), "{}", refused.head);
    let elsewhere = perch.tree(&held, "");
    assert_eq!((elsewhere.status, elsewhere.body.as_str()), (409, "{\"state\":\"live-elsewhere\"}"), "{}", elsewhere.head);

    // No token, wrong token: 401 with nothing in the body, whatever the query.
    let none = Request::new(&perch.addr, "GET", &format!("/hub/tree?session={id}")).send(&perch.addr);
    assert_eq!((none.status, none.body.as_str()), (401, ""), "{}", none.head);
    let wrong = Request::new(&perch.addr, "GET", &format!("/hub/tree?session={id}")).bearer("Bearer nope").send(&perch.addr);
    assert_eq!(wrong.status, 401);

    // The query is exact.
    for query in [
        String::new(),
        "path=src".to_string(),
        format!("session={id}&other=1"),
        format!("session={id}&session={id}"),
        format!("session={id}&path=a&path=b"),
        format!("session={id}&path=%zz"),
        format!("session={id}&path=%ff"),
        format!("session={id}&"),
        format!("session={id}&path"),
        format!("token=x&session={id}"),
    ] {
        let answer = perch.get(&format!("/hub/tree?{query}"));
        assert_eq!(answer.status, 400, "{query}: {}", answer.head);
    }
    // `path` may be left out: that is the root. Percent-escaped keys decode once.
    assert_eq!(perch.get(&format!("/hub/tree?session={id}")).status, 200);
    assert_eq!(perch.get(&format!("/hub/tree?%73ession={id}&p%61th=")).status, 200);
    assert_eq!(Request::new(&perch.addr, "POST", &format!("/hub/tree?session={id}")).bearer(perch.auth()).send(&perch.addr).status, 404);

    // A stopped session is kept again: 409, not a listing (Q9).
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(wait_gone(pid, Duration::from_secs(5)));
    let stopped = perch.tree(&id, "");
    assert_eq!((stopped.status, stopped.body.as_str()), (409, "{\"state\":\"kept\"}"), "{}", stopped.head);
}

// ---------------------------------------------------------------------------
// H2: GET /hub/tree, after the post-H1c review (logs/agents/perch-post-h1c.review.md)
// ---------------------------------------------------------------------------

/// A head with the `date` header taken out: the one header that changes between two answers.
#[cfg(feature = "test-bins")]
fn head_without_date(answer: &Answer) -> String {
    answer.head.lines().filter(|line| !line.to_ascii_lowercase().starts_with("date:")).collect::<Vec<_>>().join("\r\n")
}

/// Finding 1: on NTFS (`/mnt/c`) `GIT~1` is `.git`'s second spelling. The shape is refused as a
/// segment anywhere in `path`, with the empty 404 a missing entry gets, escaped or not.
///
/// Mutation: make `short_name` always false in `tree.rs` and every row here goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_path_with_an_ntfs_short_name_is_the_same_empty_404() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    tree_fixture(&perch.work);
    // These really exist, so it is the shape and not a missing entry that is refused.
    std::fs::create_dir_all(perch.work.join("GIT~1/objects")).unwrap();
    std::fs::create_dir_all(perch.work.join("a/SSH~1/x")).unwrap();
    std::os::unix::fs::symlink("GIT~1", perch.work.join("alias")).unwrap();
    for path in ["GIT~1", "git~1", "GIT~1/objects", "a/SSH~1/x", "a/SSH~1", "GIT%7E1", "src/AOIDE~2", "alias"] {
        let answer = perch.tree(&id, path);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "path={path}: {}", answer.head);
    }
    assert_eq!(perch.tree(&id, "a").status, 200, "a good sibling still lists");
}

/// Findings 1 and 3: a session whose cwd *is* a denied name or an alias, or sits under one,
/// lists nothing at all, root included.
///
/// Mutation: remove the cwd-segment check in `tree::list_bounded` and every row goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_of_a_session_whose_cwd_is_a_denied_name_is_not_listable() {
    let perch = Perch::start_hub(&[]);
    for cwd in [".ssh", "GIT~1", ".git/sub", "keys/.gnupg"] {
        let dir = perch.work.join(cwd);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("known_hosts"), "x").unwrap();
        let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": dir.display().to_string() }));
        assert_eq!(answer.status, 201, "{cwd}: {}{}", answer.head, answer.body);
        let id = body_id(&answer);
        for path in ["", "known_hosts"] {
            let listed = perch.tree(&id, path);
            assert_eq!((listed.status, listed.body.as_str()), (404, ""), "cwd={cwd} path={path}: {}", listed.head);
        }
    }
    // The control: the same perch lists an ordinary cwd.
    let (id, _pid, _port) = live(&perch);
    assert_eq!(perch.tree(&id, "").status, 200);
}

/// Finding 2: a resumed session's cwd is whatever its log said. The stand-in door reports the
/// sessions dir, which is outside `--root`, so the tree refuses it where a new session's works.
///
/// Mutation: drop the `tree_cwd` call in `serve::tree` (list `root` as stored) and this goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_resumed_session_whose_cwd_is_outside_root_is_not_listable() {
    let perch = Perch::start_hub(&[]);
    let id = perch.keep_a_log("resumed-elsewhere");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));
    std::fs::write(perch.sessions.path().join("visible.txt"), "x").unwrap();
    let answer = perch.tree(&id, "");
    assert_eq!((answer.status, answer.body.as_str()), (404, ""), "{}", answer.head);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// A denied name, an alias and a missing one are one answer, headers included (the `date` apart).
///
/// Mutation: give the denied path a different status or an extra header in `serve::tree` and
/// this goes red.
#[cfg(feature = "test-bins")]
#[test]
fn denied_and_missing_tree_answers_are_byte_identical() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    tree_fixture(&perch.work);
    let missing = perch.tree(&id, "nope");
    assert_eq!(missing.status, 404);
    let want = (head_without_date(&missing), missing.body.clone());
    for path in [".env", "src/.env", ".git", "id_rsa", "GIT~1", "history", "up", "a.txt"] {
        let got = perch.tree(&id, path);
        assert_eq!((head_without_date(&got), got.body.clone()), want, "path={path}");
    }
}

/// Finding 4: at most `MAX_LISTINGS` listings run at once; the rest are a 503 with a short body,
/// not a queue. A burst of 48 over a 40 000-entry directory sees both, and when it is over the
/// slots are free again.
///
/// Mutation: drop the `tree_slots.take()` in `serve::tree` and no 503 is ever seen.
#[cfg(feature = "test-bins")]
#[test]
fn concurrent_tree_listings_are_capped_with_a_503() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    let many = perch.work.join("many");
    std::fs::create_dir(&many).unwrap();
    for n in 0..40_000 {
        std::fs::write(many.join(format!("f{n:05}")), "").unwrap();
    }
    let (addr, auth) = (perch.addr.clone(), perch.auth());
    let mut seen_503 = None;
    for _round in 0..5 {
        let threads: Vec<_> = (0..48)
            .map(|_| {
                let (addr, auth, id) = (addr.clone(), auth.clone(), id.clone());
                std::thread::spawn(move || Request::new(&addr, "GET", &format!("/hub/tree?session={id}&path=many")).bearer(auth).send(&addr))
            })
            .collect();
        let answers: Vec<Answer> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(answers.iter().all(|a| a.status == 200 || a.status == 503), "{:?}", answers.iter().map(|a| a.status).collect::<Vec<_>>());
        assert!(answers.iter().any(|a| a.status == 200), "some listings still run");
        if let Some(refused) = answers.into_iter().find(|a| a.status == 503) {
            seen_503 = Some(refused);
            break;
        }
    }
    let refused = seen_503.expect("a burst of 48 over 4 slots is refused at least once");
    assert_eq!(refused.header("content-type").as_deref(), Some("application/json"));
    assert!(refused.body.len() < 100 && refused.body.contains("listing"), "a short reason: {:?}", refused.body);
    let again = perch.tree(&id, "many");
    assert_eq!(again.status, 200, "the slots are free again: {}", again.head);
}

/// Finding 8: a door that exits 0 *on its own* while live is a stop as far as the list goes
/// (`kept`, `/s` answers 409 kept), and the pane is still told how it ended.
///
/// Mutation: make `Exit::clean` false for code 0 and this goes red (it is `dead`).
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_exits_zero_on_its_own_while_live_ends_kept() {
    let perch = Perch::start_hub(&[("FAKE_EXIT_AFTER_MS", "3000"), ("FAKE_EXIT_CODE", "0")]);
    let id = perch.keep_a_log("quits-politely");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    let (pid, port) = perch.fake_last_start();
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));
    assert!(pane.read_to_end(Duration::from_secs(15)), "{:?}", pane.data());
    let end = pane.end_frame();
    assert_eq!((end["door"].as_str(), end["exit"].as_str()), (Some("dead"), Some("code:0")), "{end}");
    assert!(perch.wait_state(&id, "kept", Duration::from_secs(10)), "{:?}", perch.state_of(&id));
    assert!(wait_gone(pid, Duration::from_secs(5)));
    assert!(!perch.door_token_file(port).exists());
    let gone = perch.apireq("GET", &format!("/s/{id}/api/events")).send(&perch.addr);
    assert_eq!((gone.status, gone.body.as_str()), (409, "{\"state\":\"kept\"}"), "{}", gone.head);
}

/// ...and exit 0 while still `starting` is a failed spawn, which stays `dead`.
///
/// Mutation: drop the `came_up &&` in `Registry::mark_exited` and this goes red (it is `kept`).
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_exits_zero_while_starting_ends_dead() {
    let perch = Perch::start_hub(&[("FAKE_EXIT_BEFORE_BOOT", "0")]);
    let id = perch.keep_a_log("never-came-up");
    let answer = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(answer.status, 503, "{}{}", answer.head, answer.body);
    assert_eq!(perch.state_of(&id).as_deref(), Some("dead"), "a spawn that failed is not a stop");
}

// ---------------------------------------------------------------------------
// The login code and the session cookie (hub.md section 10.5)
// ---------------------------------------------------------------------------

impl Perch {
    /// `POST /hub/code`, with the token, as the launcher does. The code, not its answer.
    fn mint_code(&self) -> String {
        let answer = self.post("/hub/code");
        assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
        let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap();
        body["code"].as_str().expect("a code").to_string()
    }

    /// `POST /hub/redeem` with no credential at all, as a page sends it.
    fn redeem(&self, code: &str) -> Answer {
        Request::new(&self.addr, "POST", "/hub/redeem").body(serde_json::to_vec(&serde_json::json!({ "code": code })).unwrap()).send(&self.addr)
    }

    /// `name=value` of the cookie a redeem set, as a browser would send it back.
    fn cookie_of(&self, answer: &Answer) -> String {
        let set = answer.header("set-cookie").unwrap_or_else(|| panic!("no Set-Cookie: {}", answer.head));
        set.split(';').next().unwrap().to_string()
    }

    /// A cookie request as the page sends it: the browser says `same-origin`.
    fn with_cookie(&self, method: &str, path: &str, cookie: &str) -> Answer {
        Request::new(&self.addr, method, path).header("Cookie", cookie).header("Sec-Fetch-Site", "same-origin").send(&self.addr)
    }

    /// A cookie request from a browser that sends no fetch metadata at all.
    fn with_cookie_bare(&self, method: &str, path: &str, cookie: &str) -> Answer {
        Request::new(&self.addr, method, path).header("Cookie", cookie).send(&self.addr)
    }
}

/// 50 ms is the lifetime; the test waits 300 ms.
fn short_lived_codes() -> Vec<std::ffi::OsString> {
    vec!["--code-ttl-ms".into(), "50".into()]
}

/// code -> redeem -> cookie, and the cookie is a credential everywhere the token is.
/// Mutations: drop the cookie arm of the gate (the cookie request is a 401); make the cookie the
/// token itself (the value assertion); drop `HttpOnly` or `SameSite=Strict` (the attribute checks).
#[test]
fn a_code_is_traded_for_a_cookie_that_works_like_the_token() {
    let perch = Perch::launch(true, None, &[]);
    assert_eq!(perch.get("/hub/sessions").status, 200, "the token still works");
    // Nothing about a code is open without the token.
    assert_eq!(Request::new(&perch.addr, "POST", "/hub/code").send(&perch.addr).status, 401);
    let code = perch.mint_code();
    assert!(code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !code.is_empty(), "the shape the launcher prints");

    let answer = perch.redeem(&code);
    assert_eq!((answer.status, answer.body.as_str()), (204, ""), "{}", answer.head);
    let set = answer.header("set-cookie").unwrap();
    assert!(set.starts_with(&format!("minerva_{}=", perch.port)), "the port is in the name: {set}");
    assert!(set.contains("; HttpOnly") && set.contains("; SameSite=Strict") && set.contains("; Path=/"), "{set}");
    assert!(!set.contains("Secure") && !set.to_ascii_lowercase().contains("max-age"), "{set}");
    let cookie = perch.cookie_of(&answer);
    assert!(!cookie.contains(perch.token.trim()), "the cookie is its own secret, not the token");

    // No Authorization header anywhere below.
    let list = perch.with_cookie("GET", "/hub/sessions", &cookie);
    assert_eq!((list.status, list.body.as_str()), (200, "[]"), "{}", list.head);
    assert_eq!(perch.with_cookie("GET", "/hub/anything", &cookie).status, 404, "past the gate, not refused by it");
    // The cookie is not a way to mint more logins: that is the token's (the launcher's) alone.
    let minted = perch.with_cookie("POST", "/hub/code", &cookie);
    assert_eq!((minted.status, minted.body.as_str()), (403, ""), "{}", minted.head);
    assert_eq!(perch.post("/hub/code").status, 200, "the bearer token still mints");
    assert_eq!(perch.with_cookie("GET", "/s/x/api/events", &cookie).status, 404, "the proxy takes the cookie too");
    // Among other cookies.
    assert_eq!(perch.with_cookie("GET", "/hub/sessions", &format!("a=b; {cookie}; c=d")).status, 200);
    // Wrong value, another port's name, a prefix of the name, a prefix of the value: all 401.
    let (name, value) = cookie.split_once('=').unwrap();
    for bad in [
        format!("{name}=wrong"),
        format!("minerva_{}={value}", perch.port.wrapping_add(1)),
        format!("x{name}={value}"),
        format!("{name}={}", &value[..value.len() - 1]),
        format!("{name}="),
    ] {
        assert_eq!(perch.with_cookie("GET", "/hub/sessions", &bad).status, 401, "{bad}");
    }
    // The cookie is attached by the browser, so it counts only on a request that says `same-origin`:
    // no fetch metadata (an old browser on a sibling port of this host), `same-site` and `none` are all refused.
    for refused in [perch.with_cookie_bare("GET", "/hub/sessions", &cookie), perch.with_cookie_bare("POST", "/hub/sessions", &cookie)] {
        assert_eq!(refused.status, 401, "{}", refused.head);
    }
    for site in ["same-site", "none", "cross-site"] {
        let refused = Request::new(&perch.addr, "GET", "/hub/sessions").header("Cookie", &cookie).header("Sec-Fetch-Site", site).send(&perch.addr);
        assert_eq!(refused.status, 401, "cookie + {site}: {}", refused.head);
    }
    assert_eq!(perch.with_cookie("GET", "/hub/sessions", &cookie).status, 200, "same-origin passes");
    // The bearer keeps its rules: no `Sec-Fetch-Site` passes (curl), `same-site` is still the gate 3 403.
    assert_eq!(perch.get("/hub/sessions").status, 200);
    let sibling = perch.apireq("GET", "/hub/sessions").header("Sec-Fetch-Site", "same-site").send(&perch.addr);
    assert_eq!(sibling.status, 403, "{}", sibling.head);
    // Gated answers are never a document: no script, no frame, nothing loaded.
    assert_eq!(list.header("content-security-policy").as_deref(), Some("default-src 'none'; sandbox"), "{}", list.head);
    assert_eq!(perch.get("/hub/sessions").header("content-security-policy").as_deref(), Some("default-src 'none'; sandbox"));
    let rebound = Request::new(&perch.addr, "GET", "/hub/sessions").header("Cookie", &cookie).header("Sec-Fetch-Site", "same-origin").host("evil.example:80").send(&perch.addr);
    assert_eq!(rebound.status, 403, "{}", rebound.head);

    // Neither the code nor the secret is in the output.
    for out in [perch.out(), perch.err()] {
        assert!(!out.contains(&code) && !out.contains(value), "a login secret reached the output");
    }
}

/// Mutations: leave the code in place after a redeem (the second use is a 204).
#[test]
fn a_code_is_single_use_and_a_bad_one_is_refused() {
    let perch = Perch::launch(true, None, &[]);
    let code = perch.mint_code();
    assert_eq!(perch.redeem(&code).status, 204);
    let again = perch.redeem(&code);
    assert_eq!((again.status, again.body.as_str()), (403, ""), "{}", again.head);
    assert!(again.header("set-cookie").is_none());
    for bad in ["", "nope", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"] {
        assert_eq!(perch.redeem(bad).status, 403, "{bad:?}");
    }
    // A malformed body is the same refusal, not a different oracle.
    for body in [&b"not json"[..], b"{}", b"{\"code\":7}", b"[]"] {
        let answer = Request::new(&perch.addr, "POST", "/hub/redeem").body(body).send(&perch.addr);
        assert_eq!(answer.status, 403, "{:?}", String::from_utf8_lossy(body));
    }
    // Any other method on the route is an ordinary gated 401.
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/redeem").send(&perch.addr).status, 401);
    // A wrong try does not burn a good code.
    let code = perch.mint_code();
    assert_eq!(perch.redeem("nope").status, 403);
    assert_eq!(perch.redeem(&code).status, 204);
}

/// Mutations: ignore the age (the late redeem is a 204).
#[test]
fn an_expired_code_is_refused() {
    // Only the late direction is asserted here: a slow machine makes the code older, never younger, so
    // this cannot flake. "Inside its lifetime" is every other test in this section, at the real 30 s.
    let perch = Perch::launch(true, None, &short_lived_codes());
    let stale = perch.mint_code();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(perch.redeem(&stale).status, 403, "past its lifetime");
}

/// The open route keeps every gate but the token. Mutations: skip the host, fetch-metadata or
/// body-cap gate for it.
#[test]
fn redeem_is_open_only_to_the_token_gate() {
    let perch = Perch::launch(true, None, &[]);
    let code = perch.mint_code();
    let body = serde_json::to_vec(&serde_json::json!({ "code": code })).unwrap();
    let cross = Request::new(&perch.addr, "POST", "/hub/redeem").header("Sec-Fetch-Site", "cross-site").body(body.clone()).send(&perch.addr);
    assert_eq!(cross.status, 403, "{}", cross.head);
    let rebound = Request::new(&perch.addr, "POST", "/hub/redeem").host("evil.example:80").body(body.clone()).send(&perch.addr);
    assert_eq!(rebound.status, 403, "{}", rebound.head);
    let big = send_oversize(&perch.addr, "POST", "/hub/redeem", None, 4096);
    assert_eq!(big.status, 413, "{}", big.head);
    // None of those spent the code; the page's own `fetch` (`same-origin`) can.
    let typed = Request::new(&perch.addr, "POST", "/hub/redeem").header("Sec-Fetch-Site", "same-origin").body(body).send(&perch.addr);
    assert_eq!(typed.status, 204, "{}", typed.head);
    // No credential, no cookie: still the 401 the gates promise.
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/sessions").send(&perch.addr).status, 401);
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/sessions").header("Cookie", "").send(&perch.addr).status, 401);
}

/// The proxy's SSE route takes the cookie, and nothing of it reaches the door.
/// Mutations: leave the cookie arm out of the proxy path (the pane is refused).
#[cfg(feature = "test-bins")]
#[test]
fn the_cookie_opens_a_pane() {
    let mut perch = Perch::start_hub(&[]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string(), "model": "mock" }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let id = body_id(&answer);
    let cookie = perch.cookie_of(&perch.redeem(&perch.mint_code()));
    let mut pane = Pane::adopt(Pane::begin_with(&perch, &id, &format!("Cookie: {cookie}\r\nSec-Fetch-Site: same-origin")));
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)), "the stream opened on the cookie alone");
    drop(pane);
    let said = Request::new(&perch.addr, "POST", &format!("/s/{id}/api/say")).header("Cookie", &cookie).header("Sec-Fetch-Site", "same-origin").body(br#"{"text":"hi"}"#.to_vec()).send(&perch.addr);
    assert_eq!(said.status, 202, "{}{}", said.head, said.body);
    let cookie_value = cookie.split_once('=').unwrap().1.to_string();
    assert!(perch.fake_records().iter().all(|line| !line.contains(&cookie_value)), "the door was handed the cookie");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    perch.stop();
}

// ---------------------------------------------------------------------------
// Titles, saved chats you can read, and the second (read-only) source
// ---------------------------------------------------------------------------

/// eidolon's own JSON lines for a chat (`eidolon log --json`), as the stand-in prints them: a start, and, when `first` is given,
/// that message and a reply.
fn chat_log(first: Option<&str>) -> String {
    let mut lines = vec![serde_json::json!({ "id": 0, "parent": null, "ts_ms": 1, "kind": { "SessionStart": { "model": "mock", "cwd": "/tmp", "system": null } } })];
    if let Some(text) = first {
        let said = |role: &str, text: &str| serde_json::json!({ "role": role, "content": [{ "type": "text", "text": text }] });
        lines.push(serde_json::json!({ "id": 1, "parent": 0, "ts_ms": 2, "kind": { "UserMessage": said("user", text) } }));
        lines.push(serde_json::json!({ "id": 2, "parent": 1, "ts_ms": 3, "kind": { "AssistantMessage": said("assistant", "ok") } }));
    }
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// A perch with a Minerva home, the stand-in as eidolon, and the member's config given. Eidolon's own sessions dir (the second
/// source) is `<perch.data>/eidolon/sessions`, where the stand-in's `sessions` reads it too.
#[cfg(feature = "test-bins")]
fn start_home(member: &str) -> (tempfile::TempDir, PathBuf, Perch) {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("minerva.toml"), "default_model = \"mock\"\n").unwrap();
    let home = home.canonicalize().unwrap();
    let perch = Perch::start_hub_member(&["--root".into(), base.path().canonicalize().unwrap().into(), "--home".into(), home.clone().into()], member);
    (base, home, perch)
}

#[cfg(feature = "test-bins")]
impl Perch {
    /// Eidolon's own sessions dir, as the stand-in and the perch both resolve it from `$XDG_DATA_HOME`.
    fn shared(&self) -> PathBuf {
        let dir = self.data.path().join("eidolon").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

/// A saved chat's title is what `eidolon sessions` says its first message is (flattened, never decoded by the perch); one with no
/// message yet is `<folder> · HH:MM`; and the question is asked once per change, not once per list.
/// Mutations: drop the cache (the helper count climbs on every list); ignore `sessions` (the title is the fallback).
#[cfg(feature = "test-bins")]
#[test]
fn a_saved_chat_is_titled_by_eidolons_own_listing_and_asked_once() {
    let (_base, home, perch) = start_home("");
    let sessions = home.join("sessions");
    std::fs::write(sessions.join("asked.eid"), chat_log(Some("fix the  login\nbug"))).unwrap();
    std::fs::write(sessions.join("blank.eid"), chat_log(None)).unwrap();
    let asked = perch.row("asked").unwrap();
    assert_eq!(asked["title"], "fix the login bug", "{asked}");
    let blank = perch.row("blank").unwrap()["title"].as_str().unwrap().to_string();
    let (name, at) = blank.split_once(" · ").unwrap_or_else(|| panic!("a fallback is `<name> · HH:MM`: {blank:?}"));
    assert_eq!(name, "chat", "a log from before this perch has no cwd the perch may read");
    assert!(at.len() == 5 && at.as_bytes()[2] == b':' && at.chars().filter(|c| c.is_ascii_digit()).count() == 4, "{blank:?}");
    for _ in 0..3 {
        perch.list();
    }
    let helpers = perch.fake_helpers();
    assert_eq!(helpers.len(), 1, "four listings, one `eidolon sessions`: {helpers:?}");
    assert!(helpers[0].starts_with("helper sessions ") && helpers[0].ends_with("config=true"), "{helpers:?}");
    // A new log is the one thing that makes it ask again, and it asks once for the whole directory.
    std::fs::write(sessions.join("later.eid"), chat_log(Some("second chat"))).unwrap();
    assert_eq!(perch.row("later").unwrap()["title"], "second chat");
    assert_eq!(perch.fake_helpers().len(), 2);
}

/// A live chat is titled by the roster's `meta.json` (eidolon's own), and wears `<folder> · HH:MM` until it has one; a failing
/// `eidolon sessions` loses no row and is not retried on every poll.
/// Mutations: read no roster title for a door of ours (the fallback stays); retry a failed listing each time.
#[cfg(feature = "test-bins")]
#[test]
fn a_live_chat_takes_the_roster_title_and_a_failing_listing_loses_no_row() {
    let perch = Perch::start_hub(&[("FAKE_HELPER_FAIL", "1")]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string(), "model": "mock" }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let id = body_id(&answer);
    let title = perch.row(&id).unwrap()["title"].as_str().unwrap().to_string();
    assert!(title.starts_with("work · "), "the door's cwd names the folder: {title:?}");
    let (pid, _) = perch.fake_last_start();
    perch.roster_row("eidolon-live", &perch.sessions.path().join(format!("{id}.eid")), pid);
    assert_eq!(perch.row(&id).unwrap()["title"], "a peer", "the roster's title wins while the door is live");
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));
    perch.keep_a_log("old-one");
    for _ in 0..3 {
        perch.list();
    }
    assert!(perch.row("old-one").unwrap()["title"].as_str().unwrap().starts_with("chat · "), "a failed `sessions` is the fallback name");
    assert_eq!(perch.fake_helpers().len(), 1, "a failed listing is not asked again at every poll: {:?}", perch.fake_helpers());
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// `GET /hub/sessions/<id>/log` is eidolon's `log --json` for a saved chat: the bytes as printed, 404 for an id that is not one,
/// 413 over the size cap, and it starts nothing (opening a saved chat never resumes it). `[ continue ]` is the resume route,
/// with the home's config. Mutations: no cap; a resume that drops `--config`; a log route that starts a door.
#[cfg(feature = "test-bins")]
#[test]
fn a_saved_chat_is_readable_without_a_door_and_continues_by_resume() {
    let (_base, home, perch) = start_home("");
    let sessions = home.join("sessions");
    let text = chat_log(Some("what is a nonce"));
    std::fs::write(sessions.join("saved.eid"), &text).unwrap();
    let got = perch.get("/hub/sessions/saved/log");
    assert_eq!(got.status, 200, "{}", got.head);
    assert_eq!(got.header("content-type").as_deref(), Some("application/x-ndjson"), "{}", got.head);
    assert_eq!(got.body, text, "eidolon's lines, as printed");
    assert!(perch.fake_helpers().iter().any(|line| line.starts_with("helper log --json ") && line.ends_with("config=true")), "{:?}", perch.fake_helpers());
    assert!(perch.fake_records().iter().all(|line| !line.starts_with("start ")), "reading a chat starts no door: {:?}", perch.fake_records());
    assert_eq!(perch.state_of("saved").as_deref(), Some("kept"));
    // Not a chat: unknown, not an id, a query, and the other method.
    assert_eq!(perch.get("/hub/sessions/nope/log").status, 404);
    assert_eq!(perch.get("/hub/sessions/..%2Fsaved/log").status, 404);
    assert_eq!(perch.get("/hub/sessions/saved/log?after=3").status, 404, "no query: a route that is not there");
    assert_eq!(perch.post("/hub/sessions/saved/log").status, 404);
    // The size cap: a 17 MiB log is a 413 with a reason, not a truncated transcript.
    std::fs::write(sessions.join("huge.eid"), vec![b'x'; 17 * 1024 * 1024]).unwrap();
    let refused = perch.get("/hub/sessions/huge/log");
    assert_eq!(refused.status, 413, "{}", refused.head);
    assert!(refused.body.contains("too large"), "{}", refused.body);
    // Continue = the resume route, which is what spends: with the home's config, on that log.
    let resumed = perch.post("/hub/sessions/saved/resume");
    assert_eq!(resumed.status, 200, "{}{}", resumed.head, resumed.body);
    let argv = perch.fake_argv();
    assert!(argv.starts_with(&format!("argv --config {} web ", home.join("config.toml").display())), "{argv}");
    assert!(argv.contains(&format!("--session {}", sessions.join("saved.eid").display())), "{argv}");
    assert_eq!(perch.state_of("saved").as_deref(), Some("live"));
    assert_eq!(perch.post("/hub/sessions/saved/stop").status, 202);
}

/// Under `--home`, eidolon's own sessions dir is a second source: its chats are listed `origin: "other"`, titled the same way
/// (a `sessions` run with no `--config`), readable through the same route (also with no config), never written, and `continue`
/// resumes one with the home's config; after which it is just a live chat, and `other` again once it stops. An id both dirs have
/// is the home's. Without `--home` there is no such source. Mutations: no second source; origin missing; `--config` passed for the
/// other dir's `sessions`; the other source listed without `--home`.
#[cfg(feature = "test-bins")]
#[test]
fn eidolons_own_dir_is_a_second_read_only_source_flagged_other() {
    let (_base, home, perch) = start_home("");
    let shared = perch.shared();
    let old = chat_log(Some("an old question"));
    std::fs::write(shared.join("old.eid"), &old).unwrap();
    std::fs::write(shared.join("dup.eid"), chat_log(Some("theirs"))).unwrap();
    std::fs::write(home.join("sessions/dup.eid"), chat_log(Some("ours"))).unwrap();
    std::fs::write(home.join("sessions/new.eid"), chat_log(Some("mine"))).unwrap();
    let row = perch.row("old").expect("the other source is listed");
    assert_eq!((row["state"].as_str(), row["origin"].as_str(), row["title"].as_str()), (Some("kept"), Some("other"), Some("an old question")), "{row}");
    assert!(perch.row("new").unwrap().get("origin").is_none(), "the home's own chats are not flagged");
    let dup = perch.row("dup").unwrap();
    assert_eq!((dup["title"].as_str(), dup.get("origin")), (Some("ours"), None), "an id both dirs have is the home's: {dup}");
    assert_eq!(perch.list().iter().filter(|r| r["id"] == "dup").count(), 1);
    let helpers = perch.fake_helpers();
    assert!(helpers.iter().any(|h| h.ends_with("config=true")) && helpers.iter().any(|h| h.ends_with("config=false")), "one listing per source: {helpers:?}");
    // Readable, with eidolon's own config (none is passed).
    let got = perch.get("/hub/sessions/old/log");
    assert_eq!((got.status, got.body.as_str()), (200, old.as_str()), "{}", got.head);
    assert!(perch.fake_helpers().iter().any(|h| h.starts_with("helper log --json ") && h.contains("/eidolon/sessions/old.eid") && h.ends_with("config=false")));
    assert_eq!(perch.get("/s/old/api/events").status, 409, "a pane on it is told it is kept, like any saved chat");
    assert_eq!(std::fs::read_to_string(shared.join("old.eid")).unwrap(), old, "nothing in it is written");
    // Continue: the member's normal config, on that log, in place.
    let resumed = perch.post("/hub/sessions/old/resume");
    assert_eq!(resumed.status, 200, "{}{}", resumed.head, resumed.body);
    let argv = perch.fake_argv();
    assert!(argv.starts_with(&format!("argv --config {} web ", home.join("config.toml").display())), "{argv}");
    assert!(argv.contains(&format!("--session {}", shared.join("old.eid").display())), "{argv}");
    let live = perch.row("old").unwrap();
    assert_eq!((live["state"].as_str(), live.get("origin")), (Some("live"), None), "{live}");
    assert_eq!(perch.post("/hub/sessions/old/stop").status, 202);
    assert!(perch.wait_state("old", "kept", Duration::from_secs(5)));
    assert_eq!(perch.row("old").unwrap()["origin"], "other", "stopped, it is eidolon's own again");
    drop(perch);

    // Without `--home` there is no second source, whatever sits in the data dir.
    let plain = Perch::start_hub(&[]);
    std::fs::write(plain.shared().join("stray.eid"), chat_log(Some("not listed"))).unwrap();
    plain.keep_a_log("here");
    let ids: Vec<String> = plain.list().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids, ["here"], "{ids:?}");
    assert_eq!(plain.get("/hub/sessions/stray/log").status, 404);
}

// ---------------------------------------------------------------------------
// Projects and the folder picker (hub.md section 7a)
// ---------------------------------------------------------------------------

#[cfg(feature = "test-bins")]
fn json_of(answer: &Answer) -> serde_json::Value {
    serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON body: {e}: {:?} ({})", answer.body, answer.head))
}

/// A project is a named folder under the roots, kept in `<home>/projects.json` (0600): one entry per folder, the name
/// defaults to the folder's, removing one never touches the folder, and a folder outside the roots is a 422.
/// Mutations: drop the dedupe (a second 201), the roots check (the outside 422 becomes 201), or have DELETE remove the folder.
#[cfg(feature = "test-bins")]
#[test]
fn projects_are_named_folders_kept_once_and_forgotten_without_touching_files() {
    use std::os::unix::fs::PermissionsExt as _;
    let (base, home, perch) = start_home("");
    let root = base.path().canonicalize().unwrap();
    assert_eq!(json_of(&perch.get("/hub/projects")), serde_json::json!([]));
    let proj = root.join("my-app");
    std::fs::create_dir(&proj).unwrap();
    std::fs::write(proj.join("keep.txt"), "x").unwrap();

    let made = perch.post_json("/hub/projects", serde_json::json!({ "path": proj }));
    assert_eq!(made.status, 201, "{}{}", made.head, made.body);
    let entry = json_of(&made);
    assert_eq!((entry["name"].as_str(), entry["path"].as_str()), (Some("my-app"), proj.to_str()), "the name is the folder's");
    assert!(entry["created"].as_u64().unwrap() > 0);
    // The same folder again (trailing slash and a new name included) is the existing entry.
    let again = perch.post_json("/hub/projects", serde_json::json!({ "path": format!("{}/", proj.display()), "name": "Other" }));
    assert_eq!((again.status, json_of(&again)), (200, entry.clone()), "{}", again.body);
    let named = root.join("two");
    std::fs::create_dir(&named).unwrap();
    let two = perch.post_json("/hub/projects", serde_json::json!({ "path": named, "name": "  The Two " }));
    assert_eq!((two.status, json_of(&two)["name"].clone()), (201, serde_json::json!("The Two")));
    let listed = json_of(&perch.get("/hub/projects"));
    assert_eq!(listed.as_array().unwrap().len(), 2, "{listed}");
    let file = home.join("projects.json");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(&file).unwrap()).unwrap(), listed, "what is listed is what is on disk");

    // Refusals.
    let outside = tempfile::tempdir().unwrap();
    for bad in [outside.path().to_str().unwrap().to_string(), "/etc".into(), format!("{}/nonexistent", root.display()), format!("{}/..", proj.display())] {
        let refused = perch.post_json("/hub/projects", serde_json::json!({ "path": bad }));
        assert_eq!(refused.status, 422, "{bad}: {}{}", refused.head, refused.body);
    }
    std::fs::create_dir(root.join(".git")).unwrap();
    assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "path": root.join(".git") })).status, 422, "a denied folder is not a project");
    let fresh = root.join("three");
    std::fs::create_dir(&fresh).unwrap();
    assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "path": fresh, "name": "a\nb" })).status, 400, "a bad name");
    assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "nope": 1 })).status, 400);
    assert_eq!(json_of(&perch.get("/hub/projects")).as_array().unwrap().len(), 2, "a refusal adds nothing");

    // Delete forgets the entry and only that.
    assert_eq!(perch.apireq("DELETE", "/hub/projects/0").send(&perch.addr).status, 204);
    assert!(proj.join("keep.txt").is_file() && proj.is_dir(), "the folder and its files are untouched");
    let left = json_of(&perch.get("/hub/projects"));
    assert_eq!(left.as_array().unwrap().len(), 1);
    assert_eq!(left[0]["name"], "The Two");
    for missing in ["/hub/projects/5", "/hub/projects/x", "/hub/projects/-1", "/hub/projects/+0", "/hub/projects/0/1"] {
        assert_eq!(perch.apireq("DELETE", missing).send(&perch.addr).status, 404, "{missing}");
    }
    assert_eq!(perch.apireq("DELETE", "/hub/projects").send(&perch.addr).status, 405, "only an entry can be deleted");
    assert_eq!(Request::new(&perch.addr, "DELETE", "/hub/projects/0").send(&perch.addr).status, 401, "behind the token like the rest");
    assert_eq!(perch.apireq("DELETE", "/hub/sessions/x").send(&perch.addr).status, 405, "no other DELETE exists");
}

/// A chat in a project is titled by the project, a scratch chat is `new chat`, never the stamp; and a saved chat
/// still knows its folder (the cwd is remembered by the perch, not decoded from the journal).
/// Mutations: put the folder name back in the scratch title; drop the project lookup; drop the kept row's cwd.
#[cfg(feature = "test-bins")]
#[test]
fn chat_titles_name_the_project_or_new_chat_and_never_a_scratch_stamp() {
    let (base, home, perch) = start_home("");
    let root = base.path().canonicalize().unwrap();
    let proj = root.join("folder-name");
    std::fs::create_dir(&proj).unwrap();
    assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "path": proj, "name": "Pretty Name" })).status, 201);

    let in_project = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "cwd": proj })));
    let scratch_a = body_id(&perch.post_json("/hub/sessions", serde_json::json!({})));
    let scratch_b = body_id(&perch.post_json("/hub/sessions", serde_json::json!({})));
    let title = |id: &str| perch.row(id).unwrap()["title"].as_str().unwrap().to_string();
    assert!(title(&in_project).starts_with("Pretty Name · "), "{}", title(&in_project));
    for id in [&scratch_a, &scratch_b] {
        let t = title(id);
        assert!(t.starts_with("new chat · "), "{t}");
        assert!(t.split_once(" · ").is_some_and(|(_, at)| at.len() == 5), "only a time after the name, no stamp: {t}");
    }
    let cwd_of = |id: &str| perch.row(id).unwrap()["cwd"].as_str().unwrap().to_string();
    let (ca, cb) = (cwd_of(&scratch_a), cwd_of(&scratch_b));
    assert_ne!(ca, cb, "two [new chat] are two folders");
    assert!(ca.starts_with(home.join("scratch").to_str().unwrap()));

    // Saved: the row keeps its folder, so the page can put it under its project.
    assert_eq!(perch.post(&format!("/hub/sessions/{in_project}/stop")).status, 202);
    assert!(perch.wait_state(&in_project, "kept", Duration::from_secs(5)));
    assert_eq!(perch.row(&in_project).unwrap()["cwd"].as_str(), proj.to_str(), "{}", perch.row(&in_project).unwrap());
    assert!(std::fs::read_to_string(home.join("chat-cwds.json")).unwrap().contains(proj.to_str().unwrap()));
    for id in [&scratch_a, &scratch_b] {
        assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    }
}

/// The picker lists subfolders only, starts at the roots, and every way out (`..`, a link, a denied name, an 8.3 alias,
/// outside the roots, a file) is the same empty 404. Mutations: list files; drop the starts_with check; drop the deny test.
#[cfg(feature = "test-bins")]
#[test]
fn the_folder_picker_lists_subfolders_under_the_roots_only() {
    let (base, _home, perch) = start_home("");
    let root = base.path().canonicalize().unwrap();
    for sub in ["work/b", "work/a", "work/.hidden", "work/.git", "work/GIT~1", "work/secrets"] {
        std::fs::create_dir_all(root.join(sub)).unwrap();
    }
    std::fs::write(root.join("work/file.txt"), "x").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), root.join("work/escape")).unwrap();

    // No path: the roots (the harness gives the tempdir and the home).
    let top = perch.get("/hub/dirs");
    assert_eq!((top.status, top.header("content-type").as_deref()), (200, Some("application/json")), "{}", top.head);
    let top = json_of(&top);
    assert!(top["parent"].is_null() && top["path"] == "");
    let paths: Vec<&str> = top["dirs"].as_array().unwrap().iter().map(|d| d["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&root.to_str().unwrap()), "{paths:?}");
    assert!(top["dirs"].as_array().unwrap().iter().all(|d| !d["name"].as_str().unwrap().is_empty()), "every root is labelled");
    assert_eq!(json_of(&perch.get("/hub/dirs?path=")), top, "an empty path is the roots too");

    let q = |path: &std::path::Path| format!("/hub/dirs?path={}", path.display().to_string().replace('/', "%2F"));
    let got = json_of(&perch.get(&q(&root.join("work"))));
    let names: Vec<&str> = got["dirs"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["a", "b"], "no file, link, dot folder, denied name or alias: {got}");
    assert_eq!(got["dirs"][0]["path"].as_str(), root.join("work/a").to_str());
    assert_eq!(got["parent"].as_str(), root.to_str(), "up from work is its parent");
    assert_eq!(got["crumbs"].as_array().unwrap().last().unwrap()["name"], "work");
    assert_eq!(json_of(&perch.get(&q(&root)))["parent"], "", "above a root is the roots");

    for bad in [
        root.join("work/escape"),
        root.join("work/.git"),
        root.join("work/GIT~1"),
        root.join("work/secrets"),
        root.join("work/file.txt"),
        root.join("work/nope"),
        root.join("work/b/../a"),
        root.parent().unwrap().to_path_buf(),
        "/".into(),
        "/etc".into(),
        elsewhere.path().to_path_buf(),
    ] {
        let refused = perch.get(&q(&bad));
        assert_eq!((refused.status, refused.body.as_str()), (404, ""), "{}", bad.display());
    }
    assert_eq!(perch.get("/hub/dirs?path=relative%2Fdir").status, 404);
    assert_eq!(perch.get("/hub/dirs?path=a%5Cb").status, 404, "a backslash is not a separator here: the page converts");
    for bad in ["/hub/dirs?x=1", "/hub/dirs?path=1&path=2", "/hub/dirs?path=%zz", "/hub/dirs?path"] {
        assert_eq!(perch.get(bad).status, 400, "{bad}");
    }
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/dirs").send(&perch.addr).status, 401);
}

/// `POST /hub/dirs` makes one folder, in one root, with a plain name. Mutations: allow `/` or `..` in the name; drop the parent
/// check (the outside folder appears); use create_dir_all (a missing parent is made).
#[cfg(feature = "test-bins")]
#[test]
fn the_folder_picker_makes_one_folder_with_a_plain_name() {
    let (base, _home, perch) = start_home("");
    let root = base.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("work")).unwrap();
    let make = |parent: &std::path::Path, name: &str| perch.post_json("/hub/dirs", serde_json::json!({ "parent": parent, "name": name }));

    let ok = make(&root.join("work"), "new project");
    assert_eq!(ok.status, 201, "{}{}", ok.head, ok.body);
    assert_eq!(json_of(&ok)["path"].as_str(), root.join("work/new project").to_str());
    assert!(root.join("work/new project").is_dir());
    assert_eq!(make(&root.join("work"), "new project").status, 409, "it exists");

    let long = "n".repeat(101);
    for bad in ["", " ", "a/b", "a\\b", "..", "a..b", ".hidden", "a\nb", "a\u{7}b", " x", "x ", "GIT~1", "secrets", long.as_str()] {
        let refused = make(&root.join("work"), bad);
        assert_eq!(refused.status, 400, "{bad:?}: {}{}", refused.head, refused.body);
    }
    assert_eq!(std::fs::read_dir(root.join("work")).unwrap().count(), 1, "nothing but the one folder was made");

    let outside = tempfile::tempdir().unwrap();
    for parent in [outside.path().to_path_buf(), root.join("work/missing"), root.join("work/.."), root.parent().unwrap().to_path_buf(), "/etc".into(), "rel".into()] {
        let refused = make(&parent, "evil");
        assert_eq!(refused.status, 422, "{}: {}{}", parent.display(), refused.head, refused.body);
    }
    assert!(!outside.path().join("evil").exists() && !root.join("work/missing").exists() && !root.parent().unwrap().join("evil").exists());
    std::fs::create_dir(root.join(".git")).unwrap();
    assert_eq!(make(&root.join(".git"), "x").status, 422, "not inside a denied folder");
    assert_eq!(perch.post_json("/hub/dirs", serde_json::json!({ "parent": root })).status, 400);
    assert_eq!(Request::new(&perch.addr, "POST", "/hub/dirs").send(&perch.addr).status, 401);
}

// ---------------------------------------------------------------------------
// The composer's model and mode menus: GET /hub/models, POST /hub/sessions/<id>/restart
// ---------------------------------------------------------------------------

/// `GET /hub/models` is eidolon's own listing (`eidolon models`, run with the Minerva config), parsed into keys with a name and a
/// note, and asked once for ten minutes; a hung command is a 504 inside the bound and a failed one a 502, and neither is cached.
/// Mutations: no cache (the helper count climbs); no timeout (the hung case never answers); cache a failure.
#[cfg(feature = "test-bins")]
#[test]
fn the_model_list_is_eidolons_own_cached_and_bounded() {
    let (_base, home, perch) = start_home("");
    let first = perch.get("/hub/models");
    assert_eq!(first.status, 200, "{}{}", first.head, first.body);
    assert_eq!(first.header("content-type").as_deref(), Some("application/json"), "{}", first.head);
    let rows: serde_json::Value = serde_json::from_str(&first.body).unwrap();
    assert_eq!(rows[0], serde_json::json!({ "key": "mock:alpha", "name": "Alpha", "note": "mock · 10k ctx · free" }), "{rows}");
    assert_eq!(rows[1]["key"], "mock:beta");
    assert_eq!(rows.as_array().unwrap().len(), 2, "the provider table and the notice are not models: {rows}");
    assert_eq!(perch.get("/hub/models").body, first.body);
    let helpers = perch.fake_helpers();
    assert_eq!(helpers.len(), 1, "two requests, one `eidolon models`: {helpers:?}");
    assert!(helpers[0].starts_with("helper models") && helpers[0].ends_with("config=true"), "run with the Minerva config: {helpers:?}");
    assert!(home.join("config.toml").is_file());
    // Behind the token, like every /hub route.
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/models").send(&perch.addr).status, 401);

    // A command that does not answer in time: 504, inside the bound, and asked again next time (a failure is not cached).
    let slow = Perch::start_hub(&[("FAKE_MODELS_SLEEP_MS", "5000"), ("PERCH_MODELS_TIMEOUT_MS", "400")]);
    let began = Instant::now();
    let late = slow.get("/hub/models");
    assert_eq!(late.status, 504, "{}{}", late.head, late.body);
    assert!(began.elapsed() < Duration::from_secs(4), "the bound held: {:?}", began.elapsed());
    assert_eq!(slow.get("/hub/models").status, 504);
    assert_eq!(slow.fake_helpers().len(), 2, "a timeout is asked again, not cached");

    let failing = Perch::start_hub(&[("FAKE_HELPER_FAIL", "1")]);
    let broken = failing.get("/hub/models");
    assert_eq!(broken.status, 502, "{}{}", broken.head, broken.body);
    assert!(broken.body.contains("could not list its models"), "{}", broken.body);
}

/// The restart route: the same log and the Minerva config, the new `-m` / `--yolo`, the old flags kept when the request names
/// one of them, no restart for flags the door already has, a revert when the new ones do not come up, and the refusals.
/// Mutations: drop `--yolo` or `-m` from the new argv; resume with `--cwd` (a new chat) instead of `--session`; forget the
/// model across a yolo-only restart; no revert; accept `-m --yolo`.
#[cfg(feature = "test-bins")]
#[test]
fn a_restart_resumes_the_same_log_with_the_new_flags_and_keeps_the_rest() {
    let (_base, home, perch) = start_home("");
    let config = home.join("config.toml");
    let created = perch.post_json("/hub/sessions", serde_json::json!({}));
    assert_eq!(created.status, 201, "{}{}", created.head, created.body);
    let id = body_id(&created);
    let log = home.join("sessions").join(format!("{id}.eid"));
    let starts = |perch: &Perch| perch.fake_records().iter().filter(|line| line.starts_with("start ")).count();
    assert_eq!(starts(&perch), 1);

    let restart = |body: serde_json::Value| perch.post_json(&format!("/hub/sessions/{id}/restart"), body);
    // Model and mode in one: the same log, the home's config, `--yolo` before the subcommand, `-m` after the target.
    let first = restart(serde_json::json!({ "model": "mock:alpha", "yolo": true }));
    assert_eq!(first.status, 200, "{}{}", first.head, first.body);
    let said: serde_json::Value = serde_json::from_str(&first.body).unwrap();
    assert_eq!(said, serde_json::json!({ "id": id, "model": "mock:alpha", "yolo": true, "restarted": true }));
    let argv = perch.fake_argv();
    assert!(argv.starts_with(&format!("argv --config {} --yolo web ", config.display())), "{argv}");
    assert!(argv.contains(&format!("--session {}", log.display())) && !argv.contains("--cwd"), "the same log, not a new chat: {argv}");
    assert!(argv.ends_with(" -m mock:alpha"), "{argv}");
    assert_eq!(starts(&perch), 2);
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"), "the same id is live again");
    assert_eq!(perch.list().iter().filter(|row| row["id"] == id.as_str()).count(), 1, "one row, not a new chat beside it");

    // Only the mode changes: the model the door has stays, `--yolo` goes.
    let second = restart(serde_json::json!({ "yolo": false }));
    assert_eq!(second.status, 200, "{}{}", second.head, second.body);
    let argv = perch.fake_argv();
    assert!(!argv.contains("--yolo") && argv.ends_with(" -m mock:alpha"), "the model is kept across a mode change: {argv}");
    assert_eq!(starts(&perch), 3);
    // Only the model changes: yolo is off, stays off.
    assert_eq!(restart(serde_json::json!({ "model": "mock:beta" })).status, 200);
    assert!(perch.fake_argv().ends_with(" -m mock:beta") && !perch.fake_argv().contains("--yolo"), "{}", perch.fake_argv());
    assert_eq!(starts(&perch), 4);

    // What it already is: nothing is restarted.
    let same = restart(serde_json::json!({ "model": "mock:beta", "yolo": false }));
    assert_eq!(same.status, 200);
    assert!(same.body.contains("\"restarted\":false"), "{}", same.body);
    assert_eq!(starts(&perch), 4, "no new door for the flags it already has");

    // A model that does not start: the chat is brought back on the one it had, and the answer says so.
    let nope = restart(serde_json::json!({ "model": "nope" }));
    assert_eq!(nope.status, 503, "{}{}", nope.head, nope.body);
    let said: serde_json::Value = serde_json::from_str(&nope.body).unwrap();
    assert_eq!(said["reverted"], true, "{said}");
    assert!(said["error"].as_str().unwrap().contains("back on mock:beta"), "{said}");
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));
    assert!(perch.fake_argv().ends_with(" -m mock:beta"), "the revert is the old model: {}", perch.fake_argv());

    // Refusals. A body that names nothing, and a model that could be read as a flag, never reach a door.
    let before = starts(&perch);
    assert_eq!(restart(serde_json::json!({})).status, 400);
    assert_eq!(restart(serde_json::json!({ "model": "--yolo" })).status, 400);
    assert_eq!(restart(serde_json::json!({ "model": "a b" })).status, 400);
    assert_eq!(perch.post_json("/hub/sessions/nothing-here/restart", serde_json::json!({ "yolo": true })).status, 404);
    assert_eq!(perch.post_json("/hub/sessions/..%2Fx/restart", serde_json::json!({ "yolo": true })).status, 404);
    std::fs::write(home.join("sessions").join("saved.eid"), chat_log(None)).unwrap();
    let kept = perch.post_json("/hub/sessions/saved/restart", serde_json::json!({ "yolo": true }));
    assert_eq!(kept.status, 409, "a saved chat is continued, not restarted: {}{}", kept.head, kept.body);
    assert!(kept.body.contains("not running"), "{}", kept.body);
    assert_eq!(perch.get(&format!("/hub/sessions/{id}/restart")).status, 404, "POST only");
    assert_eq!(starts(&perch), before);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// A restart in the middle of a turn would cut it off: a running turn (the door's own `turn-state`, read off the perch's stream)
/// is a 409 with a sentence, and nothing is stopped. When the turn ends the same request goes through, on a **resumed** chat
/// too (its stream is kept, which it was not before). Mutations: ignore `turn-state`; do not keep a resumed door's stream.
#[cfg(feature = "test-bins")]
#[test]
fn a_restart_waits_for_the_turn_to_end() {
    let perch = Perch::start_hub(&[("FAKE_SAY_RUNS", "1")]);
    let (id, _, _) = live(&perch);
    // A resumed door is the harder case: stop, resume, and only then run a turn.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    let starts = || perch.fake_records().iter().filter(|line| line.starts_with("start ")).count();
    assert_eq!(starts(), 2);
    let said = perch.post_json(&format!("/s/{id}/api/say"), serde_json::json!({ "text": "go" }));
    assert_eq!(said.status, 202, "{}{}", said.head, said.body);
    assert!(wait_record(&perch, Duration::from_secs(3), |lines| lines.iter().any(|line| line == "turn-state true")), "the door announced the turn");
    std::thread::sleep(Duration::from_millis(300));
    let busy = perch.post_json(&format!("/hub/sessions/{id}/restart"), serde_json::json!({ "yolo": true }));
    assert_eq!(busy.status, 409, "{}{}", busy.head, busy.body);
    assert!(busy.body.contains("wait for the turn to end"), "{}", busy.body);
    assert_eq!(starts(), 2, "a refused restart stops nothing");
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));

    assert_eq!(perch.post(&format!("/s/{id}/api/cancel")).status, 204);
    let deadline = Instant::now() + Duration::from_secs(3);
    let done = loop {
        let answer = perch.post_json(&format!("/hub/sessions/{id}/restart"), serde_json::json!({ "yolo": true }));
        if answer.status != 409 || Instant::now() > deadline {
            break answer;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(done.status, 200, "{}{}", done.head, done.body);
    assert!(perch.fake_argv().contains("--yolo"), "{}", perch.fake_argv());
    assert_eq!(starts(), 3);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}
