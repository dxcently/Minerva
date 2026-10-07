//! The guard rows (slice G1): the deny rows in the policies the perch generates, `perch::guard::status()` and the term-text word check.
//!
//! Three layers, because the question has three halves:
//!
//! - **The tables, compiled with Rune** (always runs): eidolon's shipped `policy.rn` (read from `../eidolon/crates/rune/policy.rn`, never
//!   copied) wrapped by [`perch::guard::wrap`] (the auto/yolo guard file) and by [`perch::mode::wrap`] (manual), called the way the
//!   harness calls them. The deny corpus is refused, the allow corpus and every other verdict are the stock table's, and one random corpus
//!   goes through the Rune and through the Rust copy of the rows (`refuses_argv`, `refuses_text`) and must agree.
//! - **The real eidolon** (`MINERVA_TEST_EIDOLON=/abs/path`, else the first `eidolon` on `PATH`; with `MINERVA_REQUIRE_EIDOLON=1` and none,
//!   the tests FAIL instead of passing without having looked; each one that does run prints `REAL-EIDOLON-RAN <test>` so a run can be
//!   counted with `--nocapture`): `eidolon policy` over the generated configs, `policy-rows`, the shapes the rows are known to miss (pinned),
//!   a start with a stock table that cannot be wrapped, a directory that cannot be written, a probe that does not see a refusal, a member
//!   whose gate is off, and the permits store: `eidolon plugins trust/grant` against the temp default config, then a real workflow run
//!   under the guarded config and under a run-shaped one (R1a's derivation), where an ask would be declined for want of a person.
//! - **The term-text check** (`refuses_term_text`).
//!
//! Every eidolon this file starts goes through a wrapper script that pins HOME and every XDG_ directory to a temp tree (and asserts it is
//! under the temp dir), the swarm is off in the member config, and nothing here names a provider but `mock`. The operator's
//! `~/.config/eidolon`, `~/Minerva` and `~/.aoide` are never read or written.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use rune::termcolor::{ColorChoice, StandardStream};
use rune::{Context, Diagnostics, Source, Sources, Vm};

use perch::guard::{REASON_AOIDE, REASON_KEYFILE, REASON_KEYS, REASON_SSH};

// ---------------------------------------------------------------------------
// Rune
// ---------------------------------------------------------------------------

fn stock_table() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../eidolon/crates/rune/policy.rn");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading eidolon's shipped table at {}: {e}", path.display()))
}

/// A compiled table, called the way the harness calls it. Compiling it must produce no diagnostics at all: a warning in a policy the
/// member's eidolon runs is a table that may not load.
struct Table {
    vm: Vm,
}

/// Probes the table is called through, because a Rust caller cannot build the `#{}` the table takes as its third argument.
const PROBES: &str = r#"
pub fn probe_tool(tool, approval) {
    let r = classify_tool(tool, approval, #{});
    `${r.decision}|${r.read_only}|${r.reason}`
}
pub fn probe_program(argv) {
    let r = classify_program(argv, #{rust: true, python: true, node: true, nix: true, posture: "allow"});
    `${r.decision}|${r.read_only}|${r.reason}`
}
pub fn probe_fs(prog) { fs_verb_tier(prog) }
pub fn probe_default(posture) { bash_default(posture) }
"#;

/// Probes of the rows themselves, for the tables that carry them (not the stock one).
const ROW_PROBES: &str = r#"
pub fn probe_words(argv) { minerva_guard_words(argv, 0) }
pub fn probe_text(text) { minerva_guard_text(text, 0, [262144]) }
"#;

impl Table {
    fn new(src: &str) -> Self {
        let context = Context::with_default_modules().unwrap();
        let mut sources = Sources::new();
        sources.insert(Source::new("policy", src).unwrap()).unwrap();
        let mut diagnostics = Diagnostics::new();
        let unit = rune::prepare(&mut sources).with_context(&context).with_diagnostics(&mut diagnostics).build();
        if !diagnostics.is_empty() {
            let mut out = StandardStream::stderr(ColorChoice::Never);
            diagnostics.emit(&mut out, &sources).unwrap();
        }
        assert!(diagnostics.is_empty(), "the table compiles with no warnings");
        let unit = unit.expect("the table compiles");
        Self { vm: Vm::new(Arc::new(context.runtime().unwrap()), Arc::new(unit)) }
    }

    /// `(decision, read_only, reason)` of a tool call.
    fn tool(&mut self, tool: &str, approval: &str) -> (String, bool, String) {
        let value = self.vm.call(["probe_tool"], (tool.to_string(), approval.to_string())).unwrap_or_else(|e| panic!("probe_tool({tool}): {e}"));
        split(rune::from_value::<String>(value).unwrap())
    }

    /// `(decision, read_only, reason)` of a shell leaf.
    fn program(&mut self, argv: &[&str]) -> (String, bool, String) {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let value = self.vm.call(["probe_program"], (argv.clone(),)).unwrap_or_else(|e| panic!("probe_program({argv:?}): {e}"));
        split(rune::from_value::<String>(value).unwrap())
    }

    fn word(&mut self, entry: &str, arg: &str) -> String {
        let value = self.vm.call([entry], (arg.to_string(),)).unwrap_or_else(|e| panic!("{entry}({arg}): {e}"));
        rune::from_value::<String>(value).unwrap()
    }

    /// The rows' own answer for one command: the sentence, or "".
    fn words(&mut self, argv: Vec<String>) -> String {
        let value = self.vm.call(["probe_words"], (argv.clone(),)).unwrap_or_else(|e| panic!("minerva_guard_words({argv:?}): {e}"));
        rune::from_value::<String>(value).unwrap()
    }

    /// The rows' own answer for a code string: the sentence, or "".
    fn text(&mut self, text: &str) -> String {
        let value = self.vm.call(["probe_text"], (text.to_string(),)).unwrap_or_else(|e| panic!("minerva_guard_text({text:?}): {e}"));
        rune::from_value::<String>(value).unwrap()
    }
}

fn split(joined: String) -> (String, bool, String) {
    let mut parts = joined.splitn(3, '|');
    let decision = parts.next().unwrap().to_string();
    let read_only = parts.next().unwrap() == "true";
    (decision, read_only, parts.next().unwrap().to_string())
}

/// `(stock, guard file, manual)`, compiled from eidolon's shipped table.
fn tables() -> (Table, Table, Table) {
    let stock = stock_table();
    let guard = perch::guard::wrap(&stock).expect("eidolon's shipped table can be wrapped by the guard");
    let manual = perch::mode::wrap(&stock).expect("and by the manual wrapper");
    (
        Table::new(&format!("{stock}{PROBES}")),
        Table::new(&format!("{guard}{PROBES}{ROW_PROBES}")),
        Table::new(&format!("{manual}{PROBES}{ROW_PROBES}")),
    )
}

/// What the algebra hands the leaf table for each command line below, which is the line split into its words; a `bash -c` string is one.
const DENIED: &[(&[&str], &str)] = &[
    (&["aoide", "mail", "send", "x"], REASON_AOIDE),
    (&["env", "aoide", "send", "--id", "x", "--submit"], REASON_AOIDE),
    (&["/home/u/.local/bin/aoide", "node", "spawn"], REASON_AOIDE),
    (&["bash", "-c", "aoide mail read --for minerva"], REASON_AOIDE),
    (&["timeout", "5", "aoide", "pair"], REASON_AOIDE),
    (&["ssh", "host"], REASON_SSH),
    (&["env", "ssh", "-p", "22", "host"], REASON_SSH),
    (&["bash", "-c", "ssh h true"], REASON_SSH),
    (&["minerva", "keys", "add", "k.pub", "--confirm", "F"], REASON_KEYS),
    (&["/opt/m/minerva", "keys", "remove", "k1"], REASON_KEYS),
    (&["bash", "-c", "hoot keys allow-web F"], REASON_KEYS),
    (&["perch-open.sh", "--keys-arm", "F"], REASON_KEYS),
    // Past the spec's list: the other wrappers and the other interpreters, a runner for hoot, and find -exec.
    (&["nohup", "aoide", "pair"], REASON_AOIDE),
    (&["FOO=bar", "aoide", "x"], REASON_AOIDE),
    (&["stdbuf", "-oL", "aoide", "x"], REASON_AOIDE),
    (&["xargs", "-I", "{}", "aoide", "{}"], REASON_AOIDE),
    (&["python3", "-c", "import os; os.system('aoide pair')"], REASON_AOIDE),
    (&["sh", "-lc", "cd ~ && aoide pair"], REASON_AOIDE),
    (&["find", ".", "-exec", "aoide", "x", ";"], REASON_AOIDE),
    (&["eval", "aoide pair"], REASON_AOIDE),
    (&["powershell.exe", "-File", "hoot.ps1", "keys", "allow-web", "F"], REASON_KEYS),
    (&["pwsh", "-Command", "hoot keys allow-web F"], REASON_KEYS),
    (&["minerva", "web", "--keys-arm", "F"], REASON_KEYS),
    (&["bash", "/opt/m/bin/perch-open.sh", "--keys-arm", "F"], REASON_KEYS),
    (&["nice", "-n", "10", "ssh", "-N", "h"], REASON_SSH),
    // The review's round: every candidate code flag, the other shells' spellings, the other ssh programs and wrappers, I1's real argv, the
    // key directory as a word, and nesting.
    (&["pwsh", "-NoProfile", "-Command", "ssh host"], REASON_SSH),
    (&["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", "aoide mail send x"], REASON_AOIDE),
    (&["bash", "-e", "-c", "ssh host"], REASON_SSH),
    (&["bash", "-cx", "ssh h"], REASON_SSH),
    (&["bash", "-c", "--", "ssh h"], REASON_SSH),
    (&["cmd", "/c", "aoide pair"], REASON_AOIDE),
    (&["cmd.exe", "/K", "ssh h"], REASON_SSH),
    (&["script", "-qc", "aoide pair", "/dev/null"], REASON_AOIDE),
    (&["sshpass", "-p", "x", "ssh", "h"], REASON_SSH),
    (&["autossh", "-M", "0", "h"], REASON_SSH),
    (&["mosh", "h"], REASON_SSH),
    (&["flock", "/tmp/l", "aoide", "x"], REASON_AOIDE),
    (&["watch", "-n", "5", "aoide", "x"], REASON_AOIDE),
    (&["unbuffer", "aoide", "x"], REASON_AOIDE),
    (&["chrt", "-f", "10", "aoide", "x"], REASON_AOIDE),
    (&["taskset", "-c", "0,1", "aoide", "x"], REASON_AOIDE),
    (&["taskset", "0x1", "aoide", "x"], REASON_AOIDE),
    (&["strace", "-o", "/tmp/t", "aoide", "x"], REASON_AOIDE),
    (&["env", "-S", "ssh h"], REASON_SSH),
    (&["env", "--chdir", "/tmp", "aoide", "x"], REASON_AOIDE),
    (&["timeout", "--kill-after", "3", "5", "aoide", "x"], REASON_AOIDE),
    (&["xargs", "--max-args", "1", "aoide", "x"], REASON_AOIDE),
    (&["!", "aoide", "x"], REASON_AOIDE),
    (&["python3", "-c", "import subprocess; subprocess.run(['ssh','h'])"], REASON_SSH),
    (&["perl", "-e", "system(\"aoide\")"], REASON_AOIDE),
    (&["node", "-e", "require('child_process').execSync('ssh h')"], REASON_SSH),
    (&["hoot.ps1", "keys", "allow-web", "F"], REASON_KEYS),
    (&["pwsh", "-File", "C:\\tools\\hoot.cmd", "keys", "allow-web", "F"], REASON_KEYS),
    (&["bin/perch-open.sh", "--keys-arm", "0F7A3A5C447EC7EF", "--port", "48110", "--token-file", "/t/x"], REASON_KEYS),
    (&["cp", "evil.pub", "/h/.config/minerva/update-keys/"], REASON_KEYFILE),
    (&["tee", "/h/.config/Minerva/Update-Keys/k.pub"], REASON_KEYFILE),
    (&["bash", "-c", "cp x ~/.config/minerva/update-keys/"], REASON_KEYFILE),
    (&["bash", "-c", "bash -c 'bash -c aoide x'"], REASON_AOIDE),
];

/// Commands that mention aoide, ssh or keys without running them, or run something next to them. The guard says nothing about these.
const ALLOWED: &[&[&str]] = &[
    &["ls", "~/src/aoide"],
    &["grep", "-r", "aoide", "."],
    &["scp", "a", "b:c"],
    &["echo", "ssh"],
    &["minerva", "keys", "list"],
    &["minerva", "status", "--json"],
    &["minerva", "keys"],
    &["minerva", "keys", "inspect", "k1"],
    &["rsync", "-e", "ssh", "a", "b:c"],
    &["ssh-keygen", "-t", "ed25519"],
    &["cargo", "test", "-p", "aoide-core"],
    &["cat", "aoide.log"],
    &["command", "-v", "ssh"],
    &["which", "aoide"],
    &["bash", "-c", "ls ~/src/aoide && echo ssh"],
    &["git", "clone", "git@github.com:a/b"],
    &["hoot", "status"],
    &["echo", "hoot", "keys"],
    &["bash", "script.sh"],
    // The false positives the review found: code that merely names a program is not a command, an option that is not a code option is not
    // one, and keywords and wrappers with nothing behind them run nothing.
    &["python3", "-c", "print('aoide ssh')"],
    &["python3", "-c", "x = f(aoide)"],
    &["python3", "-c", "os.system('ls aoide')"],
    &["node", "-e", "console.log('ssh')"],
    &["perl", "-ne", "print"],
    &["ruby", "-e", "puts 'aoide'"],
    &["bash", "-e", "script.sh"],
    &["bash", "-x", "-v"],
    &["pwsh", "-NoProfile", "-File", "x.ps1"],
    &["cmd", "/d", "/q"],
    &["flock", "/tmp/l", "ls", "aoide"],
    &["watch", "-n", "5", "ls"],
    &["echo", "if", "ssh", "then"],
    &["env", "-i", "ls"],
    &["grep", "-rn", "update-keys", "installer"],
    &["ls", "-la", "~/.config/minerva"],
];

/// A spread of what agents run, none of it touching a row: the guard file must give the stock table's answer to every one.
const UNTOUCHED: &[&[&str]] = &[
    &["ls"], &["ls", "-la", "/tmp"], &["cat", "Cargo.toml"], &["git", "status"], &["git", "push", "--force"], &["git", "reset", "--hard"], &["cargo", "build"],
    &["cargo", "publish"], &["python3", "x.py"], &["python3", "-c", "print(1)"], &["node", "-e", "1"], &["bash", "-c", "echo hi"], &["sh"], &["curl", "http://x"],
    &["curl", "-o", "f", "http://x"], &["rm", "-rf", "x"], &["sudo", "ls"], &["shutdown"], &["mkfs.ext4", "x"], &["useradd", "x"], &["gh", "repo", "delete", "x"],
    &["systemctl", "poweroff"], &["systemctl", "status"], &["docker", "ps"], &["tar", "xf", "a.tar"], &["mkdir", "x"], &["env"], &["env", "FOO=1", "ls"],
    &["timeout", "5", "sleep", "1"], &["xargs", "echo"], &["nohup", "cargo", "build"], &["frobnicate"], &["scp", "a", "b:c"], &["rsync", "a", "b:c"],
    &["ssh-add", "-l"], &["minerva", "keys", "list"], &["minerva", "status"], &["jq", "."], &["find", ".", "-name", "x"], &["find", ".", "-exec", "ls", "{}", ";"],
    &[],
];

const TOOLS: &[&str] = &[
    "read", "grep", "write", "edit", "fetch", "search", "ask_user", "choices_user", "mneme_rpc", "vv", "Read", "Glob", "Bash", "WebFetch", "BashOutput", "peers",
    "send", "wait_for", "tool_search", "bash", "subagent", "browser_click", "jev_run", "term_list", "term_open", "term_read", "term_send", "term_close", "mail_send",
    "melete_dispatch", "melete_status", "some_plugin_verb",
];

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// The guard file refuses the deny corpus with the right sentence, in the policy and in the Rust copy alike, and the allow corpus is the
/// stock table's. Mutations: drop a row from rows.rn; change a sentence; judge any argv token instead of the program (the allow corpus
/// goes red: `ls ~/src/aoide` and `grep -r aoide .`).
#[test]
fn the_guard_file_refuses_what_the_rows_name_and_nothing_else() {
    let (mut stock, mut guard, _) = tables();
    for (line, sentence) in DENIED {
        let (decision, read_only, reason) = guard.program(line);
        assert_eq!((decision.as_str(), read_only, reason.as_str()), ("deny", false, *sentence), "{line:?}");
        assert_eq!(guard.words(argv(line)), *sentence, "{line:?}: the rows' own answer");
        assert_eq!(perch::guard::refuses_argv(&argv(line)), Some(*sentence), "{line:?}: the Rust copy");
        assert_ne!(stock.program(line).0, "deny", "{line:?}: control, the stock table does not refuse it");
    }
    for line in ALLOWED {
        assert_eq!(guard.words(argv(line)), "", "{line:?}");
        assert_eq!(perch::guard::refuses_argv(&argv(line)), None, "{line:?}: the Rust copy");
        assert_eq!(guard.program(line), stock.program(line), "{line:?} is the stock table's answer, reason included");
    }
}

/// Every other verdict is the member's table's, byte for byte: the tools, the shell leaves, the filesystem verbs and the default posture.
/// A guard that compiles but lets every verdict through, or changes one, would be the worst case. Mutations: a pass-through that returns
/// `allow`; a wrapper that rewrites the reason (the vouch store keys on the stock fallthrough reason, so vouches would stop counting).
#[test]
fn every_other_verdict_is_the_stock_tables() {
    let (mut stock, mut guard, _) = tables();
    for line in UNTOUCHED {
        assert_eq!(guard.program(line), stock.program(line), "{line:?}");
    }
    for tool in TOOLS {
        for approval in ["read_only", "mutating", "destructive", "unknown"] {
            assert_eq!(guard.tool(tool, approval), stock.tool(tool, approval), "{tool} ({approval})");
        }
    }
    for verb in ["mkdir", "cp", "mv", "touch", "tee", "ln", "rm", "rmdir", "dd", "chmod", "ls"] {
        assert_eq!(guard.word("probe_fs", verb), stock.word("probe_fs", verb), "{verb}");
    }
    for posture in ["allow", "flag", "deny"] {
        assert_eq!(guard.word("probe_default", posture), stock.word("probe_default", posture), "{posture}");
    }
    // The fallthrough the vouch store listens for survives the wrapper: a plugin verb is `unclassified tool (declared ...)` under the guard
    // exactly as in the stock table, which is what lets `eidolon plugins trust` answer it.
    let (decision, _, reason) = guard.tool("term_send", "mutating");
    assert_eq!(decision, "flag");
    assert!(reason.starts_with("unclassified tool (declared "), "{reason}");
}

/// Manual carries the same rows, ahead of its asks: a refusal is a `deny` and not a question, and everything else asks as before.
/// Mutations: put the rows after the manual wrapper's `minerva_asks`; leave them out of `mode::wrap`.
#[test]
fn manual_refuses_the_same_rows_ahead_of_its_questions() {
    let (_, _, mut manual) = tables();
    for (line, sentence) in DENIED {
        let (decision, _, reason) = manual.program(line);
        assert_eq!((decision.as_str(), reason.as_str()), ("deny", *sentence), "{line:?}");
    }
    for line in ALLOWED {
        let (decision, _, reason) = manual.program(line);
        assert_ne!(decision, "deny", "{line:?}");
        assert!(reason.starts_with("manual mode: "), "{line:?} is asked about, as manual asks about every leaf: {reason}");
    }
    // D6, the manual column: every terminal and mail tool asks, the reads of it included, with manual's own reason prefix.
    for tool in ["term_list", "term_read", "term_open", "term_send", "term_close", "mail_send"] {
        for approval in ["read_only", "mutating"] {
            let (decision, _, reason) = manual.tool(tool, approval);
            assert_eq!(decision, "flag", "{tool} ({approval})");
            assert!(reason.starts_with("manual mode: unclassified tool"), "{tool}: {reason}");
        }
    }
}

/// The guard file renames `classify_program` and nothing else: the other three entry points stay the member's own, where eidolon reads
/// them (`policy-rows` compares the string arms of `pub fn classify_tool` with the shipped table, so a renamed one would look edited), and
/// the rows are in once. Mutation: rename all four in the guard (`rename_entry_points` in `guard::wrap`).
#[test]
fn the_guard_file_renames_classify_program_once_and_nothing_else() {
    let stock = stock_table();
    let guard = perch::guard::wrap(&stock).unwrap();
    assert_eq!(guard.matches("fn minerva_stock_classify_program(").count(), 1);
    assert_eq!(guard.lines().filter(|l| l.starts_with("pub fn classify_program(")).count(), 1, "the wrapper's alone");
    for entry in ["classify_tool", "fs_verb_tier", "bash_default"] {
        assert_eq!(guard.matches(&format!("fn minerva_stock_{entry}(")).count(), 0, "{entry} is not renamed");
        assert_eq!(guard.lines().filter(|l| l.starts_with(&format!("pub fn {entry}("))).count(), 1, "{entry}: the member's own, once");
    }
    assert_eq!(guard.matches("const MINERVA_GUARD_AOIDE =").count(), 1, "the rows are in once");
    assert!(guard.starts_with(stock.lines().next().unwrap()), "the member's text comes first, its marker line included");
    assert!(!guard.contains("minerva manual mode"), "and it carries nothing of the manual wrapper");
    assert!(perch::guard::wrap(&guard).is_err(), "a guard is not a table to wrap");
}

/// Manual wraps the member's table and never a guard, and takes over all four entry points: exactly one `minerva_stock_` rename each.
/// Mutation: build the manual policy from the guard file.
#[test]
fn manual_renames_each_entry_point_once() {
    let stock = stock_table();
    let guard = perch::guard::wrap(&stock).unwrap();
    let manual = perch::mode::wrap(&stock).unwrap();
    for entry in ["classify_tool", "classify_program", "fs_verb_tier", "bash_default"] {
        assert_eq!(manual.matches(&format!("fn minerva_stock_{entry}(")).count(), 1, "{entry}");
        assert_eq!(manual.lines().filter(|l| l.starts_with(&format!("pub fn {entry}("))).count(), 1, "{entry}: the wrapper's alone");
    }
    assert_eq!(manual.matches("const MINERVA_GUARD_AOIDE =").count(), 1, "the rows are in once");
    assert!(manual.starts_with(stock.lines().next().unwrap()), "the member's text comes first, its marker line included");
    assert!(!manual.contains("minerva guard: generated"), "manual does not carry the guard file's wrapper");
    assert!(perch::mode::wrap(&guard).is_err(), "a guard is not a table to wrap");
    assert!(perch::guard::wrap(&manual).is_err(), "nor is the manual policy");
}

// ---------------------------------------------------------------------------
// The Rune rows and the Rust rows agree
// ---------------------------------------------------------------------------

/// A small xorshift, so the corpus is the same on every machine and a failure can be replayed from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }
}

const WORDS: &[&str] = &[
    "aoide", "aoided", "aoide.exe", "Aoide", "AOIDE.EXE", "a/b/aoide", "/home/u/.local/bin/aoide", "aoide-core", "ssh", "SSH", "ssh.exe", "ssh-keygen", "minerva", "minerva.exe",
    "keys", "add", "remove", "list", "inspect", "web", "status", "--keys-arm", "--keys-arm=F", "-c", "-lc", "-e", "-ne", "-r", "-Command", "--command", "--eval", "-x", "--", "-",
    "hoot", "hoot.ps1", "perch-open.sh", "bash", "sh", "zsh", "python3", "python", "perl", "node", "pwsh", "powershell.exe", "eval", "find", "-exec", "-execdir", "{}", "+", ";",
    "env", "nohup", "timeout", "5", "5s", "xargs", "-I", "-n", "-P", "nice", "stdbuf", "-o", "L", "-oL", "command", "-v", "-V", "exec", "-a", "setsid", "time", "ionice", "-c1",
    "FOO=bar", "A=", "=x", "1=2", "x", "y", "ls", "echo", "grep", "-r", "cat", "f", "é", "日本", "İ", "\u{212a}", "%", "$", "~/x", "../y", "a b", "",
    // The review's round: every new token of rows.rn and mod.rs.
    "sshpass", "autossh", "mosh", "-NoProfile", "-NonInteractive", "-cx", "-ec", "-C", "-S", "-Saoide", "--chdir", "--unset", "--split-string", "--split-string=ssh h",
    "flock", "script", "unbuffer", "chrt", "taskset", "-c0", "0x1", "watch", "strace", "cmd", "cmd.exe", "/c", "/k", "/C", "/d", "pwsh.exe", "powershell", "hoot.cmd",
    "C:\\x\\hoot.ps1", "C:\\bin\\Aoide.exe", "a\\b\\ssh", "minerva/update-keys", "~/.config/Minerva/Update-Keys/k.pub", "update-keys", "subprocess", "subprocess.run(['ssh',", "os.system",
    "system(", "system(aoide", "exec(", "execSync(", "popen(", "spawn(", "`", "print(1)", "'ls", "aoide)", "ssh)", "!", "if", "then", "elif", "else", "do", "while", "until",
    "ruby", "php", "deno", "nodejs", "ksh", "fish", "tcsh", "ash", "dash", "csh", "-o", "-f", "--output", "--format", "--max-args", "--replace", "--delimiter", "--timeout", "-w",
    "--signal", "--kill-after", "--adjustment", "--input", "--error", "--arg-file", "--max-procs", "-p", "-s", "-E", "-X", "--command=ssh", "-Command", "-com", "-File", "--print",
    "perch-open.sh", "bin/perch-open.sh", "--port", "48110", "--token-file", "/t/x", "0F7A3A5C447EC7EF", "allow-web", "add", "confirm", "--confirm", "--json",
];

/// Whole command lines assembled from the shapes that matter, so the random corpus meets the nested and wrapped ones often enough.
const PREFIXES: &[&str] = &[
    "", "", "env ", "nohup ", "timeout 5 ", "env -S ", "env --chdir /tmp ", "flock /tmp/l ", "watch -n 5 ", "strace -o /tmp/t ", "chrt -f 10 ", "taskset -c 0 ", "taskset 0x1 ",
    "! ", "if ", "while ", "time -o /tmp/t ", "xargs -I {} ", "xargs --max-args 1 ", "nice -n 5 ", "command ", "command -v ", "exec -a x ", "FOO=1 ", "unbuffer ", "setsid ",
];

const CORES: &[&str] = &[
    "aoide x", "aoided", "ssh h", "sshpass -p x ssh h", "autossh -M 0 h", "mosh h", "minerva keys add k", "minerva keys list", "minerva web --keys-arm F", "hoot keys allow-web F",
    "hoot.ps1 keys allow-web F", "perch-open.sh --keys-arm F --port 1 --token-file /t/x", "ls", "echo aoide", "cp x ~/.config/minerva/update-keys/", "ls minerva-update-keys",
    "python3 -c os.system('aoide')", "python3 -c print('ssh')", "node -e execSync('ssh h')", "perl -e `ssh h`", "perl -e print(aoide)", "bash -c ", "bash -cx ", "bash -e -c ",
    "bash -c -- ", "bash -e x.sh", "pwsh -NoProfile -Command ", "powershell.exe -Command ", "cmd /c ", "cmd.exe /k ", "sh -lc ", "zsh -c ", "eval ", "find . -exec ", "find . -execdir ",
    "script -qc ", "script x", "python3 -c print(1)", "bash",
];

/// A line of up to three nested levels: a prefix and a core, and a nested one after a core that takes a command.
fn assembled(rng: &mut Rng) -> String {
    let mut line = String::new();
    for _ in 0..1 + rng.next() % 3 {
        line.push_str(rng.pick(PREFIXES));
        line.push_str(rng.pick(CORES));
        if line.ends_with(' ') {
            continue;
        }
        break;
    }
    if line.ends_with(' ') {
        line.push_str("aoide x");
    }
    line
}

const GLUE: &[&str] = &[" ", " ", " ", " ", "", ";", " && ", " | ", "\n", "\t", " (", ") ", " `", "` ", " \"", "\" ", " '", "' ", " \\", " {", "} ", " || ", " & "];

/// The same random command lines and code strings through the Rune rows and through the Rust copy. Neither may raise (a Rune error would
/// make eidolon ask about every command), and both must give the same sentence. Mutations: change one list in rows.rn or in mod.rs.
#[test]
fn the_rune_rows_and_the_rust_rows_give_the_same_answers_on_a_random_corpus() {
    let (_, mut guard, _) = tables();
    let mut rng = Rng(0x6d696e65727661);
    for round in 0..6000 {
        let words: Vec<String> = (0..1 + rng.next() % 9).map(|_| rng.pick(WORDS).to_string()).collect();
        let rune = guard.words(words.clone());
        let rust = perch::guard::refuses_argv(&words).unwrap_or("");
        assert_eq!(rune, rust, "round {round}: argv {words:?}");

        let mut text = String::new();
        for _ in 0..1 + rng.next() % 10 {
            text.push_str(rng.pick(GLUE));
            text.push_str(rng.pick(WORDS));
        }
        let rune = guard.text(&text);
        let rust = perch::guard::refuses_text(&text).unwrap_or("");
        assert_eq!(rune, rust, "round {round}: text {text:?}");
        // The term-text check is the same rows without ssh: it never says more than the full check, and says nothing for ssh.
        let term = perch::guard::refuses_term_text(&text);
        assert!(term.is_none() || term == Some(rust) || rust == REASON_SSH, "round {round}: {text:?}: term {term:?}, full {rust:?}");
        assert_ne!(term, Some(REASON_SSH));
    }
    // The assembled lines: a prefix, a core, and nesting, as the argv the algebra hands (split at blanks) and as one text.
    let mut refused = 0;
    for round in 0..6000 {
        let line = assembled(&mut rng);
        let words: Vec<String> = line.split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect();
        let (rune, rust) = (guard.words(words.clone()), perch::guard::refuses_argv(&words).unwrap_or(""));
        assert_eq!(rune, rust, "assembled round {round}: argv {words:?}");
        let (rune, rust) = (guard.text(&line), perch::guard::refuses_text(&line).unwrap_or(""));
        assert_eq!(rune, rust, "assembled round {round}: text {line:?}");
        refused += usize::from(!rust.is_empty());
        let term = perch::guard::refuses_term_text(&line);
        assert!(term.is_none() || term == Some(rust) || rust == REASON_SSH, "assembled round {round}: {line:?}: term {term:?}, full {rust:?}");
    }
    assert!(refused > 300, "the assembled corpus reaches the refusals ({refused} of 6000), so the agreement above means something");
}

/// Hostile shapes do not raise or run away in the Rune: empty lists, a lone wrapper, nesting past the bound, a megabyte of blanks.
#[test]
fn hostile_command_lines_are_answered_not_raised() {
    let (_, mut guard, _) = tables();
    for line in [&[][..], &[""], &["env"], &["timeout"], &["nice", "-n"], &["xargs", "-I"], &["command", "-v"], &["exec", "-a"], &["bash", "-c"], &["find", "-exec"], &["--"], &["=", "="]] {
        assert_eq!(guard.words(argv(line)), "", "{line:?}");
        assert_eq!(perch::guard::refuses_argv(&argv(line)), None, "{line:?}");
    }
    let nest = |levels: usize| "bash -c ".repeat(levels) + "aoide";
    assert_eq!(guard.text(&nest(3)), REASON_AOIDE);
    let started = std::time::Instant::now();
    assert_eq!(guard.text(&nest(4000)), "", "past the bound it is a stated miss, and it returns");
    assert_eq!(guard.text(&" ".repeat(1_000_000)), "");
    assert_eq!(guard.text(&"a;".repeat(50_000)), "");
    assert!(started.elapsed() < std::time::Duration::from_secs(60), "{:?}", started.elapsed());
}

/// `find -exec` nested forty levels with a benign tail, and ten thousand with an aoide tail: both are answered (the second is past the
/// depth bound, a stated miss), neither raises, neither takes long, in the Rune and in the Rust, which runs on a 2 MiB stack. A nested
/// `bash -c` flag is bounded by the work budget as well, so a command that repeats a flag is not read once per way of nesting it.
/// Mutations: judge every -exec from the start of the line again (quadratic); drop the depth check; drop the work budget.
#[test]
fn nested_find_exec_and_repeated_flags_are_linear_and_bounded() {
    let (_, mut guard, _) = tables();
    let found = |levels: usize, tail: &str| ("find . -exec ".repeat(levels) + tail).split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect::<Vec<_>>();
    let started = std::time::Instant::now();
    assert_eq!(guard.words(found(40, "ls ;")), "", "forty levels of find, a benign tail");
    assert_eq!(guard.words(found(10_000, "aoide x ;")), "", "ten thousand levels, an aoide tail: past the bound, a stated miss");
    assert_eq!(guard.words(found(3, "aoide x ;")), REASON_AOIDE, "and the shallow one is read");
    let (benign, hostile) = (found(40, "ls ;"), found(10_000, "aoide x ;"));
    let small = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || (perch::guard::refuses_argv(&benign), perch::guard::refuses_argv(&hostile)))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(small, (None, None));
    // The same flag repeated: the line is 40 KB of `-c`, and its first sixteen are judged, each against the rest of the line.
    let flags: Vec<String> = std::iter::once("bash".to_string()).chain((0..5000).map(|_| "-c".to_string())).chain(["aoide".to_string()]).collect();
    assert_eq!(guard.words(flags.clone()), perch::guard::refuses_argv(&flags).unwrap_or(""), "the two agree on the repeated flag");
    let nested = "bash -c ".repeat(5000) + "aoide";
    let words: Vec<String> = nested.split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect();
    assert_eq!(guard.words(words.clone()), perch::guard::refuses_argv(&words).unwrap_or(""));
    assert!(started.elapsed() < std::time::Duration::from_secs(60), "{:?}", started.elapsed());
}

// ---------------------------------------------------------------------------
// refuses_term_text
// ---------------------------------------------------------------------------

/// The text an agent types into a terminal: the aoide CLI and the key changes are refused (with the sentence), ssh is the intended path,
/// and a word that merely appears is not a command. Mutations: judge every word (the second list goes red); drop the hoot row.
#[test]
fn term_text_refuses_the_aoide_cli_and_key_changes_and_leaves_ssh_alone() {
    for text in ["aoide mail send x\n", "cd ~/src && aoide pair\n", "echo hi | aoide send", "bash -c \"aoide mail read --for minerva\"", "env aoide node spawn", "FOO=1 aoided", "\\aoide x"] {
        assert_eq!(perch::guard::refuses_term_text(text), Some(REASON_AOIDE), "{text:?}");
    }
    // The key rows, in I1's real spellings: `minerva web --keys-arm F`, `bin/perch-open.sh --keys-arm F --port N --token-file X`, and
    // `hoot keys allow-web F`, plain, chained, wrapped and run through a shell or powershell.
    for text in [
        "minerva keys add k.pub --confirm F\n", "hoot keys allow-web F\n", "perch-open.sh --keys-arm F", "pwsh -Command \"hoot keys allow-web F\"", "minerva web --keys-arm F\n",
        "minerva web --keys-arm 0F7A3A5C447EC7EF --port 48110\n", "bin/perch-open.sh --keys-arm 0F7A3A5C447EC7EF --port 48110 --token-file /t/x\n", "cd ~/Projects/Minerva && minerva web --keys-arm F",
        "bash -c \"hoot keys allow-web F\"", "powershell.exe -NoProfile -Command \"hoot.ps1 keys allow-web F\"", "env X=1 minerva keys add k", ".\\hoot.ps1 keys allow-web F",
    ] {
        assert_eq!(perch::guard::refuses_term_text(text), Some(REASON_KEYS), "{text:?}");
    }
    for text in ["cp evil.pub ~/.config/minerva/update-keys/\n", "tee ~/.config/Minerva/Update-Keys/k.pub < evil.pub", "cat evil.pub > C:\\Users\\u\\minerva\\update-keys\\k.pub"] {
        assert_eq!(perch::guard::refuses_term_text(text), Some(REASON_KEYFILE), "{text:?}");
    }
    for text in [
        "ssh host\n", "ssh -p 22 u@h 'ls ~/src/aoide'", "ls ~/src/aoide\n", "grep -r aoide .\n", "echo aoide", "cat aoide.log", "cd ~/Projects/Minerva/Aoide", "scp a b:c", "minerva keys list\n",
        "minerva status --json", "", "\n", "x", "sshpass -p x ssh host\n", "bash -c \"ssh h uptime\"", "mosh h", "echo hoot keys", "grep -rn update-keys installer", "minerva web --port 48110",
    ] {
        assert_eq!(perch::guard::refuses_term_text(text), None, "{text:?}");
    }
}

/// `refuses_term_text` judges one `term_send` text, the first 8 KiB of it, and is a hint and not a gate: a command split across sends, one
/// built by a variable, or one past the cap passes. These are the stated misses, pinned, so that changing one is a decision.
#[test]
fn term_text_is_a_hint_and_its_misses_are_pinned() {
    for text in ["ao", "ide mail send x\n", "a=aoi; b=de; $a$b mail send x", "ao\x7fide x", "aoi\tde x", "echo YWlvZGUgeA== | base64 -d | sh", "./run-it.sh"] {
        assert_eq!(perch::guard::refuses_term_text(text), None, "{text:?}");
    }
    let pad = "echo a; ".repeat(1024);
    assert_eq!(pad.len(), 8192);
    assert_eq!(perch::guard::refuses_term_text(&format!("{pad}aoide x")), None, "past 8 KiB");
    assert_eq!(perch::guard::refuses_term_text(&format!("{}aoide x", &pad[..8192 - 8])), Some(REASON_AOIDE), "and inside it");
    assert_eq!(perch::guard::refuses_term_text(&"é".repeat(8192)), None, "a cut inside a character is a cut, not a panic");
}

// ---------------------------------------------------------------------------
// A temp tree, and the eidolon inside it
// ---------------------------------------------------------------------------

/// The real eidolon: `MINERVA_TEST_EIDOLON` (an absolute path) when set, else the first `eidolon` on `PATH`. With neither the test says so
/// and passes, unless `MINERVA_REQUIRE_EIDOLON=1`, in which case it fails: a run that was meant to look at the real thing never passes
/// without having looked.
fn eidolon() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MINERVA_TEST_EIDOLON").map(PathBuf::from) {
        assert!(path.is_absolute() && path.is_file(), "MINERVA_TEST_EIDOLON={} is not an absolute path to a file", path.display());
        return Some(path);
    }
    let found = std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|dir| dir.join("eidolon")).find(|candidate| candidate.is_file()));
    assert!(
        found.is_some() || std::env::var("MINERVA_REQUIRE_EIDOLON").as_deref() != Ok("1"),
        "MINERVA_REQUIRE_EIDOLON=1 and there is no eidolon: set MINERVA_TEST_EIDOLON to its absolute path"
    );
    found
}

/// `generate_with` records a process-wide status; the tests that read it back take turns, and read it before they let go.
static GENERATING: Mutex<()> = Mutex::new(());

/// Runs a command that may be a script written a moment ago: another test's fork can hold the script's write handle for an instant,
/// which Linux reports as ETXTBSY (26).
fn output_of(command: &mut Command) -> std::process::Output {
    for attempt in 0..100 {
        match command.stdin(std::process::Stdio::null()).output() {
            Ok(out) => return out,
            Err(e) if e.raw_os_error() == Some(26) && attempt < 99 => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(e) => panic!("running {:?}: {e}", command.get_program()),
        }
    }
    unreachable!()
}

fn executable(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    // The first run of a fresh script is the one that can meet ETXTBSY; get it over with here, where it is retried.
    output_of(Command::new(path).arg("--version"));
}

/// What `eidolon policy` printed about one command.
struct Verdict {
    tier: String,
    reason: String,
    reads: String,
    harness: String,
    text: String,
}

/// The status as of one `generate`, and the config it wrote.
struct Generated {
    config: PathBuf,
    status: perch::guard::GuardStatus,
}

/// A temp tree standing in for `~`: `config/eidolon/config.toml` is the member's config (swarm off, `plugins_dir` inside the tree), `Minerva/`
/// is the perch home, and `eidolon` is what the perch is told to run: a wrapper that pins HOME and every XDG_ directory to this tree before
/// it execs the real binary (so the unpack, the probe and the grants the perch itself runs cannot reach the operator's config), or a stub.
struct Tree {
    _root: tempfile::TempDir,
    base: PathBuf,
    config_dir: PathBuf,
    home: PathBuf,
    member: PathBuf,
    work: PathBuf,
    eidolon: PathBuf,
}

impl Tree {
    /// A tree with no eidolon in it: `eidolon` names nothing, which is right for a guard that is off before anything would be run. The
    /// member's table is eidolon's shipped one, written where eidolon would have unpacked it.
    fn bare(tail: &str) -> Self {
        let tree = Self::lay_out(tail);
        std::fs::write(tree.config_dir.join("policy.rn"), stock_table()).unwrap();
        tree
    }

    /// [`Tree::bare`], with a script that stands in for eidolon.
    fn stub(tail: &str, script: &str) -> Self {
        let mut tree = Self::bare(tail);
        tree.eidolon = tree.base.join("bin/eidolon-stub");
        executable(&tree.eidolon, &format!("#!/bin/sh\n{script}\n"));
        tree
    }

    /// A tree over the real eidolon, with nothing unpacked (the first `generate` does that, as a first start would). `None`, loudly, when
    /// there is no eidolon on `PATH`.
    fn real(tail: &str) -> Option<Self> {
        let Some(real) = eidolon() else {
            eprintln!("guard: no eidolon (MINERVA_TEST_EIDOLON, PATH); the real-eidolon checks are skipped");
            return None;
        };
        let mut tree = Self::lay_out(tail);
        let (base, run) = (&tree.base, tree.base.join("run"));
        assert!(!real.to_string_lossy().contains('\''), "{}", real.display());
        let script = format!(
            "#!/bin/sh\nexport HOME='{b}/h'\nexport XDG_CONFIG_HOME='{b}/config'\nexport XDG_DATA_HOME='{b}/data'\nexport XDG_STATE_HOME='{b}/state'\n\
             export XDG_CACHE_HOME='{b}/cache'\nexport XDG_RUNTIME_DIR='{r}'\nexec '{e}' \"$@\"\n",
            b = base.display(),
            r = run.display(),
            e = real.display(),
        );
        let wrapper = base.join("bin/eidolon-temp");
        executable(&wrapper, &script);
        tree.eidolon = wrapper;
        // Counted by the run that wants to know how many real-eidolon tests did look (`--nocapture`, then count the lines).
        eprintln!("REAL-EIDOLON-RAN {}", std::thread::current().name().unwrap_or("?"));
        Some(tree)
    }

    fn lay_out(tail: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().to_path_buf();
        assert!(base.starts_with(std::env::temp_dir()), "the tree is under the temp dir, never under a real HOME: {}", base.display());
        assert!(!base.to_string_lossy().contains('\''), "{}", base.display());
        let (config_dir, home, work) = (base.join("config/eidolon"), base.join("Minerva"), base.join("work"));
        for dir in [&config_dir, &home, &work, &base.join("h"), &base.join("plugins"), &base.join("bin"), &base.join("run")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::set_permissions(base.join("run"), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let member = config_dir.join("config.toml");
        std::fs::write(&member, format!("plugins_dir = '{}'\n\n[swarm]\nenabled = false\n{tail}", base.join("plugins").display())).unwrap();
        let eidolon = base.join("bin/none");
        Self { _root: root, base, config_dir, home, member, work, eidolon }
    }

    fn generate(&self) -> Generated {
        self.generate_at(&self.home, &self.eidolon)
    }

    /// [`Tree::generate`] for another home over the same member config, or with another eidolon (a stub that answers differently).
    fn generate_at(&self, home: &Path, eidolon: &Path) -> Generated {
        let _turn = GENERATING.lock().unwrap_or_else(|e| e.into_inner());
        let config = perch::config::generate_with(home, &self.member, Some(eidolon)).unwrap();
        Generated { config, status: perch::guard::status() }
    }

    fn manual(&self) -> PathBuf {
        let manual = perch::mode::generate_manual(&self.home, &self.member, &self.eidolon).unwrap();
        assert_eq!(manual, self.home.join("config-manual.toml"));
        manual
    }

    fn guard_file(&self) -> PathBuf {
        self.config_dir.join(perch::guard::file_name(&self.home))
    }

    /// eidolon, with `flags` before `--config` and `args` after. Every run is mock-only: nothing here can reach a provider.
    fn run(&self, flags: &[&str], config: Option<&Path>, args: &[&str]) -> std::process::Output {
        let mut command = Command::new(&self.eidolon);
        command.args(flags);
        if let Some(config) = config {
            command.arg("--config").arg(config);
        }
        output_of(command.args(args).current_dir(&self.work))
    }

    /// One shell command through `eidolon policy`, which classifies without running it and without a model.
    fn verdict(&self, flags: &[&str], config: &Path, command: &str) -> Verdict {
        let out = self.run(flags, Some(config), &["policy", "--", command]);
        assert!(out.status.success(), "{command}: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let field = |name: &str| text.lines().find_map(|line| line.strip_prefix(name)).unwrap_or_else(|| panic!("{name} in {text}")).trim().to_string();
        Verdict { tier: field("tier:").split_whitespace().next().unwrap().to_string(), reason: field("reason:"), reads: field("reads:"), harness: field("harness:"), text }
    }

    /// The fake terminal plugin: five verbs, a table's `unclassified tool (declared ...)` each. Every one returns a fixed word.
    fn write_term_plugin(&self) {
        let dir = self.base.join("plugins/term");
        std::fs::create_dir_all(dir.join("tools")).unwrap();
        std::fs::write(dir.join("plugin.rn"), "pub fn manifest() { #{ name: \"term\" } }\n").unwrap();
        for verb in ["list", "open", "read", "send", "close"] {
            let approval = if verb == "list" || verb == "read" { "read_only" } else { "mutating" };
            std::fs::write(
                dir.join("tools").join(format!("{verb}.rn")),
                format!(
                    "pub fn manifest() {{\n    #{{ name: \"term_{verb}\", description: \"a fake {verb}\", approval: \"{approval}\", input_schema: #{{ \"type\": \"object\", \"properties\": #{{}} }} }}\n}}\n\
                     pub async fn call(input) {{ Ok(\"ran term_{verb}\") }}\n"
                ),
            )
            .unwrap();
        }
    }

    /// A plugin with one tool, `cred_get`, that reaches an endpoint (127.0.0.1:1, where nothing listens) with a credential read from
    /// `token`. The credential needs a grant; nothing is ever sent, and the file holds a placeholder.
    fn write_cred_plugin(&self, token: &Path) {
        let dir = self.base.join("plugins/cred");
        std::fs::create_dir_all(dir.join("tools")).unwrap();
        std::fs::write(dir.join("plugin.rn"), "pub fn manifest() { #{ name: \"cred\" } }\n").unwrap();
        std::fs::write(
            dir.join("tools/get.rn"),
            format!(
                "pub fn manifest() {{\n    #{{ name: \"cred_get\", description: \"a fake credentialed call\", approval: \"read_only\", input_schema: #{{ \"type\": \"object\", \"properties\": #{{}} }} }}\n}}\n\
                 pub async fn call(input) {{\n    match eidolon::api_request(#{{ base_url: \"http://127.0.0.1:1\", token_file: \"{}\", auth: #{{ header: \"Authorization\", prefix: \"Bearer \" }} }}, \"GET\", \"/x\", None).await {{\n        Ok(_) => Ok(\"reached\"),\n        Err(e) => Err(`the endpoint said: ${{e}}`),\n    }}\n}}\n",
                token.display()
            ),
        )
        .unwrap();
    }

    /// The workflow that calls one tool by name. Its own plugin ships no verbs, so the door has nothing of its own to want vouched.
    fn write_probe_plugin(&self) -> PathBuf {
        let dir = self.base.join("probe");
        std::fs::create_dir_all(dir.join("workflows")).unwrap();
        std::fs::write(dir.join("plugin.rn"), "pub fn manifest() { #{ name: \"probe\" } }\n").unwrap();
        std::fs::write(
            dir.join("workflows/go.rn"),
            "pub fn workflow() {\n    #{ name: \"go\", description: \"call one tool\", approval: \"read_only\",\n       params: #{ \"type\": \"object\", \"properties\": #{ \"tool\": #{ \"type\": \"string\" } } } }\n}\n\
             pub async fn run(args) {\n    let tool = match args.get(\"tool\") { Some(t) => t, None => return Err(\"no tool named\") };\n    match eidolon::tool_call(tool, #{}).await {\n        Ok(text) => Ok(text),\n        Err(e) => Err(`the step failed: ${e}`),\n    }\n}\n",
        )
        .unwrap();
        dir
    }

    /// One tool call by the real gate under `config`: a workflow run (mock provider, mock model, no user to answer a question, so an ask is
    /// declined). `Ok(text)` when the step ran, `Err(reason)` when it was declined or refused.
    fn call(&self, flags: &[&str], config: &Path, tool: &str) -> Result<String, String> {
        let probe = self.write_probe_plugin();
        let args = format!("{{\"tool\":\"{tool}\"}}");
        let out = self.run(flags, Some(config), &["workflow", "run", probe.to_str().unwrap(), "go", "--args", &args, "--model", "mock", "--provider", "mock"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout.lines().rev().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("no result line: {out:?}"));
        let json: serde_json::Value = serde_json::from_str(line).unwrap();
        match (json["status"].as_str(), out.status.code()) {
            (Some("completed"), Some(0)) => Ok(json["report"].as_str().unwrap_or_default().to_string()),
            (Some("failed"), Some(2)) => Err(json["reason"].as_str().unwrap_or_default().to_string()),
            _ => panic!("{tool}: neither completed nor failed: {line} ({:?})", out.status),
        }
    }

    fn permits(&self, dir: &Path) -> String {
        std::fs::read_to_string(dir.join("policy.permits.rn")).unwrap_or_default()
    }
}

/// A stub for eidolon that answers `policy -- aoide mail send x` the way a working guard does: the table is the one `[policy] path` of
/// the `--config` it was given, the tier is Deny and the reason is the guard's sentence.
fn passing_stub() -> String {
    r#"[ "$1" = --version ] && exit 0
cfg=
while [ $# -gt 0 ]; do
  case "$1" in --config) cfg="$2"; shift 2;; *) shift;; esac
done
table=$(sed -n 's/^path = "\(.*\)"$/\1/p' "$cfg" | head -n 1)
echo "table:   $table"
echo "cwd:     /"
echo "command: aoide mail send x"
echo "tier:    Deny"
echo "reason:  @REASON@""#
        .replace("@REASON@", REASON_AOIDE)
}

/// [`passing_stub`], except that its first run appends a line to `policy` first (an eidolon that syncs its shipped table when it starts),
/// counts its runs in `counter`, and, with `fail_after_sync`, allows everything from the second run on.
fn syncing_stub(counter: &Path, policy: &Path, fail_after_sync: bool) -> String {
    let after = if fail_after_sync { "if [ \"$n\" -ge 2 ]; then echo 'tier:    Allow'; exit 0; fi\n" } else { "" };
    format!(
        "[ \"$1\" = --version ] && exit 0\nn=$(cat '{c}' 2>/dev/null || echo 0)\nn=$((n+1))\necho $n > '{c}'\nif [ \"$n\" = 1 ]; then printf '\\n// synced by an eidolon upgrade\\n' >> '{p}'; fi\n{after}{pass}",
        c = counter.display(),
        p = policy.display(),
        pass = passing_stub().replacen("[ \"$1\" = --version ] && exit 0\n", "", 1),
    )
}

fn mode_of(path: &Path) -> u32 {
    std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path).unwrap().permissions()) & 0o777
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// The real eidolon, over the shell
// ---------------------------------------------------------------------------

/// Shell command lines, as `eidolon policy` takes them, with the sentence each is refused with.
const SHELL_DENIED: &[(&str, &str)] = &[
    ("aoide mail send x", REASON_AOIDE),
    ("env aoide send --id x --submit", REASON_AOIDE),
    ("/home/u/.local/bin/aoide node spawn", REASON_AOIDE),
    ("bash -c \"aoide mail read --for minerva\"", REASON_AOIDE),
    ("timeout 5 aoide pair", REASON_AOIDE),
    ("FOO=bar aoide x", REASON_AOIDE),
    ("echo hi | aoide x", REASON_AOIDE),
    ("cd ~ && aoide pair", REASON_AOIDE),
    ("xargs aoide x", REASON_AOIDE),
    ("find . -exec aoide x \\;", REASON_AOIDE),
    ("ssh host", REASON_SSH),
    ("env ssh -p 22 host", REASON_SSH),
    ("bash -c \"ssh h true\"", REASON_SSH),
    ("cat f && ssh h true", REASON_SSH),
    ("minerva keys add k.pub --confirm F", REASON_KEYS),
    ("/opt/m/minerva keys remove k1", REASON_KEYS),
    ("hoot keys allow-web F", REASON_KEYS),
    ("perch-open.sh --keys-arm F", REASON_KEYS),
    ("minerva web --keys-arm F", REASON_KEYS),
    // I1's real argv.
    ("minerva web --keys-arm 0F7A3A5C447EC7EF --port 48110", REASON_KEYS),
    ("bin/perch-open.sh --keys-arm 0F7A3A5C447EC7EF --port 48110 --token-file /t/x", REASON_KEYS),
    ("hoot keys allow-web 0F7A3A5C447EC7EF", REASON_KEYS),
    // Every candidate code flag; the other ssh programs and wrappers.
    ("pwsh -NoProfile -Command \"ssh host\"", REASON_SSH),
    ("powershell.exe -NoProfile -NonInteractive -Command \"aoide mail send x\"", REASON_AOIDE),
    ("bash -e -c \"ssh host\"", REASON_SSH),
    ("bash -cx \"ssh h\"", REASON_SSH),
    ("bash -c -- \"ssh h\"", REASON_SSH),
    ("sshpass -p x ssh h", REASON_SSH),
    ("autossh -M 0 h", REASON_SSH),
    ("flock /tmp/l aoide x", REASON_AOIDE),
    ("watch -n 5 aoide x", REASON_AOIDE),
    ("echo x | flock /tmp/l aoide x", REASON_AOIDE),
    // The key directory as a word (the one a file operation reaches through a program the rows see).
    ("rsync evil.pub ~/.config/minerva/update-keys/", REASON_KEYFILE),
    ("ls ~/.config/minerva/update-keys", REASON_KEYFILE),
];

/// Lines that name aoide, ssh or keys without running them: the member's table answers, the guard does not.
const SHELL_ALLOWED: &[&str] = &[
    "ls ~/src/aoide",
    "grep -r aoide .",
    "scp a b:c",
    "echo ssh",
    "minerva keys list",
    "cat aoide.log",
    "rsync -e ssh a b:c",
    "echo \"ssh aoide\"",
    "ls | grep ssh",
    "ssh-keygen -t ed25519 -f k",
    "git status",
    "rm -rf x",
    "sudo ls",
    "frobnicate --now",
    "python3 -c \"print('aoide ssh')\"",
    "node -e \"console.log('ssh')\"",
    "bash -e script.sh",
    "ruby -e \"puts 'aoide'\"",
    "flock /tmp/l ls aoide",
    "echo if ssh then",
    "grep -rn update-keys installer",
];

/// The generated configs, loaded by the real eidolon: the deny corpus is refused under auto, under `--yolo` (a refusal is not a question,
/// so yolo does not answer it) and under manual, with the sentence; everything else is what the member's own config says. Mutations: the
/// guard file left out of `[policy] path`; the rows after the member's table; a manual policy built without the rows; yolo given the
/// rows' `flag`.
#[test]
fn real_eidolon_refuses_the_deny_corpus_in_auto_yolo_and_manual() {
    let Some(tree) = Tree::real("") else { return };
    let generated = tree.generate();
    assert!(generated.status.on, "{:?}", generated.status);
    let manual = tree.manual();
    let member = &tree.member;
    for (command, sentence) in SHELL_DENIED {
        let control = tree.verdict(&[], member, command);
        assert_ne!(control.tier, "Deny", "`{command}` is not a stock refusal: the guard's is the only one ({})", control.text);
        for (name, flags, config) in [("auto", &[][..], &generated.config), ("yolo", &["--yolo"][..], &generated.config), ("manual", &[][..], &manual)] {
            let v = tree.verdict(flags, config, command);
            assert_eq!(v.tier, "Deny", "{name}: `{command}`\n{}", v.text);
            assert_eq!(v.harness, "refused", "{name}: `{command}` is refused, not asked");
            // A refused stage of a pipeline or a chain is reported by the algebra in its own words ("unsafe pipeline stage blocked"): the
            // table has no say in the sentence there, so the agent gets the refusal and not the pointer.
            let compound = command.contains('|') || command.contains("&&");
            let said = v.reason.contains(sentence) || (compound && v.reason.contains("blocked"));
            assert!(said, "{name}: `{command}`: {}", v.reason);
        }
    }
    for command in SHELL_ALLOWED {
        let control = tree.verdict(&[], member, command);
        for (name, flags) in [("auto", &[][..]), ("yolo", &["--yolo"][..])] {
            let v = tree.verdict(flags, &generated.config, command);
            assert_eq!((&v.tier, &v.reason, &v.reads), (&control.tier, &control.reason, &control.reads), "{name}: `{command}` is the member's answer\n{}", v.text);
        }
        let v = tree.verdict(&[], &manual, command);
        assert_ne!(v.tier, "Allow", "manual still asks about `{command}`");
        assert!(control.tier != "Deny" || v.tier == "Deny", "and keeps the member's refusals: `{command}`");
    }
}

/// The guard file sits beside the member's policy, mode 0600, named in the generated config with the gate forced on; a second start
/// writes the same bytes, and a changed policy is picked up. Mutations: write it under the perch home; leave a temp file behind; skip the
/// chmod.
#[test]
fn the_guard_file_is_beside_the_members_policy_and_a_start_rewrites_it() {
    let Some(tree) = Tree::real("") else { return };
    let first = tree.generate();
    let file = tree.guard_file();
    assert_eq!(first.status.file.as_deref(), Some(file.as_path()));
    assert!(first.status.on && first.status.why.is_empty(), "{:?}", first.status);
    assert_eq!(mode_of(&file), 0o600);
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("minerva-guard-") && name.ends_with(".rn") && name.len() == "minerva-guard-".len() + 8 + ".rn".len(), "a file per home: {name}");
    assert!(!names_in(&tree.config_dir).iter().any(|n| n.ends_with(".tmp")), "{:?}", names_in(&tree.config_dir));
    assert!(!names_in(&tree.home).iter().any(|n| n.contains("guard")), "the perch home holds no guard file: {:?}", names_in(&tree.home));
    let config: toml::Table = std::fs::read_to_string(&first.config).unwrap().parse().unwrap();
    assert_eq!(config["policy"]["path"].as_str(), file.to_str());
    assert_eq!(config["policy"]["enabled"].as_bool(), Some(true));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("const MINERVA_GUARD_AOIDE =") && text.contains("fn minerva_stock_classify_program("), "the stock table, renamed, and the rows");
    // The member's own table is untouched: it is the input, never the output.
    assert!(std::fs::read_to_string(tree.config_dir.join("policy.rn")).unwrap().contains("pub fn classify_program("));
    // Again: the same file; and after the member edits their table, a different one carrying the edit.
    let second = tree.generate();
    assert!(second.status.on, "{:?}", second.status);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    let policy = tree.config_dir.join("policy.rn");
    let edited = std::fs::read_to_string(&policy).unwrap() + "\n// the member's own note\n";
    std::fs::write(&policy, &edited).unwrap();
    let third = tree.generate();
    assert!(third.status.on, "{:?}", third.status);
    assert!(std::fs::read_to_string(&file).unwrap().contains("// the member's own note"));
}

// ---------------------------------------------------------------------------
// The real eidolon, over tools and the permits store
// ---------------------------------------------------------------------------

/// One permits store: `policy.permits.rn` beside the member's policy is the file the guard file reads too, so a vouch written under the
/// member's config is honoured under the guarded one and the other way about, and no second store appears. Mutations: put the guard file
/// in a directory of its own; name a different policy path for the permits store to follow.
#[test]
fn the_member_and_the_guard_share_one_permits_store() {
    let Some(tree) = Tree::real("") else { return };
    tree.write_term_plugin();
    let guarded = tree.generate();
    assert!(guarded.status.on, "{:?}", guarded.status);
    let (member, config) = (&tree.member, &guarded.config);
    // Nothing vouched: the guarded config asks (and no user is there to say yes).
    assert!(tree.call(&[], config, "term_list").unwrap_err().contains("declined"));
    // The member vouches under their own config; the guarded one honours it. The unvouched verb still asks.
    let out = tree.run(&[], None, &["plugins", "trust", "term", "list"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(tree.call(&[], config, "term_list").unwrap(), "ran term_list");
    assert!(tree.call(&[], config, "term_open").unwrap_err().contains("declined"));
    // A vouch written under the guarded config is the member's too.
    let out = tree.run(&[], Some(config), &["plugins", "trust", "term", "open"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(tree.call(&[], member, "term_open").unwrap(), "ran term_open");
    let store = tree.permits(&tree.config_dir);
    assert!(store.contains("verb: \"term_list\"") && store.contains("verb: \"term_open\""), "{store}");
    let stores: Vec<String> = names_in(&tree.config_dir).into_iter().filter(|n| n.contains("permits")).collect();
    assert_eq!(stores, ["policy.permits.rn"], "one store");
    // A vouch is for the script it saw: change a vouched verb and it asks again, under the guard as anywhere.
    let list = tree.base.join("plugins/term/tools/list.rn");
    std::fs::write(&list, std::fs::read_to_string(&list).unwrap() + "// edited\n").unwrap();
    assert!(tree.call(&[], config, "term_list").unwrap_err().contains("declined"));
}

/// D6 through the real gate: with the installer's vouches (everything but `term_send`), auto runs list, open, read and close and asks about
/// send; yolo runs all five; manual asks about all five whatever is vouched, because its reason prefix is not the fallthrough a vouch
/// answers. Mutations: vouch `term_send` too (auto's send goes green); lose the manual prefix (manual's vouched four run).
#[test]
fn the_term_matrix_through_the_real_gate() {
    let Some(tree) = Tree::real("") else { return };
    tree.write_term_plugin();
    let auto = tree.generate();
    assert!(auto.status.on, "{:?}", auto.status);
    let manual = tree.manual();
    for verb in ["list", "open", "read", "close"] {
        for store in [None, Some(&manual)] {
            let out = tree.run(&[], store.map(|p| p.as_path()), &["plugins", "trust", "term", verb]);
            assert!(out.status.success(), "{out:?}");
        }
    }
    let all = ["term_list", "term_open", "term_read", "term_close", "term_send"];
    for tool in &all[..4] {
        assert_eq!(tree.call(&[], &auto.config, tool).unwrap(), format!("ran {tool}"), "auto: {tool} is vouched");
    }
    assert!(tree.call(&[], &auto.config, "term_send").unwrap_err().contains("declined"), "auto asks about term_send");
    for tool in all {
        assert_eq!(tree.call(&["--yolo"], &auto.config, tool).unwrap(), format!("ran {tool}"), "yolo runs {tool}");
        assert!(tree.call(&[], &manual, tool).unwrap_err().contains("declined"), "manual asks about {tool}, vouched or not");
    }
    // Vouching send in manual's own store changes nothing there either.
    let out = tree.run(&[], Some(&manual), &["plugins", "trust", "term", "send"]);
    assert!(out.status.success(), "{out:?}");
    assert!(tree.call(&[], &manual, "term_send").unwrap_err().contains("declined"));
}

/// The credential pre-grants: each token file to its tools in manual's own store, six rows, the member's store untouched, and a path that
/// is not absolute (or holds a control character) never reaches eidolon. Mutations: grant under the member's config; skip the validation.
#[test]
fn pregrant_puts_each_token_file_on_its_tools_in_manuals_store() {
    let Some(tree) = Tree::real("") else { return };
    let generated = tree.generate();
    assert!(generated.status.on, "{:?}", generated.status);
    let manual = tree.manual();
    let (term, mail) = (tree.base.join("term.token"), tree.base.join("mail.token"));
    for file in [&term, &mail] {
        std::fs::write(file, "placeholder, not a secret\n").unwrap();
    }
    let failed = perch::mode::pregrant(&tree.eidolon, &manual, perch::mode::AgentFiles { term: Some(&term), mail: Some(&mail) });
    assert!(failed.is_empty(), "{failed:?}");
    let store = tree.permits(&tree.home);
    for tool in ["term_list", "term_open", "term_read", "term_send", "term_close"] {
        assert!(store.contains(&format!("tool: \"{tool}\", source: \"file:{}\"", term.display())), "{tool}\n{store}");
    }
    assert!(store.contains(&format!("tool: \"mail_send\", source: \"file:{}\"", mail.display())), "{store}");
    assert_eq!(store.matches("tool: \"").count(), 6, "six rows and no more:\n{store}");
    assert!(!store.contains("placeholder"), "the file's content never passes through");
    assert!(!tree.permits(&tree.config_dir).contains("term_open"), "the member's store is theirs");

    // What is not a plain absolute path is refused here, before eidolon: one sentence each, and no new row.
    let before = tree.permits(&tree.home);
    for bad in ["relative.token", "../up.token", "/tmp/with\nnewline", "/tmp/with\u{7}bell", ""] {
        let bad = PathBuf::from(bad);
        let failed = perch::mode::pregrant(&tree.eidolon, &manual, perch::mode::AgentFiles { term: Some(&bad), mail: None });
        assert_eq!(failed.len(), 1, "{bad:?}: {failed:?}");
        assert!(failed[0].contains("not an absolute path"), "{failed:?}");
    }
    assert_eq!(tree.permits(&tree.home), before);
    // And a failing eidolon is a sentence per tool, not a panic and not a hang.
    let broken = Tree::stub("", "echo nope >&2\nexit 1");
    let failed = perch::mode::pregrant(&broken.eidolon, &manual, perch::mode::AgentFiles { term: None, mail: Some(&mail) });
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].contains("mail_send") && failed[0].contains("nope"), "{failed:?}");
}

// ---------------------------------------------------------------------------
// When the guard cannot be on
// ---------------------------------------------------------------------------

/// What the config the perch hands a chat says about `[policy]`, parsed.
fn policy_table(config: &Path) -> Option<toml::Table> {
    let table: toml::Table = std::fs::read_to_string(config).unwrap().parse().unwrap();
    table.get("policy").and_then(|p| p.as_table()).cloned()
}

/// A table that does not have the four entry points once each cannot be wrapped: guard off with a sentence, the member's config stands,
/// nothing is left beside their policy. Mutations: leave the config pointing at the guard file anyway; keep the file.
#[test]
fn a_table_that_cannot_be_wrapped_leaves_the_members_config_and_says_why() {
    for table in [
        "pub fn classify_tool(tool, approval, input) { #{decision: \"allow\", reason: \"x\", read_only: true} }\n",
        "pub fn classify_tool(a, b, c) { 1 }\npub fn classify_program(a, b) { 1 }\npub fn fs_verb_tier(a) { 1 }\n",
        "pub fn classify_tool(a, b, c) { 1 }\npub fn classify_program(a, b) { 1 }\npub fn fs_verb_tier(a) { 1 }\npub fn bash_default(a) { 1 }\npub fn bash_default(a) { 2 }\n",
        "pub fn classify_tool(a, b, c) { 1 }\npub fn classify_program(a, b) { 1 }\npub fn fs_verb_tier(a) { 1 }\npub fn bash_default(a) { 1 }\nfn minerva_stock_bash_default(a) { 3 }\n",
    ] {
        let tree = Tree::bare("");
        let mine = tree.config_dir.join("mine.rn");
        std::fs::write(&tree.member, format!("[swarm]\nenabled = false\n[policy]\npath = '{}'\n", mine.display())).unwrap();
        std::fs::write(&mine, table).unwrap();
        let generated = tree.generate();
        assert!(!generated.status.on && generated.status.file.is_none(), "{:?}", generated.status);
        assert!(!generated.status.why.is_empty(), "a sentence, not nothing");
        assert_eq!(policy_table(&generated.config).unwrap()["path"].as_str(), mine.to_str(), "the member's own policy stands");
        assert!(!tree.guard_file().exists());
    }
}

/// A policy path the guard cannot write beside: not absolute, in a directory that is not there (not made), or in one that cannot be
/// written. Off with a sentence each; nothing created. Mutations: `create_dir_all` the directory; ignore the write error.
#[test]
fn a_policy_directory_that_cannot_hold_the_guard_file_turns_it_off() {
    let tree = Tree::bare("");
    let set = |policy: &str| std::fs::write(&tree.member, format!("[swarm]\nenabled = false\n[policy]\npath = '{policy}'\n")).unwrap();

    set("policy.rn");
    let generated = tree.generate();
    assert!(!generated.status.on && generated.status.why.contains("not absolute"), "{:?}", generated.status);

    let nowhere = tree.base.join("nowhere/policy.rn");
    set(nowhere.to_str().unwrap());
    let generated = tree.generate();
    assert!(!generated.status.on && generated.status.why.contains("does not exist"), "{:?}", generated.status);
    assert!(!nowhere.parent().unwrap().exists(), "and the directory was not made");

    let locked = tree.base.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("policy.rn"), stock_table()).unwrap();
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o555)).unwrap();
    let can_write_anyway = std::fs::write(locked.join("probe"), "x").is_ok(); // root, or a filesystem without modes
    set(locked.join("policy.rn").to_str().unwrap());
    let generated = tree.generate();
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    if !can_write_anyway {
        assert!(!generated.status.on && generated.status.why.contains("cannot be written"), "{:?}", generated.status);
        assert_eq!(names_in(&locked), ["policy.rn"]);
    }
}

/// A start that cannot get a refusal out of eidolon does not claim the guard: a stub that allows, one that fails, and no eidolon at all.
/// The config goes back to the member's and the file is removed, so nothing names a guard that was never proved. Mutations: skip the
/// probe; leave the config; leave the file.
#[test]
fn a_probe_that_does_not_see_a_refusal_turns_the_guard_off_and_restores_the_config() {
    let cases: [(&str, &str); 3] = [
        ("echo 'tier:    Allow'", "was not refused"),
        ("echo 'tier:    Flag'", "was not refused"),
        ("echo 'it broke' >&2\nexit 1", "no verdict"),
    ];
    for (script, expected) in cases {
        let tree = Tree::stub("", script);
        let generated = tree.generate();
        assert!(!generated.status.on && generated.status.file.is_none(), "{script}: {:?}", generated.status);
        assert!(generated.status.why.contains(expected), "{script}: {:?}", generated.status);
        assert!(policy_table(&generated.config).is_none(), "{script}: the member's config has no [policy]");
        assert!(!tree.guard_file().exists(), "{script}: the file went with the claim");
    }
    let tree = Tree::bare("");
    let generated = tree.generate();
    assert!(!generated.status.on && generated.status.why.contains("could not run eidolon"), "{:?}", generated.status);
    assert!(policy_table(&generated.config).is_none() && !tree.guard_file().exists());
}

/// `generate` with no eidolon in hand wraps a table already on disk and names it in the config, but does not call the guard on: nothing has
/// yet proved that the member's eidolon refuses with it. The status says so. Mutation: report on.
#[test]
fn generating_without_eidolon_writes_the_file_but_does_not_claim_it() {
    let tree = Tree::bare("");
    let _turn = GENERATING.lock().unwrap_or_else(|e| e.into_inner());
    let config = perch::config::generate(&tree.home, &tree.member).unwrap();
    let status = perch::guard::status();
    assert!(!status.on && status.why.contains("not yet been asked"), "{status:?}");
    assert!(tree.guard_file().is_file());
    assert_eq!(policy_table(&config).unwrap()["path"].as_str(), tree.guard_file().to_str());
}

/// A member whose config says `[policy] enabled = false` gets the gate back under Minerva, and is told. Real eidolon: the guarded config
/// refuses `aoide mail send x` and does not print the "nothing is gated" note the member's own does. Mutation: leave `enabled` alone.
#[test]
fn a_member_with_the_gate_off_gets_it_on_and_a_note() {
    let Some(tree) = Tree::real("\n[policy]\nenabled = false\n") else { return };
    let generated = tree.generate();
    assert!(generated.status.on, "{:?}", generated.status);
    assert!(generated.status.why.contains("enabled = false"), "{:?}", generated.status);
    assert_eq!(policy_table(&generated.config).unwrap()["enabled"].as_bool(), Some(true));
    let guarded = tree.verdict(&[], &generated.config, "aoide mail send x");
    assert_eq!(guarded.tier, "Deny");
    assert!(!guarded.text.contains("nothing is gated"), "{}", guarded.text);
    let own = tree.verdict(&[], &tree.member, "ls");
    assert!(own.text.contains("nothing is gated"), "control: the member's own config says it: {}", own.text);
    // Their gate was off, so nothing was ever asked: the unrecognised command stays unasked rather than becoming a question.
    assert_eq!(tree.verdict(&[], &generated.config, "frobnicate --now").tier, "Allow");
}

/// Adding the guard adds a `[policy]` entry, and an entry changes what an unrecognised command gets unless the member's config already
/// had one. The member's posture is kept whichever it was: no entry (allow), an entry that says nothing (eidolon's flag), an entry that
/// says deny. Mutations: write the entry without `default`; always write `default = "allow"`.
#[test]
fn the_guard_does_not_change_what_an_unrecognised_command_gets() {
    for (tail, tier) in [("", "Allow"), ("\n[policy]\njudge = \"fau:cheap\"\n", "Flag"), ("\n[policy]\ndefault = \"deny\"\n", "Deny"), ("\n[policy]\ndefault = \"flag\"\n", "Flag")] {
        let Some(tree) = Tree::real(tail) else { return };
        let generated = tree.generate();
        assert!(generated.status.on, "{tail:?}: {:?}", generated.status);
        let manual = tree.manual();
        for command in ["frobnicate --now", "minerva keys list"] {
            let own = tree.verdict(&[], &tree.member, command);
            let guarded = tree.verdict(&[], &generated.config, command);
            assert_eq!((own.tier.as_str(), own.reason.as_str()), (tier, "unrecognized command"), "{tail:?}: control, the member's own: {}", own.text);
            assert_eq!((&guarded.tier, &guarded.reason), (&own.tier, &own.reason), "{tail:?}: `{command}`");
            assert_ne!(tree.verdict(&[], &manual, command).tier, "Allow", "{tail:?}: manual asks (or refuses) whatever the posture");
        }
    }
}

/// A member whose gate was off wrote their `judge` and `yolo` for a gate that never ran. With the gate forced on they would start
/// answering questions for the operator, so the guard drops them (as the manual config does) and the note says so. Mutation: leave them.
#[test]
fn a_member_with_the_gate_off_loses_judge_and_yolo_and_is_told() {
    let tree = Tree::stub("\n[policy]\nenabled = false\nyolo = true\njudge = \"x\"\n", &passing_stub());
    let generated = tree.generate();
    assert!(generated.status.on, "{:?}", generated.status);
    let policy = policy_table(&generated.config).unwrap();
    assert_eq!((policy["enabled"].as_bool(), policy["yolo"].as_bool()), (Some(true), Some(false)));
    assert!(!policy.contains_key("judge"), "{policy:?}");
    assert!(generated.status.why.contains("enabled = false"), "the loaded tab matches this: {:?}", generated.status);
    assert!(generated.status.why.contains("judge") && generated.status.why.contains("yolo"), "{:?}", generated.status);
    // A member whose gate was on keeps theirs.
    let tree = Tree::stub("\n[policy]\nyolo = true\njudge = \"x\"\n", &passing_stub());
    let generated = tree.generate();
    assert!(generated.status.on && generated.status.why.is_empty(), "{:?}", generated.status);
    let policy = policy_table(&generated.config).unwrap();
    assert_eq!((policy["yolo"].as_bool(), policy["judge"].as_str()), (Some(true), Some("x")));
}

// ---------------------------------------------------------------------------
// The file's life: a name per home, the order of restore and remove, a table that changes, an eidolon that hangs
// ---------------------------------------------------------------------------

/// Two homes over one eidolon config each have a guard file of their own, in the one directory (so the shared permits store is still
/// found beside them), and the second's probe failing removes the second's file and never the first's. Mutations: one file name for every
/// home; remove by directory glob.
#[test]
fn two_homes_over_one_config_keep_their_own_guard_files() {
    let tree = Tree::stub("", &passing_stub());
    let first = tree.generate();
    assert!(first.status.on, "{:?}", first.status);
    let one = tree.guard_file();
    assert_eq!(first.status.file.as_deref(), Some(one.as_path()));
    let home2 = tree.base.join("Minerva2");
    std::fs::create_dir(&home2).unwrap();
    let two = tree.config_dir.join(perch::guard::file_name(&home2));
    assert_ne!(one, two, "a file per home");
    assert_eq!(one.parent(), two.parent(), "in one directory");

    // The second home's probe fails (an eidolon that allows everything): its file goes, the first's stays, the first's config still names it.
    let allows = tree.base.join("bin/eidolon-allows");
    executable(&allows, "#!/bin/sh\n[ \"$1\" = --version ] && exit 0\necho 'tier:    Allow'\n");
    let failed = tree.generate_at(&home2, &allows);
    assert!(!failed.status.on && failed.status.why.contains("was not refused"), "{:?}", failed.status);
    assert!(one.is_file(), "the first home's guard file survived the second's failing probe");
    assert!(!two.exists(), "and the second's is gone");
    assert_eq!(policy_table(&first.config).unwrap()["path"].as_str(), one.to_str());
    assert!(policy_table(&failed.config).is_none(), "the second home is back on the member's own config");

    // With a working eidolon both are there at once, side by side.
    let both = tree.generate_at(&home2, &tree.eidolon);
    assert!(both.status.on, "{:?}", both.status);
    assert!(one.is_file() && two.is_file());
    let guards: Vec<String> = names_in(&tree.config_dir).into_iter().filter(|n| n.starts_with("minerva-guard-")).collect();
    assert_eq!(guards.len(), 2, "{guards:?}");
    assert_eq!(policy_table(&both.config).unwrap()["path"].as_str(), two.to_str());
}

/// The config goes back before the file goes: if it cannot be put back the start fails and the file is left where the config may still
/// name it, never the other way round. The home is made read-only by the probe itself. Mutation: remove the file first.
#[test]
fn a_config_that_cannot_be_restored_stops_the_start_and_keeps_the_guard_file() {
    let mut tree = Tree::bare("");
    let lock = tree.base.join("scratch-lock");
    std::fs::create_dir(&lock).unwrap();
    std::fs::set_permissions(&lock, std::os::unix::fs::PermissionsExt::from_mode(0o555)).unwrap();
    let can_write_anyway = std::fs::write(lock.join("x"), "x").is_ok(); // root, or a filesystem without modes
    std::fs::set_permissions(&lock, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    if can_write_anyway {
        eprintln!("guard: this user can write into a 0555 directory; the restore-failure check is skipped");
        return;
    }
    tree.eidolon = tree.base.join("bin/eidolon-locks");
    executable(&tree.eidolon, &format!("#!/bin/sh\n[ \"$1\" = --version ] && exit 0\nchmod 555 '{}'\necho 'tier:    Allow'\n", tree.home.display()));
    let _turn = GENERATING.lock().unwrap_or_else(|e| e.into_inner());
    let result = perch::config::generate_with(&tree.home, &tree.member, Some(&tree.eidolon));
    let status = perch::guard::status();
    std::fs::set_permissions(&tree.home, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("could not be put back"), "{error}");
    assert!(tree.guard_file().is_file(), "the file stays, because the config may still name it");
    assert!(!status.on, "{status:?}");
    let config: toml::Table = std::fs::read_to_string(tree.home.join("config.toml")).unwrap().parse().unwrap();
    assert_eq!(config["policy"]["path"].as_str(), tree.guard_file().to_str(), "and the config still names it: a file that is there");
}

/// eidolon syncs its shipped tables when it starts, so the first run (the probe) can be the one that changes the member's table. The
/// guard is then made again from the new text and probed once more; when that second probe fails the guard is off, the config is the
/// member's and the file is gone. Mutations: skip the second staging; skip the second probe.
#[test]
fn a_table_that_eidolon_syncs_during_the_probe_is_wrapped_again_and_probed_again() {
    let mut tree = Tree::bare("");
    let (counter, policy) = (tree.base.join("probes"), tree.config_dir.join("policy.rn"));
    tree.eidolon = tree.base.join("bin/eidolon-syncs");
    executable(&tree.eidolon, &format!("#!/bin/sh\n{}\n", syncing_stub(&counter, &policy, false)));
    let generated = tree.generate();
    assert!(generated.status.on, "{:?}", generated.status);
    assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "2", "probed twice");
    assert!(std::fs::read_to_string(tree.guard_file()).unwrap().contains("// synced by an eidolon upgrade"), "the guard was made again from the synced table");

    let mut tree = Tree::bare("");
    let (counter, policy) = (tree.base.join("probes"), tree.config_dir.join("policy.rn"));
    tree.eidolon = tree.base.join("bin/eidolon-syncs");
    executable(&tree.eidolon, &format!("#!/bin/sh\n{}\n", syncing_stub(&counter, &policy, true)));
    let generated = tree.generate();
    assert!(!generated.status.on && generated.status.why.contains("was not refused"), "{:?}", generated.status);
    assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "2");
    assert!(!tree.guard_file().exists() && policy_table(&generated.config).is_none(), "the file went with the claim, and the config is the member's");
}

/// An eidolon that never answers does not hold the perch: the probe is cut off at its limit (6 s, where `perch-open.sh` waits 20), the
/// child is killed, and the guard is off with that sentence. Mutation: wait for the child.
#[test]
fn a_hung_eidolon_turns_the_guard_off_within_the_probe_limit() {
    let tree = Tree::stub("", "[ \"$1\" = --version ] && exit 0\nexec sleep 60");
    let started = std::time::Instant::now();
    let generated = tree.generate();
    let took = started.elapsed();
    assert!(!generated.status.on && generated.status.why.contains("no answer within 6 s"), "{:?}", generated.status);
    assert!(took < std::time::Duration::from_secs(19), "{took:?}: well inside what perch-open.sh waits for");
    assert!(!tree.guard_file().exists() && policy_table(&generated.config).is_none());
}

/// A probe that is refused for another reason, or by another table, is not the guard: the sentence must be the guard's and the `table:`
/// line must name the guard file. Mutations: accept any Deny; do not look at the table line.
#[test]
fn a_probe_must_be_refused_with_the_guards_sentence_by_the_guards_file() {
    let elsewhere = format!("echo 'table:   /elsewhere/policy.rn'\necho 'tier:    Deny'\necho 'reason:  {REASON_AOIDE}'");
    let stock_reason = "echo \"table:   $(sed -n 's/^path = \"\\(.*\\)\"$/\\1/p' \"$2\" | head -n 1)\"\necho 'tier:    Deny'\necho 'reason:  a stock refusal'";
    for (script, expected) in [(elsewhere.as_str(), "not by the guard file"), (stock_reason, "not with the guard's sentence"), ("echo 'tier:    Deny'", "not by the guard file")] {
        let mut stub = Tree::bare("");
        stub.eidolon = stub.base.join("bin/eidolon-odd");
        executable(&stub.eidolon, &format!("#!/bin/sh\n[ \"$1\" = --version ] && exit 0\n{script}\n"));
        let generated = stub.generate();
        assert!(!generated.status.on && generated.status.why.contains(expected), "{script}: {:?}", generated.status);
        assert!(!stub.guard_file().exists(), "{script}");
    }
}

/// A first start has no config directory at all: eidolon makes it when it unpacks its tables, so the unpack comes before the directory
/// is looked for, and the guard is staged in the directory it made. (The one command run is `policy true`, which classifies and starts
/// nothing.) Mutation: look for the directory first.
#[test]
fn a_first_start_with_no_config_directory_unpacks_before_the_guard_is_staged() {
    let Some(tree) = Tree::real("") else { return };
    std::fs::remove_dir_all(&tree.config_dir).unwrap();
    let generated = tree.generate();
    assert!(generated.status.on, "{:?}", generated.status);
    assert!(tree.config_dir.is_dir() && tree.guard_file().is_file(), "{:?}", names_in(&tree.base.join("config")));
    assert_eq!(tree.verdict(&[], &generated.config, "aoide mail send x").tier, "Deny");
}

// ---------------------------------------------------------------------------
// The real eidolon: policy-rows, the shared store with a credential, and the measured reach of the rows
// ---------------------------------------------------------------------------

/// `eidolon policy-rows` compares the string arms of `pub fn classify_tool` in the policy in force with the shipped table. Under the
/// guarded config it says what it says under the member's own, because only `classify_program` is renamed; under the manual policy,
/// which renames `classify_tool`, it reports a difference (the control that says this test can see one). Mutation: rename all four in
/// the guard.
#[test]
fn policy_rows_under_the_guarded_config_reads_the_members_own_rows() {
    let Some(tree) = Tree::real("") else { return };
    let generated = tree.generate();
    assert!(generated.status.on, "{:?}", generated.status);
    let manual = tree.manual();
    let rows = |config: &Path| {
        let out = tree.run(&[], Some(config), &["policy-rows"]);
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.starts_with("yours:")).collect::<Vec<_>>().join("\n")
    };
    let own = rows(&tree.member);
    assert!(own.contains("identical"), "control, the member's own table is the shipped one: {own}");
    assert_eq!(rows(&generated.config), own, "auto and yolo (one config) read the member's rows");
    assert!(!rows(&manual).contains("identical"), "control: a renamed classify_tool is a difference to this view");
}

/// A credential grant is in the member's store like a vouch, and the guard file reads that store: a tool the member granted runs under
/// the guarded config with no question, and under a config in some other directory that copies only the guarded `[policy] path`. Without
/// the grant it asks (declined, for want of a person). Mutations: put the guard file in a directory of its own; copy the store.
#[test]
fn a_credential_grant_in_the_members_store_is_honoured_under_the_guard_and_under_a_config_elsewhere() {
    let Some(tree) = Tree::real("") else { return };
    let token = tree.base.join("cred.token");
    std::fs::write(&token, "placeholder, not a secret\n").unwrap();
    tree.write_cred_plugin(&token);
    let guarded = tree.generate();
    assert!(guarded.status.on, "{:?}", guarded.status);
    let out = tree.run(&[], None, &["plugins", "trust", "cred", "get"]);
    assert!(out.status.success(), "{out:?}");
    let before = tree.call(&[], &guarded.config, "cred_get").unwrap_err();
    assert!(before.contains("has no grant covering it"), "no grant yet: the credential is a question, and no one answers it: {before}");

    let source = format!("file:{}", token.display());
    let out = tree.run(&[], None, &["plugins", "grant", "cred_get", &source]);
    assert!(out.status.success(), "{out:?}");
    assert!(tree.permits(&tree.config_dir).contains("cred_get"), "the grant is in the member's store");

    // Under the guarded config: no question. The call goes as far as the endpoint, where nothing listens.
    let asked = |reason: &str| reason.contains("has no grant covering it") || reason.contains("declined");
    let after = tree.call(&[], &guarded.config, "cred_get").unwrap_err();
    assert!(!asked(&after), "no question under the guard: {after}");
    assert!(after.contains("endpoint said"), "it reached the endpoint: {after}");
    // Under yolo too, and under a config in another directory that names only the guarded policy.
    assert!(!asked(&tree.call(&["--yolo"], &guarded.config, "cred_get").unwrap_err()));
    let other_dir = tree.base.join("elsewhere");
    std::fs::create_dir(&other_dir).unwrap();
    let other = other_dir.join("config.toml");
    let policy = guarded.status.file.as_ref().unwrap();
    std::fs::write(
        &other,
        format!(
            "plugins_dir = '{}'\n\n[swarm]\nenabled = false\n\n[policy]\npath = '{}'\nenabled = true\ndefault = \"allow\"\n",
            tree.base.join("plugins").display(),
            policy.display()
        ),
    )
    .unwrap();
    let there = tree.call(&[], &other, "cred_get").unwrap_err();
    assert!(!asked(&there) && there.contains("endpoint said"), "{there}");
    assert_eq!(names_in(&other_dir), ["config.toml"], "and no store of its own appeared there");
}

/// Shapes the rows never see, measured against the real eidolon and pinned: under yolo they run, unasked. They are here so that an
/// eidolon whose decomposition starts to hand them to the table shows up as a failing test (and a decision), and so that nothing in the
/// docs claims more than this. The shapes the rows do reach are pinned next to them, refused under yolo as well.
const PINNED_KNOWN_MISSES: &[&str] = &[
    "for h in a b; do ssh $h uptime; done",
    "(cd x && aoide mail send x)",
    "ssh -fN -L 8080:localhost:80 host &",
    "nohup aoided > l 2>&1 &",
    "if ssh h true; then echo ok; fi",
    "{ ssh h uptime; } 2>&1",
    "out=$(ssh h uptime 2>&1)",
    "f() { ssh h; }; f",
    "eidolon --yolo -p \"ssh host uptime\"",
    // The file verbs are the algebra's own: `cp`, `mv`, `tee`, `touch`, `mkdir` and `ln` are "workspace file ops" that it decides itself,
    // reading `~/x` as a path inside the workspace, and never ask the table. Row 4 does not reach them, in auto either.
    "cp evil.pub ~/.config/minerva/update-keys/",
    "mv evil.pub ~/.config/minerva/update-keys/k.pub",
];

const REACHED_SHAPES: &[&str] = &["ssh h uptime", "cat f | ssh h uptime", "true && ssh h uptime", "true; ssh h uptime", "while test -f flag; do ssh h uptime; done", "echo $(ssh h uptime)"];

#[test]
fn the_shapes_the_rows_reach_are_refused_and_the_shapes_they_miss_run_unasked_in_yolo() {
    let Some(tree) = Tree::real("") else { return };
    let generated = tree.generate();
    assert!(generated.status.on, "{:?}", generated.status);
    let mut wrong = Vec::new();
    for command in REACHED_SHAPES {
        let v = tree.verdict(&["--yolo"], &generated.config, command);
        if (v.tier.as_str(), v.harness.as_str()) != ("Deny", "refused") {
            wrong.push(format!("reached: `{command}`\n{}", v.text));
        }
    }
    for command in PINNED_KNOWN_MISSES {
        let v = tree.verdict(&["--yolo"], &generated.config, command);
        // "Deny (shape, not merits)" is the algebra's own verdict on the whole line, which yolo answers like any question: the harness
        // line says what happens. `eidolon ...` is "the harness itself", which the table lets run silently in every mode.
        if !v.harness.starts_with("runs,") {
            wrong.push(format!("pinned known miss: `{command}` runs under yolo today; if it is refused now the rows reach it: move it to REACHED_SHAPES and the docs\n{}", v.text));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
