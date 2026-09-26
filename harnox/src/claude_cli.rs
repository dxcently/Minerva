//! Driving the headless `claude` CLI as a model backend (feature `claude-cli`).
//!
//! A consumer that authenticates by *reusing* the Claude subscription — the
//! same cached OAuth session Claude Code itself uses — does so by shelling out
//! to the real `claude` binary in headless mode rather than hitting the
//! Anthropic API with a metered key. This module is the contract with that
//! binary: the argv for a one-shot or streaming invocation, the JSON result it
//! prints, and the error taxonomy a caller's retry-or-fall-back decision hangs
//! on. [`ClaudeCli::run_task`] is a plain `tokio::process` runner for a
//! consumer with no process hygiene of its own; one that has (nice levels,
//! cgroup scopes, process-group teardown) spawns the binary itself with
//! [`ClaudeCli::build_args`] and hands the stdout to [`ClaudeCli::parse_result`].
//!
//! ## Why hand-rolled rather than `claude-agent-sdk-rs`
//!
//! That crate (tyrchen, tried at v0.6.4) does **not** work against the
//! installed CLI (2.1.177+): the CLI emits a `rate_limit_event` stream message
//! on every turn that the SDK's `Message` enum doesn't know, so its streaming
//! parser hard-fails on *every* invocation. This module sidesteps the whole
//! problem by asking the CLI for `--output-format json` (a single final result
//! object) instead of the streaming `stream-json` the SDK consumes, so
//! intermediate message-type churn in the CLI can't break it.
//!
//! ## Prompt passing: stdin, not argv
//!
//! The prompt text is written to the spawned process's stdin (`-p` with no
//! trailing prompt argument reads it from there — confirmed against the CLI,
//! v2.1.201), never appended as a CLI argument. A prompt that folds in a
//! batch of documents can run to hundreds of KB; threaded through argv that
//! risks the kernel's `ARG_MAX` limit on a single `execve()`, which fails the
//! spawn outright (an `E2BIG`-style error) — confirmed in production
//! (Melete, 2026-07-07): a run's second, much larger call failed to spawn
//! every time while its smaller first call always succeeded. [`build_args`]
//! therefore takes no prompt at all.
//!
//! [`build_args`]: ClaudeCli::build_args

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

/// How much of an unexpected stdout to echo into an error message.
const ERR_CHARS: usize = 400;

/// The `claude` binary plus the model to pin. Cheap to clone; per-run model
/// overrides are [`with_model`](Self::with_model) on a clone.
#[derive(Clone, Debug)]
pub struct ClaudeCli {
    binary: String,
    model: Option<String>,
}

/// Options for a tool-enabled task run. This is how a job that lets Claude
/// act — an MCP config, a per-invocation settings file carrying the caller's
/// hooks, a tool allow-list — is invoked.
#[derive(Debug, Default, Clone)]
pub struct TaskOptions {
    /// Path to a `--mcp-config` JSON file.
    pub mcp_config: Option<PathBuf>,
    /// Only use servers from `--mcp-config`, ignoring ambient MCP config.
    pub strict_mcp_config: bool,
    /// Path to a `--settings` JSON file (per-run hooks).
    pub settings: Option<PathBuf>,
    /// Tools to allow, e.g. `["mcp__mneme__edit_note", ...]` or `mcp__mneme__*`.
    pub allowed_tools: Vec<String>,
    /// Permission mode (`default`, `acceptEdits`, `bypassPermissions`, …).
    pub permission_mode: Option<String>,
    /// Resume the most recent session in `cwd` (`claude --continue`). Used to
    /// steer a run: the prior (killed) turn's state is on disk, so the agent
    /// keeps its context and takes the new prompt as fresh guidance.
    pub continue_session: bool,
    /// Working directory for the `claude` process. For coding jobs this is the
    /// checked-out repo, so relative paths and toolchain commands resolve there.
    pub cwd: Option<PathBuf>,
    /// Extra env vars for the `claude` process. Hook subprocesses it spawns
    /// inherit these.
    pub envs: Vec<(String, String)>,
}

/// The subset of `claude --output-format json` we care about. The CLI emits
/// more fields (usage, session metadata); we deserialize leniently and keep the
/// result text plus a little cost/session telemetry.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeResult {
    /// The assistant's final text.
    #[serde(default)]
    pub result: String,
    /// Whether the CLI reported an error turn.
    #[serde(default)]
    pub is_error: bool,
    /// For multi-turn/session resumption.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub num_turns: Option<u64>,
    /// This turn's own wall-clock time in ms, as the CLI reports it.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    // The CLI's JSON result also carries a `modelUsage` object keyed by the
    // model id(s) that actually served the request (relevant when
    // `--fallback-model` kicks in) — a dynamic-keyed map rather than a plain
    // field; left unread pending a concrete need.
}

/// Classification of a failed headless `claude` invocation. The CLI has no
/// structured error taxonomy of its own (everything surfaces as an
/// `anyhow::Error` from a non-zero exit, a malformed JSON body, or an
/// `is_error` turn), so this is a best-effort read of that error's text.
/// Deliberately separate from the generic `anyhow` error handling — a caller
/// that needs to *decide* whether to retry, fail outright, or fall back to
/// another path classifies the error first; a caller that just wants to log or
/// surface it keeps using the `anyhow::Error` as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeErrorClass {
    /// The CLI's cached subscription session is missing, expired, or invalid.
    AuthFailure,
    /// Rate-/usage-limited. Covers both an API `429`/"rate limit" *and* the
    /// distinct Claude Code subscription **usage-limit** case (a Pro/Max plan's
    /// usage cap being hit), which the CLI phrases as "usage limit reached" —
    /// not as a "rate limit" at all, and without a `429` — so it must be
    /// matched separately. Both feed the same downstream handling. `reset_at`
    /// is whatever reset-time text the CLI's message included (best-effort
    /// scrape), `None` when no such hint was present.
    RateLimit { reset_at: Option<String> },
    /// The invocation did not complete before a caller's own deadline (a
    /// `tokio::time::timeout` around a turn, for instance).
    Timeout,
    /// Anything else — a network error, a non-zero exit for an unrelated
    /// reason, malformed JSON, or an `is_error` turn without a recognizable
    /// cause.
    Other,
}

impl ClaudeErrorClass {
    /// Classify a `claude` failure from its rendered error text (typically
    /// `format!("{err:#}")`, so the full `anyhow` context chain is scanned, not
    /// just the innermost cause).
    pub fn classify(text: &str) -> Self {
        let lower = text.to_lowercase();
        if lower.contains("timed out") || lower.contains("timeout") {
            return Self::Timeout;
        }
        // An API 429 / literal "rate limit", OR the Claude Code CLI's
        // subscription usage-limit message. The latter is phrased as "usage
        // limit reached" (the CLI's own wording, verified against its strings —
        // v2.1.201; its headless usage-cap result reads like "Claude AI usage
        // limit reached|<epoch>") with no "rate limit"/"429" anywhere, which is
        // the blind spot this arm closes. There is no distinct usage-limit
        // *stream* event to key on either — the CLI only emits the one
        // `rate_limit_event` — so text is all we have to go on.
        if lower.contains("rate limit")
            || lower.contains("rate_limit")
            || lower.contains("429")
            || lower.contains("usage limit")
            || lower.contains("usage_limit")
        {
            return Self::RateLimit { reset_at: extract_reset_time(text) };
        }
        if lower.contains("not logged in")
            || lower.contains("invalid api key")
            || lower.contains("unauthorized")
            || lower.contains("authentication")
            || lower.contains("please run")
            || lower.contains("401")
        {
            return Self::AuthFailure;
        }
        Self::Other
    }
}

/// Best-effort scrape of a reset-time hint out of a rate-limit message, e.g.
/// "resets at 3pm", "try again at 15:04:00 UTC", "retry after 30s". Returns
/// the trailing clause after the first recognized anchor phrase, trimmed of
/// surrounding punctuation; `None` when no such anchor is present.
fn extract_reset_time(text: &str) -> Option<String> {
    const ANCHORS: &[&str] = &["resets at ", "reset at ", "try again at ", "retry after ", "resets in "];
    let lower = text.to_lowercase();
    for anchor in ANCHORS {
        if let Some(pos) = lower.find(anchor) {
            let start = pos + anchor.len();
            let rest = text[start..].trim_start();
            let end = rest.find(['.', ',', ')', '\n']).unwrap_or(rest.len());
            let hint = rest[..end].trim();
            if !hint.is_empty() {
                return Some(hint.to_string());
            }
        }
    }
    None
}

impl ClaudeCli {
    /// `binary` is the `claude` executable — a bare name resolved on `PATH`,
    /// or an absolute path for a daemon with a minimal `PATH`. `model` pins
    /// `--model` for every turn; `None` leaves the CLI's default.
    pub fn new(binary: impl Into<String>, model: Option<String>) -> Self {
        Self { binary: binary.into(), model }
    }

    /// Pin a per-run model override (tiering). A `None` override leaves the
    /// configured model in place; `Some(m)` pins `m` for every turn this
    /// instance runs — so a mechanical task can run on a cheaper model than a
    /// hard one without touching global config.
    pub fn with_model(mut self, model: Option<String>) -> Self {
        if model.is_some() {
            self.model = model;
        }
        self
    }

    pub fn binary(&self) -> &str {
        &self.binary
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The CLI args for a one-shot task. The prompt itself is deliberately
    /// **not** included: it's written to the child's stdin instead of appended
    /// as a trailing argv entry (see the module doc), so everything returned
    /// here is flags and paths that stay far under any `ARG_MAX` concern
    /// regardless of how large the prompt is.
    pub fn build_args(&self, opts: &TaskOptions) -> Vec<String> {
        let mut args: Vec<String> = vec!["-p".into(), "--output-format".into(), "json".into()];
        if opts.continue_session {
            args.push("--continue".into());
        }
        if let Some(model) = &self.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        if let Some(mcp) = &opts.mcp_config {
            args.push("--mcp-config".into());
            args.push(mcp.display().to_string());
            if opts.strict_mcp_config {
                args.push("--strict-mcp-config".into());
            }
        }
        if let Some(settings) = &opts.settings {
            args.push("--settings".into());
            args.push(settings.display().to_string());
        }
        if !opts.allowed_tools.is_empty() {
            args.push("--allowedTools".into());
            // The CLI accepts a space-separated list after the flag; pass each
            // as its own argv entry.
            for tool in &opts.allowed_tools {
                args.push(tool.clone());
            }
        }
        if let Some(mode) = &opts.permission_mode {
            args.push("--permission-mode".into());
            args.push(mode.clone());
        }
        args
    }

    /// The CLI args for a **streaming-session** invocation: the same tool
    /// surface [`build_args`](Self::build_args) assembles, but driven over
    /// stdin/stdout as JSON lines instead of one prompt in and one result out.
    ///
    /// `--input-format stream-json` is what keeps the process alive across
    /// turns; the CLI rejects `--output-format stream-json` without `--verbose`
    /// (confirmed against 2.1.222), and `--include-partial-messages` is what
    /// makes the reply visible while it is still being written. The caller
    /// owns the turn loop over the piped child.
    pub fn streaming_args(&self, opts: &TaskOptions) -> Vec<String> {
        let mut args = self.build_args(opts);
        // `build_args` opens with `-p --output-format json`; swap that final
        // form for the streaming one rather than appending a contradiction.
        if let Some(i) = args.iter().position(|a| a == "--output-format") {
            args[i + 1] = "stream-json".to_string();
        }
        args.extend([
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--include-partial-messages".to_string(),
            "--verbose".to_string(),
        ]);
        args
    }

    /// Parse the CLI's `--output-format json` stdout. A parse failure is a
    /// real signal — the tool's output schema changed — and surfaces loudly;
    /// an `is_error` turn is an `Err` whose text carries the CLI's own message
    /// (which is what [`ClaudeErrorClass::classify`] then reads).
    pub fn parse_result(stdout: &str) -> Result<ClaudeResult> {
        let parsed: ClaudeResult = serde_json::from_str(stdout.trim())
            .with_context(|| format!("parsing claude JSON output: {}", truncated(stdout)))?;
        if parsed.is_error {
            bail!("claude reported an error turn: {}", parsed.result);
        }
        Ok(parsed)
    }

    /// One-shot headless prompt with no tools/MCP — the plain text round-trip
    /// used to smoke-test that auth works.
    pub async fn prompt(&self, text: &str) -> Result<ClaudeResult> {
        self.run_task(text, &TaskOptions::default()).await
    }

    /// Run a tool-enabled task with a plain `tokio::process` spawn and return
    /// the parsed final result. The child is killed if the future is dropped.
    /// A consumer with its own process hygiene (scheduling demotion, cgroup
    /// caps, process-group teardown) spawns the binary itself with
    /// [`build_args`](Self::build_args) and calls [`parse_result`](Self::parse_result)
    /// on the stdout instead.
    ///
    /// The prompt is written to stdin from a task spawned concurrently with
    /// reading the child's stdout/stderr, not written-then-awaited
    /// sequentially: if a large prompt were written first and the child's own
    /// output pipe filled up before it finished consuming stdin, a strictly
    /// sequential write-then-read would deadlock (child blocked writing
    /// output, us blocked writing input).
    pub async fn run_task(&self, prompt: &str, opts: &TaskOptions) -> Result<ClaudeResult> {
        let args = self.build_args(opts);
        tracing::debug!(binary = %self.binary, ?args, "invoking claude CLI");

        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.args(&args)
            .envs(opts.envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = opts.cwd.as_deref() {
            cmd.current_dir(cwd);
        }
        let mut child = cmd.spawn().with_context(|| spawn_context(&self.binary))?;

        let mut stdin = child.stdin.take().context("claude stdin was not piped")?;
        let prompt = prompt.to_string();
        let writer = tokio::spawn(async move {
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.shutdown().await
        });
        let output = child.wait_with_output().await.context("waiting for claude")?;
        // A write error here (the child exited before reading its prompt) is
        // already the story the exit status below tells better.
        let _ = writer.await;

        if !output.status.success() {
            bail!(
                "`{}` exited with {}: {}",
                self.binary,
                output.status,
                failure_detail(&output.stderr, &output.stdout)
            );
        }
        let stdout = String::from_utf8(output.stdout).context("claude stdout was not UTF-8")?;
        Self::parse_result(&stdout)
    }
}

/// The detail half of a non-zero-exit message: trimmed stderr when there is
/// any, else a truncated tail of stdout labelled as such — the `claude` CLI in
/// headless mode reports its failures on **stdout**, and without this fallback
/// an unauthenticated CLI produces a bare, causeless `exited with 1:` line.
fn failure_detail(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = String::from_utf8_lossy(stdout);
    if stdout.trim().is_empty() {
        return "(no output on stderr or stdout)".to_string();
    }
    format!("(nothing on stderr; stdout said) {}", truncated(stdout.trim()))
}

fn truncated(s: &str) -> String {
    let shown: String = s.chars().take(ERR_CHARS).collect();
    let ellipsis = if s.chars().count() > ERR_CHARS { "…" } else { "" };
    format!("{shown}{ellipsis}")
}

/// Only `NotFound` actually means "the binary isn't on PATH" — every other
/// spawn failure (permissions, a resource limit, `ARG_MAX`, …) must report the
/// real error rather than a misleading PATH detour.
fn spawn_context(binary: &str) -> String {
    format!("spawning `{binary}` (the claude CLI; if this is NotFound, it isn't on PATH — set an absolute path)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_auth_failure() {
        assert_eq!(
            ClaudeErrorClass::classify("claude CLI exited with 1: Invalid API key · Please run /login"),
            ClaudeErrorClass::AuthFailure
        );
        assert_eq!(ClaudeErrorClass::classify("Error: not logged in"), ClaudeErrorClass::AuthFailure);
    }

    #[test]
    fn classifies_rate_limit_and_extracts_reset_time() {
        let class = ClaudeErrorClass::classify("Error: rate limit exceeded, resets at 3:04pm UTC.");
        assert_eq!(class, ClaudeErrorClass::RateLimit { reset_at: Some("3:04pm UTC".to_string()) });
    }

    #[test]
    fn classifies_rate_limit_without_a_reset_hint() {
        let class = ClaudeErrorClass::classify("claude CLI exited with 1: 429 Too Many Requests");
        assert_eq!(class, ClaudeErrorClass::RateLimit { reset_at: None });
    }

    /// The Claude Code CLI's subscription usage-limit message — a Pro/Max
    /// plan's usage cap being hit. Unlike an API 429 it contains no "rate
    /// limit"/"429" text, so it slipped straight through an earlier classifier.
    /// It reaches `classify` wrapped by `parse_result`'s "claude reported an
    /// error turn: …" prefix. No parseable textual reset hint (an absolute
    /// epoch, not "resets at …"), so `reset_at` is None — the point is that it
    /// is caught at all.
    #[test]
    fn classifies_subscription_usage_limit_reached() {
        let class =
            ClaudeErrorClass::classify("claude reported an error turn: Claude AI usage limit reached|1751808600");
        assert_eq!(class, ClaudeErrorClass::RateLimit { reset_at: None });
    }

    #[test]
    fn classifies_timeout() {
        assert_eq!(
            ClaudeErrorClass::classify("claude CLI timed out after 60s waiting for a Q&A response"),
            ClaudeErrorClass::Timeout
        );
    }

    #[test]
    fn classifies_other_for_unrecognized_errors() {
        assert_eq!(
            ClaudeErrorClass::classify("claude CLI exited with 1: something went sideways"),
            ClaudeErrorClass::Other
        );
    }

    #[test]
    fn reset_time_extraction_stops_at_punctuation() {
        assert_eq!(extract_reset_time("rate limited; try again at 14:30, thanks"), Some("14:30".to_string()));
    }

    #[test]
    fn with_model_pins_only_when_given() {
        let cli = ClaudeCli::new("claude", Some("sonnet".into()));
        assert_eq!(cli.clone().with_model(None).model(), Some("sonnet"));
        assert_eq!(cli.with_model(Some("opus".into())).model(), Some("opus"));
    }

    /// Regression test for the ARG_MAX incident: `build_args` takes no prompt
    /// at all — it's written to the child's stdin — so a huge prompt
    /// structurally has nowhere to land in argv.
    #[test]
    fn build_args_never_carries_the_prompt_no_matter_how_large() {
        let cli = ClaudeCli::new("claude", Some("opus".into()));
        let opts = TaskOptions {
            mcp_config: Some("/run/mcp.json".into()),
            strict_mcp_config: true,
            allowed_tools: vec!["Bash".into(), "mcp__mneme__edit_note".into()],
            permission_mode: Some("acceptEdits".into()),
            continue_session: true,
            ..Default::default()
        };
        let args = cli.build_args(&opts);
        let huge_prompt = "n".repeat(600_000);
        assert!(args.iter().all(|a| a.len() < 1_000));
        assert!(!args.iter().any(|a| a.contains(&huge_prompt[..1_000])));
        assert_eq!(
            args,
            [
                "-p", "--output-format", "json", "--continue", "--model", "opus", "--mcp-config", "/run/mcp.json",
                "--strict-mcp-config", "--allowedTools", "Bash", "mcp__mneme__edit_note", "--permission-mode",
                "acceptEdits",
            ]
        );
    }

    #[test]
    fn streaming_args_swap_the_output_format_and_add_the_session_flags() {
        let args = ClaudeCli::new("claude", None).streaming_args(&TaskOptions::default());
        assert_eq!(
            args,
            ["-p", "--output-format", "stream-json", "--input-format", "stream-json", "--include-partial-messages", "--verbose"]
        );
    }

    #[test]
    fn parse_result_is_lenient_on_fields_and_loud_on_errors() {
        let r = ClaudeCli::parse_result(r#"{"result":"OK","session_id":"s1","total_cost_usd":0.01,"unknown":1}"#).unwrap();
        assert_eq!(r.result, "OK");
        assert_eq!(r.session_id.as_deref(), Some("s1"));
        let err = ClaudeCli::parse_result(r#"{"result":"Claude AI usage limit reached|1","is_error":true}"#)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("claude reported an error turn:"), "got: {err}");
        assert_eq!(ClaudeErrorClass::classify(&err), ClaudeErrorClass::RateLimit { reset_at: None });
        let err = format!("{:#}", ClaudeCli::parse_result("not json at all").unwrap_err());
        assert!(err.contains("parsing claude JSON output: not json"), "got: {err}");
    }

    #[test]
    fn failure_detail_falls_back_to_stdout_then_to_an_explicit_note() {
        assert_eq!(failure_detail(b" boom \n", b"ignored"), "boom");
        assert_eq!(failure_detail(b"", b"Please run /login"), "(nothing on stderr; stdout said) Please run /login");
        assert_eq!(failure_detail(b"", b" "), "(no output on stderr or stdout)");
    }

    /// The default runner end to end, against a stand-in "claude" that echoes
    /// its stdin back as the result — proves the prompt travels over stdin and
    /// the concurrent write doesn't deadlock on a large one.
    #[tokio::test]
    async fn run_task_feeds_the_prompt_over_stdin() {
        let dir = crate::testutil::tempdir("claude-cli");
        let fake = dir.join("claude");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprompt=$(cat)\nprintf '{\"result\":\"%s\",\"num_turns\":1}' \"$(printf '%s' \"$prompt\" | wc -c | tr -d ' ')\"\n",
        )
        .unwrap();
        crate::fs::chmod_600(&fake).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let cli = ClaudeCli::new(fake.display().to_string(), None);
        let prompt = "p".repeat(300_000);
        let r = cli.run_task(&prompt, &TaskOptions::default()).await.unwrap();
        assert_eq!(r.result, "300000");
        assert_eq!(r.num_turns, Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
