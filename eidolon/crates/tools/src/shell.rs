//! Shell primitive: run a command under `bash -c` in its own session —
//! no controlling terminal, so nothing it does can reach the screen the
//! harness is drawing — with a timeout and a cancellation token, and kill
//! the whole group when either fires. [`run_background`] is the stateless
//! twin: start a command detached, return its pid, log path and status
//! path at once — no id, no table, no `stop` — for anything meant to
//! outlive the call. The status path is where the exit is written down:
//! the reaper thread is the only thing that ever sees the `ExitStatus`,
//! and it does not outlive the process that started it.
//!
//! The process-group discipline is the same lesson Melete's `proc::Exec`
//! encodes: a dropped future must not leave a child tree running. Output is
//! captured to a cap so a runaway `yes` cannot fill memory.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// Cap on captured stdout+stderr bytes.
pub const MAX_OUTPUT: usize = 512 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exec {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    /// The turn was cancelled and the command was killed for it. Recorded at
    /// the kill site rather than inferred afterwards from `code == None`, so
    /// the transcript can say what happened instead of leaving the reader to
    /// deduce it from a platform convention about signalled children.
    pub cancelled: bool,
    pub truncated: bool,
    /// The command backgrounded something that is still running and still
    /// holds this command's output stream.
    pub background: bool,
}

impl Exec {
    /// Render for the model: exit status line, then output.
    pub fn render(&self) -> String {
        let mut s = String::new();
        match (self.code, self.timed_out, self.cancelled) {
            (_, true, _) => s.push_str("[timed out; process group killed]\n"),
            (_, _, true) => s.push_str("[cancelled; process killed]\n"),
            (Some(0), _, _) => {}
            (Some(c), _, _) => s.push_str(&format!("[exit code {c}]\n")),
            (None, _, _) => s.push_str("[killed by signal]\n"),
        }
        s.push_str(&self.stdout);
        if !self.stderr.is_empty() {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str("--- stderr ---\n");
            s.push_str(&self.stderr);
        }
        if self.truncated {
            s.push_str("\n[output truncated]");
        }
        if self.background {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str("[a background process from this command is still running]");
        }
        if s.is_empty() {
            s.push_str("[no output]");
        }
        s
    }

    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out && !self.cancelled
    }
}

pub async fn run(
    cwd: &Path,
    command: &str,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
) -> anyhow::Result<Exec> {
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c").arg(command);
    spawn_and_wait(cmd, cwd, "bash", timeout, cancel).await
}

/// One program and its argv, **no shell between them**.
///
/// This is the shape a tool whose arguments come from a model wants: the
/// program is the file's to name and the arguments are values rather than
/// text, so there is no quoting rule to get wrong, no metacharacter to escape
/// and nothing a `;`, a `$(…)` or a pipe in an argument can do — the child sees
/// each argument as one word because nothing ever parsed a command line. The
/// price is the shell's own vocabulary: no pipes, no redirection, no globbing.
/// [`run`] is for the cases that need those.
///
/// Same process-group, timeout, cancellation and output semantics as [`run`];
/// only the spawn differs.
pub async fn exec(
    cwd: &Path,
    program: &str,
    args: &[String],
    timeout: Option<Duration>,
    cancel: &CancellationToken,
) -> anyhow::Result<Exec> {
    let program = program.trim();
    if program.is_empty() {
        bail!("no program to run");
    }
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    spawn_and_wait(cmd, cwd, program, timeout, cancel).await
}

/// Spawn `cmd` and see it through: own session, no tty, stdin null, output
/// captured and capped, killed as a group on timeout or cancellation, with
/// whatever was written before either still reported. The spawner's choice —
/// `bash -c`, or one program's argv — is the caller's; everything after it is
/// the same for both.
async fn spawn_and_wait(
    mut cmd: tokio::process::Command,
    cwd: &Path,
    what: &str,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
) -> anyhow::Result<Exec> {
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    no_tty(&mut cmd);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {what} in {}", cwd.display()))?;
    let pid = child.id();
    let out = child.stdout.take().expect("piped");
    let err = child.stderr.take().expect("piped");

    // Readers own shared buffers rather than returning them, so whatever was
    // captured survives a timeout. Discarding it there was its own bug: a
    // command that printed for a minute and then hung reported nothing at
    // all, which is the least useful thing to tell a model.
    let ob = Sink::default().shared();
    let eb = Sink::default().shared();
    let mut ot = tokio::spawn(read_capped(out, ob.clone()));
    let mut et = tokio::spawn(read_capped(err, eb.clone()));

    let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);
    let mut timed_out = false;
    let mut cancelled = false;
    // Wait on the *process*, not on end-of-pipe. A command that backgrounds
    // something (`server & sleep 1 && curl …`) leaves a grandchild holding
    // the inherited stdout, so the pipe never closes even though bash has
    // exited — waiting for EOF turned a one-second command into a full
    // timeout.
    let status = tokio::select! {
        s = child.wait() => Some(s?),
        _ = tokio::time::sleep(timeout) => {
            timed_out = true;
            kill_group(pid);
            None
        }
        _ = cancel.cancelled() => {
            cancelled = true;
            kill_group(pid);
            None
        }
    };

    // The child is gone; give the readers a moment to drain what is already
    // buffered. If they do not finish, something the command backgrounded is
    // still holding the write end.
    let drain = tokio::time::timeout(DRAIN, async {
        let _ = (&mut ot).await;
        let _ = (&mut et).await;
    })
    .await;
    let background = drain.is_err();
    if background {
        // Detach rather than abort. Aborting drops our end of the pipe, and
        // the next thing the background process writes kills it with SIGPIPE
        // — a server started to be looked at in a browser would survive the
        // call and then die on its first request. The readers keep draining
        // into the (already-taken, capped) sink until the pipe closes on its
        // own, which costs a bounded buffer and keeps the process usable.
        drop(ot);
        drop(et);
    }

    let status = match status {
        Some(s) => s,
        None => tokio::select! {
            s = child.wait() => s?,
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                kill_group(pid);
                child.wait().await?
            }
        },
    };

    let (ob, ot) = ob.take();
    let (eb, et) = eb.take();
    Ok(Exec {
        code: status.code(),
        stdout: String::from_utf8_lossy(&ob).into_owned(),
        stderr: String::from_utf8_lossy(&eb).into_owned(),
        timed_out,
        cancelled,
        truncated: ot || et,
        background,
    })
}

/// A command started with [`run_background`]: the three facts anyone needs
/// to find it again — the pid, the file its output streams to, and the file
/// its exit lands in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Background {
    /// Sync spawn: the pid is known and `kill(pid, 0)` answers while the
    /// command lives — which is only until it is reaped, after which the
    /// number may belong to a stranger. [`Background::exit_status`] is the
    /// durable answer; this is the live one.
    pub pid: u32,
    pub log: PathBuf,
    /// Where the exit status lands ([`Background::exit_status`]). A file
    /// beside the log rather than an entry in a table this process holds:
    /// the tool keeps its "no id, no table, no stop" shape, and the answer
    /// stays readable to a process that did not start the command.
    pub exit: PathBuf,
}

impl Background {
    /// Render for the model: the facts, and what to do with them.
    pub fn render(&self) -> String {
        let mut s = String::from("[running in the background]\n");
        s.push_str(&format!("pid: {}\n", self.pid));
        s.push_str(&format!("log: {}\n", self.log.display()));
        s.push_str(&format!(
            "status: {} (written when the command finishes)\n",
            self.exit.display()
        ));
        s.push_str(
            "output streams to the log from the first byte; read or tail it to follow the command",
        );
        s
    }

    /// The status the command finished with, once the reaper has written it.
    ///
    /// `None` means "still running, or nothing recorded it" — the second
    /// being a harness that exited before its command did, which never
    /// reaped it and so never learned its fate. That is *unknown*, not
    /// zero, and the two are told apart by the log's company: a spent pid
    /// with no status file is a command nobody saw end.
    pub fn exit_status(&self) -> Option<BackgroundExit> {
        serde_json::from_slice(&std::fs::read(&self.exit).ok()?).ok()
    }
}

/// What a background command left behind when it finished: the record the
/// reaper writes beside the log.
///
/// The reaper is the only writer and the only thing in the process that
/// ever holds the `ExitStatus`, where the code and the signal are separate
/// facts — a command killed by a signal has no code at all. Recorded rather
/// than returned because every question worth asking next ("did the build
/// succeed", "was it killed") is asked after the call that started it is
/// over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundExit {
    /// The process that was spawned — the `bash -c`, not any grandchild of
    /// it. A command that backgrounds something and returns reports here
    /// while that grandchild is still running, the same shape the
    /// foreground [`Exec::background`] flag reports.
    pub pid: u32,
    /// `None` when a signal ended it.
    pub code: Option<i32>,
    /// The signal, when one did.
    pub signal: Option<i32>,
    /// Wall clock when it was reaped: "finished at", not "ran for".
    pub at_ms: u64,
}

/// Start `command` under `bash -c` in `cwd` without waiting for it: return
/// the pid, the log path and the status path at once, and let the command
/// run on — a dev server, a watcher, a long build — outliving the call,
/// the turn's cancellation and the harness process itself. Output appends
/// to the log file from the first byte; nothing is captured into memory,
/// and no cap watches the file, the way no cap watches any server log. The
/// model follows the command by reading the log it was handed, and learns
/// how it ended — code, or signal — from the status file beside it.
///
/// There is deliberately no handle to hold: no id, no table, no `stop` —
/// that tool existed and was dropped for carrying more machinery than the
/// need. What the stateless shape keeps from the old one is the
/// discipline, not the surface:
///
/// - the command leads a session of its own ([`no_tty_std`]), so nothing
///   it prints can reach the harness terminal and nothing the harness
///   does can reach it;
/// - the child is reaped by a thread of its own (the `wait` at the spawn
///   site), so a finished background command leaves no zombie behind even
///   though nothing in this process ever asks about it again — and that
///   same thread writes the exit status down, which is the only reason
///   "did it work" has an answer later;
/// - there is no cancellation path: the token that stops a foreground
///   command is deliberately not accepted here, because surviving a
///   cancelled turn is the point.
pub fn run_background(cwd: &Path, command: &str) -> anyhow::Result<Background> {
    if command.trim().is_empty() {
        anyhow::bail!("no command to run in the background");
    }
    let dir = background_log_dir();
    let id = next_id();
    let log = dir.join(format!("{id}.log"));
    let exit = dir.join(format!("{id}.exit"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("opening {}", log.display()))?;
    let err = file
        .try_clone()
        .context("cloning the background log handle")?;

    let mut cmd = std::process::Command::new("bash");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err));
    no_tty_std(&mut cmd);
    // No `kill_on_drop` here: the point is that the command outlives this
    // call. The `Child` moves to a reaper thread, which blocks in `wait`
    // for however long the command runs and collects its exit — without
    // it, a finished background command would sit in this process's table
    // as a zombie until the harness itself exited, because nothing ever
    // asks about it again.
    let child = cmd
        .spawn()
        .with_context(|| format!("spawning background bash in {}", cwd.display()))?;
    let pid = child.id();
    // The reaper needs the status path too; the caller keeps its own copy
    // to hand back in the `Background`.
    let exit_path = exit.clone();
    std::thread::spawn(move || {
        let mut child = child;
        // `wait` failing leaves the command unreaped *and* its fate
        // unknown: write nothing rather than invent a status for it.
        let Ok(status) = child.wait() else {
            tracing::warn!(pid = pid, "could not reap a background command");
            return;
        };
        let record = BackgroundExit {
            pid,
            code: status.code(),
            signal: std::os::unix::process::ExitStatusExt::signal(&status),
            at_ms: now_ms(),
        };
        if let Err(e) = write_exit(&exit_path, &record) {
            tracing::warn!(
                error = %e,
                path = %exit_path.display(),
                "could not record a background command's status"
            );
        }
    });
    Ok(Background { pid, log, exit })
}

/// Publish a status file: written under a temporary name and renamed into
/// place, so a reader sees a finished command or nothing at all and never
/// half a status. The file's existence is the answer — the same discipline
/// `meta.json` gets in the swarm, for the same reason.
fn write_exit(path: &Path, record: &BackgroundExit) -> anyhow::Result<()> {
    let tmp = path.with_extension("exit.tmp");
    std::fs::write(&tmp, serde_json::to_vec(record)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("publishing {}", path.display()))
}

/// Wall-clock milliseconds, for the one fact here that is a time rather
/// than a name.
fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// An id unique across restarts as well as within the process: the clock
/// in hex plus a per-process counter, so one start never reuses — and
/// truncates — another's log file. The old `bg` tool's scheme, unchanged.
fn next_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("bg-{:x}-{n:04x}", now_ms())
}

/// `~/.cache/eidolon/background/`, with the same fallback the http body
/// cache uses: a build sandbox has a `$HOME` that exists on paper and not
/// in the filesystem, and the logs are then as durable as a temp dir,
/// which is better than refusing to background anything at all.
fn background_log_dir() -> PathBuf {
    let preferred = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("eidolon")
        .join("background");
    if std::fs::create_dir_all(&preferred).is_ok() {
        preferred
    } else {
        let fallback = std::env::temp_dir().join("eidolon").join("background");
        std::fs::create_dir_all(&fallback).ok();
        fallback
    }
}

/// The `std::process` twin of [`no_tty`], for the one spawner that is
/// not async — the background command, which must outlive the runtime
/// that started it. The reasoning is [`no_tty`]'s to give.
fn no_tty_std(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    // Safety: as `no_tty` — `setsid` only, which is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

/// How long to keep draining a pipe after the process itself has exited.
const DRAIN: Duration = Duration::from_millis(250);

/// Captured output plus whether the cap was hit, shared with the reader task
/// so a timeout still reports what arrived.
#[derive(Default)]
struct Sink {
    buf: Vec<u8>,
    truncated: bool,
}

type Shared = std::sync::Arc<std::sync::Mutex<Sink>>;

impl Sink {
    fn shared(self) -> Shared {
        std::sync::Arc::new(std::sync::Mutex::new(self))
    }
}

trait TakeSink {
    fn take(&self) -> (Vec<u8>, bool);
}

impl TakeSink for Shared {
    fn take(&self) -> (Vec<u8>, bool) {
        let mut g = self.lock().unwrap_or_else(|e| e.into_inner());
        (std::mem::take(&mut g.buf), g.truncated)
    }
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(mut r: R, sink: Shared) {
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut g = sink.lock().unwrap_or_else(|e| e.into_inner());
                if g.buf.len() < MAX_OUTPUT {
                    let take = n.min(MAX_OUTPUT - g.buf.len());
                    g.buf.extend_from_slice(&chunk[..take]);
                    if take < n {
                        g.truncated = true;
                    }
                } else {
                    g.truncated = true;
                }
            }
        }
    }
}

/// SIGKILL the whole process group the child leads.
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        // Safety: signalling a pid we spawned; a stale pid at worst hits
        // nothing (ESRCH) because the group id is the child's own pid.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

/// Cut a child off from the harness's terminal, before it is spawned.
///
/// The old spelling was `process_group(0)`, which stops *signals* and
/// nothing else: the child kept the TUI's terminal as its controlling
/// terminal, so a command that asks for something on it — `sudo` reading
/// a password from `/dev/tty`, an ssh passphrase, a git credential —
/// wrote its prompt straight onto the screen the TUI was drawing, at the
/// very cell where the frame had parked its cursor: in the prompt box.
/// The frame's diff never learns those cells were scribbled on, so the
/// prompt sat over the box as corruption until every one of its cells
/// happened to change, and the keystrokes meant as the password went to
/// the TUI's own raw-mode reader and into the message draft instead.
///
/// `setsid` gives the child a session of its own, and a session leader
/// has no controlling terminal until it opens one — which a child whose
/// stdio is piped and nulled cannot. A command that still asks gets its
/// refusal on stderr (`sudo: a terminal is required to read the
/// password`), which is captured and shown to the model, so the
/// headless way around (`sudo -S` fed a password by the operator, never
/// typed at a hidden prompt) is what it reaches for next.
///
/// A session leader is its own process group leader, so
/// [`kill_group`]'s `-pid` keeps working unchanged.
pub(crate) fn no_tty(cmd: &mut tokio::process::Command) {
    // Safety: the closure only calls `setsid`, which is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            // The child is a fresh fork in the parent's process group, so
            // it is not a group leader and the call cannot fail with
            // `EPERM`; the error is passed along if it does anyway.
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of `exec`: whatever an argument contains is one argument. No
    /// word splitting, no expansion, no metacharacter — because no shell ever
    /// saw a command line. A wrapper taking a value from a model wants this.
    #[tokio::test]
    async fn exec_gives_the_program_its_arguments_verbatim() {
        let d = tempfile::tempdir().unwrap();
        let e = exec(
            d.path(),
            "printf",
            &["%s|%s".to_string(), "a;b".to_string(), "$HOME`id`".to_string()],
            Some(Duration::from_secs(10)),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(e.stdout, "a;b|$HOME`id`");
        assert!(e.ok(), "{}", e.render());
        // The same text through the shell *is* interpreted: the pair is what
        // makes the difference a fact rather than a claim.
        let shell = run(
            d.path(),
            "printf '%s|%s' 'a;b' \"$HOME\"",
            Some(Duration::from_secs(10)),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_ne!(shell.stdout, e.stdout);
    }

    #[tokio::test]
    async fn exec_of_a_program_that_is_not_there_is_an_error_and_an_exit_code_is_not() {
        let d = tempfile::tempdir().unwrap();
        let e = exec(
            d.path(),
            "definitely-not-a-program-here",
            &[],
            Some(Duration::from_secs(10)),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(format!("{e:#}").contains("spawning"), "{e:#}");

        // A program that runs and says no is a result, not an error — the same
        // contract `run` has.
        let e = exec(
            d.path(),
            "false",
            &[],
            Some(Duration::from_secs(10)),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(e.code, Some(1));
        assert!(e.render().contains("[exit code 1]"), "{}", e.render());
    }

    /// The exact command ministral produced, which used to stall for the
    /// full timeout: bash exits in about a second, but the backgrounded
    /// server inherits stdout and holds the pipe open.
    /// A backgrounded process must survive the call *and keep working*.
    /// Closing our end of the pipe when the call returned killed it with
    /// SIGPIPE on its next write — so a server started to be opened in a
    /// browser came back alive and died on the first request.
    #[tokio::test]
    async fn a_background_process_keeps_running_and_writing() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("ticks");
        let cmd = format!(
            "( for i in 1 2 3 4 5; do echo tick; echo $i >> {}; sleep 0.2; done & ) ; echo started",
            log.display()
        );
        let e = run(
            d.path(),
            &cmd,
            Some(Duration::from_secs(10)),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(!e.timed_out);
        assert!(
            e.background,
            "the still-open pipe should be reported: {}",
            e.render()
        );
        assert!(e.render().contains("background process"), "{}", e.render());
        tokio::time::sleep(Duration::from_millis(1600)).await;
        let ticks = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(
            ticks, 5,
            "the background process was killed after {ticks} of 5 writes"
        );
    }

    #[tokio::test]
    async fn an_ordinary_command_is_not_flagged_as_background() {
        let d = tempfile::tempdir().unwrap();
        let e = run(d.path(), "echo hi", None, &CancellationToken::new())
            .await
            .unwrap();
        assert!(!e.background);
        assert!(!e.render().contains("background"));
    }

    /// The bug Noah photographed: a command that asks for a password on
    /// the terminal painted its prompt over the prompt box, because the
    /// child still had the TUI's terminal as its controlling one and
    /// `sudo` writes to `/dev/tty` directly, past every pipe. The child
    /// now leads a session of its own — and a session leader has no
    /// controlling terminal until it opens one, so `/dev/tty` is closed
    /// to it. The session is asked from here, with `getsid`, because
    /// `ps` is a package a build sandbox does not have: the command
    /// backgrounds a `sleep`, which holds the pipe and so is still
    /// alive to be asked about once `run` has answered.
    #[tokio::test]
    async fn a_child_cannot_ask_on_the_harness_terminal() {
        let e = run(
            Path::new("."),
            "sleep 5 & echo $!",
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let pid: libc::pid_t = e
            .stdout
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("no pid came back: {}", e.render()));
        // `getsid` is POSIX where `ps` is procps; the child keeps running
        // (it is the `sleep`, in the session `setsid` gave its bash), so
        // the answer is the child's and not a stale -1.
        let (theirs, ours) = unsafe { (libc::getsid(pid), libc::getsid(0)) };
        assert_ne!(
            theirs,
            -1,
            "the child was gone before its session could be asked: {}",
            e.render()
        );
        assert_ne!(
            theirs, ours,
            "the child is in this session, so it can open /dev/tty"
        );
        // And the terminal itself is unreachable: opening it is an error,
        // which is the refusal a password prompt would have to come from.
        let e = run(
            Path::new("."),
            "exec 3<> /dev/tty",
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(
            e.code != Some(0),
            "/dev/tty opened under setsid: {}",
            e.render()
        );
    }

    #[tokio::test]
    async fn a_backgrounded_process_does_not_hold_the_call_open() {
        let d = tempfile::tempdir().unwrap();
        let t = std::time::Instant::now();
        let e = run(
            d.path(),
            "sleep 30 & echo done",
            Some(Duration::from_secs(20)),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "took {:?}",
            t.elapsed()
        );
        assert!(!e.timed_out);
        assert_eq!(e.stdout.trim(), "done");
    }

    #[tokio::test]
    async fn output_written_before_a_timeout_is_still_reported() {
        let d = tempfile::tempdir().unwrap();
        let e = run(
            d.path(),
            "echo partial; sleep 30",
            Some(Duration::from_secs(1)),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(e.timed_out);
        // Previously this came back empty, so the model was told a command
        // timed out and nothing about what it had managed to do.
        assert_eq!(e.stdout.trim(), "partial");
        assert!(e.render().contains("partial"), "{}", e.render());
    }

    #[tokio::test]
    async fn a_cancelled_command_says_so_and_is_not_ok() {
        let d = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let canceller = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                cancel.cancel();
            })
        };
        let e = run(d.path(), "echo started; sleep 30", None, &cancel)
            .await
            .unwrap();
        canceller.await.unwrap();
        assert!(e.cancelled, "{}", e.render());
        assert!(!e.ok(), "{}", e.render());
        assert!(
            e.render().starts_with("[cancelled; process killed]\n"),
            "{}",
            e.render()
        );
        // Whatever it managed before the kill is still the transcript's.
        assert_eq!(e.stdout.trim(), "started", "{}", e.render());
    }

    #[tokio::test]
    async fn captures_and_exits() {
        let d = tempfile::tempdir().unwrap();
        let e = run(
            d.path(),
            "echo hi; echo err >&2; exit 3",
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(e.code, Some(3));
        assert_eq!(e.stdout, "hi\n");
        assert_eq!(e.stderr, "err\n");
    }

    #[tokio::test]
    async fn times_out_and_kills_children() {
        let d = tempfile::tempdir().unwrap();
        let e = run(
            d.path(),
            "sleep 30 & sleep 30",
            Some(Duration::from_millis(200)),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(e.timed_out);
    }

    /// The stateless background shape: the call answers before the command
    /// is done, the command is alive to be found by its pid, and its
    /// output — including everything written *after* the call returned —
    /// lands in the log it was handed.
    #[test]
    fn a_background_command_returns_now_and_its_log_keeps_the_output() {
        let d = tempfile::tempdir().unwrap();
        let t = std::time::Instant::now();
        let b = run_background(
            d.path(),
            "echo started; for i in 1 2 3 4 5 6; do echo tick$i; sleep 0.2; done",
        )
        .unwrap();
        assert!(
            t.elapsed() < Duration::from_secs(1),
            "the call waited for the command: {:?}",
            t.elapsed()
        );
        assert!(
            b.render().contains("running in the background"),
            "{}",
            b.render()
        );
        let pid = b.pid as i32;
        // Alive at return — and in its own session, which is what makes it
        // survivable, but `a_child_cannot_ask_on_the_harness_terminal`
        // already pins the session half for this spawner's twin.
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "the command was already gone");
        assert!(b.log.is_file(), "{}", b.render());
        // The loop runs ~1.2s; the call has long since returned by the
        // time the last tick is written, so this is post-return output.
        std::thread::sleep(Duration::from_millis(1800));
        let log = std::fs::read_to_string(&b.log).unwrap();
        assert!(log.contains("started"), "{log}");
        assert!(log.contains("tick6"), "{log}");
    }

    #[test]
    fn a_background_command_needs_a_command() {
        let d = tempfile::tempdir().unwrap();
        assert!(run_background(d.path(), "   ").is_err());
    }

    /// The other half of the stateless shape: the exit status is written
    /// down, so "did it work" has an answer long after the call that
    /// started the command is over.
    #[test]
    fn a_background_command_records_its_exit_status() {
        let d = tempfile::tempdir().unwrap();
        let b = run_background(d.path(), "echo done; exit 7").unwrap();
        assert!(
            b.exit_status().is_none(),
            "a command that has not finished has no status"
        );
        assert!(b.render().contains("status:"), "{}", b.render());
        let st = wait_for_exit(&b).expect("the reaper never wrote a status");
        assert_eq!(st.code, Some(7), "{st:?}");
        assert_eq!(st.signal, None, "{st:?}");
        assert_eq!(st.pid, b.pid, "the status names the process that was spawned");
    }

    /// A signal is not a code, and the record says which one happened: a
    /// command killed by a signal reports no exit code at all.
    #[test]
    fn a_killed_background_command_records_the_signal_and_no_code() {
        let d = tempfile::tempdir().unwrap();
        let b = run_background(d.path(), "sleep 30").unwrap();
        assert_eq!(unsafe { libc::kill(b.pid as i32, libc::SIGKILL) }, 0);
        let st = wait_for_exit(&b).expect("the reaper never wrote a status");
        assert_eq!(st.code, None, "{st:?}");
        assert_eq!(st.signal, Some(libc::SIGKILL), "{st:?}");
    }

    /// The reaper writes from its own thread, so a caller that wants the
    /// answer waits for it rather than assuming it is already there.
    fn wait_for_exit(b: &Background) -> Option<BackgroundExit> {
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if let Some(st) = b.exit_status() {
                return Some(st);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}
