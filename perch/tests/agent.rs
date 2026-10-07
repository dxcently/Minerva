//! The agent gate (slice AG): the scoped term and mail tokens, their files, and which chat a call came from.
//!
//! Three layers, each tested where it lives:
//!
//! - the **files** `--term-token-file`, `--mail-token-file` and `--agent-url-file` write: `0600` in a `0700` directory, rotated every
//!   start, removed on SIGTERM only while they still hold what this perch put there, refused (not served without) when they cannot be
//!   made, and absent when the flags are;
//! - the **gate**'s answer table on the wire: 401 for no credential and for the other scope's token, 404 for the master credential, 403
//!   for the right token from a socket no live door owns, and `{chat}` for `whoami` from a door's own socket (the fake door makes that
//!   call itself, `FAKE_AGENT_CALL`; a `sh -c curl` child of the door is the 403 case);
//! - the **attribution** (`perch::agent::attrib::chat_of`) against real sockets and real processes: the door that holds the socket is
//!   named, a child of a door is not, a door that has ended is not, and 1000 lookups across 16 doors take well under a second.

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use common::*;

// ---------------------------------------------------------------------------
// The files a perch is told to write
// ---------------------------------------------------------------------------

/// A directory that does not exist yet, and the three files below it. The perch makes the directory (`0700`); the test only names it.
struct Files {
    _root: tempfile::TempDir,
    dir: PathBuf,
    term: PathBuf,
    mail: PathBuf,
    url: PathBuf,
}

impl Files {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("agent");
        Self { term: dir.join("term.token"), mail: dir.join("mail.token"), url: dir.join("perch.url"), dir, _root: root }
    }

    fn flags(&self) -> Vec<OsString> {
        let mut flags = Vec::new();
        for (flag, path) in [("--term-token-file", &self.term), ("--mail-token-file", &self.mail), ("--agent-url-file", &self.url)] {
            flags.push(flag.into());
            flags.push(path.into());
        }
        flags
    }

    /// What the fake door reads to find the perch and the tokens (the names are `fake_eidolon/agent.rs`'s).
    #[cfg(feature = "test-bins")]
    fn fake_env(&self) -> Vec<(String, String)> {
        vec![
            ("FAKE_AGENT_URL_FILE".into(), self.url.display().to_string()),
            ("FAKE_AGENT_TERM_TOKEN_FILE".into(), self.term.display().to_string()),
            ("FAKE_AGENT_MAIL_TOKEN_FILE".into(), self.mail.display().to_string()),
        ]
    }

    fn perch(&self) -> Perch {
        Perch::builder().args(self.flags()).start()
    }

    fn term_token(&self) -> String {
        read(&self.term)
    }

    fn mail_token(&self) -> String {
        read(&self.mail)
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).trim().to_string()
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).permissions().mode() & 0o777
}

/// A request as a tool makes it: a POST with the scoped token as the bearer.
fn call(perch: &Perch, path: &str, token: &str) -> Answer {
    Request::new(&perch.addr, "POST", path).bearer(format!("Bearer {token}")).body(b"{}".to_vec()).send(&perch.addr)
}

/// No secret is in the perch's argv or environment, its boot lines or its trace-level log: the harness runs every perch at `RUST_LOG=trace`.
fn assert_no_secret(perch: &Perch, secrets: &[&str]) {
    let (argv, environ) = proc_of(perch.child.id());
    for secret in secrets {
        assert!(secret.len() >= 40, "a real token, not a placeholder");
        assert!(!argv.contains(secret), "a token in the perch's argv");
        assert!(!environ.contains(secret), "a token in the perch's environment");
        assert!(!perch.out().contains(secret), "a token on the perch's stdout");
        assert!(!perch.err().contains(secret), "a token in the perch's log");
    }
}

// ---------------------------------------------------------------------------
// The files
// ---------------------------------------------------------------------------

#[test]
fn without_the_flags_there_are_no_files_and_no_scoped_credential_opens_anything() {
    let perch = Perch::start();
    // The one token file in the perch's tree is the perch's own, in its own directory.
    let mut tokens = Vec::new();
    let mut stack = vec![perch.runtime.path().to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                tokens.push(path);
            }
        }
    }
    assert_eq!(tokens, std::slice::from_ref(&perch.token_file), "only the perch's own token file exists");
    for path in ["/hub/term/a/whoami", "/hub/mail/a/whoami", "/hub/term/a/list", "/hub/mail/a/send"] {
        // An empty bearer must not match the empty token of a scope nobody started; nor must anything else.
        for bearer in ["Bearer ", "Bearer x", "Bearer null", "Bearer undefined"] {
            let answer = Request::new(&perch.addr, "POST", path).bearer(bearer).send(&perch.addr);
            assert_eq!((answer.status, answer.body.as_str()), (401, ""), "{path} with {bearer:?}: {}", answer.head);
        }
    }
}

#[test]
fn the_files_are_0600_in_a_0700_directory_and_hold_three_different_things() {
    let files = Files::new();
    let perch = files.perch();
    assert_eq!(mode(&files.dir), 0o700, "the directory the perch made");
    for path in [&files.term, &files.mail, &files.url] {
        assert_eq!(mode(path), 0o600, "{}", path.display());
    }
    let (term, mail, master) = (files.term_token(), files.mail_token(), perch.token.trim().to_string());
    assert_eq!((term.len(), mail.len()), (43, 43), "256 bits, url-safe");
    assert!(term != mail && term != master && mail != master, "one secret per scope, and neither is the master token");
    assert_eq!(read(&files.url), format!("http://{}/", perch.addr), "the url is the bound address");
    assert_eq!(std::fs::read_to_string(&files.url).unwrap(), format!("http://{}/", perch.addr), "no trailing newline: a tool reads one line");
    // The boot lines are still exactly two, and name neither file.
    assert_eq!(perch.out().lines().count(), 2, "{}", perch.out());
    assert!(!perch.out().contains("agent"), "{}", perch.out());
    // Some traffic, so there is a log to look in.
    call(&perch, "/hub/term/a/whoami", &term);
    call(&perch, "/hub/mail/a/whoami", &mail);
    call(&perch, "/hub/term/a/whoami", &master);
    assert_no_secret(&perch, &[&term, &mail, &master]);
}

#[test]
fn a_directory_that_exists_already_is_judged_and_never_chmoded() {
    // One that others cannot write in is somebody's choice (a home directory is 0755) and is used as it is.
    let files = Files::new();
    std::fs::create_dir(&files.dir).unwrap();
    own(&files.dir, 0o755);
    let perch = files.perch();
    assert_eq!(mode(&files.dir), 0o755, "an existing directory is not tightened");
    assert_eq!(mode(&files.term), 0o600);
    drop(perch);

    // One that group or others can write in is a place where a file can be swapped for another: refused, with a sentence, untouched.
    for loose in [0o775, 0o757, 0o777] {
        let files = Files::new();
        std::fs::create_dir(&files.dir).unwrap();
        own(&files.dir, loose);
        let (refused, stdout, stderr) = run_refused(&files.flags());
        assert!(refused && !stdout.contains("listening"), "{loose:o} must refuse the start: {stdout}{stderr}");
        assert!(stderr.contains("writable by group or others") && stderr.contains(&files.dir.display().to_string()), "{loose:o}: {stderr}");
        assert_eq!(mode(&files.dir), loose, "{loose:o} is left as it was");
        assert!(!files.term.exists() && !files.mail.exists() && !files.url.exists(), "nothing was written in it");
    }
}

#[test]
fn the_files_rotate_every_start_and_go_with_a_sigterm() {
    let files = Files::new();
    let mut first = files.perch();
    let (term1, mail1) = (files.term_token(), files.mail_token());
    let status = first.stop();
    assert!(status.success(), "a clean exit: {status}");
    for path in [&files.term, &files.mail, &files.url] {
        assert!(!path.exists(), "{} is gone after SIGTERM", path.display());
    }
    assert!(files.dir.is_dir(), "the directory is not the perch's to remove");

    let mut second = files.perch();
    let (term2, mail2) = (files.term_token(), files.mail_token());
    assert!(term2 != term1 && mail2 != mail1, "a new start, new tokens");
    // The old token opens nothing on the new perch, and the new one is the real credential (403: right token, no door).
    assert_eq!(call(&second, "/hub/term/a/whoami", &term1).status, 401);
    assert_eq!(call(&second, "/hub/term/a/whoami", &term2).status, 403);
    assert!(second.stop().success());
    assert!(!files.term.exists() && !files.mail.exists() && !files.url.exists());
}

#[test]
fn a_file_the_perch_did_not_write_is_not_the_perchs_to_remove() {
    let files = Files::new();
    let mut perch = files.perch();
    // A newer perch, or the operator, wrote these since: the content is someone else's.
    std::fs::write(&files.term, "written-by-a-newer-perch\n").unwrap();
    std::fs::write(&files.url, "http://127.0.0.1:1/").unwrap();
    // The mail file is swapped for a link to a file of the operator's: removing it, or following it, would be the wrong thing.
    let precious = files.dir.join("precious");
    std::fs::write(&precious, "keep me\n").unwrap();
    std::fs::set_permissions(&precious, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    std::fs::remove_file(&files.mail).unwrap();
    std::os::unix::fs::symlink(&precious, &files.mail).unwrap();
    assert!(perch.stop().success());
    assert_eq!(std::fs::read_to_string(&files.term).unwrap(), "written-by-a-newer-perch\n", "left in place");
    assert_eq!(std::fs::read_to_string(&files.url).unwrap(), "http://127.0.0.1:1/", "left in place");
    assert!(std::fs::symlink_metadata(&files.mail).unwrap().file_type().is_symlink(), "a link is not followed and not removed");
    assert_eq!(std::fs::read_to_string(&precious).unwrap(), "keep me\n", "the link's target is untouched");
}

#[test]
fn a_stale_file_from_a_crash_is_replaced_and_its_token_opens_nothing() {
    let files = Files::new();
    // What a crash left: files from a perch that is gone, one of them looser than it should be.
    std::fs::create_dir(&files.dir).unwrap();
    own(&files.dir, 0o755);
    std::fs::write(&files.term, "stale-term-token\n").unwrap();
    std::fs::set_permissions(&files.term, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    let mut perch = files.perch();
    assert_eq!(mode(&files.term), 0o600, "replaced by a fresh 0600 file, not chmod'ed into shape");
    assert_ne!(files.term_token(), "stale-term-token");
    let first = files.term_token();
    // A SIGKILL leaves the files behind, as a crash does...
    perch.child.kill().unwrap();
    perch.child.wait().unwrap();
    assert!(files.term.exists() && files.mail.exists() && files.url.exists());
    // ...and the next start rotates them: the dead perch's token is nobody's.
    let again = files.perch();
    assert_ne!(files.term_token(), first);
    assert_eq!(call(&again, "/hub/term/a/whoami", &first).status, 401);
}

#[test]
fn a_start_that_cannot_make_a_private_file_is_refused_not_served_without() {
    let root = tempfile::tempdir().unwrap();
    let a_file = root.path().join("a-file");
    std::fs::write(&a_file, "x").unwrap();
    let term = root.path().join("t.token");
    let mut cases: Vec<(&str, Vec<OsString>)> = vec![
        ("not an absolute path", vec!["--term-token-file".into(), "term.token".into()]),
        ("not an absolute path", vec!["--mail-token-file".into(), "./m/mail.token".into()]),
        ("not an absolute path", vec!["--agent-url-file".into(), "perch.url".into()]),
        ("name the same file", vec!["--term-token-file".into(), term.clone().into(), "--mail-token-file".into(), term.clone().into()]),
        ("name the same file", vec!["--mail-token-file".into(), term.clone().into(), "--agent-url-file".into(), term.clone().into()]),
        ("creating", vec!["--term-token-file".into(), a_file.join("below").join("t.token").into()]),
    ];
    // A directory that is not ours is not a place for a secret. (As root every directory is ours, and the case would write in /usr.)
    if unsafe { libc::getuid() } != 0 {
        cases.push(("checking", vec!["--term-token-file".into(), "/usr/t.token".into()]));
    }
    // A directory that is ours and private, below one that anyone can add to and rename in (no sticky bit): whoever can rename the one
    // above can put their own directory where this one was. (With the sticky bit, as /tmp has, it is fine: every test here lives there.)
    let loose = root.path().join("loose");
    std::fs::create_dir_all(loose.join("agent")).unwrap();
    own(&loose.join("agent"), 0o700);
    own(&loose, 0o777);
    cases.push(("not sticky", vec!["--term-token-file".into(), loose.join("agent").join("t.token").into()]));
    for (why, args) in cases {
        let (refused, stdout, stderr) = run_refused(&args);
        assert!(refused, "{args:?} must refuse the start: {stderr}");
        assert!(!stdout.contains("listening"), "no boot line from a perch that then dies: {stdout}");
        assert!(stderr.contains(why), "{args:?}: the refusal says why ({why:?}): {stderr}");
    }
    assert!(!term.exists(), "nothing was left behind");
    assert!(!Path::new("/usr/t.token").exists());
}

#[test]
fn a_flag_not_given_writes_nothing_and_leaves_its_scope_closed() {
    let files = Files::new();
    // Only the term scope. The mail scope has no token at all: not even the term token opens it.
    let perch = Perch::builder().args(["--term-token-file".into(), files.term.clone().into_os_string()]).start();
    assert!(files.term.exists() && !files.mail.exists() && !files.url.exists());
    let term = files.term_token();
    assert_eq!(call(&perch, "/hub/term/a/whoami", &term).status, 403, "the term scope is open (to a door)");
    assert_eq!(call(&perch, "/hub/mail/a/whoami", &term).status, 401, "the mail scope is not");
    assert_eq!(Request::new(&perch.addr, "POST", "/hub/mail/a/whoami").bearer("Bearer ").send(&perch.addr).status, 401);
}

/// The perch asked to bind `bind` with `args` and a runtime dir of its own, and expected to refuse: `(refused, stdout, stderr)`. A perch
/// that starts instead is killed after 20 s and the test fails, so a start that should have been refused is a failure and not a hang.
fn refusal_of(bind: &str, runtime: &Path, args: &[OsString]) -> (bool, String, String) {
    use std::sync::{Arc, Mutex};
    let xdg = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_perch"))
        .arg("--bind")
        .arg(bind)
        .args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("RUST_LOG", "debug")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("running the perch binary");
    let (stdout, stderr) = (Arc::new(Mutex::new(String::new())), Arc::new(Mutex::new(String::new())));
    pump(child.stdout.take().unwrap(), stdout.clone());
    pump(child.stderr.take().unwrap(), stderr.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the perch was not refused: it started and kept serving ({args:?}): {}", stdout.lock().unwrap());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    std::thread::sleep(std::time::Duration::from_millis(100)); // the pumps read what the pipes still hold
    let (stdout, stderr) = (stdout.lock().unwrap().clone(), stderr.lock().unwrap().clone());
    (!status.success(), stdout, stderr)
}

/// [`refusal_of`] with a runtime dir made for it and the kernel's choice of port.
fn run_refused(args: &[OsString]) -> (bool, String, String) {
    let runtime = tempfile::tempdir().unwrap();
    own(runtime.path(), 0o700);
    refusal_of("127.0.0.1:0", runtime.path(), args)
}

#[test]
fn a_start_that_fails_part_way_takes_back_what_it_wrote() {
    // Not as root: there /usr is ours and the second file would be written.
    if unsafe { libc::getuid() } == 0 {
        return;
    }
    let files = Files::new();
    let args: Vec<OsString> = vec!["--term-token-file".into(), files.term.clone().into(), "--mail-token-file".into(), "/usr/m.token".into()];
    let (refused, stdout, stderr) = run_refused(&args);
    assert!(refused && !stdout.contains("listening"), "the mail file cannot be made, so the start is refused: {stdout}{stderr}");
    assert!(stderr.contains("/usr"), "{stderr}");
    assert!(!files.term.exists(), "the term file, which was written first and was good, is gone with the start that wrote it");
    assert!(!Path::new("/usr/m.token").exists());
}

#[test]
fn two_flags_that_reach_one_file_by_different_spellings_are_refused_and_leave_nothing() {
    let files = Files::new();
    std::fs::create_dir(&files.dir).unwrap();
    own(&files.dir, 0o700);
    // `x` and `agent/../agent/x` are different strings and one file: the url write replaces the term write, and the term file no longer
    // reads back what the perch put in it.
    let x = files.dir.join("x");
    let again = files.dir.join("..").join("agent").join("x");
    let args: Vec<OsString> = vec!["--term-token-file".into(), x.clone().into(), "--agent-url-file".into(), again.into()];
    let (refused, stdout, stderr) = run_refused(&args);
    assert!(refused && !stdout.contains("listening"), "{stdout}{stderr}");
    assert!(stderr.contains("does not hold"), "the refusal says what went wrong: {stderr}");
    assert!(!x.exists(), "nothing is left: the one file there is the last write, which is the url file's, and it was taken back");
    assert!(std::fs::read_dir(&files.dir).unwrap().next().is_none(), "no stray temp file either");
}

/// `--bind 127.0.0.1:<port>` with its own runtime dir, so the master token file's path is known before the start: the perch's own
/// token file, `<runtime>/minerva/perch-<port>.token`; `spell` says how `--term-token-file` writes it. Returns what the refusal printed.
fn refused_naming_the_master_file(spell: fn(&Path) -> PathBuf) -> (bool, String, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let runtime = tempfile::tempdir().unwrap();
    own(runtime.path(), 0o700);
    let master = runtime.path().join("minerva").join(format!("perch-{port}.token"));
    let args: Vec<OsString> = vec!["--term-token-file".into(), spell(&master).into()];
    let answer = refusal_of(&format!("127.0.0.1:{port}"), runtime.path(), &args);
    assert!(!master.exists(), "the perch's own token file was taken back by its own exit, so it still held the master token: {answer:?}");
    answer
}

#[test]
fn no_agent_file_may_be_the_perchs_own_token_file() {
    // By its own name, and by a spelling that gets to the same directory some other way. Neither is written: if either had been, the
    // master token file would have been replaced by a scoped token, and the perch's exit (which takes back only the file that still
    // holds the master token) would have left it behind, which the helper asserts it did not.
    fn the_same_path(master: &Path) -> PathBuf {
        master.to_path_buf()
    }
    fn through_dot_dot(master: &Path) -> PathBuf {
        master.parent().unwrap().join("..").join("minerva").join(master.file_name().unwrap())
    }
    for (what, spell) in [("the same path", the_same_path as fn(&Path) -> PathBuf), ("through `..`", through_dot_dot)] {
        let (refused, stdout, stderr) = refused_naming_the_master_file(spell);
        assert!(refused && !stdout.contains("listening"), "{what}: the start is refused: {stdout}{stderr}");
        assert!(stderr.contains("the perch's own token file"), "{what}: {stderr}");
        // Refused before anything was written, and not after: a write there would have replaced the master token with a scoped one, which
        // the exit then finds gone (the agent's cleanup took it) and says so.
        assert!(!stderr.contains("token file not read"), "{what}: the master token file was written over before the refusal: {stderr}");
    }
}

/// A new file with these bytes at `path`, put there the way a later perch would (written beside, then renamed over): another inode,
/// the same content, the same mode.
fn replace_with_the_same_bytes(path: &Path) {
    use std::io::Write as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let bytes = std::fs::read(path).unwrap();
    let before = std::fs::metadata(path).unwrap().ino();
    let beside = path.with_extension("swap");
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&beside).unwrap();
    file.write_all(&bytes).unwrap();
    drop(file);
    std::fs::rename(&beside, path).unwrap();
    assert_ne!(std::fs::metadata(path).unwrap().ino(), before, "a new file");
    assert_eq!(std::fs::read(path).unwrap(), bytes, "with the bytes the old one had");
}

#[test]
fn a_file_with_the_same_bytes_but_not_the_same_file_is_not_the_perchs_to_remove() {
    // A later perch on the same port writes the same url, and a later perch with the same flags writes its own tokens: here the bytes
    // are made equal on purpose, so only the identity of the file can tell them from this perch's own.
    let files = Files::new();
    let mut a = files.perch();
    replace_with_the_same_bytes(&files.url);
    replace_with_the_same_bytes(&files.term);
    assert!(a.stop().success());
    assert!(files.url.exists(), "the url file is another file now, and not A's to remove");
    assert!(files.term.exists(), "so is the term file");
    assert_eq!(read(&files.url), format!("http://{}/", a.addr), "untouched");
    assert!(!files.mail.exists(), "the mail file was never replaced: A's, and removed, which shows the exit did run");
}

#[test]
fn a_fifo_where_a_file_was_is_refused_and_does_not_wedge_the_exit() {
    use std::os::unix::fs::FileTypeExt as _;
    let files = Files::new();
    let mut perch = files.perch();
    let mkfifo = |path: &Path| {
        std::fs::remove_file(path).unwrap();
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0, "mkfifo {}", path.display());
    };
    // One of the agent's files, and the perch's own token file: a blocking open of either would wait for a writer that never comes.
    mkfifo(&files.term);
    mkfifo(&perch.token_file);
    let status = perch.stop(); // panics if the perch has not exited within 10 s
    assert!(status.success(), "a clean exit: {status}");
    for path in [&files.term, &perch.token_file] {
        assert!(std::fs::symlink_metadata(path).unwrap().file_type().is_fifo(), "{} is left where it is", path.display());
    }
    assert!(!files.mail.exists() && !files.url.exists(), "the files that were still the perch's own were taken back");
}

// ---------------------------------------------------------------------------
// The gate's answers
// ---------------------------------------------------------------------------

/// `POST /hub/code` with the master token, then `POST /hub/redeem`: the `name=value` of the login cookie a page holds.
fn login_cookie(perch: &Perch) -> String {
    let minted: serde_json::Value = serde_json::from_str(&perch.post("/hub/code").body).unwrap();
    let body = serde_json::to_vec(&serde_json::json!({ "code": minted["code"] })).unwrap();
    let redeemed = Request::new(&perch.addr, "POST", "/hub/redeem").body(body).send(&perch.addr);
    assert_eq!(redeemed.status, 204, "{}", redeemed.head);
    redeemed.header("set-cookie").expect("a cookie").split(';').next().unwrap().to_string()
}

#[test]
fn the_master_credential_is_a_404_on_an_agent_path_and_a_stranger_is_a_401() {
    let files = Files::new();
    let perch = files.perch();
    let master = perch.token.trim().to_string();
    let (term, mail) = (files.term_token(), files.mail_token());
    let cookie = login_cookie(&perch);

    for path in ["/hub/term/a/whoami", "/hub/mail/a/whoami", "/hub/term/a/list", "/hub/term/a/open", "/hub/mail/a/send"] {
        // The master bearer is a real credential, for the other door: nothing is here for it, and nothing says what.
        let by_bearer = perch.apireq("POST", path).send(&perch.addr);
        assert_eq!((by_bearer.status, by_bearer.body.as_str()), (404, ""), "{path} with the master bearer: {}", by_bearer.head);
        // The login cookie, as the page sends it (same-origin), is the same credential.
        let by_cookie = Request::new(&perch.addr, "POST", path).header("Cookie", &cookie).header("Sec-Fetch-Site", "same-origin").send(&perch.addr);
        assert_eq!((by_cookie.status, by_cookie.body.as_str()), (404, ""), "{path} with the cookie: {}", by_cookie.head);
        // …and a cookie that the master gate would not count (no fetch metadata) is nothing here either.
        let bare_cookie = Request::new(&perch.addr, "POST", path).header("Cookie", &cookie).send(&perch.addr);
        assert_eq!(bare_cookie.status, 401, "{path}: {}", bare_cookie.head);
        // No credential, a wrong one, the scheme spelled another way, the token where a bearer is not: all the 401 of a stranger.
        for (what, request) in [
            ("no credential", Request::new(&perch.addr, "POST", path)),
            ("a wrong bearer", Request::new(&perch.addr, "POST", path).bearer("Bearer nobody")),
            ("an empty bearer", Request::new(&perch.addr, "POST", path).bearer("Bearer ")),
            ("a lower-case scheme", Request::new(&perch.addr, "POST", path).bearer(format!("bearer {term}"))),
            ("a bare token", Request::new(&perch.addr, "POST", path).bearer(term.clone())),
            ("the term token as a cookie", Request::new(&perch.addr, "POST", path).header("Cookie", &format!("minerva_{}={term}", perch.port))),
            ("the term token in the query", Request::new(&perch.addr, "POST", &format!("{path}?token={term}"))),
            ("the mail token in a header of its own", Request::new(&perch.addr, "POST", path).header("X-Token", &mail)),
        ] {
            let answer = request.send(&perch.addr);
            assert_eq!((answer.status, answer.body.as_str()), (401, ""), "{path} with {what}: {}", answer.head);
        }
    }

    // The master bearer wins nothing by being sent beside a scoped one: the scoped one is what is judged first.
    let both = Request::new(&perch.addr, "POST", "/hub/term/a/whoami").bearer(format!("Bearer {term}")).header("Cookie", &cookie).send(&perch.addr);
    assert_eq!(both.status, 403, "the scoped token decides, and no door owns the test process: {}", both.head);
    // The master gate itself is untouched.
    assert_eq!(perch.get("/hub/sessions").status, 200);
    assert_no_secret(&perch, &[&term, &mail, &master]);
}

#[test]
fn a_scoped_token_is_good_for_its_own_prefix_and_nowhere_else() {
    let files = Files::new();
    let perch = files.perch();
    let (term, mail) = (files.term_token(), files.mail_token());
    // The other scope's token: a stranger's 401, whatever the verb.
    for path in ["/hub/term/a/whoami", "/hub/term/a/list", "/hub/term/a/open", "/hub/term/a/read", "/hub/term/a/send", "/hub/term/a/close"] {
        let answer = call(&perch, path, &mail);
        assert_eq!((answer.status, answer.body.as_str()), (401, ""), "the mail token on {path}: {}", answer.head);
    }
    for path in ["/hub/mail/a/whoami", "/hub/mail/a/send"] {
        let answer = call(&perch, path, &term);
        assert_eq!((answer.status, answer.body.as_str()), (401, ""), "the term token on {path}: {}", answer.head);
    }
    // Either token anywhere else is the ordinary 401 of the master gate: it is not a way into /hub.
    for token in [&term, &mail] {
        for (method, path) in [
            ("GET", "/hub/sessions"),
            ("POST", "/hub/sessions"),
            ("GET", "/hub/term"),
            ("GET", "/hub/term/events"),
            ("POST", "/hub/term/a"),
            ("POST", "/hub/term/a/"),
            ("POST", "/hub/term/a/x/y"),
            ("POST", "/hub/term/t1/input"),
            ("POST", "/hub/mail/a"),
            ("GET", "/hub/mail"),
            ("POST", "/hub/code"),
            ("GET", "/s/x/api/events"),
            ("GET", "/hub"),
        ] {
            let answer = Request::new(&perch.addr, method, path).bearer(format!("Bearer {token}")).send(&perch.addr);
            assert_eq!((answer.status, answer.body.as_str()), (401, ""), "{method} {path} with a scoped token: {}", answer.head);
        }
    }
    // The right token from a socket no door owns: a 403 that tells the agent what to do, and the other gates are not what it hit.
    for (path, token, words) in [
        ("/hub/term/a/whoami", &term, "term tools work in chats this Minerva opened"),
        ("/hub/term/a/list", &term, "term tools work in chats this Minerva opened"),
        ("/hub/mail/a/whoami", &mail, "mail tools work in chats this Minerva opened"),
        ("/hub/mail/a/send", &mail, "mail tools work in chats this Minerva opened"),
    ] {
        let answer = call(&perch, path, token);
        assert_eq!(answer.status, 403, "{path}: {}", answer.head);
        assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{}", answer.head);
        let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap();
        assert_eq!(body["error"], words, "{path}");
        assert!(!answer.body.contains(token.as_str()) && !answer.head.contains(token.as_str()), "a token is never echoed");
    }
}

// ---------------------------------------------------------------------------
// A door's own socket, through the fake door (FAKE_AGENT_CALL)
// ---------------------------------------------------------------------------

/// The fake door's `agent ...` lines in `$FAKE_LOG`, for the door with this pid.
#[cfg(feature = "test-bins")]
fn agent_lines(perch: &Perch, pid: u32) -> Vec<String> {
    let mark = format!("agent pid={pid} ");
    perch.fake_records().into_iter().filter(|line| line.starts_with(&mark)).collect()
}

#[cfg(feature = "test-bins")]
fn say(perch: &Perch, id: &str) {
    let said = perch.post_json(&format!("/s/{id}/api/say"), serde_json::json!({ "text": "hi" }));
    assert_eq!(said.status, 202, "{}{}", said.head, said.body);
}

/// Waits for the door to have recorded `n` agent lines.
#[cfg(feature = "test-bins")]
fn agent_lines_after(perch: &Perch, pid: u32, n: usize) -> Vec<String> {
    assert!(
        wait_record(perch, std::time::Duration::from_secs(20), |records| records.iter().filter(|l| l.starts_with(&format!("agent pid={pid} "))).count() >= n),
        "the door made {n} calls: {:?}",
        perch.fake_records()
    );
    agent_lines(perch, pid)
}

#[cfg(feature = "test-bins")]
#[test]
fn a_door_is_named_by_its_own_socket_and_the_test_process_is_not_a_door() {
    let files = Files::new();
    let perch = Perch::builder().hub().args(files.flags()).envs(files.fake_env()).env("FAKE_AGENT_CALL", "term/whoami:{}\nmail/whoami:{}").start();
    let (id1, pid1, _) = live(&perch);
    let (id2, pid2, _) = live(&perch);
    assert_ne!(id1, id2);

    // Each door, from its own socket, is told its own chat, on both prefixes.
    say(&perch, &id1);
    let first = agent_lines_after(&perch, pid1, 2);
    say(&perch, &id2);
    let second = agent_lines_after(&perch, pid2, 2);
    for (lines, pid, id) in [(&first, pid1, &id1), (&second, pid2, &id2)] {
        let chat = format!("{{\"chat\":\"{id}\"}}");
        assert!(lines.contains(&format!("agent pid={pid} via=door term/whoami 200 {chat}")), "{lines:?}");
        assert!(lines.contains(&format!("agent pid={pid} via=door mail/whoami 200 {chat}")), "{lines:?}");
    }
    assert!(first.iter().all(|line| !line.contains(&id2)) && second.iter().all(|line| !line.contains(&id1)), "no door is told another's chat");

    // The same tokens from the test process, with two live doors on the box, name nobody.
    let (term, mail) = (files.term_token(), files.mail_token());
    assert_eq!(call(&perch, "/hub/term/a/whoami", &term).status, 403);
    assert_eq!(call(&perch, "/hub/mail/a/whoami", &mail).status, 403);
    // A door that held no turn does not matter: `whoami` is not about a turn. A second call from the same door still works.
    say(&perch, &id1);
    assert_eq!(agent_lines_after(&perch, pid1, 4).len(), 4);
    assert_no_secret(&perch, &[&term, &mail, perch.token.trim()]);
    // The doors read the tokens from their files, and the perch gave them none: not in a door's argv or environment either.
    assert_no_secret_in_doors(&[pid1, pid2], &[&term, &mail, perch.token.trim()]);
}

/// No secret in the argv or the environment of these live doors: the tokens reach a door by its reading a file, never by its start.
#[cfg(feature = "test-bins")]
fn assert_no_secret_in_doors(pids: &[u32], secrets: &[&str]) {
    for pid in pids {
        let (argv, environ) = proc_of(*pid);
        assert!(!argv.is_empty() && !environ.is_empty(), "door {pid}: a real argv and environment were read");
        for secret in secrets {
            assert!(secret.len() >= 40, "a real token, not a placeholder");
            assert!(!argv.contains(secret), "a token in door {pid}'s argv");
            assert!(!environ.contains(secret), "a token in door {pid}'s environment");
        }
    }
}

/// `whoami` is answered by the gate, which owes the gates `serve` would have run for it: the `Host`, fetch metadata and the method.
/// Asked from a door's own socket (a tool that is not a door gets 403 before any of them), each with a request that is wrong in that
/// one way, and then with one that is right.
#[cfg(feature = "test-bins")]
#[test]
fn whoami_keeps_the_hosts_fetch_metadatas_and_methods_gates() {
    let files = Files::new();
    let script = [
        "[host=evil.example]term/whoami:{}",
        "[sec-fetch-site=cross-site]term/whoami:{}",
        "[sec-fetch-site=same-site]mail/whoami:{}",
        "[method=GET]term/whoami:{}",
        "[method=PUT]mail/whoami:{}",
        "[host=localhost:1,sec-fetch-site=cross-site,method=GET]term/whoami:{}",
        "[sec-fetch-site=same-origin]term/whoami:{}",
        "term/whoami:{}",
    ]
    .join("\n");
    let perch = Perch::builder().hub().args(files.flags()).envs(files.fake_env()).env("FAKE_AGENT_CALL", script).start();
    let (id, pid, _) = live(&perch);
    say(&perch, &id);
    let lines = agent_lines_after(&perch, pid, 8);
    let at = |n: usize| lines[n].strip_prefix(&format!("agent pid={pid} via=door ")).unwrap_or_else(|| panic!("{:?}", lines[n])).to_string();
    let chat = format!("{{\"chat\":\"{id}\"}}");
    // The fake records in order, and an empty body leaves the line ending in a space.
    assert_eq!(at(0), "term/whoami 403 ", "a Host that is not this perch's");
    assert_eq!(at(1), "term/whoami 403 ", "cross-site");
    assert_eq!(at(2), "mail/whoami 403 ", "same-site");
    assert_eq!(at(3), "term/whoami 405 ", "GET");
    assert_eq!(at(4), "mail/whoami 405 ", "PUT");
    assert_eq!(at(5), "term/whoami 403 ", "wrong in three ways: the Host and fetch metadata come before the method");
    assert_eq!(at(6), format!("term/whoami 200 {chat}"), "same-origin is fine");
    assert_eq!(at(7), format!("term/whoami 200 {chat}"), "and the plain call");
}

/// A call that passes the gate reaches the module's route with the chat in its extensions. Until T2 and M3 land the routes are the
/// empty 404 of their stubs, which is neither the gate's 401 (no credential) nor its 403 (no door): the door got through.
#[cfg(feature = "test-bins")]
#[test]
fn a_call_that_passes_the_gate_reaches_the_modules_route() {
    let files = Files::new();
    let perch = Perch::builder().hub().args(files.flags()).envs(files.fake_env()).env("FAKE_AGENT_CALL", "term/list:{}\nmail/send:{}\nterm/whoami:{}").start();
    let (id, pid, _) = live(&perch);
    say(&perch, &id);
    let lines = agent_lines_after(&perch, pid, 3);
    assert!(lines.contains(&format!("agent pid={pid} via=door term/list 404 ")), "the term module's stub, past the gate: {lines:?}");
    assert!(lines.contains(&format!("agent pid={pid} via=door mail/send 404 ")), "the mail module's stub, past the gate: {lines:?}");
    assert!(lines.contains(&format!("agent pid={pid} via=door term/whoami 200 {{\"chat\":\"{id}\"}}")), "{lines:?}");
    // The same two calls from the door's child curl never get that far.
    drop(perch);
    let files = Files::new();
    let child = Perch::builder().hub().args(files.flags()).envs(files.fake_env()).env("FAKE_AGENT_VIA", "child").env("FAKE_AGENT_CALL", "term/list:{}\nmail/send:{}").start();
    let (id, pid, _) = live(&child);
    say(&child, &id);
    let lines = agent_lines_after(&child, pid, 2);
    for line in &lines {
        assert!(line.contains(" 403 "), "a door's child is refused before any route: {line}");
    }
}

/// A door's environment as the perch gave it: `NAME=value` pairs.
#[cfg(feature = "test-bins")]
fn environ_of(pid: u32) -> std::collections::HashMap<String, String> {
    let (_, environ) = proc_of(pid);
    environ.split('\0').filter_map(|pair| pair.split_once('=')).map(|(name, value)| (name.to_string(), value.to_string())).collect()
}

/// Real doors are built on HTTP clients that send a loopback call to the operator's proxy when `http_proxy` or `ALL_PROXY` is set (they
/// have no bypass of their own), which would make every call arrive from the proxy's socket (a 403) and put the scoped token in the
/// proxy's history. The perch gives each door a `NO_PROXY` that lists loopback, after whatever the operator had. The fake door does what
/// those clients do, so a dead proxy is a door that cannot make its call unless the bypass is there.
#[cfg(feature = "test-bins")]
#[test]
fn a_door_under_an_http_proxy_still_reaches_the_perch_on_loopback() {
    let files = Files::new();
    // Port 1 has nothing listening: a call that goes to this proxy fails to connect.
    let dead = "http://127.0.0.1:1";
    let perch = Perch::builder()
        .hub()
        .args(files.flags())
        .envs(files.fake_env())
        .env("http_proxy", dead)
        .env("HTTP_PROXY", dead)
        .env("ALL_PROXY", dead)
        .env("NO_PROXY", "corp.example")
        .env("no_proxy", "corp.example")
        .env("FAKE_AGENT_CALL", "term/whoami:{}\nmail/whoami:{}")
        .start();
    let (id, pid, _) = live(&perch);
    let door = environ_of(pid);
    assert_eq!(door.get("http_proxy").map(String::as_str), Some(dead), "the operator's proxy is the door's too");
    assert_eq!(door.get("NO_PROXY").map(String::as_str), Some("corp.example,127.0.0.1,localhost,::1"), "what the operator listed, then loopback");
    assert_eq!(door.get("no_proxy").map(String::as_str), Some("corp.example,127.0.0.1,localhost,::1"), "the other spelling gets the same");
    say(&perch, &id);
    let lines = agent_lines_after(&perch, pid, 2);
    let chat = format!("{{\"chat\":\"{id}\"}}");
    assert!(lines.contains(&format!("agent pid={pid} via=door term/whoami 200 {chat}")), "{lines:?}");
    assert!(lines.contains(&format!("agent pid={pid} via=door mail/whoami 200 {chat}")), "{lines:?}");
    // And with no list of the operator's at all: loopback is the whole list, in both spellings.
    drop(perch);
    let files = Files::new();
    let bare = Perch::builder()
        .hub()
        .args(files.flags())
        .envs(files.fake_env())
        .env("http_proxy", dead)
        .env("ALL_PROXY", dead)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .env("FAKE_AGENT_CALL", "term/whoami:{}")
        .start();
    let (id, pid, _) = live(&bare);
    let door = environ_of(pid);
    assert_eq!(door.get("NO_PROXY").map(String::as_str), Some("127.0.0.1,localhost,::1"));
    assert_eq!(door.get("no_proxy").map(String::as_str), Some("127.0.0.1,localhost,::1"));
    say(&bare, &id);
    assert!(agent_lines_after(&bare, pid, 1).contains(&format!("agent pid={pid} via=door term/whoami 200 {{\"chat\":\"{id}\"}}")));
}

#[cfg(feature = "test-bins")]
#[test]
fn a_curl_that_the_door_runs_is_a_child_and_is_refused() {
    let files = Files::new();
    let perch = Perch::builder().hub().args(files.flags()).envs(files.fake_env()).env("FAKE_AGENT_VIA", "child").env("FAKE_AGENT_CALL", "term/whoami:{}\nmail/whoami:{}").start();
    let (id, pid, _) = live(&perch);
    say(&perch, &id);
    let lines = agent_lines_after(&perch, pid, 2);
    for (scope, words) in [("term", "term tools work in chats this Minerva opened"), ("mail", "mail tools work in chats this Minerva opened")] {
        let want = format!("agent pid={pid} via=child {scope}/whoami 403 {{\"error\":\"{words}\"}}");
        assert!(lines.contains(&want), "the door's child is not the door ({scope}): {lines:?}");
    }
    // The token was read by curl from a 0600 header file, never from argv: nothing of it is on the perch's side either.
    assert_no_secret(&perch, &[&files.term_token(), &files.mail_token()]);
    assert_no_secret_in_doors(&[pid], &[&files.term_token(), &files.mail_token(), perch.token.trim()]);
}

#[cfg(feature = "test-bins")]
#[test]
fn a_door_without_a_token_file_or_with_the_wrong_scope_is_the_gates_401() {
    let files = Files::new();
    // The fake is pointed at the term token for its mail call: a scope's token is for that scope only, from a door too.
    let mut env = files.fake_env();
    for (name, value) in &mut env {
        if name == "FAKE_AGENT_MAIL_TOKEN_FILE" {
            *value = files.term.display().to_string();
        }
    }
    let perch = Perch::builder().hub().args(files.flags()).envs(env).env("FAKE_AGENT_CALL", "mail/whoami:{}\nterm/whoami:{}").start();
    let (id, pid, _) = live(&perch);
    say(&perch, &id);
    let lines = agent_lines_after(&perch, pid, 2);
    assert!(lines.contains(&format!("agent pid={pid} via=door mail/whoami 401 ")), "the term token on the mail prefix: {lines:?}");
    assert!(lines.contains(&format!("agent pid={pid} via=door term/whoami 200 {{\"chat\":\"{id}\"}}")), "{lines:?}");
}

// ---------------------------------------------------------------------------
// The attribution, against real sockets and real processes
// ---------------------------------------------------------------------------

mod attribution {
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::os::unix::io::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;
    use std::time::Instant;

    use perch::agent::attrib::{chat_of, chat_of_hinted, socket_inode, socket_inode_by_diag, socket_inode_by_proc, Hints};
    use perch::sessions::{Exit, Registry};
    use tokio::sync::watch;

    /// A registry with no doors: rows are added with [`door`], by the pid the test says is a door's.
    fn registry(dir: &Path) -> Arc<Registry> {
        Registry::new(dir.join("sessions"), dir.join("run"), PathBuf::from("eidolon"), vec![dir.to_path_buf()]).unwrap()
    }

    /// A live door row: reserved, attached to `pid`, ready and `live`. The sender is the door's exit: send on it and the door has ended.
    fn door(registry: &Registry, id: &str, pid: u32) -> watch::Sender<Option<Exit>> {
        let (exit, watching) = watch::channel(None);
        let log = PathBuf::from(format!("/tmp/agent-test-{id}.eid"));
        assert!(registry.reserve(id, &log));
        registry.attach(id, pid, watching);
        registry.ready(id, 1, Arc::from("not-a-real-token"));
        registry.mark_live(id, &log, Path::new("/tmp"));
        exit
    }

    /// One connection on loopback: the client's end, the server's end, the client's address (what the perch's `Peer` would be) and the
    /// listener's (what it is bound to).
    fn connection() -> (TcpStream, TcpStream, SocketAddr, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let bound = listener.local_addr().unwrap();
        let client = TcpStream::connect(bound).unwrap();
        let (server, peer) = listener.accept().unwrap();
        assert_eq!(client.local_addr().unwrap(), peer);
        (client, server, peer, bound)
    }

    /// A process that sleeps and keeps `fds` open besides its own: a door's descriptors are many, and the lookup has to read them all.
    /// If `socket` is given, the sleeper holds a copy of it at fd 9 (what a door's own socket is).
    struct Sleeper(Child);

    impl Sleeper {
        fn spawn(socket: Option<i32>, extra: i32) -> Self {
            let mut sleep = Command::new("sleep");
            sleep.arg("120").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            // Between fork and exec: dup2 and fcntl only, which are async-signal-safe. The copies are not close-on-exec. `dup2(9, 9)` is
            // a no-op that leaves the flag set, and the test process's own descriptor for the socket is 9 now and then (other tests are
            // opening and closing theirs on other threads): the flag is cleared by hand, or the sleeper would not hold the socket at all.
            unsafe {
                sleep.pre_exec(move || {
                    if let Some(socket) = socket
                        && (libc::dup2(socket, 9) < 0 || libc::fcntl(9, libc::F_SETFD, 0) < 0)
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    for fd in 10..10 + extra {
                        if libc::dup2(0, fd) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
            Self(sleep.spawn().expect("spawning sleep"))
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn the_door_that_holds_the_socket_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path());
        let _exit = door(&registry, "chat-a", std::process::id());
        let (_client, _server, peer, bound) = connection();
        assert!(socket_inode(peer, bound).is_some(), "the client's row is in /proc/net/tcp");
        assert_eq!(chat_of(&registry, peer, bound).as_deref(), Some("chat-a"));
        // The server's end of the same connection is the perch's own socket: the same process here, but the lookup is for the client's.
        assert_ne!(socket_inode(peer, bound), socket_inode(bound, peer));
    }

    /// The kernel's exact lookup and the proc table are two ways to one answer, for v4 and for v6, and for the perch's own end too.
    #[test]
    fn sock_diag_and_the_proc_table_name_the_same_socket() {
        let (_client, _server, peer, bound) = connection();
        let by_diag = socket_inode_by_diag(peer, bound);
        let by_proc = socket_inode_by_proc(peer, bound);
        assert!(by_diag.is_some(), "sock_diag answers for an established connection on this kernel");
        assert_eq!(by_diag, by_proc);
        assert_eq!(socket_inode_by_diag(bound, peer), socket_inode_by_proc(bound, peer), "the server's end, asked the other way round");
        assert_ne!(by_diag, socket_inode_by_diag(bound, peer), "…and it is another socket");
        // v6 loopback, when the box has it.
        if let Ok(listener) = TcpListener::bind("[::1]:0") {
            let bound6 = listener.local_addr().unwrap();
            let client6 = TcpStream::connect(bound6).unwrap();
            let (_server6, peer6) = listener.accept().unwrap();
            assert_eq!(client6.local_addr().unwrap(), peer6);
            assert!(socket_inode_by_diag(peer6, bound6).is_some());
            assert_eq!(socket_inode_by_diag(peer6, bound6), socket_inode_by_proc(peer6, bound6));
        }
        // A tuple that is no connection: no answer from either, not a listener's inode.
        let listener_port = bound;
        assert_eq!(socket_inode_by_diag(listener_port, "127.0.0.1:1".parse().unwrap()), None);
        assert_eq!(socket_inode_by_proc(listener_port, "127.0.0.1:1".parse().unwrap()), None);
        assert_eq!(socket_inode_by_diag("127.0.0.1:1".parse().unwrap(), bound), None);
    }

    #[test]
    fn a_socket_no_door_holds_names_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path());
        let (_client, _server, peer, bound) = connection();
        assert_eq!(chat_of(&registry, peer, bound), None, "no door at all");
        // Doors that exist and do not hold it: this test process (which does) is not one of them.
        let sleepers: Vec<Sleeper> = (0..3).map(|_| Sleeper::spawn(None, 8)).collect();
        let _exits: Vec<_> = sleepers.iter().enumerate().map(|(n, sleeper)| door(&registry, &format!("chat-{n}"), sleeper.pid())).collect();
        assert_eq!(chat_of(&registry, peer, bound), None, "the holder is a process that is not a door: a curl, a TUI, a workflow run");
        // A reservation whose fork never happened has pid 0 and is nobody's.
        assert!(registry.reserve("reserved", Path::new("/tmp/agent-test-reserved.eid")));
        assert_eq!(chat_of(&registry, peer, bound), None);
    }

    #[test]
    fn an_address_that_is_not_a_connection_names_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path());
        let _exit = door(&registry, "chat-a", std::process::id());
        let (_client, _server, _peer, bound) = connection();
        let nowhere: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert_eq!(chat_of(&registry, nowhere, bound), None);
        let other: SocketAddr = "127.0.0.1:2".parse().unwrap();
        assert_eq!(chat_of(&registry, _peer, other), None, "the right client, the wrong server: not this perch's connection");
    }

    /// No walk up through parents, and none down: the process that holds the socket must itself be the door.
    #[test]
    fn a_child_of_a_door_is_not_a_door() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path());
        // This process is the door. A child of it (a bash, a curl) ends up holding the connection and the door lets go of it.
        let _exit = door(&registry, "chat-a", std::process::id());
        let (client, _server, peer, bound) = connection();
        assert_eq!(chat_of(&registry, peer, bound).as_deref(), Some("chat-a"), "while the door holds it");
        let child = Sleeper::spawn(Some(client.as_raw_fd()), 0);
        assert_eq!(chat_of(&registry, peer, bound).as_deref(), Some("chat-a"), "a copy in the child changes nothing while the door holds it too");
        drop(client);
        assert_eq!(chat_of(&registry, peer, bound), None, "the child holds it and the child is not a door");
        // And the other way round: the child is the door, the connection is the parent's.
        let registry = self::registry(dir.path());
        let _exit = door(&registry, "chat-child", child.pid());
        assert_eq!(chat_of(&registry, peer, bound).as_deref(), Some("chat-child"), "the process that holds it is the door");
    }

    #[test]
    fn a_door_that_has_ended_is_not_a_door() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path());
        let exit = door(&registry, "chat-a", std::process::id());
        let (_client, _server, peer, bound) = connection();
        assert_eq!(chat_of(&registry, peer, bound).as_deref(), Some("chat-a"));
        exit.send(Some(Exit { code: Some(0), signal: None })).unwrap();
        assert_eq!(chat_of(&registry, peer, bound), None, "its exit is published: a pid the kernel may hand to anyone");
    }

    /// Sixteen doors, each holding its own connection (at fd 9) and sixteen other descriptors, as `(chat, client address, server
    /// address)`. The sleepers, servers and exits are returned to be kept alive.
    #[allow(clippy::type_complexity)]
    fn sixteen_doors(registry: &Registry) -> (Vec<(String, SocketAddr, SocketAddr)>, Vec<(Sleeper, TcpStream, watch::Sender<Option<Exit>>)>) {
        let mut kept = Vec::new();
        let mut want = Vec::new();
        for n in 0..16 {
            let (client, server, peer, bound) = connection();
            let sleeper = Sleeper::spawn(Some(client.as_raw_fd()), 16);
            drop(client);
            let exit = door(registry, &format!("chat-{n}"), sleeper.pid());
            want.push((format!("chat-{n}"), peer, bound));
            kept.push((sleeper, server, exit));
        }
        (want, kept)
    }

    /// 1000 lookups across 16 doors, as the gate makes them (with its hints, the calls of one connection after another): every one finds
    /// the right door, and the lot takes well under a second.
    #[test]
    fn a_thousand_lookups_across_sixteen_doors_take_under_a_second() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path());
        let (want, kept) = sixteen_doors(&registry);
        let hints = Hints::default();
        // Once, so the first lookup is not the one that pays for a cold cache.
        assert_eq!(chat_of_hinted(&registry, &hints, want[0].1, want[0].2).as_deref(), Some("chat-0"));
        let started = Instant::now();
        let mut found = 0;
        for round in 0..63 {
            for (id, peer, bound) in &want {
                assert_eq!(chat_of_hinted(&registry, &hints, *peer, *bound).as_deref(), Some(id.as_str()), "round {round}");
                found += 1;
            }
        }
        let took = started.elapsed();
        assert_eq!(found, 1008);
        assert!(took.as_millis() < 1000, "{found} lookups took {took:?}");
        drop(kept);
    }

    /// The worst case: every call a new connection, so no hint ever applies and each lookup reads every door's descriptors. About 0.7 s
    /// for 1008 of them in a debug build on the box this was written on, and the bound is a multiple of that: it is here to catch a lookup
    /// that costs the table and not the doors (the first version, which read `/proc/net/tcp` for each call, took 18 s).
    #[test]
    fn a_thousand_lookups_that_each_walk_the_doors_are_still_seconds_not_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path());
        let (want, kept) = sixteen_doors(&registry);
        let started = Instant::now();
        let mut found = 0;
        for (id, peer, bound) in want.iter().cycle().take(1008) {
            assert_eq!(chat_of(&registry, *peer, *bound).as_deref(), Some(id.as_str()));
            found += 1;
        }
        let took = started.elapsed();
        assert!(took.as_millis() < 5000, "{found} walking lookups took {took:?}");
        drop(kept);
    }

    /// A hint is asked and not believed. The door held the socket, the hint says so, and then the socket is in a child that is not a door:
    /// the hint must not name the door.
    #[test]
    fn a_hint_that_has_gone_stale_names_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path());
        let hints = Hints::default();
        let _exit = door(&registry, "chat-a", std::process::id());
        let (client, _server, peer, bound) = connection();
        assert_eq!(chat_of_hinted(&registry, &hints, peer, bound).as_deref(), Some("chat-a"), "found, and remembered");
        assert_eq!(chat_of_hinted(&registry, &hints, peer, bound).as_deref(), Some("chat-a"), "found by the hint");
        let _child = Sleeper::spawn(Some(client.as_raw_fd()), 0);
        drop(client);
        assert_eq!(chat_of_hinted(&registry, &hints, peer, bound), None, "the descriptor is gone and the holder is not a door");
    }

    /// Descriptor numbers are reused. A hint for one connection is keyed by that connection's inode, and whatever opens in the same
    /// descriptor afterwards is another inode: it is looked up for itself, and the old connection stays nobody's.
    #[test]
    fn a_connection_that_has_closed_does_not_lend_its_hint_to_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path());
        let hints = Hints::default();
        let _exit = door(&registry, "chat-a", std::process::id());
        let (client, _server, peer, bound) = connection();
        assert_eq!(chat_of_hinted(&registry, &hints, peer, bound).as_deref(), Some("chat-a"));
        drop(client);
        assert_eq!(chat_of_hinted(&registry, &hints, peer, bound), None, "the client end is closed: no socket with an inode is left");
        let (_second, _server2, peer2, bound2) = connection();
        assert_ne!(peer, peer2);
        assert_eq!(chat_of_hinted(&registry, &hints, peer2, bound2).as_deref(), Some("chat-a"), "the new connection, by its own inode");
        assert_eq!(chat_of_hinted(&registry, &hints, peer, bound), None, "and the first is still nobody's");
    }

    /// The registry has the last word: a hint to a door that has ended is nothing.
    #[test]
    fn a_hint_to_a_door_that_has_ended_names_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path());
        let hints = Hints::default();
        let exit = door(&registry, "chat-a", std::process::id());
        let (_client, _server, peer, bound) = connection();
        assert_eq!(chat_of_hinted(&registry, &hints, peer, bound).as_deref(), Some("chat-a"));
        exit.send(Some(Exit { code: Some(0), signal: None })).unwrap();
        assert_eq!(chat_of_hinted(&registry, &hints, peer, bound), None);
    }
}
