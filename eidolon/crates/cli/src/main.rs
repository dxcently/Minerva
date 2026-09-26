//! `eidolon` — the headless command-line consumer of the core.
//!
//! This binary is the first *consumer* of `eidolon-core`, not the harness
//! itself: it proves the loop, the dispatcher, the session log and the Rune
//! built-ins work with no terminal UI attached. The interactive TUI (a Rune
//! program over a curated Rust rendering vocabulary) is the next consumer
//! and replaces nothing here.
//!
//! Subcommands:
//!
//! - `run [-m MODEL] [--provider mock] PROMPT…` — one turn in a new
//!   session, streaming text to stdout, tools executed through the
//!   dispatcher. Every tool call runs: the harness ships no hook that
//!   refuses or prompts (see `eidolon_core::policy`).
//! - `resume SESSION [PROMPT…]` — reopen a log; finish an unsettled turn,
//!   then run a new one if a prompt is given.
//! - `sessions` — list logs under the sessions directory.
//! - `log SESSION [--json]` — dump a log's records as text (a debugging
//!   aid, kept deliberately behind a subcommand rather than a `cat`-able
//!   format), or as one JSON line per record for a reader that cannot
//!   decode the journal.
//! - `chat [--session S]` — a line-oriented REPL: each line is a turn;
//!   `:model`, `:fork`, `:tree`, `:compact`, `:log`, `:quit`. The `:` line
//!   resolves against the TUI's one command registry, so the names are the
//!   TUI's names and `:help` is generated from the same table; a command
//!   that needs a screen says so. Ctrl-c cancels
//!   the turn in flight and returns to the prompt; at the prompt, twice
//!   within two seconds exits.
//! - `tools` — print the built-in manifests as the model sees them.
//! - `models` — the catalog. The one command that lets a provider script's
//!   `models()` wait on its gateway; a launch answers from what this (or
//!   the last launch's background refresh) fetched.
//! - `tui [--session S]` (the default with no subcommand) — the interactive
//!   terminal UI from `eidolon-tui`; the layout script is
//!   `~/.config/eidolon/ui.rn` if present, else the built-in default. It
//!   opens `--session` or a new log, and moves between the rest from the
//!   inside (`space r`), so this flag is a starting point and not the only
//!   way to reach one.
//! - `web [--session S] [--bind ADDR] [--ui-dir PATH]` — the browser
//!   consumer: the same session and `Dispatcher` as `tui`, served by
//!   `eidolon-web` on loopback HTTP. Boot prints the path of the `0600`
//!   token file, never the token.
//!
//! ## Signals
//!
//! A headless turn is cancelled by SIGINT and SIGTERM alike
//! ([`cancel_on_signals`]): a `timeout(1)` or a supervisor stop leaves the
//! same `Cancelled` record and settled journal a Ctrl-C does.
//!
//! Providers are harnox's; `--provider mock` is a scripted stand-in that
//! needs no credentials.

mod config;
mod chatgpt_login;
mod google_login;
mod persona;
mod system;
mod term;
mod vantage;

use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use eidolon_core::UsageExt;
use eidolon_core::boot;
use eidolon_core::event::Event;
use eidolon_core::policy::{AllowAll, PolicyHook};
use eidolon_core::session::RecordKind;
use eidolon_core::user::tools::ChoicesUser;
use eidolon_core::*;

#[derive(Parser)]
#[command(
    name = "eidolon",
    about = "Headless driver for the Eidolon core",
    version
)]
struct Cli {
    /// Config file (default: ~/.config/eidolon/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Answer every question the gate raises with yes, without asking.
    ///
    /// For pointing a small model at a directory as a general-purpose
    /// tool, where most of what it does is *structurally* unfamiliar
    /// rather than dangerous and every one of those is a question. The
    /// three things the table refuses on the merits — privilege
    /// escalation, powering the box down, destroying a filesystem — still
    /// refuse; `[policy] enabled = false` is the way to have no gate at
    /// all. Every waived call is journaled, so a session run this way is
    /// still a legible list of what nobody was asked about. In the TUI
    /// this is the starting position of a switch `:yolo` flips. `[policy]
    /// yolo = true` in the config is the same answer given once, for
    /// every launch; this flag arms it for one.
    #[arg(long, global = true)]
    yolo: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    Run {
        #[arg(short, long)]
        model: Option<String>,
        /// Provider override; `mock` needs no credentials.
        #[arg(long)]
        provider: Option<String>,
        /// Working directory for tools (default: current).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Attach an image to the turn. Repeat for several.
        #[arg(long = "image", value_name = "PATH")]
        images: Vec<PathBuf>,
        /// Wear a persona from the vault — a note under
        /// `wiki/personalities/`, by its frontmatter `name` or its vault
        /// path. Needs `[mneme]` configured.
        #[arg(long, value_name = "NAME")]
        persona: Option<String>,
        prompt: Vec<String>,
    },
    Resume {
        session: PathBuf,
        /// Fork: continue from this record id instead of the head.
        #[arg(long)]
        at: Option<u64>,
        #[arg(long)]
        provider: Option<String>,
        /// Run the reopened session's tools here rather than in the
        /// directory its start record names — the reopen-time override a
        /// session needs when its birth directory is on another machine
        /// or is gone. To land a shipped session on this machine first,
        /// `eidolon import`.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Attach an image to the turn. Repeat for several.
        #[arg(long = "image", value_name = "PATH")]
        images: Vec<PathBuf>,
        prompt: Vec<String>,
    },
    Chat {
        /// Reopen this session instead of starting a new one.
        #[arg(long)]
        session: Option<PathBuf>,
        #[arg(short, long)]
        model: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Wear a persona from the vault — a note under
        /// `wiki/personalities/`, by its frontmatter `name` or its vault
        /// path. Needs `[mneme]` configured.
        #[arg(long, value_name = "NAME")]
        persona: Option<String>,
    },
    Sessions,
    /// The other Eidolon sessions running right now.
    Peers,
    /// Send a message into a running session from outside it — through the
    /// swarm's inbox, not the keyboard, so an idle session that listens on
    /// its doorbell can be woken and a busy one is handed the message at
    /// its next safe point.
    ///
    /// The recipient's model is told the message came from an external
    /// tool: never as its operator, never as a fellow session. That is
    /// what makes this door safe to hand to an automation — Aoide is the
    /// expected first customer.
    Send {
        /// The sender's identity, as the recipient's model will be told —
        /// every integrating tool passes its own (Aoide passes `aoide`),
        /// which is what lets a session tell its tools apart.
        #[arg(long, value_name = "NAME")]
        from: Option<String>,
        /// Wake the recipient even on a channel fan-out — an idle session
        /// starts a turn, one mid-turn is steered at its next safe point.
        #[arg(long)]
        wake: bool,
        /// Leave the message for the recipient's next turn boundary rather
        /// than steering a busy session or waking an idle one.
        #[arg(long)]
        no_wake: bool,
        /// Resolve `channel` against this directory rather than the current one.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// A session id — a unique prefix is enough — or `channel` for
        /// every session on this project.
        to: String,
        /// The message. A single `-` reads it from stdin.
        text: Vec<String>,
    },
    /// The doors into and out of a running session: who is there, sending
    /// one a message, and asking one to finish.
    ///
    /// `list` and `send` are `eidolon peers` and `eidolon send` under one
    /// verb, so the three things one session can do to another are found
    /// together; `quiesce` is the settled close of a session hop. None of
    /// them registers a session of its own — these are questions asked *of*
    /// the swarm, not members of it.
    Door {
        #[command(subcommand)]
        op: DoorOp,
    },
    /// Add a git worktree — a second checkout of this repository, so a
    /// second session can work without editing the same files.
    Worktree {
        /// Branch to check out. Created from the current HEAD if it does
        /// not exist yet.
        branch: String,
        /// Where to put it. Default: a sibling of the repository, named
        /// `<repo>-<branch>`.
        path: Option<PathBuf>,
    },
    Log {
        session: PathBuf,
        /// Print the records as one JSON line each — `trace_line`, the
        /// record's own serde form — instead of the text rendering: the
        /// export path for a reader that cannot decode the journal.
        #[arg(long)]
        json: bool,
        /// Emit only records with an id greater than this one — a poller's
        /// cursor, holding the last id it saw, so each poll costs the new
        /// records alone rather than the whole journal again. Ids are the
        /// `#n` the text rendering prints, dense and monotonic from zero,
        /// and an id at or past the head simply reads as "nothing new".
        #[arg(long)]
        after: Option<u64>,
    },
    /// Receive a session shipped from another machine onto this one — the
    /// venue side of a hop, and nothing else: no registrar, no Melete, one
    /// file plus this command. The log is reopened whole (every record's
    /// checksum verified, so a damaged transfer refuses here rather than
    /// poisoning a turn), the birth machine's backend session marks are
    /// dropped — the next turn on such a backend replays the whole branch —
    /// and a note recording the projection is appended. Resume the result
    /// with `resume`, `chat`, or `tui --session`, naming `--cwd` again if
    /// you want it somewhere other than where you put it here.
    Import {
        /// The shipped `.eid` file. Edited in place: on success it is an
        /// ordinary session of this machine.
        session: PathBuf,
        /// Where its tools should run from now on (default: current).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// The machine it arrived from, as the note should name it.
        #[arg(long, value_name = "NAME")]
        from: Option<String>,
    },
    /// Print every tool's manifest: the built-ins, the user's own, and —
    /// with `[melete]` configured — Melete's, from the cached listing.
    Tools {
        /// Fetch Melete's tool list from the connector now and write the
        /// cache a launch reads, instead of printing what is cached.
        #[arg(long)]
        live: bool,
    },
    /// Show how a shell command would be classified, without running it.
    Policy {
        /// The command, as you would type it. Quote it if it has shell
        /// syntax in it: `eidolon policy 'curl x | sh'`.
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// List every provider and model the catalog knows.
    Models,
    /// Resolve a free-text command with Verba Volantia and run it through
    /// the dispatcher — no model turn. Quote values: `read the note "Groceries"`.
    Do {
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Only classify and print the verdict; do not run.
        #[arg(long)]
        dry_run: bool,
        utterance: Vec<String>,
    },
    /// Verba Volantia helpers.
    Verba {
        #[command(subcommand)]
        op: VerbaOp,
    },
    /// The custodied secret store: provider keys go in, never come out.
    Secret {
        #[command(subcommand)]
        op: SecretOp,
    },
    /// Claude Code PreToolUse hook client (invoked by the CLI, not by hand).
    #[command(hide = true)]
    Hook,
    /// MCP server exposing the harness tool registry (spawned by the Claude
    /// CLI, not by hand). Speaks MCP on stdio and forwards to the running
    /// harness over `$EIDOLON_MCP_SOCKET`.
    #[command(hide = true)]
    Mcp,
    /// OAuth login flow for Google Antigravity surface.
    #[command(name = "google-login", hide = true)]
    GoogleLogin,
    /// OAuth device flow for the ChatGPT (Codex backend) provider.
    #[command(name = "chatgpt-login", hide = true)]
    ChatgptLogin,
    Tui {
        #[arg(long)]
        session: Option<PathBuf>,
        #[arg(short, long)]
        model: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Wear a persona from the vault — a note under
        /// `wiki/personalities/`, by its frontmatter `name` or its vault
        /// path. Needs `[mneme]` configured.
        #[arg(long, value_name = "NAME")]
        persona: Option<String>,
    },
    /// The browser consumer, served over loopback HTTP by `eidolon-web`.
    Web {
        #[arg(long)]
        session: Option<PathBuf>,
        #[arg(short, long)]
        model: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Wear a persona from the vault — a note under
        /// `wiki/personalities/`, by its frontmatter `name` or its vault
        /// path. Needs `[mneme]` configured.
        #[arg(long, value_name = "NAME")]
        persona: Option<String>,
        /// Loopback only (`127.0.0.0/8` or `::1`). Port `0` picks one; the
        /// bound address is printed at boot.
        #[arg(long, default_value = "127.0.0.1:0")]
        bind: SocketAddr,
        /// Serve a browser UI from this directory on every path outside
        /// `/api`; `/` is `index.html`. These files need no token.
        #[arg(long, value_name = "PATH")]
        ui_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum VerbaOp {
    /// Print a templates.json skeleton for the harness's own tools, for
    /// `verba-volantia gen`.
    Spec,
    /// Classify an utterance with every configured bundle and print the verdicts.
    Classify { utterance: Vec<String> },
}

#[derive(Subcommand)]
enum DoorOp {
    /// The other Eidolon sessions running right now — the same roster as
    /// `eidolon peers`.
    List,
    /// Send a message into a running session from outside it — the same
    /// path as `eidolon send`, whose wording for what becomes of it is
    /// unchanged.
    Send {
        /// The sender's identity, as the recipient's model will be told.
        #[arg(long, value_name = "NAME")]
        from: Option<String>,
        /// Wake the recipient even on a channel fan-out.
        #[arg(long)]
        wake: bool,
        /// Leave the message for the recipient's next turn boundary.
        #[arg(long)]
        no_wake: bool,
        /// Resolve `channel` against this directory rather than the current one.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// A session id — a unique prefix is enough — or `channel`.
        to: String,
        /// The message. A single `-` reads it from stdin.
        text: Vec<String>,
    },
    /// Ask a running session to finish at its next turn boundary, and wait
    /// for it — the birth-side half of a session hop (`eidolon import` is
    /// the venue's).
    ///
    /// Never forced: the request is noted and the session settles where its
    /// turn ends, so a tool call in flight is not interrupted and a busy
    /// session is not cut off. A session that has not settled when the
    /// timeout expires is *refused* — the session goes on unchanged and the
    /// command exits non-zero, so a shipper can tell what happened without
    /// reading prose.
    Quiesce {
        /// Session id — a unique prefix is enough — from `door list`.
        session: String,
        /// Where it is going: named in the marker the session journals and
        /// in the goodbye it says on the way out.
        #[arg(long, value_name = "NAME")]
        destination: Option<String>,
        /// Seconds to wait for the boundary before refusing.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
}

#[derive(Subcommand)]
enum SecretOp {
    /// Store (or rotate) NAME; the value is read from stdin, never argv.
    Set {
        name: String,
        /// Host the secret is for, recorded as metadata.
        #[arg(long)]
        service: Option<String>,
    },
    List,
    Delete {
        name: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The startup budget's clock (`EIDOLON_BOOT_LOG`); see `eidolon_core::boot`.
    boot::mark("main");
    let cli = Cli::parse();
    // `tracing` writes to stderr, and stderr is the terminal the
    // interactive consumer is drawing on: a line from a background task
    // — a remote that will not answer, a retry — lands on top of the
    // frame, and the UI has no way to take it back. So the drawing
    // commands get no subscriber at all unless `EIDOLON_LOG` names a
    // file to put one in; every headless command keeps stderr, where a
    // log line is the point.
    let drawing = matches!(cli.cmd, None | Some(Cmd::Tui { .. }));
    let filter = || tracing_subscriber::EnvFilter::from_default_env();
    match std::env::var_os("EIDOLON_LOG") {
        Some(path) => {
            let file = std::fs::File::create(&path)
                .with_context(|| format!("creating {}", path.to_string_lossy()))?;
            tracing_subscriber::fmt()
                .with_env_filter(filter())
                .with_writer(std::sync::Mutex::new(file))
                .init();
        }
        None if !drawing => {
            tracing_subscriber::fmt()
                .with_env_filter(filter())
                .with_writer(std::io::stderr)
                .init();
        }
        None => {}
    }
    // Both run before the config is loaded: they are bridges to an already
    // running harness, not harness instances of their own.
    if matches!(cli.cmd, Some(Cmd::Hook)) {
        eidolon_claude::hook::run_client();
        return Ok(());
    }
    if matches!(cli.cmd, Some(Cmd::Mcp)) {
        eidolon_claude::mcp::run_client();
        return Ok(());
    }
    let cfg = config::Config::load(cli.config.as_deref())?;
    boot::mark("config");
    // What the binary ships, unpacked beside what the operator wrote —
    // before the loaders, so the launch that upgrades reads what it seeded.
    for note in system::sync(&config::config_dir(), &cfg.tools_dir()) {
        eprintln!("[system] {note}");
    }
    boot::mark("system");

    // The flag arms it for this launch; the config, for every launch.
    let yolo = cli.yolo || cfg.policy_yolo();
    let cmd = cli.cmd.unwrap_or(Cmd::Tui {
        session: None,
        model: None,
        provider: None,
        cwd: None,
        persona: None,
    });
    match cmd {
        Cmd::Hook | Cmd::Mcp => unreachable!("handled above"),
        Cmd::GoogleLogin => google_login::run_google_login().await,
        Cmd::ChatgptLogin => chatgpt_login::run_chatgpt_login().await,
        Cmd::Tui {
            session,
            model,
            provider,
            cwd,
            persona,
        } => {
            // Three Rune contexts are needed before the first frame — the
            // providers', the tools' (built in `build`), the UI script's —
            // and each costs a few milliseconds nothing else depends on. So
            // they build on threads of their own while this thread opens
            // the session and reads the config; whoever needs one first
            // finds it ready or waits for it, never builds a second.
            std::thread::spawn(eidolon_providers::script::warm);
            std::thread::spawn(eidolon_tui::script::warm);
            let (mut s, cwd) = open_or_create(&cfg, session, model.as_deref(), cwd)?;
            if let Some(p) = &persona {
                s.set_persona(p)?;
            }
            boot::mark("session");
            let (ui_tx, ui_rx) = tokio::sync::mpsc::unbounded_channel();
            let user: Arc<dyn UserIo> = Arc::new(eidolon_tui::TuiUser::new(ui_tx.clone()));
            let mut rt = build(&cfg, s, cwd, model.as_deref(), provider.as_deref(), user, yolo).await?;
            let ui_script_path = config::config_dir().join("ui.rn");
            let ui_script = std::fs::read_to_string(&ui_script_path).ok();
            let opts = eidolon_tui::TuiOptions {
                ui_script,
                // Kept for `reload_ui`, which reads it again in place.
                ui_script_path: Some(ui_script_path),
                catalog: Some(rt.catalog.clone()),
                cwd: rt.cwd.clone(),
                scratch: cfg.sessions_dir().join("claude-cli"),
                sessions_dir: cfg.sessions_dir(),
                // Dispatch mode's surfaces. Empty unless `[[verba.bundles]]`
                // says otherwise, and the mode says so rather than opening.
                palette: Some(Arc::new(cfg.palette())),
                swarm: rt.swarm.clone(),
                doorbell: rt.doorbell.take(),
                // What the terminal is asked to do with an attached
                // image. Resolved here, once, where the config is read.
                inline_images: cfg.inline_images(),
                // `[transcript] fold`, for when a finished run of calls
                // folds. Read once, where the config is read.
                fold: cfg.fold(),
                // The switch `:yolo` throws, already armed if `--yolo`
                // was passed. `None` when there is no gate.
                yolo: rt.yolo.clone(),
                // `[terminal]`, for the `T` chords. Passed as declared —
                // which emulator and which editor this machine has is
                // worked out on the first press, off the boot path.
                launcher: cfg.launcher(),
            };
            eidolon_tui::run(rt.agent.clone(), ui_tx, ui_rx, opts).await
        }
        Cmd::Web {
            session,
            model,
            provider,
            cwd,
            persona,
            bind,
            ui_dir,
        } => {
            // No UI script to warm, unlike `tui`.
            std::thread::spawn(eidolon_providers::script::warm);
            let (mut s, cwd) = open_or_create(&cfg, session, model.as_deref(), cwd)?;
            if let Some(p) = &persona {
                s.set_persona(p)?;
            }
            boot::mark("session");
            let web_user = Arc::new(eidolon_web::WebUser::new());
            let user: Arc<dyn UserIo> = web_user.clone();
            let mut rt = build(&cfg, s, cwd, model.as_deref(), provider.as_deref(), user, yolo).await?;
            struct Roster(Arc<eidolon_swarm::Presence>);

            impl eidolon_web::driver::Presence for Roster {
                fn set_busy(&self, busy: bool) {
                    self.0.set_busy(busy);
                }
                // `crates/tui/src/app.rs`'s rule for the same field.
                fn title_if_empty(&self, text: &str) {
                    self.0.update(|m| {
                        if m.title.trim().is_empty() {
                            m.title = text.chars().take(72).collect();
                        }
                    });
                }
                fn deregister(&self) {
                    self.0.deregister();
                }
                fn id(&self) -> String {
                    self.0.id().to_string()
                }
            }

            // As in `drive`, so a signal still removes the token file.
            let cancel = CancellationToken::new();
            {
                let cancel = cancel.clone();
                tokio::spawn(cancel_on_signals(cancel));
            }
            eidolon_web::run(
                rt.agent.clone(),
                web_user,
                eidolon_web::WebOptions {
                    bind,
                    ui_dir,
                    cwd: rt.cwd.clone(),
                    runtime_dir: eidolon_swarm::Presence::root(),
                    // No gate at all reads as a closed switch.
                    yolo: rt.yolo.clone().unwrap_or_default(),
                    doorbell: rt.doorbell.take(),
                    presence: rt.swarm.clone().map(|p| Arc::new(Roster(p)) as Arc<dyn eidolon_web::driver::Presence>),
                },
                cancel,
            )
            .await
        }
        Cmd::Run {
            model,
            provider,
            cwd,
            images,
            persona,
            prompt,
        } => {
            let prompt = prompt.join(" ");
            if prompt.trim().is_empty() {
                bail!("give a prompt");
            }
            // Read before the session is created: a path that is not an
            // image should cost a message, not an empty log on disk.
            let images = attachments(&images)?;
            let cwd = match cwd {
                Some(c) => c.canonicalize()?,
                None => std::env::current_dir()?,
            };
            let model = model
                .or_else(|| cfg.default_model.clone())
                .unwrap_or_else(|| default_model(&cfg));
            let mut session = cfg.create_session(&model, &cwd, Some(cfg.system_prompt()))?;
            eprintln!("session: {}", session.path().display());
            // Pinned on the log before the turn, so the record is on the
            // branch the turn reads and a `resume` of this session comes
            // back in the same voice.
            if let Some(p) = &persona {
                session.set_persona(p)?;
            }
            let mut rt = build(
                &cfg,
                session,
                cwd,
                Some(&model),
                provider.as_deref(),
                Arc::new(term::StdinUser),
                yolo,
            )
            .await?;
            report_persona(&rt).await;
            drive(
                &mut rt,
                Some(Said {
                    text: prompt,
                    images,
                }),
            )
            .await
        }
        Cmd::Resume {
            session,
            at,
            provider,
            cwd,
            images,
            prompt,
        } => {
            // Before the log is opened, so a path that is not an image
            // costs a message rather than a half-entered session.
            let images = attachments(&images)?;
            // `--cwd` is the reopen-time override — see `open_with_cwd`.
            let mut s = match cwd {
                Some(c) => {
                    let c = c.canonicalize()?;
                    Session::open_with_cwd(&session, &c)?
                }
                None => Session::open(&session)?,
            };
            if let Some(id) = at {
                // Fork before reading the model, so the branch's own model wins.
                s.fork_at(id)?;
                eprintln!("[forked at #{id}]");
            }
            let cwd = launch_cwd(PathBuf::from(s.cwd().context("session has no start record")?));
            // Resume carries no `--model`: what the branch names is the
            // birth machine's key, so a resolution failure takes the
            // projection fallback, never the refused-request path.
            let mut rt = build(
                &cfg,
                s,
                cwd,
                None,
                provider.as_deref(),
                Arc::new(term::StdinUser),
                yolo,
            )
            .await?;
            let prompt = prompt.join(" ");
            drive(
                &mut rt,
                (!prompt.trim().is_empty()).then_some(Said {
                    text: prompt,
                    images,
                }),
            )
            .await
        }
        Cmd::Chat {
            session,
            model,
            provider,
            cwd,
            persona,
        } => {
            let (mut s, cwd) = open_or_create(&cfg, session, model.as_deref(), cwd)?;
            if let Some(p) = &persona {
                s.set_persona(p)?;
            }
            let mut rt = build(
                &cfg,
                s,
                cwd,
                model.as_deref(),
                provider.as_deref(),
                Arc::new(term::StdinUser),
                yolo,
            )
            .await?;
            report_persona(&rt).await;
            repl(&mut rt, &cfg).await
        }
        // Answered without registering a session to ask with: this is a
        // question about the swarm, not a member of it. It sweeps the
        // registrations of sessions that died without cleaning up, which
        // is the other reason to have it — a scan is what collects them.
        Cmd::Peers => door_list(),
        // An outside caller's door into a running session: the same
        // inbox a peer writes to, but marked as coming from outside any
        // session, so the recipient's model frames it as a tool's input —
        // never as its operator's words and never as a colleague's.
        Cmd::Send {
            from,
            wake,
            no_wake,
            cwd,
            to,
            text,
        } => door_send(from, wake, no_wake, cwd, to, text),
        // The same two, plus the settled close of a hop. `quiesce` leaves
        // the session's own process to notice the request at its next turn
        // boundary; this side only asks and waits.
        Cmd::Door { op } => match op {
            DoorOp::List => door_list(),
            DoorOp::Send {
                from,
                wake,
                no_wake,
                cwd,
                to,
                text,
            } => door_send(from, wake, no_wake, cwd, to, text),
            DoorOp::Quiesce {
                session,
                destination,
                timeout,
            } => door_quiesce(
                &session,
                destination.as_deref().unwrap_or(eidolon_core::quiesce::ANOTHER_MACHINE),
                std::time::Duration::from_secs(timeout),
            ),
        },
        Cmd::Worktree { branch, path } => worktree(&branch, path.as_deref()),
        Cmd::Sessions => {
            let dir = cfg.sessions_dir();
            let mut entries: Vec<_> = std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.filter_map(Result::ok)
                        .filter(|e| e.path().extension().is_some_and(|x| x == "eid"))
                        .collect()
                })
                .unwrap_or_default();
            entries.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
            for e in entries.iter().rev() {
                let p = e.path();
                // Read-only: a listed session may be live mid-append, and a
                // repairing open here would truncate its journal.
                match Session::open_readonly(&p) {
                    Ok(s) => {
                        let model = s.model().unwrap_or_default();
                        let first = s.branch().iter().find_map(|r| match &r.kind {
                            RecordKind::UserMessage(m) => Some(m.text()),
                            _ => None,
                        });
                        let first = first.unwrap_or_default();
                        let first: String = first.chars().take(60).collect();
                        println!(
                            "{}\t{}\t{}\t{}",
                            p.display(),
                            model,
                            s.records().len(),
                            first.replace('\n', " ")
                        );
                    }
                    Err(e) => println!("{}\t(unreadable: {e})", p.display()),
                }
            }
            Ok(())
        }
        Cmd::Log {
            session,
            json,
            after,
        } => {
            // Read-only: `log` may run against a live session, and the
            // repairing open would truncate a mid-append record under the
            // writer. See `SessionLog::open_readonly`.
            let s = Session::open_readonly(&session)?;
            let from = after.map_or(0, |a| a.saturating_add(1) as usize);
            let records: &[_] = s.records().get(from..).unwrap_or(&[]);
            if json {
                use std::io::Write;
                let mut out = std::io::stdout();
                for r in records {
                    out.write_all(eidolon_core::session::trace_line(r)?.as_bytes())?;
                }
                out.flush()?;
                return Ok(());
            }
            for r in records {
                let parent = r
                    .parent
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".into());
                println!("#{} ← {} {}", r.id, parent, term::record_line(r));
            }
            println!("head: {:?}  settled: {}", s.head(), s.is_settled());
            Ok(())
        }
        Cmd::Import {
            session,
            cwd,
            from,
        } => {
            let cwd = match cwd {
                Some(c) => c.canonicalize()?,
                None => std::env::current_dir()?,
            };
            let mut s = Session::open_projected(&session, &cwd, from.as_deref())?;
            // The birth facts stay what they were; the note says what
            // changed. Print both, so the operator sees the whole
            // projection in one screen and `eidolon log` holds it forever.
            let (birth_model, birth) = s
                .start()
                .map(|(m, c, _)| (m.to_string(), c.to_string()))
                .unwrap_or_default();
            println!("{}", s.path().display());
            println!("  model:      {}", if birth_model.is_empty() { "(none)" } else { &birth_model });
            println!("  born at:    {}", if birth.is_empty() { "(none)" } else { &birth });
            println!("  running at: {}", cwd.display());
            // Landing, not just receiving: the branch's model key is the
            // birth machine's decision, and here it may name no provider
            // (`eidolon:…`, Melete's engine prefix). One that fails is
            // journaled as `ModelChanged` onto this machine's default — the
            // record the claude driver already journals when a backend
            // reports what actually runs — so the landed session opens
            // without per-launch compensation. The model walk applies
            // changes forward from the start record, so every earlier turn
            // keeps the birth attribution; only the turns this machine runs
            // take the new key. The birth key stays in the start record.
            let catalog = config::catalog(&cfg, eidolon_providers::Fetch::Cached);
            let key = s
                .model()
                .unwrap_or_else(|| birth_model.clone());
            if !key.is_empty()
                && catalog.resolve_unclaimed(&key, None).is_err()
            {
                let fallback =
                    cfg.default_model.clone().unwrap_or_else(|| default_model(&cfg));
                match catalog.resolve_unclaimed(&fallback, None) {
                    Ok(_) => {
                        s.append(RecordKind::ModelChanged { model: fallback.clone() })?;
                        println!(
                            "  lands on:   `{fallback}` — `{key}` names no provider here"
                        );
                    }
                    Err(e) => println!(
                        "  lands on:   nothing yet — `{key}` names no provider here and the default does not either ({e:#})"
                    ),
                }
            }
            // What the first open will do, said here rather than discovered
            // there: a settled branch opens clean, a reply standing without
            // its boundary opens as it stands, anything owed is finished
            // before the first prompt is read.
            println!(
                "  settle:     {}",
                if s.is_settled() {
                    "settled".to_string()
                } else if s.needs_finishing() {
                    "unfinished — the next open finishes the owed work first".to_string()
                } else {
                    "the last reply stands without a boundary; opens as it stands".to_string()
                }
            );
            if let Some(text) = s.records().iter().rev().find_map(|r| match &r.kind {
                RecordKind::ExternalMessage { text, .. } => Some(text),
                _ => None,
            }) {
                println!("  told model: {text}");
            } else if !s.unanswered_tool_uses().is_empty() {
                println!(
                    "  told model: nothing yet — the branch owes results, so the landing brief would poison the replay; the finishing turn runs first"
                );
            }
            if let Some(text) = s.records().iter().rev().find_map(|r| match &r.kind {
                RecordKind::Note { text } => Some(text),
                _ => None,
            }) {
                println!("  note:       {text}");
            }
            Ok(())
        }
        Cmd::Secret { op } => {
            let store = config::secret_store(&cfg);
            match op {
                SecretOp::Set { name, service } => {
                    let v = if std::io::stdin().is_terminal() {
                        eprint!("value for `{name}` (input hidden, enter to finish): ");
                        use std::io::Write;
                        std::io::stderr().flush().ok();
                        term::read_hidden_line()?
                    } else {
                        use std::io::Read;
                        let mut v = String::new();
                        std::io::stdin().read_to_string(&mut v)?;
                        v
                    };
                    let msg = store.set(&name, v.trim(), service, Vec::new())?;
                    println!("{msg}");
                }
                SecretOp::List => {
                    let metas = store.list()?;
                    if metas.is_empty() {
                        println!("(no secrets)");
                    }
                    for m in metas {
                        println!(
                            "{}\t{:?}\t{}",
                            m.name,
                            m.kind,
                            m.service.unwrap_or_default()
                        );
                    }
                }
                SecretOp::Delete { name } => println!("{}", store.delete(&name)?),
            }
            Ok(())
        }
        Cmd::Do {
            cwd,
            dry_run,
            utterance,
        } => {
            let text = utterance.join(" ");
            if text.trim().is_empty() {
                bail!("give an utterance");
            }
            let palette = cfg.palette();
            if palette.is_empty() {
                bail!("no [[verba.bundles]] configured");
            }
            let (res, abstained) = palette.resolve(&text).await;
            for (b, v) in &abstained {
                eprintln!(
                    "[{b}] abstained: intent={} p={:.2} margin={:.2} accept={:?}",
                    v.intent, v.intent_prob, v.margin, v.accept
                );
            }
            let Some(res) = res else {
                eprintln!("no bundle accepted the utterance; send it to a model instead");
                std::process::exit(3);
            };
            eprintln!(
                "[{}] {} p={:.2} slots={}",
                res.bundle,
                res.verdict.intent,
                res.verdict.intent_prob,
                serde_json::to_string(&res.verdict.slots)?
            );
            if dry_run {
                println!("{}", serde_json::to_string_pretty(&res.call)?);
                return Ok(());
            }
            let cwd = match cwd {
                Some(c) => c.canonicalize()?,
                None => std::env::current_dir()?,
            };
            let session = cfg.create_session("mock", &cwd, None)?;
            let rt = build(
                &cfg,
                session,
                cwd,
                // Named by this very command, so a resolution failure is a
                // refused request, not a projection landing.
                Some("mock"),
                Some("mock"),
                Arc::new(term::StdinUser),
                yolo,
            )
            .await?;
            let out = rt
                .agent
                .dispatcher()
                .dispatch_user(res.call, &text, CancellationToken::new())
                .await;
            if out.is_error {
                eprintln!("[error] {}", out.content);
                std::process::exit(1);
            }
            println!("{}", out.content);
            Ok(())
        }
        Cmd::Verba { op } => match op {
            VerbaOp::Spec => {
                let host = match cfg.mneme()? {
                    Some(m) => eidolon_rune::Host::with_mneme(std::env::current_dir()?, m),
                    None => eidolon_rune::Host::new(std::env::current_dir()?),
                };
                // The templates are per *registered tool*, so this surface has
                // to be the session's: a key means `search` is one of them.
                if let Some(key) = config::search_key(&cfg) {
                    host.attach_search_key(key);
                }
                host.attach_secrets(std::sync::Arc::new(config::secret_store(&cfg)));
                let mut reg = ToolRegistry::new();
                for note in eidolon_rune::register_builtins(&mut reg, &host, &cfg.tools_dir())? {
                    eprintln!("[tool] {note}");
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&eidolon_verba::spec::templates_skeleton(
                        &reg.manifests()
                    ))?
                );
                Ok(())
            }
            VerbaOp::Classify { utterance } => {
                let text = utterance.join(" ");
                let palette = cfg.palette();
                if palette.is_empty() {
                    bail!("no [[verba.bundles]] configured");
                }
                // Every bundle, one at a time, which is what this command
                // says it does: `resolve` stops at the first that accepts
                // and so cannot report what the rest made of the line.
                let names: Vec<String> = palette.names().into_iter().map(str::to_string).collect();
                for name in names {
                    match palette.resolve_on(&name, &text).await {
                        Ok((res, v)) => {
                            let verdict = if res.is_some() { "accept" } else { "abstain" };
                            println!("{name}\t{verdict}\t{}", serde_json::to_string(&v)?);
                        }
                        Err(e) => println!("{name}\terror\t{e:#}"),
                    }
                }
                Ok(())
            }
        },
        Cmd::Models => {
            // The listing is the one place a script's `models()` waits on
            // its gateway: this command exists to say what is served now,
            // and what it fetches is what the next launch answers from.
            let cat = config::catalog(&cfg, eidolon_providers::Fetch::Live);
            for p in cat.providers() {
                println!(
                    "{}\t{:?}\t{}\t({})",
                    p.def.name, p.def.wire, p.def.base_url, p.source
                );
            }
            if cat.has_claude() {
                println!("claude-cli\tClaude Code CLI");
            }
            println!();
            // Heaviest first, as the picker shows them; the column is the
            // operator's ordering and is blank where they have said nothing.
            for m in cat.models() {
                let w = if m.weight == 0 {
                    String::new()
                } else {
                    format!("{:+}", m.weight)
                };
                println!("{:<40} {:<40} {:>3}  {}", m.key, m.name, w, m.describe());
            }
            Ok(())
        }
        Cmd::Policy { command } => {
            let text = command.join(" ");
            if text.trim().is_empty() {
                anyhow::bail!("give me a command to classify");
            }
            let cwd = std::env::current_dir()?;
            let host = eidolon_rune::Host::new(cwd.clone());
            let settings = cfg.policy_settings().unwrap_or_default();
            let (policy, note) =
                eidolon_rune::policy::ScriptPolicy::load(&cfg.policy_path(), &host, settings)?;
            println!("table:   {}", note.unwrap_or_else(|| "built in".into()));
            println!("cwd:     {}", cwd.display());
            let v = policy.explain(&text, &cwd);
            println!("command: {text}");
            println!(
                "tier:    {:?}{}",
                v.decision,
                if v.structural {
                    " (shape, not merits)"
                } else {
                    ""
                }
            );
            println!("reason:  {}", v.reason);
            println!("reads:   {}", v.read_only);
            // What the tier means here is the part worth printing: a `flag`
            // that becomes a question and a `deny` that becomes one are two
            // different answers with the same name.
            let asks = match (yolo, cfg.policy_judge()) {
                // Yolo answers it below the judge, so with the switch armed
                // the judge is not in this sentence at all — printing it
                // would be describing a call that is never made.
                (true, _) => "runs, unasked (yolo)".into(),
                // A question with a judge configured is a question the
                // operator may never see, and printing "asks you first"
                // would be describing a session this machine does not run.
                (false, Some((model, _))) => format!("asks {model}, then you if it is unsure"),
                (false, None) => "asks you first".to_string(),
            };
            let outcome = match (v.decision, v.structural) {
                (eidolon_rune::policy::Decision::Allow, _) => "runs, silently".into(),
                (eidolon_rune::policy::Decision::Deny, false) => "refused".into(),
                _ => asks,
            };
            println!("harness: {outcome}");
            // `enabled = false` is worth saying out loud here: otherwise this
            // prints a verdict for a gate that is not running.
            if cfg.policy_settings().is_none() {
                println!("\nNOTE: [policy] enabled = false — nothing is gated in a real session.");
            }
            Ok(())
        }
        Cmd::Tools { live } => {
            let host = eidolon_rune::Host::new(std::env::current_dir()?);
            // The surface a session would have, which is the point of this
            // listing: `search` exists only on a session with a key, so a
            // diagnostic that did not read one would hide the tool the
            // operator just stored a key for. The store likewise: it is what a
            // script's `api_request` resolves a named secret against, and a tool
            // naming one would otherwise be listed as though it could not.
            if let Some(key) = config::search_key(&cfg) {
                host.attach_search_key(key);
            }
            host.attach_secrets(std::sync::Arc::new(config::secret_store(&cfg)));
            let mut reg = ToolRegistry::new();
            let mut notes = eidolon_rune::register_builtins(&mut reg, &host, &cfg.tools_dir())?;
            notes.extend(eidolon_rune::register_dir(
                &mut reg,
                &host,
                &cfg.tools_dir(),
            ));
            for note in notes {
                eprintln!("[tool] {note}");
            }
            // Shown even without a vault configured, for reference. One
            // probe host for both, since a host carries the Rune context.
            let probe = eidolon_rune::Host::with_mneme(
                std::env::current_dir()?,
                eidolon_remote::Mneme::new(
                    "http://127.0.0.1:1/mcp",
                    eidolon_remote::Credential::TokenFile("/dev/null".into()),
                ),
            );
            for (name, shipped) in eidolon_rune::MNEME_BUILTINS {
                // From the seeded copy, as a session with a vault would.
                let (src, _) = eidolon_rune::builtin_source(&cfg.tools_dir(), name, shipped);
                reg.register(Arc::new(eidolon_rune::ScriptTool::compile(
                    name,
                    &src,
                    probe.clone(),
                )?));
            }
            // The search tool a session always has, and the deferred
            // tools it reaches: Melete's listing, from the cache a launch
            // reads — or, with `--live`, from the connector, which is the
            // one command that waits on it and is how the cache is primed.
            reg.register(Arc::new(eidolon_core::ToolSearch::new()));
            // The park, shown for the same reason the vault channel above is:
            // this command is the reference for the surface a session has,
            // and a tool a session would carry that this listing did not
            // would make the listing a lie. Its registry is a fresh one —
            // nothing is armed from `eidolon tools`, only printed.
            reg.register(Arc::new(eidolon_core::wait::WaitFor::new(
                Arc::new(eidolon_core::wait::Parks::new()),
                eidolon_core::wait::DEFAULT_TIMEOUT,
                eidolon_core::wait::MAX_TIMEOUT,
            )));
            // The vault command channel, shown for the same reason the vault
            // builtins above are: this command is the reference for the
            // surface a session has, and a tool a session would carry but
            // this listing did not would make the listing a lie. It is
            // *unattached* here — nothing is dispatched from `eidolon tools`
            // — which is exactly what its manifest needs to be printed. In
            // the inline seam there is no tool to print at all: the
            // convention is a prompt splice, so the listing says that
            // instead of showing a tool no session has.
            if let Some(bundle) = command_bundle(&cfg)? {
                match cfg.verba_entry() {
                    eidolon_verba::Entry::Tool => {
                        reg.register(Arc::new(eidolon_verba::VaultCommands::new(
                            cfg.palette(),
                            bundle,
                        )));
                    }
                    eidolon_verba::Entry::Inline => eprintln!(
                        "[verba] the vault command channel is inline here (`! <command>` lines in the model's reply, answered in place): it has no tool to list"
                    ),
                }
            }
            match cfg.melete()? {
                Some((client, opts)) => {
                    let client = Arc::new(client);
                    let path = eidolon_remote::melete::cache::path();
                    if live {
                        let Some(path) = path.as_deref() else {
                            bail!("no cache directory to write Melete's tool list into")
                        };
                        let listing = eidolon_remote::melete::refresh(&client, path).await?;
                        eprintln!(
                            "[melete] {} tools listed; cache written to {}",
                            listing.len(),
                            path.display()
                        );
                        eidolon_remote::melete::register_listing(
                            &mut reg, &client, &listing, &opts,
                        );
                    } else {
                        for note in eidolon_remote::melete::register(
                            &mut reg,
                            client,
                            &opts,
                            path.as_deref(),
                        ) {
                            eprintln!("[melete] {note}");
                        }
                    }
                }
                None if live => eprintln!("[melete] --live needs [melete] in config.toml"),
                None => {}
            }
            for m in reg.manifests() {
                println!("{}", serde_json::to_string_pretty(&m)?);
            }
            Ok(())
        }
    }
}

struct Runtime {
    agent: Arc<Agent>,
    bus: EventBus,
    catalog: Arc<eidolon_providers::Catalog>,
    cwd: PathBuf,
    /// This session's registration among its peers, when the swarm is on.
    swarm: Option<Arc<eidolon_swarm::Presence>>,
    /// The blanket-yes switch, when there is a gate for it to answer for.
    /// `None` when the classifier is off — there is nothing to bypass, and
    /// a switch that claimed otherwise would be a badge saying the gate
    /// was blind in a session that never had one.
    yolo: Option<eidolon_core::yolo::Switch>,
    /// Rings when a peer has left a message. The payload is already in
    /// the inbox; this only says to go and look, so an idle session need
    /// not poll and a busy one can ignore it until its next boundary.
    doorbell: Option<tokio::sync::mpsc::UnboundedReceiver<()>>,
    /// Ends the socket listener. Held rather than used: the task is meant
    /// to live exactly as long as the process.
    _swarm_task: Option<tokio::task::JoinHandle<()>>,
}

/// The end-of-run token summary.
///
/// Two different numbers, named as such: the context is the last turn's
/// input (what the model had to read, and what the window fills with), the
/// input total is what the whole session was billed for. Reporting only the
/// latter — or only `input_tokens` — is what made a 205k-context turn look
/// like twelve tokens.
/// What a finished turn says on the way out: the tokens, and — when the
/// turn did not end the way a turn is meant to — why it stopped.
///
/// Headless `run` printed the outcome's `Debug`, which carried the reason
/// but buried it in a struct dump; `chat` destructured it away entirely. A
/// response cut off at its cap looks exactly like a finished one, so both
/// now say it in the same words as the TUI.
fn report(rt: &Runtime, out: &TurnOutcome) {
    match out {
        TurnOutcome::Settled {
            stop_reason,
            usage,
            calls,
        } => {
            let mut line = format!(
                "\n[in {} out {} · {} call{}",
                eidolon_core::usage::human(usage.total_input()),
                eidolon_core::usage::human(usage.output_tokens),
                calls,
                if *calls == 1 { "" } else { "s" }
            );
            // Priced from the definition, when it declares rates; a model
            // that declares none prints no figure rather than `$0.00`.
            if let Some(cost) = rt.catalog.cost(&rt.agent.model_key()) {
                line.push_str(&format!(
                    " · {}",
                    eidolon_core::usage::dollars(cost.price(usage))
                ));
            }
            eprintln!("{line}]");
            if let Some(note) = eidolon_core::usage::stop_note(*stop_reason) {
                eprintln!("[{note}]");
            }
        }
        // The same shape as a settle's line, said in the same voice as the
        // loop's `Event::Error` — which the event printer may already have
        // printed in full, so this stays to the point: what it spent, and
        // that the branch is still continuable.
        TurnOutcome::Exhausted { usage, calls } => {
            eprintln!(
                "\n[in {} out {} · {} call{} — stopped at the iteration limit; a follow-up \
message continues from where it stopped]",
                eidolon_core::usage::human(usage.total_input()),
                eidolon_core::usage::human(usage.output_tokens),
                calls,
                if *calls == 1 { "" } else { "s" }
            );
        }
        o => eprintln!("\n[{o:?}]"),
    }
}

fn usage_summary(rt: &Runtime, session: &Session) -> String {
    let total = session.usage();
    let context = session.last_input_tokens();
    let limit = rt.catalog.windows().get(&rt.agent.model_key()).copied();
    let mut s = format!(
        "context {} · session in {} out {}",
        eidolon_core::usage::context_line(context, limit),
        eidolon_core::usage::human(total.total_input()),
        eidolon_core::usage::human(total.output_tokens),
    );
    if total.cached_input() > 0 {
        s.push_str(&format!(
            " ({} cached)",
            eidolon_core::usage::human(total.cached_input())
        ));
    }
    // Each turn at the rates of the model it ran on, since `:model` moves
    // a session between providers mid-branch. Turns on an unpriced model
    // are left out and said so, rather than summed in as free.
    if let Some((spend, unpriced)) = session_spend(rt, session) {
        s.push_str(&format!(" · {}", eidolon_core::usage::dollars(spend)));
        if unpriced > 0 {
            s.push_str(&format!(
                " (+{unpriced} unpriced turn{})",
                if unpriced == 1 { "" } else { "s" }
            ));
        }
    }
    s
}

/// What the branch cost in dollars, and how many of its turns could not
/// be priced. `None` when none could — a session entirely on the Claude
/// CLI has a bill, but not one this harness can compute.
fn session_spend(rt: &Runtime, session: &Session) -> Option<(f64, usize)> {
    let mut model: Option<String> = None;
    let mut spend = 0.0;
    let mut priced = 0usize;
    let mut unpriced = 0usize;
    for r in session.branch() {
        match &r.kind {
            RecordKind::SessionStart { model: m, .. } | RecordKind::ModelChanged { model: m } => {
                model = Some(m.clone())
            }
            RecordKind::TurnSettled { usage, .. } => {
                match model.as_deref().and_then(|m| rt.catalog.cost(m)) {
                    Some(c) => {
                        spend += c.price(usage);
                        priced += 1;
                    }
                    None => unpriced += 1,
                }
            }
            _ => {}
        }
    }
    (priced > 0).then_some((spend, unpriced))
}

fn cfg_scratch() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("eidolon")
        .join("sessions")
        .join("claude-cli")
}

/// With the Claude CLI configured and no `default_model`, the CLI's own
/// default (`sonnet`); otherwise the mock.
fn default_model(cfg: &config::Config) -> String {
    if cfg.claude_cli.is_some() {
        "sonnet".into()
    } else {
        "mock".into()
    }
}

/// Reopen `session` or create a fresh one in `cwd`.
///
/// `model` only matters for the create arm — the fresh log's start record
/// pins it. A reopen's model rides to `build` separately, so the launch can
/// tell an operator-named key from one the journal carried.
fn open_or_create(
    cfg: &config::Config,
    session: Option<PathBuf>,
    model: Option<&str>,
    cwd: Option<PathBuf>,
) -> anyhow::Result<(Session, PathBuf)> {
    match session {
        Some(p) => {
            // `--cwd` on a reopen is the reopen-time cwd override (see
            // `Session::open_with_cwd`): the tools run where the caller
            // said, while the start record keeps the directory the session
            // was born in. Without it the log's own directory stands,
            // exactly as before — unless that directory does not exist on
            // this machine, which is every session that hopped here: the
            // launch directory stands in, stated.
            let s = match cwd {
                Some(c) => {
                    let c = c.canonicalize()?;
                    Session::open_with_cwd(&p, &c)?
                }
                None => Session::open(&p)?,
            };
            let cwd = launch_cwd(PathBuf::from(s.cwd().context("session has no start record")?));
            Ok((s, cwd))
        }
        None => {
            let cwd = match cwd {
                Some(c) => c.canonicalize()?,
                None => std::env::current_dir()?,
            };
            let model = model
                .map(str::to_string)
                .or_else(|| cfg.default_model.clone())
                .unwrap_or_else(|| default_model(cfg));
            let s = cfg.create_session(&model, &cwd, Some(cfg.system_prompt()))?;
            eprintln!("session: {}", s.path().display());
            Ok((s, cwd))
        }
    }
}

/// The directory a reopened session's tools run in: the one the log
/// carries, unless it does not exist on this machine — the birth cwd of a
/// session projected from elsewhere would fail every spawn. The launch
/// directory stands in, stated on stderr, and `--cwd` still overrides.
fn launch_cwd(logged: PathBuf) -> PathBuf {
    if logged.is_dir() {
        return logged;
    }
    match std::env::current_dir() {
        Ok(here) => {
            eprintln!(
                "[cwd] {} does not exist on this machine; tools run from {} (--cwd overrides)",
                logged.display(),
                here.display()
            );
            here
        }
        // Nowhere to stand in — hand the logged path on and let the spawn
        // failure name it, as before.
        Err(_) => logged,
    }
}

/// Which bundle the vault command channel resolves against, if this session
/// should have one at all: a `[[verba.bundles]]` entry aimed at
/// `mneme_rpc` **and** a `[mneme]` stanza to reach.
///
/// Both halves are load-bearing. The channel's whole promise is that the
/// model writes one line and the loop serialises it into an ordinary
/// `mneme_rpc` dispatch, so without a vault behind it every command would be
/// a guaranteed error line; and without a bundle there is no classifier to
/// ask. No bundles is the same answer the CLI's `do` and the TUI's dispatch
/// mode give: say so rather than guess.
fn command_bundle(cfg: &config::Config) -> anyhow::Result<Option<String>> {
    if cfg.mneme()?.is_none() {
        return Ok(None);
    }
    Ok(cfg
        .verba
        .as_ref()
        .and_then(|v| {
            v.bundles
                .iter()
                .find(|b| b.target == eidolon_verba::Target::MnemeRpc)
        })
        .map(|b| b.name.clone()))
}

async fn build(
    cfg: &config::Config,
    mut session: Session,
    cwd: PathBuf,
    model: Option<&str>,
    provider_override: Option<&str>,
    user: Arc<dyn UserIo>,
    yolo: bool,
) -> anyhow::Result<Runtime> {
    // The tools' host first, so its Rune context can build on another
    // thread while this one builds the catalog; see the Tui arm.
    //
    // The vault client is kept here as well as moved into the host, because
    // the session's own identity — its roster id and working directory —
    // arrives further down (the presence is registered once the log exists),
    // and the label every call is attributed with has to be set on the client
    // the tools reach. Clones share one cell, so whichever copy is reached
    // last, both see it.
    let mneme = cfg.mneme()?;
    let host = match &mneme {
        Some(m) => eidolon_rune::Host::with_mneme(cwd.clone(), m.clone()),
        None => eidolon_rune::Host::new(cwd.clone()),
    };
    // The key the session's `search` tool would use, if the operator has
    // stored one: read here, in Rust, so the value never reaches a script —
    // and before `register_builtins` below, which is what decides whether the
    // tool exists at all.
    if let Some(key) = config::search_key(cfg) {
        host.attach_search_key(key);
    }
    // And the store a script's own endpoint resolves its `token_secret` against:
    // handed over here, read per call inside the primitive, and never a string a
    // script, a tool result or a log can reach — the same arrangement the search
    // key above has, with the name coming from the script.
    host.attach_secrets(std::sync::Arc::new(config::secret_store(cfg)));
    // That thread compiles the policy table too, rather than only warming the
    // context. `policy.rn` is a 500-line script and five entry-point calls at
    // load — about 2 ms, which is a fifth of the whole launch and buys the
    // main thread nothing, since it is building the catalog meanwhile. The
    // table is needed at the `Dispatcher`, several steps below.
    let policy_task = {
        let host = host.clone();
        let settings = cfg.policy_settings();
        let path = cfg.policy_path();
        std::thread::spawn(move || -> anyhow::Result<Option<(eidolon_rune::policy::ScriptPolicy, Option<String>)>> {
            let _ = host.compiler();
            match settings {
                Some(settings) => Ok(Some(eidolon_rune::policy::ScriptPolicy::load(&path, &host, settings)?)),
                None => Ok(None),
            }
        })
    };
    // Announce this session to its neighbours *before* the backend is
    // built or the tools are registered. The tools' registry decides then
    // whether `peers` and `send` exist at all, and a token-pool claim is
    // taken *as* the registered session — the holder a lane is attributed
    // to is the presence another session can probe, so claiming before
    // registering would attribute a lane to nobody. Registration is a
    // `mkdir` and one small write — nothing here opens a log, spawns a
    // process or waits on anything, which is what keeps it on the way to
    // the first frame. Failing to register is a line on stderr and a
    // session that works alone, never a harness that will not start.
    //
    // The model this launch resolves: the one the operator named — flag,
    // config default, or `do`'s mock — or, when none was named, the one the
    // journal carries. That second provenance is what `backend_for` below
    // tells a refused request from a projection landing by.
    let from_journal = model.is_none();
    let model: String = model.map(str::to_string).unwrap_or_else(|| {
        session
            .model()
            .unwrap_or_else(|| session.start().map(|(m, ..)| m.to_string()).unwrap_or_default())
    });
    // The meta's model starts as the string the launch was given; the
    // resolved key lands in it two marks below, once there is one.
    let swarm = if cfg.swarm_enabled() {
        let title = first_asked(&session);
        match eidolon_swarm::Presence::register(
            &eidolon_swarm::Presence::root(),
            session.path(),
            &cwd,
            &model,
            &title,
        ) {
            Ok(p) => {
                host.attach_swarm(p.clone());
                Some(p)
            }
            Err(e) => {
                eprintln!("[swarm] not registering this session: {e:#}");
                None
            }
        }
    } else {
        None
    };
    boot::mark("swarm");
    // The vault label this session's writes are attributed with, built here
    // because it is identity and identity is now known: the roster id comes
    // from the presence above, and a session that did not register — no swarm
    // configured, or a registration that failed — is labelled by its log,
    // which is the other name it has that is unique to it and stable.
    //
    // Handed to the vault client (the label has to sit under the tools' own
    // `mneme_call`, not on this side of the Rune boundary) and to the agent
    // below, which keeps the two parts that move with the conversation, the
    // persona it wears and the model it runs, current.
    let attribution = eidolon_core::attribution::Attribution::new(
        swarm.as_ref().map(|p| p.id().to_string()).unwrap_or_else(|| {
            session
                .path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "session".into())
        }),
        eidolon_swarm::tilde(&cwd.display().to_string()),
    );
    if let Some(m) = &mneme {
        m.set_attribution(attribution.clone());
    }
    let catalog = Arc::new(match &swarm {
        Some(p) => config::catalog(cfg, eidolon_providers::Fetch::Cached)
            .with_keypool(eidolon_swarm::Presence::root(), p.id()),
        // A session that did not register has nothing to claim a lane as;
        // a pooled provider then serves it the first lane, shared.
        None => config::catalog(cfg, eidolon_providers::Fetch::Cached),
    });
    boot::mark("catalog");
    // A model the operator named failing to resolve is a refused request.
    // A model the *journal* carried failing to resolve is a projection
    // landing: the key belongs to another machine's catalog (`eidolon:…`,
    // Melete's engine prefix, names no provider here), and the branch's
    // turns are history either way. So the session opens on the configured
    // default instead — stated here, journaled as a note beside the
    // projection note it follows, and `space m` moves it from there. The
    // birth key stays what the branch records; nothing is rewritten.
    let (backend, key, claim) = match catalog.backend_for(
        &model,
        provider_override,
        &cwd,
        &cfg.sessions_dir().join("claude-cli"),
    ) {
        Ok(resolved) => resolved,
        Err(e) if from_journal && provider_override.is_none() => {
            let fallback = cfg
                .default_model
                .clone()
                .unwrap_or_else(|| default_model(cfg));
            eprintln!(
                "[model] {e}; opening this session on `{fallback}` (space m to change)"
            );
            let note = format!(
                "opened on `{fallback}`: no provider on this machine serves the journaled model `{model}`"
            );
            let already = session.records().last().is_some_and(|r| {
                matches!(&r.kind, RecordKind::Note { text } if *text == note)
            });
            if !already
                && let Err(e) = session.append(RecordKind::Note { text: note })
            {
                eprintln!("[model] could not journal the substitution: {e:#}");
            }
            catalog.backend_for(
                &fallback,
                provider_override,
                &cwd,
                &cfg.sessions_dir().join("claude-cli"),
            )?
        }
        Err(e) => return Err(e),
    };
    if let Some(p) = &swarm {
        // What the roster shows is the model the session resolved onto, as
        // it always was — registering before the catalog existed only
        // means publishing it a moment later.
        p.update(|m| m.model = key.clone());
    }
    if let Some(k) = claim.filter(|k| k.fresh) {
        // Per-session key attribution, journaled where the turns it
        // attributes live: which lane of which pool this conversation's
        // requests ride, readable back out of `eidolon log` beside the
        // usage it explains. Best-effort — the lane is claimed either way,
        // and a log that will not take a note must not stop a session.
        let text = format!(
            "key pool: {} lane {} ({})",
            k.provider,
            k.secret,
            swarm.as_ref().map(|p| p.id().to_string()).unwrap_or_default()
        );
        if let Err(e) = session.append(RecordKind::Note { text }) {
            eprintln!("[keypool] could not journal the claim: {e:#}");
        }
        eprintln!(
            "[keypool] {} lane {} held by this session",
            k.provider, k.secret
        );
    }
    boot::mark("backend");

    // The command safety classifier: `harnox::policy` takes a command apart,
    // `policy.rn` decides the leaves. Built on the host above, so the table
    // compiles against the same Rune context the tools do rather than
    // standing up a second one. `[policy] enabled = false` puts `AllowAll`
    // back, which is the honest way to have no gate.
    let (policy, gated): (Arc<dyn PolicyHook>, bool) = match policy_task
        .join()
        .map_err(|_| anyhow::anyhow!("the thread compiling policy.rn panicked"))??
    {
        Some((policy, note)) => {
            if let Some(note) = note {
                eprintln!("[policy] table: {note}");
            }
            (Arc::new(policy), true)
        }
        None => (Arc::new(AllowAll), false),
    };

    // The blanket yes, under the judge and over the table. Installed
    // whenever there is a gate, armed or not: `:yolo` is a switch the
    // operator throws mid-session, and a switch that only existed when the
    // launch flag had been passed would be one they could not reach at the
    // moment they wanted it. Disarmed it is an atomic load per question.
    //
    // With no gate there is no switch, and `--yolo` says so rather than
    // pretending: a session where nothing is classified has no questions
    // to answer.
    let (policy, yolo): (Arc<dyn PolicyHook>, _) = if gated {
        let switch = eidolon_core::yolo::Switch::new(yolo);
        if switch.armed() {
            eprintln!("[policy] yolo: every question is answered yes; refusals still refuse");
        }
        (
            Arc::new(eidolon_core::yolo::Yolo::new(policy, switch.clone())),
            Some(switch),
        )
    } else {
        if yolo {
            eprintln!(
                "[policy] --yolo: there is no gate here to bypass ([policy] enabled = false)"
            );
        }
        (policy, None)
    };
    boot::mark("policy");
    let bus = EventBus::default();
    // The log's path, read before the session moves behind the mutex —
    // spills land beside it, where a session hop carries both.
    let log_path = session.path().to_path_buf();
    let session = Arc::new(Mutex::new(session));

    // The context-aware layer, if the operator named a model for it. It
    // wraps the table rather than replacing it: the deterministic pass still
    // decides everything, and this only answers the questions that pass
    // raised. Off by default — see `[policy] judge`.
    //
    // Nothing here is on the way to the first frame: building it is
    // resolving one catalog key, and the model is not called until a call is
    // flagged.
    let policy = match cfg.policy_judge() {
        Some((key, timeout)) => match judge_provider(&catalog, &key, &cwd) {
            Ok(provider) => {
                eprintln!("[policy] questions go to {key} first");
                Arc::new(eidolon_core::escalate::Judge::new(
                    policy,
                    provider,
                    eidolon_providers::catalog::model_id(&key),
                    timeout,
                    session.clone(),
                )) as Arc<dyn PolicyHook>
            }
            // A judge that will not build is a session that asks more
            // questions, which is the safe direction and not worth
            // refusing to start over. Said out loud, though: the operator
            // configured something that is not running.
            Err(e) => {
                eprintln!("[policy] no judge ({e:#}); every question reaches you");
                policy
            }
        },
        None => policy,
    };

    let mut reg = ToolRegistry::new();
    // Each built-in from its seeded copy in the tools directory, or from
    // the binary when there is none; then the user's own tools, on the
    // same contract. A broken file is a line on stderr and the rest still
    // load — a broken copy of a built-in, with the shipped one in force.
    let mut notes = eidolon_rune::register_builtins(&mut reg, &host, &cfg.tools_dir())?;
    notes.extend(eidolon_rune::register_dir(
        &mut reg,
        &host,
        &cfg.tools_dir(),
    ));
    for note in notes {
        eprintln!("[tool] {note}");
    }
    reg.register(Arc::new(ChoicesUser::new(user.clone())));
    // The park: `wait_for` arms a condition against the registry the loop
    // reads, which is why the registry is created here — the tool has to
    // exist before the dispatcher that serves it, and the agent that ends
    // the turn comes later still. The facts a condition is evaluated against
    // are wired at the same moment, because they are this binary's to know.
    let parks = Arc::new(eidolon_core::wait::Parks::new());
    parks.set_vantage(Arc::new(vantage::Facts::new(
        cwd.clone(),
        swarm.clone(),
        mneme.clone(),
    )));
    reg.register(Arc::new(eidolon_core::wait::WaitFor::new(
        parks.clone(),
        eidolon_core::wait::DEFAULT_TIMEOUT,
        eidolon_core::wait::MAX_TIMEOUT,
    )));
    // The search tool, and the deferred tools it reaches for. Melete's
    // listing comes from the cache and is never fetched on the way to the
    // first frame; a missing or day-old cache is refreshed on a thread of
    // its own for the next launch, and the note says so. No `[melete]`
    // stanza registers nothing: silently when there is no cache either,
    // but with a word when one is primed — a wired binary that registers
    // nothing while a cache sits there was once a stale config that cost
    // an hour to diagnose (bugs tracker, 2026-09-06 #2).
    let search = Arc::new(eidolon_core::ToolSearch::new());
    reg.register(search.clone());
    if let Some((client, opts)) = cfg.melete()? {
        for note in eidolon_remote::melete::register(
            &mut reg,
            Arc::new(client),
            &opts,
            eidolon_remote::melete::cache::path().as_deref(),
        ) {
            eprintln!("[melete] {note}");
        }
    } else if let Some(path) = eidolon_remote::melete::cache::path().filter(|p| p.exists()) {
        eprintln!(
            "[melete] tool cache present at {} but no [melete] in config; not registering — add the stanza or prime with: cargo run -- tools --live",
            path.display()
        );
    }
    // The vault command channel: one prose line in, one dispatched Mneme read
    // out. Armed only where it can actually work — a `[[verba.bundles]]`
    // aimed at `mneme_rpc` *and* a `[mneme]` stanza to reach, since the
    // resolved call is an ordinary `mneme_rpc` dispatch and a channel with no
    // vault behind it would be a guaranteed error line for the model. No
    // bundles means no channel, exactly as the CLI and the TUI say elsewhere.
    //
    // Two entry seams, and `[verba] entry` picks between them. `inline` (the
    // default) intercepts `! <command>` lines in the model's own reply and
    // splices the teaching into the system prompt instead of registering a
    // tool; `tool` registers `vv`. They never coexist: two surfaces taught at
    // once is two conventions to satisfy and two adoption rates measured as
    // one.
    //
    // Inline is armed only for a **provider** backend. A driver owns its own
    // loop — the harness never assembles that turn's request and has no call
    // to hand the answers to — so such a session keeps the tool, and the
    // teaching is not spliced there: describing a channel nothing answers is
    // the tool-starved confabulation the vault notes are about, and it is
    // exactly what a mismatch between the prompt and the wiring would do.
    let channel = command_bundle(cfg)?
        .map(|bundle| Arc::new(eidolon_verba::VaultCommands::new(cfg.palette(), bundle)));
    let inline = channel.is_some()
        && cfg.verba_entry() == eidolon_verba::Entry::Inline
        && matches!(backend, Backend::Provider(_));
    if let Some(channel) = &channel {
        if inline {
            reg.register_prompt("Vault commands", eidolon_verba::command::inline_prompt());
            eprintln!(
                "[verba] vault commands are inline: `! <command>` lines in the model's reply, answered in place (no `vv` tool)"
            );
        } else {
            reg.register(channel.clone());
        }
    }
    boot::mark("tools");

    let dispatcher = Arc::new(Dispatcher::new(
        reg,
        policy,
        user,
        bus.clone(),
        session,
        cwd.clone(),
    ));
    // Set where the log's path was still in hand, above.
    dispatcher.set_spill_dir(Some(Dispatcher::spill_dir_for(&log_path)));
    host.attach(&dispatcher);
    search.attach(&dispatcher);
    if let Some(channel) = &channel {
        channel.attach(&dispatcher);
        // The same object also watches the chokepoint: the model's own
        // hand-written `mneme_rpc` calls are what the channel is measured
        // against, and this is the only place they can be seen.
        dispatcher.observe(channel.clone());
    }

    let agent = Agent::with_backend(
        backend,
        dispatcher,
        AgentConfig {
            model: eidolon_providers::catalog::model_id(&key).to_string(),
            model_key: key,
            system: Some(cfg.system_prompt()),
            max_tokens: cfg.max_tokens,
            max_iterations: cfg.max_iterations,
            thinking: cfg
                .thinking_budget
                .map(|b| ThinkingConfig { budget_tokens: b }),
            cache_retention: cfg.cache_retention,
            project_instructions: cfg.project_instructions(),
        },
    );
    boot::mark("agent");
    // The two parts of the label that move: the persona the turn wears and
    // the model the session runs. The agent seeds the model from the config
    // just handed to it and refreshes the persona from the resolution every
    // turn already runs.
    agent.set_attribution(attribution.clone());

    // Personas come from the same vault the tools reach, so a harness with
    // no `[mneme]` simply has none — `:persona` says so rather than
    // failing, and nothing on the way to the first frame asks the vault
    // anything.
    if let Some(m) = cfg.mneme()? {
        // The persona source reads notes through a client of its own, so it
        // needs the label too: a `list_notes` or `read_note` it makes is an
        // RPC like any other, and one vault client without a label is one
        // path where the model's own value would have survived.
        m.set_attribution(attribution.clone());
        agent.set_persona_source(Arc::new(persona::VaultPersonas::new(m)));
    }

    // Arm the interceptor where it was chosen and where it can work: the
    // loop scans each completed reply for the channel's marked lines, runs
    // what it finds through the same gate and the same nested dispatch, and
    // gives the model another call to read the answers on. See
    // `eidolon_core::commands`.
    if inline && let Some(channel) = &channel {
        agent.set_command_channel(channel.clone());
    }

    // The doorbell is bound after the agent so that a message arriving in
    // the same millisecond has somewhere to be taken delivery of. The agent
    // is shared from here on: the socket's op handler runs on its own task
    // and has to reach the same session the consumer drives.
    let agent = Arc::new(agent);
    // The same parks the `wait_for` tool was built against: the loop reads
    // them to end the turn a park was armed in, and the drivers read them to
    // journal a fire and start the woke turn.
    agent.set_parks(parks);
    let (doorbell, task) = match &swarm {
        Some(p) => {
            agent.set_inbox(p.clone());
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            // The quiesce half of the doorbell. The socket knows `ping` and
            // `notify`; a question about this session's own turn loop is
            // answered by the session, which is the only thing holding it —
            // and the same ring carries the answer, so an idle session's
            // driver wakes to journal the marker rather than waiting for a
            // turn that may never come.
            let handler: eidolon_swarm::socket::OpHandler = {
                let agent = agent.clone();
                Arc::new(
                    move |req: &serde_json::Value,
                          ring: &tokio::sync::mpsc::UnboundedSender<()>| {
                        eidolon_core::quiesce::doorbell::answer(&agent, req, ring)
                    },
                )
            };
            match eidolon_swarm::socket::serve_with(
                &p.socket_path(),
                tx,
                CancellationToken::new(),
                Some(handler),
            )
            .await
            {
                Ok(t) => (Some(rx), Some(t)),
                Err(e) => {
                    // Unreachable is not the same as unregistered: the
                    // inbox still fills and is still drained at every
                    // turn boundary. What is lost is being woken, and
                    // being *seen* — a session whose socket does not
                    // answer reads as dead to the roster.
                    eprintln!("[swarm] no doorbell, so peers will not see this session: {e:#}");
                    (None, None)
                }
            }
        }
        None => (None, None),
    };
    Ok(Runtime {
        agent,
        bus,
        catalog,
        cwd,
        swarm,
        yolo,
        doorbell,
        _swarm_task: task,
    })
}

/// Who is running right now — `eidolon peers`, or `door list`.
///
/// Answered without registering a session to ask with: this is a question
/// about the swarm, not a membership of it. It sweeps the registrations of
/// sessions that died without cleaning up, which is the other reason to have
/// it — a scan is what collects them.
fn door_list() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let peers = eidolon_swarm::scan(&eidolon_swarm::Presence::root());
    println!(
        "{}",
        eidolon_swarm::roster_of(peers, &eidolon_swarm::channel_key_of(&cwd), None)
    );
    Ok(())
}

/// An outside caller's door into a running session: the same inbox a peer
/// writes to, but marked as coming from outside any session, so the
/// recipient's model frames it as a tool's input — never as its operator's
/// words and never as a colleague's. The message is written before the
/// doorbell is rung, so an unanswered ring is *queued*, not lost.
///
/// A lone `-` reads the message from stdin, which is the only way to hand
/// one over without it appearing in `ps` and in the shell's history.
fn door_send(
    from: Option<String>,
    wake: bool,
    no_wake: bool,
    cwd: Option<PathBuf>,
    to: String,
    text: Vec<String>,
) -> anyhow::Result<()> {
    let cwd = match cwd {
        Some(c) => c.canonicalize()?,
        None => std::env::current_dir()?,
    };
    let mut text = text.join(" ");
    if text.trim() == "-" {
        use std::io::Read;
        text.clear();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("reading the message from stdin")?;
    }
    let wake = if wake {
        Some(true)
    } else if no_wake {
        Some(false)
    } else {
        None
    };
    let from = from.as_deref().unwrap_or("cli");
    println!(
        "{}",
        eidolon_swarm::send_external(
            &eidolon_swarm::Presence::root(),
            &cwd,
            from,
            &to,
            &text,
            wake
        )?
    );
    Ok(())
}

/// Ask a running session to quiesce, and wait for its boundary — the
/// outside half of `eidolon_core::quiesce`, and the whole of what a shipper
/// needs on the birth side.
///
/// Three outcomes, and the exit status separates them: settled (0), refused
/// by timeout (2, said on stderr), and no such session (1, through `bail!`).
/// A session that answers the ask but never settles is the refusal, never
/// the dead door — the ask went through and the boundary did not arrive.
///
/// One thing needs saying about *when* the door goes. A session that has
/// quiesced leaves the roster and removes its own socket on the way out
/// (`Presence::deregister`), so the status poll that would have read
/// `quiesced: true` can find nothing to connect to: the success and the
/// close arrive as one event. That is not read as a fault, and not guessed
/// at either — the session's own journal is opened and asked, and the
/// quiesce marker is the one thing a settled close leaves there (see
/// [`journal_says_quiesced`]). The log is the truth, which is the same
/// reason the shipper copies it rather than trusting this exit status.
fn door_quiesce(
    session: &str,
    destination: &str,
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    let peers = eidolon_swarm::scan(&eidolon_swarm::Presence::root());
    let Some(peer) = eidolon_swarm::presence::find_in(&peers, session) else {
        anyhow::bail!("{}", no_such_session(&peers, session));
    };
    let (socket, log, id) = (peer.socket(), peer.meta.log.clone(), peer.meta.id.clone());
    let knock = eidolon_core::quiesce::knock_and_wait(destination, timeout, |req| {
        eidolon_swarm::socket::request(&socket, req)
            .with_context(|| format!("asking session {id}"))
    });
    let knock = match knock {
        Ok(k) => k,
        Err(_) if journal_says_quiesced(&log, destination) => {
            println!(
                "{}",
                eidolon_core::quiesce::Knock::Settled.line(destination, timeout)
            );
            return Ok(());
        }
        // It left without settling: the error stands, and it says what the
        // reader saw — a session that stopped answering.
        Err(e) => return Err(e),
    };
    match knock {
        eidolon_core::quiesce::Knock::Settled => {
            println!("{}", knock.line(destination, timeout));
            Ok(())
        }
        eidolon_core::quiesce::Knock::Refused => {
            // A refusal is what a shipper acts on, so it is not a quiet
            // success and not a dead door: its own exit status, and the
            // reason on stderr where a pipeline can see it.
            eprintln!("{}", knock.line(destination, timeout));
            std::process::exit(2)
        }
    }
}

/// Does this journal say the session settled for this destination? The
/// caller's half of the protocol when the door closes under it.
///
/// The marker is a free-form `Note` naming the hop, journaled at the boundary
/// *after* the settle — so the branch reads as settled
/// ([`Session::is_settled`] walks past a note to the `TurnSettled` behind it)
/// and the marker is the last record. Both are required: a session that
/// settled for someone else, or one whose last word says it quiesced for
/// this venue without a boundary behind it, is not a success to report.
fn journal_says_quiesced(log: &std::path::Path, destination: &str) -> bool {
    let Ok(s) = Session::open(log) else {
        return false;
    };
    s.is_settled()
        && s.records().last().is_some_and(|r| {
            matches!(&r.kind, RecordKind::Note { text }
                if text.contains(&format!("quiesced for projection to {destination}")))
        })
}

/// The error for a session that is nobody: a dead end turned into one retry
/// by saying who *is* there. The swarm's own wording, for the caller that
/// has a roster and no presence to ask with.
fn no_such_session(peers: &[eidolon_swarm::Peer], name: &str) -> String {
    if peers.is_empty() {
        format!("there is no session `{name}`, and no session is registered at all")
    } else {
        let names: Vec<String> = peers.iter().map(|p| p.meta.id.clone()).collect();
        format!("there is no session `{name}`. Running right now: {}.", names.join(", "))
    }
}

/// `git worktree add`, with the naming decided rather than asked for.
///
/// The harness makes the checkout and stops there. It does **not** start a
/// session in it: a session is a process with a terminal, and the harness
/// has no notion of opening one — spawning siblings is a real feature and
/// a much larger one than this. What this buys, today, is the half that
/// stands on its own: two agents editing one repository without editing
/// one working tree.
///
/// The worktrees are found by the swarm without being told about them:
/// they share a git common directory, which is what the project channel
/// keys on, so a session started in either one is on the same channel.
fn worktree(branch: &str, path: Option<&std::path::Path>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let git = |args: &[&str]| -> anyhow::Result<String> {
        let out = std::process::Command::new("git")
            .current_dir(&cwd)
            .args(args)
            .output()
            .context("running git")?;
        if !out.status.success() {
            bail!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };

    let top = PathBuf::from(
        git(&["rev-parse", "--show-toplevel"]).context("this is not a git repository")?,
    );
    let name = top
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => top
            .parent()
            .unwrap_or(&top)
            .join(format!("{name}-{}", branch.replace('/', "-"))),
    };
    if path.exists() {
        bail!("{} already exists", path.display());
    }

    // An existing branch is checked out; a new one is created here. Asking
    // git which it is first turns "fatal: a branch named … already exists"
    // into the ordinary case it usually is.
    let exists = git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/heads/{branch}"),
    ])
    .is_ok();
    let p = path.display().to_string();
    if exists {
        git(&["worktree", "add", &p, branch])?;
    } else {
        git(&["worktree", "add", "-b", branch, &p])?;
    }

    println!("{}", path.display());
    eprintln!("[worktree] {branch} at {}", path.display());
    eprintln!(
        "[worktree] start a session there with: cd {} && eidolon",
        path.display()
    );
    eprintln!("[worktree] it shares this repository's channel, so `peers` will see it");
    Ok(())
}

/// The provider behind the model the operator named as the policy judge.
///
/// It is resolved separately from the session's own backend on purpose: the
/// judge answers a one-line question and wants something cheap and fast,
/// which is rarely the model the session is running. A backend that drives
/// its own process (the Claude CLI) cannot serve — the judge needs a plain
/// completion, not a session — so naming one is an error the operator hears
/// about rather than a judge that silently never fires.
fn judge_provider(
    catalog: &eidolon_providers::Catalog,
    key: &str,
    cwd: &std::path::Path,
) -> anyhow::Result<Arc<dyn eidolon_core::Provider>> {
    // A scratch path the driver arm would use; unreachable for a provider,
    // and this arm rejects the driver. The pool claim rides along — a
    // judge resolved from the session's process re-reads the session's own
    // lane, sticky, rather than taking a second key for one-shot answers.
    let (backend, _, _) = catalog.backend_for(key, None, cwd, cwd)?;
    match backend {
        Backend::Provider(p) => Ok(p),
        Backend::Driver(d) => {
            anyhow::bail!(
                "`{}` drives its own session and cannot answer a one-shot question",
                d.name()
            )
        }
    }
}

/// Journal a fresh token-pool claim onto the session the agent holds —
/// the same attribution line [`build`] writes at launch, for a pool first
/// claimed mid-session (`:model` onto a pooled provider, or adopting a
/// log that ran on one). Sticky re-claims journal nothing: one line per
/// provider per session is the whole ledger.
async fn journal_claim(
    agent: &Agent,
    claim: &Option<eidolon_providers::catalog::ClaimedKey>,
) {
    let Some(k) = claim.as_ref().filter(|k| k.fresh) else { return };
    let text = format!("key pool: {} lane {}", k.provider, k.secret);
    let mut s = agent.session().lock().await;
    if let Err(e) = s.append(RecordKind::Note { text }) {
        eprintln!("[keypool] could not journal the claim: {e:#}");
    }
}

/// The first thing asked of a session — what it is remembered by in the
/// picker, and the only cheap answer to "what is that one working on".
/// Empty for a session that has not been spoken to yet; the consumer
/// fills it in when it is.
fn first_asked(session: &Session) -> String {
    session
        .branch()
        .into_iter()
        .find_map(|r| match &r.kind {
            RecordKind::UserMessage(m) => Some(m.text()),
            _ => None,
        })
        .map(|t| {
            let flat: String = t
                .replace('\n', " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            flat.chars().take(72).collect()
        })
        .unwrap_or_default()
}

/// One headless turn's input: the words, and whatever was attached.
struct Said {
    text: String,
    images: Vec<ContentBlock>,
}

/// Read every `--image` path, or say which one was not an image.
///
/// The read happens here, in the process the operator typed the path into,
/// and never through the dispatcher — the path came from the operator, who
/// is the one thing the policy hook exists to represent. A path chosen by
/// the *model* would be a different question and would have to be a
/// dispatched call.
fn attachments(paths: &[PathBuf]) -> anyhow::Result<Vec<ContentBlock>> {
    paths
        .iter()
        .map(|p| eidolon_tools::image::Attachment::load(p).map(|a| a.block()))
        .collect()
}

/// Say which persona the session is wearing, once, before the first turn.
///
/// It resolves the note rather than echoing the pin, because those two can
/// disagree in exactly the case worth reporting: a pin that names nothing,
/// or a vault that is not answering, is silent everywhere else — the turn
/// runs in the harness's own voice by design — and a headless run would give
/// the operator no way to tell that from a persona that loaded. Resolving
/// here costs the one fetch the turn was going to make anyway.
///
/// The line names the memory note too when the pin read one, for the same
/// reason: only the pin is journaled, so a turn that wore a character without
/// her memory — or a memory without her — leaves nothing behind to notice it
/// by.
async fn report_persona(rt: &Runtime) {
    let pin = { rt.agent.session().lock().await.persona() };
    let Some(pin) = pin else { return };
    match rt.agent.resolve_persona().await {
        r if r.is_empty() => eprintln!("persona: '{pin}' did not resolve; running unpersonated"),
        r => eprintln!("persona: {}{}", r.name.as_deref().unwrap_or(&pin), worn_memory(&r)),
    }
}

/// The memory note a pin also read, as a wear line's tail: empty when the
/// persona has none, which is the ordinary case and wants no note.
fn worn_memory(r: &eidolon_core::persona::Resolved) -> String {
    r.memory.as_deref().map(|m| format!(" (memory: {m})")).unwrap_or_default()
}

/// SIGINT and SIGTERM cancel the same token: the keystroke and the
/// supervisor's `timeout(1)` mean the same thing to a turn. Unhandled,
/// SIGTERM killed the process mid-call and left the journal unsettled.
async fn cancel_on_signals(cancel: CancellationToken) {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "could not listen for SIGTERM; SIGINT still cancels");
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("\n[cancelling]");
                cancel.cancel();
            }
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    eprintln!("\n[cancelling]");
    cancel.cancel();
}

async fn drive(rt: &mut Runtime, said: Option<Said>) -> anyhow::Result<()> {
    let cancel = CancellationToken::new();
    {
        let cancel = cancel.clone();
        tokio::spawn(cancel_on_signals(cancel));
    }
    let printer = tokio::spawn(term::print_events(rt.bus.subscribe()));
    let unsettled = rt.agent.session().lock().await.needs_finishing();
    if unsettled {
        eprintln!("[finishing an unsettled turn]");
        let out = rt.agent.continue_turn(cancel.clone()).await?;
        report(rt, &out);
    }
    if let Some(said) = said {
        // Images first, then the words: the order both wires read a
        // message in, and the order the TUI draws them.
        let mut blocks = said.images;
        blocks.push(ContentBlock::text(said.text));
        let out = rt.agent.run_turn(blocks, cancel.clone()).await?;
        report(rt, &out);
    }
    // A turn that parked is not a finished turn. This is a one-shot process,
    // so it stays alive until the condition resolves or its deadline arrives
    // — the same wake an interactive driver gets by staying open — and only
    // then prints its usage and exits.
    //
    // Mail does not wake it: this loop watches the park's ring and not the
    // doorbell, so a parked `run` session reads its inbox at the turn the
    // park buys whatever level the park named. The interactive drivers ask
    // `Parks::admits` at their doorbell; wiring one in here would be a second
    // wake path and a behaviour change for a one-shot process, so it is left
    // as the gap it is rather than half-done.
    loop {
        if !rt.agent.parks().is_armed() || cancel.is_cancelled() {
            break;
        }
        if rt.agent.take_fires().await.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        }
        eprintln!("[waking: the wait resolved]");
        let out = rt.agent.continue_turn(cancel.clone()).await?;
        report(rt, &out);
    }
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    printer.abort();
    let session = rt.agent.session().lock().await;
    eprintln!("[{}]", usage_summary(rt, &session));
    Ok(())
}

/// The line-oriented REPL. One cancellation token per turn; the ctrl-c
/// task cancels whichever token is current, or counts presses at the
/// prompt.
/// The idle half of peer delivery: take the mail that is waiting and
/// answer what asked to be answered. A turn in flight never comes through
/// here — it collects its own peer traffic at its safe points — so this
/// only ever runs at the prompt, where starting a turn on a sender's
/// message is exactly what its `wake` asked for. The TUI driver's
/// `collect` is the same shape on the same ring; this one reports on
/// stderr, which is the screen a headless session has.
///
/// The quiesce is checked first, and it wins: a session that has been asked
/// to settle must not start the turn a message would have bought. `true`
/// means the session has quiesced and the REPL should leave.
async fn answer_the_door(
    rt: &Runtime,
    current: &Arc<std::sync::Mutex<Option<CancellationToken>>>,
) -> bool {
    if close_if_quiesced(rt).await {
        return true;
    }
    let messages = rt.agent.drain_peers().await;
    // Drained is delivered; what is decided here is whether any of it wakes a
    // session that is parked. A park's `wake` level says which mail may start
    // a turn — level 0 for none, level 1 for mail marked to wake (the
    // ordinary rule), level 2 for any — and mail it refuses waits in the
    // branch for the next turn rather than being dropped.
    let asked: Vec<String> = messages
        .iter()
        .filter(|m| rt.agent.parks().admits(m.wake))
        .map(|m| m.from.clone())
        .collect();
    if asked.is_empty() {
        return false;
    }
    eprintln!("[answering {}]", asked.join(", "));
    let t = CancellationToken::new();
    *current.lock().unwrap() = Some(t.clone());
    let out = rt.agent.continue_turn(t).await;
    *current.lock().unwrap() = None;
    match out {
        Ok(o) => report(rt, &o),
        Err(e) => eprintln!("\n[error] {e:#}"),
    }
    // The turn that just settled may itself have been the boundary.
    close_if_quiesced(rt).await
}

/// Settle a quiesce that has reached its boundary, say the goodbye, and
/// leave the roster. `true` means the session has finished: the caller stops.
///
/// An idle session has no turn to journal the marker at, so this is where it
/// happens; a busy one already had it journaled at its settle, and the flag
/// is what this reads. Nothing is forced either way — a marker that could not
/// be written leaves the session unsettled, running, and saying so.
async fn close_if_quiesced(rt: &Runtime) -> bool {
    match rt.agent.poll_quiesce().await {
        Ok(true) => {}
        Ok(false) => return false,
        Err(e) => {
            eprintln!("[quiesce] could not journal the marker: {e:#}");
            return false;
        }
    }
    let destination = rt
        .agent
        .quiesce_request()
        .map(|q| q.destination)
        .unwrap_or_default();
    let id = rt.swarm.as_ref().map(|p| p.id().to_string());
    eprintln!("\n{}", eidolon_core::quiesce::goodbye(&destination, id.as_deref()));
    // Off the roster before anything else looks: a session that has projected
    // is deliberately dark, because its presence belongs to its venue now.
    if let Some(p) = &rt.swarm {
        p.deregister();
    }
    true
}

async fn repl(rt: &mut Runtime, cfg: &config::Config) -> anyhow::Result<()> {
    use std::sync::Mutex as StdMutex;
    let current: Arc<StdMutex<Option<CancellationToken>>> = Arc::default();
    let quit = CancellationToken::new();
    {
        let current = current.clone();
        let quit = quit.clone();
        tokio::spawn(async move {
            let mut last: Option<std::time::Instant> = None;
            let mut term = match tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::terminate(),
            ) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "could not listen for SIGTERM; SIGINT still quits");
                    quit.cancel();
                    return;
                }
            };
            // SIGTERM at the prompt quits outright; mid-turn it cancels
            // the turn like Ctrl-C.
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {
                        quit.cancel();
                        return;
                    }
                }
                let tok = current.lock().unwrap().clone();
                match tok {
                    Some(t) => {
                        eprintln!("\n[cancelling the turn]");
                        t.cancel();
                    }
                    None => {
                        if last.is_some_and(|l| l.elapsed() < std::time::Duration::from_secs(2)) {
                            quit.cancel();
                            return;
                        }
                        eprintln!("\n[ctrl-c again to quit, or :quit]");
                        last = Some(std::time::Instant::now());
                    }
                }
            }
        });
    }
    let printer = tokio::spawn(term::print_events(rt.bus.subscribe()));

    if rt.agent.session().lock().await.needs_finishing() {
        eprintln!("[finishing an unsettled turn]");
        let t = CancellationToken::new();
        *current.lock().unwrap() = Some(t.clone());
        let out = rt.agent.continue_turn(t).await;
        *current.lock().unwrap() = None;
        eprintln!("\n[{:?}]", out?);
    }
    eprintln!("[model {} · :help for commands]", rt.agent.model_key());
    let mut staged: Vec<eidolon_tools::image::Attachment> = Vec::new();

    // One reader thread for the life of the REPL, feeding a channel — so
    // the loop can wait on stdin and the doorbell at once without ever
    // abandoning a blocked read (a second reader spawned beside the first
    // would race it for the next line and could eat one). The thread
    // prompts only when told it may, so the prompt appears when the loop
    // is actually ready for input rather than the instant the previous
    // line was taken.
    let (lines_tx, mut lines_rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut first = true;
        loop {
            if !first && ready_rx.recv().is_err() {
                return;
            }
            first = false;
            match term::read_prompt("› ") {
                Some(l) => {
                    if lines_tx.send(Some(l)).is_err() {
                        return;
                    }
                }
                None => {
                    let _ = lines_tx.send(None);
                    return;
                }
            }
        }
    });
    let mut doorbell = rt.doorbell.take();
    // A park resolving is the harness's own wake, and it gets its own ring
    // rather than the doorbell's: mail and a condition are different things
    // to be woken by, and only one of them is a peer talking.
    let mut parks_rx = rt.agent.parks().events();
    // The reader starts READING; the flag says it is parked waiting for
    // permission to prompt again — set on every path that consumed a
    // line, and on no path that rang the doorbell mid-read.
    let mut prompt_next = false;

    loop {
        if prompt_next {
            prompt_next = false;
            let _ = ready_tx.send(());
        }
        let rang = async {
            match &mut doorbell {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        let park_ring = async {
            let _ = parks_rx.recv().await;
        };
        let line = tokio::select! {
            l = lines_rx.recv() => {
                prompt_next = true;
                l.flatten()
            }
            _ = quit.cancelled() => None,
            // The doorbell: mail has arrived while nobody is driving.
            // Draining here is the idle case — a turn in flight collects
            // its own peer traffic at its safe points — and a message
            // that asked to wake starts the turn its sender wanted,
            // exactly as the TUI's driver does on the same ring.
            _ = rang => {
                if answer_the_door(rt, &current).await {
                    break;
                }
                continue;
            }
            // A park this session armed has resolved. The fires are taken
            // here, at the prompt, for the reason a waking peer's mail is:
            // a turn in flight takes them itself at its safe points, and
            // journaling one between a `tool_use` and its results would turn
            // every outstanding call into a synthetic interruption.
            _ = park_ring => {
                if rt.agent.take_fires().await.is_empty() {
                    continue;
                }
                let t = CancellationToken::new();
                *current.lock().unwrap() = Some(t.clone());
                let out = rt.agent.continue_turn(t).await;
                *current.lock().unwrap() = None;
                match out {
                    Ok(o) => report(rt, &o),
                    Err(e) => eprintln!("\n[error] {e:#}"),
                }
                continue;
            }
        };
        let Some(line) = line else { break };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        // `:`, as in the TUI — `/` is reserved, so a line starting with
        // one is prompt text and goes to the model. The name is the TUI's
        // registry's, so `:model` here and `:model` there are one command
        // and not two spellings; a UI-thread command has no screen to act
        // on here and says so rather than pretending.
        if let Some(cmd) = line.strip_prefix(':') {
            let mut parts = cmd.splitn(2, ' ');
            let name = parts.next().unwrap_or("");
            let arg = parts.next().map(str::trim).unwrap_or("");
            // `:do` is `dispatch`'s spelling from before the one vocabulary,
            // and `:log` is chat's own; both are kept.
            let name = match name {
                "do" => "dispatch",
                "q" | "exit" => "quit",
                n => n,
            };
            match name {
                "quit" => break,
                "help" => {
                    eprintln!("the TUI's vocabulary, as far as a line-oriented chat can run it:");
                    // The driver's rows, plus the one UI-thread row that
                    // needs no screen: `yolo` is a store to a shared
                    // atomic, and a command chat runs but does not list is
                    // a command nobody here finds.
                    for c in eidolon_tui::command::COMMANDS
                        .iter()
                        .filter(|c| c.run == eidolon_tui::command::Run::Driver || c.name == "yolo")
                    {
                        eprintln!("  {:<34} {}", eidolon_tui::command::usage(c), c.help);
                    }
                    // Three UI-thread commands that chat can honestly run:
                    // staging an image needs somewhere to keep it until the
                    // next line is sent, and chat has that. It cannot draw
                    // one, which is a different lack and not this one.
                    for c in eidolon_tui::command::COMMANDS
                        .iter()
                        .filter(|c| c.group == "images")
                    {
                        eprintln!("  {:<34} {}", eidolon_tui::command::usage(c), c.help);
                    }
                    eprintln!("  {:<34} the records on this branch", ":log");
                    eprintln!("  {:<34} leave", ":quit");
                    eprintln!("(:do is :dispatch; quote values — :do read the note \"Groceries\")");
                }
                // Staged here and sent with the next line, exactly as in
                // the TUI. Chat cannot *draw* an image, which is why the
                // confirmation names it in full: the label is all the
                // operator gets to check they attached what they meant to.
                "attach" => match eidolon_tools::image::Attachment::load(
                    &eidolon_tools::image::expand_tilde(arg),
                ) {
                    Ok(a) => {
                        eprintln!("[attached {}]", a.label());
                        staged.push(a);
                    }
                    Err(e) => eprintln!("[{e:#}]"),
                },
                "attach_clipboard" => match eidolon_tools::image::from_clipboard() {
                    Some(a) => {
                        eprintln!("[attached {}]", a.label());
                        staged.push(a);
                    }
                    None => eprintln!("[no image on the clipboard]"),
                },
                "attach_clear" => {
                    let before = staged.len();
                    match arg {
                        "" => staged.clear(),
                        name => staged.retain(|a| a.name != name),
                    }
                    eprintln!("[{} unstaged]", before - staged.len());
                }
                "dispatch" => {
                    let palette = cfg.palette();
                    if palette.is_empty() {
                        eprintln!("[no [[verba.bundles]] configured]");
                        continue;
                    }
                    let (res, _) = palette.resolve(arg).await;
                    match res {
                        Some(res) => {
                            eprintln!(
                                "[{} → {} {}]",
                                res.bundle,
                                res.verdict.intent,
                                serde_json::to_string(&res.verdict.slots).unwrap_or_default()
                            );
                            let out = rt
                                .agent
                                .dispatcher()
                                .dispatch_user(res.call, arg, CancellationToken::new())
                                .await;
                            println!("{}", out.content);
                        }
                        None => eprintln!("[no bundle accepted that; send it as a prompt instead]"),
                    }
                }
                "model" => {
                    if arg.is_empty() {
                        eprintln!("[model {}]", rt.agent.model_key());
                        for m in rt.catalog.models() {
                            eprintln!("  {:<36} {}", m.key, m.name);
                        }
                    } else {
                        match rt.catalog.backend_for(arg, None, &rt.cwd, &cfg_scratch()) {
                            Ok((backend, key, claim)) => {
                                rt.agent.set_backend(backend);
                                rt.agent
                                    .set_model(
                                        key.clone(),
                                        eidolon_providers::catalog::model_id(&key).to_string(),
                                    )
                                    .await?;
                                journal_claim(&rt.agent, &claim).await;
                                // The roster the neighbours read says so
                                // too. It was written once at launch and
                                // nothing moved it since, so a peer went on
                                // reading the model this chat opened on.
                                if let Some(p) = &rt.swarm {
                                    p.set_model(&rt.agent.model_key());
                                }
                                eprintln!("[model → {key}]");
                            }
                            Err(e) => eprintln!("[{e:#}]"),
                        }
                    }
                }
                "fork" => match arg.parse::<u64>() {
                    Ok(id) => match rt.agent.fork_at(id).await {
                        Ok(()) => eprintln!("[head → #{id}; the next turn forks from there]"),
                        Err(e) => eprintln!("[{e}]"),
                    },
                    Err(_) => eprintln!("[:fork needs a record id; see :tree]"),
                },
                "tree" => {
                    let s = rt.agent.session().lock().await;
                    for n in s.tree() {
                        let mark = if n.is_head {
                            "●"
                        } else if n.on_branch {
                            "│"
                        } else {
                            " "
                        };
                        let forks = if n.children.len() > 1 {
                            format!("  ⑂ {:?}", n.children)
                        } else {
                            String::new()
                        };
                        println!(
                            "{mark} #{:<4} {}{}",
                            n.record.id,
                            term::record_line(n.record),
                            forks
                        );
                    }
                }
                "compact" => {
                    let t = CancellationToken::new();
                    *current.lock().unwrap() = Some(t.clone());
                    let r = rt.agent.compact(t).await;
                    *current.lock().unwrap() = None;
                    match r {
                        Ok(true) => eprintln!("[compacted]"),
                        Ok(false) => eprintln!("[nothing to compact]"),
                        Err(e) => eprintln!("[compaction failed: {e:#}]"),
                    }
                }
                "note" => {
                    let mut sess = rt.agent.session().lock().await;
                    match arg.trim() {
                        "" => eprintln!(
                            "[{}]",
                            sess.session_note()
                                .unwrap_or_else(|| "no session note".into())
                        ),
                        "-" => {
                            sess.set_session_note("")?;
                            eprintln!("[session note cleared]");
                        }
                        text => {
                            sess.set_session_note(text)?;
                            eprintln!("[session note set]");
                        }
                    }
                }
                // Bare *lists* here rather than opening a picker, which is
                // the one place this differs from the TUI: there is no
                // screen to put a picker on, and the list is the same
                // question answered in the form this surface has.
                "persona" => {
                    let Some(source) = rt.agent.persona_source() else {
                        eprintln!("[no vault configured; personas need [mneme] in config.toml]");
                        continue;
                    };
                    match arg.trim() {
                        "" => match eidolon_core::persona::list(source.as_ref()).await {
                            Err(e) => eprintln!("[{e:#}]"),
                            Ok(list) if list.is_empty() => {
                                eprintln!(
                                    "[no personas under {}]",
                                    eidolon_core::persona::PERSONA_FOLDER
                                )
                            }
                            Ok(list) => {
                                let worn = rt.agent.session().lock().await.persona();
                                for p in list {
                                    let here = worn.as_deref() == Some(p.name.as_str())
                                        || worn.as_deref() == Some(p.path.as_str());
                                    eprintln!(
                                        "{} {}  ({})",
                                        if here { "*" } else { " " },
                                        p.name,
                                        p.path
                                    );
                                }
                            }
                        },
                        "-" => {
                            let cleared = rt.agent.session().lock().await.set_persona("")?;
                            eprintln!(
                                "[{}]",
                                if cleared {
                                    "persona taken off"
                                } else {
                                    "no persona was on"
                                }
                            );
                        }
                        // Resolved before it is accepted, as in the TUI: a
                        // pin that names nothing is a session that runs
                        // silently unpersonated from here on.
                        pin => match eidolon_core::persona::resolve(source.as_ref(), pin).await {
                            Err(e) => eprintln!("[{e:#}]"),
                            Ok(r) if r.is_empty() => {
                                eprintln!("['{pin}' has no body to wear; not pinned]")
                            }
                            Ok(r) => {
                                let name = r.name.as_deref().unwrap_or(pin).to_string();
                                rt.agent.session().lock().await.set_persona(pin)?;
                                eprintln!("[wearing {name}{}]", worn_memory(&r));
                            }
                        },
                    }
                }
                "pin" | "unpin" => {
                    let a = arg.trim();
                    if a.is_empty() && name == "pin" {
                        let pins = rt.agent.session().lock().await.pins();
                        if pins.is_empty() {
                            eprintln!("[nothing pinned]");
                        } else {
                            for p in pins {
                                println!("{p}");
                            }
                        }
                    } else if a.is_empty() {
                        eprintln!("[:unpin takes a path]");
                    } else {
                        let path = eidolon_tools::image::expand_tilde(a)
                            .to_string_lossy()
                            .to_string();
                        let pinning = name == "pin";
                        if pinning && let Err(e) = std::fs::metadata(&path) {
                            eprintln!("[{path}: {e}]");
                        } else {
                            match rt.agent.session().lock().await.set_pinned(&path, pinning) {
                                Ok(true) => eprintln!(
                                    "[{} {path}]",
                                    if pinning { "pinned" } else { "unpinned" }
                                ),
                                Ok(false) => eprintln!(
                                    "[{path} is already {}]",
                                    if pinning { "pinned" } else { "not pinned" }
                                ),
                                Err(e) => eprintln!("[{e:#}]"),
                            }
                        }
                    }
                }
                // The inspector runs no model, so it is as cheap in chat
                // as it is in the TUI; the window comes from the catalog
                // the same way the status line's does.
                "context" => {
                    let r = rt.agent.context_report().await;
                    let limit = rt.catalog.windows().get(&rt.agent.model_key()).copied();
                    if let Some(n) = r.compacted {
                        println!("compacted: {n} messages folded away and out of view");
                    }
                    for b in &r.blocks {
                        println!("{:<10} {:>8}c  {}", b.kind, b.chars, b.label);
                    }
                    println!(
                        "{:<10} {:>8}c  {} blocks · {}c of it tool guidance · ctx {}",
                        "total",
                        r.chars,
                        r.blocks.len(),
                        r.guidance_chars,
                        eidolon_core::usage::context_line(r.tokens, limit),
                    );
                }
                // Worth wiring here rather than falling through to "the
                // TUI has it": a long-running chat on the Claude backend is
                // exactly where a backend session accumulates output the
                // branch never stored.
                "fresh" => match rt.agent.fresh_eyes().await {
                    Ok(f) => eprintln!("[{f}]"),
                    Err(e) => eprintln!("[{e:#}]"),
                },
                "log" => {
                    let s = rt.agent.session().lock().await;
                    for r in s.branch() {
                        println!("#{:<4} {}", r.id, term::record_line(r));
                    }
                }
                // Wired here rather than falling through to "it acts on
                // the screen", which would be a lie: the switch is an
                // atomic shared with the hook, and it is only the *badge*
                // that needs a status line. Chat has no badge, so it says
                // so on every throw and nowhere else — which is the one
                // place this differs from the TUI.
                "yolo" => match (&rt.yolo, arg.trim()) {
                    (None, _) => {
                        eprintln!("[there is no gate here to bypass ([policy] enabled = false)]")
                    }
                    (Some(y), "on") | (Some(y), "yes") => {
                        y.set(true);
                        eprintln!("[yolo: every question is answered yes; refusals still refuse]");
                    }
                    (Some(y), "off") | (Some(y), "no") => {
                        y.set(false);
                        eprintln!("[yolo off: the gate asks again]");
                    }
                    (Some(y), "") => match y.toggle() {
                        true => eprintln!(
                            "[yolo: every question is answered yes; refusals still refuse]"
                        ),
                        false => eprintln!("[yolo off: the gate asks again]"),
                    },
                    (Some(_), other) => eprintln!("[yolo takes on or off, not {other}]"),
                },
                other => match eidolon_tui::command::lookup(other) {
                    Some(c) if c.run == eidolon_tui::command::Run::Ui => {
                        eprintln!("[:{other} acts on the screen; it needs the TUI]")
                    }
                    Some(_) => eprintln!("[:{other} is not wired in chat yet; the TUI has it]"),
                    None => {
                        let near: Vec<String> = eidolon_tui::command::matches(other)
                            .iter()
                            .take(3)
                            .map(|c| format!(":{}", c.name))
                            .collect();
                        if near.is_empty() {
                            eprintln!("[unknown command :{other}; :help]");
                        } else {
                            eprintln!(
                                "[unknown command :{other}; did you mean {}? :help lists them]",
                                near.join(", ")
                            );
                        }
                    }
                },
            }
            continue;
        }
        let t = CancellationToken::new();
        *current.lock().unwrap() = Some(t.clone());
        // Whatever was staged goes with this line and is gone afterwards —
        // an attachment belongs to the next message sent, not to the
        // session.
        let mut blocks: Vec<ContentBlock> = staged.drain(..).map(|a| a.block()).collect();
        blocks.push(ContentBlock::text(line));
        let out = rt.agent.run_turn(blocks, t).await;
        *current.lock().unwrap() = None;
        match out {
            Ok(o) => report(rt, &o),
            Err(e) => eprintln!("\n[error] {e:#}"),
        }
    }
    printer.abort();
    let session = rt.agent.session().lock().await;
    eprintln!(
        "[session {} · {}]",
        session.path().display(),
        usage_summary(rt, &session)
    );
    Ok(())
}

#[allow(dead_code)]
fn _assert_event_is_send(_: Event) {}
