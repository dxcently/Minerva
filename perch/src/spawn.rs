//! Spawning a door: the argv, the fork, the two boot lines, and the door token file.
//!
//! hub.md §4's spawn sequence, steps 3-6, is the spec:
//!
//! ```text
//!   3  eidolon web --bind 127.0.0.1:0 --cwd <dir> | --session <path> [-m <model>]
//!   4  env inherited, stdin /dev/null, stdout piped, stderr to the perch's log
//!   5  read stdout until both boot lines, 20 s, else kill and `dead`
//!   6  the token path must be <runtime>/eidolon/web-<port>.token, opened O_NOFOLLOW,
//!      owner-checked, mode & 077 == 0, read, and held in memory
//! ```
//!
//! **No token ever enters argv or env** (§10.4 row 1): the door mints its own and prints
//! only the *path*, which the perch then refuses to trust without every check below. That is
//! also why there is nothing here to hand a door — [`argv`] has no token parameter at all.
//!
//! **The fork happens on one long-lived thread.** §2's PDEATHSIG hypothesis: the child's
//! `PR_SET_PDEATHSIG` fires when the *thread that forked* exits, and a tokio blocking-pool
//! thread exits when it goes idle — so a door forked from the pool would be SIGTERMed for
//! no reason. [`Spawner`] keeps one thread whose only job is to fork, for the life of the
//! perch. `tokio::process::Command::spawn` is synchronous, so the fork really does happen on
//! the thread that calls it.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStdout;
use tokio::sync::{oneshot, watch};

use crate::sessions::{self, Exit, ExitWatch, Registry};

/// §4 step 5's startup timeout. The door prints both lines before it serves anything, so a
/// door that has not printed them in 20 s is not coming up.
pub const BOOT_TIMEOUT: Duration = Duration::from_secs(20);
/// §4 step 7: `hello` is the first frame of the watcher's stream and never waits on the
/// network, so the same 20 s is generous for it too.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a door that never came up gets to honour SIGTERM. Short on purpose: nothing
/// holds this child open — the perch never opened a watcher to it, or closed the one it did
/// before asking it to stop.
pub const GIVE_UP_GRACE: Duration = Duration::from_secs(2);

/// `--cwd <dir>` for a new session, `--session <log>` for a kept one (§4 step 3). The two
/// are mutually exclusive by type, because a door given both would write one log twice.
pub enum Target<'a> {
    New(&'a Path),
    Kept(&'a Path),
}

/// The argv, and nothing but the argv. `--ui-dir` is absent on purpose (the door serves no
/// statics under the perch) and no element is ever a token.
pub fn argv(eidolon: &Path, target: Target<'_>, model: Option<&str>) -> Vec<OsString> {
    let mut argv = vec![eidolon.as_os_str().to_os_string(), "web".into(), "--bind".into(), "127.0.0.1:0".into()];
    match target {
        Target::New(cwd) => {
            argv.push("--cwd".into());
            argv.push(cwd.as_os_str().to_os_string());
        }
        Target::Kept(log) => {
            argv.push("--session".into());
            argv.push(log.as_os_str().to_os_string());
        }
    }
    if let Some(model) = model {
        argv.push("-m".into());
        argv.push(model.into());
    }
    argv
}

/// Where a door's token file must be: `<runtime dir>/eidolon/web-<port>.token` (§4 step 6).
/// The port comes from the door's own first line, so this is the path the perch expects for
/// *this* door and no other — scanning the directory for a new `web-*.token` would race
/// every other door starting.
pub fn door_token_path(runtime_dir: &Path, port: u16) -> PathBuf {
    runtime_dir.join("eidolon").join(format!("web-{port}.token"))
}

/// Read a door's token, refusing anything that is not an owner-only regular file reached
/// without following a link.
///
/// The refusals are the point (§10.4): a path a child printed is not a path this perch will
/// follow anywhere, and a token file another user could read was never a secret.
pub fn read_token_file(path: &Path) -> io::Result<String> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::os::unix::io::FromRawFd as _;

    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("{} contains a NUL", path.display())))?;
    // O_NOFOLLOW: a symlink at the token path is a refusal, not something to follow.
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // Owned from here, so every early return closes it.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a regular file", path.display())));
    }
    let ours = unsafe { libc::getuid() };
    if meta.uid() != ours {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is owned by uid {}, not ours ({ours})", path.display(), meta.uid())));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} came out {mode:o}, not owner-only", path.display())));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    // A trailing newline in a token file is a plausible upstream change and not a different
    // token; nothing else about the bytes is forgiven.
    Ok(text.trim().to_string())
}

/// The one thread that will ever fork a door for this perch, and the channel to it.
pub struct Spawner {
    jobs: std::sync::mpsc::Sender<Job>,
}

struct Job {
    argv: Vec<OsString>,
    reply: oneshot::Sender<io::Result<tokio::process::Child>>,
}

impl Spawner {
    /// Started once, from inside the runtime: the thread enters the runtime's context so
    /// that the child it forks is registered with the runtime's driver, and blocks on the
    /// job channel until the process exits.
    pub fn new() -> Result<Self> {
        let handle = tokio::runtime::Handle::current();
        let (jobs, work) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("perch-spawn".into())
            .spawn(move || {
                // The guard lives as long as this thread, which is what makes every fork
                // from here happen on a thread that outlives the perch.
                let _runtime = handle.enter();
                while let Ok(job) = work.recv() {
                    let _ = job.reply.send(fork(&job.argv));
                }
            })
            .context("starting the thread that forks doors")?;
        Ok(Self { jobs })
    }

    async fn spawn(&self, argv: Vec<OsString>) -> io::Result<tokio::process::Child> {
        let (reply, answer) = oneshot::channel();
        self.jobs
            .send(Job { argv, reply })
            .map_err(|_| io::Error::other("the thread that forks doors is gone"))?;
        answer.await.map_err(|_| io::Error::other("the thread that forks doors did not answer"))?
    }
}

/// Runs on the spawn thread: this is the call that forks.
fn fork(argv: &[OsString]) -> io::Result<tokio::process::Child> {
    let (program, args) = argv.split_first().ok_or_else(|| io::Error::other("no door binary in the argv"))?;
    let mut command = tokio::process::Command::new(program);
    command.args(args);
    // §4 step 4. stderr is inherited: a door's complaints belong in the perch's own log,
    // and nothing on that path is a secret either.
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::inherit());
    // §2's hypothesis, made true by where this function runs: this thread is the perch's
    // spawn thread and outlives every door, so PDEATHSIG fires when the *perch* dies.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}

/// A forked door: its pid, the channel its reaper publishes the exit on, and its stdout,
/// where the two boot lines are read from.
pub struct Forked {
    pub pid: u32,
    pub exit: ExitWatch,
    stdout: BufReader<ChildStdout>,
}

/// Fork the door and start the task that will reap it.
///
/// Nothing waits on that task: the exit reaches everyone through [`Forked::exit`], which is
/// also what `stop` waits on with a deadline.
pub async fn fork_door(spawner: &Spawner, registry: &Arc<Registry>, argv: Vec<OsString>) -> Result<Forked> {
    let mut child = spawner.spawn(argv).await.context("spawning the door")?;
    let pid = child.id().ok_or_else(|| anyhow::anyhow!("the door exited before it had a pid"))?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("the door's stdout was not a pipe"))?;
    let (publish, exit) = watch::channel(None);
    let registry = registry.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        // Published first, so a waiter is never held up by this task; then the cleanup
        // (`mark_exited`) that makes the row `dead` and removes a token file the door could
        // not remove itself.
        let _ = publish.send(Some(Exit::of(status)));
        registry.mark_exited(pid);
    });
    Ok(Forked { pid, exit, stdout: BufReader::new(stdout) })
}

/// What §4 step 6 ends with: the port the door bound and the token it minted, in memory and
/// nowhere else.
pub struct Booted {
    pub port: u16,
    pub token: Arc<str>,
}

/// §4 steps 5 and 6, and the only way they can fail: stop the child and leave nothing of it
/// behind. A door that never printed its lines is killed after [`GIVE_UP_GRACE`]; a door
/// whose token file was refused has that file removed once it is gone, because a refused
/// token file is still a door's secret file and the door does not always get to remove it.
pub async fn boot(forked: &mut Forked, runtime_dir: &Path) -> Result<Booted> {
    let lines = match tokio::time::timeout(BOOT_TIMEOUT, boot_lines(&mut forked.stdout)).await {
        Ok(Ok(lines)) => lines,
        Ok(Err(e)) => {
            give_up(forked, None).await;
            return Err(e);
        }
        Err(_) => {
            give_up(forked, None).await;
            return Err(anyhow::anyhow!("the door printed no boot lines within {BOOT_TIMEOUT:?}"));
        }
    };
    let (addr, printed) = lines;

    // The printed path must be the one this port's token belongs in. Anything else is a path
    // the perch will not open — and not a path it will touch either, so nothing is removed in
    // this branch: a path a child printed is not a file the perch has any business deleting.
    let expected = door_token_path(runtime_dir, addr.port());
    if printed != expected {
        give_up(forked, None).await;
        anyhow::bail!("the door named a token file outside {}: {}", runtime_dir.join("eidolon").display(), printed.display());
    }

    match read_token_file(&expected) {
        Ok(token) => Ok(Booted { port: addr.port(), token: Arc::from(token) }),
        Err(e) => {
            give_up(forked, Some(expected.clone())).await;
            Err(anyhow::Error::new(e).context(format!("reading the door token {}", expected.display())))
        }
    }
}

/// Stop a door that will not be adopted, the way a stop stops one — SIGTERM, a short wait,
/// SIGKILL — and remove the token file at the *validated* path if there is one, since a
/// SIGKILLed child never removes its own.
async fn give_up(forked: &Forked, token_file: Option<PathBuf>) {
    sessions::signal_and_wait(forked.pid, forked.exit.clone(), GIVE_UP_GRACE).await;
    if let Some(path) = token_file {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::debug!(error = %e, path = %path.display(), "perch: a refused door token file was not removed");
        }
    }
}

/// Both boot lines, or an error that says which one was missing or wrong (§4 step 5).
async fn boot_lines(stdout: &mut BufReader<ChildStdout>) -> Result<(std::net::SocketAddr, PathBuf)> {
    let first = next_line(stdout).await.context("the door's first boot line")?;
    let second = next_line(stdout).await.context("the door's second boot line")?;
    parse_boot(&first, &second)
}

async fn next_line(stdout: &mut BufReader<ChildStdout>) -> Result<String> {
    let mut line = String::new();
    let read = stdout.read_line(&mut line).await.context("reading the door's stdout")?;
    anyhow::ensure!(read > 0, "the door's stdout ended before both boot lines");
    Ok(line.trim_end().to_string())
}

/// The door's boot contract, exactly: `listening on http://<addr>/` then `token file: <path>`
/// (crates/web/src/lib.rs:136-137). It is human text, which is why hub.md W3 wants a
/// machine-readable line; until then a reworded line is a door the perch refuses to adopt.
fn parse_boot(first: &str, second: &str) -> Result<(std::net::SocketAddr, PathBuf)> {
    let addr = first
        .strip_prefix("listening on http://")
        .and_then(|rest| rest.strip_suffix('/'))
        .ok_or_else(|| anyhow::anyhow!("the door's first line is not `listening on http://<addr>/`: {first:?}"))?;
    let addr: std::net::SocketAddr = addr.parse().with_context(|| format!("the door's address {addr:?}"))?;
    // The watcher only ever connects to a loopback address (hub.md §1); a door that says it
    // bound anything else is not one this perch will talk to.
    anyhow::ensure!(addr.ip().is_loopback(), "the door bound {addr}, which is not loopback");
    let printed = second
        .strip_prefix("token file: ")
        .ok_or_else(|| anyhow::anyhow!("the door's second line is not `token file: <path>`: {second:?}"))?;
    Ok((addr, PathBuf::from(printed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_argv_is_the_doors_and_carries_no_token_and_no_ui_dir() {
        let eidolon = Path::new("/usr/bin/eidolon");
        let fresh = argv(eidolon, Target::New(Path::new("/home/me/work")), None);
        assert_eq!(words(&fresh), ["/usr/bin/eidolon", "web", "--bind", "127.0.0.1:0", "--cwd", "/home/me/work"]);
        let resumed = argv(eidolon, Target::Kept(Path::new("/logs/ctf-pwn-3.eid")), Some("sonnet"));
        assert_eq!(
            words(&resumed),
            ["/usr/bin/eidolon", "web", "--bind", "127.0.0.1:0", "--session", "/logs/ctf-pwn-3.eid", "-m", "sonnet"]
        );
        // §10.4 row 1: there is no token to pass, and this is where that would show. The
        // door mints its own; `--ui-dir` is the perch's statics, not the door's.
        for arg in fresh.iter().chain(resumed.iter()) {
            let arg = arg.to_string_lossy();
            assert!(!arg.contains("token"), "{arg} names a token");
            assert!(!arg.contains("Authorization"), "{arg} names the header");
            assert_ne!(arg, "--ui-dir");
        }
    }

    fn words(argv: &[OsString]) -> Vec<String> {
        argv.iter().map(|arg| arg.to_string_lossy().to_string()).collect()
    }

    #[test]
    fn the_boot_lines_are_the_doors_two_and_a_loopback_one() {
        let (addr, path) = parse_boot("listening on http://127.0.0.1:4477/", "token file: /run/user/1000/eidolon/web-4477.token").unwrap();
        assert_eq!(addr, "127.0.0.1:4477".parse().unwrap());
        assert_eq!(path, Path::new("/run/user/1000/eidolon/web-4477.token"));
        assert_eq!(door_token_path(Path::new("/run/user/1000"), addr.port()), path, "the path is the one this port's token belongs in");

        // A reworded line, a line that is not an address, and a door that is not loopback:
        // all refused rather than guessed at (§4 step 5, hub.md W3).
        for (first, second) in [
            ("listening at http://127.0.0.1:4477/", "token file: /x/web-4477.token"),
            ("listening on http://nonsense/", "token file: /x/web-1.token"),
            ("listening on http://0.0.0.0:4477/", "token file: /x/web-4477.token"),
            ("listening on http://127.0.0.1:4477/", "token: /x/web-4477.token"),
        ] {
            assert!(parse_boot(first, second).is_err(), "{first:?} {second:?} was accepted");
        }
    }

    #[test]
    fn a_door_token_file_is_read_only_when_it_is_ours_and_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-1.token");
        std::fs::write(&path, "s3cret\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token_file(&path).unwrap(), "s3cret", "a trailing newline is not a different token");

        // Loose mode, directory, and a symlink: each refused, and the refusal says what it
        // found so a log line can be acted on.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = read_token_file(&path).unwrap_err();
        assert!(err.to_string().contains("644"), "the refusal names the mode it found: {err}");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let sub = dir.path().join("a-directory");
        std::fs::create_dir(&sub).unwrap();
        assert!(read_token_file(&sub).is_err(), "a directory is not a token file");
        let link = dir.path().join("a-link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_token_file(&link).is_err(), "O_NOFOLLOW: a link at the token path is refused");
        assert!(read_token_file(&dir.path().join("nowhere")).unwrap_err().kind() == io::ErrorKind::NotFound, "a missing file is NotFound, which a cleanup can act on");
    }
}
