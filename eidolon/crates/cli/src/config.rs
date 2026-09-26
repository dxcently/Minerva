//! `~/.config/eidolon/config.toml` and provider resolution.
//!
//! ```toml
//! default_model = "claude-sonnet-5"
//! max_tokens = 64000
//! max_iterations = 128
//! # thinking_budget = 4096
//!
//! [[providers]]
//! name = "anthropic"
//! wire = "anthropic"
//! base_url = "https://api.anthropic.com"
//! token_file = "~/.config/eidolon/anthropic.token"
//! auth = "bearer"          # a `claude setup-token` bearer; "api_key" for a key
//! models = ["claude-*"]
//!
//! [[providers]]
//! name = "kimi"
//! wire = "anthropic"
//! base_url = "https://api.moonshot.ai/anthropic"
//! token_file = "~/.config/eidolon/kimi.token"
//! models = ["kimi-*", "moonshot-*"]
//! ```
//!
//! ```toml
//! [claude_cli]                      # the Claude CLI as the backend
//! binary = "claude"                  # default
//! models = ["claude-*", "sonnet", "opus", "haiku"]
//!
//! [mneme]
//! mcp_url = "https://vault.example/mcp"
//! passphrase_file = "~/.config/eidolon/mneme.pass"   # or token_file = …
//!
//! [project]
//! instructions = "auto"              # AGENTS.md, falling back to CLAUDE.md
//! ```
//!
//! `[[providers]]` entries are harnox's `ProviderSpec` shape, optionally
//! with `headers`, `compat` and full model tables (see
//! `eidolon_providers::ProviderDef::from_toml`); Rune scripts in
//! `providers_dir` declare the rest. `mock` needs no config.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

use eidolon_remote::{Credential, Melete, Mneme};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub default_model: Option<String>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Upper bound on model calls per turn, and — a few calls before it —
    /// the point where the harness tells the model to wrap up
    /// ([`eidolon_core::session::RecordKind::TurnBudget`]).
    ///
    /// The unit is a *model call*, not a tool call, and the two diverge by
    /// model: one that batches five reads into a single reply spends one
    /// call on them; glm on this machine averaged 1.12 tool calls per
    /// message, spending one iteration per tool. The old default of 64 was
    /// set with the first kind in mind and cut the second off mid-work —
    /// every turn in the logs that hit it ended on a landing edit or a
    /// test run, not a loop. 128 covers the p99 turn (92 calls) measured
    /// here, and a runaway still stops, one wrap-up message later.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    pub thinking_budget: Option<u32>,
    pub sessions_dir: Option<PathBuf>,
    pub system_prompt: Option<String>,
    /// How long an endpoint should hold what a turn writes to its prompt
    /// cache: `"short"` (the endpoint's own default), `"long"`, or
    /// `"none"`.
    ///
    /// The whole saving from a prompt cache turns on the entry still being
    /// alive when the next request arrives, and the default entry on the
    /// Anthropic wire lives five minutes — which covers a tool loop and
    /// loses every time a person reads the answer before replying. `long`
    /// buys the hour-long entry at a higher write rate (2× the base rate
    /// rather than 1.25×), and pays for itself on the first turn that
    /// would otherwise have missed. Left at `short` because it is the
    /// endpoint's own answer and costs nothing to be wrong about.
    #[serde(default)]
    pub cache_retention: eidolon_core::provider::CacheRetention,
    #[serde(default)]
    pub providers: Vec<toml::Value>,
    /// The model picker's ordering: `provider:id` (or a glob over it) →
    /// weight, high first. Unnamed is zero. Declared here and nowhere
    /// else — nothing in the harness writes a weight back.
    #[serde(default)]
    pub model_weights: std::collections::BTreeMap<String, i64>,
    /// Directory of Rune provider scripts (default `~/.config/eidolon/providers`).
    pub providers_dir: Option<PathBuf>,
    /// Directory of the user's own Rune tools (default `~/.config/eidolon/tools`).
    pub tools_dir: Option<PathBuf>,
    /// Where `secrets.enc` and `secret.key` live (default `~/.config/eidolon/secrets`).
    pub secrets_dir: Option<PathBuf>,
    pub mneme: Option<MnemeEntry>,
    pub melete: Option<MeleteEntry>,
    pub claude_cli: Option<ClaudeCliEntry>,
    pub verba: Option<VerbaEntry>,
    pub swarm: Option<SwarmEntry>,
    pub policy: Option<PolicyEntry>,
    pub project: Option<ProjectEntry>,
    pub images: Option<ImagesEntry>,
    pub terminal: Option<TerminalEntry>,
    pub transcript: Option<TranscriptEntry>,
}

/// How the TUI's `T` chords open another window.
///
/// ```toml
/// [terminal]
/// # command = ["foot", "-D", "{dir}"]   # the emulator; {dir} is where to open
/// # editor  = ["hx"]                     # T H
/// # files   = ["yazi"]                   # T R
/// ```
///
/// Every field is an argv and not a command line, because a command line
/// is a thing that needs a shell to take apart, and a shell between a
/// keystroke and a window is one more place a space in a path can go
/// wrong. Two placeholders: `{dir}` in `command` is where the window
/// opens, and `{path}` in `editor`/`files` is what to open — appended
/// when it is not mentioned, which is what `["hx"]` wants, and honoured
/// where it is, which is what a program reached through a shell needs
/// (`["nu", "-e", "ya \"{path}\""]`). The program to run, when there is
/// one, follows `command`'s arguments.
///
/// Absent — or any field left out — and the harness works it out on the
/// first press rather than at startup: `$TERMINAL`, then the emulator
/// `$TERM` says we are inside, then the first it can find; and
/// `$VISUAL`/`$EDITOR` for the editor. See `eidolon_tui::launch`, which
/// carries the reasoning and the candidate lists.
///
/// The field worth setting by hand is `files`, and only for a shell
/// wrapper: `["nu", "-e", "ya"]` runs the function that leaves the shell
/// in the directory yazi exited from, which a bare `yazi` cannot do and
/// the harness cannot guess.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TerminalEntry {
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub editor: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
}

/// How attached images are drawn in the terminal.
///
/// ```toml
/// [images]
/// # inline = "auto"   # auto | off | sixel | kitty
/// ```
///
/// `auto` reads the environment — `KITTY_WINDOW_ID` and `TERM_PROGRAM` for
/// the kitty protocol — and leaves everything else off unless
/// `EIDOLON_ENABLE_SIXEL=1` explicitly opts sixel back in. The honest way
/// to ask the terminal itself is a DA1 query (`ESC [ c`, then read the
/// reply), and that is a round-trip on a terminal that may not answer: a
/// hang on the path to the first frame, in a harness whose whole startup
/// budget is ten milliseconds. So the guess is cheap and this is how an
/// operator corrects it.
///
/// Turning it `off` costs the pixels and not the feature: the model is
/// still sent the image, and the transcript still says what was attached.
/// That is the right setting for a session over ssh into something plain,
/// and for anyone who would rather read a caption than look at a
/// thumbnail.
#[derive(Debug, Clone, Deserialize)]
pub struct ImagesEntry {
    #[serde(default = "auto")]
    pub inline: String,
}

fn auto() -> String {
    "auto".into()
}

/// How the transcript treats a run of finished tool calls while their turn
/// is still running.
///
/// ```toml
/// [transcript]
/// # fold = "settle"   # settle | live
/// ```
///
/// `settle` (the default) holds the turn open — every call since the
/// operator's message stays on screen, output and all, until the turn
/// settles and the whole of it collapses to one summary line. The summary
/// never rewrites itself under the operator, at the price of a long turn
/// pushing the earlier context off the screen while it runs. `live` is
/// the older behaviour: a run folds to its summary the moment the next
/// call begins, so the screen keeps only the call in flight — cheaper to
/// follow, at the price of a summary that pops with every call. Both are
/// reasonable, which is why it is a setting. The knob for *how much* a
/// drawn call shows is not here: that is `space o` in the session.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TranscriptEntry {
    #[serde(default)]
    pub fold: eidolon_tui::state::Fold,
}

/// The project instructions a session reads from the directory it runs in.
///
/// ```toml
/// [project]
/// # instructions = "auto"    # auto | claude | agents | off
/// ```
///
/// These files are a convention every coding harness now shares: a
/// directory states how it wants to be worked on, and the harness puts
/// that in front of the model without being asked. `auto` (the default)
/// takes `AGENTS.md` and falls back to `CLAUDE.md`; naming one takes that
/// file alone, and `off` reads neither — the honest way out for a machine
/// where the prompt is spent on the work. A directory holding no such
/// file costs nothing whatever the setting says, so the default is quiet
/// everywhere it does not apply.
///
/// The file is re-read at the start of every turn and never journaled, on
/// the persona's schedule and for its reasons: an edit is live on the next
/// turn, and the log holds no snapshot of instructions that have moved on.
/// See `eidolon_core::project`.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectEntry {
    #[serde(default)]
    pub instructions: eidolon_core::project::ProjectInstructions,
}

/// Coordination between concurrent sessions.
///
/// ```toml
/// [swarm]
/// # enabled = true
/// ```
///
/// On by default, because the feature is the point and the cost of a
/// registration is a `mkdir` and a socket. `enabled = false` is the way
/// out for a machine where a session should be alone — and, being
/// configuration, it is decided where the rest of the machine is decided
/// and holds for the life of the session.
#[derive(Debug, Clone, Deserialize)]
pub struct SwarmEntry {
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// The command safety classifier.
///
/// ```toml
/// [policy]
/// # enabled = true
/// # default = "allow"          # allow | flag | deny
/// # toolchains = ["rust", "nix"]
/// # judge = "fau:openai/gpt-5-mini"   # off when absent
/// # judge_timeout_ms = 4000
/// # yolo = false                # true: every question is a yes, from launch
/// ```
///
/// On by default. The gate is the point, and it is built to be quiet: it
/// asks about the shapes worth a question and refuses only what should
/// not happen on this machine at all. `enabled = false` puts `AllowAll`
/// back, which is the honest way out for someone who wants none of it —
/// better than a table edited until it says yes to everything, because
/// the config says plainly that there is no gate.
///
/// The table itself is `~/.config/eidolon/policy.rn` when that exists and
/// the one in the binary otherwise; nothing here tunes *which* commands
/// are which, because that belongs in the script where it can be read.
#[derive(Debug, Clone, Deserialize)]
pub struct PolicyEntry {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// What a command matching nothing in the table gets.
    ///
    /// `allow` because every command is on screen as it runs, so an
    /// unrecognized *program name* is not by itself evidence of anything
    /// — and a gate that asks about every unfamiliar binary is the gate
    /// that gets switched off. The shapes worth stopping are recognized,
    /// by the table or by the algebra above it.
    #[serde(default)]
    pub default: eidolon_rune::policy::BashDefault,
    /// Toolchains this machine expects to be used. A declared toolchain's
    /// commands are ordinary work; an undeclared one's fall to `default`.
    #[serde(default)]
    pub toolchains: Vec<eidolon_rune::policy::Toolchain>,
    /// The model that answers the questions the table raises — a catalog
    /// key, like anywhere else a model is named. Absent, there is no such
    /// layer and every question reaches the operator.
    ///
    /// Off unless named because it is a spend: the deterministic pass costs
    /// a tenth of a millisecond and this costs an API call, so a harness
    /// that switched it on by itself would be choosing to spend the
    /// operator's money on their behalf. Name a cheap, fast one — the
    /// answer is a single word, and the operator is waiting for it.
    ///
    /// It can only ever turn a question into a yes; it cannot refuse
    /// anything, and it never sees a call the table allowed or denied
    /// outright. See [`eidolon_core::escalate`].
    pub judge: Option<String>,
    /// How long to wait for that answer before giving up and asking the
    /// operator. Short on purpose: somebody is sitting in front of the
    /// prompt this is trying to save them, and a gate that thinks for ten
    /// seconds has cost more than the question would have.
    #[serde(default = "judge_timeout")]
    pub judge_timeout_ms: u64,
    /// Start every session with the yolo switch armed — `--yolo` as a
    /// standing answer rather than a flag typed each launch.
    ///
    /// It is the operator's own record that argues for it: over every
    /// session on this machine the gate asked twenty-four times, nearly
    /// all of them about shapes, and was told yes twenty-three. A question
    /// answered yes every time is a keypress, and a keypress in a loop is
    /// what the switch exists to remove. What it does not touch is
    /// unchanged: the three refusals on the merits still refuse, every
    /// waived call is journaled as such, and `:yolo` still throws the
    /// switch off for a session. Off unless said, because it is a posture
    /// and the config is where a posture is read.
    #[serde(default)]
    pub yolo: bool,
}

fn judge_timeout() -> u64 {
    4000
}

fn yes() -> bool {
    true
}

/// Verba Volantia: the classifier binary, which seam the vault command
/// channel enters by, and one bundle per tool surface.
///
/// ```toml
/// [verba]
/// # binary = "verba-volantia"
/// # entry = "inline"             # or "tool"
/// [[verba.bundles]]
/// name = "mneme"
/// weights_dir = "~/Development/VerbaVolantia/weights/mneme"
/// target = "mneme_rpc"          # or "tools"
/// ```
///
/// `entry` selects how the **model** reaches the channel: `inline` (the
/// default) has it write `! <command>` lines in its own reply, intercepted
/// by the loop and answered in the same turn, with the teaching spliced into
/// the system prompt and no `vv` tool registered; `tool` registers the `vv`
/// tool instead. Both are the same engine — gate, reconciliation, nested
/// dispatch, harvest — and the model must never see both at once. A backend
/// that owns its own loop (the Claude CLI) keeps the tool whatever this says,
/// because there is no request for the harness to intercept. The operator's
/// own `:do` / `eidolon do` is dispatch mode and is unaffected either way.
#[derive(Debug, Clone, Deserialize)]
pub struct VerbaEntry {
    #[serde(default)]
    pub binary: Option<String>,
    /// Which entry seam the command channel uses — `eidolon_verba::Entry`.
    #[serde(default)]
    pub entry: eidolon_verba::Entry,
    #[serde(default)]
    pub bundles: Vec<VerbaBundleEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerbaBundleEntry {
    pub name: String,
    pub weights_dir: PathBuf,
    pub target: eidolon_verba::Target,
}

impl VerbaEntry {
    pub fn palette(&self) -> eidolon_verba::Palette {
        let binary = self
            .binary
            .clone()
            .unwrap_or_else(|| "verba-volantia".into());
        let mut p = eidolon_verba::Palette::new();
        for b in &self.bundles {
            let dir = expand_home(&b.weights_dir);
            p.add(eidolon_verba::Bundle::new(
                &b.name,
                b.target.clone(),
                eidolon_verba::VerbaDispatcher::new(&binary, dir.display().to_string()),
            ));
        }
        p
    }
}

fn expand_home(p: &Path) -> PathBuf {
    if let Ok(rest) = p.strip_prefix("~")
        && let Some(h) = std::env::var_os("HOME")
    {
        return PathBuf::from(h).join(rest);
    }
    p.to_path_buf()
}

/// The Claude CLI as a turn driver. `--provider claude-cli` selects it
/// regardless of model; otherwise a model matching `models` does.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeCliEntry {
    #[serde(default)]
    pub binary: Option<String>,
    #[serde(default = "default_claude_models")]
    pub models: Vec<String>,
    /// Which tools the CLI may use for a turn:
    ///
    /// - `own` (default) — its own built-ins, policed by the `PreToolUse`
    ///   hook. Claude Code is tuned for these and is strongest with them.
    /// - `eidolon` — only this harness's registry, served over MCP, with the
    ///   built-ins disabled. Every call is executed and journaled by the
    ///   dispatcher, so the log records what the harness did rather than
    ///   what the CLI reported.
    #[serde(default)]
    pub tools: eidolon_claude::ToolMode,
}

fn default_claude_models() -> Vec<String> {
    ["claude-*", "sonnet", "opus", "haiku"]
        .into_iter()
        .map(String::from)
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
pub struct MnemeEntry {
    pub mcp_url: String,
    #[serde(default)]
    pub passphrase_file: Option<PathBuf>,
    #[serde(default)]
    pub token_file: Option<PathBuf>,
}

impl MnemeEntry {
    pub fn client(&self) -> anyhow::Result<Mneme> {
        let cred = match (&self.passphrase_file, &self.token_file) {
            (Some(p), _) => Credential::PassphraseFile(p.clone()),
            (None, Some(t)) => Credential::TokenFile(t.clone()),
            (None, None) => bail!("[mneme] needs passphrase_file or token_file"),
        };
        Ok(Mneme::new(self.mcp_url.clone(), cred))
    }
}

/// Melete, the job harness, reached from this desktop — `[melete]`.
///
/// ```toml
/// [melete]
/// mcp_url = "https://melete.rdct.dev/mcp"
/// passphrase_file = "~/.config/pi-extensions/melete-password"
/// # core = ["get_docs", "read_doc", "status", "job_status", "run_code_task"]
/// # prefix = "melete_"
/// ```
///
/// Every tool the connector lists is registered under `prefix` and deferred:
/// nothing is offered without a `tool_search` unless `core` (server-side
/// names) says otherwise, and the default core is empty; see
/// `eidolon_remote::melete`. The listing is cached and refreshed off the
/// launch path; `eidolon tools --live` primes it.
#[derive(Debug, Clone, Deserialize)]
pub struct MeleteEntry {
    pub mcp_url: String,
    #[serde(default)]
    pub passphrase_file: Option<PathBuf>,
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    #[serde(default)]
    pub core: Option<Vec<String>>,
    #[serde(default)]
    pub prefix: Option<String>,
}

impl MeleteEntry {
    pub fn client(&self) -> anyhow::Result<Melete> {
        let cred = match (&self.passphrase_file, &self.token_file) {
            (Some(p), _) => Credential::PassphraseFile(p.clone()),
            (None, Some(t)) => Credential::TokenFile(t.clone()),
            (None, None) => bail!("[melete] needs passphrase_file or token_file"),
        };
        Ok(Melete::new(self.mcp_url.clone(), cred))
    }

    pub fn options(&self) -> eidolon_remote::melete::Options {
        let mut opts = eidolon_remote::melete::Options::default();
        if let Some(core) = &self.core {
            opts.core = core.clone();
        }
        if let Some(prefix) = &self.prefix {
            opts.prefix = prefix.clone();
        }
        opts
    }
}

/// The cap the harness asks for when the config does not.
///
/// It is a *request* ceiling, not a reservation: nothing is spent by asking
/// for room the model does not use, and the number's only job is to be out
/// of the way of an honest answer. 8192 was that number for the models of
/// 2024; the current ones write up to 128k in a turn, and a cap below what
/// the work needs does not shorten the answer — it truncates it mid-word,
/// which costs the whole turn and another one to ask again. So: high enough
/// to disappear, low enough to still be a runaway's backstop.
///
/// The Anthropic wire requires the field, which is why [`AgentConfig`] and
/// `ChatRequest` carry it unconditionally; an OpenAI-compatible endpoint
/// would default it for us, and gets this instead.
///
/// [`AgentConfig`]: eidolon_core::agent::AgentConfig
fn default_max_tokens() -> u32 {
    64_000
}

/// See [`Config::max_iterations`]. Agrees with
/// [`eidolon_core::agent::AgentConfig::default`], which is what a session
/// runs on when the config says nothing.
fn default_max_iterations() -> u32 {
    128
}

impl Config {
    pub fn load(explicit: Option<&Path>) -> anyhow::Result<Self> {
        let path = match explicit {
            Some(p) => p.to_path_buf(),
            None => config_dir().join("config.toml"),
        };
        if !path.exists() {
            if explicit.is_some() {
                bail!("config {} does not exist", path.display());
            }
            // Empty TOML, not derived `Default`, so `#[serde(default = ...)]` fallbacks fire.
            return toml::from_str("").context("building the no-file default config");
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Whether this session announces itself to its neighbours. Absent a
    /// `[swarm]` table, yes.
    pub fn swarm_enabled(&self) -> bool {
        self.swarm.as_ref().is_none_or(|s| s.enabled)
    }

    /// What `[project] instructions` says; `auto` when the table is
    /// absent, because a directory with no such file — the ordinary
    /// directory — makes the question moot.
    pub fn project_instructions(&self) -> eidolon_core::project::ProjectInstructions {
        self.project
            .as_ref()
            .map(|p| p.instructions)
            .unwrap_or_default()
    }

    /// What `[images] inline` says; `auto` when the table is absent.
    pub fn inline_images(&self) -> String {
        self.images
            .as_ref()
            .map(|i| i.inline.clone())
            .unwrap_or_else(auto)
    }

    /// `[transcript] fold`; the TUI default (`settle`) when the table is
    /// absent.
    pub fn fold(&self) -> eidolon_tui::state::Fold {
        self.transcript
            .as_ref()
            .map(|t| t.fold)
            .unwrap_or_default()
    }

    /// `[terminal]`, as the TUI's `T` chords want it. An absent table is
    /// an empty launcher, which is not a disabled one: empty means *work
    /// it out when asked*, and asking is what the first press does.
    pub fn launcher(&self) -> eidolon_tui::launch::Launcher {
        self.terminal
            .as_ref()
            .map_or_else(Default::default, |t| eidolon_tui::launch::Launcher {
                terminal: t.command.clone(),
                editor: t.editor.clone(),
                files: t.files.clone(),
            })
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.sessions_dir.clone().unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("eidolon")
                .join("sessions")
        })
    }

    /// Create a session log under a name nothing is using.
    ///
    /// The millisecond *is* the name, which is unique until two sessions
    /// start in the same one — and starting several at once is what a
    /// swarm is, so this stopped being theoretical the first time two
    /// harnesses were launched together: both picked the same name and the
    /// second died, because `Session::create` refuses to overwrite.
    ///
    /// Asking whether the file exists first would only narrow the race,
    /// not close it — both processes would look, both would see nothing,
    /// both would try. So the creation itself is the test: `create_new` is
    /// atomic, exactly one of them wins it, and the loser takes the next
    /// name. The suffix keeps the timestamp's ordering, which is what the
    /// session picker sorts on.
    pub fn create_session(
        &self,
        model: &str,
        cwd: &Path,
        system: Option<String>,
    ) -> anyhow::Result<eidolon_core::Session> {
        let dir = self.sessions_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        let mut last = None;
        for n in 0u32..64 {
            let path = dir.join(if n == 0 {
                format!("{t}.eid")
            } else {
                format!("{t}-{n}.eid")
            });
            match eidolon_core::Session::create(&path, model, cwd, system.clone()) {
                Ok(s) => return Ok(s),
                // Taken by a session that started in the same millisecond;
                // anything else is a real failure and is reported as one.
                Err(e) if path.exists() => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("could not find an unused session log name")))
    }

    pub fn palette(&self) -> eidolon_verba::Palette {
        self.verba
            .as_ref()
            .map(VerbaEntry::palette)
            .unwrap_or_default()
    }

    /// Which seam the vault command channel enters by. Unconfigured reads as
    /// [`eidolon_verba::Entry::Inline`] — the interceptor is the end-state,
    /// and the tool is the fallback one line of config puts a session back
    /// on.
    pub fn verba_entry(&self) -> eidolon_verba::Entry {
        self.verba.as_ref().map(|v| v.entry).unwrap_or_default()
    }

    pub fn providers_dir(&self) -> PathBuf {
        self.providers_dir
            .clone()
            .unwrap_or_else(|| config_dir().join("providers"))
    }

    /// Where a user's own `*.rn` tools live. Loaded after the built-ins,
    /// so a tool of the same name replaces one.
    pub fn tools_dir(&self) -> PathBuf {
        self.tools_dir
            .as_deref()
            .map(expand_home)
            .unwrap_or_else(|| config_dir().join("tools"))
    }

    /// The operator's own leaf table, if they wrote one. Absent is the
    /// ordinary case and means the table compiled into the binary.
    pub fn policy_path(&self) -> PathBuf {
        config_dir().join("policy.rn")
    }

    /// How to build the classifier, or `None` when the operator turned it
    /// off. Unconfigured is the same as configured-and-enabled: a gate you
    /// get only by asking for it is one most sessions run without.
    pub fn policy_settings(&self) -> Option<eidolon_rune::policy::PolicySettings> {
        let entry = self.policy.as_ref();
        if entry.is_some_and(|p| !p.enabled) {
            return None;
        }
        let mut settings = eidolon_rune::policy::PolicySettings::default();
        if let Some(p) = entry {
            settings.default = p.default;
            settings.toolchains = p.toolchains.clone();
        }
        Some(settings)
    }

    /// The model that answers the gate's questions, and how long to wait for
    /// it. `None` when the operator named none, or when the gate is off
    /// entirely — a second layer over `AllowAll` would have nothing to
    /// escalate.
    /// Whether sessions open with the yolo switch armed — `[policy] yolo`.
    /// With no gate at all there is no switch to arm.
    pub fn policy_yolo(&self) -> bool {
        self.policy.as_ref().is_some_and(|p| p.enabled && p.yolo)
    }

    pub fn policy_judge(&self) -> Option<(String, std::time::Duration)> {
        let entry = self.policy.as_ref()?;
        if !entry.enabled {
            return None;
        }
        let model = entry.judge.clone()?;
        Some((
            model,
            std::time::Duration::from_millis(entry.judge_timeout_ms),
        ))
    }

    pub fn mneme(&self) -> anyhow::Result<Option<Mneme>> {
        self.mneme.as_ref().map(MnemeEntry::client).transpose()
    }

    /// The Melete client and how its listing is registered, when
    /// `[melete]` is configured.
    pub fn melete(&self) -> anyhow::Result<Option<(Melete, eidolon_remote::melete::Options)>> {
        self.melete
            .as_ref()
            .map(|m| Ok((m.client()?, m.options())))
            .transpose()
    }

    pub fn system_prompt(&self) -> String {
        self.system_prompt.clone().unwrap_or_else(|| {
            concat!(
                "You are a coding assistant working in a terminal harness. Ground repository-specific claims in the code and available evidence. ",
                "For implementation tasks, inspect the relevant code, make the smallest coherent change that satisfies the request, ",
                "and verify it with appropriate checks. Preserve unrelated work and follow the project's conventions. ",
                "For review or advice, provide findings without modifying files unless asked. ",
                "Proceed on reasonable, reversible assumptions; ask when ambiguity materially changes the scope or consequences. ",
                "After a failed call, use its error to change the next step: inspect the schema, re-read the target, or check state before retrying. ",
                "Do not repeat an unchanged failed call without evidence of a transient failure. ",
                "During long tasks, give brief progress updates at milestones or blockers; stop re-verifying once the relevant checks pass. ",
                "Be concise, but state important uncertainty and distinguish what you changed, what you verified, and what remains unverified. ",
                "Report only actions and results supported by actual tool responses; never reconstruct command output from expectation."
            ).into()
        })
    }
}

/// The custodied secret store: `secrets.enc` + `secret.key` under the
/// config dir (`secrets_dir` to relocate the key elsewhere).
pub fn secret_store(cfg: &Config) -> harnox::secrets::SecretStore {
    harnox::secrets::SecretStore::at(
        cfg.secrets_dir
            .clone()
            .unwrap_or_else(|| config_dir().join("secrets")),
    )
}

/// The secret a session's `search` tool runs with, when the operator has
/// stored one: `eidolon secret set brave_search < key.txt`.
pub const SEARCH_SECRET: &str = "brave_search";

/// Read [`SEARCH_SECRET`] for a session that is being assembled, or `None` for
/// a session that has no search at all — the key *is* the capability.
///
/// `external_value` is the store's one door out, and this is the Rust-side
/// consumer it is for: the value goes into the host, which puts it in a header
/// and reads it nowhere else. The audit line names the secret and the consumer
/// and never the value, the way a provider's injection does.
pub fn search_key(cfg: &Config) -> Option<String> {
    let key = secret_store(cfg).external_value(SEARCH_SECRET)?;
    tracing::info!(secret = %SEARCH_SECRET, consumer = "search", "secret injected");
    Some(key.to_string())
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("eidolon")
}

/// Build the catalog: the built-in providers, then `[[providers]]` tables,
/// then every `*.rn` in the providers directory (default
/// `~/.config/eidolon/providers`), plus the Claude CLI entry. The order is
/// the override order — a user's `deepseek` replaces the shipped one.
///
/// `fetch` says whether a script's `models()` may wait on the network:
/// a launch passes `Cached` (the last listing, refreshed behind the
/// prompt), `eidolon models` passes `Live`.
pub fn catalog(cfg: &Config, fetch: eidolon_providers::Fetch) -> eidolon_providers::Catalog {
    let claude = cfg
        .claude_cli
        .as_ref()
        .map(|c| eidolon_providers::catalog::ClaudeCliEntry {
            binary: c.binary.clone(),
            models: c.models.clone(),
            tools: c.tools,
        });
    let mut cat = eidolon_providers::Catalog::new()
        .with_claude(claude)
        .with_store(std::sync::Arc::new(secret_store(cfg)))
        .with_fetch(fetch);
    cat.load_builtins();
    // An entry that names a provider already loaded — a built-in, or one
    // an earlier entry declared — *patches* it: `name` plus the one field
    // being changed, with everything else inherited. Anything else is a
    // whole definition, and must say enough to be one.
    for t in &cfg.providers {
        let patched = match eidolon_providers::def::ProviderPatch::from_toml(t) {
            Ok(p) if cat.provider_exists(&p.name) => cat.patch(p, "config.toml"),
            Ok(_) => match eidolon_providers::ProviderDef::from_toml(t) {
                Ok(def) => {
                    cat.add_def(def, None, "config.toml");
                    Ok(())
                }
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        };
        if let Err(e) = patched {
            cat.load_errors
                .push(format!("config.toml [[providers]]: {e:#}"));
        }
    }
    cat.load_scripts(&cfg.providers_dir());
    cat.set_weights(cfg.model_weights.clone());
    for e in &cat.load_errors {
        eprintln!("[provider] {e}");
    }
    cat
}

#[cfg(test)]
mod tests {
    use super::Config;
    use eidolon_core::project::ProjectInstructions;

    #[test]
    fn system_prompt_defaults_without_overriding_operator_text() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.system_prompt().contains("Ground repository-specific claims"));
        assert!(cfg.system_prompt().contains("what remains unverified"));
        let cfg: Config = toml::from_str("system_prompt = 'An operator-owned prompt.'").unwrap();
        assert_eq!(cfg.system_prompt(), "An operator-owned prompt.");
        let cfg: Config = toml::from_str("system_prompt = ''").unwrap();
        assert_eq!(cfg.system_prompt(), "", "an explicit empty prompt is still an override");
    }

    /// `[policy] yolo` is the flag said once. It is read only where there
    /// is a gate to arm: with `enabled = false` there is no switch, and
    /// saying yes to questions nobody asks would be a claim about nothing.
    #[test]
    fn yolo_is_a_standing_answer_only_where_there_is_a_gate() {
        let cfg: Config = toml::from_str("[policy]\nyolo = true\n").unwrap();
        assert!(cfg.policy_yolo());
        let cfg: Config = toml::from_str("[policy]\nyolo = true\nenabled = false\n").unwrap();
        assert!(!cfg.policy_yolo(), "no gate, no switch");
        let cfg: Config = toml::from_str("[policy]\n").unwrap();
        assert!(!cfg.policy_yolo(), "off unless said");
        let cfg: Config = toml::from_str("").unwrap();
        assert!(!cfg.policy_yolo());
    }

    /// `[transcript] fold` names when a finished run of calls folds.
    /// `live` restores the mid-turn fold; an absent table or key is the
    /// settle rule, and a value that is neither is a config error rather
    /// than a silent default.
    #[test]
    fn transcript_fold_names_when_runs_collapse() {
        let cfg: Config = toml::from_str("[transcript]\nfold = \"live\"\n").unwrap();
        assert_eq!(cfg.fold(), eidolon_tui::state::Fold::Live);
        let cfg: Config = toml::from_str("[transcript]\nfold = \"settle\"\n").unwrap();
        assert_eq!(cfg.fold(), eidolon_tui::state::Fold::Settle);
        let cfg: Config = toml::from_str("[transcript]\n").unwrap();
        assert_eq!(cfg.fold(), eidolon_tui::state::Fold::Settle, "an absent key is the default");
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.fold(), eidolon_tui::state::Fold::Settle, "an absent table is the default");
        assert!(toml::from_str::<Config>("[transcript]\nfold = \"whenever\"\n").is_err());
    }

    /// `[project] instructions` picks the file, and an absent table is
    /// `auto` — the ordinary directory holds neither file, so the default
    /// has to be the one that costs nothing there.
    #[test]
    fn project_instructions_name_the_file_and_default_to_auto() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.project_instructions(), ProjectInstructions::Auto);
        let cfg: Config = toml::from_str("[project]\n").unwrap();
        assert_eq!(cfg.project_instructions(), ProjectInstructions::Auto);
        let cfg: Config = toml::from_str("[project]\ninstructions = \"agents\"\n").unwrap();
        assert_eq!(cfg.project_instructions(), ProjectInstructions::Agents);
        let cfg: Config = toml::from_str("[project]\ninstructions = \"off\"\n").unwrap();
        assert_eq!(cfg.project_instructions(), ProjectInstructions::Off);
    }

    /// A missing config file must still land on the documented serde
    /// fallbacks, not the derived-`Default` zeros a plain `Config::default()`
    /// would give `max_tokens`/`max_iterations`. Runs under a scratch
    /// `XDG_CONFIG_HOME` so it exercises `load`'s real no-file branch
    /// without touching the developer's actual `~/.config`.
    #[test]
    fn load_with_no_config_file_keeps_the_documented_defaults() {
        let scratch = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        unsafe { std::env::set_var("XDG_CONFIG_HOME", scratch.path()) };
        let result = Config::load(None);
        match previous {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        let cfg = result.unwrap();
        assert_eq!(cfg.max_iterations, 128, "turn loop must not run 0..0");
        assert_eq!(cfg.max_tokens, 64_000);
    }

    /// The search key comes out of the custodied store under the name the
    /// wiring and the docs both use, and a store with nothing in it answers
    /// `None` — which is a session with no `search` tool, not an error.
    #[test]
    fn the_search_key_comes_from_the_secret_store_or_not_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            secrets_dir: Some(dir.path().to_path_buf()),
            ..toml::from_str("").unwrap()
        };
        assert_eq!(
            super::search_key(&cfg),
            None,
            "nothing stored, nothing to search with"
        );
        super::secret_store(&cfg)
            .set(super::SEARCH_SECRET, "brave-key", None, vec![])
            .unwrap();
        assert_eq!(super::search_key(&cfg).as_deref(), Some("brave-key"));
    }
}
