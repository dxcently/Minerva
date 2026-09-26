//! The Claude CLI as a turn driver — "wrap the CLI, exactly as Melete does".
//!
//! `claude -p` is an agent, not a model: it runs its own loop and its own
//! tools (`Read`, `Edit`, `Bash`, …) and keeps its own session on disk. So
//! it plugs in as a [`TurnDriver`], not a `Provider`, and four things keep
//! the harness's guarantees intact:
//!
//! 1. **Policy stays ours.** Every tool the CLI wants to run first hits a
//!    `PreToolUse` hook (`eidolon hook`), which connects to a unix socket
//!    this driver listens on and asks [`Dispatcher::adjudicate`]. A
//!    read-only tool is allowed, a mutating one goes to the same approval
//!    dialog the harness's own tools use, and a hook that cannot reach us
//!    denies. The CLI never prompts on its own — headless, it cannot.
//! 2. **The log stays the truth.** The CLI's `stream-json` output reports
//!    every assistant message, tool use and tool result; the driver
//!    journals them as the same record kinds the provider loop writes, plus
//!    a `BackendSession` carrying the CLI's session id so the next turn
//!    resumes it (`--resume`). Forking below that point forks *our* log
//!    only — the CLI's session keeps its full history, which is a known v1
//!    limitation.
//! 3. **Consumers cannot tell.** Deltas, tool events and `TurnSettled` go
//!    out on the bus exactly as the provider loop publishes them.
//! 4. **Switching models keeps the conversation.** The CLI resumes only
//!    what it said itself, so a branch whose earlier turns ran on a
//!    provider — or on the CLI before `:model` left and came back — is
//!    history it has never seen. That gap is exactly the records after its
//!    last `BackendSession` mark, and [`transcript::preamble`] renders them
//!    in front of the prompt. The mark is re-journaled at the end of every
//!    turn, so what has already been handed over is never handed over
//!    twice.
//!
//! One process per turn; the prompt goes over stdin as a `stream-json`
//! user message and stdin is closed, so the CLI exits after `result`.
//! Cancellation kills the process group.
//!
//! Wire facts this module relies on, confirmed against Claude Code 2.1.257
//! (fixture in `tests/fixtures/salve.jsonl`): `system/init` carries
//! `session_id`; `stream_event.event` is the raw Anthropic SSE event;
//! `assistant.message.content` arrives **per content block** with a shared
//! `message.id`; `user.message.content` carries `tool_result` blocks whose
//! `content` is a string or a list of text parts; `result` carries `usage`,
//! `total_cost_usd`, `stop_reason` and `is_error`.

pub mod hook;
pub mod mcp;
pub mod stream;
pub mod transcript;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context as _;
use futures_util::future::BoxFuture;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use eidolon_core::agent::{Agent, TurnDriver, TurnOutcome};
use eidolon_core::event::Event;
use eidolon_core::message::{ContentBlock, StopReason};
use eidolon_core::session::RecordKind;
use harnox::claude_cli::{ClaudeCli, TaskOptions};

pub const BACKEND: &str = "claude-cli";
/// Env var the hook subprocess reads to find us.
pub const HOOK_SOCKET_ENV: &str = "EIDOLON_HOOK_SOCKET";
/// Env var the `eidolon mcp` subprocess reads to find us.
pub const MCP_SOCKET_ENV: &str = "EIDOLON_MCP_SOCKET";

/// Which tools the CLI may use for a turn.
///
/// The default is [`ToolMode::Own`], because Claude Code is tuned for its own
/// tools and is measurably better with them. [`ToolMode::Eidolon`] trades
/// some of that for policy and execution the harness owns outright.
///
/// There is deliberately no "both" mode. Offered its own tools alongside
/// ours, Claude consistently picks its own, so the registry goes unused and
/// the mode answers no question. Its absence is also a safety property: the
/// only mode that passes `--tools ""` is the one that serves the MCP server,
/// so the built-ins can never be disabled without a replacement — see the
/// note in [`mcp`] on what a tool-starved model does instead of stopping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolMode {
    /// The CLI's built-ins, policed through the `PreToolUse` hook.
    #[default]
    Own,
    /// Only the Eidolon registry, served over MCP. Built-ins are disabled.
    Eidolon,
}

/// Whether `tool` is one of ours arriving back through the CLI's MCP client.
/// Such a call is adjudicated *and executed* by [`Dispatcher::dispatch`], so
/// the `PreToolUse` hook must let it past rather than asking a second time.
pub fn is_eidolon_tool(tool: &str) -> bool {
    tool.strip_prefix("mcp__")
        .and_then(|r| r.strip_prefix(mcp::SERVER))
        .is_some_and(|r| r.starts_with("__"))
}

pub struct ClaudeDriver {
    cli: ClaudeCli,
    /// The harness binary, for the hook and MCP commands.
    exe: PathBuf,
    cwd: PathBuf,
    /// Where per-turn scratch (settings file, socket) lives.
    scratch: PathBuf,
    tools: ToolMode,
}

/// One append flag for the stable harness guidance. Never duplicate the CLI's
/// own project prompt or claim it has the registry's tools in `Own` mode.
fn system_args(mode: ToolMode, vault: Option<&str>, tools: Option<String>) -> Vec<String> {
    let parts: Vec<String> = [vault.map(str::to_string), tools.filter(|_| mode == ToolMode::Eidolon)]
        .into_iter().flatten().collect();
    if parts.is_empty() { Vec::new() } else { vec!["--append-system-prompt".into(), parts.join("\n\n")] }
}

impl ClaudeDriver {
    pub fn new(
        binary: impl Into<String>,
        model: Option<String>,
        exe: PathBuf,
        cwd: PathBuf,
        scratch: PathBuf,
        tools: ToolMode,
    ) -> Self {
        ClaudeDriver {
            cli: ClaudeCli::new(binary, model),
            exe,
            cwd,
            scratch,
            tools,
        }
    }

    /// The tool-surface flags for this turn's mode.
    ///
    /// All of it or none of it: `--mcp-config` supplies the registry,
    /// `--strict-mcp-config` keeps the user's own servers out, and only with
    /// those in place is it safe to empty the built-in allowlist with
    /// `--tools ""`. Kept in one function so that stays true by construction.
    fn tool_args(&self, mcp_socket: &Path) -> Vec<String> {
        match self.tools {
            ToolMode::Own => Vec::new(),
            ToolMode::Eidolon => vec![
                "--mcp-config".into(),
                self.mcp_config_json(mcp_socket),
                "--strict-mcp-config".into(),
                "--tools".into(),
                String::new(),
            ],
        }
    }

    /// The `--mcp-config` payload naming `eidolon mcp` as a stdio server.
    fn mcp_config_json(&self, socket: &Path) -> String {
        serde_json::json!({
            "mcpServers": {
                mcp::SERVER: {
                    "command": self.exe.display().to_string(),
                    "args": ["mcp"],
                    "env": { MCP_SOCKET_ENV: socket.display().to_string() },
                }
            }
        })
        .to_string()
    }

    fn settings_json(&self) -> String {
        let cmd = format!("{} hook", self.exe.display());
        serde_json::json!({
            "hooks": { "PreToolUse": [ { "matcher": "", "hooks": [ { "type": "command", "command": cmd } ] } ] }
        })
        .to_string()
    }
}

impl TurnDriver for ClaudeDriver {
    fn name(&self) -> &str {
        BACKEND
    }

    fn run_turn<'a>(
        &'a self,
        agent: &'a Agent,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<TurnOutcome>> {
        Box::pin(async move { self.turn(agent, cancel).await })
    }
}

impl ClaudeDriver {
    async fn turn(&self, agent: &Agent, cancel: CancellationToken) -> anyhow::Result<TurnOutcome> {
        let input = {
            let s = agent.session().lock().await;
            turn_input(&s)
        };
        let Some((resume, text, images)) = input else {
            // Nothing to send: the branch already ends on an answer.
            return Ok(TurnOutcome::Settled {
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
                calls: 0,
            });
        };
        let model = agent.config().model;

        std::fs::create_dir_all(&self.scratch)
            .with_context(|| format!("creating {}", self.scratch.display()))?;
        let nonce = format!("{}-{}", std::process::id(), now_ms());
        let settings_path = self.scratch.join(format!("settings-{nonce}.json"));
        std::fs::write(&settings_path, self.settings_json())?;
        // Unix socket paths are capped at ~108 bytes, so the socket lives in
        // the runtime dir under a short name, never under the session tree.
        let sock_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let socket_path = sock_dir.join(format!("eidolon-hook-{nonce}.sock"));
        let listener =
            hook::serve(&socket_path, agent.dispatcher().clone(), cancel.clone()).await?;

        // The registry, served over MCP, when the mode calls for it. The
        // socket is this turn's; `eidolon mcp` is spawned by the CLI and
        // finds it through the environment.
        let mcp_socket = sock_dir.join(format!("eidolon-mcp-{nonce}.sock"));
        let mcp_listener = if self.tools == ToolMode::Eidolon {
            Some(mcp::serve(&mcp_socket, agent.dispatcher().clone(), cancel.clone()).await?)
        } else {
            None
        };

        let cli = self
            .cli
            .clone()
            .with_model(if model.is_empty() || model == BACKEND {
                None
            } else {
                Some(model)
            });
        let mut envs = vec![(
            HOOK_SOCKET_ENV.to_string(),
            socket_path.display().to_string(),
        )];
        if self.tools == ToolMode::Eidolon {
            envs.push((MCP_SOCKET_ENV.to_string(), mcp_socket.display().to_string()));
        }
        let opts = TaskOptions {
            settings: Some(settings_path.clone()),
            cwd: Some(self.cwd.clone()),
            envs,
            ..Default::default()
        };
        let mut args = cli.streaming_args(&opts);
        args.extend(self.tool_args(&mcp_socket));
        // The CLI composes its own prompt. Append the vault capability in
        // either tool mode, plus registry guidance only when it runs our tools.
        args.extend(system_args(self.tools, agent.vault_guidance(),
            agent.dispatcher().registry().guidance()));
        if let Some(id) = &resume {
            args.push("--resume".into());
            args.push(id.clone());
        }

        let mut cmd = tokio::process::Command::new(cli.binary());
        cmd.args(&args)
            .current_dir(&self.cwd)
            .envs(opts.envs.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // A session of its own, so nothing the CLI or a grandchild of its
        // does can reach the terminal the TUI is drawing on: a child with
        // the harness's terminal as its controlling one paints any prompt
        // it asks (a password, a confirmation) straight over the UI's own
        // cells, at the cursor the frame left there. `setsid` leaves it
        // no terminal to ask on, and its stdio is piped.
        // Safety: the closure calls `setsid` only, which is
        // async-signal-safe; the child is a fresh fork in the parent's
        // group, so the call cannot fail with `EPERM`.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning {}", cli.binary()))?;
        let pid = child.id();

        // The prompt over stdin, then EOF so the process ends after `result`.
        {
            let mut stdin = child.stdin.take().context("claude stdin")?;
            // Images first, then the words — the order both wires read a
            // message in, and the order the transcript draws.
            let mut content: Vec<serde_json::Value> =
                images.iter().filter_map(image_to_wire).collect();
            content.push(serde_json::json!({ "type": "text", "text": text }));
            let line = serde_json::json!({ "type": "user", "message": { "role": "user", "content": content } });
            stdin.write_all(format!("{line}\n").as_bytes()).await?;
            stdin.shutdown().await?;
        }
        let stdout = child.stdout.take().context("claude stdout")?;
        let stderr = child.stderr.take().context("claude stderr")?;
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if buf.len() < 8192 {
                    buf.push_str(&l);
                    buf.push('\n');
                }
            }
            buf
        });

        let mut sink = stream::Sink::new(agent, resume);
        let mut lines = BufReader::new(stdout).lines();
        let outcome = loop {
            let next = tokio::select! {
                l = lines.next_line() => l,
                _ = cancel.cancelled() => {
                    kill_group(pid);
                    // The count is what this backend knows about a turn
                    // it did not finish; the usage is not, and is left
                    // at nothing rather than guessed at.
                    break TurnOutcome::Cancelled { usage: Default::default(), calls: sink.model_calls() };
                }
            };
            match next {
                Ok(Some(line)) => {
                    if let Err(e) = sink.line(&line).await {
                        tracing::warn!(error = %e, "claude stream line");
                    }
                }
                Ok(None) => break sink.finish().await?,
                Err(e) => {
                    kill_group(pid);
                    anyhow::bail!("reading claude stdout: {e}");
                }
            }
        };
        let status = child.wait().await;
        // Whatever it managed to say, the CLI's own session now holds the
        // branch up to here: mark it, so the next turn replays only what
        // comes after and never this again.
        sink.caught_up().await?;
        let _ = std::fs::remove_file(&settings_path);
        listener.abort();
        let _ = std::fs::remove_file(&socket_path);
        if let Some(t) = mcp_listener {
            t.abort();
            let _ = std::fs::remove_file(&mcp_socket);
        }
        let err_text = stderr_task.await.unwrap_or_default();

        match outcome {
            TurnOutcome::Cancelled { usage, calls } => {
                // The measurement the killed turn *did* make, journaled
                // before the boundary explains the spend: every assistant
                // event carries one call's input, and nothing here reaches
                // core otherwise — `TurnOutcome::Cancelled` carries the
                // turn's spend and cannot carry a context. Without this the
                // gauge sits on the previous turn's number, which on a
                // just-compacted branch is the summary's own size.
                sink.journal_context().await?;
                Ok(TurnOutcome::Cancelled { usage, calls })
            }
            out => {
                if let Ok(st) = status
                    && !st.success()
                    && !sink.saw_result()
                {
                    let class = harnox::claude_cli::ClaudeErrorClass::classify(&err_text);
                    agent.bus().publish(Event::Error(format!(
                        "claude exited {st}: {class:?}: {}",
                        err_text.trim()
                    )));
                    anyhow::bail!("claude exited {st}: {}", err_text.trim());
                }
                Ok(out)
            }
        }
    }
}

/// What this turn has to say to the CLI: the session id to `--resume`, and
/// the prompt itself.
///
/// The prompt is the pending user message, but not only that. The CLI
/// resumes the conversation *it* had, so anything the branch said through
/// another backend — the session began on a provider, or `:model` left and
/// came back — is missing from it, and a question like "did you see the
/// file it made?" arrives with nothing to refer to. Those records are
/// exactly the ones after the last `BackendSession` mark, and they go in
/// front of the prompt as a transcript. `None` means the branch ends on an
/// answer and there is nothing to ask.
fn turn_input(
    s: &eidolon_core::session::Session,
) -> Option<(Option<String>, String, Vec<ContentBlock>)> {
    let mark = s.backend_session_mark(BACKEND);
    let pending = s.pending_user_message()?;
    let mut unseen = s.messages_after(mark.as_ref().map(|(id, _)| *id));
    // The pending message is the prompt; it must not also be the transcript.
    // It is the *tail* of the last message rather than the last message
    // when it landed on tool results the branch still owed: `messages_after`
    // folds those into one user message, results first, so the results
    // stay in the transcript and only the words come off.
    if let Some(last) = unseen.last_mut()
        && last.role == eidolon_core::Role::User
        && last.content.ends_with(&pending.content)
    {
        let keep = last.content.len() - pending.content.len();
        last.content.truncate(keep);
        if last.content.is_empty() {
            unseen.pop();
        }
    }
    let models = models_after(s, mark.as_ref().map(|(id, _)| *id));
    let text = match transcript::preamble(&unseen, &models) {
        Some(p) => {
            tracing::debug!(
                messages = unseen.len(),
                ?models,
                "catching the claude session up on turns it did not run"
            );
            format!("{p}\n\n{}", pending.text())
        }
        None => pending.text(),
    };
    // Images travel as themselves. They cannot go into the preamble the
    // way the unseen turns do — that is prose, and a picture rendered as
    // prose is the placeholder, not the picture — so they are handed back
    // beside the text and put on the message the CLI is sent.
    //
    // **The catch-up span's images go too, not just the pending
    // message's.** This backend used to send only the latter, on the
    // reasoning that resending every image on the branch would re-upload
    // the whole history's pixels on every turn. That reasoning was about
    // the wrong span: `unseen` is bounded by the `BackendSession` mark,
    // and the mark is rewritten on *every* CLI turn, so these images are
    // sent once — on the switch — and never again. What the old rule
    // actually did was lose the picture at the only moment it mattered:
    // attach a screenshot, ask another model about it, switch to this one,
    // and it was told "an image was attached" and shown nothing, which
    // reads exactly like a model that cannot see.
    let mut images: Vec<ContentBlock> = unseen
        .iter()
        .chain(std::iter::once(&pending))
        .flat_map(|m| m.content.iter())
        .filter(|b| matches!(b, ContentBlock::Image { .. }))
        .cloned()
        .collect();
    // Bounded all the same, because a long stretch on another backend can
    // accumulate more than a request should carry. The *newest* survive —
    // the pending message's above all, since that is what the operator is
    // asking about — and the preamble still names the ones that do not, so
    // a dropped image is described rather than vanishing.
    let mut budget = MAX_CARRIED_IMAGE_BYTES;
    let keep = images
        .iter()
        .rev()
        .take_while(|b| match b {
            ContentBlock::Image { source, .. } => {
                budget = budget.saturating_sub(source.byte_len().unwrap_or(0));
                budget > 0
            }
            _ => false,
        })
        .count()
        .max(1);
    if keep < images.len() {
        tracing::debug!(
            carried = keep,
            dropped = images.len() - keep,
            "trimming images on the way to the claude session"
        );
        images.drain(..images.len() - keep);
    }
    Some((mark.map(|(_, id)| id), text, images))
}

/// How many bytes of image one catch-up may carry. Generous, because it
/// is paid once per backend switch rather than per turn, and stingy enough
/// that a long stretch of screenshots on another model cannot build a
/// request the CLI will refuse.
const MAX_CARRIED_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// An image block as the CLI's stream-json input wants it, which is the
/// Anthropic content block unchanged — the CLI speaks the same wire.
fn image_to_wire(b: &ContentBlock) -> Option<serde_json::Value> {
    let ContentBlock::Image { source, .. } = b else {
        return None;
    };
    Some(serde_json::json!({ "type": "image", "source": source }))
}

/// The models that answered over the unseen span, oldest first.
///
/// The transcript is what the CLI is told about turns it did not run, and
/// the user refers to whoever ran them by name — "look at what mistral just
/// made". So the names travel with it: without them every earlier turn is
/// an anonymous `assistant:`, and that question points at nobody.
///
/// The walk starts at the top of the branch, not at `after`, because the
/// model in effect at `after` was set by a `ModelChanged` that may sit well
/// above it.
fn models_after(
    s: &eidolon_core::session::Session,
    after: Option<eidolon_core::session::RecordId>,
) -> Vec<String> {
    let branch = s.branch();
    let start = after
        .and_then(|id| branch.iter().position(|r| r.id == id))
        .map_or(0, |i| i + 1);
    let mut current: Option<&str> = None;
    let mut out: Vec<String> = Vec::new();
    for (i, r) in branch.iter().enumerate() {
        match &r.kind {
            RecordKind::SessionStart { model, .. } | RecordKind::ModelChanged { model } => {
                current = Some(model)
            }
            // Only a model that actually spoke is named; a `:model` that was
            // switched away from before it answered never ran anything.
            RecordKind::AssistantMessage(_) if i >= start => {
                if let Some(m) = current
                    && !out.iter().any(|o| o == m)
                {
                    out.push(m.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        // Safety: signalling the group of a child we spawned with its own
        // process group; the gid is the child's pid.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Approval tier for a Claude Code built-in, by name. Anything unknown —
/// including every `mcp__*` tool — is treated as mutating, so it asks.
pub fn approval_for(tool: &str) -> eidolon_core::policy::Approval {
    use eidolon_core::policy::Approval::*;
    match tool {
        "Read" | "Glob" | "Grep" | "LS" | "WebFetch" | "WebSearch" | "TodoRead" | "ToolSearch"
        | "LSP" | "TaskList" | "TaskGet" => ReadOnly,
        _ => Mutating,
    }
}

/// Convenience for consumers building the driver from a config table.
pub fn driver(
    binary: Option<String>,
    model: Option<String>,
    cwd: PathBuf,
    scratch: PathBuf,
    tools: ToolMode,
) -> anyhow::Result<Arc<dyn TurnDriver>> {
    let exe = std::env::current_exe()
        .context("locating the harness binary for the hook and mcp commands")?;
    Ok(Arc::new(ClaudeDriver::new(
        binary.unwrap_or_else(|| "claude".into()),
        model,
        exe,
        cwd,
        scratch,
        tools,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidolon_core::message::{ContentBlock, Json, Message};
    use eidolon_core::session::{RecordId, RecordKind, Session};

    fn settled(s: &mut Session) -> RecordId {
        s.append(RecordKind::TurnSettled {
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
        })
        .unwrap()
    }

    /// The bug this guards: the CLI resumes only its own conversation, so a
    /// session that began on another backend used to reach it with the
    /// question and nothing else — "did you see the file it made?" against
    /// an empty history.
    /// The pending message's images come back beside its text so they
    /// can go on the message the CLI is sent. They cannot ride in the
    /// preamble: that is prose, and a picture written as prose is the
    /// placeholder rather than the picture.
    #[test]
    fn the_pending_messages_images_travel_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(
            &dir.path().join("s.eid"),
            "claude-cli:sonnet",
            dir.path(),
            None,
        )
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user(vec![
            ContentBlock::image("image/png", "AAAA", Some("shot.png".into())),
            ContentBlock::text("what is this?"),
        ])))
        .unwrap();
        let (_, prompt, images) = turn_input(&s).unwrap();
        assert_eq!(prompt, "what is this?");
        assert_eq!(images.len(), 1);
        let wire = image_to_wire(&images[0]).unwrap();
        assert_eq!(wire["type"], "image");
        assert_eq!(wire["source"]["type"], "base64");
        assert_eq!(wire["source"]["media_type"], "image/png");
        assert_eq!(wire["source"]["data"], "AAAA");
    }

    /// **The bug this backend shipped with.** Attach a picture, ask
    /// another model about it, then switch here: the image is on a turn
    /// this CLI session never ran, and it used to be *named* in the
    /// preamble and never sent — which reads exactly like a model that
    /// cannot see. The catch-up span's images travel with it.
    #[test]
    fn a_picture_from_a_turn_this_session_did_not_run_is_carried_over() {
        let dir = tempfile::tempdir().unwrap();
        let mut s =
            Session::create(&dir.path().join("s.eid"), "fau:gemini", dir.path(), None).unwrap();
        s.append(RecordKind::UserMessage(Message::user(vec![
            ContentBlock::image("image/png", "AAAA", Some("puppies.png".into())),
            ContentBlock::text("can you see this?"),
        ])))
        .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("two puppies"),
        ])))
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("and now?")))
            .unwrap();
        let (_, text, images) = turn_input(&s).unwrap();
        assert_eq!(images.len(), 1, "the picture was left behind: {images:?}");
        assert!(
            matches!(&images[0], ContentBlock::Image { alt, .. } if alt.as_deref() == Some("puppies.png"))
        );
        // And the preamble still says whose turn it was on, so the model
        // can tell which picture goes with which question.
        assert!(text.contains("puppies.png"), "{text}");
    }

    /// But not one this session has already been shown. The mark moves to
    /// the end of the branch on every CLI turn, so anything below it is
    /// in the CLI's own session and re-sending would be paying for the
    /// same pixels twice.
    #[test]
    fn a_picture_this_session_has_already_seen_is_not_sent_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(
            &dir.path().join("s.eid"),
            "claude-cli:sonnet",
            dir.path(),
            None,
        )
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user(vec![
            ContentBlock::image("image/png", "AAAA", Some("old.png".into())),
        ])))
        .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("a screenshot"),
        ])))
        .unwrap();
        s.append(RecordKind::BackendSession {
            backend: BACKEND.into(),
            id: "sess-1".into(),
        })
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("and now?")))
            .unwrap();
        let (resume, _, images) = turn_input(&s).unwrap();
        assert_eq!(resume.as_deref(), Some("sess-1"));
        assert!(
            images.is_empty(),
            "an image below the mark was sent again: {images:?}"
        );
    }

    /// A long stretch on another backend cannot build a request the CLI
    /// will refuse. The newest survive — the pending message's above all,
    /// since that is what is being asked about — and the preamble still
    /// names the rest.
    #[test]
    fn a_catch_up_carrying_too_many_pictures_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let mut s =
            Session::create(&dir.path().join("s.eid"), "fau:gemini", dir.path(), None).unwrap();
        // Six images of 4 MB apiece, against a 16 MB budget.
        let big = "A".repeat(4 * 1024 * 1024 / 3 * 4);
        for i in 0..5 {
            s.append(RecordKind::UserMessage(Message::user(vec![
                ContentBlock::image("image/png", &big, Some(format!("old{i}.png"))),
            ])))
            .unwrap();
            s.append(RecordKind::AssistantMessage(Message::assistant(vec![
                ContentBlock::text("seen"),
            ])))
            .unwrap();
        }
        s.append(RecordKind::UserMessage(Message::user(vec![
            ContentBlock::image("image/png", &big, Some("newest.png".into())),
            ContentBlock::text("and this one?"),
        ])))
        .unwrap();
        let (_, _, images) = turn_input(&s).unwrap();
        assert!(images.len() < 6, "nothing was trimmed: {}", images.len());
        let last = images.last().unwrap();
        assert!(
            matches!(last, ContentBlock::Image { alt, .. } if alt.as_deref() == Some("newest.png")),
            "the pending message's own picture was trimmed: {last:?}"
        );
    }

    #[test]
    fn a_branch_the_cli_never_ran_reaches_it_as_a_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(
            &dir.path().join("s.eid"),
            "fau:some-model",
            dir.path(),
            None,
        )
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text(
            "make me a flappy bird game",
        )))
        .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("Here it is."),
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "write".into(),
                input: Json(r#"{"path":"flappy.html"}"#.into()),
            },
        ])))
        .unwrap();
        s.append(RecordKind::ToolResult {
            tool_use_id: "t1".into(),
            content: "created /w/flappy.html (5279 bytes)".into(),
            is_error: false,
        })
        .unwrap();
        settled(&mut s);
        s.append(RecordKind::ModelChanged {
            model: "claude-cli:sonnet".into(),
        })
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text(
            "did you see the file it made?",
        )))
        .unwrap();

        let (resume, prompt, _) = turn_input(&s).unwrap();
        assert_eq!(
            resume, None,
            "the CLI has no session on this branch to resume"
        );
        assert!(prompt.contains("make me a flappy bird game"), "{prompt}");
        assert!(prompt.contains("called write"), "{prompt}");
        assert!(prompt.contains("created /w/flappy.html"), "{prompt}");
        assert!(
            prompt.ends_with("did you see the file it made?"),
            "the real question comes last:\n{prompt}"
        );
        assert_eq!(
            prompt.matches("did you see the file it made?").count(),
            1,
            "asked once, not twice"
        );
    }

    /// Once it has answered, its own session holds the branch: a second
    /// question is sent bare, on `--resume`.
    #[test]
    fn what_the_cli_already_heard_is_not_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(
            &dir.path().join("s.eid"),
            "claude-cli:sonnet",
            dir.path(),
            None,
        )
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("salve")))
            .unwrap();
        s.append(RecordKind::BackendSession {
            backend: BACKEND.into(),
            id: "sess-1".into(),
        })
        .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("salve mundi"),
        ])))
        .unwrap();
        settled(&mut s);
        // The end-of-turn mark: everything above is in the CLI's session.
        s.append(RecordKind::BackendSession {
            backend: BACKEND.into(),
            id: "sess-1".into(),
        })
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("and again")))
            .unwrap();

        let (resume, prompt, _) = turn_input(&s).unwrap();
        assert_eq!(resume.as_deref(), Some("sess-1"));
        assert_eq!(
            prompt, "and again",
            "resume covers the history; nothing is replayed"
        );
    }

    /// `:model` away and back: only the turns that ran elsewhere in between
    /// are replayed, not the CLI's own.
    #[test]
    fn only_the_turns_it_missed_are_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(
            &dir.path().join("s.eid"),
            "claude-cli:sonnet",
            dir.path(),
            None,
        )
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text(
            "first question",
        )))
        .unwrap();
        s.append(RecordKind::BackendSession {
            backend: BACKEND.into(),
            id: "sess-1".into(),
        })
        .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("first answer"),
        ])))
        .unwrap();
        settled(&mut s);
        s.append(RecordKind::BackendSession {
            backend: BACKEND.into(),
            id: "sess-1".into(),
        })
        .unwrap();
        // Away to a provider for a turn…
        s.append(RecordKind::ModelChanged {
            model: "fau:some-model".into(),
        })
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text(
            "second question",
        )))
        .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("second answer"),
        ])))
        .unwrap();
        settled(&mut s);
        // …and back.
        s.append(RecordKind::ModelChanged {
            model: "claude-cli:sonnet".into(),
        })
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text(
            "third question",
        )))
        .unwrap();

        let (resume, prompt, _) = turn_input(&s).unwrap();
        assert_eq!(
            resume.as_deref(),
            Some("sess-1"),
            "the same CLI session is resumed"
        );
        assert!(
            prompt.contains("second question") && prompt.contains("second answer"),
            "{prompt}"
        );
        assert!(
            !prompt.contains("first answer"),
            "its own turn is in its own session:\n{prompt}"
        );
        assert!(prompt.ends_with("third question"), "{prompt}");
    }

    /// The whole point of naming: the branch ran on a provider, so the CLI
    /// is told which one, and a question that refers to it by name lands.
    #[test]
    fn the_transcript_says_which_model_ran_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(
            &dir.path().join("s.eid"),
            "fau:openai/ministral-3:14b",
            dir.path(),
            None,
        )
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text(
            "make me a flappy bird game",
        )))
        .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("done"),
        ])))
        .unwrap();
        settled(&mut s);
        s.append(RecordKind::ModelChanged {
            model: "claude-cli:opus".into(),
        })
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text(
            "look at what mistral just made",
        )))
        .unwrap();

        let (_, prompt, _) = turn_input(&s).unwrap();
        assert!(
            prompt.contains("run by `fau:openai/ministral-3:14b`"),
            "{prompt}"
        );
    }

    /// Only the span the CLI missed is attributed. Its own earlier turn is
    /// already in its own session, so `claude-cli:*` must not be named as
    /// some other model the user was talking to.
    #[test]
    fn its_own_turns_are_not_attributed_to_a_stranger() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(
            &dir.path().join("s.eid"),
            "claude-cli:sonnet",
            dir.path(),
            None,
        )
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("first")))
            .unwrap();
        s.append(RecordKind::BackendSession {
            backend: BACKEND.into(),
            id: "sess-1".into(),
        })
        .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("first answer"),
        ])))
        .unwrap();
        settled(&mut s);
        s.append(RecordKind::BackendSession {
            backend: BACKEND.into(),
            id: "sess-1".into(),
        })
        .unwrap();
        s.append(RecordKind::ModelChanged {
            model: "fau:openai/ministral-3:14b".into(),
        })
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("second")))
            .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("second answer"),
        ])))
        .unwrap();
        settled(&mut s);
        s.append(RecordKind::ModelChanged {
            model: "claude-cli:sonnet".into(),
        })
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("third")))
            .unwrap();

        let (_, prompt, _) = turn_input(&s).unwrap();
        assert!(
            prompt.contains("run by `fau:openai/ministral-3:14b`"),
            "{prompt}"
        );
        assert!(
            !prompt.contains("claude-cli"),
            "its own turn is not someone else's:\n{prompt}"
        );
    }

    /// A `:model` switched away from before it ever answered names nobody.
    #[test]
    fn a_model_that_never_spoke_is_not_named() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(&dir.path().join("s.eid"), "fau:a", dir.path(), None).unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("hi")))
            .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("hello"),
        ])))
        .unwrap();
        settled(&mut s);
        // Two hops with no turn in between: only `fau:a` ever spoke.
        s.append(RecordKind::ModelChanged {
            model: "fau:never-ran".into(),
        })
        .unwrap();
        s.append(RecordKind::ModelChanged {
            model: "claude-cli:sonnet".into(),
        })
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text(
            "what did you say?",
        )))
        .unwrap();

        let (_, prompt, _) = turn_input(&s).unwrap();
        assert!(prompt.contains("run by `fau:a`"), "{prompt}");
        assert!(!prompt.contains("never-ran"), "{prompt}");
    }

    /// Nothing pending means nothing to send — the branch ends on an answer.
    #[test]
    fn a_settled_branch_has_no_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(
            &dir.path().join("s.eid"),
            "claude-cli:sonnet",
            dir.path(),
            None,
        )
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("hi")))
            .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("hello"),
        ])))
        .unwrap();
        settled(&mut s);
        assert!(turn_input(&s).is_none());
    }

    /// A peer message after a settle is the pending prompt for the driver,
    /// framed exactly as the provider loop would frame it. This is what
    /// makes a peer-woken turn on the Claude CLI actually say something.
    #[test]
    fn a_peer_message_is_the_drivers_pending_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(
            &dir.path().join("s.eid"),
            "claude-cli:sonnet",
            dir.path(),
            None,
        )
        .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("hi")))
            .unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("hello"),
        ])))
        .unwrap();
        settled(&mut s);
        s.append(RecordKind::PeerMessage {
            from: "eidolon-9f2c".into(),
            from_cwd: "/tmp/wt".into(),
            channel: None,
            text: "rebased onto master".into(),
        })
        .unwrap();

        let (_, prompt, _) = turn_input(&s).unwrap();
        assert!(prompt.contains("rebased onto master"), "{prompt}");
        assert!(prompt.contains("not the operator speaking"), "{prompt}");
    }

    #[test]
    fn eidolon_mcp_tools_are_recognised() {
        assert!(is_eidolon_tool("mcp__eidolon__read"));
        assert!(is_eidolon_tool("mcp__eidolon__bash"));
        // Another server's tools are not ours, and must still be adjudicated.
        assert!(!is_eidolon_tool("mcp__other__read"));
        assert!(!is_eidolon_tool("mcp__eidolonx__read"));
        assert!(!is_eidolon_tool("Read"));
        assert!(!is_eidolon_tool("mcp__eidolon"));
    }

    #[test]
    fn own_is_the_default() {
        assert_eq!(ToolMode::default(), ToolMode::Own);
        assert_eq!(
            serde_json::from_str::<ToolMode>("\"own\"").unwrap(),
            ToolMode::Own
        );
        assert_eq!(
            serde_json::from_str::<ToolMode>("\"eidolon\"").unwrap(),
            ToolMode::Eidolon
        );
        // `both` was removed; a config naming it should fail loudly rather
        // than silently falling back to a mode the operator did not choose.
        assert!(serde_json::from_str::<ToolMode>("\"both\"").is_err());
    }

    #[test]
    fn vault_guidance_reaches_both_cli_tool_modes_without_duplicate_append_flags() {
        let vault = eidolon_core::persona::CITATION_GUIDANCE;
        for mode in [ToolMode::Own, ToolMode::Eidolon] {
            let args = system_args(mode, Some(vault), Some("tool guidance".into()));
            assert_eq!(args.len(), 2);
            assert_eq!(args[0], "--append-system-prompt");
            assert!(args[1].starts_with(vault));
            assert_eq!(args[1].contains("tool guidance"), mode == ToolMode::Eidolon);
        }
        assert!(system_args(ToolMode::Own, None, Some("tools".into())).is_empty());
        assert!(system_args(ToolMode::Eidolon, None, None).is_empty());
        assert_eq!(system_args(ToolMode::Eidolon, None, Some("tools".into()))[1], "tools");
    }

    fn args_for(mode: ToolMode) -> Vec<String> {
        ClaudeDriver::new(
            "claude",
            None,
            PathBuf::from("/h"),
            PathBuf::from("/w"),
            PathBuf::from("/s"),
            mode,
        )
        .tool_args(Path::new("/run/x.sock"))
    }

    /// `--tools ""` must never be passed without the MCP server that
    /// replaces the built-ins it removes: a model with a tool-shaped prompt
    /// and no tool channel writes calls, and their results, as prose.
    #[test]
    fn emptying_the_builtins_always_comes_with_the_registry() {
        for mode in [ToolMode::Own, ToolMode::Eidolon] {
            let args = args_for(mode);
            let empties_tools = args
                .windows(2)
                .any(|w| w[0] == "--tools" && w[1].is_empty());
            let serves_mcp = args.iter().any(|a| a == "--mcp-config");
            assert_eq!(
                empties_tools, serves_mcp,
                "{mode:?}: the two must always travel together"
            );
        }
    }

    #[test]
    fn own_passes_no_tool_flags_at_all() {
        assert!(args_for(ToolMode::Own).is_empty());
    }

    #[test]
    fn eidolon_mode_locks_the_surface_to_the_registry() {
        let args = args_for(ToolMode::Eidolon);
        assert!(
            args.contains(&"--strict-mcp-config".to_string()),
            "{args:?}"
        );
        let i = args.iter().position(|a| a == "--tools").expect("--tools");
        assert_eq!(args[i + 1], "", "the built-in allowlist must be empty");
        let cfg = args
            .iter()
            .position(|a| a == "--mcp-config")
            .expect("--mcp-config");
        let v: serde_json::Value = serde_json::from_str(&args[cfg + 1]).unwrap();
        assert_eq!(v["mcpServers"]["eidolon"]["args"][0], "mcp");
    }

    #[test]
    fn the_mcp_config_names_our_binary_and_socket() {
        let d = ClaudeDriver::new(
            "claude",
            None,
            PathBuf::from("/usr/bin/eidolon"),
            PathBuf::from("/w"),
            PathBuf::from("/s"),
            ToolMode::Eidolon,
        );
        let v: serde_json::Value =
            serde_json::from_str(&d.mcp_config_json(Path::new("/run/x.sock"))).unwrap();
        assert_eq!(v["mcpServers"]["eidolon"]["command"], "/usr/bin/eidolon");
        assert_eq!(v["mcpServers"]["eidolon"]["args"][0], "mcp");
        assert_eq!(
            v["mcpServers"]["eidolon"]["env"][MCP_SOCKET_ENV],
            "/run/x.sock"
        );
    }
}
