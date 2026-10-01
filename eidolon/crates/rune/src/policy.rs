//! `policy.rn` as a [`PolicyHook`]: the shared decomposition algebra over a
//! scripted leaf table.
//!
//! The shape is deliberately the same one built-in tools have. A `.rn` file
//! ships inside the binary, `~/.config/eidolon/policy.rn` replaces it
//! wholesale, and every entry point is a `pub fn` compiled once against the
//! host's one Rune context. Nothing about policy needed a second mechanism.
//!
//! ## What each half decides
//!
//! [`harnox::policy`] parses a shell command into the real grammar and composes
//! per-leaf verdicts across pipes, chains, sequences, substitutions and
//! redirects; the script answers only "which tier is this already-decomposed
//! `argv`?". A script edit can therefore change which recognized command is
//! silent, asked about or refused — and can change nothing about what
//! composition, scoping or an unparseable line do.
//!
//! ## How the stage recognizes the fallthrough, and what that couples to
//!
//! The vouch store is consulted at the table's fallthrough — and the stage
//! detects the fallthrough by the table's own vocabulary: the shipped
//! `policy.rn` answers a tool it has never heard of with the reason
//! `unclassified tool (declared …)`, and [`FALLTHROUGH_REASON`] names that
//! prefix. This is a real coupling, chosen on purpose: the operator's
//! existing `policy.rn` copy carries the same vocabulary, which is what
//! makes vouches work without the operator editing their file — and a table
//! whose fallthrough says something else simply never consults the store,
//! so a vouched verb there gets the ordinary question, fail-closed with no
//! error. A named row's flag is the operator's own file speaking and is
//! never lifted; there is deliberately no second path (the table consulting
//! the store itself) alongside this one — one decision, one implementation.
//!
//! ## Why a `Deny` from the algebra becomes an `Ask` here
//!
//! The algebra refuses two different things with the same tier: a command it
//! judges (the table said no) and a command it cannot take apart (a subshell, a
//! `for` loop, a path outside the working directory). Unattended, both are
//! refusals. In front of a person the second is a question — an operator's
//! agent writes `for f in *.rs; do …; done` constantly, and refusing it outright
//! would make the classifier something to switch off. `Verdict::structural`
//! carries the distinction across the seam, and this is the only place that
//! reads it.
//!
//! ## Failing
//!
//! A script that will not load, will not compile, or raises at runtime yields
//! [`Verdict::Ask`] — not `Deny`, and never `Allow`. Denying everything bricks
//! an interactive session over a typo in a config file; allowing everything
//! turns a broken gate into no gate silently. Asking is the honest answer, and
//! it degrades correctly on its own: a headless run installs
//! [`NoUser`](eidolon_core::user::NoUser), which declines every question, so
//! "ask" there *is* "refuse".

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, anyhow};
use async_trait::async_trait;
use rune::runtime::RuntimeContext;
use rune::{Unit, Vm};
use serde::Deserialize;
use serde_json::json;

use eidolon_core::policy::{PolicyHook, Ruling, Verdict};
use eidolon_core::tool::{ToolCall, ToolManifest};
// Re-exported rather than merely imported: a consumer configures the
// classifier and reads its answers without naming harnox, because which crate
// the algebra lives in is not a fact `config.toml` or a CLI arm should carry.
pub use harnox::policy::{
    BashDefault, Decision, LeafTable, PolicyContext, Toolchain, Verdict as Tier, classify_command,
};

use crate::host::Host;
use crate::permits::Permits;
use crate::script::{json_to_rune, rune_to_json};

/// The script that ships in the binary. A user's own replaces it wholesale.
pub const DEFAULT_POLICY: &str = include_str!("../policy.rn");

/// The shipped table's version marker — `(version, date wording)` — or
/// `None` for a copy that predates the marking, which is itself a date.
/// The marker is the line `// table-version: N (date)` at the top of a
/// table: a seeded copy inherits it, an edited copy keeps the one it was
/// seeded with, and no marker at all means the copy is older than the
/// marking is. It answers "is my base current" cheaply; the rows are the
/// ground truth, and [`table_rows`] is what a comparison actually reads.
pub fn table_version(src: &str) -> Option<(u64, String)> {
    for line in src.lines() {
        let Some(body) = line.trim().strip_prefix("//").map(str::trim) else {
            continue;
        };
        let Some(rest) = body.strip_prefix("table-version:") else {
            continue;
        };
        let rest = rest.trim();
        let (version, dated) = match rest.split_once(char::is_whitespace) {
            Some((n, d)) => (
                n,
                d.trim().trim_start_matches('(').trim_end_matches(')').trim(),
            ),
            None => (rest, ""),
        };
        return version.parse::<u64>().ok().map(|v| (v, dated.to_string()));
    }
    None
}

/// The table's named rows: the string arms of `pub fn classify_tool`,
/// comments stripped. This is the unit a copy is compared in — behaviour
/// differs per *row*, so a stale copy is answered in rows and never as a
/// line diff. A row only in the operator's copy is their own; one only in
/// the shipped table is what the harness added since, and its absence
/// costs a question on every call it names.
pub fn table_rows(src: &str) -> BTreeSet<String> {
    let Some(body) = fn_body(src, "pub fn classify_tool") else {
        return BTreeSet::new();
    };
    let mut rows = BTreeSet::new();
    for line in body.lines() {
        // a commented-out row is not a row: everything from the comment
        // opener on is prose, whatever it quotes
        let code = match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        };
        if let Some(name) = quoted_arm(code) {
            rows.insert(name);
        }
    }
    rows
}

/// The text after a top-level function's signature line, through to the
/// next function at column zero — this file's layout — or to the end of
/// the source when none follows.
fn fn_body(src: &str, marker: &str) -> Option<String> {
    let lines: Vec<&str> = src.lines().collect();
    let start = lines.iter().position(|l| l.trim_start().starts_with(marker))?;
    let body: Vec<&str> = lines[start + 1..]
        .iter()
        .take_while(|l| !(l.starts_with("fn ") || l.starts_with("pub fn ")))
        .copied()
        .collect();
    Some(body.join("\n"))
}

/// The string-literal match arm a line opens with — `"read" => …` — or
/// nothing. The quoted name before the arrow is the row; the tier and
/// reason come after it and are not rows. A line whose literal holds an
/// escape is skipped rather than guessed at.
fn quoted_arm(line: &str) -> Option<String> {
    let open = line.find('"')?;
    let rest = &line[open + 1..];
    let close = rest.find('"')?;
    if rest[..close].contains('\\') {
        return None;
    }
    rest[close + 1..]
        .trim_start()
        .starts_with("=>")
        .then(|| rest[..close].to_string())
}

/// How the operator configured the gate. Everything here is read once at
/// startup and never written: like the rest of the harness's configuration, the
/// file belongs to whatever generates it.
#[derive(Clone, Debug)]
pub struct PolicySettings {
    /// Toolchains the operator expects to be used here. A toolchain's commands
    /// are silent when declared and fall to `default` when not.
    pub toolchains: Vec<Toolchain>,
    /// What an unrecognized command gets.
    pub default: BashDefault,
}

impl Default for PolicySettings {
    fn default() -> Self {
        // Allow, and the reasoning is in `policy.rn`'s `bash_default`: every
        // command is on screen as it runs, so an unfamiliar program name is not
        // by itself worth a question, and the shapes that are worth one are
        // recognized by the table or by the algebra.
        Self {
            toolchains: Vec::new(),
            default: BashDefault::Allow,
        }
    }
}

/// `policy.rn` — composed with `policy.permits.rn` in front of it — compiled,
/// as the harness's policy hook.
///
/// Two sources, one unit, the arrangement [`crate::permits`] documents: the
/// permits file first, so its `vouched` and `granted` are bare-name callable
/// from the table, and the table behind it unchanged. The unit is behind a
/// lock because it is **recomposed when the permits file changes** — a vouch
/// takes effect on the next call, not the next session — and a recompose
/// that fails leaves the previous unit running: a stale store beats a broken
/// gate, and the error is said out loud.
pub struct ScriptPolicy {
    unit: Mutex<Arc<Unit>>,
    runtime: Arc<RuntimeContext>,
    settings: PolicySettings,
    /// The table's own text, kept so a recompose puts the same table back in
    /// front of the new rows.
    table_src: Arc<str>,
    /// The host the table compiled against, kept for the same reason: a
    /// recompose is a real compile, against the context the unit runs on.
    host: Arc<Host>,
    /// The store, when there is one to read: `load` builds it beside the
    /// policy file it probed. `None` for a `compile` with no store — the
    /// fallthrough consult quietly does nothing, which is what a caller with
    /// no permits file asked for.
    permits: Option<Arc<Permits>>,
    /// What the current unit was composed with, so a change is one string
    /// compare away.
    composed: Mutex<String>,
}

/// The reason the shipped table's fallthrough answers with, and the one
/// string the stage reads to recognize it: an *unclassified* tool is a
/// question nobody has answered — which is what a vouch may answer — while a
/// named row's flag is the operator's own file speaking, and the operator's
/// file rules. The vocabulary belongs to the table; this constant is the
/// stage's side of that agreement, pinned by a test that classifies an
/// unknown name and checks the stage can still tell.
pub const FALLTHROUGH_REASON: &str = "unclassified tool";

/// The `#{decision, reason, read_only}` object every entry point returns.
#[derive(Debug, Deserialize)]
struct RawVerdict {
    decision: String,
    reason: String,
    #[serde(default)]
    read_only: bool,
}

/// Compile one source — the table alone, for a policy with no store.
fn compile_one(src: &str, context: &rune::Context) -> anyhow::Result<Unit> {
    let mut sources = rune::Sources::new();
    sources
        .insert(rune::Source::new("<policy>", src).map_err(|e| anyhow!("policy source: {e}"))?)
        .map_err(|e| anyhow!("inserting the policy source: {e}"))?;
    finish(sources, context)
}

/// Compile the store in front of the table, one unit, the arrangement the
/// TUI's prelude and a workflow's `lib/` both already use: bare-name calls
/// across sources, no import syntax. The permits source goes in first, so
/// its accessors are the ones in the namespace.
pub(crate) fn compile_composed(
    permits_src: &str,
    table_src: &str,
    context: &rune::Context,
) -> anyhow::Result<Unit> {
    let mut sources = rune::Sources::new();
    for (name, src) in [
        (crate::permits::PERMITS_FILE, permits_src),
        ("<policy>", table_src),
    ] {
        sources
            .insert(rune::Source::new(name, src).map_err(|e| anyhow!("source {name}: {e}"))?)
            .map_err(|e| anyhow!("inserting {name}: {e}"))?;
    }
    finish(sources, context)
}

/// The half every policy compile shares: prepare against a context, and on
/// failure render the diagnostics — a table edit that breaks the gate should
/// name the line, not the fact.
fn finish(mut sources: rune::Sources, context: &rune::Context) -> anyhow::Result<Unit> {
    let mut diagnostics = rune::Diagnostics::new();
    rune::prepare(&mut sources)
        .with_context(context)
        .with_diagnostics(&mut diagnostics)
        .build()
        .map_err(|_| {
            let mut buf = rune::termcolor::Buffer::no_color();
            let rendered = match diagnostics.emit(&mut buf, &sources) {
                Ok(()) => String::from_utf8_lossy(buf.as_slice()).into_owned(),
                Err(e) => format!("<could not render diagnostics: {e}>"),
            };
            anyhow!("policy.rn failed to compile:\n{}", rendered.trim())
        })
}

impl ScriptPolicy {
    /// Compile `src` against the host's one context. Fails loudly: a policy
    /// that will not compile is worth refusing to start over, because the
    /// alternative is a session that silently has no gate.
    pub fn compile(src: &str, host: &Arc<Host>, settings: PolicySettings) -> anyhow::Result<Self> {
        Self::compose(src, None, host, settings)
    }

    /// The whole build: the table's text, optionally the store whose file
    /// composes in front of it, the host, the settings. One function, so
    /// `load`, a recompose on change, and the write path's composed check
    /// compile exactly the same way.
    fn compose(
        table_src: &str,
        permits: Option<Arc<Permits>>,
        host: &Arc<Host>,
        settings: PolicySettings,
    ) -> anyhow::Result<Self> {
        let compiler = host.compiler()?;
        let permits_src = permits.as_ref().map(|p| p.source());
        let unit = match permits_src.as_deref() {
            Some(permits_src) => compile_composed(permits_src, table_src, &compiler.context)
                .with_context(|| format!("composing {} in front of the table", crate::permits::PERMITS_FILE))?,
            None => compile_one(table_src, &compiler.context)?,
        };
        let this = ScriptPolicy {
            unit: Mutex::new(Arc::new(unit)),
            runtime: compiler.runtime.clone(),
            host: host.clone(),
            settings,
            table_src: Arc::from(table_src),
            permits,
            composed: Mutex::new(permits_src.unwrap_or_default()),
        };
        this.check_entry_points()?;
        Ok(this)
    }

    /// The built-in script, for a consumer that configures nothing.
    pub fn builtin(host: &Arc<Host>, settings: PolicySettings) -> anyhow::Result<Self> {
        Self::compile(DEFAULT_POLICY, host, settings)
    }

    /// The store this policy composes in front of its table. The session
    /// wires it onward after startup — to the host, so the endpoint's grant
    /// check reads the same rows, and to the write path, so a grant's
    /// composed check is this table's.
    pub fn permits(&self) -> Option<&Arc<Permits>> {
        self.permits.as_ref()
    }

    /// The operator's `policy.rn` if there is one at `path`, else the shipped
    /// table. The second value is the path when a user's own was loaded, so a
    /// caller can say which table is deciding — a gate you cannot identify is
    /// one you cannot reason about — and `None` when it is the built-in.
    ///
    /// A user file that exists but will not load is an error, not a fallback:
    /// silently reverting to the built-in would leave the operator believing
    /// their edits were in force.
    ///
    /// The store comes with it: `policy.permits.rn` beside the file probed,
    /// present or not, composed in front of whatever table loaded. An
    /// operator's aged copy needs no edit for a grant or vouch to take
    /// effect — the accessors exist in the unit regardless.
    pub fn load(
        path: &Path,
        host: &Arc<Host>,
        settings: PolicySettings,
    ) -> anyhow::Result<(Self, Option<String>)> {
        let permits = Arc::new(Permits::from_policy_path(path));
        match std::fs::read_to_string(path) {
            Ok(src) => {
                let policy = Self::compose(&src, Some(permits), host, settings)
                    .with_context(|| format!("loading {}", path.display()))?;
                Ok((policy, Some(path.display().to_string())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok((Self::compose(DEFAULT_POLICY, Some(permits), host, settings)?, None))
            }
            Err(e) => Err(anyhow!("reading {}: {e}", path.display())),
        }
    }

    /// Compile the store's current text in front of this policy's table —
    /// the check every write to the store goes through before anything
    /// lands. A permits file that would break the gate is refused, and the
    /// previous file stays on disk.
    pub fn recompose(&self, permits_src: &str) -> anyhow::Result<()> {
        compile_composed(permits_src, &self.table_src, &self.host.compiler()?.context).map(|_| ())
    }

    /// Recompose the running unit when the store's file has changed — the
    /// read side of re-read-on-change. Called at the top of every verdict.
    fn refresh(&self) {
        let Some(permits) = &self.permits else {
            return;
        };
        let src = permits.source();
        let mut composed = self.composed.lock().expect("policy compose lock");
        if *composed == src {
            return;
        }
        let context = match self.host.compiler() {
            Ok(c) => &c.context,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "the permits file changed but the compile context is gone; the previous unit stays in force");
                return;
            }
        };
        match compile_composed(&src, &self.table_src, context) {
            Ok(unit) => {
                *self.unit.lock().expect("policy unit lock") = Arc::new(unit);
                *composed = src;
            }
            Err(e) => {
                // The unit that loaded is still in force, rows and all: a
                // stale store beats a broken gate. Said once per change,
                // not once per call — `composed` stays behind on purpose.
                tracing::error!(
                    error = %format!("{e:#}"),
                    "the permits file would not compose with the table; the previous unit stays in force"
                );
            }
        }
    }

    /// Call every entry point once, at load, so a script missing one is a
    /// startup error naming it rather than a surprise on the call that needed
    /// it. A table that answers wrongly is a policy question; a table that
    /// cannot answer at all is a bug, and it should not take a `rm -rf` to find
    /// it.
    fn check_entry_points(&self) -> anyhow::Result<()> {
        self.shell_arg("bash")
            .context("policy.rn: `pub fn shell_arg(tool)`")?;
        self.tool_verdict("read", "read_only", &json!({}))
            .context("policy.rn: `pub fn classify_tool(tool, approval, input)`")?;
        self.fs_verb_tier_checked("rm")
            .context("policy.rn: `pub fn fs_verb_tier(prog)`")?;
        self.bash_default_checked(BashDefault::Allow)
            .context("policy.rn: `pub fn bash_default(posture)`")?;
        self.program_verdict(&["ls"], &self.context(Path::new("/")))
            .context("policy.rn: `pub fn classify_program(argv, ctx)`")?;
        Ok(())
    }

    /// The context a command runs in: the dispatcher's live working directory
    /// as the scope, plus the operator's declarations.
    fn context(&self, cwd: &Path) -> PolicyContext {
        PolicyContext {
            workspace_root: Some(cwd.to_path_buf()),
            toolchains: self.settings.toolchains.clone(),
            bash_default: self.settings.default,
            unscoped: false,
        }
    }

    /// Run one entry point and deserialize whatever it returned.
    fn call<T: for<'de> serde::Deserialize<'de>>(
        &self,
        entry: &str,
        args: Vec<rune::Value>,
    ) -> anyhow::Result<T> {
        let unit = self.unit.lock().expect("policy unit lock").clone();
        let mut vm = Vm::new(self.runtime.clone(), unit);
        let value = vm
            .execute([entry], args)
            .map_err(|e| anyhow!("policy.rn has no `pub fn {entry}`: {e}"))?
            .complete()
            .into_result()
            .map_err(|e| anyhow!("policy.rn's {entry} failed: {e}"))?;
        let json = rune_to_json(&value)
            .with_context(|| format!("policy.rn's {entry} returned something unserialisable"))?;
        serde_json::from_value(json)
            .with_context(|| format!("policy.rn's {entry} returned the wrong shape"))
    }

    fn shell_arg(&self, tool: &str) -> anyhow::Result<String> {
        self.call("shell_arg", vec![json_to_rune(&json!(tool))])
    }

    fn tool_verdict(
        &self,
        tool: &str,
        approval: &str,
        input: &serde_json::Value,
    ) -> anyhow::Result<Tier> {
        let raw: RawVerdict = self.call(
            "classify_tool",
            vec![
                json_to_rune(&json!(tool)),
                json_to_rune(&json!(approval)),
                json_to_rune(input),
            ],
        )?;
        raw.into_tier()
    }

    fn program_verdict(&self, argv: &[&str], ctx: &PolicyContext) -> anyhow::Result<Tier> {
        let cond = json!({
            "rust": ctx.has(Toolchain::Rust),
            "python": ctx.has(Toolchain::Python),
            "node": ctx.has(Toolchain::Node),
            "nix": ctx.has(Toolchain::Nix),
            "posture": ctx.posture(),
        });
        let raw: RawVerdict = self.call(
            "classify_program",
            vec![json_to_rune(&json!(argv)), json_to_rune(&cond)],
        )?;
        raw.into_tier()
    }

    fn fs_verb_tier_checked(&self, prog: &str) -> anyhow::Result<Decision> {
        let s: String = self.call("fs_verb_tier", vec![json_to_rune(&json!(prog))])?;
        decision(&s)
    }

    fn bash_default_checked(&self, posture: BashDefault) -> anyhow::Result<Decision> {
        let name = match posture {
            BashDefault::Allow => "allow",
            BashDefault::Flag => "flag",
            BashDefault::Deny => "deny",
        };
        let s: String = self.call("bash_default", vec![json_to_rune(&json!(name))])?;
        decision(&s)
    }

    /// How a shell command classifies, without running it — what
    /// `eidolon policy` prints. The same path a real call takes, so what it
    /// reports is what would happen rather than an approximation of it.
    pub fn explain(&self, command: &str, cwd: &Path) -> Tier {
        classify_command(command, &self.context(cwd), self)
    }

    /// The whole decision for one call, as a tier plus the reason. Split out
    /// from [`PolicyHook::pre_tool`] so the mapping from tier to verdict — the
    /// one place `structural` is read — has nothing else in it.
    fn classify(
        &self,
        call: &ToolCall,
        manifest: &ToolManifest,
        cwd: &Path,
    ) -> anyhow::Result<Tier> {
        let name = bare_tool_name(&call.name);
        let field = self.shell_arg(name)?;
        if !field.is_empty()
            && let Some(command) = call.input.get(&field).and_then(|v| v.as_str())
        {
            return Ok(classify_command(command, &self.context(cwd), self));
        }
        // The tool's own verdict: the shell algebra only runs when the
        // table names a command field, above.
        self.tool_verdict(name, manifest.approval.as_str(), &call.input)
    }
}

/// The tool's own name, with any MCP qualification stripped — `bash` from
/// `mcp__eidolon__bash`.
///
/// A tool served over MCP arrives carrying the server it came from, and the
/// server's name is a wiring detail: the same `bash` must classify the same way
/// whether the model reached it directly or the Claude CLI reached it through
/// `eidolon mcp`. Stripping here rather than in the table also means a user's
/// table never has to know the prefix exists.
fn bare_tool_name(full: &str) -> &str {
    full.rsplit("__").next().unwrap_or(full)
}

fn decision(s: &str) -> anyhow::Result<Decision> {
    match s {
        "allow" => Ok(Decision::Allow),
        "flag" => Ok(Decision::Flag),
        "deny" => Ok(Decision::Deny),
        other => Err(anyhow!("policy.rn returned an unrecognized tier `{other}`")),
    }
}

impl RawVerdict {
    fn into_tier(self) -> anyhow::Result<Tier> {
        Ok(Tier::tier(
            decision(&self.decision)?,
            self.reason,
            self.read_only,
        ))
    }
}

/// The script is the leaf table. Each method fails *loud but safe*: a script
/// error becomes a `Flag`, which the hook turns into a question, rather than an
/// `Allow` nobody asked for or a `Deny` that stops the session dead.
impl LeafTable for ScriptPolicy {
    fn classify_program(&self, argv: &[&str], ctx: &PolicyContext) -> Tier {
        self.program_verdict(argv, ctx).unwrap_or_else(|e| {
            tracing::error!(error = %format!("{e:#}"), "policy.rn classify_program failed");
            Tier::flag("the policy table failed on this command", false)
        })
    }

    fn fs_verb_tier(&self, prog: &str) -> Decision {
        self.fs_verb_tier_checked(prog).unwrap_or_else(|e| {
            tracing::error!(error = %format!("{e:#}"), "policy.rn fs_verb_tier failed");
            Decision::Flag
        })
    }

    fn bash_default(&self, posture: BashDefault) -> Decision {
        self.bash_default_checked(posture).unwrap_or_else(|e| {
            tracing::error!(error = %format!("{e:#}"), "policy.rn bash_default failed");
            Decision::Flag
        })
    }
}

#[async_trait]
impl PolicyHook for ScriptPolicy {
    async fn pre_tool(&self, call: &ToolCall, manifest: &ToolManifest, cwd: &Path) -> Ruling {
        // The store may have changed since the last verdict — a vouch
        // written in another terminal, a grant answered in this one. The
        // unit is recomposed before anything is classified, so the table
        // and the store it sits in front of are the ones on disk.
        self.refresh();
        let tier = match self.classify(call, manifest, cwd) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "the policy table failed");
                return Ruling {
                    verdict: Verdict::Ask(format!(
                        "The policy table failed ({e}). Run `{}` anyway?",
                        call.name
                    )),
                    reason: Some("the policy table failed".into()),
                    // Not the algebra declining to take something apart —
                    // the table not answering at all. A ledger must not read
                    // this as a shape nobody taught it.
                    structural: false,
                    judged: None,
                    yolo: false,
                    vouched: None,
                };
            }
        };
        // A tool a plugin contributed is asked about *by its plugin*, so the
        // person answering knows whose effect they are approving and the
        // question the log keeps carries the same tag the audit lines do.
        // The classifier's own reason — the unit a ledger groups by — is
        // deliberately left alone: whose the tool is is a fact about the
        // call, not a thing a table edit flips.
        let whose = match &manifest.plugin {
            Some(p) => format!(" (plugin {p})"),
            None => String::new(),
        };
        let mut vouched = None;
        let verdict = match tier.decision {
            Decision::Allow => Verdict::Allow,
            // The table's fallthrough — a tool no row answers — is the one
            // question the store may answer. A named row's flag is the
            // operator's own file speaking and stands; a refusal is an
            // answer and stands; a structural question is about the command,
            // not the tool. The vouch's hash was compared against the tool
            // actually loaded, so an edited plugin falls through to the
            // ordinary question below.
            Decision::Flag if tier.reason.starts_with(FALLTHROUGH_REASON) => {
                match self.vouch(bare_tool_name(&call.name), manifest) {
                    Some(grant) => {
                        vouched = Some(grant);
                        Verdict::Allow
                    }
                    None => Verdict::Ask(format!("{}{whose} — {}. Run it?", call.name, tier.reason)),
                }
            }
            Decision::Flag => {
                Verdict::Ask(format!("{}{whose} — {}. Run it?", call.name, tier.reason))
            }
            // A refusal the algebra made for want of understanding is a
            // question; one the table made is an answer. See the module doc.
            Decision::Deny if tier.structural => Verdict::Ask(format!(
                "{}{whose} — {}. Run it anyway?",
                call.name, tier.reason
            )),
            Decision::Deny => Verdict::Deny(tier.reason.to_string()),
        };
        // The reason travels beside the sentence built around it, because
        // the sentence names a tool and a table edit does not. See
        // [`Ruling::reason`].
        Ruling {
            verdict,
            reason: Some(tier.reason.into_owned()),
            structural: tier.structural,
            judged: None,
            yolo: false,
            vouched,
        }
    }
}

impl ScriptPolicy {
    /// The store's answer for one verb, when it may speak: the operator
    /// vouched for this exact name, contributed by this exact plugin, whose
    /// script is byte-for-byte the one the vouch saw. Any difference and it
    /// is not the tool that was trusted — the question stands, and saying
    /// yes again is the operator's to do.
    fn vouch(&self, name: &str, manifest: &ToolManifest) -> Option<String> {
        let permits = self.permits.as_ref()?;
        let row = permits.vouched(name)?;
        if !row.covers(manifest) {
            return None;
        }
        Some(format!(
            "vouched for {} by {} at {}",
            name, row.granted_by, row.granted_at
        ))
    }

    /// Whether the table has a **named row** for this tool — anything the
    /// table says about the name that is not its fallthrough. This is the
    /// listing's read: a verb the operator's own file answers is ruled, and
    /// a vouch neither replaces it nor needs to.
    pub fn named_row(&self, tool: &str) -> anyhow::Result<Option<Decision>> {
        let raw: RawVerdict = self.call(
            "classify_tool",
            vec![
                json_to_rune(&json!(tool)),
                json_to_rune(&json!("unknown")),
                json_to_rune(&json!({})),
            ],
        )?;
        if raw.reason.starts_with(FALLTHROUGH_REASON) {
            return Ok(None);
        }
        Ok(Some(raw.into_tier()?.decision))
    }
}

#[cfg(test)]
mod tests;
