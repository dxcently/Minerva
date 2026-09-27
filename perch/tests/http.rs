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
    /// The whole directory tree: `ui/` is what `--ui-dir` names, and `secret.txt` sits
    /// beside it, where a request must not be able to reach.
    root: tempfile::TempDir,
    ui: PathBuf,
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

        let runtime = tempfile::tempdir().unwrap();
        // `tempfile` makes `0755`; a perch runtime dir is `0700`, and the perch refuses
        // anything looser, so the child would never print a boot line.
        own(runtime.path(), 0o700);
        let mut command = Command::new(env!("CARGO_BIN_EXE_perch"));
        command.arg("--bind").arg("127.0.0.1:0");
        if ui_dir {
            command.arg("--ui-dir").arg(&ui);
        }
        let mut child = command
            .env("XDG_RUNTIME_DIR", runtime.path())
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

        Self { child, addr, port, token, token_file, runtime, root, ui, boot, stdout, stderr }
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
        let text = String::from_utf8_lossy(raw).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line in {text:?}"));
        let mut headers = Vec::new();
        for line in head.lines().skip(1) {
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
            }
        }
        Self { status, head: head.to_string(), headers, body: body.to_string() }
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
fn a_gated_route_with_the_right_token_is_404_because_no_route_exists_yet() {
    // The point of H1a: the gate passed, and what is behind it is H1b's and H1c's.
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    for (method, path) in [
        ("GET", "/hub"),
        ("GET", "/hub/sessions"),
        ("POST", "/hub/sessions"),
        ("GET", "/hub/tree"),
        ("GET", "/hub/events"),
        ("GET", "/s"),
        ("GET", "/s/x/api/events"),
        ("POST", "/s/x/api/say"),
    ] {
        let answer = Request::new(&perch.addr, method, path).bearer(auth.clone()).send(&perch.addr);
        assert_eq!(answer.status, 404, "{method} {path} with the token: {}", answer.head);
        assert_eq!(answer.body, "", "no route has a body yet: {path}");
    }
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
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/tree?path=/tmp").bearer(format!("Bearer {}", perch.token.trim())).send(&perch.addr).status, 404);
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
