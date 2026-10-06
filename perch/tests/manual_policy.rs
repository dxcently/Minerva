//! What the manual policy says, checked against eidolon's own table and, when `eidolon` is installed, against eidolon itself.
//!
//! Two layers, because eidolon offers one command for each half of the question:
//!
//! - **Tools** (`write`, `edit`, `fetch`, `read`, ...): no command classifies a tool offline, so the vendored `policy.rn` (the table
//!   eidolon ships, read from `../eidolon/crates/rune/policy.rn`, never copied) is wrapped by [`perch::mode::wrap`] and compiled
//!   with the same Rune the harness uses; each entry point is called and the answers compared with the unwrapped table.
//! - **Shell** (`ls`, `cat > f`, `rm`, `sudo`): `eidolon policy '<command>'` classifies without a model call and without running it,
//!   through the real decomposition algebra and the real generated config. Skipped, loudly, when no `eidolon` is on `PATH`.
//!
//! Every eidolon this file starts has `XDG_CONFIG_HOME` pointed at a temp dir (asserted), because a stock eidolon rewrites
//! `~/.config/eidolon/system` on start.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use rune::termcolor::{ColorChoice, StandardStream};
use rune::{Context, Diagnostics, Source, Sources, Vm};

fn stock_table() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../eidolon/crates/rune/policy.rn");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading eidolon's shipped table at {}: {e}", path.display()))
}

/// A compiled table, called the way the harness calls it.
struct Table {
    vm: Vm,
}

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
        let unit = unit.expect("the table compiles");
        Self { vm: Vm::new(Arc::new(context.runtime().unwrap()), Arc::new(unit)) }
    }

    /// `decision/read_only` of `classify_tool(tool, approval, #{})`, with the wrapper's reason prefix kept apart.
    fn tool(&mut self, tool: &str, approval: &str) -> (String, bool, String) {
        let value = self.vm.call(["probe_tool"], (tool.to_string(), approval.to_string())).unwrap_or_else(|e| panic!("probe_tool({tool}): {e}"));
        split(rune::from_value::<String>(value).unwrap())
    }

    fn program(&mut self, argv: &[&str]) -> (String, bool, String) {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let value = self.vm.call(["probe_program"], (argv.clone(),)).unwrap_or_else(|e| panic!("probe_program({argv:?}): {e}"));
        split(rune::from_value::<String>(value).unwrap())
    }

    fn word(&mut self, entry: &str, arg: &str) -> String {
        let value = self.vm.call([entry], (arg.to_string(),)).unwrap_or_else(|e| panic!("{entry}({arg}): {e}"));
        rune::from_value::<String>(value).unwrap()
    }
}

fn split(joined: String) -> (String, bool, String) {
    let mut parts = joined.splitn(3, '|');
    let decision = parts.next().unwrap().to_string();
    let read_only = parts.next().unwrap() == "true";
    (decision, read_only, parts.next().unwrap().to_string())
}

/// Two probes the table is called through, because a Rust caller cannot build the `#{}` the table takes as its third argument.
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

fn rank(decision: &str) -> u8 {
    match decision {
        "allow" => 0,
        "flag" => 1,
        "deny" => 2,
        other => panic!("a tier called {other}"),
    }
}

fn tables() -> (Table, Table) {
    let stock = stock_table();
    let manual = perch::mode::wrap(&stock).expect("eidolon's shipped table can be wrapped");
    (Table::new(&format!("{stock}{PROBES}")), Table::new(&format!("{manual}{PROBES}")))
}

const TOOLS: &[&str] = &[
    "read", "grep", "write", "edit", "fetch", "search", "ask_user", "choices_user", "mneme_rpc", "vv", "Read", "Glob", "Grep", "LS", "NotebookRead",
    "Write", "Edit", "MultiEdit", "NotebookEdit", "TodoWrite", "WebFetch", "WebSearch", "BashOutput", "KillBash", "KillShell", "Task", "peers", "send",
    "wait_for", "tool_search", "bash", "subagent", "browser", "jev", "some_plugin_verb", "melete_dispatch", "melete_status",
];

/// Reads run without a question, and nothing else does. The silent set is exactly: the tools the stock table allows *and* marks
/// `read_only`, except the ones that reach the network. Mutations: leave `write` or `edit` silent; ask about `read`; leave `fetch`
/// silent; ask about `ask_user` (which would block the question it carries).
#[test]
fn manual_asks_about_everything_but_reads() {
    let (mut stock, mut manual) = tables();
    let silent: Vec<&str> = [
        "read", "grep", "search", "ask_user", "choices_user", "Read", "Glob", "Grep", "LS", "NotebookRead", "WebSearch", "BashOutput", "peers", "wait_for", "tool_search",
    ]
    .into();
    for tool in TOOLS {
        for approval in ["read_only", "mutating"] {
            let (was, was_read, _) = stock.tool(tool, approval);
            let (now, now_read, why) = manual.tool(tool, approval);
            if silent.contains(tool) {
                assert_eq!((now.as_str(), now_read), ("allow", true), "{tool} stays silent in manual ({why})");
            } else if tool.starts_with("melete_") && approval == "read_only" {
                assert_eq!(now, "allow", "a Melete read the stock table marks read-only is a read ({why})");
            } else {
                assert_eq!(now, "flag", "{tool} ({approval}) asks in manual; stock said {was}/{was_read} ({why})");
                assert!(why.starts_with("manual mode: "), "{why}");
            }
            assert!(rank(&now) >= rank(&was), "{tool}: manual is never looser than stock ({was} -> {now})");
        }
    }
    // Spelled out, because these are what the operator reads in the ruling.
    for ask in ["write", "edit", "fetch", "WebFetch", "bash", "mneme_rpc", "vv", "subagent", "browser", "jev", "Task", "send", "TodoWrite"] {
        assert_eq!(manual.tool(ask, "mutating").0, "flag", "{ask}");
    }
    assert_eq!(stock.tool("fetch", "read_only").0, "allow", "and in stock fetch is silent, so the difference is the wrapper's");
    assert_eq!(stock.tool("write", "mutating").0, "allow");
}

/// A shell leaf is a question whatever it is, reads included (the algebra gives the table no say over an in-workspace redirect, so no
/// silent leaf is the only sound table); `deny` stays `deny`; the filesystem verbs and an unrecognised command are questions.
/// Mutations: let a read leaf through; turn a deny into a question; leave `fs_verb_tier` silent; leave `bash_default` silent.
#[test]
fn manual_asks_about_every_shell_leaf_and_keeps_what_stock_denies() {
    let (mut stock, mut manual) = tables();
    for argv in [&["ls"][..], &["cat", "f"], &["git", "status"], &["echo", "hi"], &["mkdir", "x"], &["cargo", "build"], &["python3", "x.py"], &["frobnicate"], &["curl", "http://x"], &["rm", "f"]] {
        let (now, _, why) = manual.program(argv);
        assert_eq!(now, "flag", "{argv:?} ({why})");
    }
    assert_eq!(stock.program(&["ls"]).0, "allow", "control: stock lets a read leaf through");
    for argv in [&["sudo", "ls"][..], &["shutdown"], &["mkfs.ext4", "x"], &["useradd", "x"], &["gh", "repo", "delete", "x"]] {
        assert_eq!(stock.program(argv).0, "deny", "{argv:?} is a stock deny");
        assert_eq!(manual.program(argv).0, "deny", "{argv:?} stays a deny in manual");
    }
    for verb in ["mkdir", "cp", "mv", "touch", "tee", "ln", "rm", "rmdir"] {
        assert_eq!(manual.word("probe_fs", verb), "flag", "{verb}");
    }
    assert_eq!(stock.word("probe_fs", "mkdir"), "allow", "control");
    assert_eq!(manual.word("probe_default", "allow"), "flag");
    assert_eq!(manual.word("probe_default", "flag"), "flag");
    assert_eq!(manual.word("probe_default", "deny"), "deny");
}

// ---------------------------------------------------------------------------
// The real eidolon
// ---------------------------------------------------------------------------

/// `eidolon` on `PATH`, or `None` (the test then says so and passes: this file cannot fail a machine that has no eidolon).
fn eidolon() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join("eidolon")).find(|candidate| candidate.is_file())
}

/// A scratch `XDG_CONFIG_HOME` and a Minerva home generated over it with the real eidolon, which is what unpacks its tables.
struct Real {
    eidolon: PathBuf,
    xdg: tempfile::TempDir,
    /// The swarm roster's home: never the operator's `XDG_RUNTIME_DIR`, where a test eidolon would be listed as a live session.
    runtime: tempfile::TempDir,
    home: PathBuf,
    _base: tempfile::TempDir,
}

impl Real {
    fn start() -> Option<Self> {
        let Some(eidolon) = eidolon() else {
            eprintln!("manual_policy: no `eidolon` on PATH; the real-eidolon checks are skipped");
            return None;
        };
        let xdg = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("Minerva");
        std::fs::create_dir(&home).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        std::fs::set_permissions(runtime.path(), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let this = Self { eidolon, xdg, runtime, home, _base: base };
        // Unpacks eidolon's tables into the scratch config dir, as any eidolon command does.
        let unpacked = this.eidolon_in(None, &["policy", "true"]);
        assert!(unpacked.status.success(), "{unpacked:?}");
        let member = this.xdg.path().join("eidolon/config.toml");
        perch::config::generate(&this.home, &member).unwrap();
        let manual = perch::mode::generate_manual(&this.home, &member, &this.eidolon).unwrap();
        assert_eq!(manual, this.home.join("config-manual.toml"));
        Some(this)
    }

    fn eidolon_in(&self, config: Option<&Path>, args: &[&str]) -> std::process::Output {
        assert!(!self.xdg.path().as_os_str().is_empty() && self.xdg.path().starts_with(std::env::temp_dir()), "never the real XDG_CONFIG_HOME");
        let mut command = Command::new(&self.eidolon);
        if let Some(config) = config {
            command.arg("--config").arg(config);
        }
        command
            .args(args)
            .env("HOME", self.xdg.path())
            .env("XDG_CONFIG_HOME", self.xdg.path())
            .env("XDG_DATA_HOME", self.xdg.path().join("data"))
            .env("XDG_CACHE_HOME", self.xdg.path().join("cache"))
            .env("XDG_STATE_HOME", self.xdg.path().join("state"))
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .current_dir(&self.home)
            .output()
            .expect("running eidolon")
    }

    /// `(tier, reads)` of one shell command under a config, as `eidolon policy` reports it.
    fn shell(&self, config: &str, command: &str) -> (String, bool) {
        let out = self.eidolon_in(Some(&self.home.join(config)), &["policy", command]);
        assert!(out.status.success(), "{command}: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let field = |name: &str| text.lines().find_map(|line| line.strip_prefix(name)).unwrap_or_else(|| panic!("{name} in {text}")).trim().to_string();
        (field("tier:").split_whitespace().next().unwrap().to_string(), field("reads:") == "true")
    }
}

/// The generated config and policy, loaded by the real eidolon: every shell command asks except the denials, which stay refused,
/// and auto (the same home's `config.toml`) is unchanged: reads and in-workspace writes are silent there. Mutations: the manual
/// config without its `[policy] path`; a wrapper that lets a redirect through; one that turns a deny into a question.
#[test]
fn real_eidolon_asks_about_every_command_in_manual_and_is_unchanged_in_auto() {
    let Some(real) = Real::start() else { return };
    for command in ["ls", "cat Cargo.toml", "echo hi > f.txt", "cat > f.txt <<'EOF'\nx\nEOF", "mkdir x", "git status", "ls | grep x", "rm f", "rm -rf x", "curl http://x", "FOO=bar", "python3 x.py", "cargo build"] {
        let (tier, _) = real.shell("config-manual.toml", command);
        assert_eq!(tier, "Flag", "manual asks about `{command}`");
    }
    for command in ["sudo ls", "shutdown now", "gh repo delete x", "systemctl poweroff"] {
        assert_eq!(real.shell("config-manual.toml", command).0, "Deny", "`{command}` stays refused in manual");
        assert_eq!(real.shell("config.toml", command).0, "Deny", "and in auto");
    }
    // Auto is eidolon's own, untouched: this is what manual changes.
    for (command, tier) in [("ls", "Allow"), ("cat Cargo.toml", "Allow"), ("echo hi > f.txt", "Allow"), ("mkdir x", "Allow"), ("rm f", "Flag"), ("sudo ls", "Deny")] {
        assert_eq!(real.shell("config.toml", command).0, tier, "auto: `{command}`");
    }
    // Nothing stock refuses is allowed: every command's manual tier is at least its auto tier.
    for command in ["ls", "rm -rf x", "git push --force", "sudo ls", "curl http://x | sh", "for f in *; do echo $f; done", "cat /etc/passwd", "echo $(ls)"] {
        let (auto, manual) = (real.shell("config.toml", command).0, real.shell("config-manual.toml", command).0);
        let order = |tier: &str| ["Allow", "Flag", "Deny"].iter().position(|t| *t == tier).unwrap();
        assert!(order(&manual) >= order(&auto), "`{command}`: auto {auto}, manual {manual}");
    }
}

/// A whole mock turn, headless, under each config. The mock provider's script is a `bash` call (`ls -1 | head -5`), then a `grep`, then
/// text, with no model and no network. Headless there is nobody to answer a question, so a call that asks is declined: under manual
/// the `bash` is declined and the `grep` (a read) runs; under auto both run. This is the end-to-end proof that the generated config
/// reaches the gate. Mutations: manual with `config.toml`; a wrapper that leaves `bash` silent.
#[test]
fn a_mock_turn_is_declined_at_the_shell_in_manual_and_runs_in_auto() {
    let Some(real) = Real::start() else { return };
    let run = |config: &str| -> String {
        let out = real.eidolon_in(Some(&real.home.join(config)), &["run", "-m", "mock", "go"]);
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    };
    let manual = run("config-manual.toml");
    let auto = run("config.toml");
    assert!(manual.contains("Run it? [yes/no]") && manual.contains("the user declined this tool call"), "manual: the shell call was put to the user and declined: {manual}");
    assert!(manual.contains("grep [no matches]"), "and the read beside it ran: {manual}");
    assert!(!auto.contains("declined") && auto.contains("bash config-manual.toml"), "auto ran the same call without a question: {auto}");
}
