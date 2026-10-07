//! The guard rows (slice G1): the deny rows the perch adds to the policies it generates, and what it tells the page about them.
//!
//! ```text
//!   rows.rn          the Rune text of the rows, fixed: nothing from a request or a file is ever pasted into it
//!   wrap(stock)      the member's table with its classify_program renamed and a classify_program that adds the rows
//!   stage()          writes that as <the member's policy dir>/minerva-guard-<8 hex of the home>.rn (0600, atomically, every start)
//!   probe()          `eidolon --config <home>/config.toml policy -- aoide mail send x` must answer Deny with the guard's own sentence
//!   status()         whether the rows are in the config chats start with, which file, and why not
//!   refuses_term_text(text)   the same word check, in Rust, for the text an agent types into a terminal (a hint, not a gate)
//! ```
//!
//! **Why beside the member's policy.** eidolon keeps its permits store (`policy.permits.rn`: the vouches `eidolon plugins trust` writes and
//! the credential grants `eidolon plugins grant` writes) next to whichever policy file is in force. A guard written in the Minerva home
//! would have moved auto, yolo and every run onto an empty store, so the installer's trust would have stopped counting. Written next to the
//! member's own policy, `beside()` resolves to the very store the installer and `eidolon plugins trust` write, and nothing is ever
//! re-applied. It is the one Minerva file the perch puts in that directory, named for its home (two perches over one eidolon config do not
//! overwrite each other's file, and one failing never removes the other's).
//!
//! **What the rows are.** A deny for the aoide CLI, for `ssh` (and `sshpass`, `autossh`, `mosh`), for changing update keys, and for any
//! word that names the update-keys directory, judged on the program a command really runs (past `env`, `nohup`, `timeout`, shell keywords,
//! ...) and on the code string of `bash -c`, `pwsh -Command`, `cmd /c` and the like. See `rows.rn` for the exact rules. Every other verdict
//! is the member's table's, unchanged: only `classify_program` is renamed, so eidolon's `policy-rows` still reads the member's own rows.
//!
//! **What they reach, and what they do not.** The policy hands the rows one simple command at a time: a flat command, each stage of a
//! pipeline, each part of a `&&` / `||` / `;` chain, the body of a `while` loop, and one level of `$(...)`. A compound shape that eidolon's
//! decomposition judges as a whole never reaches them: `for`, `if` and `case`, a subshell `( ... )`, a `{ ...; }` group, a function body, a
//! command in the background (`... &`) and a redirect that leaves the workspace. Those are a question in auto and a yes in yolo, so in yolo
//! `for h in a b; do ssh $h uptime; done` runs. The file verbs (`cp`, `mv`, `tee`, ...) are the algebra's own "workspace file op" and never
//! reach the table at all. And the rows are in the config of the chat's OWN tool calls only: a subagent's children, and
//! any eidolon an agent starts itself (`eidolon -p ...`), run under the member's config, without them. `tests/guard.rs` pins the shapes
//! measured, so an eidolon that starts to reach them shows up as a failing test. A refusal that does reach the rows is a deny in every mode:
//! yolo answers questions, never refusals.
//!
//! **When it is off.** The perch still starts, and config.toml is the member's own (today's behavior), when the table cannot be wrapped
//! (an entry point not found exactly once after an eidolon update), the directory cannot be written (a policy in the nix store), or the
//! start-up probe does not answer Deny with the guard's sentence from the guard's file. [`status`] says which, in one sentence the page
//! shows as "guard rows are off: <why>". Yolo is the default mode, so a broken guard never makes chats unavailable; it makes them
//! unguarded and says so.
//!
//! **A speed bump, not a boundary.** A script on disk, base64 or any other disguise gets past a table that reads command lines. eidolon
//! has no sandbox and agents run as the operator; the rows stop the honest mistake and the obvious request, and that is all they claim.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

/// The sentences a refusal carries back to the agent. `rows.rn` holds the same text (a test pins that they agree).
pub const REASON_AOIDE: &str = "Minerva: agents send Aoide mail with mail_send (opt-in per chat); the aoide CLI is not for agents here";
pub const REASON_SSH: &str = "Minerva: ssh through a terminal, term_open then term_send, so the operator watches live";
pub const REASON_KEYS: &str = "Minerva: update keys are added by the operator, from the page or a terminal, not by agents";
pub const REASON_KEYFILE: &str = "Minerva: the update-keys directory is the operator's; keys are added or removed from the page or a terminal, not through files an agent writes";

/// How long the start-up probe may take before the guard is declared off. A hung eidolon must not hang the perch, and the installer's
/// `perch-open.sh` gives the perch 20 s to come up.
const PROBE_LIMIT: Duration = Duration::from_secs(6);

/// Whether the guard rows are in the config a chat starts with, which file holds them, and why not when they are off. The loaded tab
/// shows it through `/hub/meta/policy`. `why` is the bare reason when `on` is false ("guard rows are off: <why>"); when `on` is true it
/// is empty, or a note the operator should read (a member whose policy says `enabled = false` gets eidolon's gate back under Minerva).
///
/// `on` is about the config only: the rows are in the config of this perch's own chats, and they reach the simple commands eidolon's
/// decomposition hands the table. It does not say that nothing else can run `ssh` (see the module docs for what the rows do not reach).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GuardStatus {
    pub on: bool,
    pub file: Option<PathBuf>,
    pub why: String,
}

impl GuardStatus {
    pub fn off(why: impl Into<String>) -> Self {
        Self { on: false, file: None, why: why.into() }
    }

    pub fn on(file: PathBuf, note: Option<String>) -> Self {
        Self { on: true, file: Some(file), why: note.unwrap_or_default() }
    }
}

/// What the last start settled. `None` until a start has: a perch that never got as far as the config says so.
static STATUS: Mutex<Option<GuardStatus>> = Mutex::new(None);

/// The guard's status as of the last start: whether this perch's chats were given the rows in their config (not whether every command
/// is caught; see the module docs).
pub fn status() -> GuardStatus {
    STATUS.lock().unwrap().clone().unwrap_or_else(|| GuardStatus::off("the perch has not set the guard up"))
}

/// Remembers `status` for [`status`], and says it in the log when the guard is off (an error, because the default log level would hide
/// a warning, and a missing guard is the thing nobody should learn about only from the page).
pub(crate) fn record(status: GuardStatus) {
    if !status.on {
        tracing::error!(why = %status.why, "perch: guard rows are off");
    }
    *STATUS.lock().unwrap() = Some(status);
}

/// [`record`] for a status that is not the last word (a config generated before eidolon is in hand): kept for [`status`], logged at debug.
pub(crate) fn record_provisional(status: GuardStatus) {
    tracing::debug!(on = status.on, why = %status.why, "perch: guard status, before eidolon was asked");
    *STATUS.lock().unwrap() = Some(status);
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

const ROWS_SRC: &str = include_str!("rows.rn");

/// The Rune of the rows: constants and helper functions, all named `minerva_guard_*` / `MINERVA_GUARD_*` so they share a namespace
/// with the member's table and eidolon's permits store (`policy.permits.rn` is compiled in front of the table) without a clash.
/// Line ends are LF whatever the checkout did to the source file.
pub fn rows() -> String {
    ROWS_SRC.replace("\r\n", "\n")
}

/// The guard file's own entry point: `classify_program` with the rows in front of the member's. The other three entry points are the
/// member's table's own, unrenamed, so eidolon (and `policy-rows`) reads them where it always did.
const WRAPPER: &str = r#"
// ===========================================================================
// minerva guard: generated by minerva perch from your policy, whose
// classify_program is renamed minerva_stock_classify_program above. Every
// verdict is the table's own except the refusals in the rows. Do not edit
// this file; it is rewritten at every perch start.
// ===========================================================================

pub fn classify_program(argv, ctx) {
    let refused = minerva_guard_words(argv, 0);
    if refused != "" {
        return #{decision: "deny", reason: refused, read_only: false};
    }
    minerva_stock_classify_program(argv, ctx)
}
"#;

/// The guard file's text: `stock` with its `classify_program` renamed, the rows, and the wrapper. Fails like the manual wrapper does,
/// when any of the four entry points is not found exactly once at the start of a line, and when `stock` is already a table Minerva
/// generated (a guard that wrapped a guard would hide the first one's rows behind the second's names).
pub fn wrap(stock: &str) -> Result<String> {
    let mut out = crate::mode::rename_program_entry(stock)?;
    out.push_str(&rows());
    out.push_str(WRAPPER);
    Ok(out)
}

/// The guard file's name for `home`: `minerva-guard-<8 hex of sha256(canonical home)>.rn`. Two homes over one eidolon config each have
/// a file of their own, in the one directory, so `beside()` still finds the one shared permits store.
pub fn file_name(home: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let canonical = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let digest = harnox::crypto::hex(&harnox::crypto::sha256(canonical.as_os_str().as_bytes()));
    format!("minerva-guard-{}.rn", &digest[..8])
}

/// A guard file that was written, and the member's table it was made from (to notice when eidolon changes that table underneath it).
#[derive(Clone, Debug)]
pub struct Staged {
    pub file: PathBuf,
    pub stock: String,
}

/// Writes the guard file beside the member's policy and returns it. `eidolon` lets a first run unpack eidolon's tables (it is what
/// `policy true` does); without it only a table already on disk can be wrapped. `Err` is the bare reason the guard is off.
pub fn stage(member: &Path, config: &Path, home: &Path, eidolon: Option<&Path>) -> std::result::Result<Staged, String> {
    let policy = crate::mode::policy_file(member)?;
    let dir = policy.parent().filter(|dir| !dir.as_os_str().is_empty()).ok_or_else(|| format!("{} has no directory", policy.display()))?;
    // On the default path the directory is eidolon's own config directory, which a first run has not made yet: eidolon makes it when it
    // unpacks its tables, so that comes before the directory is looked for. Any other path that names nowhere is the member's mistake,
    // not a place to create.
    let mut stock = None;
    if !dir.is_dir()
        && member.parent() == Some(dir)
        && let Some(eidolon) = eidolon
    {
        stock = Some(crate::mode::policy_in_force(member, config, eidolon).map_err(|e| format!("{e:#}")));
    }
    if !dir.is_dir() {
        return Err(match stock {
            Some(Err(why)) => why,
            _ => format!("the policy directory {} does not exist", dir.display()),
        });
    }
    let stock = match stock {
        Some(done) => done?,
        None => match eidolon {
            Some(eidolon) => crate::mode::policy_in_force(member, config, eidolon),
            None => crate::mode::read_stock(member),
        }
        .map_err(|e| format!("{e:#}"))?,
    };
    stage_text(dir, home, stock)
}

/// Wraps `stock` and writes the result as `home`'s guard file in `dir`.
fn stage_text(dir: &Path, home: &Path, stock: String) -> std::result::Result<Staged, String> {
    let text = wrap(&stock).map_err(|e| format!("{e:#}"))?;
    let file = dir.join(file_name(home));
    harnox::fs::write_atomic_0600(&file, text.as_bytes()).map_err(|e| format!("{} cannot be written: {e}", file.display()))?;
    Ok(Staged { file, stock })
}

/// After a passing probe: eidolon syncs its shipped tables when it starts, so the member's table may be newer than the one the guard was
/// made from. `None` when it is the same; otherwise the guard file rewritten from the table now on disk, to be probed once more.
pub fn restage_if_changed(member: &Path, home: &Path, staged: &Staged) -> Option<std::result::Result<Staged, String>> {
    let now = crate::mode::read_stock(member).ok()?;
    if now == staged.stock {
        return None;
    }
    let dir = staged.file.parent()?;
    Some(stage_text(dir, home, now))
}

/// The start-up probe: `eidolon --config <config> policy -- aoide mail send x` must be refused with the guard's own sentence by the guard's
/// own file. A guard that compiles but lets every verdict through, a table eidolon would not load, or a config that names some other
/// policy is caught here and not discovered when an agent runs `aoide`.
pub fn probe(eidolon: &Path, config: &Path, cwd: &Path, file: &Path) -> std::result::Result<(), String> {
    let mut command = Command::new(eidolon);
    command.arg("--config").arg(config).args(["policy", "--", "aoide", "mail", "send", "x"]).current_dir(cwd);
    let out = run_bounded(command, PROBE_LIMIT).map_err(|e| format!("the start-up probe could not run eidolon: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let field = |name: &str| stdout.lines().find_map(|line| line.strip_prefix(name)).map(str::trim);
    match (out.status.success(), field("tier:")) {
        (true, Some("Deny")) => {
            if field("table:").map(Path::new) != Some(file) {
                return Err(format!("the start-up probe was answered by {}, not by the guard file {}", field("table:").unwrap_or("no table"), file.display()));
            }
            if field("reason:") != Some(REASON_AOIDE) {
                return Err("the start-up probe was refused, but not with the guard's sentence".to_string());
            }
            Ok(())
        }
        (true, Some(other)) => Err(format!("the start-up probe `aoide mail send x` was not refused (eidolon said {other})")),
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let line = stderr.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("").trim();
            Err(format!("the start-up probe got no verdict from `eidolon policy` ({}): {}", out.status, line.chars().take(200).collect::<String>()))
        }
    }
}

/// Runs `command` with no stdin and both outputs captured, and gives up after `limit`: the child is killed and the answer is an error.
/// For the commands the perch asks of eidolon at start-up, none of which may be able to hold the start.
pub(crate) fn run_bounded(mut command: Command, limit: Duration) -> std::result::Result<Output, String> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = command.spawn().map_err(|e| format!("{}: {e}", command.get_program().to_string_lossy()))?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(limit) {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            Err(format!("no answer within {} s", limit.as_secs()))
        }
    }
}

// ---------------------------------------------------------------------------
// The same rows, in Rust
// ---------------------------------------------------------------------------
//
// `rows.rn` is the policy's copy and this is the perch's: the text an agent types into a terminal is judged by the perch (T2), which links
// no Rune. The two are written to agree line for line, and tests/guard.rs runs one random corpus through both.

const WRAPPERS: [&str; 17] = [
    "env", "nohup", "timeout", "xargs", "nice", "stdbuf", "command", "exec", "setsid", "time", "ionice", "flock", "unbuffer", "chrt", "taskset", "watch", "strace",
];
const POSITIONAL: [&str; 4] = ["timeout", "flock", "chrt", "taskset"];
const SHELLS: [&str; 10] = ["sh", "bash", "zsh", "dash", "ksh", "fish", "ash", "csh", "tcsh", "script"];
const INTERPRETERS: [&str; 6] = ["perl", "ruby", "node", "nodejs", "deno", "php"];
const POWERSHELLS: [&str; 4] = ["pwsh", "pwsh.exe", "powershell", "powershell.exe"];
const CMDS: [&str; 2] = ["cmd", "cmd.exe"];
const KEYWORDS: [&str; 8] = ["!", "if", "then", "elif", "else", "do", "while", "until"];
const SSHLIKE: [&str; 5] = ["ssh", "ssh.exe", "sshpass", "autossh", "mosh"];
const MARKERS: [&str; 9] = ["system(", "exec(", "execSync(", "spawn(", "spawnSync(", "popen(", "os.system", "subprocess", "`"];

/// Code nests this deep and no further; a code string over `MAX_TEXT` bytes is not read; at most `MAX_FLAGS` code options of one command
/// and `MAX_MARKS` occurrences of one marker are judged; and one command line reads at most `MAX_WORK` bytes of code text in all.
/// Bounds on the work, and misses that are stated.
const MAX_DEPTH: usize = 6;
const MAX_TEXT: usize = 65536;
/// The bytes of code text one command line may have read in all, however its flags nest (see `minerva_guard_charge` in rows.rn).
const MAX_WORK: usize = 262144;
const MAX_FLAGS: usize = 16;
const MAX_MARKS: usize = 16;

/// How much of one `term_send` text is judged.
const TERM_TEXT_LIMIT: usize = 8 * 1024;

/// The last path component (either kind of separator), lower case.
fn base(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or("").to_lowercase()
}

/// The word after its first `n` characters.
fn skip(word: &str, n: usize) -> String {
    word.chars().skip(n).collect()
}

/// `NAME=value`, the prefix that sets a variable for one command.
fn is_assign(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'),
        None => false,
    }
}

/// Whether the wrapper's option `flag` takes the next word as its value.
fn valued(wrapper: &str, flag: &str) -> bool {
    match wrapper {
        "env" => matches!(flag, "-u" | "-C" | "-S" | "--unset" | "--chdir" | "--split-string"),
        "timeout" => matches!(flag, "-s" | "-k" | "--signal" | "--kill-after"),
        "nice" => matches!(flag, "-n" | "--adjustment"),
        "stdbuf" => matches!(flag, "-i" | "-o" | "-e" | "--input" | "--output" | "--error"),
        "xargs" => matches!(
            flag,
            "-n" | "-P" | "-I" | "-L" | "-s" | "-d" | "-E" | "-a" | "-J" | "--max-args" | "--max-procs" | "--replace" | "--arg-file" | "--delimiter"
        ),
        "exec" => flag == "-a",
        "ionice" => matches!(flag, "-c" | "-n" | "-p"),
        "time" => matches!(flag, "-o" | "-f" | "--output" | "--format"),
        "flock" => matches!(flag, "-w" | "-E" | "--timeout" | "--conflict-exit-code"),
        "taskset" => matches!(flag, "-c" | "--cpu-list"),
        "watch" => matches!(flag, "-n" | "--interval"),
        "strace" => matches!(flag, "-e" | "-o" | "-p" | "-s" | "-u" | "-E" | "-a" | "-P" | "-S" | "-I" | "-O" | "-X" | "-b" | "-U"),
        _ => false,
    }
}

/// Where the program a command really runs starts in `words`, past `NAME=value` prefixes, shell keywords and the wrappers that only start
/// another command (with their own options, and the one positional `timeout`, `flock`, `chrt` and `taskset` take), and the values `env -S`
/// was given, which are command lines. The index is `words.len()` or more when nothing runs.
fn start(words: &[String], from: usize) -> (usize, Vec<String>) {
    let mut texts = Vec::new();
    let mut i = from;
    while i < words.len() {
        let w = words[i].as_str();
        if is_assign(w) || KEYWORDS.contains(&w) {
            i += 1;
            continue;
        }
        let b = base(w);
        if !WRAPPERS.contains(&b.as_str()) {
            return (i, texts);
        }
        i += 1;
        let mut positional = POSITIONAL.contains(&b.as_str());
        while i < words.len() {
            let o = words[i].as_str();
            if o == "--" {
                i += 1;
                break;
            }
            if o.len() < 2 || !o.starts_with('-') {
                break;
            }
            if b == "command" && (o == "-v" || o == "-V") {
                return (words.len(), texts);
            }
            i += 1;
            if b == "env" {
                if o == "-S" || o == "--split-string" {
                    if i < words.len() {
                        texts.push(words[i].clone());
                        i += 1;
                    }
                    continue;
                }
                if o.starts_with("--split-string=") {
                    texts.push(skip(o, 15));
                    continue;
                }
                if o.starts_with("-S") && !o.starts_with("--") {
                    texts.push(skip(o, 2));
                    continue;
                }
            }
            if b == "taskset" && (o == "-c" || o == "--cpu-list") {
                positional = false;
            }
            if valued(&b, o) {
                i += 1;
            }
        }
        if positional && i < words.len() {
            i += 1;
        }
    }
    (i, texts)
}

fn join(words: &[String], from: usize) -> String {
    words.get(from..).unwrap_or_default().join(" ")
}

fn later(words: &[String], from: usize, word: &str) -> bool {
    words.iter().skip(from).any(|w| w == word)
}

/// `hoot`, `hoot.ps1`, `hoot.cmd` or a path ending in one of them.
fn is_hoot(b: &str) -> bool {
    b == "hoot" || b.ends_with("hoot.ps1") || b.ends_with("hoot.cmd")
}

/// `hoot keys` (or `hoot.ps1 keys`) as a program or as the script of a runner: a hoot word with `keys` after it.
fn hoot_keys(words: &[String], from: usize) -> bool {
    (from..words.len()).any(|i| is_hoot(&base(&words[i])) && later(words, i + 1, "keys"))
}

/// 1 for a shell, 2 for a language that takes its program as an option (python, perl, node), 3 for powershell, 4 for cmd, 0 for the rest.
fn flavor(prog: &str) -> u8 {
    if SHELLS.contains(&prog) {
        1
    } else if prog.starts_with("python") || INTERPRETERS.contains(&prog) {
        2
    } else if POWERSHELLS.contains(&prog) {
        3
    } else if CMDS.contains(&prog) {
        4
    } else {
        0
    }
}

/// Whether `flag` is an option after which the next word is program text (see `minerva_guard_code_flag` in rows.rn).
fn code_flag(flavor: u8, flag: &str) -> bool {
    if flavor == 1 {
        if flag == "--command" {
            return true;
        }
        return flag.len() >= 2 && flag.starts_with('-') && !flag.starts_with("--") && flag.contains('c');
    }
    let f = flag.to_lowercase();
    match flavor {
        2 => {
            if matches!(f.as_str(), "--command" | "--eval" | "--print") {
                return true;
            }
            if f.len() < 2 || !f.starts_with('-') || f.starts_with("--") {
                return false;
            }
            f.ends_with('c') || f.ends_with('e') || f.ends_with('r') || f.ends_with('p')
        }
        3 => f.len() >= 2 && f.starts_with('-') && !f.starts_with("--") && "-command".starts_with(f.as_str()),
        _ => matches!(f.as_str(), "/c" | "/k" | "/r"),
    }
}

/// `text` as shell words in command segments (see `minerva_guard_segments` in rows.rn).
fn segments(text: &str, code: bool, first_only: bool) -> Vec<Vec<String>> {
    let (mut segments, mut words, mut word) = (Vec::new(), Vec::new(), String::new());
    for c in text.chars() {
        if matches!(c, '\\' | '"' | '\'') {
            continue;
        }
        if c == ' ' || c == '\t' || (code && matches!(c, ',' | '[' | ']')) {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            continue;
        }
        let boundary = matches!(c, ';' | '&' | '|' | ')' | '`' | '\n' | '\r') || (!code && matches!(c, '(' | '{' | '}'));
        if boundary {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if !words.is_empty() {
                segments.push(std::mem::take(&mut words));
                if first_only {
                    return segments;
                }
            }
            continue;
        }
        word.push(c);
    }
    if !word.is_empty() {
        words.push(word);
    }
    if !words.is_empty() {
        segments.push(words);
    }
    segments
}

/// What one judgment carries down its recursion: whether rule 2 (ssh) applies, and the bytes of code text it may still read.
struct Ctx {
    ssh: bool,
    left: usize,
}

impl Ctx {
    fn new(ssh: bool) -> Self {
        Self { ssh, left: MAX_WORK }
    }

    /// Takes `n + 1` from the work budget, or says there is not that much left (`minerva_guard_charge`).
    fn charge(&mut self, n: usize) -> bool {
        match self.left.checked_sub(n + 1) {
            Some(left) => {
                self.left = left;
                true
            }
            None => false,
        }
    }
}

fn refused_text(text: &str, depth: usize, cx: &mut Ctx) -> Option<&'static str> {
    if depth > MAX_DEPTH || text.len() > MAX_TEXT || !cx.charge(text.len()) {
        return None;
    }
    // The key directory named anywhere in a shell text, before the backslashes of a Windows path are read as escapes.
    if text.to_lowercase().replace('\\', "/").contains("minerva/update-keys") {
        return Some(REASON_KEYFILE);
    }
    segments(text, false, false).iter().find_map(|words| refused_words(words, depth + 1, cx))
}

/// `s` without its leading run of the characters a call opens with, and with `ident` also without an identifier tail first.
fn lead(s: &str, ident: bool) -> String {
    let mut chars = s.chars().peekable();
    if ident {
        while chars.next_if(|c| c.is_alphanumeric() || matches!(c, '_' | '.')).is_some() {}
    }
    while chars.next_if(|c| matches!(c, ' ' | '\t' | '(' | '[' | '{' | '"' | '\'' | ',')).is_some() {}
    chars.collect()
}

/// A command a language's code string starts, judged (see `minerva_guard_code` in rows.rn).
fn refused_code(text: &str, depth: usize, cx: &mut Ctx) -> Option<&'static str> {
    if depth > MAX_DEPTH || text.len() > MAX_TEXT || !cx.charge(text.len()) {
        return None;
    }
    for marker in MARKERS {
        let mut rest = text;
        for _ in 0..MAX_MARKS {
            let Some((_, after)) = rest.split_once(marker) else { break };
            rest = after;
            let tail = lead(after, marker == "subprocess");
            for words in segments(&tail, true, true) {
                if let Some(reason) = refused_words(&words, depth + 1, cx) {
                    return Some(reason);
                }
            }
        }
    }
    None
}

/// The sentence if one command (a program and its arguments) is refused, else `None`. `ssh` is whether rule 2 applies: a leaf the
/// policy judges, yes; the text an agent types into a terminal, no, because ssh from a terminal is the intended way.
fn refused_words(words: &[String], depth: usize, cx: &mut Ctx) -> Option<&'static str> {
    if depth > MAX_DEPTH {
        return None;
    }
    let (p, texts) = start(words, 0);
    if let Some(reason) = texts.iter().find_map(|t| refused_text(t, depth, cx)) {
        return Some(reason);
    }
    if p >= words.len() {
        return None;
    }
    let first = words[p].as_str();
    // A program word with a blank in it is a command line quoted as one word (`watch "ssh h"`).
    if (first.contains(' ') || first.contains('\t')) && let Some(reason) = refused_text(first, depth, cx) {
        return Some(reason);
    }
    let prog = base(first);
    let prog = prog.as_str();

    let arm = words.iter().any(|w| w == "--keys-arm" || w.starts_with("--keys-arm="));
    let opener = words.iter().any(|w| base(w) == "perch-open.sh");
    let keyfile = words.iter().any(|w| w.to_lowercase().replace('\\', "/").contains("minerva/update-keys"));

    if matches!(prog, "aoide" | "aoide.exe" | "aoided" | "aoided.exe") {
        return Some(REASON_AOIDE);
    }
    if cx.ssh && SSHLIKE.contains(&prog) {
        return Some(REASON_SSH);
    }
    if keyfile {
        return Some(REASON_KEYFILE);
    }

    let minerva = matches!(prog, "minerva" | "minerva.exe");
    if minerva && words.get(p + 1).is_some_and(|w| w == "keys") {
        let verb = words[p + 2..].iter().find(|w| !w.starts_with('-'));
        if verb.is_some_and(|verb| verb != "list" && verb != "inspect") {
            return Some(REASON_KEYS);
        }
    }
    if arm && (opener || minerva) {
        return Some(REASON_KEYS);
    }

    let kind = flavor(prog);
    if (is_hoot(prog) || kind > 0) && hoot_keys(words, p) {
        return Some(REASON_KEYS);
    }

    if kind > 0 {
        let mut seen = 0;
        let mut k = p + 1;
        while k < words.len() && seen < MAX_FLAGS {
            if code_flag(kind, &words[k]) {
                seen += 1;
                let from = if words.get(k + 1).is_some_and(|w| w == "--") { k + 2 } else { k + 1 };
                let text = join(words, from);
                let reason = if kind == 2 { refused_code(&text, depth, cx) } else { refused_text(&text, depth, cx) };
                if reason.is_some() {
                    return reason;
                }
            }
            k += 1;
        }
        return None;
    }
    if prog == "eval" {
        return refused_text(&join(words, p + 1), depth, cx);
    }
    if prog == "find" {
        let mut k = p + 1;
        while k < words.len() {
            if matches!(words[k].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                let mut m = k + 1;
                while m < words.len() && words[m] != ";" && words[m] != "+" {
                    m += 1;
                }
                if let Some(reason) = refused_words(&words[k + 1..m], depth + 1, cx) {
                    return Some(reason);
                }
                k = m + 1;
            } else {
                k += 1;
            }
        }
    }
    None
}

/// The sentence if the leaf `argv` is one the policy refuses (every row), else `None`. What `rows.rn` computes, for the tests that hold
/// the two to the same answers.
pub fn refuses_argv(argv: &[String]) -> Option<&'static str> {
    refused_words(argv, 0, &mut Ctx::new(true))
}

/// The sentence if the code string `text` (what `bash -c` is given) holds a command the policy refuses, every row.
pub fn refuses_text(text: &str) -> Option<&'static str> {
    refused_text(text, 0, &mut Ctx::new(true))
}

/// The sentence if the code string `text` is a language's (what `python -c` or `perl -e` is given), judged where it starts a command.
pub fn refuses_code(text: &str) -> Option<&'static str> {
    refused_code(text, 0, &mut Ctx::new(true))
}

/// The guard's sentence when one `term_send` text names something an agent may not run: the aoide CLI, a change to the update keys, or a
/// word that names the update-keys directory. `ssh` is not refused here, because ssh through a terminal is the way Minerva wants it done.
/// The first 8 KiB of the text is read as shell words, and the program of each command is judged (so `echo aoide` and `ls ~/src/aoide`
/// pass, `cd x && aoide send` and `bash -c "aoide pair"` do not). It does not depend on whether eidolon's policy accepted the rows: the
/// perch does this one itself.
///
/// It is a hint, not a gate. It judges one text: a command split across sends (`ao` then `ide`), typed with DEL or tab completion, built
/// by a variable or read from a file, or sent after the first 8 KiB, passes. The refusal that matters is the policy's.
pub fn refuses_term_text(text: &str) -> Option<&'static str> {
    let mut end = text.len().min(TERM_TEXT_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    refused_text(&text[..end], 0, &mut Ctx::new(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn the_sentences_are_plain_text_and_the_rows_say_the_same_thing() {
        for reason in [REASON_AOIDE, REASON_SSH, REASON_KEYS, REASON_KEYFILE] {
            assert!(!reason.contains(['"', '\\', '`', '$', '\n']), "{reason}: nothing in it needs escaping in a Rune string");
            assert!(rows().contains(&format!("= \"{reason}\";")), "rows.rn holds: {reason}");
        }
    }

    #[test]
    fn the_rows_are_fixed_text_that_names_only_its_own_functions() {
        // The wrapper takes nothing from outside: two different stock tables give the same appended text.
        let stock = |extra: &str| format!("pub fn classify_tool(a, b, c) {{ 1 }}\npub fn classify_program(a, b) {{ 1 }}\npub fn fs_verb_tier(a) {{ 1 }}\npub fn bash_default(a) {{ 1 }}\n{extra}");
        let (one, two) = (wrap(&stock("")).unwrap(), wrap(&stock("// {x}\n")).unwrap());
        let appended = format!("{}{WRAPPER}", rows());
        assert!(one.ends_with(&appended) && two.ends_with(&appended));
        // Every helper the rows define has the prefix, so none can shadow the member's names or the permits store's.
        for line in rows().lines().filter(|l| l.starts_with("fn ") || l.starts_with("const ")) {
            assert!(line.starts_with("fn minerva_guard_") || line.starts_with("const MINERVA_GUARD_"), "{line}");
        }
    }

    #[test]
    fn a_guard_renames_only_classify_program_and_is_never_wrapped_in_a_guard() {
        let stock = "pub fn classify_tool(a, b, c) { 1 }\npub fn classify_program(a, b) { 1 }\npub fn fs_verb_tier(a) { 1 }\npub fn bash_default(a) { 1 }\n";
        let guard = wrap(stock).unwrap();
        assert!(wrap(&guard).is_err(), "the output of wrap is not an input to it");
        assert!(wrap("pub fn classify_tool(a, b, c) { 1 }").is_err(), "three entry points missing: nothing is guessed");
        assert!(wrap(&(stock.to_string() + "pub fn bash_default(p) { p }\n")).is_err(), "a name twice is a guess");
        let count = |prefix: &str| guard.lines().filter(|l| l.starts_with(prefix)).count();
        assert_eq!((count("pub fn classify_program("), count("fn minerva_stock_classify_program(")), (1, 1), "the wrapper's, and the member's renamed");
        for entry in ["classify_tool", "fs_verb_tier", "bash_default"] {
            assert_eq!(count(&format!("pub fn {entry}(")), 1, "{entry} is the member's own, where eidolon reads it");
            assert_eq!(count(&format!("fn minerva_stock_{entry}(")), 0, "{entry} is not renamed");
        }
    }

    #[test]
    fn the_file_is_named_for_its_home_and_two_homes_differ() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (one, two) = (file_name(a.path()), file_name(b.path()));
        for name in [&one, &two] {
            let hex = name.strip_prefix("minerva-guard-").and_then(|n| n.strip_suffix(".rn")).unwrap();
            assert!(hex.len() == 8 && hex.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)), "{name}");
        }
        assert_ne!(one, two);
        assert_eq!(file_name(a.path()), one, "stable");
        let link = b.path().join("alias");
        std::os::unix::fs::symlink(a.path(), &link).unwrap();
        assert_eq!(file_name(&link), one, "the canonical home, so a symlink to it is the same home");
    }

    #[test]
    fn the_word_check_judges_the_program_and_not_a_word_in_an_argument() {
        for line in [
            "aoide mail send x", "env aoide send --id x --submit", "/home/u/.local/bin/aoide node spawn", "timeout 5 aoide pair", "FOO=bar aoide x",
            "nohup aoide pair", "nice -n 10 aoide x", "stdbuf -o L aoide x", "xargs -n 1 aoide x", "xargs -I {} aoide {}", "exec -a x aoide pair",
            "env -i -u FOO BAR=1 aoide x", "timeout -s KILL 5 aoide x", "timeout --signal=KILL 5 env aoide x", "AOIDE.EXE x", "./aoided",
            "bash -c aoide mail read", "sh -lc aoide pair", "find . -exec aoide x", "eval aoide x", "python3 -c os.system('aoide pair')", "perl -e system(\"aoide\")",
            "C:\\bin\\Aoide.exe x", "flock /tmp/l aoide x", "strace -o /tmp/t aoide x", "watch -n 5 aoide x", "unbuffer aoide x", "chrt -f 10 aoide x",
            "taskset 0x1 aoide x", "taskset -c 0,1 aoide x", "env --chdir /tmp aoide x", "timeout --kill-after 3 5 aoide x", "nice --adjustment 5 aoide x",
            "stdbuf --output L aoide x", "xargs --max-args 1 aoide x", "xargs --replace {} aoide {}", "time -o /tmp/t aoide x", "time --format %e aoide x",
            "env -S aoide pair", "env --split-string=aoide pair", "env -Saoide x", "watch aoide pair", "bash -e -c aoide pair", "bash -cx aoide pair",
            "bash -c -- aoide pair", "pwsh -NoProfile -Command aoide pair", "powershell.exe -NoProfile -NonInteractive -Command aoide mail send x",
            "cmd /c aoide pair", "cmd.exe /K aoide pair", "script -qc aoide pair /dev/null", "! aoide x", "if aoide x", "while aoide x", "do aoide x",
        ] {
            assert_eq!(refuses_argv(&argv(line)), Some(REASON_AOIDE), "{line}");
        }
        for line in [
            "ls ~/src/aoide", "grep -r aoide .", "echo aoide", "cat aoide.log", "cargo test -p aoide-core", "command -v aoide", "which aoide",
            "ssh-keygen -t ed25519", "scp a b:c", "rsync -e ssh a b:c", "echo ssh", "git clone git@github.com:a/b", "minerva keys list",
            "minerva keys", "minerva keys --json inspect k1", "minerva status --json", "grep -c aoide f", "sed -e s/aoide/x/ f", "python3 x.py",
            "python3 -c print(1)", "python3 -c x=f(ssh)", "node -e console.log(aoide)", "perl -e print(ssh)", "perl -ne print", "ruby -e puts(aoide)",
            "bash -e script.sh", "bash script.sh", "bash -x -v", "pwsh -NoProfile -File x.ps1", "cmd /d /q", "env -i ls", "flock /tmp/l ls aoide", "watch -n 5 ls",
            "timeout 5 sleep 1", "echo if ssh then", "php -r echo(ssh);", "python3 -c os.system('ls aoide')",
        ] {
            assert_eq!(refuses_argv(&argv(line)), None, "{line}");
        }
    }

    #[test]
    fn ssh_and_the_key_rows_have_their_own_sentences() {
        for line in [
            "ssh host", "env ssh -p 22 host", "/usr/bin/ssh h", "bash -c ssh h true", "command -p ssh h", "nohup ssh -N h", "sshpass -p x ssh h", "autossh -M 0 h",
            "mosh h", "pwsh -NoProfile -Command ssh host", "bash -e -c ssh host", "bash -cx ssh h", "bash -c -- ssh h", "env -S ssh h", "for h in a; do ssh h; done",
            "echo x; if ssh h true", "python3 -c subprocess.run(['ssh','h'])", "perl -e `ssh h`", "node -e execSync('ssh h')", "watch ssh h uptime",
        ] {
            // The last five are inputs whose sentences the shell check reads word by word.
            let line = line.replace("for h in a; do ssh h; done", "bash -c for h in a; do ssh h; done").replace("echo x; if ssh h true", "bash -c echo x; if ssh h true");
            assert_eq!(refuses_argv(&argv(&line)), Some(REASON_SSH), "{line}");
        }
        for line in [
            "minerva keys add k.pub --confirm F", "/opt/m/minerva keys remove k1", "minerva keys --confirm F add k", "minerva web --keys-arm F",
            "perch-open.sh --keys-arm F", "bash /x/bin/perch-open.sh --keys-arm F", "hoot keys allow-web F", "hoot.ps1 keys allow-web F",
            "powershell.exe -File hoot.ps1 keys allow-web F", "pwsh -Command hoot keys allow-web F", "bash -c hoot keys allow-web F", "env X=1 minerva keys add k",
            "bin/perch-open.sh --keys-arm 0F7A3A5C447EC7EF --port 48110 --token-file /t/x", "minerva web --keys-arm 0F7A3A5C447EC7EF --port 48110", "C:\\x\\hoot.ps1 keys allow-web F",
            "pwsh -File C:\\tools\\hoot.cmd keys allow-web F", "./my-hoot.ps1 keys allow-web F",
        ] {
            assert_eq!(refuses_argv(&argv(line)), Some(REASON_KEYS), "{line}");
        }
        for line in [
            "cp evil.pub /home/u/.config/minerva/update-keys/", "ls ~/.config/minerva/update-keys", "tee /h/.config/Minerva/Update-Keys/k.pub", "cat C:\\u\\minerva\\update-keys\\k.pub",
            "bash -c cp x ~/.config/minerva/update-keys/",
        ] {
            assert_eq!(refuses_argv(&argv(line)), Some(REASON_KEYFILE), "{line}");
        }
        assert_eq!(refuses_argv(&argv("echo hoot keys")), None, "a word in an argument");
        assert_eq!(refuses_argv(&argv("grep -rn update-keys installer")), None, "the bare name is not the directory");
    }

    #[test]
    fn term_text_refuses_aoide_and_keys_and_leaves_ssh_to_the_terminal() {
        for text in [
            "aoide mail send x\n", "cd ~/src && aoide pair", "echo hi | aoide send", "ls; aoide node spawn", "(aoide pair)", "$(aoide pair)", "`aoide pair`",
            "bash -c \"aoide mail read --for minerva\"", "a\"o\"ide x", "\\aoide x", "FOO=1 aoide x", "sudo -u x aoide x ; aoide y", "minerva keys add k.pub",
            "hoot keys allow-web F\n", "perch-open.sh --keys-arm F", "minerva web --keys-arm 0F7A3A5C447EC7EF\n",
            "bin/perch-open.sh --keys-arm 0F7A3A5C447EC7EF --port 48110 --token-file /t/x\n", "cp evil.pub ~/.config/minerva/update-keys/\n",
        ] {
            assert!(refuses_term_text(text).is_some(), "{text:?}");
        }
        for text in [
            "ls ~/src/aoide", "grep -r aoide .", "echo aoide", "ssh host", "ssh -p 22 user@host\n", "scp a b:c", "cd aoide-core && ls", "", "   ", ";;;", "minerva keys list",
            "sshpass -p x ssh h", "bash -c \"ssh h\"",
        ] {
            assert_eq!(refuses_term_text(text), None, "{text:?}");
        }
        assert_eq!(refuses_term_text("aoide x"), Some(REASON_AOIDE));
        assert_eq!(refuses_term_text("hoot keys allow-web F"), Some(REASON_KEYS));
        assert_eq!(refuses_term_text("cp x ~/.config/minerva/update-keys/"), Some(REASON_KEYFILE));
    }

    #[test]
    fn a_term_text_is_judged_to_8_kib_and_no_further() {
        let pad = "echo a; ".repeat(TERM_TEXT_LIMIT / 8);
        assert_eq!(pad.len(), TERM_TEXT_LIMIT);
        assert_eq!(refuses_term_text(&format!("{pad}aoide x")), None, "past the cap is a stated miss");
        assert_eq!(refuses_term_text(&format!("{}aoide x", &pad[..TERM_TEXT_LIMIT - 8])), Some(REASON_AOIDE), "inside it is read");
        // The cut falls inside a multi-byte character without a panic.
        let wide = "é".repeat(TERM_TEXT_LIMIT);
        assert_eq!(refuses_term_text(&wide), None);
        assert_eq!(refuses_term_text(&format!("{}aoide", "é".repeat(TERM_TEXT_LIMIT / 2 - 3))), None);
    }

    /// Nesting is read six levels deep and no further, a code string over 64 KiB is not read, and nothing recurses on the stack without
    /// that bound: a bound on the work, and misses that are stated (a speed bump, not a boundary).
    #[test]
    fn nothing_here_can_run_away_on_a_hostile_command_line() {
        let nest = |levels: usize| "bash -c ".repeat(levels) + "aoide";
        assert_eq!(refuses_term_text(&nest(3)), Some(REASON_AOIDE));
        assert_eq!(refuses_term_text(&nest(5000)), None, "and it returns");
        let long = "a ".repeat(20_000);
        assert_eq!(refuses_term_text(&long), None);
        assert_eq!(refuses_argv(&[]), None);
        for line in ["env", "timeout", "nice -n", "flock", "taskset -c", "env -S", "watch -n", "find -exec", "bash -c", "bash -c --", "--", "!"] {
            assert_eq!(refuses_argv(&argv(line)), None, "{line}");
        }
        assert_eq!(refuses_argv(&["".to_string(), "".to_string()]), None);
        // find -exec nested: forty levels with a harmless tail, and ten thousand with an aoide tail, on a small stack, and in good time.
        let started = std::time::Instant::now();
        let found = |levels: usize, tail: &str| ("find . -exec ".repeat(levels) + tail).split(' ').map(str::to_string).collect::<Vec<_>>();
        let (benign, hostile) = (found(40, "ls ;"), found(10_000, "aoide x ;"));
        let small = std::thread::Builder::new().stack_size(2 * 1024 * 1024).spawn(move || (refuses_argv(&benign), refuses_argv(&hostile))).unwrap().join().unwrap();
        assert_eq!(small, (None, None), "past the depth bound is a stated miss, and it returns");
        assert_eq!(refuses_argv(&found(3, "aoide x ;")), Some(REASON_AOIDE));
        // Many -exec in one find are read once each, not once per position.
        let wide: Vec<String> = ["find", "."].iter().map(|s| s.to_string()).chain((0..20_000).flat_map(|_| ["-exec", "ls", ";"].map(String::from))).collect();
        assert_eq!(refuses_argv(&wide), None);
        // The same text nested in code strings, a hundred thousand markers, a hundred thousand flags.
        assert_eq!(refuses_code(&"system(".repeat(100_000)), None);
        assert_eq!(refuses_code(&"`ls`".repeat(10_000)), None);
        let flags: Vec<String> = std::iter::once("bash".to_string()).chain((0..100_000).map(|_| "-c".to_string())).collect();
        assert_eq!(refuses_argv(&flags), None);
        assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
    }

    #[test]
    fn status_is_off_and_says_why_until_a_start_sets_it() {
        let off = GuardStatus::off("because");
        assert!(!off.on && off.file.is_none() && off.why == "because");
        let on = GuardStatus::on(PathBuf::from("/h/.config/eidolon/minerva-guard.rn"), None);
        assert!(on.on && on.file.as_deref() == Some(Path::new("/h/.config/eidolon/minerva-guard.rn")) && on.why.is_empty());
        assert_eq!(GuardStatus::on(PathBuf::from("/f"), Some("note".into())).why, "note");
    }

    /// The commands the perch asks of eidolon at start-up cannot hold the start: a hang is cut off at the limit, killed, and reported.
    #[test]
    fn a_command_that_hangs_is_killed_at_the_limit_and_one_that_answers_is_not() {
        let started = std::time::Instant::now();
        let mut hang = Command::new("sleep");
        hang.arg("60");
        let why = run_bounded(hang, Duration::from_secs(1)).unwrap_err();
        assert!(why.contains("no answer within 1 s"), "{why}");
        assert!(started.elapsed() < Duration::from_secs(20), "it did not wait for the child");
        let mut quick = Command::new("sh");
        quick.args(["-c", "echo out; echo err >&2; exit 3"]);
        let out = run_bounded(quick, Duration::from_secs(30)).unwrap();
        assert_eq!((out.status.code(), String::from_utf8_lossy(&out.stdout).trim(), String::from_utf8_lossy(&out.stderr).trim()), (Some(3), "out", "err"));
        let why = run_bounded(Command::new("/nonexistent/eidolon"), Duration::from_secs(1)).unwrap_err();
        assert!(why.contains("/nonexistent/eidolon"), "{why}");
    }

    #[test]
    fn the_probe_wants_the_guards_sentence_from_the_guards_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("minerva-guard-00000000.rn");
        let stub = |body: &str| {
            let path = dir.path().join("stub.sh");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
            path
        };
        let reply = |table: &str, tier: &str, reason: &str| format!("echo 'table:   {table}'; echo 'tier:    {tier}'; echo 'reason:  {reason}'");
        let ok = stub(&reply(file.to_str().unwrap(), "Deny", REASON_AOIDE));
        // `Text file busy` can follow a fresh write under parallel tests: a retry is the honest answer.
        let run = |path: &Path| (0..50).find_map(|_| match probe(path, Path::new("/c.toml"), dir.path(), &file) { Err(e) if e.contains("busy") => { std::thread::sleep(Duration::from_millis(20)); None } done => Some(done) }).unwrap();
        assert_eq!(run(&ok), Ok(()));
        let other_table = stub(&reply("/elsewhere/policy.rn", "Deny", REASON_AOIDE));
        assert!(run(&other_table).unwrap_err().contains("not by the guard file"));
        let built_in = stub(&reply("built in", "Deny", REASON_AOIDE));
        assert!(run(&built_in).unwrap_err().contains("not by the guard file"));
        let other_reason = stub(&reply(file.to_str().unwrap(), "Deny", "a stock refusal"));
        assert!(run(&other_reason).unwrap_err().contains("not with the guard's sentence"));
        let allow = stub(&reply(file.to_str().unwrap(), "Allow", "read"));
        assert!(run(&allow).unwrap_err().contains("was not refused"));
    }
}
