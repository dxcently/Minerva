//! Shared pieces of the installer's integration tests: a fixture environment that cannot see the operator's, a stub HTTP server on
//! the slice's loopback port, and a `curl` shim that records what the scripts hand to curl and refuses to touch the operator's perch.
#![allow(dead_code)]

use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The operator's perch. No test listens there, and the curl shim refuses any call aimed at it.
pub const OPERATORS_PORT: u16 = 47820;

/// This process's stub port: a loopback port the OS hands out, picked once per process, never the operator's. A fixed number (48110
/// before) is shared by every run of this suite on the host, from every worktree, and a stranger's listener on it made tests fail with
/// "already in use by something that is not our perch" or met requests that were not theirs. A port the OS picks is free when it is
/// chosen, and this process's tests are the only ones that bind it.
pub fn stub_port() -> u16 {
    static PORT: OnceLock<u16> = OnceLock::new();
    *PORT.get_or_init(|| {
        for _ in 0..100 {
            let probe = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback port for the stub");
            let port = probe.local_addr().expect("the probe's address").port();
            drop(probe);
            if port != OPERATORS_PORT {
                return port;
            }
        }
        panic!("the OS gave no loopback port for the stub");
    })
}

/// `STUB_PORT` is the stub's port wherever a test writes it: in a format string, or with `.to_string()`. `stub_port()` is the number.
pub struct StubPort;
pub const STUB_PORT: StubPort = StubPort;

impl std::fmt::Display for StubPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", stub_port())
    }
}

/// Holding the stub's port (and the right to ask what is on it). One holder at a time, across threads and across processes: the tests of
/// one process share `stub_port()`, so they take turns. The lock is a `flock` on a file in the temp dir, which the system lets go of
/// when the holder exits, however it exits.
pub struct PortGuard {
    _file: File,
}

pub fn port_lock() -> PortGuard {
    let path = std::env::temp_dir().join("minerva-installer-stub.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .unwrap_or_else(|e| panic!("cannot open the port lock {}: {e}", path.display()));
    // each call opens its own file description, so threads of one process wait for each other just as processes do
    file.lock().unwrap();
    PortGuard { _file: file }
}

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

pub fn write(path: &Path, text: &str, mode: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

pub fn mkdir(path: &Path, mode: u32) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// What the stub saw of one request.
#[derive(Debug, Clone)]
pub struct Seen {
    pub request_line: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// A one-answer-fits-all HTTP server on 127.0.0.1:`port`, recording every request. Stops when dropped.
pub struct Stub {
    pub seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Stub {
    pub fn start(port: u16, status: u16, body: &str) -> Stub {
        Stub::start_slow(port, status, body, Duration::ZERO)
    }

    /// As `start`, but each answer is held back for `delay` after the request has been read (a perch that is slow to reply).
    pub fn start_slow(port: u16, status: u16, body: &str, delay: Duration) -> Stub {
        assert_ne!(port, OPERATORS_PORT, "a test never listens where the operator's perch does");
        let started = Instant::now();
        let listener = loop {
            match TcpListener::bind(("127.0.0.1", port)) {
                Ok(l) => break l,
                Err(e) if started.elapsed() < Duration::from_secs(10) => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => panic!("cannot bind the stub on 127.0.0.1:{port}: {e}"),
            }
        };
        listener.set_nonblocking(true).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, st2, body) = (seen.clone(), stop.clone(), body.to_string());
        let thread = std::thread::spawn(move || {
            while !st2.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((conn, _)) => {
                        if let Some(req) = serve_one(conn, status, &body, delay) {
                            s2.lock().unwrap().push(req);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                    Err(_) => break,
                }
            }
        });
        Stub { seen, stop, thread: Some(thread) }
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve_one(mut conn: TcpStream, status: u16, body: &str, delay: Duration) -> Option<Seen> {
    conn.set_nonblocking(false).ok()?;
    conn.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 1024];
    let head_end = loop {
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = conn.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?.to_string();
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    let want: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body_bytes = raw[head_end + 4..].to_vec();
    while body_bytes.len() < want {
        let n = conn.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        body_bytes.extend_from_slice(&buf[..n]);
    }
    std::thread::sleep(delay);
    let reply = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = conn.write_all(reply.as_bytes());
    Some(Seen { request_line, headers, body: String::from_utf8_lossy(&body_bytes).into_owned() })
}

/// The real `curl` on the standard path.
fn real_curl() -> PathBuf {
    for d in ["/usr/bin", "/bin", "/usr/local/bin"] {
        let p = Path::new(d).join("curl");
        if p.is_file() {
            return p;
        }
    }
    panic!("these tests need curl");
}

/// A fixture environment: a temp HOME that holds DECOY tokens at every default place a script could read one, a runtime dir, and a
/// `curl` shim first on PATH. The shim logs each call's argv (`argv.log`), the mode of every `-H @file` header file and its contents
/// (`headers.log`), and refuses (exit 7, logged as REFUSED) any call aimed at the operator's port, without making it.
pub struct Env {
    pub dir: tempfile::TempDir,
    pub home: PathBuf,
    pub run: PathBuf,
    pub shim: PathBuf,
}

/// The token a script must NOT pick up: it is at the default paths.
pub const DECOY: &str = "decoy-token-DECOY-token-0123456789abcdef";
/// The token the test hands over explicitly (and the stub must receive).
pub const GIVEN: &str = "given-token-GIVEN-token-0123456789abcdef";

impl Env {
    pub fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let home = root.join("home");
        let run = root.join("run");
        let shim = root.join("shim");
        mkdir(&home, 0o700);
        mkdir(&run, 0o700);
        mkdir(&shim, 0o700);
        mkdir(&run.join("minerva"), 0o700);
        // decoys: the perch's own token files for the stub's port AND the operator's port, and the agent tokens
        for port in [stub_port(), OPERATORS_PORT] {
            write(&run.join(format!("minerva/perch-{port}.token")), DECOY, 0o600);
        }
        for name in ["minerva-term.token", "minerva-mail.token", "jev.token", "browser.token"] {
            write(&home.join(".config/eidolon").join(name), DECOY, 0o600);
        }
        let shim_script = format!(
            r#"#!/bin/bash
log="{log}"
{{
  printf 'ARGV'
  printf ' [%s]' "$@"
  printf '\n'
}} >>"$log/argv.log"
prev=
for a in "$@"; do
  if [ "$prev" = -H ] && [ "${{a#@}}" != "$a" ]; then
    f=${{a#@}}
    printf 'HDRFILE %s mode=%s\n' "$f" "$(stat -c %a "$f")" >>"$log/argv.log"
    cat "$f" >>"$log/headers.log"
  fi
  case "$a" in
    *:{operators}*) printf 'REFUSED %s\n' "$a" >>"$log/argv.log"; echo "curl shim: refusing the operator's port" >&2; exit 7 ;;
  esac
  prev=$a
done
exec {curl} "$@"
"#,
            log = shim.display(),
            operators = OPERATORS_PORT,
            curl = real_curl().display()
        );
        write(&shim.join("curl"), &shim_script, 0o755);
        Env { dir, home, run, shim }
    }

    pub fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.shim.display())
    }

    /// A command with a cleared environment: only what the fixture sets.
    pub fn cmd(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut c = Command::new(program);
        c.env_clear()
            .env("PATH", self.path())
            .env("HOME", &self.home)
            .env("MINERVA_HOME", self.home.join("Minerva"))
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("LC_ALL", "C");
        c
    }

    pub fn argv_log(&self) -> String {
        std::fs::read_to_string(self.shim.join("argv.log")).unwrap_or_default()
    }

    pub fn headers_log(&self) -> String {
        std::fs::read_to_string(self.shim.join("headers.log")).unwrap_or_default()
    }

    /// Header files a script left behind.
    pub fn leftover_headers(&self) -> Vec<String> {
        std::fs::read_dir(self.run.join("minerva"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("hdr."))
            .collect()
    }
}

pub fn text(o: &Output) -> (String, String) {
    (String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}
