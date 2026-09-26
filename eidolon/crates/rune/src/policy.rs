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

use std::path::Path;
use std::sync::Arc;

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
use crate::script::{json_to_rune, rune_to_json};

/// The script that ships in the binary. A user's own replaces it wholesale.
pub const DEFAULT_POLICY: &str = include_str!("../policy.rn");

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

/// `policy.rn`, compiled, as the harness's policy hook.
pub struct ScriptPolicy {
    unit: Arc<Unit>,
    runtime: Arc<RuntimeContext>,
    settings: PolicySettings,
}

/// The `#{decision, reason, read_only}` object every entry point returns.
#[derive(Debug, Deserialize)]
struct RawVerdict {
    decision: String,
    reason: String,
    #[serde(default)]
    read_only: bool,
}

impl ScriptPolicy {
    /// Compile `src` against the host's one context. Fails loudly: a policy
    /// that will not compile is worth refusing to start over, because the
    /// alternative is a session that silently has no gate.
    pub fn compile(src: &str, host: &Arc<Host>, settings: PolicySettings) -> anyhow::Result<Self> {
        let compiler = host.compiler()?;
        let mut sources = rune::Sources::new();
        sources
            .insert(rune::Source::new("<policy>", src).map_err(|e| anyhow!("policy source: {e}"))?)
            .map_err(|e| anyhow!("inserting the policy source: {e}"))?;
        let mut diagnostics = rune::Diagnostics::new();
        let unit = rune::prepare(&mut sources)
            .with_context(&compiler.context)
            .with_diagnostics(&mut diagnostics)
            .build()
            .map_err(|_| {
                let mut buf = rune::termcolor::Buffer::no_color();
                let rendered = match diagnostics.emit(&mut buf, &sources) {
                    Ok(()) => String::from_utf8_lossy(buf.as_slice()).into_owned(),
                    Err(e) => format!("<could not render diagnostics: {e}>"),
                };
                anyhow!("policy.rn failed to compile:\n{}", rendered.trim())
            })?;
        let this = ScriptPolicy {
            unit: Arc::new(unit),
            runtime: compiler.runtime.clone(),
            settings,
        };
        this.check_entry_points()?;
        Ok(this)
    }

    /// The built-in script, for a consumer that configures nothing.
    pub fn builtin(host: &Arc<Host>, settings: PolicySettings) -> anyhow::Result<Self> {
        Self::compile(DEFAULT_POLICY, host, settings)
    }

    /// The operator's `policy.rn` if there is one at `path`, else the shipped
    /// table. The second value is the path when a user's own was loaded, so a
    /// caller can say which table is deciding — a gate you cannot identify is
    /// one you cannot reason about — and `None` when it is the built-in.
    ///
    /// A user file that exists but will not load is an error, not a fallback:
    /// silently reverting to the built-in would leave the operator believing
    /// their edits were in force.
    pub fn load(
        path: &Path,
        host: &Arc<Host>,
        settings: PolicySettings,
    ) -> anyhow::Result<(Self, Option<String>)> {
        match std::fs::read_to_string(path) {
            Ok(src) => {
                let policy = Self::compile(&src, host, settings)
                    .with_context(|| format!("loading {}", path.display()))?;
                Ok((policy, Some(path.display().to_string())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok((Self::builtin(host, settings)?, None))
            }
            Err(e) => Err(anyhow!("reading {}: {e}", path.display())),
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
        let mut vm = Vm::new(self.runtime.clone(), self.unit.clone());
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
                };
            }
        };
        let verdict = match tier.decision {
            Decision::Allow => Verdict::Allow,
            Decision::Flag => Verdict::Ask(format!("{} — {}. Run it?", call.name, tier.reason)),
            // A refusal the algebra made for want of understanding is a
            // question; one the table made is an answer. See the module doc.
            Decision::Deny if tier.structural => {
                Verdict::Ask(format!("{} — {}. Run it anyway?", call.name, tier.reason))
            }
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
        }
    }
}

#[cfg(test)]
mod tests;
