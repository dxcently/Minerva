//! The two loops: the UI thread (terminal, script, state) and the async
//! driver (agent, bus, turns, commands), and the channels between them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton,
    MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use eidolon_core::agent::{Agent, TurnOutcome};
use eidolon_core::event::Event;
use eidolon_core::message::ContentBlock;
use eidolon_core::session::{Record, RecordKind, Session};
use eidolon_core::tool::CallOrigin;
use eidolon_core::user::Choice;

use crate::command::{self, Run};
use crate::edit::{Buffer, Mode};
use crate::jump::{Jump, Spot, Step};
use crate::keys::{Keys, Resolved};
use crate::render::Renderer;
use crate::script::{Action, UiScript};
use crate::search::{Dir, Search};
use crate::state::{Dialog, DialogKind, Entry, Level, MiniKind, PickAction, Selection, UiState};

/// Core → UI.
pub enum UiMsg {
    Tree(crate::vault::Tree),
    /// A parked wait's watcher sampled a change: the checklist ticks.
    /// Read straight off the parks registry — never journaled, never on
    /// the bus — because ticks are the polling made visible, and the log
    /// keeps the arm and the fire.
    ParkTicked(Vec<eidolon_core::wait::ParkTick>),
    VaultNote { id: u64, result: Result<crate::vault::Note, String> },
    Bus(Event),
    Dialog(DialogRequest),
    /// Open a single line of free text — a credential, masked as typed —
    /// answered the way a `Pick` row is: `run` with `key` and the typed
    /// text handed to it, as `Action::Command`. Never journaled and never
    /// on the `:` line: the value exists only in the dialog's own field
    /// and in the command it commits to.
    Ask {
        title: String,
        masked: bool,
        run: String,
        key: String,
    },
    /// Open a picker. [`PickAction`] says what enter does with the choice.
    Pick {
        title: String,
        items: Vec<PickItem>,
        pick: PickAction,
        current: Option<String>,
    },
    TurnDone(Result<TurnOutcome, String>),
    /// A word for the status line: what a command came to. The driver's
    /// [`UiState::info`], and as passing. See [`crate::state::Notice`].
    Info(String),
    /// A line for the transcript: something that happened to the
    /// conversation, which the log will say again on resume. The
    /// driver's [`UiState::note`], and as rare.
    Note(String),
    /// A page to read — the driver's [`DialogKind::Page`]. For what is
    /// too long to be a notice and is not the conversation either.
    Page {
        title: String,
        text: String,
    },
    /// The context inspector's report, formatted on the UI thread because
    /// the percentage needs the model's window and the driver has no
    /// catalog. The driver measures; the UI draws.
    Context(Box<eidolon_core::agent::ContextReport>),
    ModelChanged(String),
    /// The driver has moved to a different session log; the UI is now
    /// about that one. Carries the new branch rather than a signal to go
    /// and read it, because the session is behind the driver's lock and
    /// the UI thread has no business taking it.
    Resumed {
        path: String,
        cwd: String,
        model: String,
        branch: Vec<Record>,
        pins: Vec<String>,
        /// A session born a moment ago by `:new`, not picked up from the
        /// log directory: no one resumed it, so it wears no resumed row —
        /// the start screen is what it opens on, and a note would only
        /// surface above the first message later.
        fresh: bool,
    },
    /// The files pinned into every turn, as they now stand.
    ///
    /// Sent when they change and once at startup, so that `:unpin` can
    /// complete off them: pins are session state and the session is
    /// behind the driver's lock, and a completion popup rebuilt on every
    /// keystroke cannot go and ask. See [`crate::command::Fill`].
    Pins(Vec<String>),
    /// The vault's personas, as `(name, path)`, so `:persona` can complete
    /// off them — [`Pins`](Self::Pins)' arrangement, for the same reason.
    ///
    /// Warmed on a task of its own once the driver is up, and refreshed
    /// whenever the picker reads the vault. It is the one completion
    /// source behind a *network* call rather than behind the driver's
    /// lock, which is why it is warmed off the startup path entirely: a
    /// vault that is slow, or absent, must cost the first frame nothing.
    /// Until it arrives `:persona` completes to nothing and its picker
    /// still works, because the picker asks the vault itself.
    Personas(Vec<(String, String)>),
    /// What the subscription behind the model in play has left, as the
    /// driver just read it — see [`refresh_quota`] for when, and
    /// [`crate::usage::QuotaReading`] for what the page does with it.
    Quota(crate::usage::QuotaReading),
    /// End the UI: the driver is finishing on its own — today, because the
    /// session was asked to quiesce and has reached its boundary — rather
    /// than because a key or a command asked to leave. The terminal is
    /// restored and the driver's farewell is printed to stderr behind it,
    /// so the operator sees where their session went even though the frame
    /// that drew it is gone with the alternate screen.
    Quit,
}

#[derive(Clone, Debug)]
pub struct PickItem {
    pub key: String,
    pub label: String,
    pub description: Option<String>,
    /// The model picker's ordering weight; zero everywhere else.
    pub weight: i64,
}

pub struct DialogRequest {
    pub kind: DialogKind,
    pub prompt: String,
    pub options: Vec<Choice>,
    pub reply: tokio::sync::oneshot::Sender<Option<String>>,
}

/// UI → core.
#[derive(Debug)]
pub enum Cmd {
    VaultRead { id: u64, target: String, from: Option<String> },
    Send(Utterance),
    Cancel,
    Command {
        name: String,
        arg: String,
    },
    /// Resolve `text` on one named dispatch surface and run what it makes.
    /// Separate from `Command` because the surface is UI state — the mode
    /// is aimed before the line is typed — and the driver has no way to
    /// ask what it was pointed at.
    Dispatch {
        surface: String,
        text: String,
    },
    /// Read the subscription's quota again. Sent when the usage panel
    /// opens on a reading older than [`crate::state::QUOTA_STALE_MS`], or
    /// with none: the driver refreshes after every turn on its own, so
    /// this is for the reading that went stale while nothing ran.
    Quota,
    Quit,
}

/// One thing the operator said: the words, and whatever was attached to
/// them.
///
/// A `String` until images existed, and it had to stop being one for the
/// steer's sake more than the send's. A message composed with a screenshot
/// while a turn is in flight steers that turn, and a steer of plain
/// strings would have dropped the screenshot on the floor between the
/// keystroke and the delivery — the operator would have watched the
/// attachment leave the prompt and never arrive.
#[derive(Clone, Default, PartialEq)]
pub struct Utterance {
    pub text: String,
    pub images: Vec<ContentBlock>,
}

/// An utterance debugs as the words it carries — and, when it carries
/// pictures too, says how many.
///
/// Derived, it printed a megabyte of base64 per attached image, which is
/// not a thing to have in a log line, a test failure or a panic message.
/// The image-free form is the bare quoted string, which is also what this
/// was before images existed.
impl std::fmt::Debug for Utterance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.text)?;
        match self.images.len() {
            0 => Ok(()),
            1 => write!(f, " +1 image"),
            n => write!(f, " +{n} images"),
        }
    }
}

impl Utterance {
    pub fn text(text: impl Into<String>) -> Utterance {
        Utterance {
            text: text.into(),
            images: Vec::new(),
        }
    }

    /// The content blocks, images first.
    ///
    /// Images lead because both wires read a message in order and both
    /// answer better when the question follows the picture it is about —
    /// and because it is the order the transcript draws, so what the model
    /// is sent and what the operator sees are the same sequence.
    pub fn blocks(self) -> Vec<ContentBlock> {
        let mut out = self.images;
        if !self.text.is_empty() {
            out.push(ContentBlock::text(self.text));
        }
        out
    }
}

pub struct TuiOptions {
    /// Script source; `None` uses the built-in default.
    pub ui_script: Option<String>,
    /// Where that source lives, so `reload_ui` can read it again. `None`
    /// makes a reload recompile whatever was first loaded.
    pub ui_script_path: Option<PathBuf>,
    /// The model catalog, for `model` and its picker.
    pub catalog: Option<Arc<eidolon_providers::Catalog>>,
    /// Working directory and scratch dir, for building a swapped-in backend.
    pub cwd: std::path::PathBuf,
    pub scratch: std::path::PathBuf,
    /// Where the session logs live, for `sessions` and the pickers over it.
    pub sessions_dir: std::path::PathBuf,
    /// `[images] inline`: `auto`, `off`, `sixel` or `kitty`.
    pub inline_images: String,
    /// `[transcript] fold`: whether a finished run of calls folds
    /// mid-turn (`live`) or the turn holds itself open until it settles
    /// (`settle`, the default). See [`crate::state::Fold`].
    pub fold: crate::state::Fold,
    /// This session's registration among its peers, when the swarm is on.
    /// Drives the roster, the busy flag other sessions read, and the
    /// title they see.
    pub swarm: Option<Arc<eidolon_swarm::Presence>>,
    /// Rings when a peer has left a message. `None` when the swarm is off
    /// or the socket would not bind.
    pub doorbell: Option<mpsc::UnboundedReceiver<()>>,
    /// The verba palette, for dispatch mode. `None` — or an empty one —
    /// leaves the mode configured but refusing to open, which is what an
    /// operator with no `[[verba.bundles]]` should see.
    pub palette: Option<Arc<eidolon_verba::Palette>>,
    /// The blanket-yes switch, shared with the policy hook in the
    /// dispatcher. `None` when the classifier is off, which is what makes
    /// `:yolo` say there is no gate here rather than drawing a badge over
    /// a session that never had one.
    pub yolo: Option<eidolon_core::yolo::Switch>,
    /// `[terminal]`: how the `T` chords open another window. Default —
    /// every field empty — is not "disabled" but "work it out on the
    /// first press", which is where the `PATH` scan belongs. See
    /// [`crate::launch`].
    pub launcher: crate::launch::Launcher,
}

/// Run the TUI to completion. The caller has already built the agent with
/// a [`crate::TuiUser`] made from `ui_tx` (so `choices_user` reaches us).
pub async fn run(
    agent: Arc<Agent>,
    ui_tx: mpsc::UnboundedSender<UiMsg>,
    ui_rx: mpsc::UnboundedReceiver<UiMsg>,
    opts: TuiOptions,
) -> anyhow::Result<()> {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Cmd>();

    // Seed the UI state from the session.
    let (session_path, cwd, branch_state) = {
        let s = agent.session().lock().await;
        let mut st = UiState::new(
            agent.model_key(),
            s.path().display().to_string(),
            String::new(),
        );
        // The status line's denominator. Read here because this is the
        // last place that holds both the catalog and the state, and it is
        // a walk over definitions already in memory — no provider is
        // asked anything, so the first frame waits on nothing for it.
        st.windows = opts
            .catalog
            .as_ref()
            .map(|c| c.windows())
            .unwrap_or_default();
        // And the rates, on the same arrangement, so a settling turn can
        // be priced at the rates of the model it ran on.
        st.costs = opts.catalog.as_ref().map(|c| c.costs()).unwrap_or_default();
        // And the keys themselves, for what `:model` completes against.
        // The same walk, over the same definitions already in memory:
        // nothing is asked of a provider here either.
        st.model_keys = opts
            .catalog
            .as_ref()
            .map(|c| c.models().into_iter().map(|m| (m.key, m.name)).collect())
            .unwrap_or_default();
        // And every provider `:login` could reach, the same walk again.
        st.provider_keys = opts
            .catalog
            .as_ref()
            .map(|c| {
                c.providers()
                    .iter()
                    .filter(|p| p.def.token_secret.is_some())
                    .map(|p| p.def.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        st.sessions_dir = opts.sessions_dir.clone();
        st.pins = s.pins();
        st.seed(&s.branch());
        // The effective cwd, not the raw start record: a session opened
        // with a reopen-time override (`Session::open_with_cwd` — a
        // projection's shape) shows where it runs, which is the one thing
        // this line is for.
        let cwd = s.cwd().unwrap_or_default();
        (s.path().display().to_string(), cwd, st)
    };
    let mut state = branch_state;
    state.session_path = session_path;
    state.cwd = cwd;

    // Bus → UI.
    {
        let mut rx = agent.dispatcher().bus().subscribe();
        let tx = ui_tx.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if tx.send(UiMsg::Bus(ev)).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }

    // Dispatch mode is aimed at a surface before a word of it is typed,
    // so the UI thread needs the surface *names*: resolving is async and
    // lives on the driver's side of the channel, and the mode has to know
    // whether it can open at all before it asks anything.
    let surfaces = opts
        .palette
        .as_ref()
        .map(|p| p.names().iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    state.dispatch = crate::dispatch::Dispatch::new(surfaces);
    // Whether this terminal draws pixels, and how big a cell is. Asked
    // once, here: both are properties of the terminal the session was
    // launched in, neither changes under it, and the layout walk reads
    // them on every frame.
    state.image_protocol = crate::image::Protocol::resolve(&opts.inline_images);
    // When the transcript folds a finished run: mid-turn, or once at the
    // settle. Read once where the config is; the view only asks the state.
    state.fold = opts.fold;
    // What the config said, verbatim. Resolving it — finding the
    // emulator, finding the editor — is the first `T` press's job, not
    // the first frame's.
    state.launcher = opts.launcher.clone();
    state.yolo = opts.yolo.clone();
    state.cell_size = crate::image::cell_size();

    let script_src = opts
        .ui_script
        .clone()
        .unwrap_or_else(|| crate::DEFAULT_UI.to_string());
    let script_path = opts.ui_script_path.clone();
    eidolon_core::boot::mark("ui state");
    let ui_thread = {
        let cmd_tx = cmd_tx.clone();
        std::thread::Builder::new()
            .name("eidolon-ui".into())
            .spawn(move || ui_loop(state, ui_rx, cmd_tx, script_src, script_path))?
    };

    let ctx = DriverCtx {
        catalog: opts.catalog,
        cwd: opts.cwd,
        scratch: opts.scratch,
        sessions_dir: opts.sessions_dir,
        palette: opts.palette,
        swarm: opts.swarm,
    };
    let farewell = driver(agent, ui_tx, cmd_rx, opts.doorbell, ctx).await?;
    let _ = ui_thread.join();
    // After the alternate screen is gone: the goodbye's transcript line went
    // with it, and an operator whose session was asked to leave from
    // somewhere else would otherwise see their prompt come back and nothing
    // else at all.
    if let Some(text) = farewell {
        eprintln!("\n{text}");
    }
    Ok(())
}

struct DriverCtx {
    catalog: Option<Arc<eidolon_providers::Catalog>>,
    palette: Option<Arc<eidolon_verba::Palette>>,
    cwd: std::path::PathBuf,
    scratch: std::path::PathBuf,
    sessions_dir: std::path::PathBuf,
    swarm: Option<Arc<eidolon_swarm::Presence>>,
}

/// The async side. Turns run as tasks so `Cancel` can land mid-turn; a
/// `Send` during a turn steers it (`Agent::steer`), so the model reads
/// the message beside the results of the calls it has in flight, before
/// its next call, rather than waiting out a settle that may be many
/// calls away.
///
/// What this loop still does is pick up after a turn that did *not*
/// settle: a cancelled or failed turn leaves any steer undelivered in
/// the agent, where it waits for the next turn and rides that one's
/// first request.
/// The vault's personas as `(name, path)` rows, or `None` when there is no
/// vault or it would not answer. Silent on failure by design: this feeds a
/// completion popup, and a vault that is down should cost the operator a
/// convenience, never a notice they did not ask for.
async fn list_personas(agent: &Agent) -> Option<Vec<(String, String)>> {
    let source = agent.persona_source()?;
    match eidolon_core::persona::list(source.as_ref()).await {
        Ok(list) => Some(list.into_iter().map(|p| (p.name, p.path)).collect()),
        Err(e) => {
            tracing::warn!("listing the vault's personas: {e:#}");
            None
        }
    }
}

async fn driver(
    agent: Arc<Agent>,
    ui_tx: mpsc::UnboundedSender<UiMsg>,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    mut doorbell: Option<mpsc::UnboundedReceiver<()>>,
    ctx: DriverCtx,
) -> anyhow::Result<Option<String>> {
    let mut turn: Option<(
        CancellationToken,
        tokio::task::JoinHandle<anyhow::Result<TurnOutcome>>,
    )> = None;
    // The goodbye, when the driver is leaving because the session quiesced
    // rather than because the operator asked to quit. Handed back to `run`
    // to print once the terminal is its own again.
    let mut farewell: Option<String> = None;
    // A park resolving wakes this session the way a peer's message does, and
    // through a ring of its own: the harness answering a condition, rather
    // than someone talking.
    let mut parks_rx = agent.parks().events();

    // Warm what `:persona` completes against, off to one side. A task
    // rather than a step: this is the only completion source that costs a
    // round trip to a server, and the driver has a crashed turn to finish
    // and a keyboard to start serving.
    if agent.has_persona_source() {
        let (a, tx) = (agent.clone(), ui_tx.clone());
        tokio::spawn(async move {
            if let Some(list) = list_personas(&a).await {
                let _ = tx.send(UiMsg::Personas(list));
            }
        });
    }

    // And what the plan has left, before the first turn spends any of
    // it. Off the driver on a thread of its own, so the prompt is up
    // while the account answers.
    refresh_quota(&agent, &ui_tx, &ctx);

    // Finish an unsettled turn from a crash before accepting input — and
    // only one that is unfinished: a branch that already ends on the
    // model's reply (a projection from a harness that journals no boundary)
    // shows as it stands rather than spending a duplicate continuation.
    if agent.session().lock().await.needs_finishing() {
        let _ = ui_tx.send(UiMsg::Info("finishing an unsettled turn".into()));
        let t = CancellationToken::new();
        let a = agent.clone();
        turn = Some((
            t.clone(),
            tokio::spawn(async move { a.continue_turn(t).await }),
        ));
    }

    loop {
        // What the roster shows about this session. Cheap: the write only
        // happens when the answer changes.
        if let Some(p) = &ctx.swarm {
            p.set_busy(turn.is_some());
        }
        let done = async {
            match &mut turn {
                Some((_, h)) => h.await,
                None => std::future::pending().await,
            }
        };
        let rang = async {
            match &mut doorbell {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        let park_ring = async {
            let _ = parks_rx.recv().await;
        };
        tokio::select! {
            // A peer has left a message. The message itself is already in
            // the inbox — this is only the ring — and a session mid-turn
            // takes the mail marked to wake at the next safe point inside
            // the loop, so the only reason to act on the ring here is an
            // idle session: draining now would journal a user-shaped
            // record between an assistant's `tool_use` and its results,
            // which turns every outstanding call into a synthetic
            // interruption.
            _ = rang => {
                if turn.is_none() {
                    // A ring is either mail or a quiesce, and the doorbell
                    // cannot tell them apart — so ask, before draining: a
                    // session that has been asked to settle must not start
                    // the turn a message would have bought.
                    if let Some(text) = close_if_quiesced(&agent, &ctx, &ui_tx).await {
                        farewell = Some(text);
                        break;
                    }
                    turn = collect(&agent, &ui_tx).await;
                }
            }
            // A park has resolved. Only an idle session acts on it: a turn
            // in flight takes the fires itself at its next safe point, and
            // journaling one here would land between a `tool_use` and its
            // results.
            _ = park_ring => {
                // Ticks first, and whatever the session is doing: a row
                // that moved is a view update, never a turn.
                let ticks = agent.parks().take_ticks();
                if !ticks.is_empty() {
                    let _ = ui_tx.send(UiMsg::ParkTicked(ticks));
                }
                if turn.is_none() {
                    if let Some(text) = close_if_quiesced(&agent, &ctx, &ui_tx).await {
                        farewell = Some(text);
                        break;
                    }
                    if !agent.take_fires().await.is_empty() {
                        turn = Some(start_continue(&agent));
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Cmd::Send(said) => {
                        if let Some(p) = &ctx.swarm {
                            let first = said.text.clone();
                            p.update(|m| {
                                if m.title.trim().is_empty() {
                                    m.title = first.chars().take(72).collect();
                                }
                            });
                        }
                        if turn.is_some() {
                            // Steers, not queues: the model reads this
                            // beside the results of the calls it has in
                            // flight, before its next call, instead of
                            // after a settle that may be many calls away.
                            // The core journals it when the model reads it
                            // and publishes `UserMessage` then, which is
                            // when the transcript stamps its record.
                            agent.steer(said.blocks());
                            let _ = ui_tx.send(UiMsg::Info(
                                "steering: the model reads it before its next call".into(),
                            ));
                        } else {
                            turn = Some(start_turn(&agent, said.blocks()));
                        }
                    }
                    Cmd::Cancel => {
                        if let Some((t, _)) = &turn {
                            t.cancel();
                        }
                    }
                    Cmd::Command { name, arg } => {
                        let before = agent.session().lock().await.path().to_path_buf();
                        let model_before = agent.model_key();
                        if let Some(msg) = command(&agent, &ui_tx, &ctx, &name, &arg, turn.is_some()).await {
                            let _ = ui_tx.send(msg);
                        }
                        // A command that moved the model — `model`, or a
                        // `resume` adopting a log written on another —
                        // may have moved it onto another account. Asked
                        // by comparing keys rather than by naming the
                        // commands, for the reason the session check
                        // below is.
                        if agent.model_key() != model_before {
                            // And the roster the peers read says so: it is
                            // registered with the model in hand at launch
                            // and nothing moved it since, so a neighbour
                            // went on reading the launch model for the rest
                            // of the session.
                            if let Some(p) = &ctx.swarm {
                                p.set_model(&agent.model_key());
                            }
                            refresh_quota(&agent, &ui_tx, &ctx);
                        }
                        // A command that moved us into another log — only
                        // `resume` and `new_session` can — may have landed
                        // on a turn that never settled. Finish it, exactly
                        // as opening that log at startup would; asking
                        // whether the session changed rather than which
                        // command ran keeps the two from having to know
                        // about each other.
                        if turn.is_none() {
                            let session = agent.session().lock().await;
                            if session.path() != before && session.needs_finishing() {
                                drop(session);
                                let _ = ui_tx.send(UiMsg::Info("finishing an unsettled turn".into()));
                                let t = CancellationToken::new();
                                let a = agent.clone();
                                turn = Some((t.clone(), tokio::spawn(async move { a.continue_turn(t).await })));
                            }
                        }
                    }
                    // A dispatch is a tool call the operator wrote
                    // directly, and it journals into the session like any
                    // other, so it waits for a turn rather than
                    // interleaving its records with one. Resolving and
                    // running go to a task: the classifier is
                    // milliseconds but the call it makes need not be, and
                    // the loop has keys to keep answering.
                    Cmd::Dispatch { surface, text } => {
                        if turn.is_some() {
                            let _ = ui_tx.send(UiMsg::Info("wait for the turn to finish before dispatching".into()));
                        } else {
                            match &ctx.palette {
                                Some(p) => spawn_dispatch(&agent, &ui_tx, p.clone(), Some(surface), text),
                                None => {
                                    let _ = ui_tx.send(UiMsg::Info(NO_BUNDLES.into()));
                                }
                            }
                        }
                    }
                    Cmd::VaultRead { id, target, from } => {
                        let source = agent.persona_source();
                        let tx = ui_tx.clone();
                        tokio::spawn(async move {
                            let _ = tx.send(crate::vault::fetch_message(source, id, target, from).await);
                        });
                    }
                    Cmd::Quota => refresh_quota(&agent, &ui_tx, &ctx),
                    Cmd::Quit => {
                        if let Some((t, _)) = &turn {
                            t.cancel();
                        }
                        break;
                    }
                }
            }
            res = done => {
                turn = None;
                let msg = match res {
                    Ok(r) => r.map_err(|e| format!("{e:#}")),
                    Err(e) => Err(format!("turn task failed: {e}")),
                };
                let _ = ui_tx.send(UiMsg::TurnDone(msg));
                // The turn spent some of the window; read what is left.
                // Settled, cancelled or failed alike — a cancelled turn
                // spent too.
                refresh_quota(&agent, &ui_tx, &ctx);
                // The boundary this turn just reached is the one a quiesce
                // was waiting for, if there was one. Before `collect`, and
                // never after: a message here would buy the very turn the
                // quiesce exists to stop.
                if let Some(text) = close_if_quiesced(&agent, &ctx, &ui_tx).await {
                    farewell = Some(text);
                    break;
                }
                // A steer typed into a turn that then failed or was
                // cancelled is still waiting in the agent; it rides the
                // next turn's first request, so there is nothing to pick
                // up here. Peers are the ones with somewhere to be:
                if let Some(next) = collect(&agent, &ui_tx).await {
                    // Messages that rang while this turn was running have
                    // been waiting for exactly this moment. Without this
                    // they would sit in the inbox until the operator said
                    // something — which is most of the time, and is the
                    // difference between a swarm and a group of sessions
                    // that happen to share a directory.
                    turn = Some(next);
                }
            }
        }
    }
    Ok(farewell)
}

/// Close the session if a quiesce has reached its boundary: settle an idle
/// one, draw the goodbye, print it to the terminal, and leave the roster.
///
/// `Some(goodbye)` means the session has finished and the driver should stop.
/// `None` is the ordinary case — nothing was asked, or a turn is still in
/// flight and the boundary has not arrived.
///
/// Nothing here forces anything: an unsettled session goes on as it was, and
/// `Ok(false)` from the poll is the timeout's answer, not a failure. The
/// marker itself is written by [`Agent::poll_quiesce`] for an idle session
/// and by the loop at a settle for a busy one.
async fn close_if_quiesced(
    agent: &Arc<Agent>,
    ctx: &DriverCtx,
    ui_tx: &mpsc::UnboundedSender<UiMsg>,
) -> Option<String> {
    match agent.poll_quiesce().await {
        Ok(true) => {}
        Ok(false) => return None,
        Err(e) => {
            // The marker could not be written, so the quiesce is not
            // settled and the session goes on — saying so rather than
            // closing over a journal that does not record why.
            let _ = ui_tx.send(UiMsg::Info(format!(
                "could not journal the quiesce marker: {e:#}"
            )));
            return None;
        }
    }
    let destination = agent
        .quiesce_request()
        .map(|q| q.destination)
        .unwrap_or_default();
    let id = ctx.swarm.as_ref().map(|p| p.id().to_string());
    let text = eidolon_core::quiesce::goodbye(&destination, id.as_deref());
    // The transcript line is drawn in the last frame; the copy `run` prints
    // is what survives the alternate screen.
    let _ = ui_tx.send(UiMsg::Note(text.clone()));
    // Off the roster before anything else looks: a session that has
    // projected is deliberately dark, because its presence belongs to the
    // venue it moved to and a neighbour still sending here would be mailing
    // a journal that has stopped.
    if let Some(p) = &ctx.swarm {
        p.deregister();
    }
    let _ = ui_tx.send(UiMsg::Quit);
    Some(text)
}

/// Read what the subscription behind the model in play has left, off the
/// driver, and hand the UI the reading with the time it was taken.
///
/// Nothing is sent when the provider has no `quota()`: the panel draws
/// the section only for a reading it holds, so a metered provider — or
/// the mock — has no section rather than an empty one. The read is a
/// network round trip (up to `http_get`'s timeout when the endpoint is
/// down), so it runs on a blocking thread and the driver goes on serving
/// keys; the reading is keyed by provider, and the UI draws it only while
/// the model in play is that provider's, so a `:model` across providers
/// while one is in flight cannot land one account's window under
/// another's name.
fn refresh_quota(agent: &Arc<Agent>, ui_tx: &mpsc::UnboundedSender<UiMsg>, ctx: &DriverCtx) {
    let key = agent.model_key();
    let Some(script) = ctx.catalog.as_ref().and_then(|c| c.quota_source(&key)) else {
        return;
    };
    let Some(provider) = crate::usage::provider_of(&key).map(str::to_string) else {
        return;
    };
    let tx = ui_tx.clone();
    tokio::task::spawn_blocking(move || {
        let result = script.quota().map_err(|e| format!("{e:#}"));
        let _ = tx.send(UiMsg::Quota(crate::usage::QuotaReading {
            provider,
            read_ms: crate::usage::now_ms(),
            result,
        }));
    });
}

/// Take delivery of whatever peers have left, and decide whether any of
/// it is worth a turn.
///
/// Called at the two moments a turn is *not* running: a doorbell while
/// idle, and the end of a turn. A turn in flight takes the mail marked to
/// wake itself, at the same safe points as a steer, so by the time this
/// runs after a settle the waking inbox is usually already empty — the
/// notes that did not ask to wake are exactly what is left for it. It
/// still matters here because a message can land in the instant before
/// the settle is journaled — and because a session mid-turn is not the
/// only thing that can ring: an idle one can too.
///
/// Whether a message *wakes* one is the park's question, not this
/// function's: `Parks::admits` is a park's `wake` level — 0 nothing, 1
/// mail marked to wake, 2 anything — and mail it refuses is still
/// delivered, because the drain above is what journals it.
async fn collect(
    agent: &Arc<Agent>,
    ui_tx: &mpsc::UnboundedSender<UiMsg>,
) -> Option<(
    CancellationToken,
    tokio::task::JoinHandle<anyhow::Result<TurnOutcome>>,
)> {
    // Each message is drawn by the `PeerMessage` event this publishes, so
    // nothing is silently swallowed even when nothing is woken.
    let messages = agent.drain_peers().await;
    let asked: Vec<String> = messages
        .into_iter()
        .filter(|m| agent.parks().admits(m.wake))
        .map(|m| m.from)
        .collect();
    if asked.is_empty() {
        return None;
    }
    let _ = ui_tx.send(UiMsg::Note(format!("answering {}", asked.join(", "))));
    Some(start_continue(agent))
}

/// A turn with no user message of its own. The peer messages just
/// journaled are the last thing on the branch, so they are what it
/// answers — which is exactly what `continue_turn` is for.
fn start_continue(
    agent: &Arc<Agent>,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<anyhow::Result<TurnOutcome>>,
) {
    let t = CancellationToken::new();
    let a = agent.clone();
    let tok = t.clone();
    (t, tokio::spawn(async move { a.continue_turn(tok).await }))
}

fn start_turn(
    agent: &Arc<Agent>,
    blocks: Vec<ContentBlock>,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<anyhow::Result<TurnOutcome>>,
) {
    let t = CancellationToken::new();
    let a = agent.clone();
    let tok = t.clone();
    (
        t,
        tokio::spawn(async move { a.run_turn(blocks, tok).await }),
    )
}

/// The [`Run::Driver`] half of the vocabulary: the commands that need the
/// agent, the catalog or the session logs. What *is* a command is
/// declared once in [`crate::command`]; this is what each of those
/// entries does.
async fn command(
    agent: &Arc<Agent>,
    ui_tx: &mpsc::UnboundedSender<UiMsg>,
    ctx: &DriverCtx,
    name: &str,
    arg: &str,
    busy: bool,
) -> Option<UiMsg> {
    Some(match name {
        // No `steer` here any more: a message sent while a turn runs *is*
        // a steer now (see `Cmd::Send`), so the command was a second
        // spelling of the prompt's own act — and one that slipped past the
        // vault reader's no-sending rule, being a command rather than a
        // submit.
        "model" => {
            let Some(catalog) = &ctx.catalog else {
                return Some(UiMsg::Info("no model catalog configured".into()));
            };
            if arg.is_empty() {
                let items = catalog
                    .models()
                    .into_iter()
                    .map(|m| PickItem {
                        key: m.key.clone(),
                        label: m.name.clone(),
                        description: Some(m.describe()),
                        weight: m.weight,
                    })
                    .collect();
                UiMsg::Pick {
                    title: "model".into(),
                    items,
                    pick: PickAction::Run("model".into()),
                    current: Some(agent.model_key()),
                }
            } else {
                if busy {
                    return Some(UiMsg::Info(
                        "wait for the turn to finish before switching models".into(),
                    ));
                }
                match catalog.backend_for(arg, None, &ctx.cwd, &ctx.scratch) {
                    Ok((backend, key, claim)) => {
                        agent.set_backend(backend);
                        let id = eidolon_providers::catalog::model_id(&key).to_string();
                        match agent.set_model(key.clone(), id).await {
                            Ok(()) => {
                                journal_claim(agent, &claim).await;
                                UiMsg::ModelChanged(key)
                            }
                            Err(e) => UiMsg::Info(format!("{e:#}")),
                        }
                    }
                    Err(e) => UiMsg::Info(format!("{e:#}")),
                }
            }
        }
        // A credential never rides the `:` line: the picker names the
        // provider, a masked field takes the value, and the two meet
        // here as `provider\x01secret` — a seam only this arm ever
        // splits, so the value crosses the channel once and is gone.
        "login" => {
            let Some(catalog) = &ctx.catalog else {
                return Some(UiMsg::Info("no provider catalog configured".into()));
            };
            if let Some((provider, secret)) = arg.split_once('\u{1}') {
                let Some(p) = catalog.providers().iter().find(|p| p.def.name == provider) else {
                    return Some(UiMsg::Info(format!("no provider named `{provider}`")));
                };
                let Some(token_secret) = p.def.token_secret.clone() else {
                    return Some(UiMsg::Info(format!(
                        "`{provider}` has no token_secret to store a key under"
                    )));
                };
                match catalog.set_secret(&token_secret, secret, Some(provider.to_string())) {
                    Ok(()) => UiMsg::Info(format!("stored key for {provider}")),
                    Err(e) => UiMsg::Info(format!("{e:#}")),
                }
            } else if arg.is_empty() {
                let items = catalog
                    .providers()
                    .iter()
                    .filter_map(|p| {
                        p.def.token_secret.as_ref().map(|secret| PickItem {
                            key: p.def.name.clone(),
                            label: p.def.name.clone(),
                            description: Some(if catalog.secret_present(secret) {
                                "key set".into()
                            } else {
                                "no key".into()
                            }),
                            weight: 0,
                        })
                    })
                    .collect::<Vec<_>>();
                if items.is_empty() {
                    return Some(UiMsg::Info(
                        "no provider declares a token_secret to log in to".into(),
                    ));
                }
                UiMsg::Pick {
                    title: "login".into(),
                    items,
                    pick: PickAction::Run("login".into()),
                    current: None,
                }
            } else {
                let provider = arg.to_string();
                let Some(p) = catalog.providers().iter().find(|p| p.def.name == provider) else {
                    return Some(UiMsg::Info(format!("no provider named `{provider}`")));
                };
                if p.def.token_secret.is_none() {
                    return Some(UiMsg::Info(format!(
                        "`{provider}` has no token_secret to store a key under"
                    )));
                }
                UiMsg::Ask {
                    title: format!(" {provider} · API key "),
                    masked: true,
                    run: "login".into(),
                    key: provider,
                }
            }
        }
        "fork" => {
            if busy {
                return Some(UiMsg::Info(
                    "wait for the turn to finish before forking".into(),
                ));
            }
            match arg.parse::<u64>() {
                Ok(id) => match agent.fork_at(id).await {
                    Ok(()) => UiMsg::Info(format!("head → #{id}; the next turn forks from there")),
                    Err(e) => UiMsg::Info(format!("{e:#}")),
                },
                Err(_) => UiMsg::Info(format!("fork takes a record id, not {arg:?}")),
            }
        }
        "tree" => {
            let s = agent.session().lock().await;
            UiMsg::Tree(tree_report(&s.tree()))
        }
        "compact" => {
            if busy {
                return Some(UiMsg::Info(
                    "wait for the turn to finish before compacting".into(),
                ));
            }
            let a = agent.clone();
            let tx = ui_tx.clone();
            tokio::spawn(async move {
                let msg = match a.compact(CancellationToken::new()).await {
                    Ok(true) => "compacted".to_string(),
                    Ok(false) => "nothing to compact".to_string(),
                    Err(e) => format!("compaction failed: {e:#}"),
                };
                let _ = tx.send(UiMsg::Info(msg));
            });
            return None;
        }
        // The ids come from the UI, which resolved them from the cursor;
        // the driver only writes them down. Several at once because a
        // trace selection can cover several blocks.
        "exclude" | "restore" => {
            let excluded = name == "exclude";
            let mut s = agent.session().lock().await;
            let mut n = 0;
            for id in arg.split(',').filter_map(|t| t.trim().parse::<u64>().ok()) {
                match s.set_excluded(id, excluded) {
                    Ok(true) => n += 1,
                    Ok(false) => {}
                    Err(e) => return Some(UiMsg::Info(format!("{e:#}"))),
                }
            }
            let what = if excluded { "struck" } else { "restored" };
            UiMsg::Info(match n {
                0 => format!("nothing to {}", if excluded { "strike" } else { "restore" }),
                1 => format!("{what} 1 record from the context"),
                n => format!("{what} {n} records from the context"),
            })
        }
        // The standing note. `-` clears rather than an empty argument,
        // because a bare `:note` is how you ask what it currently says and
        // those two must not be the same keystroke.
        "note" => {
            let arg = arg.trim();
            let mut s = agent.session().lock().await;
            match arg {
                "" => UiMsg::Info(match s.session_note() {
                    Some(n) => format!("session note: {n}"),
                    None => "no session note; :note TEXT sets one".into(),
                }),
                "-" => match s.set_session_note("") {
                    Ok(()) => UiMsg::Info("session note cleared".into()),
                    Err(e) => UiMsg::Info(format!("{e:#}")),
                },
                text => match s.set_session_note(text) {
                    Ok(()) => UiMsg::Info(format!(
                        "session note set ({} chars); it is in every turn from here",
                        text.chars().count()
                    )),
                    Err(e) => UiMsg::Info(format!("{e:#}")),
                },
            }
        }
        // Bare, the vault becomes the picker, exactly as `:model` opens the
        // catalog. `-` clears, as it does on `:note`.
        //
        // A pin is **resolved before it is accepted**, for the reason the
        // `:pin` below refuses an unreadable path: a persona that names no
        // note is not an error once and then over with, it is a session
        // that quietly runs unpersonated for the rest of its life, and the
        // one moment the operator can act on it is the keystroke that set
        // it. The fetch it costs is the fetch the next turn was going to
        // make anyway.
        "persona" => {
            if !agent.has_persona_source() {
                return Some(UiMsg::Info(
                    "no vault configured; personas need [mneme] in config.toml".into(),
                ));
            }
            let source = agent.persona_source().expect("just checked");
            let arg = arg.trim();
            if arg.is_empty() {
                let current = agent.session().lock().await.persona();
                return Some(match eidolon_core::persona::list(source.as_ref()).await {
                    Ok(list) if list.is_empty() => UiMsg::Info(format!(
                        "no personas in the vault under {}",
                        eidolon_core::persona::PERSONA_FOLDER
                    )),
                    Ok(list) => {
                        // The read is done; let the completion popup have
                        // it too, rather than making it wait for the warm
                        // that may have failed while the vault was down.
                        let _ = ui_tx.send(UiMsg::Personas(
                            list.iter()
                                .map(|p| (p.name.clone(), p.path.clone()))
                                .collect(),
                        ));
                        let items = list
                            .into_iter()
                            .map(|p| PickItem {
                                key: p.name.clone(),
                                label: p.name,
                                description: Some(p.path),
                                weight: 0,
                            })
                            .collect();
                        UiMsg::Pick {
                            title: "persona".into(),
                            items,
                            pick: PickAction::Run("persona".into()),
                            current,
                        }
                    }
                    Err(e) => UiMsg::Info(format!("reading the vault's personas: {e:#}")),
                });
            }
            if arg == "-" {
                let mut s = agent.session().lock().await;
                return Some(match s.set_persona("") {
                    Ok(true) => UiMsg::Info(
                        "persona cleared; the next turn is in the harness's own voice".into(),
                    ),
                    Ok(false) => UiMsg::Info("no persona was pinned".into()),
                    Err(e) => UiMsg::Info(format!("{e:#}")),
                });
            }
            match eidolon_core::persona::resolve(source.as_ref(), arg).await {
                Err(e) => UiMsg::Info(format!("{e:#}")),
                Ok(r) if r.is_empty() => {
                    UiMsg::Info(format!("'{arg}' has no body to wear; not pinned"))
                }
                Ok(r) => {
                    let name = r.name.unwrap_or_else(|| arg.to_string());
                    // The memory note the pin also read, when it read one:
                    // only the pin is journaled, so this line is the one place
                    // the operator can see what a turn will actually carry.
                    let memory = r
                        .memory
                        .as_deref()
                        .map(|m| format!(" — with memory {m}"))
                        .unwrap_or_default();
                    let mut s = agent.session().lock().await;
                    match s.set_persona(arg) {
                        Ok(true) => UiMsg::Info(format!(
                            "wearing {name}{memory} ({} chars); it is in every turn from here",
                            r.prefix.chars().count()
                        )),
                        Ok(false) => UiMsg::Info(format!("already wearing {name}")),
                        Err(e) => UiMsg::Info(format!("{e:#}")),
                    }
                }
            }
        }
        // A path that does not resolve is refused at the keyboard rather
        // than accepted and reported to the model as unreadable on every
        // turn from now on.
        "pin" | "unpin" => {
            let arg = arg.trim();
            if arg.is_empty() && name == "pin" {
                let pins = agent.session().lock().await.pins();
                return Some(UiMsg::Info(if pins.is_empty() {
                    "nothing pinned; :pin PATH re-reads a file into every turn".into()
                } else {
                    format!("pinned:\n{}", pins.join("\n"))
                }));
            }
            if arg.is_empty() {
                return Some(UiMsg::Info(":unpin takes a path; :pin lists them".into()));
            }
            let path = eidolon_tools::image::expand_tilde(arg);
            let path = path.to_string_lossy().to_string();
            let pinning = name == "pin";
            if pinning && let Err(e) = std::fs::metadata(&path) {
                return Some(UiMsg::Info(format!("{path}: {e}")));
            }
            let mut s = agent.session().lock().await;
            let changed = s.set_pinned(&path, pinning);
            if matches!(changed, Ok(true)) {
                // What `:unpin` completes against. Sent rather than
                // asked for: the popup is on the other thread and is
                // rebuilt on every keystroke.
                let _ = ui_tx.send(UiMsg::Pins(s.pins()));
            }
            match changed {
                Ok(true) if pinning => {
                    UiMsg::Info(format!("pinned {path}; re-read at the start of every turn"))
                }
                Ok(true) => UiMsg::Info(format!("unpinned {path}")),
                Ok(false) if pinning => UiMsg::Info(format!("{path} is already pinned")),
                Ok(false) => UiMsg::Info(format!("{path} is not pinned")),
                Err(e) => UiMsg::Info(format!("{e:#}")),
            }
        }
        // Model-free by construction: `context_report` asks the same two
        // calls a turn asks and sends nothing, so opening it costs the
        // operator neither a token nor a wait. It is not guarded on
        // `busy` for that reason — looking at the context while a turn
        // runs is exactly when you want to.
        "context" => return Some(UiMsg::Context(Box::new(agent.context_report().await))),
        // Dropping the backend's own session is not a turn and costs
        // nothing, but it is still guarded on `busy`: a turn in flight
        // writes a fresh mark when it settles, so dropping one underneath
        // it would be undone a second later and read as a control that
        // does not work.
        "fresh" => {
            if busy {
                return Some(UiMsg::Info("wait for the turn to finish".into()));
            }
            match agent.fresh_eyes().await {
                Ok(f) => UiMsg::Info(f.to_string()),
                Err(e) => UiMsg::Info(format!("{e:#}")),
            }
        }
        // ------------------------------------------------------ sessions
        //
        // A session is a file in a flat directory named by the millisecond
        // it was created — the right shape for an append-only log and no
        // shape at all for finding one again. These two open the same
        // list; they differ only in what enter does with the row. Deleting
        // goes through [`PickAction::Edit`], as `set` and `unset` do: the
        // choice comes back as a half-written `:delete_session PATH` for
        // the operator to press enter on, and that second keystroke is the
        // confirmation, on a list whose highlighted row moves under the
        // fingers.
        "sessions" | "delete_session" => {
            let arg = arg.trim();
            let all = arg == "all";
            if name == "delete_session" && !arg.is_empty() && !all {
                let open = agent.session().lock().await.path().to_path_buf();
                return Some(UiMsg::Info(
                    match crate::sessions::delete(Path::new(arg), &open) {
                        Ok(msg) => msg,
                        Err(e) => format!("{e:#}"),
                    },
                ));
            }
            let cwd = agent.dispatcher().cwd();
            let open = agent.session().lock().await.path().to_path_buf();
            let list = crate::sessions::list(&ctx.sessions_dir, &cwd, all);
            if list.is_empty() {
                return Some(UiMsg::Info(format!(
                    "no sessions in {}{}",
                    crate::sessions::tilde(&ctx.sessions_dir.display().to_string()),
                    if all {
                        ""
                    } else {
                        "; `:sessions all` counts the empty ones and the other directories'"
                    }
                )));
            }
            // The model column is padded to the widest so the rows read
            // down as columns rather than across as sentences; everything
            // else is either fixed width already or last on the line.
            let width = list
                .iter()
                .map(|s| s.model.chars().count())
                .max()
                .unwrap_or(0);
            let items = list
                .iter()
                .map(|s| PickItem {
                    key: s.path.display().to_string(),
                    label: format!(
                        "{:>4}  {:>3} msg  {:<width$}  {}",
                        crate::sessions::ago(s.modified),
                        s.messages,
                        s.model,
                        s.first
                    ),
                    description: Some(s.description(&cwd)),
                    weight: 0,
                })
                .collect();
            let pick = if name == "delete_session" {
                PickAction::Edit("delete_session".into())
            } else {
                PickAction::Run("resume".into())
            };
            UiMsg::Pick {
                title: name.into(),
                items,
                pick,
                current: Some(open.display().to_string()),
            }
        }
        "resume" => {
            if busy {
                return Some(UiMsg::Info(
                    "wait for the turn to finish before moving to another session".into(),
                ));
            }
            let arg = arg.trim();
            if arg.is_empty() {
                return Some(UiMsg::Info(
                    ":resume takes a session path; press space r for the list".into(),
                ));
            }
            let path = PathBuf::from(arg);
            if path == agent.session().lock().await.path() {
                return Some(UiMsg::Info("that is the session you are in".into()));
            }
            match Session::open(&path) {
                Ok(session) => match adopt(agent, ctx, session).await {
                    Ok(msg) => msg,
                    Err(e) => UiMsg::Info(format!("{e:#}")),
                },
                Err(e) => UiMsg::Info(format!("{e:#}")),
            }
        }
        "new_session" => {
            if busy {
                return Some(UiMsg::Info(
                    "wait for the turn to finish before starting a new session".into(),
                ));
            }
            match new_session(agent, ctx).await {
                Ok(msg) => msg,
                Err(e) => UiMsg::Info(format!("{e:#}")),
            }
        }
        // The typed form. It has not entered a surface, so unlike dispatch
        // mode it asks the whole palette and takes the first bundle that
        // accepts — this is `chat`'s `:do`, in the TUI.
        "dispatch" => {
            if arg.trim().is_empty() {
                return Some(UiMsg::Info(
                    ":dispatch takes an utterance; ! opens the mode instead".into(),
                ));
            }
            if busy {
                return Some(UiMsg::Info(
                    "wait for the turn to finish before dispatching".into(),
                ));
            }
            let Some(p) = &ctx.palette else {
                return Some(UiMsg::Info(NO_BUNDLES.into()));
            };
            spawn_dispatch(agent, ui_tx, p.clone(), None, arg.to_string());
            return None;
        }
        other => UiMsg::Info(format!("no such command: {other}; :help lists them")),
    })
}

/// What to say when there is no classifier to ask.
const NO_BUNDLES: &str = "no [[verba.bundles]] configured; there is nothing to dispatch to";

/// A trace command reached from `:` or a binding while the transcript
/// has no cursor. Naming the key is the point: the command is real and
/// the mode is one press away.
const NO_TRACE: &str = "trace mode is not on \u{2014} `K` puts a cursor on the transcript";

/// Why `l` did nothing. There are two reasons and they want different
/// answers: this block is prose, or the detail knob has already opened
/// every fold there is.
fn nothing_to_open(state: &UiState) -> &'static str {
    match state.detail {
        crate::state::Detail::Folded => "nothing to open here \u{2014} this block is drawn in full",
        _ => "nothing is folded: `space o` puts the detail knob back on folded",
    }
}

/// Resolve an utterance and run the call it makes, off the driver loop.
///
/// `surface` names one bundle — dispatch mode, which was aimed before the
/// line was typed — or is `None` for the whole palette in order. Either
/// way the call goes through `Dispatcher::dispatch`, so the policy hook,
/// the journal and the bus events that put it in the transcript are the
/// ones a model-written call gets. Only the verdict is reported here; the
/// call and its output arrive over the bus like any other tool traffic.
fn spawn_dispatch(
    agent: &Arc<Agent>,
    ui_tx: &mpsc::UnboundedSender<UiMsg>,
    palette: Arc<eidolon_verba::Palette>,
    surface: Option<String>,
    text: String,
) {
    let agent = agent.clone();
    let tx = ui_tx.clone();
    tokio::spawn(async move {
        let resolved = match &surface {
            Some(name) => palette
                .resolve_on(name, &text)
                .await
                .map(|(res, v)| (res, vec![(name.clone(), v)])),
            None => Ok(palette.resolve(&text).await),
        };
        match resolved {
            Ok((Some(res), _)) => {
                let _ = tx.send(UiMsg::Info(crate::dispatch::resolved_line(
                    &res.bundle,
                    &res.verdict,
                )));
                // `dispatch_user`, not `dispatch`: the utterance goes on the
                // branch beside the call, so the model is told what was
                // asked for as well as what ran, and a resume draws both.
                agent
                    .dispatcher()
                    .dispatch_user(res.call, &text, CancellationToken::new())
                    .await;
            }
            // Abstention is the default and not a failure: say what was
            // closest and stop, rather than spending a turn on a line the
            // operator aimed at a classifier on purpose.
            Ok((None, verdicts)) => {
                let said = verdicts
                    .iter()
                    .map(|(b, v)| crate::dispatch::abstained_line(b, v))
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = tx.send(UiMsg::Info(if said.is_empty() {
                    NO_BUNDLES.to_string()
                } else {
                    said
                }));
            }
            Err(e) => {
                let _ = tx.send(UiMsg::Info(format!("dispatch: {e:#}")));
            }
        }
    });
}

/// Journal a fresh token-pool claim onto the session the agent holds, for
/// a pool first claimed by `:model` or by adoption mid-session. The same
/// line the CLI's `build` writes at launch; sticky re-claims (the common
/// case — the session already holds its lane) journal nothing.
async fn journal_claim(agent: &Agent, claim: &Option<eidolon_providers::catalog::ClaimedKey>) {
    let Some(k) = claim.as_ref().filter(|k| k.fresh) else { return };
    let text = format!("key pool: {} lane {}", k.provider, k.secret);
    let mut s = agent.session().lock().await;
    if let Err(e) = s.append(RecordKind::Note { text }) {
        tracing::warn!(error = %e, "could not journal the key-pool claim");
    }
}

/// Continue in a different log, in place.
///
/// The order is the point. Everything that can fail happens before
/// anything moves, so a session written on a model the catalog no longer
/// resolves leaves the running one exactly as it was rather than half
/// swapped. Then the three things that make a session what it is move
/// together: the backend for its model, the directory its tools run in
/// (`Dispatcher::set_cwd` — a log carries the directory it was started
/// in, and resuming it anywhere else would point the tools at the wrong
/// tree), and the log itself.
///
/// The directory is the session's *effective* one, not the raw start
/// record: a log opened with a reopen-time cwd override — the shape a
/// projection arriving from another machine takes — adopts the override,
/// so it runs here rather than in a directory that exists only on the
/// machine that bore it.
async fn adopt(agent: &Arc<Agent>, ctx: &DriverCtx, session: Session) -> anyhow::Result<UiMsg> {
    use anyhow::Context;
    let started_on = session
        .start()
        .map(|(m, ..)| m.to_string())
        .context("that log has no start record")?;
    let cwd = {
        let logged = PathBuf::from(session.cwd().context("that log has no start record")?);
        // A log hopped from another machine carries its birth directory,
        // which does not exist here; the directory this session already
        // runs in stands in rather than failing every spawn. The `Resumed`
        // message below reports whatever this resolved to, so the operator
        // sees it.
        if logged.is_dir() {
            logged
        } else {
            agent.dispatcher().cwd()
        }
    };
    // The branch's own model wins over the one it was created with: a
    // session that switched models mid-way should come back on the one it
    // left off on.
    let key = session.model().unwrap_or(started_on);
    let resolved = match &ctx.catalog {
        Some(catalog) => Some(
            catalog
                .backend_for(&key, None, &cwd, &ctx.scratch)
                .with_context(|| {
                    format!("that session ran on `{key}`, which no longer resolves")
                })?,
        ),
        None => None,
    };
    let branch: Vec<Record> = session.branch().into_iter().cloned().collect();
    let path = session.path().display().to_string();
    if let Some((backend, key, claim)) = resolved {
        agent.set_backend(backend);
        // Adopted, not set: the log already records which model it was
        // written with, and journaling that back would be the harness
        // telling the session what the session just told it.
        agent.adopt_model(
            key.clone(),
            eidolon_providers::catalog::model_id(&key).to_string(),
        );
        journal_claim(agent, &claim).await;
    }
    agent.dispatcher().set_cwd(cwd.clone());
    agent.dispatcher().set_spill_dir(Some(
        eidolon_core::Dispatcher::spill_dir_for(session.path()),
    ));
    agent.adopt_session(session).await;
    let pins = agent.session().lock().await.pins();
    Ok(UiMsg::Resumed {
        path,
        cwd: cwd.display().to_string(),
        model: agent.model_key(),
        branch,
        pins,
        fresh: false,
    })
}

/// Start a fresh log beside the others, on the model and in the directory
/// the running session uses — `new_session` is "another one of these",
/// not "the harness as it would have started". The name is the
/// millisecond it was created, which is the scheme the CLI writes with,
/// because they land in one directory together.
async fn new_session(agent: &Arc<Agent>, ctx: &DriverCtx) -> anyhow::Result<UiMsg> {
    use anyhow::Context;
    std::fs::create_dir_all(&ctx.sessions_dir)
        .with_context(|| format!("creating {}", ctx.sessions_dir.display()))?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let path = ctx.sessions_dir.join(format!("{ms}.eid"));
    let cwd = agent.dispatcher().cwd();
    let session = Session::create(&path, &agent.model_key(), &cwd, agent.config().system)?;
    // A session born here opens on the start screen, not on a resumed
    // row it never earned: the same adoption path, minus the note.
    match adopt(agent, ctx, session).await {
        Ok(UiMsg::Resumed { path, cwd, model, branch, pins, .. }) => {
            Ok(UiMsg::Resumed { path, cwd, model, branch, pins, fresh: true })
        }
        other => other,
    }
}

/// The inspector's report as lines.
///
/// Sizes are characters and the header says so, because the one figure in
/// tokens is the measured `ctx` in the overview. Mixing an invented token
/// estimate into the same column as a real one is the confusion the whole
/// report exists to avoid.
fn context_report_lines(r: &eidolon_core::agent::ContextReport, limit: Option<u64>) -> String {
    let mut out = format!(
        "── Overview ──\n\n  Context       {}\n  Prompt        {} · {} blocks\n  Tool guidance {} (included in system)\n",
        eidolon_core::usage::context_line(r.tokens, limit), human_chars(r.chars), r.blocks.len(), human_chars(r.guidance_chars),
    );
    if let Some(n) = r.compacted {
        out.push_str(&format!("  Compacted     {n} messages folded away and out of view\n"));
    }
    out.push_str("\n── Size by source · largest first ──\n\n");
    for source in &r.sources {
        let share = if r.chars == 0 { 0.0 } else { source.chars as f64 / r.chars as f64 };
        let filled = (share * 16.0).round().clamp(0.0, 16.0) as usize;
        out.push_str(&format!("  {:<16} │ {}{} {:>8}  {:>5.1}%\n", source.kind,
            "━".repeat(filled), "·".repeat(16 - filled), human_chars(source.chars), share * 100.0));
    }
    out.push_str("\n  Shares are characters in assembled replay, not token estimates.\n  Image payload is encoded text, not vision tokens. Schemas/wire framing excluded.\n");
    let mut grouped = std::collections::BTreeMap::<&str, (usize, usize, usize)>::new();
    for tool in &r.tools {
        let row = grouped.entry(&tool.name).or_default();
        row.0 += 1;
        row.1 += tool.input_chars;
        row.2 += tool.result_chars;
    }
    let mut grouped: Vec<_> = grouped.into_iter().collect();
    grouped.sort_by(|a, b| (b.1.1 + b.1.2).cmp(&(a.1.1 + a.1.2)).then(a.0.cmp(b.0)));
    out.push_str("\n── Tool footprint · retained calls only ──\n\n");
    for (name, (count, input, result)) in grouped {
        out.push_str(&format!("  {name}\n    {count} calls │ {} arguments + {} results = {}\n",
            human_chars(input), human_chars(result), human_chars(input + result)));
    }
    if r.tools.is_empty() { out.push_str("  No tool calls in context.\n"); }
    out.push_str("\n── Largest individual calls · top 12 ──\n\n");
    for tool in r.tools.iter().take(12) {
        let error = if tool.is_error { " · error" } else { "" };
        out.push_str(&format!("  {} │ {}{error}\n    {} arguments + {} results · id {}\n", tool.name,
            human_chars(tool.input_chars + tool.result_chars), human_chars(tool.input_chars), human_chars(tool.result_chars), tool.id));
    }
    out.push_str("\n  Tool rows are a drill-down, not additional context. Arguments include tool names.\n");
    out.push_str("\n── Prompt inputs ──\n\n");
    for b in r.blocks.iter().filter(|b| matches!(b.kind, "persona" | "project" | "pins")) {
        out.push_str(&format!("  {} │ {} · {}\n", b.kind, human_chars(b.chars), b.label));
    }
    out.trim_end().to_string()
}

/// Walk branches together rather than interleaving them in append order.
/// Linear runs keep one lane; only a fork adds indentation.
fn tree_report(nodes: &[eidolon_core::TreeNode<'_>]) -> crate::vault::Tree {
    let by_id: std::collections::HashMap<_, _> = nodes.iter().map(|n| (n.record.id, n)).collect();
    let forks = nodes.iter().filter(|n| n.children.len() > 1).count();
    let head = nodes.iter().find(|n| n.is_head).map(|n| format!("#{}", n.record.id)).unwrap_or_else(|| "none".into());
    let mut out = format!("── Session map ──\n\n  {} records · {forks} fork points · head {head}\n  ● head   ◆ current path   ○ other branch\n\n── Branches ──\n\n", nodes.len());
    let mut stack: Vec<_> = nodes.iter().rev().filter(|n| n.record.parent.is_none_or(|p| !by_id.contains_key(&p)))
        .map(|n| (n.record.id, String::new(), String::new())).collect();
    let mut records = std::collections::BTreeMap::new();
    let mut source_line = out.lines().count() + 1;
    while let Some((id, prefix, edge)) = stack.pop() {
        let Some(n) = by_id.get(&id) else { continue; };
        let mark = if n.is_head { "●" } else if n.on_branch { "◆" } else { "○" };
        let suffix = if n.is_head { "  ← HEAD" } else { "" };
        records.insert(source_line, n.record.clone());
        source_line += 1;
        out.push_str(&format!("  {prefix}{edge}{mark} #{:<4} {}{suffix}\n", id, record_line(n.record).replace(['\n', '\r'], " ")));
        let next = if edge == "├─" { format!("{prefix}│ ") } else if edge == "└─" { format!("{prefix}  ") } else { prefix };
        for (i, child) in n.children.iter().enumerate().rev() {
            let edge = if n.children.len() == 1 { "" } else if i + 1 == n.children.len() { "└─" } else { "├─" };
            stack.push((*child, next.clone(), edge.to_string()));
        }
    }
    if nodes.is_empty() { out.push_str("  No records yet.\n"); }
    crate::vault::Tree { body: out, records }
}

/// Characters, short. A separate spelling from `usage::human` on purpose:
/// that one counts tokens, and the two must not look like the same unit.
fn human_chars(n: usize) -> String {
    match n {
        0..=9_999 => format!("{n}c"),
        _ => format!("{:.1}kc", n as f64 / 1000.0),
    }
}

fn record_line(r: &eidolon_core::session::Record) -> String {
    use eidolon_core::session::RecordKind::*;
    let short = |s: &str| -> String {
        let one: String = s.chars().take(60).collect();
        one.replace('\n', "⏎")
    };
    match &r.kind {
        SessionStart { model, .. } => format!("start {model}"),
        UserMessage(m) => format!("user: {}", short(&m.text())),
        AssistantMessage(m) => format!("assistant: {}", short(&m.text())),
        ToolResult {
            tool_use_id,
            is_error,
            ..
        } => format!(
            "result {tool_use_id}{}",
            if *is_error { " ERROR" } else { "" }
        ),
        UserToolCall {
            name, utterance, ..
        } => match utterance
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            Some(u) => format!("dispatch {name}: {}", short(u)),
            None => format!("dispatch {name}"),
        },
        TurnSettled { .. } => "settled".into(),
        ContextSize { tokens } => format!("context {tokens}"),
        // The same wording the CLI's log and the usage page use.
        TurnPace {
            timing,
            output_tokens,
        } => match eidolon_core::usage::pace_note(timing, *output_tokens) {
            Some(note) => format!("pace {note}"),
            None => format!("pace under a millisecond over {} calls", timing.calls),
        },
        AskUser { prompt, .. } => format!("ask {}", short(prompt)),
        Cancelled => "cancelled".into(),
        TurnSpend { usage, calls } => {
            use eidolon_core::UsageExt;
            format!(
                "spent {} in {} out over {calls} call{}",
                usage.total_input(),
                usage.output_tokens,
                if *calls == 1 { "" } else { "s" }
            )
        }
        ModelChanged { model } => format!("model → {model}"),
        Compacted {
            replaced_messages, ..
        } => format!("compacted {replaced_messages}"),
        BackendSession { backend, id } => format!("{backend} session {id}"),
        Note { text } => format!("note: {}", short(text)),
        PeerMessage { from, text, .. } => format!("from {from}: {}", short(text)),
        // An outside caller — `eidolon send` — kept distinct from peer
        // traffic for the same reason the record is.
        ExternalMessage { from, text, .. } => format!("external {from}: {}", short(text)),
        TriggerFired { condition, outcome, .. } => format!("waiting: {} — {}", short(condition), short(outcome)),
        TurnBudget { calls_left } => format!("wrap-up nudge: {calls_left} calls left"),
        PolicyVerdict {
            tool,
            reason,
            outcome,
            ..
        } => format!("policy {tool} {outcome:?}: {}", short(reason)),
        SessionNote { text } => match text.trim() {
            "" => "session note cleared".to_string(),
            t => format!("session note: {}", short(t)),
        },
        Pinned { path, pinned } => {
            format!("{} {path}", if *pinned { "pinned" } else { "unpinned" })
        }
        PersonaPinned { persona } => match persona.trim() {
            "" => "persona cleared".to_string(),
            p => format!("persona: {p}"),
        },
        Excluded { target, excluded } => {
            format!(
                "{} #{target} from the replay",
                if *excluded { "struck" } else { "restored" }
            )
        }
        // The harness's answers to commands the model wrote in its prose.
        // One line in the trace, naming the count and the first answer:
        // the full text is on the transcript beside it.
        CommandResults { lines } => match lines.first() {
            Some(first) => format!("answers {}: {}", lines.len(), short(first)),
            None => "answers".to_string(),
        },
    }
}

// ---------------------------------------------------------------- UI thread

/// Compile a script and read its tables — keymap, modes, theme — into
/// place. Every failure is a line in the transcript and a fallback to
/// the built-in script's answer, so a broken `ui.rn` never blanks the
/// screen or leaves a key unbound. Called at startup and again by
/// `reload_ui`, which is what makes editing the script a save and a
/// command rather than a restart.
fn load_ui(state: &mut UiState, src: &str) -> (Option<UiScript>, Keys) {
    state.script_error = None;
    let said_before = state.notices.len();
    let script = match UiScript::compile(src) {
        Ok(s) => Some(s),
        Err(e) => {
            state.script_error = Some(format!("{e:#}"));
            state.info(format!("ui script error; using the built-in layout: {e:#}"));
            UiScript::compile(crate::DEFAULT_UI).ok()
        }
    };
    eidolon_core::boot::mark("ui compiled");
    // The keymap is read once: it is pure data, and a `Vm` per keystroke
    // would be both wasteful and unable to remember a half-typed chord.
    let mut keymap = match script.as_ref().map(|s| s.keymap()) {
        Some(Ok(m)) => m,
        Some(Err(e)) => {
            state.info(format!(
                "ui keymap() failed; falling back to the built-in keys: {e:#}"
            ));
            UiScript::compile(crate::DEFAULT_UI)
                .ok()
                .and_then(|s| s.keymap().ok())
                .unwrap_or_default()
        }
        None => serde_json::Value::Null,
    };
    // The search line, the dialogs and trace mode take their keys from
    // tables of their own, but a script that never heard of those
    // tables must not lose them — an approval box nothing can answer is
    // a hang, and so is a mode where nothing is text and `esc` is
    // unbound. That is the test for this list: whether a missing table
    // leaves the operator *stuck*, rather than merely without a key. An
    // insert-kinded mode still types; trace mode does not, which is why
    // it is here and dispatch is not. The command line needs no
    // stand-in either, since it falls back to insert's on the
    // keystroke. The default script needs none at all, and is not
    // compiled a second time to find that out.
    if src != crate::DEFAULT_UI
        && let (Some(mine), Some(defaults)) = (
            keymap.as_object().cloned(),
            UiScript::compile(crate::DEFAULT_UI)
                .ok()
                .and_then(|s| s.keymap().ok())
                .and_then(|v| v.as_object().cloned()),
        )
    {
        let mut filled = mine;
        for name in ["search", "trace", "confirm", "choose", "pick", "page", "vault"] {
            if !filled.contains_key(name)
                && let Some(t) = defaults.get(name)
            {
                filled.insert(name.to_string(), t.clone());
            }
        }
        keymap = serde_json::Value::Object(filled);
    }
    // The script's own commands, checked before the keymap is, since a
    // binding may name one. A built-in's name is refused rather than
    // shadowed, and a row whose function the script does not define is
    // refused rather than left to fail on the keystroke.
    state.script_commands = Vec::new();
    if let Some(script) = script.as_ref() {
        match script.commands() {
            Some(Ok(v)) => match v.as_object() {
                Some(rows) => {
                    for (name, row) in rows {
                        match crate::command::Scripted::parse(name, row) {
                            Ok(c) if command::lookup(&c.name).is_some() => {
                                state.info(format!(
                                    "commands(): `{}` is a built-in and cannot be redefined",
                                    c.name
                                ));
                            }
                            Ok(c) if !script.has(&c.run) => {
                                state.info(format!("commands(): `{}` names a function `{}` the script does not define", c.name, c.run));
                            }
                            Ok(c) => state.script_commands.push(c),
                            Err(e) => state.info(format!("commands(): {e:#}")),
                        }
                    }
                }
                None => state.info("ui commands() must return an object of name → row"),
            },
            Some(Err(e)) => state.info(format!("ui commands() failed: {e:#}")),
            None => {}
        }
    }
    // A binding may only name a command the registry — or the script —
    // defines. Saying so at startup rather than on the keystroke that
    // finds it is the point of having one table: a typo in a hand-written
    // `ui.rn` is a line in the transcript, not a key that silently does
    // nothing.
    let extra: Vec<String> = state
        .script_commands
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let unknown = crate::keys::unknown_commands(&keymap, &extra);
    if !unknown.is_empty() {
        state.info(format!(
            "keymap binds commands that do not exist: {}",
            unknown.join(", ")
        ));
    }
    state.keymap = keymap.clone();

    // The mode table, laid over the default script's so a user's need
    // only say what it changes. A script that says nothing gets the
    // default's outright.
    let base = crate::script::default_modes();
    state.modes = match script.as_ref().and_then(|s| s.modes()) {
        Some(Ok(v)) => match crate::modes::ModeTable::from_value(&v, &base) {
            Ok(t) => t,
            Err(e) => {
                state.info(format!(
                    "ui modes() is not usable; keeping the built-in modes: {e:#}"
                ));
                base
            }
        },
        Some(Err(e)) => {
            state.info(format!(
                "ui modes() failed; keeping the built-in modes: {e:#}"
            ));
            base
        }
        None => base,
    };
    // The theme, the same way.
    let base = crate::script::default_theme();
    state.theme = match script.as_ref().and_then(|s| s.theme()) {
        Some(Ok(v)) => match crate::theme::Theme::from_value(&v, &base) {
            Ok((t, unknown)) => {
                if !unknown.is_empty() {
                    state.info(format!(
                        "theme() names roles nothing paints: {}",
                        unknown.join(", ")
                    ));
                }
                t
            }
            Err(e) => {
                state.info(format!(
                    "ui theme() is not usable; keeping the built-in theme: {e:#}"
                ));
                base
            }
        },
        Some(Err(e)) => {
            state.info(format!(
                "ui theme() failed; keeping the built-in theme: {e:#}"
            ));
            base
        }
        None => base,
    };
    // The labels, the same way: what tools are called, with the wire
    // name for anything the script says nothing about.
    let base = crate::script::default_labels();
    state.labels = match script.as_ref().and_then(|s| s.labels()) {
        Some(Ok(v)) => match crate::labels::Labels::from_value(&v, &base) {
            Ok(t) => t,
            Err(e) => {
                state.info(format!(
                    "ui labels() is not usable; keeping the built-in labels: {e:#}"
                ));
                base
            }
        },
        Some(Err(e)) => {
            state.info(format!(
                "ui labels() failed; keeping the built-in labels: {e:#}"
            ));
            base
        }
        None => base,
    };
    // The wait tags, the same way: a kind core does not watch is a
    // warning, not a row.
    let base = crate::labels::builtin_wait_tags();
    state.wait_tags = match script.as_ref().and_then(|s| s.wait_tags()) {
        Some(Ok(v)) => match crate::labels::WaitTags::from_value(&v, &base) {
            Ok((t, unknown)) => {
                if !unknown.is_empty() {
                    state.info(format!(
                        "wait_tags() names kinds nothing watches: {}",
                        unknown.join(", ")
                    ));
                }
                t
            }
            Err(e) => {
                state.info(format!(
                    "ui wait_tags() is not usable; keeping the built-in tags: {e:#}"
                ));
                base
            }
        },
        Some(Err(e)) => {
            state.info(format!(
                "ui wait_tags() failed; keeping the built-in tags: {e:#}"
            ));
            base
        }
        None => base,
    };
    // The two tables have to agree with each other: an editing mode with
    // no keymap is a room with no doors, and a keymap for a mode that
    // does not exist is a door onto nothing. Neither is fatal, and both
    // are worth a line now rather than a mystery later.
    let tables: Vec<String> = keymap
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    for m in state.modes.editing_names() {
        if !tables.contains(&m) {
            state.info(format!(
                "mode `{m}` has no keymap table; no key will do anything there"
            ));
        }
    }
    // A style-only row may have a table too: the search line and the
    // five dialogs take keys without being modes on the buffer.
    for t in &tables {
        if state.modes.get(t).is_none() {
            state.info(format!(
                "keymap has a table for `{t}`, which modes() does not define"
            ));
        }
    }
    // A reload can pull the mode the prompt is in out from under it.
    if state.modes.get(state.prompt.mode.as_str()).is_none() {
        state.prompt.enter(crate::edit::Mode::normal());
    }
    // Several complaints are several notices and the status line shows
    // one, so say how many there were and where the rest can be read.
    let said = state.notices.len() - said_before;
    if said > 1 {
        state.info(format!(
            "{said} notes while loading the ui \u{2014} `:messages` has them"
        ));
    }
    (script, Keys::new(keymap))
}

/// What `reload_ui` compiles: the file it was started from, read again,
/// or the built-in default when there is no such file any more — which
/// is how deleting `ui.rn` gets you the default back without a restart.
fn reload_source(state: &mut UiState, path: Option<&Path>, first: &str) -> String {
    match path {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                state.info(format!("no {}; the built-in layout is in use", p.display()));
                crate::DEFAULT_UI.to_string()
            }
            Err(e) => {
                state.info(format!(
                    "could not read {}: {e}; keeping what was loaded",
                    p.display()
                ));
                first.to_string()
            }
        },
        None => first.to_string(),
    }
}

fn ui_loop(
    mut state: UiState,
    mut rx: mpsc::UnboundedReceiver<UiMsg>,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    script_src: String,
    script_path: Option<PathBuf>,
) {
    let (mut script, mut keys) = load_ui(&mut state, &script_src);
    eidolon_core::boot::mark("ui tables");
    let mut terminal = ratatui::init();
    // The frames paint their own caret; this one is shown again on the way out.
    let _ = terminal.hide_cursor();
    eidolon_core::boot::mark("terminal");
    // The wheel scrolls the transcript and a paste arrives as one event
    // rather than as keystrokes (so a pasted newline does not submit).
    // Both are opt-in escape modes, so both are turned back off on exit.
    //
    // The third is what makes `S-ret` reachable at all: on a plain
    // terminal shift and enter arrive as an unadorned carriage return,
    // indistinguishable from enter, so the newline key the prompt binds
    // would silently send the message instead. `DISAMBIGUATE_ESCAPE_CODES`
    // is the smallest kitty-protocol flag that separates them — no
    // release events, no text reporting — and a terminal that has never
    // heard of it ignores this and the pop on the way out, which is why
    // `A-ret` stays bound beside it.
    let _ = execute!(
        std::io::stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    // `ratatui::init` installed a hook that leaves the alternate screen on
    // a panic; it knows nothing of the two modes above, so turn them off
    // ahead of it — a terminal left reporting mouse motion is unusable.
    // The frames paint their own caret and the native one starts hidden,
    // so the hook hands that back too: a panic below the exit path's
    // `Show` would otherwise leave a terminal with no cursor anywhere.
    let inner = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste,
            PopKeyboardEnhancementFlags,
            Show
        );
        inner(info);
    }));
    let mut dirty = true;
    let mut first_frame = true;
    let mut last_tick = std::time::Instant::now();
    // The frame the user is dragging over, captured only while a selection
    // exists: `Terminal::draw` swaps buffers, so by the time the button
    // comes up the drawn cells are no longer reachable from the terminal.
    let mut frame: Vec<Vec<String>> = Vec::new();

    while !state.quit {
        // Drain the core.
        while let Ok(msg) = rx.try_recv() {
            apply(&mut state, msg);
            // The highlight is anchored to screen cells, so anything that
            // redraws the text under it retires it.
            if state.selection.is_some_and(|s| !s.dragging) {
                state.selection = None;
            }
            dirty = true;
        }
        // Draw before waiting for input, not after: the first frame used
        // to sit behind a 33 ms poll for a key nobody had pressed yet, and
        // every frame a bus event asked for waited out the same poll. A key
        // still lands on the very next frame — the loop comes straight back
        // here after handling it — and text streaming in is still coalesced
        // to one frame per poll rather than one per delta.
        if drain_attachments(&mut state) {
            dirty = true;
        }
        if state
            .copied_at
            .is_some_and(|t| t.elapsed() > Duration::from_secs(2))
        {
            state.copied_at = None;
            dirty = true;
        }
        if state.expire_notice() {
            dirty = true;
        }
        if (state.working || state.waiting())
            && last_tick.elapsed() > Duration::from_millis(250)
        {
            state.tick += 1;
            state.life.step();
            last_tick = std::time::Instant::now();
            dirty = true;
        }
        if dirty {
            // Hold a scrolled-up view against the transcript's content
            // before the frame measures it: the rows the stream added
            // since the last frame pass under the window instead of
            // carrying the line being read out of it.
            hold_scroll(&mut state);
            // The mode can be entered by a key, by `:enter_mode trace`
            // or by a script's action, so the cursor is placed here —
            // once, in front of the snapshot the status line reads —
            // rather than in each of the three.
            crate::trace::sync(&mut state);
            let tree = script
                .as_ref()
                .and_then(|s| match s.view(state.snapshot()) {
                    Ok(t) => Some(t),
                    Err(e) => {
                        if state.script_error.is_none() {
                            state.script_error = Some(format!("{e:#}"));
                            state.info(format!(
                                "ui view() failed; using the built-in layout: {e:#}"
                            ));
                        }
                        None
                    }
                })
                .unwrap_or_else(fallback_tree);
            let mut r = Renderer::new(&state);
            let mut screen = None;
            if let Ok(done) = terminal.draw(|f| r.draw(f, &tree)) {
                screen = Some((done.area.width, done.area.height));
                if state.selection.is_some() {
                    frame = crate::render::screen_rows(done.buffer);
                }
            }
            if std::mem::take(&mut first_frame) {
                // The prompt is live: the startup budget ends here.
                eidolon_core::boot::mark("first frame");
                eidolon_core::boot::flush();
            }
            // The pixels, after the frame and never before it: ratatui
            // writes cells and a graphics escape writes pixels over cells,
            // so in the other order the frame paints back over the
            // picture. A placement already on screen in the same place was
            // marked skip during the draw, so its cells were not touched
            // and it does not need painting again — which is what stops
            // every image in the transcript flickering under a streaming
            // reply.
            let (lines, height, width, drawn, anchor) = (
                r.transcript_lines,
                r.transcript_height,
                r.transcript_width,
                r.view_entries.clone(),
                r.scroll_anchor,
            );
            let rect = r.transcript_rect;
            let images = std::mem::take(&mut r.images);
            let link_hits = std::mem::take(&mut r.link_hits);
            // The renderer borrows the state; painting writes to it.
            drop(r);
            if let Some(screen) = screen {
                state.screen = screen;
            }
            state.vault.hits = link_hits;
            paint_images(&mut state, images);
            if !state.vault.active {
                state.view_entries = drawn;
                state.view_width = width;
            }
            state.transcript_rect = rect;
            if !state.vault.active { state.view_height = height; }
            let max_up = lines.saturating_sub(height);
            if !state.vault.active && state.scroll_up > max_up {
                state.scroll_up = max_up;
            }
            // What the frame just drew, the anchor is now: a frame that
            // followed the live edge holds nothing (every new row is
            // meant to arrive), and a frame that drew no transcript at
            // all — a dialog over everything — leaves the last anchor
            // standing so the view resurfaces where it left off.
            if let Some(a) = anchor {
                state.scroll_anchor = (state.scroll_up > 0).then_some(a);
            }
            dirty = false;
        }
        // Wait for one event, then drain what is already queued before
        // drawing: a drag is a burst, and a frame per event fell behind it.
        let mut wait = Duration::from_millis(33);
        while event::poll(wait).unwrap_or(false) {
            wait = Duration::ZERO;
            match event::read() {
                Ok(TermEvent::Key(k)) if k.kind != KeyEventKind::Release => {
                    state.selection = None;
                    handle_key(&mut state, &mut keys, script.as_ref(), &cmd_tx, k);
                    // `reload_ui` can only ask: the script and the keymap
                    // are this loop's, so this is where they are replaced.
                    if std::mem::take(&mut state.reload) {
                        let src = reload_source(&mut state, script_path.as_deref(), &script_src);
                        (script, keys) = load_ui(&mut state, &src);
                        if state.script_error.is_none() {
                            state.info("ui reloaded: layout, keymap, modes and theme");
                        }
                    }
                    // `edit_prompt` can only ask, for the same reason: the
                    // terminal is this loop's. Hand the whole screen to
                    // the editor and take it back — [`crate::editor`] —
                    // then let the frame below repaint everything, which
                    // is what the clear inside makes the next draw do.
                    if std::mem::take(&mut state.edit_draft) {
                        crate::editor::edit_prompt(&mut state, &mut terminal);
                    }
                    dirty = true;
                }
                Ok(TermEvent::Mouse(m)) => {
                    let at = (m.column, m.row);
                    match m.kind {
                        MouseEventKind::ScrollUp => {
                            if state.dialog.is_some() {
                                for _ in 0..3 { dialog_exec(&mut state, script.as_ref(), &cmd_tx, "scroll_line_up"); }
                            } else if state.vault.active { crate::vault::exec(&mut state, &cmd_tx, "scroll_line_up", 3); }
                            else { state.set_scroll(state.scroll_up + 3); }
                            dirty = true;
                        }
                        MouseEventKind::ScrollDown => {
                            if state.dialog.is_some() {
                                for _ in 0..3 { dialog_exec(&mut state, script.as_ref(), &cmd_tx, "scroll_line_down"); }
                            } else if state.vault.active { crate::vault::exec(&mut state, &cmd_tx, "scroll_line_down", 3); }
                            else { state.set_scroll(state.scroll_up.saturating_sub(3)); }
                            dirty = true;
                        }
                        MouseEventKind::Down(MouseButton::Left) => {
                            state.selection = Some(Selection {
                                from: at,
                                to: at,
                                dragging: true,
                            });
                            state.copied_at = None;
                            dirty = true;
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            if let Some(sel) = &mut state.selection {
                                sel.to = at;
                                dirty = true;
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            if let Some(mut sel) = state.selection {
                                sel.dragging = false;
                                sel.to = at;
                                state.selection = (!sel.is_empty()).then_some(sel);
                                if sel.is_empty() {
                                    let target = state.vault.hits.iter().find(|h| h.rect.contains(at.into())).map(|h| h.target.clone());
                                    match target {
                                        Some(target) => crate::vault::request(&mut state, &cmd_tx, target),
                                        None => toggle_fold_at(&mut state, at),
                                    }
                                }
                                // A notice, as `y` gives one; the dim word went unseen.
                                if let Some(sel) = state.selection {
                                    let text = crate::render::selected_text(&frame, sel);
                                    if !text.trim().is_empty() {
                                        if crate::clipboard::copy(&text) {
                                            state.copied_at = Some(std::time::Instant::now());
                                            let n = text.lines().count();
                                            state.info(format!("copied {n} line{}", if n == 1 { "" } else { "s" }));
                                        } else {
                                            state.info("nothing took the clipboard".to_string());
                                        }
                                    }
                                }
                                dirty = true;
                            }
                        }
                        _ => {}
                    }
                }
                Ok(TermEvent::Resize(_, _)) => dirty = true,
                // Into whichever field is in front. A paste went to the
                // prompt unconditionally, which put it into a buffer
                // nobody could see while a question, a filter or a
                // search line owned the keyboard — and a pasted answer
                // then reappeared in the next message.
                Ok(TermEvent::Paste(s)) => {
                    // A `Pick`'s field is a filter and a minibuffer is a
                    // pattern, a command or an utterance: all one line
                    // by nature, so a pasted newline is flattened rather
                    // than left to submit something. Only the prompt
                    // takes a paste whole.
                    match &mut state.dialog {
                        Some(d) => {
                            d.input.insert(&s.replace('\n', " "));
                            if d.kind == DialogKind::Pick {
                                d.selected = d.visible().first().copied().unwrap_or(0);
                            }
                        }
                        None if state.mini.is_some() => {
                            target(&mut state).insert(&s.replace('\n', " "));
                            sync_search(&mut state);
                        }
                        None => state.prompt.insert(&s),
                    }
                    dirty = true;
                }
                _ => {}
            }
        }
    }
    let _ = execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        PopKeyboardEnhancementFlags,
        Show
    );
    ratatui::restore();
    let _ = cmd_tx.send(Cmd::Quit);
}

fn fallback_tree() -> serde_json::Value {
    serde_json::json!({ "kind": "column", "children": [
        { "size": "fill", "child": { "kind": "panel", "title": " eidolon ", "child": { "kind": "transcript" } } },
        { "size": "grow:3:12", "child": { "kind": "prompt" } },
        { "size": 1, "child": { "kind": "status", "left": "built-in layout", "right": "" } },
        { "kind": "dialog" }
    ]})
}

fn apply(state: &mut UiState, msg: UiMsg) {
    match msg {
        UiMsg::ParkTicked(ticks) => {
            for tick in ticks {
                state.wait_tick(tick);
            }
        }
        UiMsg::Bus(ev) => match ev {
            Event::MessageStart => {
                state.working = true;
                state.ledger.began(crate::usage::now_ms());
                if state.working_since.is_none() {
                    state.working_since = Some(std::time::Instant::now());
                    state.streamed = 0;
                    state.life.restart();
                }
                // No entry yet: which kind it is depends on which delta
                // arrives first, and an empty assistant line pushed here
                // would sit *above* the thinking that follows it.
                state.msg_start = state.transcript.len();
            }
            Event::TextDelta(t) => {
                state.streamed += t.chars().count() as u64;
                if let Some(Entry::Assistant {
                    text,
                    streaming: true,
                    ..
                }) = state.transcript.last_mut()
                {
                    text.push_str(&t);
                } else {
                    finish_streaming(state);
                    state.transcript.push(Entry::Assistant {
                        text: t,
                        streaming: true,
                    });
                }
            }
            // Thinking is an entry of its own, so the reply beginning no
            // longer erases the note that the model thought first.
            Event::ThinkingDelta(t) => {
                state.streamed += t.chars().count() as u64;
                if let Some(Entry::Thinking {
                    text,
                    streaming: true,
                }) = state.transcript.last_mut()
                {
                    text.push_str(&t);
                } else {
                    finish_streaming(state);
                    state.transcript.push(Entry::Thinking {
                        text: t,
                        streaming: true,
                    });
                }
            }
            Event::ToolUseStart { id, name } => {
                state.life.call();
                finish_streaming(state);
                state.transcript.push(Entry::Tool {
                    id,
                    name,
                    input: String::new(),
                    output: None,
                });
            }
            Event::ToolInputDelta { partial_json, .. } => {
                state.streamed += partial_json.chars().count() as u64;
                if let Some(Entry::Tool {
                    input,
                    output: None,
                    ..
                }) = state.transcript.last_mut()
                {
                    input.push_str(&partial_json);
                }
            }
            Event::UserMessage { record, message } => {
                // Where the turn began, for the ledger: the record is
                // stamped when the turn starts, which is what `seed`
                // reads on resume, so the live row says the same.
                state.ledger.began(crate::usage::now_ms());
                // The UI drew this from what was typed, before the turn
                // began; all it was waiting for is which record it became.
                // Matched by text rather than taken blindly, because two
                // messages sent during one turn can be journalled in the
                // other order from the one they were matched in — steered
                // deliveries all land at the same safe point — and each
                // must land on its own row.
                let text = message.text();
                let drawn = state.pending_user.front().copied().filter(
                    |&i| matches!(state.transcript.get(i), Some(Entry::User(t)) if *t == text),
                );
                match drawn {
                    Some(from) => {
                        state.pending_user.pop_front();
                        state.remember(from, record);
                    }
                    // A message the UI never drew — one steered in by
                    // something other than the prompt — is drawn now, from
                    // the record, the way a resume draws one.
                    None => {
                        let from = state.transcript.len();
                        for a in message
                            .content
                            .iter()
                            .filter_map(crate::image::Attachment::from_block)
                        {
                            state.transcript.push(Entry::Image(a));
                        }
                        if !text.is_empty() {
                            state.transcript.push(Entry::User(text));
                        }
                        state.remember(from, record);
                    }
                }
            }
            Event::AssistantMessage { record, message: m } => {
                state.ledger.called(crate::usage::now_ms());
                // The reply as the transcript carries it: the message's own
                // text with the channel's bare markers taken out, which is
                // what `seed` draws from the same record. See
                // [`crate::state::reply_text`].
                let text = crate::state::reply_text(&m);
                if let Some(Entry::Assistant {
                    text: t, streaming, ..
                }) = state.transcript.iter_mut().rev().find(|e| {
                    matches!(
                        e,
                        Entry::Assistant {
                            streaming: true,
                            ..
                        }
                    )
                }) {
                    *t = text;
                    *streaming = false;
                }
                // The settled message carries the thinking in full, and
                // the deltas may not have: the Claude CLI streams
                // thinking as empty deltas with a token estimate, which
                // the stream turns into one `·` apiece. So the entry the
                // deltas opened is filled in from the message rather
                // than left as a row of dots. Filled in, never inserted
                // — an insert would shift every index `opened` and the
                // trace cursor hold.
                let thinking = crate::state::thinking_of(&m);
                let from = state.msg_start.min(state.transcript.len());
                if !thinking.is_empty()
                    && let Some(Entry::Thinking { text: t, .. }) = state.transcript[from..]
                        .iter_mut()
                        .find(|e| matches!(e, Entry::Thinking { .. }))
                {
                    *t = thinking;
                }
                finish_streaming(state);
                // Every entry this message opened — the thinking, the
                // prose, the calls — belongs to it. A call's own entry is
                // reclaimed below by its result, which is the thing worth
                // striking.
                state.remember(state.msg_start.min(state.transcript.len()), record);
            }
            // A call the harness makes *inside* another call — a skill
            // script's `eidolon::*`, or the vault channel's resolved
            // `mneme_rpc` — is not the model's tool traffic, and it is not
            // on the branch as a call at all: only an operator's dispatch
            // writes a call record (`UserToolCall`), so a script's call is
            // an event with nothing behind it and a resumed session can
            // never draw it. Drawing it here would be a row that vanishes
            // on resume, so it is not drawn: what the work did is on the
            // call that made it, and — for the vault channel — in the
            // answers, which are a record.
            Event::ToolCallStarted(call) if call.origin == CallOrigin::Script => {}
            Event::ToolCallStarted(call) => {
                state.last_tool = Some(call.name.clone());
                // The streaming path already opened an entry for a call the
                // model made. One the operator made — a dispatch — has no
                // stream behind it, so this is where it appears.
                let has = state
                    .transcript
                    .iter()
                    .any(|e| matches!(e, Entry::Tool { id, output: None, .. } if *id == call.id));
                if !has {
                    state.transcript.push(Entry::Tool {
                        id: call.id,
                        name: call.name,
                        input: call.input.to_string(),
                        output: None,
                    });
                }
            }
            // And its result is not drawn either: it is the same row from
            // the other end, and with no entry to fill it would otherwise
            // be paired by order onto whichever call is still open — the
            // call that made it.
            Event::ToolCallFinished { call, .. } if call.origin == CallOrigin::Script => {}
            Event::ToolCallFinished {
                record,
                call,
                output,
            } => {
                state.tools_run += 1;
                state.life.result();
                state
                    .ledger
                    .tool(&call.name, output.is_error, crate::usage::now_ms());
                if let Some(Entry::Tool { output: o, .. }) =
                    crate::state::find_call(&mut state.transcript, &call.id)
                {
                    *o = Some((output.content.clone(), output.is_error));
                }
                // A wait that armed itself is a block about to tick: the
                // deadline the result states is what the countdown reads.
                if call.name == "wait_for" {
                    let deadline = output
                        .content
                        .lines()
                        .find_map(|l| {
                            l.strip_prefix("deadline: ")
                                .and_then(|s| s.strip_suffix('s'))
                                .and_then(|s| s.parse::<u64>().ok())
                        })
                        .unwrap_or(0);
                    state.wait_armed(&call.id, deadline);
                }
                // Claim the entry for the *result*, overwriting the
                // assistant message that opened it: striking a tool line
                // should withhold what came back, not delete the call that
                // asked. The call stays, and the model is told the output
                // was withheld rather than that the call never happened.
                if let Some(record) = record
                    && let Some(i) = state
                        .transcript
                        .iter()
                        .position(|e| matches!(e, Entry::Tool { id, .. } if *id == call.id))
                {
                    state.records.insert(i, record);
                }
            }
            Event::AskUser { .. } => {}
            // Only the judge's yes leaves no other trace; the wording is
            // the core's, so the live line and the resumed one are one
            // sentence. See `policy::verdict_note`.
            Event::PolicyVerdict {
                tool,
                outcome,
                note,
                ..
            } => {
                state.ledger.verdict(&outcome);
                if let Some(line) =
                    eidolon_core::policy::verdict_note(&outcome, &tool, note.as_deref())
                {
                    state.note_alert(line);
                }
            }
            Event::TurnSettled {
                stop_reason,
                usage,
                timing,
            } => {
                finish_streaming(state);
                state.settled(&usage, stop_reason, timing, crate::usage::now_ms());
                refresh_usage(state);
                // Settling is published with the reason attached; a turn
                // that did not finish says so rather than just stopping.
                if let Some(note) = eidolon_core::usage::stop_note(stop_reason) {
                    state.note_alert(note);
                }
            }
            Event::Queued { waiting } => state.queued = waiting,
            // The context, which is *not* the settle's usage: see
            // `RecordKind::ContextSize`. Published just before the settle,
            // so the gauge and the totals move in the same frame.
            Event::ContextSize { tokens } => {
                state.context_tokens = Some(tokens);
                state.ledger.context(tokens);
            }
            // The end of the one stream that is not a turn. The compaction's
            // own model call is published like any other's, so its summary
            // arrives as deltas and the status line says COGITAT — but the
            // `TurnDone` that takes the indicator down only ever comes for a
            // turn, so it is taken down here, and the rows the deltas opened
            // are taken back off with it. They are the `Compacted` record's
            // text and not a reply on the branch: nothing about this stream
            // is journaled as an assistant message, so a resumed session has
            // no such row to draw, and a row here would be one that vanished
            // on reopen. The record's own line — the one `seed` draws — is
            // what is left. The settle published right behind this one prices
            // the call and closes what it opened.
            //
            // This is the end of the *whole* stream it belongs to, which is
            // why it can end one the frame loop is not running: a compaction
            // is asked for at the prompt (`:compact`) and refused while a
            // turn is in flight, so it is the only thing streaming when this
            // arrives. A compaction the loop started *inside* a turn would
            // need to say which of the two streams had ended.
            Event::Compacted {
                replaced_messages, ..
            } => {
                while matches!(
                    state.transcript.last(),
                    Some(Entry::Assistant { streaming: true, .. } | Entry::Thinking { streaming: true, .. })
                ) {
                    state.transcript.pop();
                }
                state.msg_start = state.transcript.len();
                state.working = false;
                state.working_since = None;
                state.note(format!("compacted {replaced_messages} messages"));
                // Reset to unknown; the `ContextSize` published right behind
                // this carries the summary's measured size, and a consumer
                // that sees this without it draws a dash rather than a
                // false zero.
                state.context_tokens = None;
                state.ledger.compacted();
            }
            // Priced when the harness measured it, which is the whole
            // of the change: an interrupted turn spent real money and
            // used to report none, so a session stopped halfway read as
            // free both live and on resume. `usage` is empty when the
            // backend never told us — the Claude CLI reports its usage
            // in one final event a killed process never sends — and an
            // empty one is left unpriced rather than counted as zero.
            Event::Cancelled { usage, .. } => {
                finish_streaming(state);
                state.note("cancelled");
                state.ledger.cancelled();
                if usage != Default::default() {
                    state.settled(
                        &usage,
                        eidolon_core::StopReason::Cancelled,
                        None,
                        crate::usage::now_ms(),
                    );
                }
            }
            Event::Error(e) => state.note_alert(format!("error: {e}")),
            // Journaled before it was published, and only ever at a turn
            // boundary — so pushing it at the end of the transcript is
            // where it belongs, with nothing streaming to interrupt.
            Event::PeerMessage {
                from,
                channel,
                text,
                ..
            } => {
                finish_streaming(state);
                state.transcript.push(Entry::Peer {
                    from,
                    channel: channel.is_some(),
                    text,
                });
            }
            // Why this session woke. A line of its own rather than a peer's
            // entry, because it is neither the operator speaking nor a
            // colleague: the harness answering a condition the session
            // asked to be woken by.
            Event::TriggerFired {
                condition,
                outcome,
                call_id,
                ..
            } => {
                finish_streaming(state);
                state.transcript.push(Entry::Woke {
                    condition,
                    outcome: outcome.clone(),
                });
                // The block comes to rest: rows stay as the last tick drew
                // them, so the leaf that fired stays checked on screen.
                state.wait_settled(&call_id, &outcome);
                state.working = false;
            }
            // Journaled before it was published, at the top of a loop
            // iteration — between one reply's tool results and the next
            // model call, where nothing is streaming and an entry cannot
            // land inside a cluster.
            Event::TurnBudget { calls_left, .. } => {
                state.transcript
                    .push(Entry::Info(crate::state::budget_note(calls_left)));
            }
            // The harness's answers to the commands the model wrote in its
            // own prose. Published where a tool result would land, so the
            // continuation that follows reads as the reply to them — and
            // drawn by taking the marked lines out of that reply and giving
            // them the answers as their output, which is the one way the
            // live frame and a resume can agree about it.
            Event::CommandResults { record, lines } => {
                finish_streaming(state);
                state.answered(&lines, record);
            }
            // The marker is on the branch and this session's journal has
            // stopped. Nothing is drawn for it here: the driver says the
            // goodbye on the transcript and prints it to the terminal, and
            // the record is what `eidolon log` shows. Seed draws no `Note`
            // either, so a resume and the frame that just passed agree.
            Event::Quiesced { .. } => {}
        },
        UiMsg::VaultNote { id, result } => crate::vault::accept(state, id, result),
        UiMsg::Dialog(req) => {
            state.jump = None;
            state.vault.pending = None;
            if state.vault.current.is_none() && state.vault.tree.is_none() { crate::vault::close(state); }
            state.dialog = Some(Dialog {
                kind: req.kind,
                prompt: req.prompt,
                options: req
                    .options
                    .into_iter()
                    .map(|c| (c.label, c.description))
                    .collect(),
                keys: Vec::new(),
                weights: Vec::new(),
                selected: 0,
                input: Dialog::field(),
                reply: Some(req.reply),
                pick: None,
                body: String::new(),
                masked: false,
            });
        }
        UiMsg::Pick {
            title,
            items,
            pick,
            current,
        } => open_pick(state, title, items, pick, current),
        UiMsg::Ask {
            title,
            masked,
            run,
            key,
        } => open_ask(state, title, masked, run, key),
        UiMsg::TurnDone(r) => {
            state.working = false;
            // Repeat the settle line `settled` put on the transcript as a notice.
            if let (true, Some(Entry::Info(t))) = (r.is_ok(), state.transcript.last())
                && crate::life::DONE.iter().any(|w| t.starts_with(&w.to_lowercase()))
            {
                state.info(t.clone());
            }
            state.working_since = None;
            finish_streaming(state);
            refresh_usage(state);
            // The bus already said it, in red, as the turn failed; the
            // same words again as a faint note under it were a second
            // error where there was one. The note stays for a failure
            // that reached nobody's alert.
            if let Err(e) = r {
                let line = format!("error: {e}");
                if !matches!(state.transcript.last(), Some(Entry::Alert(a)) if *a == line) {
                    state.note(line);
                }
            }
        }
        // The driver is leaving, so the UI ends its own loop on the next
        // pass — after drawing this batch, which is how the goodbye note
        // that came with it gets its frame.
        UiMsg::Quit => state.quit = true,
        UiMsg::Info(s) => state.info(s),
        UiMsg::Note(s) => state.note(s),
        UiMsg::Tree(tree) => crate::vault::open_tree(state, tree),
        UiMsg::Page { title, text } => open_page(state, title, text),
        UiMsg::Context(r) => {
            let limit = state.context_limit();
            open_page(state, "context".into(), context_report_lines(&r, limit));
        }
        UiMsg::ModelChanged(m) => {
            state.note(format!("model → {m}"));
            state.model = m;
        }
        UiMsg::Resumed {
            path,
            cwd,
            model,
            branch,
            pins,
            fresh,
        } => {
            state.pins = pins;
            state.adopt(path, cwd, model, &branch);
            // Parks from the session before are not this session's live
            // waits: their blocks draw collapsed from the records, which
            // is what the seed knows how to say.
            state.waits.clear();
            if !fresh {
                let verb = if branch.is_empty() {
                    "started"
                } else {
                    "resumed"
                };
                state.note(format!(
                    "{verb} {} · {}",
                    crate::sessions::tilde(&state.session_path),
                    state.model
                ));
            }
        }
        UiMsg::Pins(p) => state.pins = p,
        UiMsg::Personas(p) => state.persona_keys = p,
        // Two reads can be in flight at once — a turn settling as the
        // panel opens — and the later-taken one is the truth whichever
        // lands first.
        UiMsg::Quota(r) => {
            if state.quota.as_ref().is_none_or(|q| q.read_ms <= r.read_ms) {
                state.quota = Some(r);
                refresh_usage(state);
            }
        }
    }
}

/// Put a picker up. Both sides raise one — the driver over [`UiMsg::Pick`]
/// for anything that needs the agent, `exec` directly for the palette —
/// and they should look and behave identically, so they build it here.
fn open_pick(
    state: &mut UiState,
    title: String,
    items: Vec<PickItem>,
    pick: PickAction,
    current: Option<String>,
) {
    state.jump = None;
    let selected = current
        .and_then(|c| items.iter().position(|i| i.key == c))
        .unwrap_or(0);
    state.dialog = Some(Dialog {
        kind: DialogKind::Pick,
        prompt: title,
        options: items
            .iter()
            .map(|i| (i.label.clone(), i.description.clone()))
            .collect(),
        weights: items.iter().map(|i| i.weight).collect(),
        keys: items.into_iter().map(|i| i.key).collect(),
        selected,
        input: Dialog::field(),
        reply: None,
        pick: Some(pick),
        body: String::new(),
        masked: false,
    });
}

/// Put a single-line text prompt up — a credential's home. `run` is the
/// command `submit` hands the typed text to, the way [`PickAction::Run`]
/// hands a picker's chosen key to one; `key` is carried alongside it (the
/// provider a login is for) since a value typed here answers a question
/// a previous command already narrowed. Cancelling, or submitting empty,
/// commits nothing.
fn open_ask(state: &mut UiState, title: String, masked: bool, run: String, key: String) {
    state.jump = None;
    state.dialog = Some(Dialog {
        kind: DialogKind::Ask,
        prompt: title,
        options: Vec::new(),
        weights: Vec::new(),
        keys: vec![key],
        selected: 0,
        input: Dialog::field(),
        reply: None,
        pick: Some(PickAction::Run(run)),
        body: String::new(),
        masked,
    });
}

/// Put a page up. A page is a dialog the way a picker is — it owns the
/// keyboard and takes its keys from a table — and unlike one it answers
/// nothing: it is read, scrolled and closed.
/// The usage page, redrawn from the ledger while it is up: a turn that
/// settles under the reader changes what the page says, and a page that
/// went stale the moment it was opened would have to be closed and
/// reopened to be believed.
fn refresh_usage(state: &mut UiState) {
    if state
        .dialog
        .as_ref()
        .is_some_and(|d| d.kind == DialogKind::Page && d.prompt == "usage")
    {
        let body = state.usage_page();
        if let Some(d) = state.dialog.as_mut() {
            d.body = body;
        }
    }
}

pub(crate) fn open_page(state: &mut UiState, title: String, text: String) {
    state.jump = None;
    state.dialog = Some(Dialog {
        kind: DialogKind::Page,
        prompt: title,
        options: Vec::new(),
        keys: Vec::new(),
        weights: Vec::new(),
        selected: 0,
        input: Dialog::field(),
        reply: None,
        pick: None,
        body: text,
        masked: false,
    });
}

/// Close whatever the model was in the middle of writing. An entry that
/// never got any text is dropped rather than left as a blank line: a
/// pure tool-use turn opens an assistant entry and never fills it.
fn finish_streaming(state: &mut UiState) {
    let empty = match state.transcript.last_mut() {
        Some(Entry::Assistant { streaming, text }) | Some(Entry::Thinking { streaming, text }) => {
            *streaming = false;
            text.is_empty()
        }
        _ => false,
    };
    if empty {
        state.transcript.pop();
    }
}

fn handle_key(
    state: &mut UiState,
    keys: &mut Keys,
    script: Option<&UiScript>,
    cmd_tx: &mpsc::UnboundedSender<Cmd>,
    k: ratatui::crossterm::event::KeyEvent,
) {
    // A set of letter tags owns the keyboard for the one or two presses
    // it lasts. It has to: every letter on it is a tag, including the
    // ones that are also commands, which is the same reason `f` holds
    // the keyboard open for its argument.
    if state.jump.is_some() {
        keys.reset();
        state.pending.clear();
        state.menu = None;
        jump_key(state, cmd_tx, k);
        return;
    }

    // A dialog and the minibuffer are modes to the hands, and they are
    // modes to the keymap too: each has its own table (`confirm`, `ask`,
    // `choose`, `pick`, `search`, `command`, `dispatch`), so the
    // which-key popup can promise what a key does there and a user can
    // rebind it. All of them take text — an unclaimed printable key is
    // typed into the dialog's field or the one-line buffer — so they are
    // text modes to the resolver, the way insert is.
    //
    // Every one-line mode is insert to the buffer, so a script with no
    // table for the one that is up gets insert's, which is the whole of
    // what such a line needs: keys are text, `ret` submits, `esc` takes
    // it down. That fallback used to name the *prompt's* mode, which was
    // right only while the `:` line was the prompt itself.
    let name = state.mode_name();
    let mode = match state.mini.is_some() && !keys.has(name) {
        true => "insert".to_string(),
        false => name.to_string(),
    };
    let text_mode = state.dialog.is_some() || state.mini.is_some() || state.prompt.mode.typing();
    match keys.press(&k, &mode, text_mode) {
        Resolved::Pending | Resolved::Unbound => {}
        Resolved::Literal(s) => {
            state.completing = None;
            if state.dialog.is_some() {
                dialog_literal(state, &s);
            } else {
                target(state).insert(&s);
            }
        }
        Resolved::Command { name, count, arg } => {
            // Anything but another tab ends the cycle, so the next tab
            // completes what is on the line now rather than the stem from
            // three edits ago.
            if !matches!(name.as_str(), "complete_next" | "complete_prev") {
                state.completing = None;
            }
            exec(
                state,
                script,
                cmd_tx,
                &name,
                count,
                arg.as_deref().unwrap_or(""),
            );
        }
    }
    state.pending = keys.pending_keys();
    // A chord wins the popup while one is open; otherwise the command line
    // gets it. They are never both live.
    state.menu = keys.menu(&mode).or_else(|| completions(state));
    // A key that entered trace mode has to leave a cursor behind it, not
    // wait for the next frame to grow one: the very next key is a motion.
    // Idempotent, so the draw's own call is a no-op.
    crate::trace::sync(state);
    sync_search(state);
}

/// The buffer the keyboard is aimed at: the [minibuffer](crate::state::Mini)
/// while one is up, the prompt otherwise.
///
/// Every motion, every verb and every change goes through this, which is
/// what makes a `:` line, a pattern and a dispatch utterance edit
/// exactly as a message does — `w`, `d`, `y`, `u`, the readline kills,
/// all of it — rather than each of them owning a hand-written subset. It
/// is also the whole of the routing: nothing else in this module needs
/// to know which line is in front.
fn target(state: &mut UiState) -> &mut Buffer {
    match &mut state.mini {
        Some(m) => &mut m.input,
        None => &mut state.prompt,
    }
}

/// What the half-typed command line could still become. This is the
/// search: the list narrows as you type, and `tab` walks it.
///
/// It is [`crate::complete`] drawn — name spot or argument spot, one
/// list either way — so the popup can never offer a row `tab` would not
/// write. The rows are numbered the same in both because a row with
/// nothing to insert is only ever produced when there is nothing else to
/// show; see the module note there.
fn completions(state: &UiState) -> Option<crate::state::Menu> {
    let typed = state.command_line()?;
    // While cycling, the list stays the one the *stem* matched — the line
    // itself now holds a candidate and would match only itself.
    let stem = state
        .completing
        .as_ref()
        .map(|(s, _)| s.as_str())
        .unwrap_or(typed);
    let c = crate::complete::complete(state, stem)?;
    Some(crate::state::Menu {
        title: c.title,
        items: c.rows.into_iter().map(|r| (r.label, r.help)).collect(),
        selected: Some(state.completing.as_ref().map(|(_, i)| *i).unwrap_or(0)),
    })
}

/// A set of letter tags, while it is up. Every printable key is a tag
/// character; anything else — `esc` above all — puts the tags away
/// without moving, and so does a letter no tag begins with.
fn jump_key(state: &mut UiState, cmd_tx: &mpsc::UnboundedSender<Cmd>, k: ratatui::crossterm::event::KeyEvent) {
    let Some(j) = &mut state.jump else { return };
    let printable = matches!(k.code, KeyCode::Char(_))
        && !k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.contains(KeyModifiers::ALT);
    let KeyCode::Char(c) = k.code else {
        state.jump = None;
        return;
    };
    if !printable {
        state.jump = None;
        return;
    }
    match j.press(c) {
        Step::Pending => {}
        Step::Miss => state.jump = None,
        Step::Go(i) => {
            let spot = j.spot(i).cloned();
            state.jump = None;
            if let Some(spot) = spot {
                if let Spot::Link { target, .. } = spot { crate::vault::request(state, cmd_tx, target); }
                else { land(state, &spot); }
            }
        }
    }
}

/// Where a tag goes — one arm per cursor the harness has, because a set
/// of tags may point at more than one of them at once.
fn land(state: &mut UiState, spot: &Spot) {
    match spot {
        Spot::Link { .. } => {}
        // A motion, so select mode extends to it rather than jumping —
        // `Buffer::goto` is where that is decided, not here.
        Spot::Word(at) => state.prompt.goto(*at),
        Spot::Hit(i) => {
            if let Some(entry) = state.search.as_mut().and_then(|s| s.goto(*i)) {
                search_landed(state, entry);
            }
        }
        // A word in the transcript has no cursor to become, so landing
        // on one **takes** it: the cells are selected — the same screen
        // selection a mouse drag makes, so it is lit and `y` copies it —
        // and the word goes to the register and the clipboard at once.
        // Which is the thing one actually wants a word out of a
        // transcript for: a path, an identifier, an error string, put
        // back into the next message with `p`.
        Spot::Cell { y, .. } if state.vault.active && state.tracing() => {
            let top = state.vault.page.as_ref().map_or(0, |d| d.selected);
            let row = top + y.saturating_sub(state.transcript_rect.y + 1) as usize;
            if let Some(line) = crate::vault::document(state).source_lines.get(row) {
                crate::vault::trace_goto(state, *line);
            }
        }
        Spot::Cell { x, y, len, text } => {
            state.selection = Some(Selection {
                from: (*x, *y),
                to: (x + len.saturating_sub(1), *y),
                dragging: false,
            });
            state.prompt.register = text.clone();
            state.copied_at = crate::clipboard::copy(text).then(std::time::Instant::now);
            state.info(format!("yanked `{text}` \u{2014} `p` pastes it"));
        }
        // The transcript's own cursor. Entering trace mode is part of
        // landing: a tag that put the cursor somewhere invisible and
        // left the keyboard in normal mode would have moved nothing the
        // operator can see.
        Spot::Block(entry) => {
            if !state.tracing() {
                match state.modes.mode("trace") {
                    Ok(m) => state.prompt.enter(m),
                    Err(e) => {
                        state.info(format!("trace mode is not in modes(): {e:#}"));
                        return;
                    }
                }
            }
            scroll_to(state, *entry);
            crate::trace::goto(state, *entry);
        }
    }
}

/// The search line's own commands, resolved from the `search` table of
/// the keymap. Returns whether `name` meant something here; a command
/// that does not — `quit`, say, or any of the buffer's own, which the
/// minibuffer now takes like any other text — falls through.
///
/// Three keys, where there used to be nine: the six motions and the
/// backspace are the buffer's now, because the pattern is written into a
/// [`Buffer`] rather than a one-line field of its own. What is left is
/// what only means something *to a search*.
fn search_exec(state: &mut UiState, name: &str) -> bool {
    if state.search.is_none() {
        return false;
    }
    match name {
        // Tags over the matches on screen — the reason this is faster
        // than `n`: the eye has already found the one it wants.
        "goto_hit" => tag_hits(state),
        // Open the box the match is hiding in, without leaving the
        // search. A hit inside a fold is counted onto the summary line
        // rather than lit, and this is the key that turns the count back
        // into the text — the search line says so while there is
        // something to open.
        "reveal_match" => reveal_match(state),
        _ => return false,
    }
    true
}

/// Search the active reading surface; note hits are rendered rows, transcript hits are entries.
fn refresh_search(state: &UiState, search: &mut Search, from: Option<usize>) {
    let from = from.unwrap_or(search.anchor);
    if state.vault.active {
        let entries: Vec<_> = crate::vault::document(state).lines.iter().map(|line| Entry::Assistant {
            text: line.spans.iter().map(|s| s.content.as_ref()).collect(), streaming: false,
        }).collect();
        search.refresh(&entries, "", from);
    } else { search.refresh(&state.transcript, state.prompt.text(), from); }
}

/// Keep the live search's pattern in step with the line being typed.
///
/// Called once, after every keystroke, rather than from each editing arm
/// — which is the point of the minibuffer being a whole [`Buffer`]: a
/// pattern can now change by way of a kill, a paste, an undo or a
/// yanked register as readily as by a letter, and one place that
/// notices is the only arrangement that stays true as the buffer grows
/// verbs.
fn sync_search(state: &mut UiState) {
    if !state.searching() {
        return;
    }
    let Some(typed) = state.mini.as_ref().map(|m| m.text().to_string()) else {
        return;
    };
    if state.search.as_ref().is_some_and(|s| s.pattern == typed) {
        return;
    }
    let Some(mut s) = state.search.take() else {
        return;
    };
    s.pattern = typed;
    // Anchored to where the view was when the search opened, not to
    // where the last keystroke scrolled it, or the preview would walk
    // forward through the session a letter at a time.
    refresh_search(state, &mut s, None);
    let found = s.entry();
    state.search = Some(s);
    if let Some(e) = found {
        scroll_to(state, e);
    }
}

/// Put `entry` on the first row of the transcript. The line arithmetic
/// is the renderer's, so that the scroll lands where the draw will put
/// it — see [`crate::render::lines_below`].
///
/// The one entry past the end is the draft (see
/// [`crate::search::scan_all`]), which is drawn in the prompt and so is
/// never anywhere but on the screen: landing on it follows the tail
/// rather than scrolling to a line that does not exist.
fn scroll_to(state: &mut UiState, entry: usize) {
    if state.vault.active {
        let last = crate::vault::document(state).lines.len().saturating_sub(crate::vault::layout(state).1);
        if let Some(d) = &mut state.vault.page { d.selected = entry.min(last); }
        if (state.tracing() || state.vault.tree.is_some()) && let Some(line) = crate::vault::document(state).source_lines.get(entry) {
            crate::vault::trace_goto(state, *line);
        }
        return;
    }
    if entry >= state.transcript.len() {
        state.set_scroll(0);
        return;
    }
    let below = crate::render::lines_below(state, state.view_width, entry);
    state.set_scroll(below.saturating_sub(state.view_height));
}

/// Re-derive `scroll_up` from where the last frame anchored the view, so
/// a scroll away from the live edge is held against the transcript's
/// **content** and not its bottom edge. `scroll_up` counts rows up from
/// the end, so left alone it is a scroll that *follows* — every row the
/// stream appends slides the window down over the thing being read, which
/// is the one thing a scroll up is for. The anchor is the bottom visible
/// row's place in its block; the rows below it are recounted each frame
/// (the walk in [`crate::render::lines_below`], bounded by how far the
/// view is scrolled, the same bound the draw's own walk already pays), so
/// growth *and* shrinkage below the window — a fold collapsing the run
/// that was scrolled past — both leave the reading line where it is.
///
/// [`UiState::set_scroll`] spends the anchor whenever the operator's own
/// hand moves the view; this is the other writer, and the frame that
/// follows re-anchors from wherever that hand landed.
fn hold_scroll(state: &mut UiState) {
    if let Some((entry, off)) = state.scroll_anchor {
        state.scroll_up =
            crate::render::lines_below(state, state.view_width, entry).saturating_sub(off + 1);
    }
}

/// The search landed on `entry`: scroll to it, and — in trace mode —
/// put the cursor on the block that holds it.
///
/// The cursor follows and the fold stays shut. A hit inside one is
/// already reported on the line standing in for it (`Ran 3 shell
/// commands · 2 matches`), so landing on that line is landing on the
/// match as far as the screen is concerned, and `l` is one key. An `n`
/// that unfolded a cluster on its own would rearrange the screen behind
/// a key whose whole job is to move a small distance through it.
fn search_landed(state: &mut UiState, entry: usize) {
    scroll_to(state, entry);
    if !state.vault.active && state.tracing() && entry < state.transcript.len() {
        crate::trace::goto(state, entry);
    }
}

/// `ret` on the minibuffer, which means three different things.
///
/// The snapshot is taken **before** the line comes down, because that is
/// what a script's `on_submit` reads to tell a command from an
/// utterance: `ui.rn` branches on `s.mode == "dispatch"`, and the line
/// is handed over whole — sigil and all — so `parse_command` still sees
/// `:model sonnet` and nothing in a user's script has to change.
fn submit_mini(
    state: &mut UiState,
    script: Option<&UiScript>,
    cmd_tx: &mpsc::UnboundedSender<Cmd>,
) {
    let Some(m) = state.mini.as_ref() else { return };
    let (kind, line, typed) = (m.kind, m.line(), m.submitted());
    if let MiniKind::Search(_) = kind {
        // Accepted: the line comes down, the search stays. `n` repeats
        // it and the matches keep their highlight until `esc`. In trace
        // mode the cursor takes the hit, so `l` opens whatever it is in.
        let found = state.search.as_ref().and_then(crate::search::Search::entry);
        close_mini(state, true);
        if let Some(e) = found.filter(|_| !state.vault.active && state.tracing()) {
            crate::trace::goto(state, e);
        }
        return;
    }
    let snapshot = state.snapshot();
    // A `:` line is done with when its command runs; a dispatch line is
    // not. Sending is no reason to leave insert — the prompt comes back
    // empty in the mode the message was written in — and the same holds
    // one rung along: a run of utterances costs one `!` and not one
    // each. So dispatch keeps its line, emptied, and every other kind
    // takes it down.
    match kind {
        MiniKind::Dispatch => {
            if let Some(m) = state.mini.as_mut() {
                let said = m.input.take();
                state
                    .mini_history
                    .insert(kind.as_str(), m.input.history.clone());
                if said.trim().is_empty() {
                    return;
                }
            }
        }
        _ => {
            close_mini(state, true);
        }
    }
    if typed.trim().is_empty() {
        return;
    }
    // The fallback is the meaning of the line's own sigil, so a harness
    // running without a script still has a command line and a dispatch
    // mode rather than sending `:quit` to the model.
    let fallback = match kind {
        MiniKind::Dispatch => Action::Dispatch(typed.clone()),
        _ => match line[1..].split_once(char::is_whitespace) {
            Some((n, a)) => Action::Command {
                name: n.to_string(),
                arg: a.trim().to_string(),
            },
            None => Action::Command {
                name: line[1..].to_string(),
                arg: String::new(),
            },
        },
    };
    let action = script
        .and_then(|s| s.on_submit(snapshot, &typed).ok())
        .unwrap_or(fallback);
    act(state, script, cmd_tx, action);
}

/// The command that opens each one-line mode — the one place the two
/// vocabularies are tied together, so `:enter_mode search` and `/` end
/// up in the same arm.
fn mini_command(kind: MiniKind) -> &'static str {
    match kind {
        MiniKind::Command => "command_line",
        MiniKind::Search(crate::search::Dir::Backward) => "rsearch",
        MiniKind::Search(_) => "search",
        MiniKind::Dispatch => "dispatch_mode",
    }
}

/// Open the search line: a live [`Search`] and the minibuffer to type
/// its pattern into. The anchor is where the view is now, so the
/// incremental preview starts from what you were looking at.
fn open_search(state: &mut UiState, dir: Dir) {
    state.jump = None;
    let origin = state.vault.page.as_ref().map_or(state.scroll_up, |d| d.selected);
    state.search = Some(Search::new(dir, origin, if state.vault.active { origin } else { state.view_entries.start }));
    open_mini(state, MiniKind::Search(dir));
}

/// Put a one-line mode in front of the prompt.
///
/// The buffer is built fresh each time, but its **history is not**: what
/// has been typed on each of the three lines is kept on the state and
/// handed back, so `:` remembers commands, `!` remembers utterances, and
/// neither of them is in the history of the messages you have sent. That
/// last part is new — the `:` line was the prompt, so every command ever
/// run was a message in the prompt's own history, between the things you
/// had actually said.
fn open_mini(state: &mut UiState, kind: MiniKind) {
    let mut m = crate::state::Mini::new(kind);
    m.input.history = state
        .mini_history
        .get(kind.as_str())
        .cloned()
        .unwrap_or_default();
    state.mini = Some(m);
    state.completing = None;
}

/// Take the minibuffer down, and hand back what was on it.
///
/// `keep` is whether what was typed stands. A submitted search keeps its
/// hits and its highlight — the line comes down and `n` goes on
/// repeating it — while a cancelled one puts the view back where it was
/// and forgets the whole search, as Helix does.
fn close_mini(state: &mut UiState, keep: bool) -> Option<crate::state::Mini> {
    let m = state.mini.take()?;
    state.completing = None;
    if !m.text().trim().is_empty() {
        let mut history = m.input.history.clone();
        history.push(m.text().to_string());
        state.mini_history.insert(m.kind.as_str(), history);
    }
    if matches!(m.kind, MiniKind::Search(_))
        && !keep
        && let Some(s) = state.search.take()
    {
        if state.vault.active {
            if let Some(d) = &mut state.vault.page { d.selected = s.origin; }
        } else { state.set_scroll(s.origin); }
    }
    Some(m)
}

/// `n`/`N`, `count` times. The rescan on each step is what keeps the
/// search honest while the model is still writing: the transcript grows
/// under a search that was made against a shorter one.
fn rep_search(state: &mut UiState, count: usize, same: bool) {
    if state.search.is_none() {
        state.info("no search yet — `/` starts one");
        return;
    }
    for _ in 0..count.max(1) {
        let Some(mut s) = state.search.take() else {
            return;
        };
        let here = s.entry().unwrap_or(s.anchor);
        refresh_search(state, &mut s, Some(here));
        let found = s.step(same);
        state.search = Some(s);
        match found {
            Some(e) => search_landed(state, e),
            None => {
                state.info("no matches");
                return;
            }
        }
    }
}

/// Open whatever is keeping the current match off the screen.
///
/// Usually that is the fold the hit sits in, and opening it draws every
/// call in that run with all of its output — the run was opened to find
/// something in it, so a capped output would only hide the match one
/// level further down. When the hit is not in a fold, the thing hiding
/// it is the detail cap, and the only knob for that is the global one.
fn reveal_match(state: &mut UiState) {
    if state.vault.active { return; }
    let Some(entry) = state.search.as_ref().and_then(crate::search::Search::entry) else {
        state.info("no match to open");
        return;
    };
    match crate::render::fold_at(state, entry) {
        Some(run) => {
            state.opened.push(run);
            if state.tracing() {
                crate::trace::goto(state, entry);
            }
        }
        None if state.detail != crate::state::Detail::Full => {
            state.detail = crate::state::Detail::Full
        }
        None => state.info("nothing left to open — the match is already drawn"),
    }
}

/// A click on a folded run opens it, and a click on an opened one folds
/// it back. The summary line is the affordance — it is the thing on
/// screen that stands for what is hidden — so it is what answers a
/// click, and `space o` stays the way to open everything at once.
fn toggle_fold_at(state: &mut UiState, at: (u16, u16)) {
    if state.vault.active || !state.transcript_rect.contains(at.into()) {
        return;
    }
    let Some(entry) = crate::render::entry_at(state, state.transcript_rect, at.1) else {
        return;
    };
    if let Some(run) = crate::render::opened_at(state, entry) {
        state.opened.retain(|r| *r != run);
        state.info("folded");
    } else if let Some(run) = crate::render::fold_at(state, entry) {
        state.opened.push(run);
        state.info("opened");
    }
}

/// Tag the matches that are on screen. Only those: a tag is worth a
/// letter because you can see where it points, and one for every match
/// in the session would spend the alphabet on the ones you cannot.
fn tag_hits(state: &mut UiState) {
    let Some(s) = &state.search else {
        state.info("no search to tag");
        return;
    };
    let view = if state.vault.active {
        let top = state.vault.page.as_ref().map_or(0, |d| d.selected);
        top..top + crate::vault::layout(state).1
    } else { state.view_entries.clone() };
    // Two hits inside one fold are drawn as one line, so they are one
    // target: a second letter pointing at the same row would spend the
    // alphabet without buying a jump, and would be drawn nowhere.
    let mut blocks: Vec<usize> = Vec::new();
    let mut on_screen: Vec<usize> = Vec::new();
    for (i, h) in s
        .hits
        .iter()
        .enumerate()
        .filter(|(_, h)| view.contains(&h.entry))
    {
        let block = if state.vault.active { h.entry } else { crate::render::fold_at(state, h.entry).unwrap_or(h.entry) };
        if !blocks.contains(&block) {
            blocks.push(block);
            on_screen.push(i);
        }
    }
    state.jump = Jump::new(on_screen.into_iter().map(Spot::Hit).collect());
    if state.jump.is_none() {
        state.info("no matches on screen to tag");
    }
}

/// A dialog's own commands, resolved from the table named after its
/// kind (`confirm`, `choose`, `pick`). Returns whether `name`
/// meant something here. A dialog used to own the keyboard outright,
/// which kept it out of the which-key popup and out of a user's reach;
/// now `y` is `submit` and `n` is `dialog_deny` in a table like any
/// other, and the popup can say so.
fn dialog_exec(
    state: &mut UiState,
    script: Option<&UiScript>,
    cmd_tx: &mpsc::UnboundedSender<Cmd>,
    name: &str,
) -> bool {
    let Some(d) = &mut state.dialog else {
        return false;
    };
    if d.kind == DialogKind::Ask {
        match name {
            "cancel" => state.dialog = None,
            "submit" => {
                let secret = d.input.text().trim().to_string();
                let pick = d.pick.clone();
                let provider = d.keys.first().cloned().unwrap_or_default();
                state.dialog = None;
                if secret.is_empty() {
                    state.info("login cancelled: no key entered");
                } else if let Some(PickAction::Run(name)) = pick {
                    // The unit separator can never come off a keyboard, so
                    // it is safe as the seam between the provider a picker
                    // already chose and the secret just typed — the only
                    // door this text goes through is straight back into
                    // the same command, never the `:` line or a Record.
                    act(
                        state,
                        script,
                        cmd_tx,
                        Action::Command {
                            name,
                            arg: format!("{provider}\u{1}{secret}"),
                        },
                    );
                }
            }
            "delete_char_backward" => d.input.delete_char_backward(),
            "delete_char_forward" => d.input.delete_char_forward(),
            "move_char_left" => d.input.move_char_left(),
            "move_char_right" => d.input.move_char_right(),
            "goto_line_start" | "goto_first_nonwhitespace" => d.input.goto_line_start(),
            "goto_line_end" | "goto_line_end_newline" => d.input.goto_line_end_newline(),
            _ => return false,
        }
        return true;
    }
    if d.kind == DialogKind::Pick {
        let visible = d.visible();
        let pos = visible.iter().position(|&i| i == d.selected).unwrap_or(0);
        match name {
            "cancel" => state.dialog = None,
            "move_line_up" | "scroll_line_up" => {
                if let Some(&i) = visible.get(pos.saturating_sub(1)) {
                    d.selected = i;
                }
            }
            "move_line_down" | "scroll_line_down" => {
                if let Some(&i) = visible.get((pos + 1).min(visible.len().saturating_sub(1))) {
                    d.selected = i;
                }
            }
            "submit" => {
                if let (Some(&i), Some(pick)) = (visible.get(pos), d.pick.clone()) {
                    let key = d.keys.get(i).cloned().unwrap_or_default();
                    state.dialog = None;
                    match pick {
                        PickAction::Run(name) => {
                            act(state, script, cmd_tx, Action::Command { name, arg: key });
                        }
                        // Hand the half-written line back rather than
                        // running it: the operator still owes an argument.
                        PickAction::Edit(name) => {
                            open_mini(state, MiniKind::Command);
                            target(state).insert(&format!("{name} {key} "));
                        }
                    }
                } else {
                    state.dialog = None;
                }
            }
            // Backspacing widens the list, so the best match moves
            // too — same rule as typing.
            "delete_char_backward" => {
                d.input.delete_char_backward();
                d.selected = d.visible().first().copied().unwrap_or(0);
            }
            "delete_char_forward" => {
                d.input.delete_char_forward();
                d.selected = d.visible().first().copied().unwrap_or(0);
            }
            "move_char_left" => d.input.move_char_left(),
            "move_char_right" => d.input.move_char_right(),
            "goto_line_start" | "goto_first_nonwhitespace" => d.input.goto_line_start(),
            "goto_line_end" | "goto_line_end_newline" => d.input.goto_line_end_newline(),
            _ => return false,
        }
        return true;
    }
    // A page scrolls in the lines the frame drew, so the same layout
    // the renderer used says how far a page down is and where the end
    // lies — a `j` past the end would otherwise leave the operator
    // pressing `k` through rows that were never drawn.
    if d.kind == DialogKind::Page {
        let (width, rows) = crate::render::page_layout(state.screen);
        let last = crate::render::page_lines(&d.body, width).len().saturating_sub(rows);
        let half = (rows / 2).max(1);
        match name {
            "cancel" => state.dialog = None,
            "scroll_line_up" | "move_line_up" => d.selected = d.selected.saturating_sub(1),
            "scroll_line_down" | "move_line_down" => d.selected = (d.selected + 1).min(last),
            "scroll_half_up" => d.selected = d.selected.saturating_sub(half),
            "scroll_half_down" => d.selected = (d.selected + half).min(last),
            "scroll_page_up" => d.selected = d.selected.saturating_sub(rows),
            "scroll_page_down" => d.selected = (d.selected + rows).min(last),
            "transcript_top" => d.selected = 0,
            "transcript_bottom" => d.selected = last,
            "yank" => {
                let text = d.body.clone();
                state.copied_at = crate::clipboard::copy(&text).then(std::time::Instant::now);
                state.info(if state.copied_at.is_some() {
                    "copied the page"
                } else {
                    "could not reach the clipboard"
                });
            }
            _ => return false,
        }
        return true;
    }
    let answer: Option<Option<String>> = match (&d.kind, name) {
        (_, "cancel") => Some(None),
        (DialogKind::Confirm, "submit") => Some(Some("yes".into())),
        (DialogKind::Confirm, "dialog_deny") => Some(Some("no".into())),
        (DialogKind::Choose, "move_line_up" | "scroll_line_up") => {
            d.selected = d.selected.saturating_sub(1);
            None
        }
        (DialogKind::Choose, "move_line_down" | "scroll_line_down") => {
            d.selected = (d.selected + 1).min(d.options.len().saturating_sub(1));
            None
        }
        (DialogKind::Choose, "submit") => Some(d.options.get(d.selected).map(|o| o.0.clone())),
        _ => return false,
    };
    if let Some(a) = answer {
        if let Some(reply) = d.reply.take() {
            let _ = reply.send(a);
        }
        state.dialog = None;
    }
    true
}

/// A character typed at a dialog: into its field for the kinds that have
/// one, a numbered choice for `choose`, nothing for a confirm box.
fn dialog_literal(state: &mut UiState, text: &str) {
    let Some(d) = &mut state.dialog else { return };
    match d.kind {
        // Typing re-aims at the best match rather than keeping whatever
        // row was under the highlight. The list is ranked now
        // ([`crate::state::Dialog::visible`]), so the top row is the
        // answer to what has been typed so far; leaving the highlight
        // behind on a row that merely *survived* the filter is how a
        // picker ends up running the wrong thing on a confident `ret`.
        // An explicit `j`/`k` afterwards still moves and stays moved.
        DialogKind::Pick => {
            d.input.insert(text);
            d.selected = d.visible().first().copied().unwrap_or(0);
        }
        // Nothing to re-rank: one field, and it holds only what was typed
        // into it.
        DialogKind::Ask => d.input.insert(text),
        DialogKind::Choose => {
            let Some(i) = text.chars().next().and_then(|c| c.to_digit(10)) else {
                return;
            };
            let i = i as usize;
            if i >= 1 && i <= d.options.len() {
                let answer = d.options[i - 1].0.clone();
                if let Some(reply) = d.reply.take() {
                    let _ = reply.send(Some(answer));
                }
                state.dialog = None;
            }
        }
        DialogKind::Confirm | DialogKind::Page => {}
    }
}

/// Run `n` copies of a buffer command on whichever buffer the keyboard
/// is aimed at — this is what a count multiplies.
fn rep(state: &mut UiState, n: usize, f: impl Fn(&mut Buffer)) {
    for _ in 0..n.max(1) {
        f(target(state));
    }
}

/// Run a command by name. The names are [`crate::command`]'s, and a key
/// bound to a [`Run::Driver`] one is forwarded rather than refused — which
/// is what lets `space m` be `model` and not a second spelling of it.
/// A resolved program, or a complaint on the transcript and `None`.
///
/// An editor this machine does not have is worth a sentence naming the
/// two places it could be declared: the alternative is `T H` opening a
/// bare terminal, which looks exactly like `T T` and teaches the operator
/// nothing about why.
fn named(state: &mut UiState, argv: Vec<String>, missing: &str) -> Option<Vec<String>> {
    if argv.is_empty() {
        state.alert(missing);
        return None;
    }
    Some(argv)
}

/// The path the transcript's cursor is standing on, if it is standing on
/// a tool call that named one.
///
/// The **last** path under the cursor rather than the first: a cluster is
/// everything the model did between one piece of prose and the next, and
/// the question `T H` asks of a cluster is *open what it just changed* —
/// so a run that read three files and then wrote one opens the one it
/// wrote. On a single call the two answers are the same.
///
/// `None` outside trace mode, which is the whole of what makes `T H` mean
/// "here" at the prompt and "this file" on the cursor without either
/// being a special case: there is no cursor to ask.
fn cursor_path(state: &UiState) -> Option<std::path::PathBuf> {
    if !state.tracing() {
        return None;
    }
    crate::trace::selection(state)
        .filter_map(|i| match state.transcript.get(i) {
            Some(Entry::Tool { input, .. }) => crate::launch::subject(input),
            _ => None,
        })
        .next_back()
}

/// Open `program` in a new window, aimed at the typed argument, then the
/// trace cursor, then the session's own directory.
///
/// `takes_path` is whether the program is one that opens *something*. An
/// editor and a file manager are, and are handed the file under the
/// cursor — or, with nothing under it, the directory itself, because `hx`
/// with no argument is a blank scratch buffer and `hx DIR` is a file
/// picker on the directory the operator was just looking at. A shell and
/// a second harness are not: both read the working directory the window
/// was opened with, and a path in argv would be a command to run or a
/// clap error.
///
/// `None` for the program is one that could not be resolved, and whoever
/// resolved it has already said why.
fn open_window(state: &mut UiState, arg: &str, program: Option<Vec<String>>, takes_path: bool) {
    let Some(mut argv) = program else { return };
    let typed = (!arg.trim().is_empty()).then(|| eidolon_tools::image::expand_tilde(arg));
    let (dir, file) = crate::launch::aim(
        std::path::Path::new(&state.cwd),
        typed.or_else(|| cursor_path(state)).as_deref(),
    );
    // The directory always places the *window*; only a program that opens
    // things is told which one. So `T T` on a call opens a shell beside
    // the file rather than trying to run it — the same distinction the
    // trailing-`-e` rule in `launch.rs` draws.
    if takes_path {
        argv = crate::launch::with_path(
            argv,
            &file.unwrap_or_else(|| dir.clone()).display().to_string(),
        );
    }
    spawn_window(state, &dir, &argv);
}

/// Spawn, and say what was spawned. The line is [`Entry::Info`] on
/// success and [`Entry::Alert`] on failure, because a window that came up
/// on another workspace and a window that never came up look identical
/// from in here.
/// Strike (or restore) whatever the trace cursor covers.
///
/// The resolution happens here and not on the driver because the cursor is
/// a UI thing: the driver has the session and no idea what is selected.
/// What crosses the channel is record ids, which mean the same thing on
/// both sides — the entry indices they came from do not.
fn trace_strike(state: &mut UiState, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
    let sel = crate::trace::selection(state);
    let mut ids: Vec<eidolon_core::session::RecordId> = Vec::new();
    for i in sel {
        if let Some(id) = state.record_at(i)
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        state.info("nothing here to strike — this block is not on the branch yet");
        return;
    }
    // A selection that is entirely struck is a request to put it back.
    let restoring = ids.iter().all(|id| state.struck.contains(id));
    for id in &ids {
        if restoring {
            state.struck.remove(id);
        } else {
            state.struck.insert(*id);
        }
    }
    let arg = ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let name = if restoring { "restore" } else { "exclude" };
    let _ = cmd_tx.send(Cmd::Command {
        name: name.to_string(),
        arg,
    });
}

/// The one driver command the UI answers before the driver sees it.
///
/// Bare `fork` means the record the trace cursor stands on, and the cursor is
/// UI state: the driver has the session and no idea what is selected. Every
/// other spelling goes straight through — `:fork 12`, which the CLI shares,
/// and a named record off the tree.
fn ui_owns(name: &str, arg: &str) -> bool {
    name == "fork" && arg.trim().is_empty()
}

/// Fork at the record the trace cursor stands on.
///
/// The picker this replaces listed exactly the rows `:tree` prints, and trace
/// mode is already a cursor over those rows — so the cursor *is* the picker:
/// `K`, `j` to the row, `space f`. With no cursor there is nothing to fork at,
/// and the command says so rather than opening a dialog over the transcript.
///
/// The resolution happens here and not on the driver for `trace_strike`'s
/// reason: what crosses the channel is a record id, which means the same thing
/// on both sides, and the entry index it came from does not.
fn fork_here(state: &mut UiState, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
    if !state.tracing() {
        state.info(
            "fork at a record — `K` puts a cursor on the transcript, `j` to a row, then fork again",
        );
        return;
    }
    let id = if state.vault.active {
        crate::vault::tree_record(state).map(|r| r.id)
    } else { crate::trace::selection(state).find_map(|i| state.record_at(i)) };
    let Some(id) = id else {
        state.info("nothing to fork at — this block is not on the branch yet");
        return;
    };
    let _ = cmd_tx.send(Cmd::Command {
        name: "fork".to_string(),
        arg: id.to_string(),
    });
}

fn spawn_window(state: &mut UiState, dir: &std::path::Path, argv: &[String]) {
    let opened = state.launcher.open(dir, argv);
    match opened {
        Ok(line) => state.info(line),
        Err(e) => state.alert(format!("{e:#}")),
    }
}

fn exec(
    state: &mut UiState,
    script: Option<&UiScript>,
    cmd_tx: &mpsc::UnboundedSender<Cmd>,
    name: &str,
    count: usize,
    arg: &str,
) {
    if ui_owns(name, arg) {
        fork_here(state, cmd_tx);
        return;
    }
    if matches!(command::lookup(name), Some(c) if c.run == Run::Driver) {
        let _ = cmd_tx.send(Cmd::Command {
            name: name.to_string(),
            arg: arg.to_string(),
        });
        return;
    }
    // A dialog or the search line answers first while it is up: `submit`
    // there is the dialog's enter, not the prompt's. What neither claims
    // falls through to the prompt.
    if state.dialog.is_some() && dialog_exec(state, script, cmd_tx, name) {
        return;
    }
    if state.dialog.is_none() && state.vault.active && state.mini.is_none()
        && crate::vault::exec(state, cmd_tx, name, count) { return; }
    if state.searching() && search_exec(state, name) {
        return;
    }
    // The char-takers (`f`, `t`, `r`) want one character, whether it came
    // from the keymap holding the keyboard open or from a `:replace x`.
    let ch = arg.chars().next();
    let page = state.view_height.max(1);
    let half = (page / 2).max(1);
    match name {
        // ------------------------------------------------------- modes
        "insert_mode" => target(state).enter_insert(false),
        "append_mode" => target(state).enter_insert(true),
        "insert_at_line_start" => {
            target(state).goto_first_nonwhitespace();
            target(state).enter_insert(false);
        }
        "insert_at_line_end" => {
            target(state).goto_line_end();
            target(state).enter_insert(true);
        }
        "open_below" => target(state).open_line(true),
        "open_above" => target(state).open_line(false),
        // Through `enter`, not `enter_normal`: the caret-to-block shift
        // is what *leaving insert* needs, and `esc` from a mode that was
        // never typing — select, trace — would otherwise walk the prompt's
        // cursor one character left every time it was pressed. `v` out of
        // select never did that, so the two ways out disagreed.
        //
        // From a one-line mode it means the same thing it means anywhere
        // — stop typing this — and the way to stop typing a `:` line is
        // for the line to go away. A script that binds `esc` there to
        // `normal_mode` rather than to `cancel` therefore gets what it
        // meant, which the old command line could not offer: it *was*
        // the prompt, so leaving insert left the colon sitting in it.
        "normal_mode" if state.mini.is_some() => {
            close_mini(state, false);
        }
        "normal_mode" => state.prompt.enter(Mode::normal()),
        "select_mode" => target(state).toggle_select(),
        // Any editing mode the script's table defines, by name — the way
        // into a mode of the user's own. With no name, a picker over the
        // table, as every command with an argument has.
        "enter_mode" => {
            let name = arg.trim();
            // A one-line mode is *opened* rather than entered: it is a
            // buffer of its own in front of the prompt, not a mode the
            // prompt is put into. Named here so that `:enter_mode
            // dispatch` and the picker's row for it reach the same thing
            // the `!` key does.
            if let Some(kind) = MiniKind::parse(name) {
                return exec(state, script, cmd_tx, mini_command(kind), 1, "");
            }
            if name.is_empty() {
                let items = state
                    .modes
                    .rows()
                    .filter(|d| d.kind.is_some() || MiniKind::parse(&d.name).is_some())
                    .map(|d| PickItem {
                        key: d.name.clone(),
                        label: d.name.clone(),
                        description: Some(format!(
                            "{} · {}",
                            d.label,
                            d.kind.map_or("one line", crate::edit::Kind::as_str)
                        )),
                        weight: 0,
                    })
                    .collect();
                let current = Some(target(state).mode.as_str().to_string());
                open_pick(
                    state,
                    "modes".into(),
                    items,
                    PickAction::Run("enter_mode".into()),
                    current,
                );
                return;
            }
            match state.modes.mode(name) {
                Ok(m) => target(state).enter(m),
                Err(e) => state.info(format!("{e:#}")),
            }
        }

        // ------------------------------------------------------ motion
        "move_char_left" => rep(state, count, |b| b.move_char_left()),
        "move_char_right" => rep(state, count, |b| b.move_char_right()),
        "move_line_up" => rep(state, count, |b| b.move_line_up()),
        "move_line_down" => rep(state, count, |b| b.move_line_down()),
        "move_next_word_start" => rep(state, count, |b| b.move_next_word_start(false)),
        "move_next_word_end" => rep(state, count, |b| b.move_next_word_end(false)),
        "move_prev_word_start" => rep(state, count, |b| b.move_prev_word_start(false)),
        "move_next_long_word_start" => rep(state, count, |b| b.move_next_word_start(true)),
        "move_next_long_word_end" => rep(state, count, |b| b.move_next_word_end(true)),
        "move_prev_long_word_start" => rep(state, count, |b| b.move_prev_word_start(true)),
        "goto_line_start" => target(state).goto_line_start(),
        "goto_line_end" => target(state).goto_line_end(),
        "goto_line_end_newline" => target(state).goto_line_end_newline(),
        "goto_first_nonwhitespace" => target(state).goto_first_nonwhitespace(),
        "goto_buffer_start" => {
            // `gg` with a count is Helix's line goto; bare, it is the top.
            if count == 0 {
                target(state).goto_buffer_start()
            } else {
                target(state).goto_line(count)
            }
        }
        "goto_buffer_end" => target(state).goto_buffer_end(),
        "goto_line" => target(state).goto_line(count),
        "goto_column" => target(state).goto_column(count),
        "goto_next_paragraph" => rep(state, count, |b| b.goto_next_paragraph()),
        "goto_prev_paragraph" => rep(state, count, |b| b.goto_prev_paragraph()),
        "find_next_char" => {
            state.last_motion = Some(("find_next_char".into(), arg.to_string()));
            rep(state, count, |b| {
                ch.into_iter().for_each(|c| b.find_char(c, true, false))
            })
        }
        "till_next_char" => {
            state.last_motion = Some(("till_next_char".into(), arg.to_string()));
            rep(state, count, |b| {
                ch.into_iter().for_each(|c| b.find_char(c, true, true))
            })
        }
        "find_prev_char" => {
            state.last_motion = Some(("find_prev_char".into(), arg.to_string()));
            rep(state, count, |b| {
                ch.into_iter().for_each(|c| b.find_char(c, false, false))
            })
        }
        "till_prev_char" => {
            state.last_motion = Some(("till_prev_char".into(), arg.to_string()));
            rep(state, count, |b| {
                ch.into_iter().for_each(|c| b.find_char(c, false, true))
            })
        }
        "match_bracket" => {
            target(state).match_bracket();
            state.last_motion = Some(("match_bracket".into(), String::new()));
        }
        "repeat_last_motion" => {
            if let Some((name, chars)) = state.last_motion.clone() {
                exec(state, script, cmd_tx, &name, count.max(1), &chars);
            }
        }

        // --------------------------------------------------- selection
        "select_line" => rep(state, count, |b| b.select_line()),
        "select_all" => target(state).select_all(),
        "collapse_selection" => target(state).collapse(),
        "flip_selections" => target(state).flip_selection(),
        "extend_to_line_bounds" => target(state).extend_to_line_bounds(),
        "shrink_to_line_bounds" => target(state).shrink_to_line_bounds(),
        "select_object_inner" => {
            if let Some(c) = ch {
                target(state).select_object(true, c);
            }
        }
        "select_object_around" => {
            if let Some(c) = ch {
                target(state).select_object(false, c);
            }
        }
        "surround_add" => {
            if let Some(c) = ch {
                target(state).surround_add(c);
            }
        }
        "surround_replace" => {
            let mut pair = arg.chars();
            if let (Some(from), Some(to)) = (pair.next(), pair.next()) {
                target(state).surround_replace(from, to);
            }
        }
        "surround_delete" => {
            if let Some(c) = ch {
                target(state).surround_delete(c);
            }
        }

        // ------------------------------------------------------ change
        "delete_selection" => target(state).delete_selection(),
        "change_selection" => target(state).change_selection(),
        "delete_selection_noyank" => target(state).delete_selection_noyank(),
        "change_selection_noyank" => target(state).change_selection_noyank(),
        "change_to_line_end" => target(state).change_to_line_end(),
        "yank" => target(state).yank(),
        "paste_after" => rep(state, count, |b| b.paste(true)),
        "paste_before" => rep(state, count, |b| b.paste(false)),
        "replace_with_yanked" => target(state).replace_with_yanked(),
        "replace" => {
            if let Some(c) = ch {
                target(state).replace_char(c);
            }
        }
        "switch_case" => target(state).switch_case(),
        "switch_to_lowercase" => target(state).switch_case_to(false),
        "switch_to_uppercase" => target(state).switch_case_to(true),
        "join_selections" => target(state).join_selections(count),
        "indent" => rep(state, count, |b| b.indent()),
        "unindent" => rep(state, count, |b| b.unindent()),
        "increment" => target(state).bump_number(count.max(1) as i64),
        "decrement" => target(state).bump_number(-(count.max(1) as i64)),
        "repeat_last_insert" => target(state).repeat_insert(),
        "commit_undo_checkpoint" => target(state).checkpoint(),
        "add_newline_below" => target(state).add_newline(true),
        "add_newline_above" => target(state).add_newline(false),
        "undo" => rep(state, count, |b| b.undo()),
        "redo" => rep(state, count, |b| b.redo()),
        // A one-line buffer with nothing on it has nothing to delete, so
        // the backspace that would do nothing instead does what `esc`
        // does: takes the line down. Emptied is how a `:` or a `/` is
        // usually left — type the pattern, backspace it away — and the
        // key that emptied it is the one the hand is already on, where
        // `esc` is a second press nobody thinks of at the end of a word.
        // Only the minibuffer: the prompt is not a line to be left, and a
        // dialog's field has a list under it that empty is a filter for.
        "delete_char_backward" if state.mini.as_ref().is_some_and(|m| m.input.is_empty()) => {
            close_mini(state, false);
        }
        "delete_char_backward" => rep(state, count, |b| b.delete_char_backward()),
        "delete_char_forward" => rep(state, count, |b| b.delete_char_forward()),
        // Readline's `C-t`. Wherever the caret is, the character behind
        // it is what moves.
        "transpose_chars" => rep(state, count, |b| b.transpose_chars()),
        // Readline's kills, on every surface the buffer reaches: the
        // prompt, the `:` line, the pattern, an utterance, a dialog's
        // field. See [`Buffer::delete_to_line_start`].
        "delete_to_line_start" => rep(state, count, |b| b.delete_to_line_start()),
        "delete_to_line_end" => rep(state, count, |b| b.delete_to_line_end()),
        "delete_word_backward" => rep(state, count, |b| b.delete_word_backward()),
        "delete_word_forward" => rep(state, count, |b| b.delete_word_forward()),
        "insert_newline" => target(state).insert("\n"),
        "clear_prompt" => target(state).clear(),
        "history_prev" => target(state).history_prev(),
        "history_next" => target(state).history_next(),

        // -------------------------------------------------- transcript
        "scroll_line_up" => state.set_scroll(state.scroll_up + count.max(1)),
        "scroll_line_down" => state.set_scroll(state.scroll_up.saturating_sub(count.max(1))),
        "scroll_half_up" => state.set_scroll(state.scroll_up + half * count.max(1)),
        "scroll_half_down" => state.set_scroll(state.scroll_up.saturating_sub(half * count.max(1))),
        "scroll_page_up" => state.set_scroll(state.scroll_up + page * count.max(1)),
        "scroll_page_down" => state.set_scroll(state.scroll_up.saturating_sub(page * count.max(1))),
        // Clamped against the real height after the next draw.
        "transcript_top" => state.set_scroll(usize::MAX),
        "transcript_bottom" => state.set_scroll(0),
        "trace_strike" => trace_strike(state, cmd_tx),
        "cycle_detail" => state.detail = state.detail.next(),

        // ------------------------------------------------------- trace
        // A cursor on the transcript. `trace_mode` is a toggle so that
        // the key that opens it closes it — the detail knob it replaces
        // on `K` was a cycle, and the hand that reached for `K` was
        // reaching for "show me this", not for "step the whole session
        // up one level".
        "trace_mode" => match state.tracing() {
            true => state.prompt.enter_normal(),
            false => match state.modes.mode("trace") {
                Ok(m) => state.prompt.enter(m),
                Err(e) => state.info(format!("trace mode is not in modes(): {e:#}")),
            },
        },
        // `:trace [what]` — a filter for the cursor's walk. Setting one
        // enters the mode and lands on the first block that matches:
        // `:trace edit` and `j`/`k` hop edit call to edit call, `:trace
        // call` every call, `:trace *` the calls that changed something.
        // No argument clears it, and leaving the mode does too.
        "trace" => {
            let f = arg.trim().to_string();
            if f.is_empty() {
                state.trace_filter = None;
                state.info("trace: walking every block again");
            } else {
                if !state.tracing() {
                    match state.modes.mode("trace") {
                        Ok(m) => state.prompt.enter(m),
                        Err(e) => state.info(format!("trace mode is not in modes(): {e:#}")),
                    }
                }
                state.trace_filter = Some(f.clone());
                match crate::trace::walkable(state).first() {
                    Some(b) => crate::trace::goto(state, b.start),
                    None => state.info(format!("trace: nothing matches `{f}` yet")),
                }
            }
        }
        // Every fold at once needs no cursor, so it is the one pair
        // that works from anywhere.
        "trace_open_all" | "trace_close_all" => {
            let open = name == "trace_open_all";
            match crate::trace::fold_all(state, open) {
                0 => state.info(if open {
                    "nothing folded to open"
                } else {
                    "no folds are open"
                }),
                n => state.info(format!(
                    "{n} fold{} {}",
                    if n == 1 { "" } else { "s" },
                    if open { "opened" } else { "closed" }
                )),
            }
        }
        _ if name.starts_with("trace_") && !state.tracing() => state.info(NO_TRACE),
        "link_next" => crate::vault::select(state, false),
        "link_prev" => crate::vault::select(state, true),
        "link_open" => crate::vault::follow(state, cmd_tx),
        "link_back" => crate::vault::back(state),
        "link_close" => crate::vault::close(state),
        "trace_next" => crate::trace::step(state, count.max(1) as isize),
        "trace_prev" => crate::trace::step(state, -(count.max(1) as isize)),
        "trace_first" => crate::trace::edge(state, false),
        "trace_last" => crate::trace::edge(state, true),
        "trace_open" => {
            if !crate::trace::open(state) {
                state.info(nothing_to_open(state));
            }
        }
        "trace_close" => {
            if !crate::trace::close(state) {
                state.info("nothing to close here — this block is not inside a fold");
            }
        }
        "trace_toggle" => {
            if !crate::trace::open(state) && !crate::trace::close(state) {
                state.info(nothing_to_open(state));
            }
        }
        "trace_extend" => crate::trace::extend(state),
        // The clipboard *and* the prompt's register, so `p` pastes what
        // was just copied — quoting a tool's output back at the model is
        // most of why one yanks from the transcript at all.
        "trace_quote" => state.info("reference quoting needs a vault document in trace mode"),
        "trace_yank" => {
            let text = crate::trace::yanked(state);
            if text.trim().is_empty() {
                state.info("nothing to yank here");
                return;
            }
            let n = crate::trace::selection(state).len();
            state.prompt.register = text.clone();
            state.copied_at = crate::clipboard::copy(&text).then(std::time::Instant::now);
            state.info(format!(
                "yanked {n} block{} \u{2014} `p` pastes them",
                if n == 1 { "" } else { "s" }
            ));
        }

        // --------------------------------------------------- the search
        "search" => open_search(state, Dir::Forward),
        "rsearch" => open_search(state, Dir::Backward),
        "search_next" => rep_search(state, count, true),
        "search_prev" => rep_search(state, count, false),
        // Helix's `*`: take the selection as the thing to look for,
        // escaped, because a selection is text and not a pattern.
        // Helix's `*` on whichever surface has a selection: the prompt
        // everywhere else, the transcript in trace mode, where the
        // prompt is not what the hands are pointed at.
        "search_selection" => {
            let selected = if state.vault.active && state.tracing() { crate::vault::trace_text(state) } else { match state.tracing() {
                true => crate::trace::yanked(state),
                false => target(state).selection(),
            } };
            let pattern = crate::search::as_text(selected.trim());
            if pattern.is_empty() {
                state.info("nothing selected to search for");
                return;
            }
            let mut s = Search::new(Dir::Forward, state.scroll_up, state.view_entries.start);
            s.pattern = pattern;
            // Nothing left to type, so the line never comes up: this is
            // a search with no minibuffer, which is exactly what an
            // accepted one is.
            refresh_search(state, &mut s, None);
            let found = s.entry();
            state.search = Some(s);
            if let Some(e) = found {
                search_landed(state, e);
            }
        }
        "goto_hit" => tag_hits(state),
        "reveal_match" => reveal_match(state),
        // Helix's `gw`, aimed the way Helix aims it: at **what is on the
        // screen**. It used to tag the prompt's words alone, which is
        // backwards — the prompt is where you are already typing, and
        // the conversation is the part you cannot otherwise reach — so
        // it complained about an empty prompt while a screenful of text
        // sat above it untagged. Now one press labels the visible
        // transcript *and* the draft, and each label knows where it
        // lands; see [`crate::jump::Spot`].
        "goto_word" => {
            let mut spots = crate::render::screen_words(state, state.transcript_rect);
            spots.extend(state.prompt.word_starts().into_iter().map(Spot::Word));
            state.jump = Jump::new(spots);
            if state.jump.is_none() {
                state.info("nothing on screen to tag");
            }
        }
        // The same gesture at the other granularity: one label per block,
        // landing the trace cursor. Words and blocks are two sets because
        // they are two sizes of thing — a screenful is a couple of
        // hundred words and about a dozen blocks.
        "goto_block" => {
            state.jump = Jump::new(crate::render::screen_blocks(state, state.transcript_rect));
            if state.jump.is_none() {
                state.info("nothing on screen to tag");
            }
        }

        // -------------------------------------------------- dispatches
        // ------------------------------------------------------ launch
        // `T T`, `T R`, `T H`, `T E`. Each opens another window *here*,
        // and `here` is [`window_at`]: the argument if one was typed, the
        // file the trace cursor is standing on if it is standing on one,
        // and the session's working directory otherwise.
        //
        // A program that resolves to nothing — no editor on this machine
        // — says so rather than opening a bare terminal, because a window
        // that came up empty is a window the operator has to work out the
        // meaning of.
        // A terminal runs the login shell on its own; naming a program
        // would be naming the shell twice.
        "terminal" => open_window(state, arg, Some(Vec::new()), false),
        "files" => {
            let argv = state.launcher.files();
            let argv = named(
                state,
                argv,
                "no file manager found; set `[terminal] files` in config.toml",
            );
            open_window(state, arg, argv, true);
        }
        "editor" => {
            let argv = state.launcher.editor();
            let argv = named(
                state,
                argv,
                "no editor found; set `$EDITOR`, or `[terminal] editor` in config.toml",
            );
            open_window(state, arg, argv, true);
        }
        // The same editor, on the draft itself, **in this terminal** —
        // readline's `C-x C-e`, minus the execute: see [`crate::editor`].
        // It cannot run from inside `exec` — the terminal is the ui
        // loop's, and an editor has to be handed the whole screen, not a
        // frame drawn under it — so it only asks, the way `reload_ui`
        // does, and the loop does the standing down and standing up.
        // A minibuffer on the keyboard means the prompt is not what is
        // being written, and a prompt edited out from under a question
        // would be a draft the operator could not see come back.
        "edit_prompt" => {
            if state.mini.is_some() || state.dialog.is_some() {
                state.info("the editor is for the prompt, and the prompt is not on screen now");
                return;
            }
            state.edit_draft = true;
        }
        // *This* binary, not the name `eidolon`: a session running out of
        // a worktree, a `cargo run` target or one nix store path should
        // open another of the harness it is, and not whichever one is
        // first on `PATH`.
        "harness" => {
            let argv = match std::env::current_exe() {
                Ok(exe) => Some(vec![exe.display().to_string()]),
                Err(e) => {
                    state.alert(format!("cannot find this harness's own binary: {e}"));
                    None
                }
            };
            open_window(state, arg, argv, false);
        }
        // The escape hatch, and the reason the other four are not the
        // whole vocabulary: `:launch lazygit` works today, and a script
        // that binds `T G` to it needs no Rust. Split on whitespace,
        // which is what the `:` line can offer without a shell.
        "launch" => {
            let argv: Vec<String> = arg.split_whitespace().map(str::to_string).collect();
            if argv.is_empty() {
                state.info("launch what? `:launch CMD`");
            } else {
                // The command is the argument, so the window opens where
                // the cursor is looking rather than on a path parsed out
                // of it.
                let (dir, _) = crate::launch::aim(
                    std::path::Path::new(&state.cwd),
                    cursor_path(state).as_deref(),
                );
                spawn_window(state, &dir, &argv);
            }
        }

        // Aiming the prompt at a classifier. The mode refuses to open with
        // nothing configured rather than opening onto nothing, because the
        // only thing it could then do is take a line and lose it.
        "dispatch_mode" => {
            if state.dispatch.is_empty() {
                state.info(NO_BUNDLES);
                return;
            }
            if state.mini.is_some() {
                return;
            }
            // The mode is a row like any other, so its look is the
            // script's; only the guard above is the harness's. It is a
            // one-line mode now rather than a mode the *prompt* was put
            // into, which is what stops an utterance eating a message —
            // the two are different things being written, and they were
            // sharing a buffer.
            if state.modes.get("dispatch").is_none() {
                state.info("dispatch mode is not in modes()");
                return;
            }
            open_mini(state, MiniKind::Dispatch);
        }
        // The submode cycle. One surface today, so it mostly answers
        // "which one am I on" — which is worth answering, and is the same
        // key that will choose between them when there are more.
        "dispatch_surface" => {
            if state.dispatch.is_empty() {
                state.info(NO_BUNDLES);
                return;
            }
            state.dispatch.next();
            let now = state.dispatch.surface().unwrap_or_default().to_string();
            state.info(if state.dispatch.surfaces().len() == 1 {
                format!("{now} is the only dispatch surface configured")
            } else {
                format!("dispatch → {now}")
            });
        }

        // ----------------------------------------------------- eidolon
        // The minibuffer answers first, because `ret` was pressed on
        // *it*: a `:` line, a pattern or an utterance. The message
        // underneath is not being sent, and — the whole point of the
        // line living on the border — is not being touched either.
        "submit" if state.mini.is_some() => submit_mini(state, script, cmd_tx),
        "submit" if state.vault.active => { state.info("draft kept — return to the conversation before sending"); }
        "submit" => {
            let text = state.prompt.take();
            if text.trim().is_empty() {
                return;
            }
            let action = script
                .and_then(|s| s.on_submit(state.snapshot(), &text).ok())
                .unwrap_or(Action::Send(text.clone()));
            // Sending leaves the mode alone: a message written in insert
            // hands the prompt back in insert, so writing the next one
            // needs no `i` in between.
            act(state, script, cmd_tx, action);
        }
        // The one panic button, read from the front of the screen
        // backwards. A line that is up comes down first — that is what
        // `esc` means while you are typing one, and a turn in flight is
        // not what the key was pressed at. Then a finished search, which
        // keeps highlighting until something says stop, as in Helix.
        // Then the turn. With nothing in flight it is the harmless half
        // of esc: drop the selection.
        "cancel" if state.mini.is_some() => {
            close_mini(state, false);
        }
        "cancel" => {
            if state.working {
                let _ = cmd_tx.send(Cmd::Cancel);
            } else if state.search.is_some() {
                state.search = None;
            } else {
                state.prompt.collapse();
            }
        }
        "quit" => {
            state.quit = true;
            let _ = cmd_tx.send(Cmd::Quit);
        }
        // ---------------------------------------------------------- images
        // Reading the file happens here, on the UI thread, and not through
        // the dispatcher — because the path came from the operator, who is
        // the one thing the policy hook exists to represent. The day a
        // path comes from the *model* instead, this has to become a
        // dispatched call: at that point it is the model choosing what the
        // harness reads.
        "attach" => {
            let arg = arg.trim();
            if arg.is_empty() {
                state.info(
                    "attach what? `:attach PATH`, or `:attach_clipboard` for what you just copied",
                );
            } else {
                let path = eidolon_tools::image::expand_tilde(arg);
                read_attachment(state, move || {
                    eidolon_tools::image::Attachment::load(&path).map_err(|e| format!("{e:#}"))
                });
            }
        }
        "attach_clipboard" => read_attachment(state, || {
            // Three causes, one sentence: no helper, no clipboard, or
            // nothing image-shaped on it. The operator does the same thing
            // about all three, so telling them apart would be telling them
            // apart for nobody.
            crate::image::from_clipboard().ok_or_else(|| "no image on the clipboard".to_string())
        }),
        "attach_clear" => {
            let before = state.attachments.len();
            match arg.trim() {
                "" => {
                    state.attachments.clear();
                    state.info(match before {
                        0 => "nothing was attached".to_string(),
                        1 => "unstaged the image".to_string(),
                        n => format!("unstaged {n} images"),
                    });
                }
                name => {
                    state.attachments.retain(|a| a.name != name);
                    if state.attachments.len() == before {
                        state.info(format!("nothing attached is called {name}"));
                    } else {
                        state.info(format!("unstaged {name}"));
                    }
                }
            }
        }
        // Asked here, done in the UI loop, which owns the script.
        "reload_ui" => state.reload = true,
        // `:` opens the command line, on the prompt's bottom border.
        //
        // A message half-written when it opens is neither refused nor
        // set aside: it stays on the screen, in the prompt, exactly
        // where it was. This used to do nothing at all on a non-empty
        // prompt; then it stashed the draft in a `draft` field and gave
        // it back afterwards, which worked and needed a rule run after
        // every keystroke to notice the three different ways the line
        // could end. A second buffer needs none of that — nothing was
        // ever borrowed.
        "command_line" => {
            if state.mini.is_some() {
                return;
            }
            open_mini(state, MiniKind::Command);
        }
        // Tab walks the candidate list, writing each onto the line. The
        // stem is remembered so that cycling past a candidate does not
        // narrow the list to the candidate itself.
        //
        // A candidate is the whole line body and not the word, which is
        // what lets one arm serve both spots: completing the name writes
        // `model ` (space and all, since the argument is what comes
        // next), and completing the argument writes `exclude 12,14` with
        // the part already settled put back untouched.
        "complete_next" | "complete_prev" => {
            let Some(typed) = state.command_line().map(str::to_string) else {
                return;
            };
            let forward = name == "complete_next";
            let stem = state
                .completing
                .as_ref()
                .map(|(s, _)| s.clone())
                .unwrap_or(typed);
            // A row with nothing to insert is the popup saying what goes
            // here — the hint over `:note`, `:launch`, `:dispatch`. It is
            // not a stop on the cycle: `tab` at a prose argument does
            // nothing rather than typing `[TEXT]` into it.
            let found: Vec<String> = crate::complete::complete(state, &stem)
                .map(|c| c.rows)
                .unwrap_or_default()
                .into_iter()
                .map(|r| r.insert)
                .filter(|i| !i.is_empty())
                .collect();
            if found.is_empty() {
                return;
            }
            let n = found.len();
            let idx = match &state.completing {
                Some((_, i)) => (i + if forward { 1 } else { n - 1 }) % n,
                None if forward => 0,
                None => n - 1,
            };
            if let Some(m) = state.mini.as_mut() {
                m.input.clear();
                m.input.insert(&found[idx]);
            }
            // A unique completion is *settled*, and a cycle of one is not
            // a cycle. Forgetting the stem here is what lets `:mod` tab to
            // `:model ` and the very next tab be the argument's, rather
            // than rewriting the one name that matched over and over.
            state.completing = (n > 1).then_some((stem, idx));
        }
        // The keys, from the keymap as loaded, and the vocabulary, from
        // the registry plus the script's rows — generated, so neither
        // can drift from what actually runs.
        "help" => {
            let text = help_text(state);
            open_page(state, "help".into(), text);
        }
        // The blanket yes. A notice and not an entry: it is a word about
        // a keystroke, `seed` could not draw it from a record — the
        // switch is not journaled — and the status line says continuously
        // what a transcript line would say once. `alert` and not `info`
        // for arming, because the whole content of the notice is that
        // something is now switched off.
        "yolo" => match (&state.yolo, arg.trim()) {
            (None, _) => state.alert("there is no gate here to bypass ([policy] enabled = false)"),
            (Some(y), "on") | (Some(y), "yes") => {
                y.set(true);
                state.alert("yolo: every question is answered yes; refusals still refuse");
            }
            (Some(y), "off") | (Some(y), "no") => {
                y.set(false);
                state.info("yolo off: the gate asks again");
            }
            (Some(y), "") => match y.toggle() {
                true => state.alert("yolo: every question is answered yes; refusals still refuse"),
                false => state.info("yolo off: the gate asks again"),
            },
            (Some(_), other) => state.alert(format!("yolo takes on or off, not {other}")),
        },

        // What the session has spent, turn by turn. Built on the UI
        // thread from the ledger the status line's totals come from, so
        // it opens with no round trip and cannot disagree with them.
        "usage" => {
            let text = state.usage_page();
            open_page(state, "usage".into(), text);
            // A plan's window moves under an idle session — the same
            // key spent elsewhere — so a reading that has sat for a
            // minute is asked for again; the page is redrawn when the
            // answer lands, and says how old what it shows is meanwhile.
            if state.quota_is_stale(crate::usage::now_ms()) {
                let _ = cmd_tx.send(Cmd::Quota);
            }
        }
        // Every notice, oldest first: the word that went by while you
        // were typing, and the ones said before the first frame.
        "messages" => {
            if state.notices.is_empty() {
                state.info("nothing has been said yet");
            } else {
                let text: Vec<String> = state
                    .notices
                    .iter()
                    .map(|(level, t)| match level {
                        Level::Alert => format!("\u{26a0} {t}"),
                        Level::Info => t.clone(),
                    })
                    .collect();
                open_page(state, "messages".into(), text.join("\n"));
            }
        }
        // The script's own commands: its function, the snapshot, the
        // argument, and whatever action it answers with.
        other => match state
            .script_commands
            .iter()
            .find(|c| c.name == other)
            .map(|c| c.run.clone())
        {
            Some(run) => match script.map(|s| s.run_command(&run, state.snapshot(), arg)) {
                Some(Ok(action)) => {
                    act(state, script, cmd_tx, action);
                }
                Some(Err(e)) => state.info(format!("command `{other}` failed: {e:#}")),
                None => state.info(format!(
                    "command `{other}` needs the script, which did not compile"
                )),
            },
            None => state.info(format!("no such command: {other}")),
        },
    }
}

/// `:help`: every mode's bindings from the keymap as loaded, then the
/// vocabulary. The canonical three modes first, the rest in name order.
fn help_text(state: &UiState) -> String {
    let mut out = String::from("── Key reference ──\n\n  Live keymap · :reload_ui re-reads ui.rn\n  Bindings grouped by mode; commands follow below.\n");
    let Some(tables) = state.keymap.as_object() else {
        return out + &command::reference_in(&state.script_commands);
    };
    let mut names: Vec<&String> = tables.keys().collect();
    let rank = |n: &str| {
        ["normal", "insert", "select"]
            .iter()
            .position(|c| *c == n)
            .unwrap_or(3)
    };
    names.sort_by_key(|n| (rank(n), (*n).clone()));
    for name in names {
        let row = state.modes.style(name);
        let kind = row
            .kind
            .map(|k| format!(" · {}", k.as_str()))
            .unwrap_or_default();
        out.push_str(&format!("\n── {name} ({}{kind}) ──\n\n", row.label));
        for (key, label) in crate::keys::bindings(&tables[name]) {
            out.push_str(&format!("  {key:<24} │ {label}\n"));
        }
    }
    out.push_str("\n── Command vocabulary ──\n");
    // Keep the registry authoritative, adding only visual section boundaries.
    for line in command::reference_in(&state.script_commands).lines() {
        if !line.is_empty() && !line.starts_with(' ') {
            out.push_str(&format!("── {line} ──\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Paint this frame's images, and remember what was painted.
///
/// Only what moved is redrawn. A picture whose cells the frame left alone
/// is still on the screen — that is the whole point of marking them skip —
/// and repainting it would be a flicker per frame for no change.
///
/// Kitty's images are objects the terminal owns, so one that has gone has
/// to be *deleted*; sixel's are just pixels in the cell grid, and the
/// frame rubbing out the cells is what removes them. Hence the delete-all
/// on the kitty path and nothing on the other.
fn paint_images(state: &mut UiState, images: Vec<crate::image::Placement>) {
    use std::io::Write as _;
    if images.is_empty() && state.drawn_images.is_empty() {
        return;
    }
    let proto = state.image_protocol;
    let gone = state
        .drawn_images
        .iter()
        .any(|d| !images.iter().any(|p| p.same_as(d)));
    let mut out = String::new();
    if gone && proto == crate::image::Protocol::Kitty {
        out.push_str("\x1b_Ga=d\x1b\\");
    }
    for p in &images {
        // Redraw what moved, and what the delete above has just taken off
        // the screen along with it.
        let held = !gone && state.drawn_images.iter().any(|d| d.same_as(p));
        if held {
            continue;
        }
        if let Some(esc) = crate::image::draw(p, proto, state.cell_size) {
            out.push_str(&esc);
        }
    }
    if !out.is_empty() {
        let mut w = std::io::stdout();
        let _ = w.write_all(out.as_bytes());
        let _ = w.flush();
    }
    state.drawn_images = images;
}

/// Read an image on a thread and stage it when it arrives.
///
/// Never on the UI thread: a screenshot off a large display costs about a
/// fifth of a second to decode, downscale and re-encode — nearly all of it
/// the resize — and spending that in the frame loop stops the harness
/// dead. The prompt says a read is in flight meanwhile, so the delay is
/// something the operator can see rather than something they feel.
fn read_attachment(
    state: &mut UiState,
    work: impl FnOnce() -> Result<crate::image::Attachment, String> + Send + 'static,
) {
    state.attaching += 1;
    let tx = state.arrivals.0.clone();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
}

/// Take whatever the reader threads have finished. Called once per pass of
/// the UI loop, beside the driver's own messages.
fn drain_attachments(state: &mut UiState) -> bool {
    let mut any = false;
    while let Ok(res) = state.arrivals.1.try_recv() {
        any = true;
        state.attaching = state.attaching.saturating_sub(1);
        match res {
            Ok(a) => {
                let label = a.label();
                state.attachments.push(a);
                state.info(format!("attached {label}"));
            }
            Err(e) => state.info(e),
        }
    }
    any
}

/// Perform what `on_submit`, a picker or the script asked for. A named
/// command is resolved against [`crate::command`] here and nowhere else:
/// that lookup is what makes `:goto_buffer_end` and `space g e` the same
/// command rather than two vocabularies that happen to overlap.
fn act(
    state: &mut UiState,
    script: Option<&UiScript>,
    cmd_tx: &mpsc::UnboundedSender<Cmd>,
    action: Action,
) -> bool {
    match action {
        Action::None => false,
        Action::Send(_) if state.vault.active => {
            state.info("draft kept — return to the conversation before sending");
            false
        }
        Action::Send(text) => {
            // The staged attachments go *with* this message and are gone
            // from the prompt afterwards. Taken here, where the message is
            // made, rather than in the command that staged them: an
            // attachment belongs to whichever message is sent next, and
            // the operator may have typed three lines and changed their
            // mind twice since attaching it.
            let images = std::mem::take(&mut state.attachments);
            for a in &images {
                state.transcript.push(Entry::Image(a.clone()));
            }
            state.pending_user.push_back(state.transcript.len());
            state.transcript.push(Entry::User(text.clone()));
            state.set_scroll(0);
            state.working = true;
            if state.working_since.is_none() {
                state.working_since = Some(std::time::Instant::now());
                state.streamed = 0;
                state.life.restart();
            }
            let _ = cmd_tx.send(Cmd::Send(Utterance {
                text,
                images: images.iter().map(crate::image::Attachment::block).collect(),
            }));
            true
        }
        Action::Command { name, arg } => {
            match command::lookup_in(&name, &state.script_commands).map(|e| e.run()) {
                // A driver command the UI answers first goes the same way a
                // key press does, so `:fork` and `space f` cannot disagree
                // about what a bare `fork` means.
                Some(Run::Driver) if ui_owns(&name, &arg) => {
                    exec(state, script, cmd_tx, &name, 1, &arg)
                }
                Some(Run::Driver) => {
                    let _ = cmd_tx.send(Cmd::Command { name, arg });
                }
                // A UI command typed on the `:` line, with whatever
                // followed it: a char for `:replace x`, a name for
                // `:enter_mode review`, nothing for the rest.
                Some(Run::Ui) => exec(state, script, cmd_tx, &name, 1, &arg),
                None => state.info(format!(
                    "no such command: {name}; :help lists them, space p searches them"
                )),
            }
            true
        }
        Action::Insert(text) => {
            state.prompt.insert(&text);
            true
        }
        Action::Info(text) => {
            state.info(text);
            true
        }
        // The utterance is drawn straight away, and journaled by the
        // dispatcher when the call is made, so a resume replays this same
        // pair. Drawing it here rather than waiting for that round trip is
        // what keeps a cold classifier's ~150ms from reading as a dropped
        // keystroke.
        Action::Dispatch(text) => {
            let Some(surface) = state.dispatch.surface().map(str::to_string) else {
                state.info(crate::app::NO_BUNDLES);
                return true;
            };
            // Not a pending user message: a dispatch journals a
            // `UserToolCall`, never a `UserMessage`, so nothing would ever
            // have claimed this entry from the queue — it used to sit in
            // the slot until the next send overwrote it.
            state.transcript.push(Entry::User(text.clone()));
            state.set_scroll(0);
            let _ = cmd_tx.send(Cmd::Dispatch { surface, text });
            true
        }
        Action::Cancel => {
            let _ = cmd_tx.send(Cmd::Cancel);
            true
        }
        Action::Quit => {
            state.quit = true;
            let _ = cmd_tx.send(Cmd::Quit);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn trace_links_open_a_note_follow_another_and_return_to_the_untouched_draft() {
        let mut r = Rig::new();
        r.state.view_width = 40;
        r.state.transcript.push(Entry::Assistant { text: "Source: [[First]] and [[Other]]".into(), streaming: false });
        r.state.prompt.insert("unfinished draft");
        r.press("K tab");
        assert_eq!(r.state.vault.trace_focus, Some((0, 0)));
        r.press("tab S-tab ret");
        let Cmd::VaultRead { id, target, from } = r.rx.try_recv().unwrap() else { panic!("not a read") };
        assert_eq!(target, "First");
        assert!(from.is_none());
        let note = |path: &str, body: &str| crate::vault::Note {
            path: path.into(), body: body.into(), anchor: None, scroll: 0, focus: None,
        };
        apply(&mut r.state, UiMsg::VaultNote { id, result: Ok(note("First.md", "# First\n[[Second]]")) });
        r.press("tab ret");
        let Cmd::VaultRead { id, target, from } = r.rx.try_recv().unwrap() else { panic!() };
        assert_eq!(target, "Second");
        assert_eq!(from.as_deref(), Some("First.md"));
        apply(&mut r.state, UiMsg::VaultNote { id, result: Ok(note("Second.md", "# Second")) });
        r.press("backspace");
        assert_eq!(r.state.vault.current.as_ref().unwrap().path, "First.md");
        r.press("q");
        assert!(r.state.dialog.is_none());
        assert!(!r.state.vault.active);
        assert!(r.state.tracing());
        assert_eq!(r.state.trace.at, 0);
        assert_eq!(r.state.prompt.text(), "unfinished draft");
        assert_eq!(r.state.transcript.len(), 1);
        assert!(r.rx.try_recv().is_err(), "browsing sends no model turn or tool call");
    }

    #[test]
    fn closing_a_loading_reader_or_receiving_a_question_invalidates_its_reply() {
        let mut r = Rig::new();
        crate::vault::request(&mut r.state, &r.tx, "Slow".into());
        let Cmd::VaultRead { id, .. } = r.rx.try_recv().unwrap() else { panic!() };
        r.press("esc");
        apply(&mut r.state, UiMsg::VaultNote { id, result: Err("late".into()) });
        assert!(r.state.dialog.is_none());
        crate::vault::request(&mut r.state, &r.tx, "Slow".into());
        let Cmd::VaultRead { id, .. } = r.rx.try_recv().unwrap() else { panic!() };
        let (tx, _rx) = tokio::sync::oneshot::channel();
        apply(&mut r.state, UiMsg::Dialog(DialogRequest {
            kind: DialogKind::Confirm, prompt: "May I?".into(), options: Vec::new(), reply: tx,
        }));
        apply(&mut r.state, UiMsg::VaultNote { id, result: Err("late".into()) });
        assert_eq!(r.state.dialog.as_ref().unwrap().prompt, "May I?");
        crate::vault::request(&mut r.state, &r.tx, "Cannot interrupt".into());
        assert!(r.rx.try_recv().is_err());
    }

    fn reader(r: &mut Rig, body: &str) {
        r.state.transcript_rect = ratatui::layout::Rect::new(0, 0, 60, 12);
        crate::vault::request(&mut r.state, &r.tx, "Note".into());
        let Cmd::VaultRead { id, .. } = r.rx.try_recv().unwrap() else { panic!() };
        apply(&mut r.state, UiMsg::VaultNote { id, result: Ok(crate::vault::Note {
            path: "Note.md".into(), body: body.into(), anchor: None, scroll: 0, focus: None,
        }) });
    }

    #[test]
    fn vault_reader_composes_dispatches_and_commands_without_losing_the_note() {
        let mut r = Rig::new();
        reader(&mut r, "# Note\n[[Child]]");
        assert!(r.state.dialog.is_none(), "the reader is not a modal dialog");
        assert_eq!(r.state.mode_name(), "vault");
        r.write("hello from the reader").press("ret");
        assert!(r.rx.try_recv().is_err());
        assert_eq!(r.state.prompt.text(), "hello from the reader");
        assert_eq!(r.state.mode_name(), "insert");
        assert!(r.state.vault.active);
        r.press("esc !").typed("read a note").press("ret");
        assert!(matches!(r.rx.try_recv().unwrap(), Cmd::Dispatch { .. }));
        assert_eq!(r.state.mode_name(), "dispatch");
        r.press("esc :").typed("help").press("ret");
        assert_eq!(r.state.mode_name(), "page");
        assert!(r.state.vault.active);
        r.press("q");
        assert_eq!(r.state.mode_name(), "vault");
        assert_eq!(r.state.vault.current.as_ref().unwrap().path, "Note.md");
        r.press("q");
        assert!(!r.state.vault.active);
        assert!(r.rx.try_recv().is_err());
    }

    #[test]
    fn trace_selects_source_lines_and_stages_an_address_without_sending() {
        let mut r = Rig::new();
        r.state.prompt.insert("Please explain");
        reader(&mut r, "# Note\nA long line with enough words to wrap across several narrow rows.\nAnother line.\nEnd.");
        r.state.transcript_rect.width = 20;
        r.press("K j v j");
        assert_eq!(r.state.mode_name(), "trace");
        assert_eq!(crate::vault::trace_selection(&r.state), 2..=3);
        let selected = crate::vault::trace_text(&r.state);
        assert!(selected.starts_with("A long line"));
        assert!(selected.ends_with("Another line."));
        r.state.transcript_rect.width = 50;
        assert_eq!(crate::vault::trace_selection(&r.state), 2..=3, "source addresses survive reflow");
        r.press("Q");
        assert_eq!(r.state.mode_name(), "insert");
        assert_eq!(r.state.prompt.text(), "Please explain [[Note.md]] (source lines 2–3) ");
        let draft = r.state.prompt.text().to_string();
        r.press("ret");
        assert_eq!(r.state.prompt.text(), draft, "blocked submit must not consume the draft");
        assert!(!r.state.working);
        assert!(r.state.transcript.is_empty());
        assert!(r.rx.try_recv().is_err());
        assert!(!act(&mut r.state, Some(&r.script), &r.tx, Action::Send("scripted send".into())));
        assert_eq!(r.state.prompt.text(), draft);
        assert!(r.rx.try_recv().is_err());
        r.press("esc q i ret");
        assert!(matches!(r.rx.try_recv().unwrap(), Cmd::Send(_)), "sending resumes only in the conversation");
    }

    #[test]
    fn trace_word_jump_extends_document_selection_and_never_moves_hidden_transcript_cursor() {
        let mut r = Rig::new();
        r.state.transcript.push(Entry::Assistant { text: "conversation".into(), streaming: false });
        reader(&mut r, "first\nsecond\nthird");
        r.press("K v g w");
        let j = r.state.jump.as_ref().unwrap();
        let target = j.spots.iter().position(|s| matches!(s, Spot::Cell { text, .. } if text == "third")).unwrap();
        let label = j.labels[target].clone();
        r.typed(&label);
        assert_eq!(crate::vault::trace_selection(&r.state), 1..=3);
        assert_eq!(r.state.trace.at, 0);
        r.press("y");
        assert_eq!(r.state.prompt.register, "first\nsecond\nthird");
        r.press("q");
        assert!(!r.state.vault.active);
        assert!(r.state.tracing());
        assert!(r.rx.try_recv().is_err());
    }

    #[test]
    fn vault_search_uses_note_rows_and_restores_the_transcript_search() {
        let mut r = Rig::new();
        r.state.transcript.push(Entry::Assistant { text: "transcript-only".into(), streaming: false });
        r.press("/").typed("transcript-only").press("ret");
        r.state.scroll_up = 7;
        reader(&mut r, &format!("first needle\n{}second needle\n{}", "filler\n".repeat(20), "tail\n".repeat(20)));
        r.press("/").typed("needle");
        assert_eq!(r.state.search.as_ref().unwrap().hits.len(), 2);
        r.press("ret n");
        let second = r.state.vault.page.as_ref().unwrap().selected;
        assert!(second > 10);
        assert_eq!(r.state.scroll_up, 7);
        r.press("N");
        assert_eq!(r.state.vault.page.as_ref().unwrap().selected, 0);
        r.press("/").typed("second").press("esc");
        assert_eq!(r.state.vault.page.as_ref().unwrap().selected, 0, "cancel restores note position");
        r.press("q");
        assert!(!r.state.vault.active);
        assert_eq!(r.state.search.as_ref().unwrap().pattern, "transcript-only");
        assert_eq!(r.state.scroll_up, 7);
    }

    #[test]
    fn vault_word_tags_copy_words_follow_links_and_keep_draft_targets() {
        let mut r = Rig::new();
        r.state.prompt.insert("draft text");
        reader(&mut r, "ordinary [[Child|child label]]");
        r.press("g w");
        let j = r.state.jump.as_ref().unwrap();
        assert!(j.spots.iter().any(|s| matches!(s, Spot::Word(_))));
        let word = j.spots.iter().position(|s| matches!(s, Spot::Cell { text, .. } if text == "ordinary")).unwrap();
        let label = j.labels[word].clone();
        r.typed(&label);
        assert_eq!(r.state.prompt.register, "ordinary");
        assert_eq!(r.state.prompt.text(), "draft text");
        r.press("g w");
        let j = r.state.jump.as_ref().unwrap();
        let link = j.spots.iter().position(|s| matches!(s, Spot::Link { target, .. } if target == "Child")).unwrap();
        let label = j.labels[link].clone();
        r.typed(&label);
        let Cmd::VaultRead { target, from, .. } = r.rx.try_recv().unwrap() else { panic!() };
        assert_eq!(target, "Child");
        assert_eq!(from.as_deref(), Some("Note.md"));
        assert!(r.rx.try_recv().is_err(), "tagging and browsing create no model turn");
    }

    #[test]
    fn a_question_temporarily_owns_the_keys_but_preserves_a_loaded_note() {
        let mut r = Rig::new();
        reader(&mut r, "[[Child]]");
        let (tx, _rx) = tokio::sync::oneshot::channel();
        apply(&mut r.state, UiMsg::Dialog(DialogRequest {
            kind: DialogKind::Confirm, prompt: "May I?".into(), options: Vec::new(), reply: tx,
        }));
        assert_eq!(r.state.mode_name(), "confirm");
        r.press("esc");
        assert_eq!(r.state.mode_name(), "vault");
        assert_eq!(r.state.vault.current.as_ref().unwrap().path, "Note.md");
    }

    /// A UI thread's worth of state, without a terminal or a driver.
    struct Rig {
        state: UiState,
        keys: Keys,
        script: UiScript,
        tx: mpsc::UnboundedSender<Cmd>,
        rx: mpsc::UnboundedReceiver<Cmd>,
    }

    impl Rig {
        fn new() -> Self {
            Self::with_script(crate::DEFAULT_UI)
        }

        /// A rig on a script of the test's own — the default with a table
        /// or two overridden, usually.
        fn with_script(src: &str) -> Self {
            let (tx, rx) = mpsc::unbounded_channel();
            let mut state = UiState::new("m".into(), "/tmp/s.eid".into(), "/".into());
            let (script, keys) = load_ui(&mut state, src);
            let script = script.expect("the script compiles");
            state.view_height = 20;
            // One dispatch surface, as a configured harness has: the mode
            // refuses to open without one, and every test that is not
            // about *that* wants it open.
            state.dispatch = crate::dispatch::Dispatch::new(vec!["mneme".into()]);
            Rig {
                state,
                keys,
                script,
                tx,
                rx,
            }
        }

        /// Feed a space-separated key spec, in the same notation the keymap
        /// uses: `"esc g g w d"`.
        fn press(&mut self, spec: &str) -> &mut Self {
            for k in spec.split(' ').filter(|s| !s.is_empty()) {
                handle_key(
                    &mut self.state,
                    &mut self.keys,
                    Some(&self.script),
                    &self.tx,
                    key_of(k),
                );
            }
            self
        }

        /// Enter insert and type a message — what an operator does to
        /// compose one now that the prompt rests in normal mode.
        fn write(&mut self, text: &str) -> &mut Self {
            self.press("i").typed(text)
        }

        /// Feed literal characters at whatever mode the prompt is in.
        fn typed(&mut self, text: &str) -> &mut Self {
            for c in text.chars() {
                handle_key(
                    &mut self.state,
                    &mut self.keys,
                    Some(&self.script),
                    &self.tx,
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                );
            }
            self
        }

        /// A transcript to search, plus the two viewport values a draw
        /// would have left behind. These tests never draw, and the
        /// search reads both: `view_entries` is where the anchor comes
        /// from and which matches are worth a tag.
        fn seeded(&mut self, entries: &[&str]) -> &mut Self {
            self.state.transcript = entries
                .iter()
                .map(|t| Entry::User((*t).to_string()))
                .collect();
            self.state.view_width = 60;
            self.state.view_entries = 0..entries.len();
            self
        }

        /// Which transcript entry the live search is sitting on.
        fn hit(&self) -> Option<usize> {
            self.state
                .search
                .as_ref()
                .and_then(crate::search::Search::entry)
        }

        /// The message being composed.
        fn text(&self) -> &str {
            self.state.prompt.text()
        }

        /// The one-line mode in front of it, sigil and all, or `""`
        /// when none is up.
        fn line(&self) -> String {
            self.state
                .mini
                .as_ref()
                .map(crate::state::Mini::line)
                .unwrap_or_default()
        }

        /// The word on the status line, or nothing.
        fn said(&self) -> String {
            self.state
                .notice
                .as_ref()
                .map(|n| n.text.clone())
                .unwrap_or_default()
        }

        /// The page that is up.
        fn page(&self) -> String {
            match &self.state.dialog {
                Some(d) if d.kind == DialogKind::Page => d.body.clone(),
                Some(_) => panic!("a dialog is up, but not a page"),
                None => panic!(
                    "no page is up; the transcript is {:?}",
                    self.state.transcript
                ),
            }
        }

        fn cmds(&mut self) -> Vec<String> {
            let mut out = Vec::new();
            while let Ok(c) = self.rx.try_recv() {
                out.push(format!("{c:?}"));
            }
            out
        }
    }

    fn key_of(spec: &str) -> KeyEvent {
        let (mods, base) = match spec.rsplit_once('-') {
            Some((m, b)) if !m.is_empty() && !b.is_empty() => {
                let mut mo = KeyModifiers::NONE;
                if m.contains('C') {
                    mo |= KeyModifiers::CONTROL;
                }
                if m.contains('A') {
                    mo |= KeyModifiers::ALT;
                }
                if m.contains('S') {
                    mo |= KeyModifiers::SHIFT;
                }
                (mo, b)
            }
            _ => (KeyModifiers::NONE, spec),
        };
        let code = match base {
            "esc" => KeyCode::Esc,
            "ret" => KeyCode::Enter,
            "space" => KeyCode::Char(' '),
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "del" => KeyCode::Delete,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            s => KeyCode::Char(s.chars().next().unwrap()),
        };
        KeyEvent::new(code, mods)
    }

    /// The prompt rests in normal, so the harness has a keyboard: `:` and
    /// the leader reach it without an `esc` first. Composing a message is
    /// `i`, as in Helix.
    #[test]
    fn a_fresh_prompt_rests_in_normal_and_i_is_what_starts_a_message() {
        let mut r = Rig::new();
        assert_eq!(r.state.prompt.mode, Mode::normal());
        r.press("i").typed("hello");
        assert_eq!(r.text(), "hello");
        assert_eq!(r.state.prompt.mode, Mode::insert());
    }

    /// The bug that forced the change: on a prompt resting in insert, `:`
    /// typed a colon and the command line needed an `esc` nobody would
    /// guess. From normal it is the one key it looks like.
    #[test]
    fn colon_opens_the_command_line_from_a_blank_prompt_with_no_esc_first() {
        let mut r = Rig::new();
        r.press(":");
        assert_eq!(r.line(), ":");
        assert_eq!(r.state.mode_name(), "command");
        // The *prompt* stays in the mode it was resting in: the line is
        // a buffer of its own on the border, not the prompt borrowed.
        assert_eq!(r.state.prompt.mode, Mode::normal());
        r.typed("tree").press("ret");
        assert_eq!(r.cmds(), vec![r#"Command { name: "tree", arg: "" }"#]);
        assert_eq!(r.state.prompt.mode, Mode::normal());
    }

    #[test]
    fn esc_then_a_motion_and_a_verb_edits_the_way_helix_does() {
        let mut r = Rig::new();
        r.write("hello brave world").press("esc g g w d");
        assert_eq!(r.text(), "brave world");
        // `d` leaves normal mode alone: the next key is still a command.
        assert_eq!(r.state.prompt.mode, Mode::normal());
    }

    #[test]
    fn a_count_repeats_the_motion_rather_than_stretching_it() {
        let mut r = Rig::new();
        // Helix's semantics, and the reason a count is not a range: each
        // repeat *reselects*, so `2w` lands on the second word rather than
        // sweeping both. Extending across two words is `v w w`.
        r.write("one two three four").press("esc g g 2 w d");
        assert_eq!(r.text(), "one three four");
        r.press("u").press("esc g g v w w d");
        assert_eq!(r.text(), "three four");
    }

    #[test]
    fn c_deletes_and_drops_into_insert_so_the_next_key_is_text() {
        let mut r = Rig::new();
        r.write("hello world")
            .press("esc g g w c")
            .typed("goodbye ");
        assert_eq!(r.text(), "goodbye world");
        assert_eq!(r.state.prompt.mode, Mode::insert());
    }

    #[test]
    fn f_takes_its_argument_from_the_next_key_even_when_bound() {
        let mut r = Rig::new();
        // `d` is delete; here it is the char `f` is hunting for.
        r.write("abcdef").press("esc g g f d");
        assert_eq!(r.state.prompt.selection(), "abcd");
    }

    #[test]
    fn the_space_menu_opens_a_popup_and_then_runs_a_named_command() {
        let mut r = Rig::new();
        r.press("esc space");
        let m = r.state.menu.clone().expect("the popup should be open");
        assert_eq!(m.title, "space");
        assert_eq!(m.selected, None, "a chord is a grid of keys, not a list");
        assert!(m.items.iter().any(|(k, d)| k == "m" && d == "model picker"));
        r.press("m");
        assert!(r.state.menu.is_none());
        // The binding names `model` outright: there is no second spelling
        // of a driver command for a key to reach.
        assert_eq!(r.cmds(), vec![r#"Command { name: "model", arg: "" }"#]);
    }

    #[test]
    fn jk_scroll_the_transcript_and_leave_the_prompt_alone() {
        let mut r = Rig::new();
        r.write("draft").press("esc");
        r.press("k k k j");
        assert_eq!(r.state.scroll_up, 2);
        assert_eq!(r.text(), "draft");
        r.press("g b");
        assert_eq!(r.state.scroll_up, 0);
    }

    #[test]
    fn a_half_page_scroll_follows_the_viewport_the_script_laid_out() {
        let mut r = Rig::new();
        r.press("esc A-up");
        assert_eq!(r.state.scroll_up, 10); // view_height 20
    }

    /// The one thing a scroll up is for: reading something that is still
    /// being written *below* it. `scroll_up` counts rows from the bottom
    /// edge, so an unheld scroll is a scroll that follows — every row the
    /// stream appended slid the window down over the line being read.
    #[test]
    fn a_scrolled_view_holds_its_line_while_the_model_writes() {
        let mut r = Rig::new();
        r.state.view_width = 60;
        for n in 0..40 {
            r.state.transcript.push(Entry::Info(format!("info {n}")));
        }
        // What a frame drawn scrolled up leaves behind: the bottom
        // visible row is entry 29's one and only row, with ten entries
        // below it.
        r.state.scroll_anchor = Some((29, 0));
        r.state.scroll_up = 10;
        // The stream appends ten one-row entries —
        hold_scroll(&mut r.state);
        assert_eq!(r.state.scroll_up, 10, "nothing grew yet");
        for n in 0..10 {
            r.state.transcript.push(Entry::Info(format!("tail {n}")));
        }
        // — and the recount keeps the bottom row where it was, at the
        // price of a scroll that now counts twenty rows below it.
        hold_scroll(&mut r.state);
        assert_eq!(r.state.scroll_up, 20);
        // Rows taken away below the window — a fold collapsing the run
        // that was scrolled past — are held to just the same.
        for _ in 0..4 {
            r.state.transcript.pop();
        }
        hold_scroll(&mut r.state);
        assert_eq!(r.state.scroll_up, 16);
    }

    /// Following, every row is meant to arrive: no anchor, no recount,
    /// and the view stays on the live edge however much lands.
    #[test]
    fn a_following_view_takes_every_row_the_stream_adds() {
        let mut r = Rig::new();
        r.state.view_width = 60;
        for n in 0..10 {
            r.state.transcript.push(Entry::Info(format!("info {n}")));
        }
        r.state.transcript.push(Entry::Info("tail 0".into()));
        hold_scroll(&mut r.state);
        assert_eq!(r.state.scroll_up, 0);
    }

    /// The operator's hand and the anchor are the two writers of the
    /// scroll, and only one of them may hold: a wheel notch, a key, a
    /// jump spends the anchor so the recount cannot undo the hand.
    #[test]
    fn the_operators_hand_spends_the_anchor() {
        let mut r = Rig::new();
        r.state.scroll_anchor = Some((3, 0));
        r.press("esc k");
        assert_eq!(
            r.state.scroll_anchor, None,
            "a scroll key must spend the anchor"
        );
        r.state.scroll_anchor = Some((3, 0));
        r.state.set_scroll(0);
        assert_eq!(r.state.scroll_anchor, None, "so must a jump to the bottom");
    }

    /// The four the Helix config clones into every mode. `foot` eats the
    /// bare page keys, so these are the ones that actually arrive.
    #[test]
    fn the_shift_arrows_are_bound_in_insert_as_well_as_normal() {
        let mut r = Rig::new();
        r.write("  hello").press("S-up");
        assert_eq!(r.state.scroll_up, 20); // a page, without leaving insert
        assert_eq!(r.state.prompt.mode, Mode::insert());
        r.press("S-left");
        assert_eq!(r.state.prompt.range.cursor(), 2); // the first non-blank
        r.press("S-right");
        assert_eq!(r.state.prompt.range.cursor(), 7); // past the last char
        assert_eq!(r.text(), "  hello");
    }

    /// `^` is column zero here, not the first non-blank — the operator's
    /// Helix override, and the reverse of vim.
    #[test]
    fn caret_is_the_hard_line_start_and_dollar_the_slot_past_the_end() {
        let mut r = Rig::new();
        r.write("  hello").press("esc ^");
        assert_eq!(r.state.prompt.range.cursor(), 0);
        r.press("$");
        assert_eq!(r.state.prompt.range.cursor(), 7);
        // …which is what makes `$i` type at the end of the line.
        r.press("i").typed("!");
        assert_eq!(r.text(), "  hello!");
    }

    #[test]
    fn space_w_sends_the_way_space_w_writes() {
        let mut r = Rig::new();
        r.write("hello").press("esc space w");
        assert_eq!(r.cmds(), vec![r#"Send("hello")"#]);
    }

    /// Sending is not a reason to leave insert: the prompt comes back
    /// empty, in the mode the message was written in, ready for the next
    /// one. Only the first message of a session costs an `i`.
    #[test]
    fn submitting_sends_and_leaves_the_prompt_in_the_mode_it_was_written_in() {
        let mut r = Rig::new();
        r.write("hello").press("ret");
        assert_eq!(r.cmds(), vec![r#"Send("hello")"#]);
        assert_eq!(r.text(), "");
        assert_eq!(r.state.prompt.mode, Mode::insert());
        // …and it is still a live buffer, not a husk of one.
        r.typed("again").press("ret");
        assert_eq!(r.cmds(), vec![r#"Send("again")"#]);

        // Sent from normal — `space w`, or `ret` after an `esc` — it
        // rests in normal, for the same reason.
        r.write("third").press("esc space w");
        assert_eq!(r.state.prompt.mode, Mode::normal());
    }

    #[test]
    fn the_colon_command_line_is_just_the_prompt_with_a_colon_in_it() {
        let mut r = Rig::new();
        r.press("esc :").typed("tree").press("ret");
        assert_eq!(r.cmds(), vec![r#"Command { name: "tree", arg: "" }"#]);
    }

    #[test]
    fn esc_cancels_a_turn_in_flight_and_otherwise_only_drops_the_selection() {
        let mut r = Rig::new();
        r.write("hi").press("esc %");
        assert_eq!(r.state.prompt.selection(), "hi");
        r.press("esc");
        assert!(r.cmds().is_empty());
        assert_eq!(r.state.prompt.selection(), "i");

        r.state.working = true;
        r.press("esc");
        assert_eq!(r.cmds(), vec!["Cancel"]);
    }

    #[test]
    fn select_mode_grows_the_range_that_a_verb_then_takes() {
        let mut r = Rig::new();
        r.write("one two three").press("esc g g v w w d");
        assert_eq!(r.text(), "three");
    }

    #[test]
    fn a_dialog_takes_keys_from_its_own_table_and_the_prompts_does_not_reach_it() {
        let (reply, _rx) = tokio::sync::oneshot::channel();
        let mut r = Rig::new();
        r.write("draft").press("esc");
        r.state.dialog = Some(Dialog {
            kind: DialogKind::Confirm,
            weights: Vec::new(),
            prompt: "sure?".into(),
            options: Vec::new(),
            keys: Vec::new(),
            selected: 0,
            input: Dialog::field(),
            reply: Some(reply),
            pick: None,
            body: String::new(),
            masked: false,
        });
        // `d` would delete a selection; here it is just a key the confirm
        // box ignores, and the prompt behind it is untouched.
        r.press("d");
        assert_eq!(r.text(), "draft");
        assert!(r.state.dialog.is_some());
        assert_eq!(
            r.state.snapshot()["mode"],
            "confirm",
            "the dialog is the mode to the hands"
        );
        r.press("y");
        assert!(r.state.dialog.is_none());
    }

    #[test]
    fn the_search_line_and_the_dialogs_are_keymap_tables_a_script_can_rebind() {
        // The default binds `n` to deny on a confirm box; a script of the
        // user's own puts deny on `x` and `n` back to a no-op.
        let src = crate::DEFAULT_UI.replace(
            "\"n\": cmd(\"dialog_deny\", \"deny\")",
            "\"x\": cmd(\"dialog_deny\", \"deny\")",
        );
        assert_ne!(
            src,
            crate::DEFAULT_UI,
            "the default binds `n` on the confirm table"
        );
        let mut r = Rig::with_script(&src);
        let (reply, mut rx) = tokio::sync::oneshot::channel();
        r.state.dialog = Some(Dialog {
            kind: DialogKind::Confirm,
            weights: Vec::new(),
            prompt: "sure?".into(),
            options: Vec::new(),
            keys: Vec::new(),
            selected: 0,
            input: Dialog::field(),
            reply: Some(reply),
            pick: None,
            body: String::new(),
            masked: false,
        });
        r.press("n");
        assert!(
            r.state.dialog.is_some(),
            "`n` is unbound in this script's confirm table"
        );
        r.press("x");
        assert!(r.state.dialog.is_none());
        assert_eq!(rx.try_recv().unwrap(), Some("no".into()));
        // And the search line: its `esc` is the same `cancel` the prompt
        // has, resolved from the `search` table.
        r.seeded(&["alpha", "beta"]).press("/").typed("beta");
        assert_eq!(r.state.snapshot()["mode"], "search");
        assert_eq!(r.hit(), Some(1));
        r.press("esc");
        assert!(r.state.search.is_none());
    }

    /// `:login`'s masked dialog commits the typed secret to the driver as
    /// `provider\x01secret`, and only on `ret` with something typed — an
    /// empty submit and a plain `esc` both close it and send nothing, so
    /// a stray keystroke can never store an empty credential.
    #[test]
    fn the_ask_dialog_commits_a_secret_only_on_a_nonempty_submit() {
        let mut r = Rig::new();
        open_ask(
            &mut r.state,
            " ollama · API key ".into(),
            true,
            "login".into(),
            "ollama".into(),
        );
        assert!(r.state.dialog.as_ref().unwrap().masked);
        r.typed("sk-secret").press("ret");
        assert!(r.state.dialog.is_none(), "submit closes the dialog");
        assert_eq!(
            r.cmds(),
            vec![r#"Command { name: "login", arg: "ollama\u{1}sk-secret" }"#]
        );

        // Esc: nothing typed, nothing sent.
        open_ask(&mut r.state, " ollama · API key ".into(), true, "login".into(), "ollama".into());
        r.typed("abc").press("esc");
        assert!(r.state.dialog.is_none());
        assert!(r.cmds().is_empty(), "cancel commits nothing");

        // Ret with an empty field: same as cancelling.
        open_ask(&mut r.state, " ollama · API key ".into(), true, "login".into(), "ollama".into());
        r.press("ret");
        assert!(r.state.dialog.is_none());
        assert!(r.cmds().is_empty(), "an empty secret is never stored");
    }

    /// The point of one registry: `:` is not a driver-only door. A command
    /// that runs on the UI thread is typed the same way and never leaves it.
    #[test]
    fn the_colon_line_reaches_the_prompts_own_commands_as_well_as_the_harnesss() {
        let mut r = Rig::new();
        // On the `:` line and then back on the draft it was opened over,
        // which is the buffer a prompt command acts on: `submit` has
        // already taken the line, so there would otherwise be nothing
        // for `goto_buffer_end` to move through.
        r.write("hello world")
            .press("esc")
            .press(":")
            .typed("goto_buffer_start")
            .press("ret");
        assert!(
            r.cmds().is_empty(),
            "a UI command should not go to the driver"
        );
        assert_eq!(r.text(), "hello world", "the draft came back");
        r.press(":").typed("select_all").press("ret");
        assert_eq!(
            r.state.prompt.selection(),
            "hello world",
            "and a prompt command reached it"
        );
    }

    /// `space p` used to open a palette — the registry as a filterable
    /// list — and now opens the `:` line, which is the same registry with
    /// the same ranking and shows what it resolved to before running it.
    #[test]
    fn the_leader_opens_the_command_line_and_the_palette_is_gone() {
        let mut r = Rig::new();
        r.press("esc space p");
        assert!(
            r.state.dialog.is_none(),
            "no list; the line is the surface now"
        );
        assert_eq!(r.line(), ":");
        assert_eq!(r.state.snapshot()["mode"], "command");
        // The whole vocabulary, with each row's group in front of its
        // description — what the palette's second column used to carry.
        let m = r
            .state
            .menu
            .clone()
            .expect("a bare colon offers everything");
        assert_eq!(m.items.len(), crate::command::COMMANDS.len());
        assert!(
            m.items
                .iter()
                .any(|(k, d)| k == "compact" && d.starts_with("eidolon · ")),
            "{:?}",
            &m.items[..3]
        );
        assert!(
            crate::command::lookup("commands").is_none(),
            "the palette's own command went with it"
        );
        r.typed("compact").press("ret");
        assert_eq!(r.cmds(), vec![r#"Command { name: "compact", arg: "" }"#]);
    }

    /// A message half-written when the `:` line opens **does not move**.
    /// It used to be refused (`:` on a non-empty prompt did nothing),
    /// then set aside in a `draft` field and handed back by a rule run
    /// after every keystroke; now the line is a second buffer on the
    /// prompt's border and there is nothing to hand back.
    #[test]
    fn the_colon_line_leaves_the_message_where_it_was() {
        let mut r = Rig::new();
        r.write("half a thought").press("esc").press(":");
        assert_eq!(
            r.text(),
            "half a thought",
            "the draft is untouched, and on the screen"
        );
        assert_eq!(r.line(), ":");
        r.typed("compact").press("ret");
        assert_eq!(r.cmds(), vec![r#"Command { name: "compact", arg: "" }"#]);
        assert_eq!(r.text(), "half a thought");
        assert_eq!(r.line(), "", "and the line is gone");
        // Abandoning it costs the message nothing either.
        r.press(":").typed("clear_prompt");
        assert_eq!(
            r.text(),
            "half a thought",
            "the line is what is being typed, not the message"
        );
        r.press("esc");
        assert_eq!(r.line(), "");
        assert_eq!(r.text(), "half a thought");
    }

    /// A `:` command that acts on a buffer acts on the **message**, not
    /// on the line it was typed on — the line is gone by the time it
    /// runs, which is the same rule `submit` followed when it had to
    /// restore a stashed draft first.
    #[test]
    fn a_buffer_command_typed_on_the_colon_line_acts_on_the_message() {
        let mut r = Rig::new();
        r.write("half a thought")
            .press("esc")
            .press(":")
            .typed("clear_prompt")
            .press("ret");
        assert_eq!(r.text(), "", "`:clear_prompt` emptied the message");
        assert_eq!(r.line(), "");
    }

    /// Filtering a picker ranks rather than merely hiding: what the query
    /// prefixes leads, shortest first, and the highlight follows it — so
    /// the keystrokes that identify a row are the keystrokes that select
    /// it. Unranked, a row found by its *description* could sit above the
    /// one whose name you typed.
    #[test]
    fn a_picker_puts_what_you_typed_at_the_top_of_what_it_kept() {
        let mut r = Rig::new();
        let item = |key: &str, label: &str, desc: &str| PickItem {
            key: key.into(),
            label: label.into(),
            description: Some(desc.into()),
            weight: 0,
        };
        open_pick(
            &mut r.state,
            "model".into(),
            vec![
                item("fau:azure_ai/gpt-5.4", "GPT-5.4", "fau · 400k ctx"),
                item(
                    "deepseek:deepseek-v4-pro",
                    "DeepSeek V4 Pro",
                    "deepseek · reasoning",
                ),
                item(
                    "claude-cli:sonnet",
                    "sonnet (Claude Code)",
                    "claude-cli · reasoning",
                ),
            ],
            PickAction::Run("model".into()),
            None,
        );
        r.typed("deep");
        let d = r.state.dialog.as_ref().unwrap();
        let rows: Vec<&str> = d.visible().iter().map(|&i| d.keys[i].as_str()).collect();
        assert_eq!(rows[0], "deepseek:deepseek-v4-pro", "{rows:?}");
        assert_eq!(
            d.selected,
            d.visible()[0],
            "and the highlight is on it, so ret takes it"
        );
        r.press("ret");
        assert_eq!(
            r.cmds(),
            vec![r#"Command { name: "model", arg: "deepseek:deepseek-v4-pro" }"#]
        );
    }

    /// The session-delete picker hands back a half-written line rather
    /// than deleting: the second keystroke is the confirmation, on a list
    /// whose highlighted row moves under the fingers.
    #[test]
    fn choosing_a_session_to_delete_writes_the_command_line_instead_of_running_it() {
        let mut r = Rig::new();
        open_pick(
            &mut r.state,
            "delete_session".into(),
            vec![PickItem {
                key: "/logs/1.eid".into(),
                label: "1.eid".into(),
                description: None,
                weight: 0,
            }],
            PickAction::Edit("delete_session".into()),
            None,
        );
        r.press("ret");
        assert!(r.state.dialog.is_none());
        assert_eq!(r.line(), ":delete_session /logs/1.eid ");
        assert_eq!(r.state.mode_name(), "command");
        assert!(r.cmds().is_empty());
    }

    /// The fork point is the trace cursor, not a dialog.
    ///
    /// The picker this replaces listed exactly the rows `:tree` prints, and
    /// trace mode is already a cursor over those rows — so asking for a fork
    /// point and pointing at it became the same gesture. Both spellings are
    /// the same command: `space f`, and a bare `:fork` on the line, which
    /// used to open the picker from the driver side where the cursor is not
    /// visible.
    #[test]
    fn forking_from_trace_mode_takes_the_record_under_the_cursor() {
        let mut r = traced();
        // Only two of the six entries stand for a record: the user message,
        // and the cluster's own first entry.
        r.state.records.insert(0, 7);
        r.state.records.insert(1, 8);

        r.press("K g g space f");
        assert_eq!(r.cmds(), vec![r#"Command { name: "fork", arg: "7" }"#]);

        // A cluster is one block on screen, so `j` lands on its start: the
        // record is the cluster's own, not whichever part `l` would open.
        r.press("j space f");
        assert_eq!(r.cmds(), vec![r#"Command { name: "fork", arg: "8" }"#]);

        // The `:` line reaches the same command through a different door,
        // and must not disagree about what a bare `fork` means.
        r.press(":");
        r.typed("fork");
        r.press("ret");
        assert_eq!(
            r.cmds(),
            vec![r#"Command { name: "fork", arg: "8" }"#],
            "a bare :fork names the cursor too"
        );
    }

    /// With nothing under the cursor there is nothing to name, and the
    /// command says where the cursor is rather than opening a dialog over
    /// the transcript.
    #[test]
    fn bare_fork_without_a_cursor_says_where_to_go() {
        let mut r = traced();
        r.press("space f");
        assert!(r.cmds().is_empty(), "nothing was forked");
        assert!(
            infos(&r).iter().any(|i| i.contains("cursor on the transcript")),
            "{:?}",
            infos(&r)
        );

        // `K` puts the cursor on the newest block, which in this fixture
        // stands for no record at all — a notice is not a fork point.
        r.press("K space f");
        assert!(r.cmds().is_empty(), "still nothing to fork at");
        assert!(
            infos(&r).iter().any(|i| i.contains("not on the branch yet")),
            "{:?}",
            infos(&r)
        );
    }

    /// The badge is the whole point of the complaint: `:` used to read as
    /// INS, which is true and useless.
    #[test]
    fn the_command_line_reports_itself_as_its_own_mode() {
        let mut r = Rig::new();
        assert_eq!(r.state.snapshot()["mode"], "normal");
        r.press(":");
        assert_eq!(r.state.snapshot()["mode"], "command");
        r.typed("tree");
        assert_eq!(r.state.snapshot()["mode"], "command");
        // A message that merely contains a colon is not a command line.
        r.press("ret");
        r.write("see: this").press("ret");
        assert_eq!(
            r.cmds(),
            vec![
                r#"Command { name: "tree", arg: "" }"#,
                r#"Send("see: this")"#
            ]
        );
    }

    /// Emptied is how a line is usually left: type the pattern, backspace
    /// it away. So the backspace that has nothing left to delete is the
    /// way out, rather than a second key after the last character went.
    #[test]
    fn backspace_on_an_empty_command_line_takes_it_down() {
        let mut r = Rig::new();
        r.press(":");
        assert_eq!(r.state.snapshot()["mode"], "command");
        r.press("backspace");
        assert!(r.state.mini.is_none(), "the line is down");
        assert_eq!(r.state.snapshot()["mode"], "normal");
        // A line with something on it still deletes — only the backspace
        // that would delete nothing leaves.
        r.press(":").typed("tree");
        r.press("backspace backspace backspace backspace");
        assert_eq!(r.line(), ":", "the name is gone, the line is not");
        r.press("backspace");
        assert!(r.state.mini.is_none());
        // The `!` line is one line under another sigil, and reads the same.
        r.press("!");
        assert_eq!(r.state.mode_name(), "dispatch");
        r.press("backspace");
        assert!(r.state.mini.is_none());
        assert_eq!(r.state.mode_name(), "normal");
    }

    #[test]
    fn the_command_line_suggests_as_you_type_and_narrows() {
        let mut r = Rig::new();
        r.press(":");
        let all = r
            .state
            .menu
            .clone()
            .expect("a bare colon offers the whole vocabulary");
        assert_eq!(all.items.len(), crate::command::COMMANDS.len());
        assert_eq!(all.selected, Some(0));
        r.typed("comp");
        let m = r.state.menu.clone().expect("still suggesting");
        assert_eq!(m.title, ":comp");
        let names: Vec<&str> = m.items.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            vec!["compact", "complete_next", "complete_prev", "reload_ui"],
            "{names:?}"
        );
        // Typing something nothing matches retires the popup.
        r.typed("zzz");
        assert!(r.state.menu.is_none());
    }

    #[test]
    fn tab_walks_the_candidates_and_writes_each_onto_the_line() {
        let mut r = Rig::new();
        r.press(":").typed("de");
        r.press("tab");
        // The shortest prefix is the first candidate, and it takes
        // nothing, so the line keeps no space.
        assert_eq!(r.line(), ":decrement");
        r.press("tab");
        // The space is part of the completion: the command takes an
        // argument, so the next thing owed is the argument and not a
        // keystroke saying so.
        assert_eq!(r.line(), ":delete_session ");
        // Cycling stays on the stem's candidates rather than narrowing to
        // the one now on the line.
        r.press("tab");
        assert_eq!(
            r.line(),
            ":delete_selection",
            "and one that takes nothing gets no space"
        );
        r.press("S-tab");
        assert_eq!(r.line(), ":delete_session ");
        // …and the popup follows the selection.
        let m = r.state.menu.clone().unwrap();
        assert_eq!(m.items[m.selected.unwrap()].0, "delete_session [PATH]");
        r.press("ret");
        assert_eq!(
            r.cmds(),
            vec![r#"Command { name: "delete_session", arg: "" }"#]
        );
    }

    /// Past the space the popup is the *argument's*, filled from the
    /// source the command's row names — and `tab` there rewrites the
    /// argument rather than matching the whole line against the command
    /// names, which is what it used to do and why `:model son` completed
    /// to nothing at all.
    #[test]
    fn tab_completes_the_argument_once_the_name_is_settled() {
        let mut r = Rig::new();
        r.state.model_keys = vec![
            ("fau:openai/gpt-5".into(), "GPT-5".into()),
            ("deepseek:deepseek-v4-pro".into(), "DeepSeek".into()),
        ];
        r.press(":").typed("model dee");
        let m = r
            .state
            .menu
            .clone()
            .expect("the argument has a popup of its own");
        assert_eq!(
            m.title, ":model [KEY]",
            "titled with what the command wants"
        );
        assert_eq!(m.items.len(), 1);
        r.press("tab");
        assert_eq!(r.line(), ":model deepseek:deepseek-v4-pro");
        r.press("ret");
        assert_eq!(
            r.cmds(),
            vec![r#"Command { name: "model", arg: "deepseek:deepseek-v4-pro" }"#]
        );
    }

    /// A model key can have a space in it — `fau:Ministral 3 14B` is one
    /// this operator's catalog serves — so the argument is one value to
    /// the end of the line and reaches the driver whole.
    #[test]
    fn a_model_key_with_a_space_in_it_survives_the_colon_line() {
        let mut r = Rig::new();
        r.state.model_keys = vec![(
            "fau:Ministral 3 14B".into(),
            "Ministral 3 14B (FAU free)".into(),
        )];
        r.press(":").typed("model Minis").press("tab");
        assert_eq!(r.line(), ":model fau:Ministral 3 14B");
        r.press("ret");
        assert_eq!(
            r.cmds(),
            vec![r#"Command { name: "model", arg: "fau:Ministral 3 14B" }"#]
        );
    }

    /// The two spots in sequence, which is what the space is for: a
    /// completion with nothing to cycle is settled, so the next `tab` is
    /// the argument's rather than another rewrite of the one name that
    /// matched.
    #[test]
    fn a_unique_name_settles_and_the_next_tab_is_the_arguments() {
        let mut r = Rig::new();
        r.press(":").typed("enter_m").press("tab");
        assert_eq!(r.line(), ":enter_mode ");
        assert!(
            r.state.completing.is_none(),
            "a cycle of one is not a cycle"
        );
        r.press("tab");
        assert_eq!(
            r.line(),
            ":enter_mode command",
            "the modes table, which is what the row says fills it"
        );
        r.press("tab");
        assert_eq!(r.line(), ":enter_mode dispatch");
        r.press("tab");
        assert_eq!(r.line(), ":enter_mode insert");
        r.press("ret");
        assert!(r.state.prompt.mode.is("insert"));
    }

    /// An argument nothing can fill still says what it is. The row is a
    /// hint and not a candidate, so `tab` on it leaves the line alone
    /// rather than typing `[TEXT]` into the note.
    #[test]
    fn a_prose_argument_is_told_about_and_never_completed() {
        let mut r = Rig::new();
        r.press(":").typed("note remember the ");
        let m = r
            .state
            .menu
            .clone()
            .expect("the shape is still worth saying");
        assert_eq!(m.title, ":note [TEXT]");
        assert_eq!(m.items[0].0, "[TEXT]");
        r.press("tab");
        assert_eq!(r.line(), ":note remember the ");
    }

    /// Editing after a tab ends the cycle: the next tab completes what is
    /// on the line now, not the stem from three keystrokes ago.
    #[test]
    fn typing_after_a_tab_restarts_the_completion_from_the_new_stem() {
        let mut r = Rig::new();
        r.press(":").typed("c").press("tab");
        assert_eq!(r.line(), ":cancel");
        r.press("backspace backspace backspace backspace backspace backspace");
        r.typed("tr").press("tab");
        assert_eq!(r.line(), ":tree");
    }

    #[test]
    fn a_typed_command_the_registry_does_not_know_says_so() {
        let mut r = Rig::new();
        r.press("esc :").typed("nonesuch").press("ret");
        assert!(r.cmds().is_empty());
        assert!(
            r.said().contains("no such command: nonesuch"),
            "{:?}",
            r.state.notice
        );
    }

    // ------------------------------------------------------- sessions

    /// The session picker's rows are paths, and enter runs `resume` on
    /// one — the same shape as the fork picker, which is the point: a
    /// picker is never a bespoke modal.
    #[test]
    fn choosing_a_session_resumes_it() {
        let mut r = Rig::new();
        open_pick(
            &mut r.state,
            "sessions".into(),
            vec![
                PickItem {
                    key: "/logs/1.eid".into(),
                    label: "2h   4 msg  mock  the parser".into(),
                    description: Some("here".into()),
                    weight: 0,
                },
                PickItem {
                    key: "/logs/2.eid".into(),
                    label: "9d   2 msg  mock  the keymap".into(),
                    description: Some("~/other".into()),
                    weight: 0,
                },
            ],
            PickAction::Run("resume".into()),
            Some("/logs/1.eid".into()),
        );
        // The picker opens on the session already in play.
        assert_eq!(r.state.dialog.as_ref().unwrap().selected, 0);
        // Filtering runs over the label as well as the key.
        r.typed("keymap");
        r.press("ret");
        assert_eq!(
            r.cmds(),
            vec![r#"Command { name: "resume", arg: "/logs/2.eid" }"#]
        );
    }

    /// Everything drawn is *about* one session, so a resume has to take
    /// all of it: a search's hits index into the entries being replaced,
    /// and the counters are the old session's totals.
    #[test]
    fn resuming_replaces_everything_that_pointed_at_the_old_transcript() {
        let mut r = Rig::new();
        r.seeded(&["alpha", "beta"]);
        r.press("/").typed("alpha").press("ret");
        r.state.usage_in = 900;
        r.state.tools_run = 4;
        r.state.scroll_up = 6;
        r.state.detail = crate::state::Detail::Full;
        assert!(r.state.search.is_some());

        let rec = |id, kind| Record {
            id,
            parent: None,
            ts_ms: 0,
            kind,
        };
        apply(
            &mut r.state,
            UiMsg::Resumed {
                path: "/logs/2.eid".into(),
                cwd: "/other".into(),
                model: "sonnet".into(),
                branch: vec![rec(
                    1,
                    eidolon_core::session::RecordKind::UserMessage(
                        eidolon_core::message::Message::user_text("the other one"),
                    ),
                )],
                pins: Vec::new(),
                fresh: false,
            },
        );

        assert_eq!(r.state.model, "sonnet");
        assert_eq!(r.state.cwd, "/other");
        assert!(
            r.state.search.is_none() && r.state.jump.is_none(),
            "a hit into the old transcript cannot survive"
        );
        assert_eq!(
            (r.state.usage_in, r.state.tools_run, r.state.scroll_up),
            (0, 0, 0)
        );
        // The adopted branch, and a line saying what happened.
        assert!(matches!(&r.state.transcript[0], Entry::User(t) if t == "the other one"));
        assert!(
            matches!(r.state.transcript.last(), Some(Entry::Info(s)) if s.starts_with("resumed"))
        );
        // A preference is not a pointer, so it stays.
        assert_eq!(r.state.detail, crate::state::Detail::Full);
    }

    /// The three things that make a session what it is move together: the
    /// log, the directory its tools run in, and (with a catalog) the
    /// backend for its model.
    #[tokio::test]
    async fn resuming_moves_the_log_and_the_working_directory_together() {
        use eidolon_core::agent::AgentConfig;
        use eidolon_core::dispatch::Dispatcher;
        use eidolon_core::event::EventBus;
        use eidolon_core::policy::AllowAll;
        use eidolon_core::session::RecordKind;
        use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
        use eidolon_core::tool::ToolRegistry;

        let tmp = tempfile::tempdir().unwrap();
        let (here, there) = (tmp.path().join("here"), tmp.path().join("there"));
        // Both directories exist, so the recorded cwd is the one the
        // session adopts — the fallback below covers the other branch.
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&there).unwrap();
        let (first, second) = (tmp.path().join("1.eid"), tmp.path().join("2.eid"));
        let open = Session::create(&first, "mock", &here, None).unwrap();
        let mut other = Session::create(&second, "mock", &there, None).unwrap();
        other
            .append(RecordKind::UserMessage(
                eidolon_core::message::Message::user_text("the other one"),
            ))
            .unwrap();
        drop(other);

        let dispatcher = Arc::new(Dispatcher::new(
            ToolRegistry::new(),
            Arc::new(AllowAll),
            ScriptedUser::new(false),
            EventBus::default(),
            Arc::new(tokio::sync::Mutex::new(open)),
            here.clone(),
        ));
        let agent = Arc::new(Agent::new(
            ScriptedProvider::new(Vec::new()),
            dispatcher,
            AgentConfig {
                model: "mock".into(),
                ..Default::default()
            },
        ));
        let ctx = DriverCtx {
            catalog: None,
            cwd: here.clone(),
            scratch: tmp.path().to_path_buf(),
            palette: None,
            sessions_dir: tmp.path().to_path_buf(),
            swarm: None,
        };

        let msg = adopt(&agent, &ctx, Session::open(&second).unwrap())
            .await
            .unwrap();
        match msg {
            UiMsg::Resumed {
                path, cwd, branch, ..
            } => {
                assert_eq!(path, second.display().to_string());
                assert_eq!(cwd, there.display().to_string());
                assert_eq!(branch.len(), 2, "the start record and the message");
            }
            _ => panic!("adopting a session announces itself as a resume"),
        }
        assert_eq!(
            agent.session().lock().await.path(),
            second,
            "the agent journals into the adopted log"
        );
        assert_eq!(
            agent.dispatcher().cwd(),
            there,
            "and its tools run where that log was started"
        );
    }

    /// A log whose recorded cwd does not exist on this machine — the shape
    /// of a session carried from another machine *without* an override in
    /// memory, every plain reopen of a projection — adopts the running
    /// session's directory instead of pointing the tools at a path only the
    /// birth machine has. The `Resumed` message reports whichever cwd this
    /// resolved to, so the substitution is shown, not silent.
    #[tokio::test]
    async fn adopting_a_log_whose_cwd_is_gone_runs_where_the_session_stands() {
        use eidolon_core::agent::AgentConfig;
        use eidolon_core::dispatch::Dispatcher;
        use eidolon_core::event::EventBus;
        use eidolon_core::policy::AllowAll;
        use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
        use eidolon_core::tool::ToolRegistry;

        let tmp = tempfile::tempdir().unwrap();
        let here = tmp.path().join("here");
        std::fs::create_dir_all(&here).unwrap();
        let (first, second) = (tmp.path().join("1.eid"), tmp.path().join("2.eid"));
        let open = Session::create(&first, "mock", &here, None).unwrap();
        // The second log records a birth directory that is never created:
        // `/root/…` on the machine it was born on, say. Dropped before the
        // adopt, so no reopen-time override is in memory — the plain shape.
        let mut other = Session::create(
            &second,
            "mock",
            std::path::Path::new("/root/Melete/chat/sandbox"),
            None,
        )
        .unwrap();
        other
            .append(RecordKind::UserMessage(
                eidolon_core::message::Message::user_text("the other one"),
            ))
            .unwrap();
        drop(other);

        let dispatcher = Arc::new(Dispatcher::new(
            ToolRegistry::new(),
            Arc::new(AllowAll),
            ScriptedUser::new(false),
            EventBus::default(),
            Arc::new(tokio::sync::Mutex::new(open)),
            here.clone(),
        ));
        let agent = Arc::new(Agent::new(
            ScriptedProvider::new(Vec::new()),
            dispatcher,
            AgentConfig {
                model: "mock".into(),
                ..Default::default()
            },
        ));
        let ctx = DriverCtx {
            catalog: None,
            cwd: here.clone(),
            scratch: tmp.path().to_path_buf(),
            palette: None,
            sessions_dir: tmp.path().to_path_buf(),
            swarm: None,
        };

        let msg = adopt(&agent, &ctx, Session::open(&second).unwrap())
            .await
            .unwrap();
        match msg {
            UiMsg::Resumed { cwd, .. } => {
                assert_eq!(cwd, here.display().to_string(), "the birth cwd stood in for");
            }
            _ => panic!("adopting a session announces itself as a resume"),
        }
        assert_eq!(
            agent.dispatcher().cwd(),
            here,
            "the tools run where the session stands, not where the log was born"
        );
    }

    /// A session that arrived from another machine adopts the cwd it was
    /// reopened under, not the birth directory its start record carries —
    /// and the projection note rides the branch it hands over.
    #[tokio::test]
    async fn an_adopted_projection_runs_where_it_landed_not_where_it_was_born() {
        use eidolon_core::agent::AgentConfig;
        use eidolon_core::dispatch::Dispatcher;
        use eidolon_core::event::EventBus;
        use eidolon_core::policy::AllowAll;
        use eidolon_core::session::RecordKind;
        use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
        use eidolon_core::tool::ToolRegistry;

        let tmp = tempfile::tempdir().unwrap();
        let (born, landed) = (tmp.path().join("born"), tmp.path().join("landed"));
        std::fs::create_dir_all(&born).unwrap();
        std::fs::create_dir_all(&landed).unwrap();
        let (first, second) = (tmp.path().join("1.eid"), tmp.path().join("2.eid"));
        let open = Session::create(&first, "mock", &born, None).unwrap();
        let mut other = Session::create(&second, "mock", &born, None).unwrap();
        other
            .append(RecordKind::UserMessage(
                eidolon_core::message::Message::user_text("the other one"),
            ))
            .unwrap();
        drop(other);

        let dispatcher = Arc::new(Dispatcher::new(
            ToolRegistry::new(),
            Arc::new(AllowAll),
            ScriptedUser::new(false),
            EventBus::default(),
            Arc::new(tokio::sync::Mutex::new(open)),
            born.clone(),
        ));
        let agent = Arc::new(Agent::new(
            ScriptedProvider::new(Vec::new()),
            dispatcher,
            AgentConfig {
                model: "mock".into(),
                ..Default::default()
            },
        ));
        let ctx = DriverCtx {
            catalog: None,
            cwd: born.clone(),
            scratch: tmp.path().to_path_buf(),
            palette: None,
            sessions_dir: tmp.path().to_path_buf(),
            swarm: None,
        };

        // The shipped file is reopened the way a venue receives it: as a
        // projection running in `landed`, born in `born`.
        let msg = adopt(
            &agent,
            &ctx,
            Session::open_projected(&second, &landed, Some("desktop")).unwrap(),
        )
        .await
        .unwrap();
        match msg {
            UiMsg::Resumed {
                cwd, branch, ..
            } => {
                assert_eq!(cwd, landed.display().to_string());
                // The branch carries both landing facts: the operator-facing
                // projection note, and behind it the model-facing brief —
                // the one message that tells the model it moved.
                assert!(
                    matches!(branch[branch.len() - 2].kind, RecordKind::Note { .. }),
                    "the projection note rides the branch"
                );
                assert!(
                    matches!(
                        branch.last().unwrap().kind,
                        RecordKind::ExternalMessage { .. }
                    ),
                    "the landing brief rides the branch for the model"
                );
            }
            _ => panic!("adopting a session announces itself as a resume"),
        }
        assert_eq!(
            agent.dispatcher().cwd(),
            landed,
            "its tools run where the projection landed"
        );
        // The birth directory is history, untouched on the start record.
        let adopted = agent.session().lock().await;
        let (_, start_cwd, _) = adopted.start().unwrap();
        assert_eq!(start_cwd, born.to_str().unwrap());
    }

    /// A new session is another one of *these* — same model, same
    /// directory — and it becomes the one in play.
    #[tokio::test]
    async fn a_new_session_lands_beside_the_others_and_takes_over() {
        use eidolon_core::agent::AgentConfig;
        use eidolon_core::dispatch::Dispatcher;
        use eidolon_core::event::EventBus;
        use eidolon_core::policy::AllowAll;
        use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
        use eidolon_core::tool::ToolRegistry;

        let tmp = tempfile::tempdir().unwrap();
        let logs = tmp.path().join("logs");
        let cwd = tmp.path().join("work");
        let first = tmp.path().join("1.eid");
        let open = Session::create(&first, "mock", &cwd, None).unwrap();
        let dispatcher = Arc::new(Dispatcher::new(
            ToolRegistry::new(),
            Arc::new(AllowAll),
            ScriptedUser::new(false),
            EventBus::default(),
            Arc::new(tokio::sync::Mutex::new(open)),
            cwd.clone(),
        ));
        let agent = Arc::new(Agent::new(
            ScriptedProvider::new(Vec::new()),
            dispatcher,
            AgentConfig {
                model: "mock".into(),
                ..Default::default()
            },
        ));
        let ctx = DriverCtx {
            catalog: None,
            cwd: cwd.clone(),
            scratch: tmp.path().to_path_buf(),
            palette: None,
            sessions_dir: logs.clone(),
            swarm: None,
        };

        // The directory does not exist yet, as it will not on a first run.
        let msg = new_session(&agent, &ctx).await.unwrap();
        let UiMsg::Resumed { path, branch, .. } = msg else {
            panic!("a new session is announced like a resume")
        };
        assert!(Path::new(&path).starts_with(&logs) && path.ends_with(".eid"));
        assert_eq!(branch.len(), 1, "nothing but the start record");
        assert_eq!(
            agent.session().lock().await.path().display().to_string(),
            path
        );
        // Started here, on the model in play, so the picker files it under
        // this directory next time.
        let listed = crate::sessions::list(&logs, &cwd, true);
        assert_eq!(listed.len(), 1);
        assert!(listed[0].is_local(&cwd));
        assert_eq!(listed[0].model, "mock");
    }

    /// The script's table and the registry are checked against each other
    /// at startup, so a typo in a hand-written `ui.rn` is visible.
    /// A script with two modes of its own: `review`, a normal-kind mode
    /// with its own keys, and `shout`, a typing mode whose `ret` means
    /// something else — plus dispatch restyled and one theme role moved.
    const OWN_MODES: &str = r##"
        pub fn view(s) { column([fill(transcript()), fixed(3, prompt(())), fixed(1, status([on(` ${s.mode_label} `, s.mode_colour)], []))]) }
        pub fn on_submit(s, text) {
            if s.mode == "shout" { return send(`SHOUT: ${text}`); }
            if s.mode == "dispatch" { return dispatch(text); }
            match ui::parse_command(text.clone()) { Some(cmd) => command(cmd.name, cmd.arg), None => send(text) }
        }
        pub fn modes() {
            #{
                "review": mode("normal", "REV", "cyan"),
                "shout": sigil(mode("insert", "SHOUT", "#ff8800"), ">"),
                "dispatch": mode("insert", "DISP", "#00ff00"),
            }
        }
        pub fn theme() { #{ user: "#123456" } }
        pub fn keymap() {
            let out = #{ "esc": cmd("normal_mode", "normal mode"), "ret": cmd("submit", "send") };
            #{
                "normal": #{
                    "i": cmd("insert_mode", "insert"),
                    "R": cmd_arg("enter_mode", "review", "review mode"),
                    "S": cmd_arg("enter_mode", "shout", "shout mode"),
                    "!": cmd("dispatch_mode", "dispatch mode"),
                    ":": cmd("command_line", "command line"),
                    "ret": cmd("submit", "send"),
                },
                "insert": out,
                "select": out,
                "dispatch": out,
                "review": #{ "esc": cmd("normal_mode", "normal mode"), "q": cmd("quit", "quit") },
                "shout": out,
            }
        }
    "##;

    /// Every notice said so far — what `:messages` would page.
    fn infos(r: &Rig) -> Vec<String> {
        r.state.notices.iter().map(|(_, t)| t.clone()).collect()
    }

    #[test]
    fn a_mode_of_the_scripts_own_is_a_row_a_keymap_table_and_a_way_in() {
        let mut r = Rig::with_script(OWN_MODES);
        assert!(
            infos(&r).is_empty(),
            "a consistent script loads without a word: {:?}",
            infos(&r)
        );
        // In by a binding that carries its argument.
        r.press("R");
        assert!(r.state.prompt.mode.is("review"));
        assert_eq!(r.state.prompt.mode.kind, crate::edit::Kind::Normal);
        let snap = r.state.snapshot();
        assert_eq!(snap["mode_label"], "REV");
        assert_eq!(snap["mode_colour"], "cyan");
        assert_eq!(snap["mode_kind"], "normal");
        // Its own table is live: `q` quits here and `i` does nothing.
        r.press("i");
        assert!(
            r.state.prompt.mode.is("review"),
            "`i` is not bound in review mode"
        );
        r.press("esc");
        assert!(r.state.prompt.mode.is("normal"));
        // In by name on the `:` line, and `ret` there means what the
        // script says it means.
        r.press(":").typed("enter_mode shout").press("ret");
        assert!(r.state.prompt.mode.is("shout"));
        assert!(r.state.prompt.mode.typing());
        assert_eq!(r.state.snapshot()["mode_sigil"], ">");
        r.typed("hello").press("ret");
        assert!(
            r.cmds()
                .iter()
                .any(|c| c.contains("Send(\"SHOUT: hello\")")),
            "{:?}",
            r.cmds()
        );
        // A colon typed in a typing mode of the script's own is a
        // character, as in dispatch — not a command line.
        r.typed(":quit");
        assert!(r.state.command_line().is_none());
        assert_eq!(r.text(), ":quit");
    }

    #[test]
    fn dispatch_takes_its_look_from_the_table_and_the_theme_recolours_the_rest() {
        let mut r = Rig::with_script(OWN_MODES);
        r.press("!");
        assert_eq!(r.state.mode_name(), "dispatch");
        let snap = r.state.snapshot();
        assert_eq!(snap["mode_colour"], "#00ff00");
        assert_eq!(snap["mode_sigil"], "", "the override dropped the `!`");
        assert_eq!(r.state.theme.name("user"), "#123456");
        assert_eq!(
            r.state.theme.name("tool"),
            "magenta",
            "unnamed roles keep the default's"
        );
    }

    #[test]
    fn enter_mode_refuses_what_the_table_does_not_define_and_picks_when_unnamed() {
        let mut r = Rig::with_script(OWN_MODES);
        r.press(":").typed("enter_mode nope").press("ret");
        assert!(
            infos(&r)
                .last()
                .is_some_and(|t| t.contains("no mode `nope`")),
            "{:?}",
            infos(&r)
        );
        assert!(r.state.prompt.mode.is("normal"));
        // A one-line mode is *opened* rather than entered, and naming
        // one reaches the same thing its own key does — so this is the
        // `:` line asking for the `:` line, which is already up.
        r.press(":").typed("enter_mode search").press("ret");
        assert_eq!(r.state.mode_name(), "search", "`:enter_mode search` is `/`");
        r.press("esc");
        // No name: the table, as a picker.
        r.press(":").typed("enter_mode").press("ret");
        let d = r.state.dialog.as_ref().expect("a picker over the modes");
        assert_eq!(d.kind, DialogKind::Pick);
        assert!(d.keys.contains(&"review".to_string()));
        assert!(
            d.keys.contains(&"command".to_string()),
            "and the one-line modes are offered too"
        );
        assert!(
            !d.keys.contains(&"confirm".to_string()),
            "a dialog is not a mode you enter"
        );
    }

    #[test]
    fn a_table_and_a_keymap_that_disagree_say_so_at_load() {
        let src = r#"
            pub fn view(s) { column([fill(transcript()), fixed(3, prompt(()))]) }
            pub fn on_submit(s, text) { send(text) }
            pub fn modes() { #{ "review": mode("normal", "REV", "cyan"), "insert": mode("normal", "INS", "green") } }
            pub fn theme() { #{ hits: "red" } }
            pub fn keymap() { #{ "normal": #{ "i": cmd("insert_mode", "insert") }, "zzz": #{ "esc": cmd("normal_mode", "normal") } } }
        "#;
        let r = Rig::with_script(src);
        let said = infos(&r).join("\n");
        assert!(
            said.contains("must keep kind"),
            "rekinding insert is refused: {said}"
        );
        assert!(said.contains("`zzz`"), "a keymap table for no mode: {said}");
        assert!(
            said.contains("roles nothing paints: hits"),
            "a misspelt theme role: {said}"
        );
        // The refused table fell back to the default's, whole.
        assert!(r.state.modes.get("review").is_none());
        assert_eq!(r.state.modes.style("dispatch").sigil, Some('!'));
        // And the default's editing modes have no tables in this keymap.
        assert!(said.contains("mode `select` has no keymap table"), "{said}");
    }

    #[test]
    fn reload_ui_reads_the_tables_again_and_the_prompt_survives_losing_its_mode() {
        let mut r = Rig::with_script(OWN_MODES);
        r.press("R");
        assert!(r.state.prompt.mode.is("review"));
        // The command only asks; the loop is what reloads. (`:` is not
        // bound in review mode, so the line is opened from normal and the
        // prompt put back into review before the reload.)
        r.press("esc").press(":").typed("reload_ui").press("ret");
        assert!(r.state.reload);
        r.press("R");
        assert!(r.state.prompt.mode.is("review"));
        // Reloading a script with no `review` row puts the prompt back in
        // normal rather than leaving it in a mode nothing describes.
        let (script, keys) = load_ui(&mut r.state, crate::DEFAULT_UI);
        r.script = script.unwrap();
        r.keys = keys;
        assert!(r.state.prompt.mode.is("normal"));
        assert!(r.state.modes.get("review").is_none());
        assert_eq!(
            r.state.modes.style("dispatch").sigil,
            Some('!'),
            "dispatch is the default's row again"
        );
        assert_eq!(r.state.theme.name("user"), "green");
        // And the default keymap is live: `R` no longer does anything.
        r.press("R");
        assert!(r.state.prompt.mode.is("normal"));
    }

    /// A script with a command of its own, bound to a key, typed on the
    /// `:` line, completed, and listed — plus two rows that are refused.
    const OWN_COMMANDS: &str = r##"
        pub fn view(s) { column([fill(transcript()), fixed(3, prompt(()))]) }
        pub fn on_submit(s, text) {
            match ui::parse_command(text.clone()) { Some(cmd) => command(cmd.name, cmd.arg), None => send(text) }
        }
        pub fn commands() {
            #{
                "shout": custom_arg("shout", "TEXT", "send TEXT in capitals"),
                "draft": custom("draft", "put a greeting in the prompt"),
                "model": custom("nope", "a built-in's name"),
                "ghost": custom("no_such_fn", "a function that is not there"),
            }
        }
        pub fn shout(s, arg) { send(`SHOUT ${arg}`) }
        pub fn draft(s, arg) { insert("hello there") }
        pub fn keymap() {
            #{
                "normal": #{ "Z": cmd_arg("shout", "hi", "shout hi"), "D": cmd("draft", "draft"), ":": cmd("command_line", "command line"), "i": cmd("insert_mode", "insert"), "ret": cmd("submit", "send") },
                "insert": #{ "esc": cmd("normal_mode", "normal"), "ret": cmd("submit", "send") },
                "command": #{ "esc": cmd("normal_mode", "normal"), "ret": cmd("submit", "send"), "tab": cmd("complete_next", "next") },
                "select": #{ "esc": cmd("normal_mode", "normal") },
                "dispatch": #{ "esc": cmd("normal_mode", "normal") },
            }
        }
    "##;

    #[test]
    fn a_scripts_own_command_is_bound_typed_completed_and_listed_like_a_builtin() {
        let mut r = Rig::with_script(OWN_COMMANDS);
        let said = infos(&r).join("\n");
        assert!(said.contains("`model` is a built-in"), "{said}");
        assert!(
            said.contains("`ghost` names a function `no_such_fn`"),
            "{said}"
        );
        assert!(
            !said.contains("do not exist"),
            "the bindings to shout and draft are known: {said}"
        );
        assert_eq!(r.state.script_commands.len(), 2);
        // Bound, with its argument carried by the binding.
        r.press("Z");
        assert!(
            r.cmds().iter().any(|c| c.contains("Send(\"SHOUT hi\")")),
            "{:?}",
            r.cmds()
        );
        // Typed, with an argument.
        r.press(":").typed("shout loud").press("ret");
        assert!(
            r.cmds().iter().any(|c| c.contains("Send(\"SHOUT loud\")")),
            "{:?}",
            r.cmds()
        );
        // Completed on the `:` line.
        r.press(":").typed("sho").press("tab");
        assert_eq!(
            r.line(),
            ":shout ",
            "it takes an argument, so the space comes with the name"
        );
        r.press("esc");
        r.state.prompt.clear();
        // An `insert` action types into the prompt.
        r.press("D");
        assert_eq!(r.text(), "hello there");
        // Offered on the `:` line beside the built-ins, and printed by
        // help.
        r.state.prompt.clear();
        r.press("esc").press(":").typed("sho");
        let m = r.state.menu.clone().expect("the popup");
        assert!(
            m.items.iter().any(|(k, _)| k == "shout TEXT"),
            "{:?}",
            m.items
        );
        r.press("esc");
        r.state.prompt.clear();
        r.press(":").typed("help").press("ret");
        let help = r.page();
        assert!(help.contains(":shout TEXT"), "{help}");
        assert!(
            help.lines().any(|line| line.trim_start().starts_with("Z ") && line.contains("shout hi")),
            "the keys come from the live keymap: {help}"
        );
    }

    #[test]
    fn context_page_is_a_footprint_dashboard_not_a_replay() {
        use eidolon_core::agent::{ContextBlock, ContextReport};
        let report = ContextReport {
            blocks: vec![
                ContextBlock { kind: "system", label: "instructions".into(), chars: 60 },
                ContextBlock { kind: "user", label: "first request".into(), chars: 20 },
                ContextBlock { kind: "assistant", label: "first answer".into(), chars: 10 },
                ContextBlock { kind: "pins", label: "AGENTS.md".into(), chars: 10 },
            ],
            chars: 100, guidance_chars: 30, tokens: None, compacted: Some(4),
            sources: vec![ContextBlock { kind: "tool results", label: "".into(), chars: 70 },
                ContextBlock { kind: "system", label: "".into(), chars: 30 }],
            tools: vec![eidolon_core::agent::ContextTool { id: "call-7".into(), name: "read".into(),
                input_chars: 0, result_chars: 70, is_error: true }],
        };
        let text = context_report_lines(&report, Some(1000));
        assert!(text.starts_with("── Overview ──"));
        assert!(text.contains("100c · 4 blocks"));
        assert!(text.contains("30c (included in system)"));
        assert!(text.contains("4 messages folded away"));
        assert!(text.contains("not token estimates"));
        assert!(text.contains("AGENTS.md"));
        assert!(!text.contains("first request"));
        assert!(!text.contains("first answer"));
        assert!(!text.contains("replay order"));
        assert!(text.contains("70.0%"));
        assert!(text.contains("id call-7"));
        assert!(text.contains("read │ 70c · error"));
    }

    #[test]
    fn tree_page_groups_forks_without_indenting_every_record() {
        use eidolon_core::{Record, RecordKind, TreeNode};
        let records: Vec<_> = [(1, None), (2, Some(1)), (3, Some(2)), (4, Some(1)), (5, Some(4))]
            .into_iter().map(|(id, parent)| Record { id, parent, ts_ms: 0, kind: RecordKind::Cancelled }).collect();
        let nodes: Vec<_> = records.iter().map(|record| TreeNode {
            record, children: records.iter().filter(|r| r.parent == Some(record.id)).map(|r| r.id).collect(),
            is_head: record.id == 5, on_branch: [1, 4, 5].contains(&record.id),
        }).collect();
        let text = tree_report(&nodes).body;
        assert!(text.contains("5 records · 1 fork points · head #5"));
        assert!(text.contains("├─○ #2"));
        assert!(text.contains("│ ○ #3"));
        assert!(text.contains("└─◆ #4"));
        assert!(text.contains("    ● #5"));
        assert_eq!(text.matches("← HEAD").count(), 1);
        assert!(text.find("#3").unwrap() < text.find("#4").unwrap());
        assert!(tree_report(&[]).body.contains("No records yet."));
    }

    #[test]
    fn tree_reader_searches_inspects_and_forks_real_records() {
        use eidolon_core::{Record, RecordKind, TreeNode};
        let records = [
            Record { id: 41, parent: None, ts_ms: 0, kind: RecordKind::Cancelled },
            Record { id: 99, parent: Some(41), ts_ms: 0, kind: RecordKind::Note { text: "needle [[Not a vault link]]".into() } },
        ];
        let nodes = [
            TreeNode { record: &records[0], children: vec![99], is_head: false, on_branch: true },
            TreeNode { record: &records[1], children: vec![], is_head: true, on_branch: true },
        ];
        let tree = tree_report(&nodes);
        for (line, record) in &tree.records {
            assert!(tree.body.lines().nth(line - 1).unwrap().contains(&format!("#{}", record.id)));
        }
        let mut r = Rig::new();
        r.state.transcript_rect = ratatui::layout::Rect::new(0, 0, 50, 16);
        r.state.prompt.insert("unfinished draft");
        apply(&mut r.state, UiMsg::Tree(tree));
        assert!(r.state.dialog.is_none());
        assert!(crate::vault::document(&r.state).links.is_empty());
        r.press("tab");
        assert_eq!(crate::vault::tree_record(&r.state).unwrap().id, 41);
        r.press("K /").typed("needle").press("ret");
        assert_eq!(crate::vault::tree_record(&r.state).unwrap().id, 99);
        let trace = r.state.vault.trace;
        r.press("ret");
        assert!(r.state.vault.page.as_ref().unwrap().prompt.contains("record #99"));
        assert!(r.state.vault.page.as_ref().unwrap().body.contains("Not a vault link"));
        assert!(r.state.search.is_none(), "detail has its own search");
        r.press("backspace");
        assert_eq!(r.state.vault.trace, trace);
        assert!(r.state.search.is_some(), "back restores the tree search");
        r.state.transcript_rect.width = 20;
        r.press("y");
        assert!(r.state.prompt.register.contains("#99"));
        r.press("space f");
        assert!(matches!(r.rx.try_recv().unwrap(), Cmd::Command { name, arg } if name == "fork" && arg == "99"));
        r.press("q");
        assert!(!r.state.vault.active);
        assert_eq!(r.state.prompt.text(), "unfinished draft");
        assert!(r.rx.try_recv().is_err(), "inspection is neither a tool nor a model call");
    }

    #[test]
    fn help_is_generated_from_the_keymap_and_the_registry() {
        let mut r = Rig::new();
        r.press(":").typed("help").press("ret");
        let help = r.page();
        assert!(help.contains("normal (NOR · normal)"), "{help}");
        assert!(
            help.lines().any(|line| line.contains("space m") && line.contains("model picker")),
            "chords are spelled out: {help}"
        );
        assert!(
            help.contains("search (FIND)"),
            "the search line's table is listed: {help}"
        );
        assert!(
            help.contains(":enter_mode [NAME]"),
            "the vocabulary follows: {help}"
        );
    }

    /// What a keystroke came to is a word on the status line and never
    /// an entry: the transcript is what the log replays, and "no search
    /// yet" is in no log. It stays for a while and is then taken down,
    /// and `:messages` still has it.
    #[test]
    fn a_notice_is_a_word_on_the_status_line_and_never_an_entry() {
        let mut r = Rig::new();
        r.press("n");
        assert!(r.said().contains("no search yet"), "{:?}", r.state.notice);
        assert!(
            r.state.transcript.is_empty(),
            "the transcript is for the conversation: {:?}",
            r.state.transcript
        );
        assert!(!r.state.expire_notice(), "not yet");
        r.state.notice.as_mut().unwrap().at =
            std::time::Instant::now() - crate::state::NOTICE_TTL - Duration::from_secs(1);
        assert!(r.state.expire_notice());
        assert!(r.state.notice.is_none());
        r.press(":").typed("messages").press("ret");
        assert!(r.page().contains("no search yet"), "{}", r.page());
    }

    /// A page owns the keyboard the way a picker does, scrolls in the
    /// lines the frame draws, and closes leaving nothing behind — not
    /// on the transcript, and not on the prompt.
    #[test]
    fn a_page_scrolls_in_drawn_lines_and_closes_without_a_trace() {
        let mut r = Rig::new();
        r.state.screen = (80, 12);
        r.press(":").typed("help").press("ret");
        assert_eq!(r.state.mode_name(), "page");
        assert_eq!(r.state.dialog.as_ref().unwrap().selected, 0);
        r.press("j j j");
        assert_eq!(r.state.dialog.as_ref().unwrap().selected, 3);
        let (width, rows) = crate::render::page_layout(r.state.screen);
        let last = crate::render::page_lines(&r.page(), width)
            .len()
            .saturating_sub(rows);
        assert!(last > 3, "the help does not fit twelve rows: {last}");
        r.press("g e");
        assert_eq!(
            r.state.dialog.as_ref().unwrap().selected,
            last,
            "the end is the last line drawn, not past it"
        );
        r.press("j");
        assert_eq!(r.state.dialog.as_ref().unwrap().selected, last);
        r.press("S-up");
        assert_eq!(r.state.dialog.as_ref().unwrap().selected, last - rows);
        r.press("g g");
        assert_eq!(r.state.dialog.as_ref().unwrap().selected, 0);
        // A letter that is not a key does nothing — a page has no field.
        r.press("x");
        assert_eq!(r.state.mode_name(), "page");
        r.press("q");
        assert!(r.state.dialog.is_none());
        assert!(r.state.transcript.is_empty(), "{:?}", r.state.transcript);
        assert!(r.text().is_empty());
    }

    /// `:usage` is a page built from the ledger on the UI thread, on the
    /// leader as `space u`, and redrawn while it is up when a turn
    /// settles under it.
    #[test]
    fn the_usage_page_reads_the_ledger_and_follows_a_settle() {
        let mut r = Rig::new();
        r.state.screen = (100, 30);
        r.press(":").typed("usage").press("ret");
        assert_eq!(
            r.state.dialog.as_ref().map(|d| d.prompt.as_str()),
            Some("usage")
        );
        assert!(r.page().contains("none settled yet"), "{}", r.page());
        let usage = eidolon_core::Usage {
            input_tokens: 5_000,
            output_tokens: 300,
            ..Default::default()
        };
        apply(&mut r.state, UiMsg::Bus(Event::MessageStart));
        assert!(
            r.page().starts_with("0 turns"),
            "not redrawn until something settles: {}",
            r.page()
        );
        apply(
            &mut r.state,
            UiMsg::Bus(Event::ContextSize { tokens: 5_000 }),
        );
        apply(
            &mut r.state,
            UiMsg::Bus(Event::TurnSettled {
                stop_reason: eidolon_core::StopReason::EndTurn,
                usage,
                timing: None,
            }),
        );
        assert!(r.page().starts_with("1 turn"), "{}", r.page());
        assert!(r.page().contains("turns      1 settled"), "{}", r.page());
        assert!(r.page().contains("context    5000"), "{}", r.page());
        assert_eq!(r.state.ledger.turns.len(), 1);
        r.press("q");
        assert!(r.state.dialog.is_none());
        r.press("space u");
        assert_eq!(
            r.state.dialog.as_ref().map(|d| d.prompt.as_str()),
            Some("usage")
        );
        assert_eq!(r.state.mode_name(), "page");
    }

    /// A plan's windows reach the page as a reading from the driver, are
    /// redrawn under the reader when one lands, are shown only under the
    /// provider they were read from, and are asked for again when the
    /// panel opens on a stale one.
    #[test]
    fn a_quota_reading_lands_on_the_usage_page_under_its_own_provider() {
        use crate::usage::QuotaReading;
        use eidolon_providers::{Quota, QuotaWindow};
        let mut r = Rig::new();
        r.state.screen = (100, 30);
        r.state.model = "zai:glm-5.3".into();
        r.press(":").typed("usage").press("ret");
        assert!(
            !r.page().contains("quota"),
            "nothing read yet: {}",
            r.page()
        );
        assert_eq!(r.cmds(), ["Quota"], "opening with no reading asks for one");

        let reading = |read_ms: u64, pct: f64| QuotaReading {
            provider: "zai".into(),
            read_ms,
            result: Ok(Quota {
                plan: Some("lite".into()),
                windows: vec![QuotaWindow {
                    label: "5h".into(),
                    percent: pct,
                    ..Default::default()
                }],
            }),
        };
        let now = crate::usage::now_ms();
        apply(&mut r.state, UiMsg::Quota(reading(now, 7.0)));
        assert!(
            r.page()
                .contains("quota      zai \u{b7} lite plan \u{b7} as of 0s ago"),
            "redrawn under the reader: {}",
            r.page()
        );
        assert!(
            r.page().contains("           5h      7%  \u{2588}"),
            "{}",
            r.page()
        );
        // An older reading arriving late does not overwrite a newer one.
        apply(&mut r.state, UiMsg::Quota(reading(now - 5_000, 3.0)));
        assert!(
            r.page().contains("7%") && !r.page().contains("3%"),
            "{}",
            r.page()
        );

        // Fresh: reopening does not ask again.
        r.press("q");
        r.press("space u");
        assert!(
            r.cmds().is_empty(),
            "a fresh reading is not asked for again"
        );
        r.press("q");

        // Another provider's model: the reading is held, not drawn.
        apply(
            &mut r.state,
            UiMsg::ModelChanged("deepseek:deepseek-v4-pro".into()),
        );
        r.press("space u");
        assert!(
            !r.page().contains("quota"),
            "z.ai's window is not drawn under DeepSeek: {}",
            r.page()
        );
        assert_eq!(
            r.cmds(),
            ["Quota"],
            "and a reading for this provider is asked for"
        );
        r.press("q");
        apply(
            &mut r.state,
            UiMsg::ModelChanged("zai:glm-5.3-flash".into()),
        );
        r.press("space u");
        assert!(
            r.page().contains("quota      zai"),
            "back on the account, the reading is shown at once: {}",
            r.page()
        );

        // A failed read is a line, and is redrawn under the reader too.
        apply(
            &mut r.state,
            UiMsg::Quota(QuotaReading {
                provider: "zai".into(),
                read_ms: now + 1,
                result: Err("HTTP 401".into()),
            }),
        );
        assert!(
            r.page()
                .contains("quota      zai: could not be read: HTTP 401"),
            "{}",
            r.page()
        );
    }

    /// The driver's documents come the same way: a tree or a context
    /// report is a page, and a line about the conversation is an entry.
    #[test]
    fn a_page_and_a_note_from_the_driver_land_where_they_belong() {
        let mut r = Rig::new();
        apply(
            &mut r.state,
            UiMsg::Page {
                title: "tree".into(),
                text: "● #1 hello\n│ #2 there".into(),
            },
        );
        assert_eq!(
            r.state.dialog.as_ref().map(|d| d.prompt.as_str()),
            Some("tree")
        );
        assert!(r.page().contains("#2 there"));
        r.press("esc");
        apply(
            &mut r.state,
            UiMsg::Note("answering peer-1 (2 more wakes before you are asked)".into()),
        );
        assert!(
            matches!(r.state.transcript.last(), Some(Entry::Info(t)) if t.starts_with("answering"))
        );
        assert!(r.state.notice.is_none(), "a note is not a notice");
    }

    #[test]
    fn every_binding_in_the_default_keymap_names_a_real_command() {
        let s = UiScript::compile(crate::DEFAULT_UI).unwrap();
        assert_eq!(
            crate::keys::unknown_commands(&s.keymap().unwrap(), &[]),
            Vec::<String>::new()
        );
    }

    /// A key is bound in the mode where it means something and nowhere
    /// else — the popup under `space` is a promise about what these keys
    /// do *now*, and an entry needing state the mode cannot see turns it
    /// into a guess. The search line is a table of the keymap's own now,
    /// so the commands that only work while it is up live in that table
    /// and in no other; they stay reachable by name, since `:` reaches
    /// the whole registry by design.
    #[test]
    fn a_command_that_needs_the_search_line_is_bound_only_on_the_search_line() {
        let s = UiScript::compile(crate::DEFAULT_UI).unwrap();
        let mut km = s.keymap().unwrap();
        let on_line = km
            .as_object_mut()
            .unwrap()
            .remove("search")
            .expect("a search table");
        let bound = crate::keys::command_names(&km);
        let line_bound = crate::keys::command_names(&serde_json::json!({ "search": on_line }));
        for name in ["goto_hit", "reveal_match"] {
            assert!(
                command::lookup(name).is_some(),
                "{name} is still in the registry, and so still typable"
            );
            assert!(
                !bound.contains(&name.to_string()),
                "{name} is bound in a mode that cannot use it"
            );
            assert!(
                line_bound.contains(&name.to_string()),
                "{name} is bound on the search line"
            );
        }
        // The same rule for completion: it means something on the `:`
        // line and nowhere else.
        let cmd = km
            .as_object_mut()
            .unwrap()
            .remove("command")
            .expect("a command table");
        let bound = crate::keys::command_names(&km);
        assert!(
            !bound.contains(&"complete_next".to_string()),
            "completion is bound off the command line"
        );
        assert!(
            crate::keys::command_names(&serde_json::json!({ "command": cmd }))
                .contains(&"complete_next".to_string())
        );
        // Helix's own normal-mode search keys stay where the hands expect
        // them: they degrade with a message rather than meaning nothing.
        for name in [
            "search",
            "rsearch",
            "search_next",
            "search_prev",
            "search_selection",
        ] {
            assert!(
                bound.contains(&name.to_string()),
                "{name} should still be bound"
            );
        }
    }

    #[test]
    fn an_unknown_command_says_so_rather_than_failing_silently() {
        let mut r = Rig::new();
        exec(&mut r.state, None, &r.tx, "no_such_thing", 1, "");
        assert!(r.said().contains("no such command"), "{:?}", r.state.notice);
    }

    // ------------------------------------------------- search and tags

    #[test]
    fn slash_opens_the_search_line_and_typing_follows_the_matches() {
        let mut r = Rig::new();
        r.seeded(&["alpha", "beta", "gamma delta", "epsilon"]);
        r.state.scroll_up = 7;
        r.press("/");
        assert_eq!(r.state.snapshot()["mode"], "search", "the badge says so");
        r.typed("gam");
        assert_eq!(r.hit(), Some(2));
        assert_eq!(r.state.snapshot()["hits"], 1);
        assert_eq!(r.state.scroll_up, 0, "the view followed it");
        // Helix restores the view on a cancelled search, and so does this.
        r.press("esc");
        assert!(r.state.search.is_none());
        assert_eq!(r.state.scroll_up, 7);
    }

    /// The same gesture on the search line, and it is a *cancel*: the
    /// pattern goes with the line and the view goes back where it was,
    /// exactly as `esc` leaves it.
    #[test]
    fn backspace_on_an_empty_search_line_puts_the_view_back() {
        let mut r = Rig::new();
        r.seeded(&["alpha", "beta", "gamma"]);
        r.state.scroll_up = 5;
        r.press("/").typed("gam");
        assert_eq!(r.hit(), Some(2));
        r.press("backspace backspace backspace");
        assert_eq!(r.line(), "/", "the pattern is empty, the line is not");
        r.press("backspace");
        assert!(r.state.mini.is_none());
        assert!(r.state.search.is_none(), "the search went with the line");
        assert_eq!(r.state.scroll_up, 5, "and the view came back");
    }

    #[test]
    fn a_pattern_that_stops_matching_says_so_rather_than_moving() {
        let mut r = Rig::new();
        r.seeded(&["alpha", "beta"]);
        r.press("/").typed("alp");
        assert_eq!(r.hit(), Some(0));
        r.typed("zzz");
        assert_eq!(r.state.snapshot()["hits"], 0);
        assert_eq!(r.hit(), None, "nothing to sit on");
        // Backspacing out of the dead end finds it again.
        r.press("backspace backspace backspace");
        assert_eq!(r.hit(), Some(0));
    }

    #[test]
    fn ret_keeps_the_search_and_n_walks_it_wrapping() {
        let mut r = Rig::new();
        r.seeded(&["needle one", "hay", "needle two", "hay"]);
        r.press("/").typed("needle").press("ret");
        assert!(!r.state.searching(), "the line is down");
        assert!(r.state.search.is_some(), "the search is not");
        assert_eq!(r.hit(), Some(0));
        r.press("n");
        assert_eq!(r.hit(), Some(2));
        r.press("n");
        assert_eq!(r.hit(), Some(0), "wraps");
        r.press("N");
        assert_eq!(r.hit(), Some(2), "and back the other way");
        // The highlight outlives the line; esc is what puts it away.
        r.press("esc");
        assert!(r.state.search.is_none());
    }

    #[test]
    fn question_mark_searches_backwards_and_n_follows_that_direction() {
        let mut r = Rig::new();
        r.seeded(&["needle one", "hay", "needle two", "needle three"]);
        r.state.view_entries = 3..4;
        r.press("?").typed("needle").press("ret");
        assert_eq!(r.hit(), Some(3), "the nearest match at or before the view");
        r.press("n");
        assert_eq!(r.hit(), Some(2), "`n` repeats the search's own direction");
    }

    #[test]
    fn tab_tags_the_matches_on_screen_and_a_letter_goes_to_one() {
        let mut r = Rig::new();
        r.seeded(&["needle a", "hay", "needle b", "hay", "needle c"]);
        r.press("/").typed("needle").press("tab");
        let j = r.state.jump.as_ref().expect("tags over the three matches");
        assert_eq!(j.labels, ["a", "b", "c"]);
        r.press("c");
        assert!(r.state.jump.is_none(), "one press and the tags are gone");
        assert_eq!(r.hit(), Some(4));
    }

    #[test]
    fn only_the_matches_on_screen_are_worth_a_letter() {
        let mut r = Rig::new();
        r.seeded(&["needle a", "hay", "needle b", "hay", "needle c"]);
        // A view showing only the middle of the transcript.
        r.press("/").typed("needle");
        r.state.view_entries = 1..4;
        r.press("tab");
        let j = r.state.jump.as_ref().unwrap();
        assert_eq!(
            j.spots,
            vec![crate::jump::Spot::Hit(1)],
            "the second hit, and only it"
        );
        assert_eq!(j.labels, ["a"]);
    }

    #[test]
    fn nothing_on_screen_to_tag_says_so_instead_of_opening_empty_tags() {
        let mut r = Rig::new();
        r.seeded(&["needle", "hay"]);
        r.press("/").typed("needle");
        r.state.view_entries = 1..2;
        r.press("tab");
        assert!(r.state.jump.is_none());
        assert!(
            r.said().contains("no matches on screen"),
            "{:?}",
            r.state.notice
        );
    }

    #[test]
    fn gw_tags_the_prompts_words_and_a_tag_letter_is_a_tag_not_a_command() {
        let mut r = Rig::new();
        r.write("delete the cargo file").press("esc");
        r.press("g w");
        let spots = &r.state.jump.as_ref().unwrap().spots;
        assert_eq!(
            spots,
            &vec![Spot::Word(0), Spot::Word(7), Spot::Word(11), Spot::Word(17)]
        );
        // `d` is `delete_selection` in normal mode. With tags up it is the
        // fourth tag, and the buffer had better still be there afterwards.
        r.press("d");
        assert_eq!(r.text(), "delete the cargo file");
        assert_eq!(r.state.prompt.range.cursor(), 17, "on the `f` of `file`");
        assert!(r.state.jump.is_none());
    }

    /// `gw` aims at the **screen**, not at the prompt: it tags the
    /// visible conversation as well as the draft, so it has something to
    /// do with an empty prompt — which is when it used to complain.
    #[test]
    fn gw_tags_the_transcript_as_well_and_a_transcript_tag_yanks() {
        let mut r = Rig::new();
        r.seeded(&["read crates/tui/src/edit.rs"]);
        r.state.transcript_rect = ratatui::layout::Rect::new(0, 0, 60, 4);
        r.press("g w");
        let spots = r
            .state
            .jump
            .as_ref()
            .expect("tags over the transcript")
            .spots
            .clone();
        // Whitespace-delimited, so the path is one tag and not four —
        // a tag here names a thing you are about to take.
        let words: Vec<&str> = spots
            .iter()
            .filter_map(|s| match s {
                Spot::Cell { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(words.contains(&"crates/tui/src/edit.rs"), "{words:?}");
        assert!(
            !words.contains(&"crates"),
            "the path is not cut at its slashes: {words:?}"
        );
        // Landing takes it: selected on screen, and in the register.
        r.press("a");
        assert!(r.state.jump.is_none());
        assert!(r.state.selection.is_some(), "the word is lit where it sits");
        assert!(r.said().contains("yanked"), "{:?}", r.state.notice);
        r.press("i").press("C-y");
        assert!(
            r.text().contains("read"),
            "and `p`/`C-y` puts it in the message: {:?}",
            r.text()
        );
    }

    /// With nothing on screen and nothing in the prompt there is nothing
    /// to tag, and it says so — once, rather than complaining about the
    /// prompt while a screenful sits above it.
    #[test]
    fn tags_with_nothing_on_screen_say_so() {
        let mut r = Rig::new();
        r.press("g w");
        assert!(r.state.jump.is_none());
        assert!(
            r.said().contains("nothing on screen"),
            "{:?}",
            r.state.notice
        );
    }

    /// `gW` is the same gesture one unit up: a label per block, landing
    /// the trace cursor — which means entering trace mode, since a
    /// cursor nobody can see has not moved.
    #[test]
    fn gw_capital_tags_blocks_and_lands_the_trace_cursor() {
        let mut r = Rig::new();
        r.seeded(&["first thing said", "second thing said", "third thing said"]);
        r.state.transcript_rect = ratatui::layout::Rect::new(0, 0, 60, 12);
        r.press("g W");
        let spots = r
            .state
            .jump
            .as_ref()
            .expect("tags over the blocks")
            .spots
            .clone();
        assert_eq!(
            spots,
            vec![Spot::Block(0), Spot::Block(1), Spot::Block(2)],
            "one label per block, not per word"
        );
        r.press("b");
        assert!(
            r.state.tracing(),
            "landing a block tag is landing in trace mode"
        );
        assert_eq!(r.state.trace.at, 1);
    }

    #[test]
    fn a_tag_is_a_motion_so_select_mode_extends_to_it() {
        let mut r = Rig::new();
        r.write("delete the cargo file").press("esc");
        r.press("g g v");
        r.press("g w").press("c");
        let sel = r.state.prompt.selection();
        assert_eq!(sel, "delete the c", "from the head to the third word");
    }

    #[test]
    fn a_letter_no_tag_starts_with_closes_them_and_does_nothing_else() {
        let mut r = Rig::new();
        r.write("alpha beta").press("esc");
        r.press("g w");
        let before = r.state.prompt.range;
        r.press("z");
        assert!(r.state.jump.is_none());
        assert_eq!(r.state.prompt.range, before, "a miss moves nothing");
        assert_eq!(r.text(), "alpha beta");
    }

    #[test]
    fn star_searches_for_what_the_prompt_has_selected() {
        let mut r = Rig::new();
        r.seeded(&["the gamma ray", "beta"]);
        r.write("gamma").press("esc");
        r.press("% *");
        let s = r.state.search.as_ref().expect("a search");
        assert_eq!(s.pattern(), "gamma");
        assert!(
            r.state.mini.is_none(),
            "there is nothing left to type, so no line comes up"
        );
        assert_eq!(r.hit(), Some(0));
    }

    #[test]
    fn a_selection_is_searched_as_text_not_as_a_pattern() {
        let mut r = Rig::new();
        r.seeded(&["a.b literal", "axb regex"]);
        r.write("a.b").press("esc");
        r.press("% *");
        assert_eq!(r.hit(), Some(0), "`.` is a dot, not any character");
        // Two: the entry, and the draft the selection was taken from —
        // which is searched like anything else on the screen, with no
        // exception carved out for the pattern having come from it. The
        // regex one, `axb`, is not among them.
        assert_eq!(r.state.snapshot()["hits"], 2);
    }

    /// The search line leaves the message alone and searches it. Both
    /// halves are the same change: the pattern is a buffer of its own on
    /// the prompt's border, so the draft is neither hidden nor exempt.
    #[test]
    fn the_search_line_leaves_the_draft_alone_and_searches_it() {
        let mut r = Rig::new();
        r.seeded(&["the needle in the session"]);
        r.write("a needle in my draft").press("esc");
        r.press("/").typed("needle");
        assert_eq!(
            r.text(),
            "a needle in my draft",
            "the message is where it was"
        );
        assert_eq!(r.line(), "/needle");
        let s = r.state.search.as_ref().expect("a search");
        assert_eq!(s.hits.len(), 2, "the entry and the draft");
        assert_eq!(s.hits[1].entry, 1, "the draft sits after the last entry");
        // `n` walks onto it and the view follows the tail, since the
        // prompt is always drawn.
        r.press("ret").press("n");
        assert_eq!(r.hit(), Some(1));
        assert_eq!(r.state.scroll_up, 0);
        // And `esc` puts the search away without touching the message.
        r.press("esc");
        assert!(r.state.search.is_none());
        assert_eq!(r.text(), "a needle in my draft");
    }

    /// The `:` line, the pattern and the utterance are all whole
    /// buffers, so the readline keys work on them the way they work on
    /// the message — which is the point of there being one buffer type.
    #[test]
    fn the_readline_keys_reach_the_line_being_typed_and_not_the_message() {
        let mut r = Rig::new();
        r.write("the message").press("esc");
        r.press(":").typed("model fau:Ministral");
        r.press("C-w");
        assert_eq!(r.line(), ":model ", "C-w took the word, not the message's");
        r.press("C-u");
        assert_eq!(r.line(), ":");
        assert_eq!(r.text(), "the message", "and the message never moved");
        r.press("esc");
        // On the message itself, in insert.
        r.press("i").press("C-a");
        r.typed("so: ");
        assert_eq!(
            r.text(),
            "so: the message",
            "C-a is the start of the line here"
        );
        r.press("C-k");
        assert_eq!(r.text(), "so: ");
        r.press("C-y");
        assert_eq!(
            r.text(),
            "so: the message",
            "C-y puts back what the kill took"
        );
    }

    /// The summary line stands for what is hidden, so clicking it is
    /// how the hidden is asked for — and clicking again puts it back.
    #[test]
    fn a_click_on_a_folded_run_opens_it_and_a_second_folds_it_back() {
        let mut r = Rig::new();
        r.state.transcript = vec![
            Entry::User("go".into()),
            Entry::Tool { id: String::new(), name: "bash".into(), input: "{}".into(), output: Some(("out".into(), false)) },
            Entry::Tool { id: String::new(), name: "bash".into(), input: "{}".into(), output: Some(("out".into(), false)) },
        ];
        r.state.view_width = 60;
        r.state.view_entries = 0..3;
        r.state.transcript_rect = ratatui::layout::Rect::new(0, 0, 60, 8);
        let row = (0..8)
            .find(|y| crate::render::entry_at(&r.state, r.state.transcript_rect, *y) == Some(1))
            .expect("the folded run is on screen");
        assert!(crate::render::fold_at(&r.state, 1).is_some(), "not folded to begin with");
        toggle_fold_at(&mut r.state, (2, row));
        assert!(crate::render::opened_at(&r.state, 1).is_some(), "the click did not open it");
        toggle_fold_at(&mut r.state, (2, row));
        assert!(crate::render::fold_at(&r.state, 1).is_some(), "the click did not fold it back");
    }

    #[test]
    fn n_without_a_search_says_so_rather_than_doing_nothing() {
        let mut r = Rig::new();
        r.seeded(&["alpha"]);
        r.press("n");
        assert!(r.said().contains("no search yet"), "{:?}", r.state.notice);
    }

    /// A transcript whose only match is inside a settled tool call, so
    /// the fold draws a summary and the match is nowhere on screen.
    fn folded_hit() -> Rig {
        let mut r = Rig::new();
        r.state.transcript = vec![
            Entry::User("write three files".into()),
            Entry::Tool {
                id: String::new(),
                name: "write".into(),
                input: "{}".into(),
                output: Some(("random_char_3.txt: @".into(), false)),
            },
        ];
        r.state.view_width = 60;
        r.state.view_entries = 0..2;
        r
    }

    #[test]
    fn shift_tab_opens_the_fold_the_match_is_in_without_leaving_the_search() {
        let mut r = folded_hit();
        r.press("/").typed("@");
        assert_eq!(r.hit(), Some(1), "found, though nothing of it is drawn");
        assert!(
            crate::render::fold_at(&r.state, 1).is_some(),
            "and it is behind a fold"
        );
        r.press("S-tab");
        assert_eq!(r.state.opened, vec![1], "the run is open");
        assert!(r.state.searching(), "and the search line is still up");
        assert_eq!(r.hit(), Some(1), "on the same match");
        assert!(crate::render::fold_at(&r.state, 1).is_none());
    }

    #[test]
    fn opening_leaves_every_other_fold_alone() {
        let mut r = folded_hit();
        r.state.transcript.push(Entry::User("and again".into()));
        r.state.transcript.push(Entry::Tool {
            id: String::new(),
            name: "write".into(),
            input: "{}".into(),
            output: Some(("nothing here".into(), false)),
        });
        r.state.view_entries = 0..4;
        r.press("/").typed("@").press("S-tab");
        assert_eq!(r.state.opened, vec![1]);
        assert!(
            crate::render::fold_at(&r.state, 3).is_some(),
            "the other run is still folded"
        );
        assert_eq!(
            r.state.detail,
            crate::state::Detail::Folded,
            "and the global knob was not touched"
        );
    }

    #[test]
    fn a_match_that_is_not_behind_a_fold_uncaps_the_output_instead() {
        let mut r = Rig::new();
        let body = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        r.state.transcript = vec![Entry::Tool {
            id: String::new(),
            name: "read".into(),
            input: "{}".into(),
            output: Some((body, false)),
        }];
        r.state.view_width = 60;
        r.state.view_entries = 0..1;
        r.state.detail = crate::state::Detail::Calls;
        r.press("/").typed("line 39").press("S-tab");
        // Not folded, so there is no run to open — what was hiding it
        // was the cap, and that is the one knob for it.
        assert!(r.state.opened.is_empty());
        assert_eq!(r.state.detail, crate::state::Detail::Full);
    }

    #[test]
    fn reveal_says_so_when_there_is_nothing_left_to_open() {
        let mut r = Rig::new();
        r.seeded(&["the needle"]);
        r.state.detail = crate::state::Detail::Full;
        r.press("/").typed("needle").press("S-tab");
        assert!(
            r.said().contains("nothing left to open"),
            "{:?}",
            r.state.notice
        );
    }

    #[test]
    fn reveal_without_a_search_says_so_rather_than_opening_something_arbitrary() {
        let mut r = folded_hit();
        exec(&mut r.state, None, &r.tx, "reveal_match", 1, "");
        assert!(r.state.opened.is_empty());
        assert!(
            r.said().contains("no match to open"),
            "{:?}",
            r.state.notice
        );
    }

    #[test]
    fn two_matches_in_one_fold_are_one_tag_because_they_are_one_line() {
        let mut r = Rig::new();
        r.state.transcript = vec![
            Entry::Tool {
                id: String::new(),
                name: "write".into(),
                input: "{}".into(),
                output: Some(("a @ here".into(), false)),
            },
            Entry::Tool {
                id: String::new(),
                name: "write".into(),
                input: "{}".into(),
                output: Some(("another @ there".into(), false)),
            },
        ];
        r.state.view_width = 60;
        r.state.view_entries = 0..2;
        r.press("/").typed("@");
        assert_eq!(r.state.search.as_ref().unwrap().hits.len(), 2, "two hits");
        r.press("tab");
        let j = r.state.jump.as_ref().expect("tags");
        assert_eq!(j.labels, ["a"], "but one fold, so one letter");
    }

    /// The reserved slash is about *submitted text*, not about the key:
    /// a message is typed in insert, where `/` is a character.
    #[test]
    fn a_slash_typed_into_a_message_is_still_text_and_still_sent() {
        let mut r = Rig::new();
        r.write("/not a command").press("ret");
        assert!(
            r.state.search.is_none(),
            "insert mode never opened a search"
        );
        assert_eq!(r.cmds(), vec![r#"Send("/not a command")"#]);
    }

    #[test]
    fn bang_opens_dispatch_mode_and_esc_comes_back() {
        let mut r = Rig::new();
        r.write("a message in progress").press("esc");
        r.press("!");
        assert_eq!(r.state.mode_name(), "dispatch");
        // It is a text mode: keys are characters, not commands. And the
        // utterance is a line of its own — the message underneath it is
        // untouched, where it used to be the same buffer and so was
        // gone.
        r.typed("delete the note");
        assert_eq!(r.line(), "!delete the note");
        assert_eq!(r.text(), "a message in progress");
        r.press("esc");
        assert_eq!(r.state.mode_name(), "normal", "esc takes the line down");
        assert_eq!(r.line(), "");
        assert_eq!(
            r.text(),
            "a message in progress",
            "and the message is still there"
        );
    }

    #[test]
    fn a_submitted_line_goes_to_the_surface_rather_than_to_the_model() {
        let mut r = Rig::new();
        r.press("!").typed("read the note Groceries").press("ret");
        assert_eq!(
            r.cmds(),
            vec![r#"Dispatch { surface: "mneme", text: "read the note Groceries" }"#]
        );
        // The utterance lands in the transcript straight away — a cold
        // classifier takes long enough that a `ret` drawing nothing would
        // read as a dropped keystroke.
        assert!(
            matches!(r.state.transcript.last(), Some(Entry::User(t)) if t == "read the note Groceries")
        );
        // And nothing was sent to the model.
        assert!(!r.state.working);
    }

    /// Sending is no reason to leave insert, and it is no reason to
    /// close the dispatch line either: a run of utterances costs one
    /// `!`. The `:` line is the other way about — a command is done with
    /// when it runs — which is what it always did.
    #[test]
    fn the_dispatch_line_stays_up_across_a_submit_the_way_insert_does() {
        let mut r = Rig::new();
        r.press("!").typed("one").press("ret");
        assert_eq!(
            r.state.mode_name(),
            "dispatch",
            "a run of commands costs one `!`"
        );
        assert_eq!(r.line(), "!", "emptied, though");
        r.typed("two").press("ret");
        assert_eq!(r.cmds().len(), 2);
        // And the utterances are on the line's own history, not in the
        // messages you have sent.
        r.press("up");
        assert_eq!(r.line(), "!two");
        r.press("up");
        assert_eq!(r.line(), "!one");
        r.press("esc").press("i");
        assert!(
            r.state.prompt.history.is_empty(),
            "no utterance is a message"
        );
    }

    #[test]
    fn a_colon_in_dispatch_mode_is_a_character_and_not_a_command_line() {
        let mut r = Rig::new();
        r.press("!").typed(":tree");
        // No command line, so no completion popup and no `tab` cycling.
        assert_eq!(r.state.command_line(), None);
        assert_eq!(r.state.menu, None);
        r.press("ret");
        assert_eq!(
            r.cmds(),
            vec![r#"Dispatch { surface: "mneme", text: ":tree" }"#]
        );
    }

    #[test]
    fn tab_changes_surface_instead_of_completing() {
        let mut r = Rig::new();
        r.state.dispatch = crate::dispatch::Dispatch::new(vec!["mneme".into(), "tools".into()]);
        r.press("!").press("tab");
        assert_eq!(r.state.dispatch.surface(), Some("tools"));
        assert_eq!(
            r.state.mode_name(),
            "dispatch",
            "changing surface is not leaving the mode"
        );
        r.typed("x").press("ret");
        assert_eq!(
            r.cmds(),
            vec![r#"Dispatch { surface: "tools", text: "x" }"#]
        );
    }

    #[test]
    fn with_no_bundles_the_mode_says_so_rather_than_opening_onto_nothing() {
        let mut r = Rig::new();
        r.state.dispatch = crate::dispatch::Dispatch::default();
        r.press("!");
        assert_eq!(r.state.prompt.mode, Mode::normal());
        assert!(r.said().contains("verba.bundles"), "{:?}", r.state.notice);
    }

    #[test]
    fn the_leader_reaches_the_same_mode_the_bang_does() {
        let mut r = Rig::new();
        r.press("space d d");
        assert_eq!(r.state.mode_name(), "dispatch");
    }

    #[test]
    fn the_typed_form_is_a_driver_command_and_carries_its_utterance() {
        let mut r = Rig::new();
        r.press("esc :")
            .typed("dispatch read the note Groceries")
            .press("ret");
        assert_eq!(
            r.cmds(),
            vec![r#"Command { name: "dispatch", arg: "read the note Groceries" }"#]
        );
    }

    /// The same pairing bug, on the live path: three parallel calls stream
    /// in, three results come back over the bus. `ToolCallFinished` used to
    /// fill "the last entry still empty", which is the newest call, not the
    /// one that finished.
    #[test]
    fn a_finished_call_fills_its_own_entry_and_not_the_newest() {
        use eidolon_core::event::Event;
        use eidolon_core::tool::{CallOrigin, ToolCall, ToolOutput};
        let mut r = Rig::new();
        for id in ["a", "b", "c"] {
            apply(
                &mut r.state,
                UiMsg::Bus(Event::ToolUseStart {
                    id: id.into(),
                    name: "write".into(),
                }),
            );
        }
        for (id, out) in [("a", "one"), ("b", "two"), ("c", "three")] {
            let call = ToolCall {
                id: id.into(),
                name: "write".into(),
                input: serde_json::json!({}),
                origin: CallOrigin::Model,
            };
            apply(
                &mut r.state,
                UiMsg::Bus(Event::ToolCallFinished {
                    record: None,
                    call,
                    output: ToolOutput {
                        content: out.into(),
                        is_error: false,
                        ends_turn: false,
                    },
                }),
            );
        }
        let paired: Vec<(&str, &str)> = r
            .state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool {
                    id,
                    output: Some((c, _)),
                    ..
                } => Some((id.as_str(), c.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(paired, [("a", "one"), ("b", "two"), ("c", "three")]);
    }

    /// A dispatch has no stream behind it, so `ToolCallStarted` is where its
    /// entry appears — and it is an ordinary `Entry::Tool`, which is what
    /// makes it fold with the rest of the tool traffic.
    #[test]
    fn a_dispatch_draws_as_ordinary_tool_traffic() {
        use eidolon_core::event::Event;
        use eidolon_core::tool::{CallOrigin, ToolCall, ToolOutput};
        let mut r = Rig::new();
        let call = ToolCall {
            id: "vv_1".into(),
            name: "mneme_rpc".into(),
            input: serde_json::json!({ "function": "read_note" }),
            origin: CallOrigin::User,
        };
        apply(
            &mut r.state,
            UiMsg::Bus(Event::ToolCallStarted(call.clone())),
        );
        apply(
            &mut r.state,
            UiMsg::Bus(Event::ToolCallFinished {
                record: None,
                call,
                output: ToolOutput {
                    content: "eggs".into(),
                    is_error: false,
                    ends_turn: false,
                },
            }),
        );
        assert!(
            matches!(r.state.transcript.last(), Some(Entry::Tool { name, output: Some((c, false)), .. })
            if name == "mneme_rpc" && c == "eggs")
        );
        // Settled, and nothing is in flight, so the fold owns it.
        assert!(!r.state.working);
        assert_eq!(
            crate::render::fold_at(&r.state, r.state.transcript.len() - 1),
            Some(r.state.transcript.len() - 1)
        );
    }

    // ------------------------------------------------------------ trace

    /// A turn's worth of transcript, the shape the fold is for: a
    /// question, two rounds of thinking and calling with no prose
    /// between them, and the answer.
    fn traced() -> Rig {
        let mut r = Rig::new();
        r.state.transcript = vec![
            Entry::User("what is here?".into()),
            Entry::Thinking {
                text: "look at the tree first".into(),
                streaming: false,
            },
            Entry::Tool {
                id: "a".into(),
                name: "shell".into(),
                input: r#"{"command":"ls"}"#.into(),
                output: Some(("Cargo.toml".into(), false)),
            },
            Entry::Thinking {
                text: "now read the manifest".into(),
                streaming: false,
            },
            Entry::Tool {
                id: "b".into(),
                name: "read".into(),
                input: r#"{"path":"Cargo.toml"}"#.into(),
                output: Some(("[package]".into(), false)),
            },
            Entry::Assistant {
                text: "A Rust crate.".into(),
                streaming: false,
            },
        ];
        r.state.view_width = 60;
        r.state.view_entries = 0..6;
        r
    }

    /// `K` used to step a global knob; it puts a cursor on the
    /// transcript now, and the same key takes it away again.
    #[test]
    fn the_hover_key_enters_trace_mode_and_leaves_it() {
        let mut r = traced();
        r.press("K");
        assert!(r.state.tracing());
        assert!(
            r.state.trace.live,
            "the cursor is placed by the keystroke, not by the next frame"
        );
        r.press("K");
        assert!(!r.state.tracing());
        r.press("K esc");
        assert!(!r.state.tracing(), "esc leaves it too");
        assert_eq!(
            r.state.detail,
            crate::state::Detail::Folded,
            "and the detail knob was never touched"
        );
    }

    /// `:trace read` from anywhere: enters the mode, narrows the walk,
    /// lands on the first block that carries the word. `:trace` alone
    /// puts the whole transcript back under the cursor.
    #[test]
    fn trace_with_a_word_sets_the_filter_and_lands_on_a_match() {
        let mut r = traced();
        assert!(!r.state.tracing());
        exec(&mut r.state, Some(&r.script), &r.tx, "trace", 1, "read");
        assert!(r.state.tracing(), "the command enters the mode");
        assert_eq!(r.state.trace_filter.as_deref(), Some("read"));
        assert_eq!(
            r.state.trace.at, 1,
            "folded, the cluster is the first block carrying a read"
        );
        exec(&mut r.state, Some(&r.script), &r.tx, "trace", 1, "");
        assert_eq!(r.state.trace_filter, None, "no argument clears it");
        assert!(r.state.tracing(), "clearing the filter does not leave the mode");
    }

    /// The whole point: `j` steps over a cluster in one press, `l` goes
    /// into it, `j` then walks its parts, and `h` comes back out.
    #[test]
    fn the_cursor_walks_clusters_and_steps_into_them() {
        let mut r = traced();
        r.press("K g g");
        assert_eq!(r.state.trace.at, 0);
        r.press("j");
        assert_eq!(
            r.state.trace.at, 1,
            "the four entries of the cluster in one press"
        );
        r.press("j");
        assert_eq!(r.state.trace.at, 5, "and out the other side onto the reply");
        r.press("k l");
        assert_eq!(r.state.trace.at, 1, "inside the cluster now");
        r.press("j j");
        assert_eq!(r.state.trace.at, 3, "its parts, one at a time");
        r.press("h");
        assert_eq!(r.state.trace.at, 1);
        assert!(r.state.opened.is_empty(), "and the fold is shut again");
    }

    /// `v` and `y`, as in the prompt — and the yank fills the register
    /// as well as the clipboard, so `p` pastes it into a message.
    #[test]
    fn the_transcript_has_a_selection_and_it_yanks() {
        let mut r = traced();
        r.press("K g g v j y");
        assert!(r.state.prompt.register.contains("what is here?"));
        assert!(
            r.state.prompt.register.contains("look at the tree first"),
            "the thinking is text now, not a count"
        );
        assert!(r.state.prompt.register.contains("[package]"));
        assert!(!r.state.prompt.register.contains("A Rust crate"));
        assert!(
            infos(&r).iter().any(|i| i.contains("yanked 5 blocks")),
            "{:?}",
            infos(&r)
        );
    }

    /// The search moves the cursor onto the block that holds the match
    /// — and leaves the fold shut, because the line standing in for it
    /// already says how many matches are inside.
    #[test]
    fn a_search_hands_the_cursor_the_block_the_match_is_in() {
        let mut r = traced();
        r.press("K g g");
        r.press("/").typed("package").press("ret");
        assert_eq!(
            r.state.trace.at, 1,
            "the match is in entry 4, drawn as the cluster at 1"
        );
        assert!(r.state.opened.is_empty(), "n does not unfold on its own");
        assert!(!r.state.searching(), "the line came down");
        assert!(r.state.tracing(), "and the mode survived it");
        // …and now one key opens what the search found.
        r.press("l");
        assert_eq!(r.state.opened, vec![1]);
    }

    /// Entering the mode with a search already live lands on the match
    /// rather than at the top of the view.
    #[test]
    fn the_cursor_is_placed_on_the_live_search_when_the_mode_opens() {
        let mut r = traced();
        r.press("/").typed("Cargo").press("ret");
        assert!(!r.state.tracing());
        r.press("K");
        assert_eq!(r.state.trace.at, 1);
    }

    /// `*` is the prompt's selection everywhere else and the
    /// transcript's here, because the prompt is not what the hands are
    /// pointed at.
    #[test]
    fn star_searches_for_what_the_transcript_has_selected() {
        let mut r = traced();
        r.press("K g g j l j").press("*");
        let pattern = r
            .state
            .search
            .as_ref()
            .expect("a search")
            .pattern()
            .to_string();
        assert!(
            pattern.contains("Cargo\\.toml"),
            "escaped, because a selection is text: {pattern}"
        );
    }

    /// A trace command with no cursor behind it names the key that
    /// makes one, rather than moving something invisible.
    #[test]
    fn a_trace_command_outside_the_mode_says_which_key_opens_it() {
        let mut r = traced();
        exec(&mut r.state, Some(&r.script), &r.tx, "trace_next", 1, "");
        assert!(
            infos(&r)
                .last()
                .is_some_and(|i| i.contains("`K` puts a cursor")),
            "{:?}",
            infos(&r)
        );
        // Folding everything is the pair that needs no cursor.
        exec(
            &mut r.state,
            Some(&r.script),
            &r.tx,
            "trace_open_all",
            1,
            "",
        );
        assert_eq!(r.state.opened, vec![1]);
        exec(
            &mut r.state,
            Some(&r.script),
            &r.tx,
            "trace_close_all",
            1,
            "",
        );
        assert!(r.state.opened.is_empty());
    }

    /// A count multiplies a block motion, as it does every other one.
    #[test]
    fn a_count_moves_the_cursor_that_many_blocks() {
        let mut r = traced();
        r.press("K g g");
        r.press("2 j");
        assert_eq!(r.state.trace.at, 5, "over the cluster and onto the reply");
    }

    /// Leaving a mode that was never typing must not move the prompt's
    /// own cursor. `esc` used to walk it one character left each time,
    /// because the caret-to-block shift belongs to leaving *insert*.
    #[test]
    fn a_trip_through_trace_mode_leaves_the_draft_exactly_as_it_was() {
        let mut r = traced();
        r.write("hello there").press("esc");
        let (text, cursor) = (
            r.state.prompt.text().to_string(),
            r.state.prompt.range.cursor(),
        );
        r.press("K j K");
        assert_eq!(r.state.prompt.text(), text);
        assert_eq!(r.state.prompt.range.cursor(), cursor);
        assert!(r.state.prompt.mode.is("normal"));
    }

    /// Thinking survives the reply arriving, and survives being resumed
    /// — it used to do neither.
    #[test]
    fn thinking_is_an_entry_that_outlives_the_reply_it_preceded() {
        use eidolon_core::event::Event;
        use eidolon_core::message::{ContentBlock, Message};
        let mut r = Rig::new();
        apply(&mut r.state, UiMsg::Bus(Event::MessageStart));
        apply(
            &mut r.state,
            UiMsg::Bus(Event::ThinkingDelta("\u{b7}".into())),
        );
        apply(
            &mut r.state,
            UiMsg::Bus(Event::ThinkingDelta("\u{b7}".into())),
        );
        apply(
            &mut r.state,
            UiMsg::Bus(Event::TextDelta("the answer".into())),
        );
        let m = Message::assistant(vec![
            ContentBlock::Thinking {
                thinking: "the real reasoning".into(),
                signature: "s".into(),
            },
            ContentBlock::text("the answer"),
        ]);
        apply(
            &mut r.state,
            UiMsg::Bus(Event::AssistantMessage {
                record: 1,
                message: m,
            }),
        );
        // Two entries, in the order they happened, and the settled
        // message's thinking replaced the stream's placeholder dots.
        assert!(
            matches!(&r.state.transcript[0], Entry::Thinking { text, streaming: false } if text == "the real reasoning")
        );
        assert!(
            matches!(&r.state.transcript[1], Entry::Assistant { text, streaming: false } if text == "the answer")
        );
        assert_eq!(r.state.transcript.len(), 2);
    }

    /// The harness's own nested call is not drawn — in either view.
    ///
    /// A script-originated call has no record on the branch (only an
    /// operator's dispatch writes a call record), so no resume can draw it;
    /// drawing it live would be a row that vanishes. Its *result* is not
    /// drawn either, and must not be paired by order onto the call that is
    /// still open — the call that made it — which would leave that call
    /// showing the wrong output with its own result dropped.
    #[test]
    fn a_nested_call_is_not_drawn_and_does_not_land_on_the_call_that_made_it() {
        use eidolon_core::event::Event;
        use eidolon_core::tool::{ToolCall, ToolOutput};
        let mut r = Rig::new();
        let outer = ToolCall {
            id: "vv_1".into(),
            name: "vv".into(),
            input: serde_json::json!({ "line": "read the note X" }),
            origin: CallOrigin::User,
        };
        apply(
            &mut r.state,
            UiMsg::Bus(Event::ToolCallStarted(outer.clone())),
        );
        let nested = ToolCall {
            id: "vv_mneme_7".into(),
            name: "mneme_rpc".into(),
            input: serde_json::json!({ "function": "read_note" }),
            origin: CallOrigin::Script,
        };
        apply(
            &mut r.state,
            UiMsg::Bus(Event::ToolCallStarted(nested.clone())),
        );
        apply(
            &mut r.state,
            UiMsg::Bus(Event::ToolCallFinished {
                record: Some(2),
                call: nested,
                output: ToolOutput {
                    content: "the nested rpc's answer".into(),
                    is_error: false,
                    ends_turn: false,
                },
            }),
        );
        assert_eq!(r.state.transcript.len(), 1, "{:?}", r.state.transcript);
        assert_eq!(
            r.state.tools_run, 0,
            "the nested result counted nothing: only a drawn call counts"
        );
        assert_eq!(r.state.last_tool.as_deref(), Some("vv"));
        assert!(
            !r.state.ledger.tools.iter().any(|(n, _)| n == "mneme_rpc"),
            "and the ledger counts what the transcript drew"
        );
        apply(
            &mut r.state,
            UiMsg::Bus(Event::ToolCallFinished {
                record: Some(3),
                call: outer,
                output: ToolOutput {
                    content: "what the model was told".into(),
                    is_error: false,
                    ends_turn: false,
                },
            }),
        );
        let Entry::Tool {
            id,
            output: Some((out, _)),
            ..
        } = &r.state.transcript[0]
        else {
            panic!("{:?}", r.state.transcript[0])
        };
        assert_eq!(id, "vv_1");
        assert_eq!(
            out, "what the model was told",
            "its own result, not the nested call's"
        );
        assert_eq!(r.state.tools_run, 1, "the call that was drawn counted once");
    }

    /// The inline channel's answers find the reply that asked them by
    /// *shape* on the live path too — the last prose on the transcript is
    /// the one the answers belong to — and the marks come out of it and are
    /// drawn as the call they were, which is what a resume draws from the
    /// same record.
    #[test]
    fn the_live_answers_are_drawn_the_way_a_resume_draws_them() {
        use eidolon_core::event::Event;
        use eidolon_core::message::{ContentBlock, Message};
        let mut r = Rig::new();
        let reply = "Let me look.\n\n! list notes\n";
        apply(&mut r.state, UiMsg::Bus(Event::MessageStart));
        apply(&mut r.state, UiMsg::Bus(Event::TextDelta(reply.into())));
        apply(
            &mut r.state,
            UiMsg::Bus(Event::AssistantMessage {
                record: 1,
                message: Message::assistant(vec![ContentBlock::text(reply)]),
            }),
        );
        apply(
            &mut r.state,
            UiMsg::Bus(Event::CommandResults {
                record: 2,
                lines: vec!["[vv] list_notes → ok: 42 notes".into()],
            }),
        );
        assert_eq!(r.state.transcript.len(), 2, "{:?}", r.state.transcript);
        let Entry::Assistant { text, .. } = &r.state.transcript[0] else {
            panic!("{:?}", r.state.transcript[0])
        };
        assert_eq!(text, "Let me look.\n\n");
        let Entry::Ask { commands, answers } = &r.state.transcript[1] else {
            panic!("{:?}", r.state.transcript[1])
        };
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command, "list notes");
        assert_eq!(answers, &["[vv] list_notes → ok: 42 notes"]);
        assert_eq!(r.state.record_at(1), Some(2));
    }

    /// A bare marker is not a call and never becomes one: nothing ran, so
    /// there is no record to take it out and the frame would keep a lone `!`
    /// the log draws no longer. It leaves at `AssistantMessage` instead — the
    /// same text `seed` builds from that record — which is what makes the live
    /// frame and a resume agree about it.
    #[test]
    fn the_live_reply_drops_a_bare_marker_the_way_a_resume_does() {
        use eidolon_core::event::Event;
        use eidolon_core::message::{ContentBlock, Message};
        let mut r = Rig::new();
        let reply = "! \n\nThe channel answered, but only as a preview.";
        apply(&mut r.state, UiMsg::Bus(Event::MessageStart));
        apply(&mut r.state, UiMsg::Bus(Event::TextDelta(reply.into())));
        apply(
            &mut r.state,
            UiMsg::Bus(Event::AssistantMessage {
                record: 1,
                message: Message::assistant(vec![ContentBlock::text(reply)]),
            }),
        );
        let Entry::Assistant { text, streaming } = &r.state.transcript[0] else {
            panic!("{:?}", r.state.transcript[0])
        };
        assert!(!streaming);
        assert_eq!(
            text, "\nThe channel answered, but only as a preview.",
            "the marker is syntax, not something the model said"
        );
        // And a reply whose whole text was the marker leaves nothing to draw:
        // the entry settles empty and `finish_streaming` drops it, which is
        // what `seed` does with an empty text too — so a resume draws the same
        // transcript rather than a zero-line block.
        let mut r = Rig::new();
        apply(&mut r.state, UiMsg::Bus(Event::MessageStart));
        apply(&mut r.state, UiMsg::Bus(Event::TextDelta("! \n".into())));
        apply(
            &mut r.state,
            UiMsg::Bus(Event::AssistantMessage {
                record: 1,
                message: Message::assistant(vec![ContentBlock::text("! \n")]),
            }),
        );
        assert!(
            r.state.transcript.is_empty(),
            "a marker on its own asks nothing and says nothing: {:?}",
            r.state.transcript
        );
    }

    /// The hedge — a reply that marks a line *and* calls a tool — is
    /// answered only after the results it is owed, so the ask lands under
    /// them, live and resumed alike, and nothing already on the transcript
    /// moves to make room for it: the tool line keeps its own record.
    #[test]
    fn a_hedged_replys_ask_lands_after_the_results_it_waited_for() {
        use eidolon_core::event::Event;
        use eidolon_core::message::{ContentBlock, Json, Message};
        use eidolon_core::tool::{CallOrigin, ToolCall, ToolOutput};
        let mut r = Rig::new();
        let reply = "Checking.\n\n! read the note X\n";
        apply(&mut r.state, UiMsg::Bus(Event::MessageStart));
        apply(&mut r.state, UiMsg::Bus(Event::TextDelta(reply.into())));
        apply(
            &mut r.state,
            UiMsg::Bus(Event::ToolUseStart {
                id: "t1".into(),
                name: "read".into(),
            }),
        );
        apply(
            &mut r.state,
            UiMsg::Bus(Event::AssistantMessage {
                record: 1,
                message: Message::assistant(vec![
                    ContentBlock::text(reply),
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "read".into(),
                        input: Json(r#"{"path":"X"}"#.into()),
                    },
                ]),
            }),
        );
        let call = ToolCall {
            id: "t1".into(),
            name: "read".into(),
            input: serde_json::json!({ "path": "X" }),
            origin: CallOrigin::Model,
        };
        apply(
            &mut r.state,
            UiMsg::Bus(Event::ToolCallFinished {
                record: Some(3),
                call,
                output: ToolOutput {
                    content: "the note".into(),
                    is_error: false,
                    ends_turn: false,
                },
            }),
        );
        apply(
            &mut r.state,
            UiMsg::Bus(Event::CommandResults {
                record: 4,
                lines: vec!["[vv] read_note title=\"X\" → ok: the note".into()],
            }),
        );
        let kinds: Vec<&str> = r
            .state
            .transcript
            .iter()
            .map(|e| e.kind())
            .collect();
        assert_eq!(kinds, ["reply", "call", "ask"], "{:?}", r.state.transcript);
        assert_eq!(
            r.state.record_at(1),
            Some(3),
            "the tool line still belongs to its result"
        );
        assert_eq!(r.state.record_at(2), Some(4));
    }
    // ----------------------------------------------------------- launch
    //
    // Nothing here spawns anything. The two halves that could — finding
    // a terminal and running it — are `launch.rs`'s, and are tested there
    // on their pure parts; what belongs here is the wiring: that the
    // chord is where the other two tools put it, and that "here" means
    // the cursor when there is one.

    /// The chord helix and yazi share, pinned.
    ///
    /// `T T` and `T R` are the entire reason `T` is a prefix rather than
    /// Helix's `till_prev_char`, so a rename of either command — or a
    /// tidy-minded move of the group onto the leader — should fail here,
    /// rather than under a hand that has already pressed two keys.
    #[test]
    fn the_window_chord_is_where_helix_and_yazi_put_it() {
        let km = UiScript::compile(crate::DEFAULT_UI)
            .unwrap()
            .keymap()
            .unwrap();
        for mode in ["normal", "trace", "select"] {
            let t = km[mode]
                .get("T")
                .unwrap_or_else(|| panic!("no `T` in {mode}"));
            let keys = t
                .get("keys")
                .unwrap_or_else(|| panic!("`T` in {mode} is not a group"));
            for (key, command) in [
                ("T", "terminal"),
                ("R", "files"),
                ("H", "editor"),
                ("E", "harness"),
            ] {
                assert_eq!(keys[key]["cmd"], command, "T {key} in {mode}");
            }
        }
        // And the motion it displaced went where the operator's own Helix
        // config sent it, rather than somewhere this harness invented.
        assert_eq!(km["normal"]["C-t"]["cmd"], "till_prev_char");
        // The control bindings, and only these: `C-t` is the displaced
        // till, and `C-a`/`C-x` are Helix's increment and decrement — a
        // chord each, all three, with the rest of the vocabulary still on
        // the leader.
        let mut controls: Vec<&String> = km["normal"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with("C-"))
            .collect();
        controls.sort();
        assert_eq!(
            controls,
            ["C-a", "C-t", "C-x"],
            "a fourth control binding would be a second vocabulary"
        );
    }

    /// The Helix additions, pressed rather than called: digits reach the
    /// goto as a count, the object chords ride the keymap's held-open
    /// pair, and `x d` walks the draft a line at a time without leaving
    /// breaks behind.
    #[test]
    fn a_count_reaches_the_goto_through_the_digits() {
        let mut r = Rig::new();
        r.write("one\ntwo\nthree").press("esc").press("g g");
        assert_eq!(r.state.prompt.range.cursor(), 0);
        r.press("2 G");
        assert_eq!(r.state.prompt.range.cursor(), 4, "2G is the second line");
        r.press("G");
        assert_eq!(
            r.state.prompt.range.cursor(),
            8,
            "G with no digits is the last line"
        );
    }

    #[test]
    fn the_object_chords_select_surround_and_unsurround() {
        let mut r = Rig::new();
        r.write("say hello").press("esc").press("b");
        assert_eq!(r.state.prompt.range.cursor(), 4, "on the `h`");
        r.press("m i w");
        assert_eq!(r.state.prompt.selection(), "hello");
        r.press("m s \"");
        assert_eq!(r.state.prompt.text(), "say \"hello\"");
        r.press("m d \"");
        assert_eq!(r.state.prompt.text(), "say hello", "and the pair comes off");
    }

    #[test]
    fn x_then_d_chains_through_the_lines_of_the_draft() {
        let mut r = Rig::new();
        r.write("one\ntwo\nthree").press("esc").press("g g");
        r.press("x d x d x d");
        assert_eq!(
            r.state.prompt.text(),
            "",
            "three lines, three xds, no blank line left behind"
        );
    }

    /// `edit_prompt` asks when the prompt is the field being typed into,
    /// and says so when it is not — a minibuffer on the keyboard means
    /// the draft is not what is on screen, and an edit the operator
    /// cannot see come back is not an edit worth making.
    #[test]
    fn edit_prompt_asks_for_the_prompt_and_declines_for_a_minibuffer() {
        let mut r = Rig::new();
        r.press("i").typed("hello");
        exec(&mut r.state, Some(&r.script), &r.tx, "edit_prompt", 1, "");
        assert!(
            r.state.edit_draft,
            "the prompt is the field, so the loop is asked"
        );
        // The `:` line owns the keyboard: the command says so and asks
        // for nothing, and the flag is what the loop would act on.
        let mut r = Rig::new();
        r.press(":");
        exec(&mut r.state, Some(&r.script), &r.tx, "edit_prompt", 1, "");
        assert!(!r.state.edit_draft, "a minibuffer is not the prompt");
        assert!(r.said().contains("not on screen"), "{:?}", r.state.notice);
    }

    /// The readline keys, pinned. They live wherever text is typed, and
    /// the point of the list is that a tidy-minded reorganization of
    /// `readline` should fail here, rather than under hands that already
    /// know where `C-a` and `C-left` are.
    #[test]
    fn the_readline_keys_are_where_the_hands_think_they_are() {
        let km = UiScript::compile(crate::DEFAULT_UI)
            .unwrap()
            .keymap()
            .unwrap();
        let insert = &km["insert"];
        for (key, command) in [
            ("C-a", "goto_line_start"),
            ("C-e", "goto_line_end_newline"),
            ("C-b", "move_char_left"),
            ("C-f", "move_char_right"),
            ("A-b", "move_prev_word_start"),
            ("A-f", "move_next_word_end"),
            ("C-left", "move_prev_word_start"),
            ("C-right", "move_next_word_end"),
            ("C-home", "goto_buffer_start"),
            ("C-end", "goto_buffer_end"),
            ("C-h", "delete_char_backward"),
            ("C-d", "delete_char_forward"),
            ("C-u", "delete_to_line_start"),
            ("C-k", "delete_to_line_end"),
            ("C-w", "delete_word_backward"),
            ("A-d", "delete_word_forward"),
            ("C-backspace", "delete_word_backward"),
            ("C-del", "delete_word_forward"),
            ("A-backspace", "delete_word_backward"),
            ("C-y", "paste_before"),
            ("C-/", "undo"),
            ("C-_", "undo"),
            ("C-t", "transpose_chars"),
        ] {
            assert_eq!(insert[key]["cmd"], command, "{key}");
        }
        // The editor chord: readline's `C-x`, one key deep, and bound
        // only where it answers — the minibuffers type like insert and
        // take nothing else from its table.
        assert_eq!(
            insert["C-x"]["keys"]["e"]["cmd"], "edit_prompt",
            "C-x e edits the draft"
        );
        for mini in ["command", "dispatch", "search"] {
            assert!(
                km[mini].get("C-x").is_none(),
                "{mini} kept the editor chord"
            );
        }
    }

    /// *Here* is the cursor when there is a cursor.
    ///
    /// The cluster is the case that matters: it holds a shell call that
    /// names no file and a read that names one, and the answer is the
    /// file — `T H` on a fold means "open what this did", and a run that
    /// touched one file has one answer however many calls it took.
    #[test]
    fn the_window_aims_at_the_file_under_the_trace_cursor() {
        let mut r = traced();
        // No cursor at all: `T H` is the session's directory, which is
        // what it is in helix and yazi.
        assert_eq!(
            cursor_path(&r.state),
            None,
            "there is no cursor outside trace mode"
        );
        r.press("K g g j");
        assert_eq!(r.state.trace.at, 1, "on the cluster");
        assert_eq!(
            cursor_path(&r.state),
            Some(std::path::PathBuf::from("Cargo.toml"))
        );
        // Stepped into it and standing on the shell call, which named no
        // file: the honest answer is nothing, and the window falls back
        // to the directory rather than guessing at `ls`.
        r.press("l j");
        assert_eq!(r.state.trace.at, 2);
        assert_eq!(cursor_path(&r.state), None);
        // On the reply, likewise — a block with no call in it.
        r.press("h j");
        assert_eq!(r.state.trace.at, 5);
        assert_eq!(cursor_path(&r.state), None);
    }

    /// A program this machine does not have is a sentence, not a bare
    /// terminal: `T H` opening something indistinguishable from `T T`
    /// teaches the operator nothing about why.
    #[test]
    fn a_program_that_is_not_there_says_where_to_declare_it() {
        let mut r = Rig::new();
        assert_eq!(
            named(&mut r.state, Vec::new(), "no editor found; set `$EDITOR`"),
            None
        );
        assert!(
            r.said().contains("$EDITOR")
                && r.state
                    .notice
                    .as_ref()
                    .is_some_and(|n| n.level == Level::Alert),
            "{:?}",
            r.state.notice
        );
        let before = r.state.transcript.len();
        assert_eq!(
            named(&mut r.state, vec!["hx".into()], "unused"),
            Some(vec!["hx".into()])
        );
        assert_eq!(
            r.state.transcript.len(),
            before,
            "a program that is there says nothing"
        );
    }

    /// `:launch` with nothing to launch asks, rather than opening a
    /// window onto the shell it would have opened anyway.
    #[test]
    fn launch_with_no_command_asks_for_one() {
        let mut r = Rig::new();
        exec(&mut r.state, None, &r.tx, "launch", 1, "   ");
        assert!(r.said().contains("`:launch CMD`"), "{:?}", r.state.notice);
    }

    // ------------------------------------------------------------ images

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn shot(name: &str) -> crate::image::Attachment {
        crate::image::Attachment::from_bytes(name.into(), png(20, 10)).unwrap()
    }

    /// The whole path, once: attach, type, send. The attachment leaves the
    /// prompt, appears in the transcript ahead of the words, and reaches
    /// the driver as a content block.
    #[test]
    fn an_attached_image_goes_with_the_next_message_and_then_is_gone() {
        let mut r = Rig::new();
        r.state.attachments.push(shot("layout.png"));
        r.write("what is wrong here?").press("ret");

        assert!(
            r.state.attachments.is_empty(),
            "the attachment outlived the message it went with"
        );
        assert!(matches!(&r.state.transcript[0], Entry::Image(a) if a.name == "layout.png"));
        assert!(matches!(&r.state.transcript[1], Entry::User(t) if t == "what is wrong here?"));
        assert_eq!(r.cmds(), vec![r#"Send("what is wrong here?" +1 image)"#]);
    }

    /// Images lead the content blocks. Both wires read a message in order
    /// and both answer better when the question follows the picture — and
    /// it is the order the transcript draws, so what the model is sent and
    /// what the operator sees are one sequence.
    #[test]
    fn the_picture_comes_before_the_words() {
        let said = Utterance {
            text: "what is this?".into(),
            images: vec![shot("a.png").block()],
        };
        let blocks = said.blocks();
        assert!(matches!(blocks[0], ContentBlock::Image { .. }));
        assert!(matches!(&blocks[1], ContentBlock::Text { text } if text == "what is this?"));
    }

    /// Derived, this printed a megabyte of base64 per image — into log
    /// lines, test failures and panic messages alike.
    #[test]
    fn an_utterance_debugs_as_its_words_and_a_count() {
        assert_eq!(format!("{:?}", Utterance::text("hi")), r#""hi""#);
        let one = Utterance {
            text: "hi".into(),
            images: vec![shot("a.png").block()],
        };
        assert_eq!(format!("{one:?}"), r#""hi" +1 image"#);
        let two = Utterance {
            text: "hi".into(),
            images: vec![shot("a.png").block(), shot("b.png").block()],
        };
        assert_eq!(format!("{two:?}"), r#""hi" +2 images"#);
        assert!(!format!("{one:?}").contains("iVBOR"));
    }

    /// Spin the UI loop's drain until the reader threads are done. The
    /// read is on a thread now, so a test that looked straight after
    /// `exec` would be looking before the answer exists.
    fn settle(r: &mut Rig) {
        for _ in 0..2000 {
            drain_attachments(&mut r.state);
            if r.state.attaching == 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("an attachment read never came back");
    }

    #[test]
    fn attaching_something_that_is_not_an_image_says_so_and_stages_nothing() {
        let mut r = Rig::new();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("notes.txt");
        std::fs::write(&f, b"just some text").unwrap();
        exec(
            &mut r.state,
            None,
            &r.tx,
            "attach",
            1,
            &f.display().to_string(),
        );
        settle(&mut r);
        assert!(r.state.attachments.is_empty());
        let last = r.said();
        assert!(last.contains("not an image"), "{last}");
    }

    /// The read happens on a thread, and the prompt says so while it is
    /// out — a fifth of a second in which the prompt looked untouched read
    /// as a dropped keystroke.
    #[test]
    fn a_read_in_flight_is_visible_and_the_image_arrives_afterwards() {
        let mut r = Rig::new();
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("shot.png");
        std::fs::write(&f, png(20, 10)).unwrap();
        exec(
            &mut r.state,
            None,
            &r.tx,
            "attach",
            1,
            &f.display().to_string(),
        );
        // Staged nothing yet, but the count says work is out.
        assert!(r.state.attachments.is_empty());
        assert_eq!(r.state.attaching, 1);

        settle(&mut r);
        assert_eq!(r.state.attaching, 0);
        assert_eq!(r.state.attachments.len(), 1);
        assert_eq!(r.state.attachments[0].name, "shot.png");
        assert!(r.said().contains("attached shot.png"));
    }

    /// `:attach` with nothing after it answers immediately and starts no
    /// thread — there is nothing to read.
    #[test]
    fn attach_with_no_path_says_what_to_type() {
        let mut r = Rig::new();
        exec(&mut r.state, None, &r.tx, "attach", 1, "");
        assert_eq!(r.state.attaching, 0);
        assert!(r.said().contains(":attach PATH"));
    }

    #[test]
    fn attach_clear_takes_one_by_name_or_all_of_them() {
        let mut r = Rig::new();
        r.state.attachments = vec![shot("a.png"), shot("b.png")];
        exec(&mut r.state, None, &r.tx, "attach_clear", 1, "a.png");
        assert_eq!(r.state.attachments.len(), 1);
        assert_eq!(r.state.attachments[0].name, "b.png");
        exec(&mut r.state, None, &r.tx, "attach_clear", 1, "");
        assert!(r.state.attachments.is_empty());
    }

    /// Unstaging a name that is not there says so, rather than reporting a
    /// success it did not have.
    #[test]
    fn unstaging_a_name_that_is_not_there_says_so() {
        let mut r = Rig::new();
        r.state.attachments = vec![shot("a.png")];
        exec(&mut r.state, None, &r.tx, "attach_clear", 1, "nope.png");
        assert_eq!(r.state.attachments.len(), 1);
        assert!(r.said().contains("nope.png"));
    }

    /// A resumed session gets its pictures back, because the log carries
    /// them on the user message rather than in a record of their own —
    /// which is why this feature needed no new `RecordKind`.
    #[test]
    fn a_resumed_session_draws_the_images_it_was_sent() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = eidolon_core::session::Session::create(
            &dir.path().join("s.eid"),
            "prov:m",
            dir.path(),
            None,
        )
        .unwrap();
        s.append(eidolon_core::session::RecordKind::UserMessage(
            eidolon_core::Message::user(vec![
                shot("layout.png").block(),
                ContentBlock::text("what is wrong here?"),
            ]),
        ))
        .unwrap();
        let mut st = UiState::new("m".into(), "s".into(), "/".into());
        st.seed(&s.branch());
        assert!(
            matches!(&st.transcript[0], Entry::Image(a) if a.name == "layout.png" && a.dims == Some((20, 10)))
        );
        assert!(matches!(&st.transcript[1], Entry::User(t) if t == "what is wrong here?"));
    }

    /// A compaction's model call reaches the bus like any other — the
    /// status line says COGITAT and the summary arrives — but the call is
    /// not a turn: the driver's `TurnDone` never comes for it. So the frame
    /// after one has to be the frame a resume draws, and the working
    /// indicator has to let go of it.
    #[tokio::test]
    async fn a_compaction_leaves_the_frame_a_resume_would_draw() {
        use eidolon_core::agent::AgentConfig;
        use eidolon_core::dispatch::Dispatcher;
        use eidolon_core::event::EventBus;
        use eidolon_core::policy::AllowAll;
        use eidolon_core::session::Session;
        use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
        use eidolon_core::tool::ToolRegistry;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.eid");
        let session = Session::create(&path, "mock", tmp.path(), None).unwrap();
        let bus = EventBus::default();
        let dispatcher = Arc::new(Dispatcher::new(
            ToolRegistry::new(),
            Arc::new(AllowAll),
            ScriptedUser::new(true),
            bus.clone(),
            Arc::new(tokio::sync::Mutex::new(session)),
            tmp.path().to_path_buf(),
        ));
        let agent = Arc::new(Agent::new(
            ScriptedProvider::new(vec![
                ScriptedProvider::text("first reply"),
                ScriptedProvider::text("SUMMARY OF EVERYTHING"),
            ]),
            dispatcher,
            AgentConfig {
                model: "mock".into(),
                ..Default::default()
            },
        ));
        let mut state = UiState::new(
            "mock".into(),
            path.display().to_string(),
            tmp.path().display().to_string(),
        );
        let mut events = bus.subscribe();

        // The turn the operator sent — drawn from the message's own record,
        // as the None branch of `UserMessage` does for one steered in, so
        // nothing here has to stand in for the prompt's row — and the
        // driver's end of it, which is what takes the indicator down for a
        // turn.
        agent
            .run_turn(vec![ContentBlock::text("one")], CancellationToken::new())
            .await
            .unwrap();
        while let Ok(ev) = events.try_recv() {
            apply(&mut state, UiMsg::Bus(ev));
        }
        apply(
            &mut state,
            UiMsg::TurnDone(Ok(TurnOutcome::Settled {
                stop_reason: eidolon_core::StopReason::EndTurn,
                usage: Default::default(),
                calls: 1,
            })),
        );
        assert!(!state.working);

        let before = state.transcript.len();
        assert!(agent.compact(CancellationToken::new()).await.unwrap());
        while let Ok(ev) = events.try_recv() {
            apply(&mut state, UiMsg::Bus(ev));
        }

        assert!(!state.working, "the status line is not still saying COGITAT");
        assert!(state.working_since.is_none());
        assert!(
            matches!(state.transcript[before..], [Entry::Info(ref t)] if t.starts_with("compacted ")),
            "the compaction draws the line a resume draws, and not the summary as a reply: {:?}",
            &state.transcript[before..]
        );
        let mut resumed = UiState::new(
            "mock".into(),
            path.display().to_string(),
            tmp.path().display().to_string(),
        );
        {
            let s = agent.session().lock().await;
            resumed.seed(&s.branch());
        }
        assert_eq!(
            state.transcript.iter().map(Entry::text).collect::<Vec<_>>(),
            resumed.transcript.iter().map(Entry::text).collect::<Vec<_>>(),
            "and the whole frame is the frame a resume draws"
        );
    }
}
