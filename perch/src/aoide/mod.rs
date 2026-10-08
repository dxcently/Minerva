//! The Aoide runner (slice R2a): the one place that builds an `aoide` argv and runs it. `mesh` and `mail` call it; it has no route and no
//! `Ctx` field.
//!
//! **Aoide is a program, never a crate.** It is GPL-3.0 and the perch is MIT, so the perch reaches it as a child process and nothing
//! else: no `aoide-*` dependency (`tests/gpl_boundary.rs` fails on one in the lock), no linking, no code copied. What it knows of the
//! program is its argv and the `--json` envelope it prints, and the latter is parsed defensively because `schema --json` does not
//! describe it and `schemaVersion` stayed `"0"` through every change.
//!
//! **Never a `PATH` lookup.** [`Aoide::bin`] is an absolute path (`--aoide`, else `~/.local/bin/aoide` when that is executable) and
//! [`Aoide::run`] refuses anything else, because `Command::new("aoide")` finds the stale 0.0.25 in `~/.cargo/bin` before the real one in
//! `~/.local/bin`. The child's own `PATH` is the perch's, untouched; it is only never used to find aoide.
//!
//! **A closed set of argvs.** [`Shape`] is every call there is, and [`Shape::argv`] is the only code that spells one. Every value is
//! checked there, before any process is started, so a refusal costs nothing and leaves nothing behind. The spelling is always
//! `--flag=value` (aoide's parser lets a spaced flag swallow the next token unless it starts with `--`, and refuses a value that is also
//! a command word, so `--subject status` is not safe and `--subject=status` is), and `--` stands before the text, so a letter that
//! starts with `--` or says `--help` is a letter and not a flag.
//!
//! **The child's environment** is the perch's with every `AOIDE_*` variable and `XDG_RUNTIME_DIR` removed, and then only what the perch
//! means set: the four paths of [`Aoide`] and, for `mail read` alone, the reader identity [`READER`]. A perch started from inside a conducted
//! session would otherwise hand its `AOIDE_SESSION_ID` to every call (and read mail as that session), and an ambient `AOIDE_STATE_DIR` would move the
//! mailbase away from `<root>/state/mail/base.jsonl`, which is the file the receive loop stats to decide whether to read at all.
//!
//! **Bounded.** stdin is closed, stdout is capped ([`STDOUT_CAP`]), stderr keeps only its first [`STDERR_KEEP`] bytes (the rest is read
//! and dropped, so a chatty child never blocks on a full pipe), and the whole call has [`TIMEOUT`], after which the child and its
//! process group are killed and reaped. A mail read takes a blocking `flock` on `.stage.lock`, so a held lock is a timeout and not a hang.
//!
//! **What is logged** is one line per call: the verb and the exit code, never an argument, the text, or what aoide printed. [`Shape`] and
//! [`Reply`] print themselves the same way, so a stray `{:?}` is not a way for a letter into a log.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::AsyncReadExt as _;
use tokio::process::{Child, Command};

use crate::Options;

/// The mailbox every note is sent to (`<node>/minerva`) and every read is of (`--for=minerva`).
pub const MAILBOX: &str = "minerva";
/// The identity a `mail read` runs under (`AOIDE_SESSION_ID`): the reader whose cursor the perch's reads advance. Set on that call only.
pub const READER: &str = "minerva-perch";
/// How long a call may take before the child is killed. Mail reads block on `.stage.lock`, so a held lock ends here.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// How much stdout is taken. One byte more is [`Error::TooBig`], and the child is killed.
pub const STDOUT_CAP: usize = 1024 * 1024;
/// How much of stderr is kept, for the sentence of a failure that printed no envelope. The rest is read and dropped.
pub const STDERR_KEEP: usize = 8 * 1024;
/// A subject is one line of at most this many characters.
pub const SUBJECT_MAX: usize = 120;
/// A letter's text, in bytes. It sits in argv while aoide runs (so it is readable in `/proc/<pid>/cmdline`), which is why it is small, and
/// one argument can be 128 KiB at most on Linux. `mail` refuses a bigger note with a 413 before it gets here; this is the backstop.
pub const TEXT_MAX: usize = 64 * 1024;
/// A node, mesh or mailbox name: `^[a-z0-9][a-z0-9-]*$`, and at most this long.
pub const NAME_MAX: usize = 64;
/// After the child has exited, how long a pipe that something else still holds open is waited for before the call goes on with what it has.
const EXIT_GRACE: Duration = Duration::from_millis(250);
/// How long a killed child is waited for. A process in an uninterruptible sleep is not going to be reaped by waiting longer.
const REAP: Duration = Duration::from_secs(2);

/// The limits of one call, [`Limits::default`] outside tests.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub timeout: Duration,
    pub stdout_cap: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { timeout: TIMEOUT, stdout_cap: STDOUT_CAP }
    }
}

/// How to run aoide: the binary and the four places it is pointed at. Cheap to build, so a caller makes one per call from
/// [`Aoide::from_options`] and keeps none.
#[derive(Clone, Debug)]
pub struct Aoide {
    /// `--aoide`. Absolute; a relative path is refused when a call runs, because it would be a `PATH` lookup (or the cwd's).
    pub bin: PathBuf,
    /// `--aoide-home`, given to the child as `AOIDE_ROOT`. `None` leaves aoide its default (`~/.aoide`).
    pub root: Option<PathBuf>,
    /// `--aoide-audit-log` (through [`audit_flag`], the same reader the mesh view's audit path uses), given as `AOIDE_AUDIT_LOG` and only when
    /// the flag was given. Never the daemon's feed: aoide appending its own audit records into aoided's feed would be the one file both
    /// writers share. With no flag aoide writes its own default (`<root>/log`), which is the file the mesh view tails when it has no flag
    /// either. A relative one is refused when a call runs ([`Aoide::env_for`]).
    pub audit_log: Option<PathBuf>,
    /// `--aoide-daemon-socket`, given as `AOIDE_DAEMON_SOCKET`.
    pub daemon_socket: Option<PathBuf>,
    /// The perch's own `$XDG_RUNTIME_DIR`, given as `XDG_RUNTIME_DIR` (aoide's default socket is under it).
    pub runtime_dir: Option<PathBuf>,
    pub limits: Limits,
    /// Variables beyond those above, for the test doubles (`FAKE_*`). A name starting `AOIDE_` is the runner's and is refused.
    extra: Vec<(OsString, OsString)>,
}

/// `--aoide-audit-log`, as the perch was given it, or none. The one reader of that flag: the runner's `AOIDE_AUDIT_LOG` ([`Aoide::from_options`])
/// and the mesh view's audit path (`mesh::audit_path`) both take it from here, so they name the same file. Absolute is checked where each uses
/// it (the runner refuses a relative one on every call, the view names no file for it), and `main.rs` refuses one at the start, so a running
/// perch never has one.
pub fn audit_flag(opts: &Options) -> Option<PathBuf> {
    opts.aoide_audit_log.clone()
}

/// `$XDG_RUNTIME_DIR` as the perch was started with it, or none when it is not set or is not absolute: the one reader of that variable. A
/// relative or empty value is treated as unset, by the runner and by the mesh view alike, so no path under the working directory is ever given
/// to aoide or opened. Both read it here, so they agree on the value.
pub fn runtime_dir() -> Option<PathBuf> {
    absolute_dir(std::env::var_os("XDG_RUNTIME_DIR"))
}

/// The one rule for a runtime dir's value: absolute, else none ([`runtime_dir`] is this on the process's variable).
fn absolute_dir(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|dir| dir.is_absolute())
}

impl Aoide {
    pub fn new(bin: impl Into<PathBuf>) -> Self {
        Self { bin: bin.into(), root: None, audit_log: None, daemon_socket: None, runtime_dir: None, limits: Limits::default(), extra: Vec::new() }
    }

    /// The binary and paths the perch was started with. [`Error::NoBinary`] when `--aoide` was not given and `~/.local/bin/aoide` is not
    /// executable: the mesh view says so in a sentence, and mail is unavailable.
    pub fn from_options(opts: &Options) -> Result<Self, Error> {
        let bin = opts.aoide.clone().ok_or(Error::NoBinary)?;
        Ok(Self::new(bin)
            .root(opts.aoide_home.clone())
            .audit_log(audit_flag(opts))
            .daemon_socket(opts.aoide_daemon_socket.clone())
            .runtime_dir(runtime_dir()))
    }

    pub fn root(mut self, root: Option<PathBuf>) -> Self {
        self.root = root;
        self
    }

    pub fn audit_log(mut self, audit_log: Option<PathBuf>) -> Self {
        self.audit_log = audit_log;
        self
    }

    pub fn daemon_socket(mut self, daemon_socket: Option<PathBuf>) -> Self {
        self.daemon_socket = daemon_socket;
        self
    }

    pub fn runtime_dir(mut self, runtime_dir: Option<PathBuf>) -> Self {
        self.runtime_dir = runtime_dir;
        self
    }

    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// One more variable for the child. Nothing in the perch calls this: it is how a test steers the stand-in (`FAKE_AOIDE_LOG`, ...)
    /// per instance, where the process environment is shared by every test thread. A name starting `AOIDE_` is refused when a call runs.
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.extra.push((key.into(), value.into()));
        self
    }

    /// Runs `aoide --version` and reads the version out of what it printed, defensively: the last word that starts with a digit of the
    /// first line (`aoide 0.0.26` gives `0.0.26`), else that whole line; `None` when it printed nothing.
    pub async fn version(&self) -> Result<Option<String>, Error> {
        Ok(parse_version(&self.run(&Shape::Version).await?.stdout))
    }

    /// Runs one call. `Ok` is an exit 0 whose `--json` envelope says `ok` (or, for [`Shape::Version`], plain text); everything else is an
    /// [`Error`], and an aoide that ran and said no is [`Error::Failed`] with its whole reply, so a caller can read `data.reason`.
    pub async fn run(&self, shape: &Shape) -> Result<Reply, Error> {
        let verb = shape.verb();
        let outcome = self.launch(shape).await;
        match &outcome {
            Ok(reply) => tracing::info!(verb, code = reply.code, "perch: aoide ran"),
            Err(Error::Failed(reply)) => tracing::info!(verb, code = reply.code, "perch: aoide ran"),
            Err(Error::Refused(_)) => tracing::warn!(verb, "perch: aoide was not run: the call was refused"),
            Err(e) => tracing::warn!(verb, why = %e, "perch: aoide did not answer"),
        }
        outcome
    }

    async fn launch(&self, shape: &Shape) -> Result<Reply, Error> {
        // Everything that can be refused is, before a process exists.
        let argv = shape.argv()?;
        let env = self.env_for(shape)?;
        if !self.bin.is_absolute() {
            return Err(Error::Refused("the aoide binary must be an absolute path: it is never looked up on PATH".to_string()));
        }
        let mut command = Command::new(&self.bin);
        // Its own process group, so a timeout takes whatever it started with it; and `kill_on_drop` for a caller that goes away first.
        command.args(&argv).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).process_group(0);
        for (key, _) in std::env::vars_os() {
            if is_aoide_var(&key) {
                command.env_remove(key);
            }
        }
        // The perch's own `XDG_RUNTIME_DIR` is not inherited either: the child gets the runner's value (absolute, or none), so a relative
        // one in the perch's environment reaches no call.
        command.env_remove("XDG_RUNTIME_DIR");
        command.envs(env);
        let mut child = command.spawn().map_err(|e| Error::Spawn(format!("{}: {e}", self.bin.display())))?;
        let group = child.id();
        let begun = Instant::now();
        let driven = tokio::time::timeout(self.limits.timeout, drive(&mut child, self.limits.stdout_cap)).await;
        let done = match driven {
            Ok(Ok(done)) => done,
            Ok(Err(e)) => {
                put_down(&mut child, group).await;
                return Err(e);
            }
            Err(_) => {
                put_down(&mut child, group).await;
                return Err(Error::Timeout(self.limits.timeout));
            }
        };
        if done.straggler {
            // The child is gone and something it started still holds a pipe: it goes too, with what was read being the answer.
            kill_group(group);
        }
        tracing::debug!(verb = shape.verb(), ms = begun.elapsed().as_millis() as u64, "perch: aoide finished");
        let stdout = String::from_utf8_lossy(&done.stdout).into_owned();
        let reply = Reply { code: done.code, json: envelope(&stdout), stdout, stderr: String::from_utf8_lossy(&done.stderr).trim().to_string() };
        if !reply.is_ok() {
            return Err(Error::Failed(reply));
        }
        if shape.json() && reply.json.is_none() {
            return Err(Error::NotJson);
        }
        Ok(reply)
    }

    /// What the child is told. Paths are absolute or the call is refused: a relative one would be resolved against the perch's cwd.
    fn env_for(&self, shape: &Shape) -> Result<Vec<(OsString, OsString)>, Error> {
        let mut env: Vec<(OsString, OsString)> = Vec::new();
        for (name, value) in [
            ("AOIDE_ROOT", &self.root),
            ("AOIDE_AUDIT_LOG", &self.audit_log),
            ("AOIDE_DAEMON_SOCKET", &self.daemon_socket),
            ("XDG_RUNTIME_DIR", &self.runtime_dir),
        ] {
            let Some(path) = value else { continue };
            if !path.is_absolute() {
                return Err(Error::Refused(format!("{name} must be an absolute path")));
            }
            env.push((name.into(), path.as_os_str().to_os_string()));
        }
        // The reader identity belongs to a read and to nothing else.
        if matches!(shape, Shape::MailRead { .. }) {
            env.push(("AOIDE_SESSION_ID".into(), READER.into()));
        }
        for (key, value) in &self.extra {
            if is_aoide_var(key) {
                return Err(Error::Refused(format!("{} is an aoide variable, and the runner sets those itself", key.to_string_lossy())));
            }
            env.push((key.clone(), value.clone()));
        }
        Ok(env)
    }
}

/// Whether an environment variable is one of aoide's: `AOIDE_` and then anything. Exact case, as the environment is.
fn is_aoide_var(key: &OsStr) -> bool {
    key.as_bytes().starts_with(b"AOIDE_")
}

/// Kills the child's whole process group and then the child, and waits a moment for it to be reaped.
async fn put_down(child: &mut Child, group: Option<u32>) {
    kill_group(group);
    let _ = child.start_kill();
    let _ = tokio::time::timeout(REAP, child.wait()).await;
}

/// SIGKILL to a process group that is known to still have a member: the child before it was waited for, or a pipe's holder after.
fn kill_group(group: Option<u32>) {
    if let Some(pid) = group {
        unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
    }
}

/// Where a letter goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum To {
    /// This machine, `self/minerva`: filed locally, never on the wire, and so never held.
    SelfNode,
    /// A node by name, `<node>/minerva`. `self` is not a name here ([`To::SelfNode`] is), so the hold rule cannot be walked around.
    Node(String),
}

/// Who a letter says it is from (`--from=`, attribution only and not authentication, which aoide's own help says too). A fixed choice
/// and never text, so nothing a page or an agent typed can become a sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sender {
    /// The operator, through the page.
    Minerva,
    /// An agent, through `mail_send`.
    MinervaAgent,
}

impl Sender {
    pub fn name(self) -> &'static str {
        match self {
            Self::Minerva => "minerva",
            Self::MinervaAgent => "minerva-agent",
        }
    }
}

/// Every call the perch makes to aoide, and the only way to make one. Nothing here touches a node: no `node`, `session`, `send`,
/// `pair`, `spawn`, `allow`, `pull`, `dispatch` or anything on the A2A door has a variant, so none can be built.
#[derive(Clone, PartialEq, Eq)]
pub enum Shape {
    /// `--version`.
    Version,
    /// `mesh --json`: what the mesh declares and where the registry drifts from it.
    MeshJson,
    /// `mail outbox --json`: what is spooled toward a node, with each entry's delivery status. Reads only.
    MailOutboxJson,
    /// `mail read --for=minerva --json [--reread]`, as the reader [`READER`]. A plain read advances that reader's cursor; `reread` does not
    /// move it and prints what was read before too.
    MailRead { reread: bool },
    /// `mail send --json --to=<node>/minerva --from=<who> [--mesh=<m>] --subject=<s> [--hold] -- <text>`.
    MailSend { to: To, from: Sender, mesh: Option<String>, subject: String, hold: bool, text: String },
}

impl Shape {
    /// The words a log line says, never an argument.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::MeshJson => "mesh",
            Self::MailOutboxJson => "mail outbox",
            Self::MailRead { .. } => "mail read",
            Self::MailSend { .. } => "mail send",
        }
    }

    /// Whether the call is asked for `--json` and so answers with an envelope.
    fn json(&self) -> bool {
        !matches!(self, Self::Version)
    }

    /// The argv after the program name, or the sentence it was refused with. Pure: nothing is run, so a test can pin every shape.
    pub fn argv(&self) -> Result<Vec<String>, Error> {
        let words = |words: &[&str]| words.iter().map(|word| word.to_string()).collect::<Vec<_>>();
        Ok(match self {
            Self::Version => words(&["--version"]),
            Self::MeshJson => words(&["mesh", "--json"]),
            Self::MailOutboxJson => words(&["mail", "outbox", "--json"]),
            Self::MailRead { reread } => {
                let mut argv = words(&["mail", "read"]);
                argv.push(format!("--for={MAILBOX}"));
                argv.push("--json".to_string());
                if *reread {
                    argv.push("--reread".to_string());
                }
                argv
            }
            Self::MailSend { to, from, mesh, subject, hold, text } => {
                let node = match to {
                    To::SelfNode => "self",
                    To::Node(name) if name == "self" => return Err(refused("self is this machine: use To::SelfNode")),
                    To::Node(name) if name_ok(name) => name.as_str(),
                    To::Node(_) => return Err(refused(NAME_RULE)),
                };
                if let Some(mesh) = mesh {
                    if !name_ok(mesh) {
                        return Err(refused(NAME_RULE));
                    }
                    if *to == To::SelfNode {
                        return Err(refused("this machine is in no mesh: a mesh names the one a node is reached through"));
                    }
                }
                if *hold && *to == To::SelfNode {
                    return Err(refused("hold needs another node"));
                }
                subject_ok(subject)?;
                text_ok(text)?;
                let mut argv = words(&["mail", "send", "--json"]);
                argv.push(format!("--to={node}/{MAILBOX}"));
                argv.push(format!("--from={}", from.name()));
                if let Some(mesh) = mesh {
                    argv.push(format!("--mesh={mesh}"));
                }
                argv.push(format!("--subject={subject}"));
                if *hold {
                    argv.push("--hold".to_string());
                }
                // After `--` every word is the text's, so a text that starts with `--` or says `--help` is only that.
                argv.push("--".to_string());
                argv.push(text.clone());
                argv
            }
        })
    }
}

/// A shape prints its verb and the sizes of what it carries. A subject and a text are the operator's words and an agent's, and a
/// `{:?}` in a caller's log line is not a place for them.
impl std::fmt::Debug for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Version => f.write_str("Version"),
            Self::MeshJson => f.write_str("MeshJson"),
            Self::MailOutboxJson => f.write_str("MailOutboxJson"),
            Self::MailRead { reread } => f.debug_struct("MailRead").field("reread", reread).finish(),
            Self::MailSend { to, from, mesh, subject, hold, text } => f
                .debug_struct("MailSend")
                .field("to", to)
                .field("from", from)
                .field("mesh", mesh)
                .field("hold", hold)
                .field("subject_chars", &subject.chars().count())
                .field("text_bytes", &text.len())
                .finish(),
        }
    }
}

const NAME_RULE: &str = "a node or mesh name is lower case letters, digits and -, starting with a letter or digit, at most 64 characters";

fn refused(sentence: &str) -> Error {
    Error::Refused(sentence.to_string())
}

/// `^[a-z0-9][a-z0-9-]*$`, at most [`NAME_MAX`]: the grammar aoide gives node, mesh and mailbox names.
pub fn name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= NAME_MAX
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// One line, at most [`SUBJECT_MAX`] characters, with something in it. Not trimmed or cut: what is sent is what was asked, or nothing.
fn subject_ok(subject: &str) -> Result<(), Error> {
    if subject.trim().is_empty() {
        return Err(refused("a letter needs a subject"));
    }
    // A tab is a character; the rest of the control characters are not what a line is made of, and U+2028 and U+2029 end one.
    if subject.chars().any(|c| (c.is_control() && c != '\t') || c == '\u{2028}' || c == '\u{2029}') {
        return Err(refused("the subject must be one line"));
    }
    if subject.chars().count() > SUBJECT_MAX {
        return Err(refused("the subject is over 120 characters"));
    }
    Ok(())
}

fn text_ok(text: &str) -> Result<(), Error> {
    if text.is_empty() {
        return Err(refused("a letter needs text"));
    }
    if text.len() > TEXT_MAX {
        return Err(refused("the text is over 64 KiB"));
    }
    // An argument is a C string.
    if text.contains('\0') {
        return Err(refused("the text has a NUL byte, which an argument cannot carry"));
    }
    Ok(())
}

/// What aoide answered. `json` is the whole of stdout when that is one JSON object (aoide's `--json` envelope: `status`, `command`,
/// `message`, `gated`, `changed`, `data`), and is read through the accessors, which tolerate any other shape.
#[derive(Clone)]
pub struct Reply {
    /// The exit code, `-1` when the child was ended by a signal.
    pub code: i32,
    pub stdout: String,
    pub json: Option<Value>,
    /// The first [`STDERR_KEEP`] bytes of stderr, trimmed.
    pub stderr: String,
}

impl Reply {
    /// The envelope's `status`: `ok`, `error`, `usage` or `not-implemented`.
    pub fn status(&self) -> Option<&str> {
        self.json.as_ref()?.get("status")?.as_str()
    }

    /// The envelope's `message`: aoide's own sentence.
    pub fn message(&self) -> Option<&str> {
        self.json.as_ref()?.get("message")?.as_str()
    }

    pub fn data(&self) -> Option<&Value> {
        self.json.as_ref()?.get("data")
    }

    /// An exit 0 that does not say otherwise. The status and the exit code are one fact in aoide, so a disagreement is a failure.
    pub fn is_ok(&self) -> bool {
        let status = self.json.as_ref().and_then(|json| json.get("status"));
        self.code == 0 && status.is_none_or(|status| status.as_str() == Some("ok"))
    }

    /// What to tell a person: aoide's own `message` when it sent one, else the first line of what it said on stderr (or on stdout when
    /// that was not an envelope), else the exit code.
    pub fn sentence(&self) -> String {
        if let Some(message) = self.message().map(str::trim).filter(|message| !message.is_empty()) {
            return clip(message, 1000);
        }
        let stdout = if self.json.is_none() { self.stdout.as_str() } else { "" };
        for said in [self.stderr.as_str(), stdout] {
            if let Some(line) = said.lines().map(str::trim).find(|line| !line.is_empty()) {
                return clip(line, 300);
            }
        }
        match self.code {
            -1 => "aoide was stopped by a signal".to_string(),
            code => format!("aoide exited with code {code}"),
        }
    }
}

/// Sizes and the verdict, and not what was printed: a mail read's stdout is other people's letters.
impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reply")
            .field("code", &self.code)
            .field("status", &self.status())
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .finish()
    }
}

/// Why a call gave no answer.
#[derive(Debug)]
pub enum Error {
    /// No binary: `--aoide` was not given, and `~/.local/bin/aoide` is not executable.
    NoBinary,
    /// The call was refused before anything ran, with the sentence for the person who asked.
    Refused(String),
    /// The binary could not be started (it is not there, or not executable).
    Spawn(String),
    /// It did not finish in this long, and was killed.
    Timeout(Duration),
    /// It printed more than this many bytes, and was killed.
    TooBig(usize),
    /// It exited 0 for a `--json` call and printed something that is not a JSON object.
    NotJson,
    /// It ran and said no. The reply is whole, so `data.reason` and `data.meshes` are there for a caller to read.
    Failed(Reply),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoBinary => write!(f, "no aoide is configured: start the perch with --aoide <absolute path>"),
            Self::Refused(sentence) => write!(f, "{sentence}"),
            Self::Spawn(why) => write!(f, "aoide could not be started: {why}"),
            Self::Timeout(within) => write!(f, "aoide did not answer in {} s", within.as_secs_f32().ceil() as u64),
            Self::TooBig(cap) => write!(f, "aoide printed more than {cap} bytes"),
            Self::NotJson => write!(f, "aoide did not answer with JSON"),
            Self::Failed(reply) => write!(f, "{}", reply.sentence()),
        }
    }
}

impl std::error::Error for Error {}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// stdout as aoide's envelope: the whole of it is one JSON object, or it is not one. A notice ahead of the JSON is not looked for, because
/// the text a stdout line can carry is other people's and a line that merely starts with `{` must not become a verdict.
fn envelope(stdout: &str) -> Option<Value> {
    serde_json::from_str::<Value>(stdout.trim()).ok().filter(Value::is_object)
}

/// `aoide 0.0.26` and `v0.0.26` give `0.0.26`, and a line with no such word is the line (clipped).
fn parse_version(stdout: &str) -> Option<String> {
    let line = stdout.lines().map(str::trim).find(|line| !line.is_empty())?;
    let word = line.split_whitespace().rev().map(|word| word.trim_start_matches('v')).find(|word| word.starts_with(|c: char| c.is_ascii_digit()));
    Some(clip(word.unwrap_or(line), 64))
}

/// What a child left behind: its exit code and what it printed, and whether a pipe was still held open after it exited.
struct Done {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    straggler: bool,
}

/// Reads both pipes and waits for the exit in one loop, so that a child that fills stderr is read and not stalled, a stdout over `cap`
/// is noticed the moment it is, and a pipe that outlives the child (a process it started) costs [`EXIT_GRACE`] and no more.
async fn drive(child: &mut Child, cap: usize) -> Result<Done, Error> {
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(Error::Spawn("aoide's output was not a pipe".to_string()));
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut obuf, mut ebuf) = (vec![0u8; 16 * 1024], vec![0u8; 4 * 1024]);
    let (mut out_open, mut err_open) = (true, true);
    let mut exit: Option<ExitStatus> = None;
    let mut deadline: Option<tokio::time::Instant> = None;
    let mut straggler = false;
    loop {
        if exit.is_some() && !out_open && !err_open {
            break;
        }
        tokio::select! {
            read = stdout.read(&mut obuf), if out_open => match read {
                Ok(n) if n > 0 => {
                    out.extend_from_slice(&obuf[..n]);
                    // One byte over is how "over" is told from "exactly".
                    if out.len() > cap {
                        return Err(Error::TooBig(cap));
                    }
                }
                _ => out_open = false,
            },
            read = stderr.read(&mut ebuf), if err_open => match read {
                Ok(n) if n > 0 => {
                    let room = STDERR_KEEP.saturating_sub(err.len());
                    err.extend_from_slice(&ebuf[..n.min(room)]);
                }
                _ => err_open = false,
            },
            status = child.wait(), if exit.is_none() => {
                exit = Some(status.map_err(|e| Error::Spawn(format!("waiting for aoide: {e}")))?);
                deadline = Some(tokio::time::Instant::now() + EXIT_GRACE);
            },
            _ = tokio::time::sleep_until(deadline.unwrap_or_else(tokio::time::Instant::now)), if deadline.is_some() => {
                straggler = true;
                break;
            },
        }
    }
    Ok(Done { code: exit.and_then(|status| status.code()).unwrap_or(-1), stdout: out, stderr: err, straggler })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(to: To, mesh: Option<&str>, subject: &str, hold: bool, text: &str) -> Shape {
        Shape::MailSend { to, from: Sender::Minerva, mesh: mesh.map(str::to_string), subject: subject.to_string(), hold, text: text.to_string() }
    }

    fn node(name: &str) -> To {
        To::Node(name.to_string())
    }

    fn argv(shape: &Shape) -> Vec<String> {
        shape.argv().unwrap_or_else(|e| panic!("{shape:?} was refused: {e}"))
    }

    fn refusal(shape: &Shape) -> String {
        match shape.argv() {
            Err(Error::Refused(sentence)) => sentence,
            other => panic!("{shape:?} was not refused: {other:?}"),
        }
    }

    #[test]
    fn every_shape_spells_its_argv_one_way() {
        assert_eq!(argv(&Shape::Version), ["--version"]);
        assert_eq!(argv(&Shape::MeshJson), ["mesh", "--json"]);
        assert_eq!(argv(&Shape::MailOutboxJson), ["mail", "outbox", "--json"]);
        assert_eq!(argv(&Shape::MailRead { reread: false }), ["mail", "read", "--for=minerva", "--json"]);
        assert_eq!(argv(&Shape::MailRead { reread: true }), ["mail", "read", "--for=minerva", "--json", "--reread"]);
        assert_eq!(
            argv(&send(To::SelfNode, None, "a note", false, "hello")),
            ["mail", "send", "--json", "--to=self/minerva", "--from=minerva", "--subject=a note", "--", "hello"]
        );
        let all = Shape::MailSend {
            to: node("osaka-2"),
            from: Sender::MinervaAgent,
            mesh: Some("lab".to_string()),
            subject: "s".to_string(),
            hold: true,
            text: "t".to_string(),
        };
        assert_eq!(
            argv(&all),
            ["mail", "send", "--json", "--to=osaka-2/minerva", "--from=minerva-agent", "--mesh=lab", "--subject=s", "--hold", "--", "t"]
        );
    }

    #[test]
    fn the_text_is_the_last_argument_whatever_it_says() {
        for text in ["--help", "--", "-- --json", "--to=evil/minerva\n--hold", "line one\nline two\r\n", "q\u{1b}[31m 世界 \u{1f600}", "   "] {
            let got = argv(&send(node("osaka"), None, "s", false, text));
            assert_eq!(got.last().map(String::as_str), Some(text));
            assert_eq!(got[got.len() - 2], "--", "the text is behind the double dash: {got:?}");
            assert_eq!(got.iter().filter(|word| word.as_str() == "--").count(), if text == "--" { 2 } else { 1 }, "{got:?}");
        }
        // A subject that is a command word, or a flag's name, is still one value in the `=` form.
        for subject in ["status", "send", "--hold", "mail", "--help"] {
            let got = argv(&send(node("osaka"), None, subject, false, "t"));
            assert!(got.contains(&format!("--subject={subject}")), "{got:?}");
        }
    }

    #[test]
    fn what_cannot_be_spelled_safely_is_refused_with_a_sentence() {
        let long_subject = "x".repeat(SUBJECT_MAX + 1);
        let cases: Vec<(Shape, &str)> = vec![
            (send(To::SelfNode, None, "s", true, "t"), "hold needs another node"),
            (send(node("self"), None, "s", true, "t"), "self is this machine"),
            (send(node("self"), None, "s", false, "t"), "self is this machine"),
            (send(node("osaka"), Some("Lab"), "s", false, "t"), "lower case"),
            (send(node("osaka"), Some("lab\n--hold"), "s", false, "t"), "lower case"),
            (send(node("osaka"), Some("-lab"), "s", false, "t"), "lower case"),
            (send(node("osaka"), Some(""), "s", false, "t"), "lower case"),
            (send(To::SelfNode, Some("lab"), "s", false, "t"), "no mesh"),
            (send(node("Osaka"), None, "s", false, "t"), "lower case"),
            (send(node("a/b"), None, "s", false, "t"), "lower case"),
            (send(node("-x"), None, "s", false, "t"), "lower case"),
            (send(node(&"n".repeat(NAME_MAX + 1)), None, "s", false, "t"), "lower case"),
            (send(node("osaka"), None, "two\nlines", false, "t"), "one line"),
            (send(node("osaka"), None, "cr\rhere", false, "t"), "one line"),
            (send(node("osaka"), None, "esc\u{1b}[0m", false, "t"), "one line"),
            (send(node("osaka"), None, "nul\0", false, "t"), "one line"),
            (send(node("osaka"), None, "sep\u{2028}x", false, "t"), "one line"),
            (send(node("osaka"), None, &long_subject, false, "t"), "120 characters"),
            (send(node("osaka"), None, "  ", false, "t"), "needs a subject"),
            (send(node("osaka"), None, "", false, "t"), "needs a subject"),
            (send(node("osaka"), None, "s", false, ""), "needs text"),
            (send(node("osaka"), None, "s", false, "nul\0byte"), "NUL"),
            (send(node("osaka"), None, "s", false, &"x".repeat(TEXT_MAX + 1)), "64 KiB"),
        ];
        for (shape, says) in cases {
            let sentence = refusal(&shape);
            assert!(sentence.contains(says), "{shape:?}: {sentence:?} does not say {says:?}");
        }
        // The edges that are allowed: exactly the limits, a tab, a name of 64 characters, a name that is all digits.
        argv(&send(node(&"n".repeat(NAME_MAX)), Some("0-9"), &"é".repeat(SUBJECT_MAX), false, &"x".repeat(TEXT_MAX)));
        argv(&send(node("7"), None, "a\ttab", true, "t"));
    }

    #[test]
    fn a_name_is_the_grammar_and_nothing_else() {
        for ok in ["a", "0", "osaka", "yomi-strix", "a-", "9lives", "a--b"] {
            assert!(name_ok(ok), "{ok}");
        }
        for bad in ["", "-a", "A", "a_b", "a b", "a.b", "a/b", "é", "a\n", "a\0", "ａ"] {
            assert!(!name_ok(bad), "{bad:?}");
        }
    }

    fn envs(aoide: &Aoide, shape: &Shape) -> Vec<(String, String)> {
        aoide
            .env_for(shape)
            .unwrap()
            .into_iter()
            .map(|(key, value)| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
            .collect()
    }

    #[test]
    fn the_reader_identity_belongs_to_a_read_and_nothing_else() {
        let aoide = Aoide::new("/opt/aoide");
        let reader = ("AOIDE_SESSION_ID".to_string(), READER.to_string());
        for reread in [false, true] {
            assert_eq!(envs(&aoide, &Shape::MailRead { reread }), vec![reader.clone()]);
        }
        for shape in [Shape::Version, Shape::MeshJson, Shape::MailOutboxJson, send(node("osaka"), None, "s", false, "t")] {
            assert_eq!(envs(&aoide, &shape), Vec::<(String, String)>::new(), "{shape:?} runs with no identity of its own");
        }
    }

    #[test]
    fn the_four_places_become_the_four_variables_and_must_be_absolute() {
        let aoide = Aoide::new("/opt/aoide")
            .root(Some("/r".into()))
            .audit_log(Some("/a.log".into()))
            .daemon_socket(Some("/d.sock".into()))
            .runtime_dir(Some("/run/u".into()));
        assert_eq!(
            envs(&aoide, &Shape::MeshJson),
            [("AOIDE_ROOT", "/r"), ("AOIDE_AUDIT_LOG", "/a.log"), ("AOIDE_DAEMON_SOCKET", "/d.sock"), ("XDG_RUNTIME_DIR", "/run/u")]
                .map(|(key, value)| (key.to_string(), value.to_string()))
        );
        for relative in [
            Aoide::new("/opt/aoide").root(Some("state".into())),
            Aoide::new("/opt/aoide").audit_log(Some("./a.log".into())),
            Aoide::new("/opt/aoide").daemon_socket(Some("d.sock".into())),
            Aoide::new("/opt/aoide").runtime_dir(Some("run".into())),
        ] {
            assert!(matches!(relative.env_for(&Shape::MeshJson), Err(Error::Refused(why)) if why.contains("absolute")));
        }
    }

    /// The perch's runtime dir is absolute or none: a relative or empty `XDG_RUNTIME_DIR` is what an unset one is, for the runner (no
    /// `XDG_RUNTIME_DIR` in any call) and for the mesh view (no default path under the working directory). Mutation: keep a relative value
    /// (`absolute_dir` without the filter), and the relative cases below go red.
    #[test]
    fn a_relative_or_empty_runtime_dir_is_none_as_an_unset_one_is() {
        assert_eq!(absolute_dir(Some("/run/user/1000".into())), Some(PathBuf::from("/run/user/1000")));
        assert_eq!(absolute_dir(None), None);
        for not in ["", "run/user", "./run", "../run"] {
            assert_eq!(absolute_dir(Some(not.into())), None, "{not:?}");
        }
    }

    #[test]
    fn an_extra_variable_cannot_be_an_aoide_one() {
        let aoide = Aoide::new("/opt/aoide").env("FAKE_AOIDE_LOG", "/l");
        assert_eq!(envs(&aoide, &Shape::MeshJson), [("FAKE_AOIDE_LOG".to_string(), "/l".to_string())]);
        for name in ["AOIDE_SESSION_ID", "AOIDE_ROOT", "AOIDE_STATE_DIR", "AOIDE_"] {
            let bad = Aoide::new("/opt/aoide").env(name, "x");
            assert!(matches!(bad.env_for(&Shape::MeshJson), Err(Error::Refused(_))), "{name}");
        }
    }

    #[test]
    fn an_aoide_variable_is_one_that_starts_with_the_prefix_in_that_case() {
        for yes in ["AOIDE_SESSION_ID", "AOIDE_ROOT", "AOIDE_X", "AOIDE_"] {
            assert!(is_aoide_var(OsStr::new(yes)), "{yes}");
        }
        for no in ["aoide_session_id", "AOIDE", "XAOIDE_ROOT", "FAKE_AOIDE_LOG", "XDG_RUNTIME_DIR", ""] {
            assert!(!is_aoide_var(OsStr::new(no)), "{no}");
        }
    }

    #[test]
    fn a_version_is_read_out_of_whatever_was_printed() {
        assert_eq!(parse_version("aoide 0.0.26\n").as_deref(), Some("0.0.26"));
        assert_eq!(parse_version("\n  aoide v0.0.26 (abc123)  \nsecond line").as_deref(), Some("0.0.26"));
        assert_eq!(parse_version("0.0.26").as_deref(), Some("0.0.26"));
        assert_eq!(parse_version("no digits here").as_deref(), Some("no digits here"));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("  \n \n"), None);
        assert_eq!(parse_version(&"w".repeat(500)).map(|v| v.len()), Some(64));
    }

    #[test]
    fn stdout_is_an_envelope_only_when_the_whole_of_it_is_one_object() {
        assert!(envelope("{\"status\":\"ok\"}\n").is_some());
        assert!(envelope("  {\n \"status\": \"ok\"\n}  ").is_some());
        for not in ["", "ok", "[1]", "3", "\"s\"", "null", "notice\n{\"status\":\"ok\"}", "{\"status\":\"ok\"} trailing", "{\"status\":"] {
            assert!(envelope(not).is_none(), "{not:?}");
        }
    }

    fn reply(code: i32, stdout: &str, stderr: &str) -> Reply {
        Reply { code, stdout: stdout.to_string(), json: envelope(stdout), stderr: stderr.to_string() }
    }

    #[test]
    fn a_reply_is_ok_only_when_the_code_and_the_status_agree() {
        assert!(reply(0, "{\"status\":\"ok\"}", "").is_ok());
        assert!(reply(0, "{\"data\":1}", "").is_ok(), "an envelope with no status is the exit code's to judge");
        assert!(reply(0, "aoide 0.0.26", "").is_ok(), "plain text is the exit code's to judge");
        assert!(!reply(1, "{\"status\":\"ok\"}", "").is_ok());
        assert!(!reply(0, "{\"status\":\"error\"}", "").is_ok());
        assert!(!reply(0, "{\"status\":\"usage\"}", "").is_ok());
        assert!(!reply(2, "{\"status\":\"usage\"}", "").is_ok());
        assert!(!reply(-1, "", "").is_ok());
        assert!(!reply(0, "{\"status\":7}", "").is_ok(), "a status that is not a word is not ok");
    }

    #[test]
    fn the_sentence_is_aoides_own_and_falls_back_to_what_it_said() {
        let said = reply(1, "{\"status\":\"error\",\"message\":\"  no node named `ghost`\\n\"}", "stderr says more");
        assert_eq!(said.sentence(), "no node named `ghost`");
        assert_eq!(reply(1, "{\"status\":\"error\"}", "\n  first stderr line\nsecond").sentence(), "first stderr line");
        assert_eq!(reply(1, "{\"status\":\"error\",\"message\":\"\"}", "from stderr").sentence(), "from stderr");
        assert_eq!(reply(1, "plain words\nmore", "").sentence(), "plain words", "stdout is used when it was not an envelope");
        assert_eq!(reply(1, "{\"status\":\"error\"}", "").sentence(), "aoide exited with code 1", "an envelope's braces are not a sentence");
        assert_eq!(reply(-1, "", "").sentence(), "aoide was stopped by a signal");
        assert_eq!(reply(1, &format!("{{\"message\":\"{}\"}}", "m".repeat(5000)), "").sentence().len(), 1000);
    }

    #[test]
    fn a_debug_print_never_carries_the_words() {
        let secret = "the-secret-words";
        let shape = send(node("osaka"), Some("lab"), secret, false, secret);
        let shown = format!("{shape:?} {:?}", Error::Refused("x".into()));
        assert!(!shown.contains(secret), "{shown}");
        assert!(shown.contains("text_bytes") && shown.contains("osaka"), "{shown}");
        let failed = reply(1, &format!("{{\"status\":\"error\",\"message\":\"{secret}\"}}"), secret);
        let shown = format!("{:?}", Error::Failed(failed));
        assert!(!shown.contains(secret), "{shown}");
    }

    #[test]
    fn the_verbs_are_the_five_and_none_of_them_dials_a_node() {
        // The whole vocabulary, as its argv: the words before the first flag are the verb, and the verb is one of these. `node list`,
        // `session`, `pair`, `spawn`, `allow`, `node status`, `node pull` and `dispatch` are not among them and have no variant.
        let all = [
            Shape::Version,
            Shape::MeshJson,
            Shape::MailOutboxJson,
            Shape::MailRead { reread: true },
            send(To::SelfNode, None, "s", false, "t"),
            send(node("osaka"), Some("lab"), "s", true, "t"),
        ];
        for shape in &all {
            let words = argv(shape);
            let verb: Vec<&str> = words.iter().map(String::as_str).take_while(|word| !word.starts_with('-')).collect();
            assert!(
                matches!(verb.as_slice(), [] | ["mesh"] | ["mail", "outbox"] | ["mail", "read"] | ["mail", "send"]),
                "{shape:?} runs {verb:?}: {words:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_relative_binary_is_refused_and_nothing_is_looked_up() {
        for bin in ["aoide", "./aoide", "bin/aoide", ""] {
            let outcome = Aoide::new(bin).run(&Shape::Version).await;
            assert!(matches!(&outcome, Err(Error::Refused(why)) if why.contains("PATH")), "{bin:?}: {outcome:?}");
        }
    }

    #[tokio::test]
    async fn a_missing_binary_is_a_sentence_and_not_a_panic() {
        let outcome = Aoide::new("/nonexistent/aoide").run(&Shape::Version).await;
        assert!(matches!(&outcome, Err(Error::Spawn(why)) if why.contains("/nonexistent/aoide")), "{outcome:?}");
        assert!(outcome.unwrap_err().to_string().starts_with("aoide could not be started"));
    }
}
