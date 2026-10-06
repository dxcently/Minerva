//! The three approval modes a chat can run in, and the one generated policy that makes `manual` possible.
//!
//! ```text
//!   manual   --config <home>/config-manual.toml            reads run; everything else asks
//!   auto     --config <home>/config.toml                   eidolon's own rules, unchanged
//!   yolo     --config <home>/config.toml  --yolo           every question is a yes
//! ```
//!
//! **Why a generated policy and not a flag.** eidolon has no "ask about everything" switch, and its policy hook is the one place a
//! gate can be tightened without touching eidolon: `[policy] path` names a Rune file whose `classify_tool`, `classify_program`,
//! `fs_verb_tier` and `bash_default` answer allow / flag / deny. Rune has no `import`, so a file cannot call another file's
//! `classify_tool`; the only way to *wrap* the stock table is to put the wrapper in the same source. So the perch reads the policy
//! in force (the member's `[policy] path`, else `~/.config/eidolon/policy.rn`, else eidolon's own shipped copy in
//! `~/.config/eidolon/system/policy.rn`), renames its four entry points to `minerva_stock_*` (a line-start text edit, refused if a
//! name is not found exactly once), and appends four wrappers that call them. Nothing of the stock table is copied into this
//! repository, so an eidolon update that changes its table is picked up at the next perch start; one that renames an entry point
//! makes `manual` unavailable with a sentence, never silently weaker.
//!
//! **What the wrapper does.** A `deny` stays a `deny`. An `allow` that the table marked `read_only` stays an `allow` for a tool call
//! (`read`, `grep`, `search`, `ask_user`, `tool_search`, `peers`, `wait_for`, `BashOutput`, ...), except `fetch`, which reaches
//! the network and is asked. Every other `allow` and every `flag` is a `flag` (a question). For a shell command **every leaf asks,
//! reads included**: eidolon's decomposition checks an in-workspace redirect (`cat > f`, `echo x >> f`) but gives the table no
//! say in it, so `ls` is silent only because the leaf is, and `cat > f` would be silent too. The one way a table can close that is to
//! make no leaf silent. `fs_verb_tier` becomes `flag` for every verb (`mkdir`, `cp`, `mv`, `touch`, `tee`, `ln`, ...) and
//! `bash_default` becomes `flag` unless it was `deny`. A path outside the working directory is still the algebra's refusal-turned-ask.
//!
//! **The generated config.** `config-manual.toml` is `config.toml` plus `[policy] path`, with `enabled = true` (a member's
//! `enabled = false` would otherwise make manual ungated), `yolo = false` and no `judge` (a judge answers asks on the operator's
//! behalf, which manual is not). `sessions_dir` and `default_model` are the same file's, so chats appear in one list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// The file next to `config.toml` a manual door is given as `--config`.
pub const MANUAL_CONFIG: &str = "config-manual.toml";
/// The generated policy it names.
pub const MANUAL_POLICY: &str = "policy-manual.rn";

/// How a chat's tool calls are approved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Manual,
    Auto,
    #[default]
    Yolo,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Manual => "manual",
            Mode::Auto => "auto",
            Mode::Yolo => "yolo",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "manual" => Some(Mode::Manual),
            "auto" => Some(Mode::Auto),
            "yolo" => Some(Mode::Yolo),
            _ => None,
        }
    }

    /// What the old `{"yolo": bool}` spelling meant.
    pub fn from_yolo(yolo: bool) -> Self {
        if yolo { Mode::Yolo } else { Mode::Auto }
    }

    /// What a door's `hello` says it is, given what the perch asked for: `yolo` is the door's own word, the other two are ours.
    pub fn reported(self, hello_yolo: bool) -> Self {
        match (hello_yolo, self) {
            (true, _) => Mode::Yolo,
            (false, Mode::Yolo) => Mode::Auto,
            (false, other) => other,
        }
    }
}

/// What the perch learned at start about the manual config: the file to pass, or why there is none.
#[derive(Clone, Debug, Default)]
pub struct ModeSetup {
    pub manual_config: Option<PathBuf>,
    pub manual_error: Option<String>,
    pub default_mode: Mode,
}

/// The four entry points the wrapper takes over, by the stock name and the name the stock one is given.
const RENAMED: [(&str, &str); 4] = [
    ("pub fn classify_tool(", "fn minerva_stock_classify_tool("),
    ("pub fn classify_program(", "fn minerva_stock_classify_program("),
    ("pub fn fs_verb_tier(", "fn minerva_stock_fs_verb_tier("),
    ("pub fn bash_default(", "fn minerva_stock_bash_default("),
];

const WRAPPER: &str = r#"

// ===========================================================================
// minerva manual mode: generated by minerva perch, appended to the policy above
// whose four entry points were renamed minerva_stock_*. Do not edit this file;
// it is rewritten at every perch start.
//
// deny stays deny. A silent read_only tool call stays silent, except fetch.
// Everything else is a question. Every shell leaf is a question, reads included,
// because the shell algebra scopes an in-workspace redirect but never asks the
// table about it.
// ===========================================================================

fn minerva_asks(r, ask_reads) {
    if r.decision == "deny" {
        return r;
    }
    if r.decision == "allow" && r.read_only && !ask_reads {
        return r;
    }
    #{decision: "flag", reason: `manual mode: ${r.reason}`, read_only: r.read_only}
}

pub fn classify_tool(tool, approval, input) {
    let r = minerva_stock_classify_tool(tool, approval, input);
    minerva_asks(r, tool == "fetch" || tool == "WebFetch")
}

pub fn classify_program(argv, ctx) {
    minerva_asks(minerva_stock_classify_program(argv, ctx), true)
}

pub fn fs_verb_tier(prog) {
    if minerva_stock_fs_verb_tier(prog) == "deny" {
        return "deny";
    }
    "flag"
}

pub fn bash_default(posture) {
    if minerva_stock_bash_default(posture) == "deny" {
        return "deny";
    }
    "flag"
}
"#;

/// The manual policy: `stock` with its four entry points renamed and the wrappers appended. Fails when an entry point is not found
/// exactly once at the start of a line, because a wrapper over a table it cannot read would be a guess.
pub fn wrap(stock: &str) -> Result<String> {
    let mut out = String::with_capacity(stock.len() + WRAPPER.len());
    let mut seen = [0usize; 4];
    for line in stock.lines() {
        match RENAMED.iter().position(|(from, _)| line.starts_with(from)) {
            Some(i) => {
                seen[i] += 1;
                out.push_str(RENAMED[i].1);
                out.push_str(&line[RENAMED[i].0.len()..]);
            }
            None => out.push_str(line),
        }
        out.push('\n');
    }
    for (i, (from, _)) in RENAMED.iter().enumerate() {
        if seen[i] != 1 {
            bail!("eidolon's policy table has `{}` {} times at the start of a line (once is what the manual wrapper can take over)", from.trim_end_matches('('), seen[i]);
        }
    }
    out.push_str(WRAPPER);
    Ok(out)
}

/// `~/x` the way eidolon reads a path in its config.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// The policy text auto mode runs under: the member's `[policy] path`, else `<config dir>/policy.rn`, else eidolon's own shipped
/// copy under `<config dir>/system/`. `member` is the member's `config.toml`; its directory is eidolon's config dir.
fn policy_in_force(member: &Path, config: &Path, eidolon: &Path) -> Result<String> {
    let dir = member.parent().context("the member's config has no directory")?.to_path_buf();
    let named = std::fs::read_to_string(member)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|table| table.get("policy")?.get("path")?.as_str().map(expand_home));
    let candidates = [named.unwrap_or_else(|| dir.join("policy.rn")), dir.join("system").join("policy.rn")];
    let read = |candidates: &[PathBuf]| candidates.iter().find_map(|path| std::fs::read_to_string(path).ok());
    if let Some(text) = read(&candidates) {
        return Ok(text);
    }
    // A machine on which eidolon has never run: any eidolon command unpacks its shipped tables, `policy` being the cheapest.
    let ran = std::process::Command::new(eidolon)
        .arg("--config")
        .arg(config)
        .args(["policy", "true"])
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("running `{} policy true` to unpack eidolon's tables", eidolon.display()))?;
    if !ran.status.success() {
        bail!("`eidolon policy true` failed ({}): {}", ran.status, String::from_utf8_lossy(&ran.stderr).trim());
    }
    read(&candidates).with_context(|| format!("eidolon's policy table was not found at {}", candidates[0].display()))
}

/// Writes `<home>/policy-manual.rn` and `<home>/config-manual.toml` (atomically, `0600`) from the `config.toml` already generated
/// in `home`, and returns the config's path.
pub fn generate_manual(home: &Path, member: &Path, eidolon: &Path) -> Result<PathBuf> {
    let config = home.join("config.toml");
    let text = std::fs::read_to_string(&config).with_context(|| format!("reading {}", config.display()))?;
    let mut table = text.parse::<toml::Table>().with_context(|| format!("parsing {}", config.display()))?;
    let stock = policy_in_force(member, &config, eidolon)?;
    let policy = home.join(MANUAL_POLICY);
    harnox::fs::write_atomic_0600(&policy, wrap(&stock)?.as_bytes()).with_context(|| format!("writing {}", policy.display()))?;
    let path = policy.to_str().with_context(|| format!("{} is not UTF-8", policy.display()))?.to_string();
    let entry = table.entry("policy").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(entry) = entry.as_table_mut() else { bail!("{}: [policy] is not a table", config.display()) };
    entry.insert("path".into(), toml::Value::String(path));
    entry.insert("enabled".into(), toml::Value::Boolean(true));
    entry.insert("yolo".into(), toml::Value::Boolean(false));
    entry.remove("judge");
    let body = toml::to_string(&table).context("writing the manual config")?;
    let out = home.join(MANUAL_CONFIG);
    let header = "# generated by minerva perch: config.toml plus a [policy] that asks about everything but reads; not for editing\n";
    harnox::fs::write_atomic_0600(&out, format!("{header}{body}").as_bytes()).with_context(|| format!("writing {}", out.display()))?;
    Ok(out)
}

/// The start-up half: the manual config when it can be made, its reason when not, and `default_mode` from `<home>/minerva.toml`.
pub fn prepare(home: Option<&Path>, member: &Path, eidolon: &Path) -> Result<ModeSetup> {
    let Some(home) = home else {
        return Ok(ModeSetup { manual_config: None, manual_error: Some("manual mode needs --home: the perch generates its policy there".into()), default_mode: Mode::default() });
    };
    let default_mode = crate::config::minerva_default_mode(home)?.unwrap_or_default();
    Ok(match generate_manual(home, member, eidolon) {
        Ok(path) => ModeSetup { manual_config: Some(path), manual_error: None, default_mode },
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "perch: manual mode is unavailable");
            ModeSetup { manual_config: None, manual_error: Some(format!("{e:#}")), default_mode }
        }
    })
}

/// `{id: mode}`, remembered across restarts: a chat reopened after a perch restart comes back in the mode it had, never a looser one.
pub struct ChatModes {
    file: Option<PathBuf>,
    map: Mutex<HashMap<String, Mode>>,
}

impl ChatModes {
    pub fn open(file: Option<PathBuf>) -> Self {
        let map = crate::statefile::load(file.as_deref());
        Self { file, map: Mutex::new(map) }
    }

    pub fn get(&self, id: &str) -> Option<Mode> {
        self.map.lock().unwrap().get(id).copied()
    }

    /// Remembers `mode` for `id`; a write that fails is logged and the memory still has it.
    pub fn set(&self, id: &str, mode: Mode) {
        let mut map = self.map.lock().unwrap();
        if map.insert(id.to_string(), mode) == Some(mode) {
            return;
        }
        self.save(&map);
    }

    pub fn forget(&self, id: &str) {
        let mut map = self.map.lock().unwrap();
        if map.remove(id).is_some() {
            self.save(&map);
        }
    }

    fn save(&self, map: &HashMap<String, Mode>) {
        let Some(file) = &self.file else { return };
        let written = serde_json::to_vec(map).context("serializing the chat modes").and_then(|body| {
            harnox::fs::write_atomic_0600(file, &body).with_context(|| format!("writing {}", file.display()))
        });
        if let Err(e) = written {
            tracing::warn!(error = %format!("{e:#}"), "perch: the chat modes could not be saved");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STOCK: &str = "const ALLOW = \"allow\";\npub fn shell_arg(tool) { \"\" }\npub fn classify_tool(tool, approval, input) { 1 }\n\
                         pub fn classify_program(argv, ctx) { 1 }\npub fn fs_verb_tier(prog) { \"allow\" }\npub fn bash_default(posture) { posture }\n";

    #[test]
    fn the_four_entry_points_are_renamed_and_the_wrappers_appended() {
        let wrapped = wrap(STOCK).unwrap();
        for (_, to) in RENAMED {
            assert!(wrapped.contains(to), "{to}");
        }
        assert!(wrapped.contains("pub fn shell_arg(tool)"), "shell_arg is the stock one");
        for entry in ["classify_tool", "classify_program", "fs_verb_tier", "bash_default"] {
            assert_eq!(wrapped.lines().filter(|l| l.starts_with(&format!("pub fn {entry}("))).count(), 1, "{entry} is the wrapper's alone");
        }
        assert!(wrapped.starts_with("const ALLOW"), "the stock text comes first, untouched");
    }

    #[test]
    fn a_table_the_wrapper_cannot_take_over_is_refused() {
        assert!(wrap("pub fn classify_tool(a, b, c) { 1 }").is_err(), "three of four missing");
        let twice = STOCK.to_string() + "pub fn bash_default(p) { p }\n";
        assert!(wrap(&twice).is_err(), "a name twice is a guess");
        let indented = STOCK.replace("pub fn fs_verb_tier", "  pub fn fs_verb_tier");
        assert!(wrap(&indented).is_err(), "only a line that starts with it counts");
    }

    #[test]
    fn modes_parse_spell_and_report() {
        for mode in [Mode::Manual, Mode::Auto, Mode::Yolo] {
            assert_eq!(Mode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(Mode::parse("gated"), None);
        assert_eq!(Mode::default(), Mode::Yolo);
        assert_eq!((Mode::from_yolo(true), Mode::from_yolo(false)), (Mode::Yolo, Mode::Auto));
        assert_eq!(Mode::Manual.reported(true), Mode::Yolo, "the door's own word wins");
        assert_eq!(Mode::Manual.reported(false), Mode::Manual);
        assert_eq!(Mode::Yolo.reported(false), Mode::Auto, "asked for yolo, door says not: it is not");
    }

    #[test]
    fn chat_modes_survive_a_reopen_and_forget() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("chat-modes.json");
        let a = ChatModes::open(Some(file.clone()));
        a.set("x", Mode::Manual);
        a.set("y", Mode::Auto);
        let b = ChatModes::open(Some(file.clone()));
        assert_eq!((b.get("x"), b.get("y"), b.get("z")), (Some(Mode::Manual), Some(Mode::Auto), None));
        b.forget("x");
        assert_eq!(ChatModes::open(Some(file)).get("x"), None);
    }
}
