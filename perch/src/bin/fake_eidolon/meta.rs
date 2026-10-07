//! `eidolon [--config C] tools | plugins | policy-rows`: the read-only commands behind the loaded and diff tabs (slice R3).
//!
//! Each prints stock text in the shape the real command printed when this was written (checked against the master build, `eidolon tools`
//! and `plugins` with two plugins installed, and `policy-rows` over a stock table), records
//! `helper <name> <args> config=<bool> argv=<the whole argv after the program>` to `$FAKE_LOG` the way the other helpers do (`helper` in
//! `fake_eidolon.rs`), so a test can count the perch's calls and read the exact argv, `--live` or not, and exits 0. `fake_eidolon.rs`
//! calls [`run`] for these three subcommands and nothing else, after the global flags are parsed into a [`Cmd`]; the return value is the
//! process's exit code.
//!
//! - `tools`: pretty-printed JSON objects, back to back, as the real one prints them. Builtins first, then the namespaced plugin tools
//!   (`<plugin>_<file>`); eidolon puts a note on stderr (`[tool] ...`) that a perch must not read.
//! - `plugins`: the listing, `<name> <version>` at column 0 and everything else indented, as the real one.
//! - `policy-rows`: three lines. `yours:` names the policy file of the config it was given (`[policy] path`, else
//!   `$XDG_CONFIG_HOME/eidolon/policy.rn`), which is how a test tells which config the perch asked about.
//!
//! Knobs, by environment: `FAKE_HELPER_FAIL=1` (exit 1, printing nothing, like `sessions` and `log`), `FAKE_META_SLEEP_MS=<ms>` (answer
//! that late), `FAKE_META_BAD=1` (`tools` prints text that is not a JSON stream), `FAKE_META_BIG=<bytes>` (`plugins` and `policy-rows`
//! print that many more bytes of filler, indented so the listing's shape holds).

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use super::Cmd;

/// Answers `tools`, `plugins` and `policy-rows`.
pub fn run(cmd: &Cmd) -> i32 {
    record(cmd);
    if std::env::var_os("FAKE_HELPER_FAIL").is_some() {
        return 1;
    }
    if let Some(ms) = std::env::var("FAKE_META_SLEEP_MS").ok().and_then(|ms| ms.parse().ok()) {
        std::thread::sleep(Duration::from_millis(ms));
    }
    match cmd.name {
        "tools" => tools(),
        "plugins" => {
            plugins();
            filler();
        }
        "policy-rows" => {
            policy_rows(cmd);
            filler();
        }
        _ => return 2,
    }
    0
}

/// `helper <name> <args> config=<bool> argv=<argv>`, one line per call.
fn record(cmd: &Cmd) {
    let Some(path) = std::env::var_os("FAKE_LOG").map(PathBuf::from) else { return };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "helper {} {} config={} argv={}", cmd.name, cmd.args.join(" "), cmd.config.is_some(), argv.join(" "));
    }
}

/// The manifest of one tool, as `serde_json::to_string_pretty` prints a `ToolManifest`.
fn manifest(name: &str, approval: &str, description: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "description": description,
        "input_schema": { "properties": { "path": { "description": "A path", "type": "string" } }, "required": ["path"], "type": "object" },
        "approval": approval,
    })
}

fn tools() {
    eprintln!("[tool] fake_eidolon: a note on stderr that a perch does not read");
    if std::env::var_os("FAKE_META_BAD").is_some() {
        println!("[system] seeded tools/read.rn into /nowhere");
        return;
    }
    let all = [
        manifest("read", "read_only", "Read a file and return its contents with line numbers.\nUse offset and limit to page through large files."),
        manifest("write", "mutating", "Write content to a file, creating it and any parent directories."),
        manifest("bash", "mutating", "\n  Run a shell command.\n"),
        manifest("browser_click", "mutating", "Click an element by ref, in the browser plugin's page."),
        manifest("browser_open", "read_only", "Open a URL in the shared headless browser."),
        manifest("jev_graph", "read_only", "Run a jev graph."),
        manifest("jev_mark", "read_only", ""),
        manifest("tool_search", "read_only", "Search the deferred tools."),
        manifest("wait_for", "read_only", "Park until a condition holds."),
    ];
    for one in all {
        println!("{}", serde_json::to_string_pretty(&one).unwrap());
    }
}

fn plugins() {
    let listing = [
        ("browser", "0.1.0", "one headless Chromium for the whole machine, behind seven tools", &["browser_click", "browser_open"][..]),
        ("jev", "0.2.0", "jev automation graphs as eidolon workflows", &["jev_graph", "jev_mark"][..]),
    ];
    for (name, version, about, verbs) in listing {
        println!("{name} {version}");
        println!("    {about}");
        println!("    at       /fake/config/eidolon/plugins/{name}");
        println!("    record   unmanaged — installed by hand");
        println!("    payloads tools, workflows");
        for verb in verbs {
            println!("    {verb:<28} unvouched — asks");
        }
    }
}

fn policy_rows(cmd: &Cmd) {
    println!("yours:   {} — table-version 1 (2026-09-28)", policy_path(cmd.config));
    println!("shipped: built into this binary — table-version 1 (2026-09-28)");
    println!("rows:    identical (30 rows)");
}

/// The policy file `config` names, else the one eidolon looks for beside its own config (`$XDG_CONFIG_HOME/eidolon/policy.rn`).
fn policy_path(config: Option<&str>) -> String {
    let named = config
        .and_then(|config| std::fs::read_to_string(config).ok())
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|table| table.get("policy")?.get("path")?.as_str().map(str::to_string));
    named.unwrap_or_else(|| {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_default();
        base.join("eidolon").join("policy.rn").display().to_string()
    })
}

/// `FAKE_META_BIG=<bytes>`: more output after the answer, in indented lines of 100 bytes.
fn filler() {
    let Some(mut left) = std::env::var("FAKE_META_BIG").ok().and_then(|n| n.parse::<usize>().ok()) else { return };
    let line = format!("    {}\n", "x".repeat(95));
    let mut out = std::io::stdout().lock();
    while left > 0 {
        let _ = out.write_all(line.as_bytes());
        left = left.saturating_sub(line.len());
    }
}
