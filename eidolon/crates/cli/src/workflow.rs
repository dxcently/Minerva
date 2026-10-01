//! The program's door into a workflow run.
//!
//! A run is a session (ruling 8): the door creates one, and the `session` it
//! prints back is the journal a `resume` reopens. The contract is written
//! for a program caller — Melete's scheduler is the first — so stdout
//! carries exactly one JSON line whatever happened, and the exit code is
//! the verdict:
//!
//! | code | status | payload |
//! |---|---|---|
//! | 0 | `completed` | `report` — the program's return |
//! | 1 | `parked` | `question` — what the run is waiting on a person for |
//! | 2 | `failed` | `reason` — why the run stopped |
//! | 3 | `refused` | `error` — the door never started (or re-entered) a run |
//!
//! Parked is neither success nor failure and gets its own non-zero exit: a
//! parked run that exited zero would be a lie about a run waiting on
//! somebody. The line names the session and the run in every outcome that
//! knows them, so the caller that answers can come back through `resume`
//! and read the same shape again. A refusal is its own code because it
//! journals nothing — nothing happened — and because it is the code
//! `eidolon verba` already uses for a refusal. (Code 2 is also clap's
//! usage-error code; that fires before any of this verb's code runs and
//! never competes with a run outcome.)
//!
//! No new policy path: the door only starts a run, and the gate classifies
//! every step exactly as it does for a model- or operator-started one.
//! Nobody sits at this door, so its questions are declined (`NoUser`, fail
//! closed) unless `--yolo` arms the blanket yes.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, anyhow, bail};
use clap::Subcommand;
use eidolon_core::message::Json;
use eidolon_core::session::{Record, RecordId, RecordKind};
use eidolon_core::tool::ToolRegistry;
use eidolon_core::user::NoUser;
use eidolon_core::workflow::RunStatus;
use eidolon_rune::workflow::{RunOutcome, Workflow};
use eidolon_rune::{Host, ScriptTool};
use tokio_util::sync::CancellationToken;

use crate::config;

/// The exit codes, as the contract names them.
pub(crate) const COMPLETED: i32 = 0;
pub(crate) const PARKED: i32 = 1;
pub(crate) const FAILED: i32 = 2;
pub(crate) const REFUSED: i32 = 3;

/// What the door hands back: one JSON line for stdout, and the exit code
/// that is the verdict.
pub(crate) struct Outcome {
    pub line: String,
    pub code: i32,
}

/// A refusal, rendered — what main folds any door error into, so a program
/// caller reads JSON even when the door never got as far as a run.
pub(crate) fn refused(error: String) -> Outcome {
    Outcome {
        line: serde_json::json!({ "status": "refused", "error": error }).to_string(),
        code: REFUSED,
    }
}

#[derive(Subcommand)]
pub(crate) enum WorkflowOp {
    /// Start a run: compile `<plugin>/workflows/<workflow>.rn`, journal the
    /// pin in a session of its own, and run the program.
    Run(RunRequest),
    /// Re-enter a run the branch holds — a parked one answered, or one cut
    /// mid-flight — and print the same machine shape back.
    Resume(ResumeRequest),
}

#[derive(clap::Args)]
pub(crate) struct RunRequest {
    /// The plugin directory: its `workflows/` holds the program, its
    /// `tools/` are registered for the run, its `lib/` compiles in front.
    /// The directory's name is the pin's `plugin`.
    pub plugin: PathBuf,
    /// The workflow's file stem under the plugin's `workflows/`.
    pub workflow: String,
    /// The argument map, as JSON — an object. `-` reads stdin.
    #[arg(long)]
    pub args: Option<String>,
    /// The version recorded in the pin — what the caller declares it
    /// shipped. The hash over the compiled sources is the resume's real
    /// identity check; this is provenance for everything that reads the
    /// log later.
    #[arg(long)]
    pub version: Option<String>,
    /// Model for the run's session — what `ask_model` answers on, and the
    /// start record's key. Default: the config's, else `mock`.
    #[arg(long)]
    pub model: Option<String>,
    /// Provider override; `mock` needs no credentials.
    #[arg(long)]
    pub provider: Option<String>,
    /// Working directory for the run's tools (default: current).
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// The most steps the run may dispatch — the steps half of the per-run
    /// budget (ruling 4). Journaled with the pin; a resume reads it from
    /// the branch and cannot name a larger one.
    #[arg(long)]
    pub budget_steps: Option<u32>,
    /// The most the run may spend, in dollars — `0.50` is half a dollar.
    /// The spend half of the per-run budget, journaled in micros; priced
    /// spend counts, unpriced calls are a named floor. A resume cannot
    /// raise it.
    #[arg(long)]
    pub budget_spend: Option<f64>,
}

#[derive(clap::Args)]
pub(crate) struct ResumeRequest {
    /// The session the run lives in — the path `run` printed.
    pub session: PathBuf,
    /// The run's record id — the number `run` printed.
    #[arg(value_name = "RUN")]
    pub run: u64,
    /// The plugin directory the run started from. Resume replays against
    /// the program at hand, so it has to be where it was.
    #[arg(long)]
    pub plugin: PathBuf,
    /// The answer to the run's parked question, recorded under the park's
    /// own id before the run re-enters. Refused when nothing is parked.
    #[arg(long)]
    pub answer: Option<String>,
    /// Provider override; `mock` needs no credentials.
    #[arg(long)]
    pub provider: Option<String>,
    /// Working directory override (default: the session's own).
    #[arg(long)]
    pub cwd: Option<PathBuf>,
}

impl RunRequest {
    /// The argument map as the string the pin journals. It must be a JSON
    /// object — the pin carries a map, and a scalar wearing one would be a
    /// pin that lies about what the caller sent.
    fn args_json(&self) -> anyhow::Result<String> {
        let raw = match self.args.as_deref() {
            None => "{}".to_string(),
            Some("-") => {
                let mut s = String::new();
                std::io::stdin()
                    .read_to_string(&mut s)
                    .context("reading the run's args from stdin")?;
                s
            }
            Some(s) => s.to_string(),
        };
        let v: serde_json::Value = serde_json::from_str(raw.trim()).context("the run's args are not JSON")?;
        if !v.is_object() {
            bail!("the run's args are a JSON object");
        }
        Ok(raw)
    }
}

/// Start a run in a session of its own. The session is the run's journal
/// and the id the door prints — a resume reopens it, and everything the
/// run did is replayable from it.
pub(crate) async fn start(cfg: &config::Config, req: RunRequest, yolo: bool) -> anyhow::Result<Outcome> {
    let cwd = cwd_for(req.cwd.clone())?;
    // The program is read and compiled first: a workflow that is not there,
    // or does not compile, costs a message — not an empty journal on disk.
    // The same ordering `run` applies to a prompt that is not an image.
    let program = Program::load(&req.plugin, &req.workflow)?;
    ensure_enabled(cfg, &program)?;
    // The door's permission check, before anything is journaled for the
    // run: every verb the plugin ships must be vouched. A refusal is a
    // refusal, not a question — the door names what is not approved and
    // exits non-zero.
    let permits = eidolon_rune::permits::Permits::from_policy_path(&cfg.policy_path());
    vouch_check(&permits, &program)?;
    program.preflight(&cwd)?;
    let args = req.args_json()?;
    // The budget, parsed before a session exists: a negative spend is a
    // caller's typo, and a refusal is cheaper than a journal that holds a
    // constraint nobody meant.
    let budget = eidolon_core::workflow::RunBudget {
        steps: req.budget_steps,
        spend_micros: match req.budget_spend {
            None => None,
            Some(d) if d < 0.0 => bail!("the spend budget is a dollar amount, not {}", d),
            Some(d) => Some((d * 1_000_000.0).round() as u64),
        },
    };
    let model = req
        .model
        .clone()
        .or_else(|| cfg.default_model.clone())
        .unwrap_or_else(|| crate::default_model(cfg));
    let session = cfg.create_session(&model, &cwd, None)?;
    let rt = crate::build(
        cfg,
        session,
        Arc::new(NoUser),
        yolo,
        crate::Launch {
            cwd,
            model: Some(model),
            provider: req.provider.clone(),
            plugin: Some(program.dir.clone()),
        },
    )
    .await?;
    // The run compiles against the session's own host — the one whose
    // dispatcher the step door rides — from the same sources the preflight
    // already refused on, so the second compile cannot disagree.
    let wf = Workflow::compile(
        &rt.host,
        &program.plugin,
        req.version.as_deref().unwrap_or(""),
        &program.workflow,
        &program.src,
        &program.lib,
    )?;
    let cancel = CancellationToken::new();
    {
        let cancel = cancel.clone();
        tokio::spawn(crate::cancel_on_signals(cancel));
    }
    let outcome = wf.start(rt.agent.dispatcher(), Json(args), budget, cancel).await?;
    let (run, session) = {
        let s = rt.agent.session().lock().await;
        (
            s.workflow_runs().pop().map(|r| r.id).context("the run left no pin behind")?,
            s.path().display().to_string(),
        )
    };
    Ok(done(&session, run, outcome))
}

/// Re-enter a run: refuse what the interpreter refuses — before anything is
/// recorded — record the answer under the park's own id, and replay.
pub(crate) async fn resume(cfg: &config::Config, req: ResumeRequest, yolo: bool) -> anyhow::Result<Outcome> {
    let override_cwd = match &req.cwd {
        Some(c) => Some(cwd_for(Some(c.clone()))?),
        None => None,
    };
    let s = match &override_cwd {
        Some(c) => eidolon_core::Session::open_with_cwd(&req.session, c)?,
        None => eidolon_core::Session::open(&req.session)?,
    };
    let back = s
        .workflow_run(req.run)
        .ok_or_else(|| anyhow!("no run #{} in {}", req.run, req.session.display()))?;
    let cwd = match override_cwd {
        Some(c) => c,
        None => {
            let logged = s.cwd().context("session has no start record")?;
            crate::launch_cwd(PathBuf::from(logged))
        }
    };
    let program = Program::load(&req.plugin, &back.pin.workflow)?;
    ensure_enabled(cfg, &program)?;
    // Everything that can refuse, refuses here — before the answer is
    // recorded. A refusal journals nothing, and an answer that outlived
    // its refusal would be a fact in a log that says nothing happened.
    let pre = program.preflight(&cwd)?;
    if pre.hash() != back.pin.hash {
        bail!(
            "refusing run #{}: it was started under hash {} and the program at hand is {}",
            req.run,
            back.pin.hash,
            pre.hash()
        );
    }
    if let Some(gap) = back.gap() {
        bail!(
            "refusing run #{}: step {gap} cannot be replayed — a hole is a refusal, never a re-run",
            req.run
        );
    }
    ensure_no_failed_step(&s.branch(), req.run)?;
    // The tail check's twin — the same words the interpreter refuses with,
    // repeated here because the door must refuse before it records, and
    // the interpreter's own check runs only once the program is in hand.
    if let Some(last) = back.steps.last()
        && last.result.is_none()
        && matches!(back.end, Some(RunStatus::Completed) | Some(RunStatus::Failed))
    {
        bail!(
            "refusing run #{}: it ended {:?} with step {} unanswered",
            req.run,
            back.end,
            last.index
        );
    }
    // The park the answer belongs to, found while the session is still in
    // this hand: the first ordinal with no answer is the line the program
    // stops on, and that is the only id an answer may be recorded under.
    let park = match &req.answer {
        Some(_) => Some(
            first_unanswered_park(&s.branch(), req.run)
                .ok_or_else(|| anyhow!("run #{} has no unanswered park to answer", req.run))?,
        ),
        None => None,
    };
    // Resume carries no model: what the branch names is the key, the same
    // rule `resume` keeps for a conversation.
    let rt = crate::build(
        cfg,
        s,
        Arc::new(NoUser),
        yolo,
        crate::Launch {
            cwd,
            model: None,
            provider: req.provider.clone(),
            plugin: Some(program.dir),
        },
    )
    .await?;
    let wf = Workflow::compile(
        &rt.host,
        &back.pin.plugin,
        &back.pin.version,
        &back.pin.workflow,
        &program.src,
        &program.lib,
    )?;
    if let Some((call_id, prompt)) = park {
        let answer = req.answer.clone().context("an answer vanished before it was recorded")?;
        rt.agent
            .session()
            .lock()
            .await
            .append(RecordKind::AskUser {
                call_id,
                prompt,
                answer: Some(answer),
            })
            .context("recording the answer")?;
    }
    let cancel = CancellationToken::new();
    {
        let cancel = cancel.clone();
        tokio::spawn(crate::cancel_on_signals(cancel));
    }
    let outcome = wf.resume(rt.agent.dispatcher(), req.run, cancel).await?;
    let session = rt.agent.session().lock().await.path().display().to_string();
    Ok(done(&session, req.run, outcome))
}

fn cwd_for(cwd: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match cwd {
        Some(c) => c
            .canonicalize()
            .with_context(|| format!("finding {}", c.display())),
        None => std::env::current_dir().context("finding the current directory"),
    }
}

/// The run's ending, as the machine reads it: the session id, the run id,
/// the status, and the one field the status owes.
fn done(session: &str, run: RecordId, outcome: RunOutcome) -> Outcome {
    match outcome {
        RunOutcome::Completed { report } => Outcome {
            line: serde_json::json!({ "status": "completed", "session": session, "run": run, "report": report.content })
                .to_string(),
            code: COMPLETED,
        },
        RunOutcome::Failed { reason } => Outcome {
            line: serde_json::json!({ "status": "failed", "session": session, "run": run, "reason": reason }).to_string(),
            code: FAILED,
        },
        RunOutcome::Parked { question } => Outcome {
            line: serde_json::json!({ "status": "parked", "session": session, "run": run, "question": question })
                .to_string(),
            code: PARKED,
        },
    }
}

/// The run's first park still waiting on somebody. Parks answer in program
/// order — the ordinal is the program's own — so the first unanswered
/// ordinal is the line the program stops on. Each park's newest record
/// wins, because the answer is a second record under the same id, not an
/// edit of the first.
fn first_unanswered_park(branch: &[&Record], run: RecordId) -> Option<(String, String)> {
    let prefix = format!("w{run}-park");
    let mut parks: BTreeMap<u32, (String, Option<String>)> = BTreeMap::new();
    for rec in branch {
        if let RecordKind::AskUser {
            call_id,
            prompt,
            answer,
        } = &rec.kind
            && let Some(n) = call_id.strip_prefix(&prefix).and_then(|n| n.parse::<u32>().ok())
        {
            parks.insert(n, (prompt.clone(), answer.clone()));
        }
    }
    parks
        .into_iter()
        .find(|(_, (_, answer))| answer.is_none())
        .map(|(n, (prompt, _))| (format!("{prefix}{n}"), prompt))
}

/// A plugin's verbs, as adoption registers them: every `tools/*.rn` whose
/// stem can carry the plugin's namespace and does not join to a built-in's
/// name. **One implementation**, because the door's check and the door's
/// own registration agreeing is the whole reason a vouch can match what
/// runs — they disagreed once (the check derived `<plugin>_<stem>` while
/// the registration took the file's declared name), and a vouched run asked
/// anyway (2026-09-28).
pub(crate) fn plugin_stems(dir: &Path, plugin: &str) -> anyhow::Result<Vec<String>> {
    let tools = dir.join("tools");
    let mut stems: Vec<String> = match std::fs::read_dir(&tools) {
        Ok(files) => files
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "rn"))
            .filter_map(|e| e.path().file_stem()?.to_str().map(String::from))
            // A stem outside the namespace grammar is refused at adoption,
            // and a plugin tool never takes a built-in's name — neither is
            // ever a verb, so neither is registered here nor required to be
            // vouched.
            .filter(|stem| eidolon_rune::plugins::valid_name(stem))
            .filter(|stem| !eidolon_rune::is_builtin(&format!("{plugin}_{stem}")))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", tools.display())),
    };
    stems.sort();
    Ok(stems)
}

/// The registered verb name for one of a plugin's stems.
pub(crate) fn plugin_verb(plugin: &str, stem: &str) -> String {
    format!("{plugin}_{stem}")
}

/// A plugin's name, from **the path the caller used**: the directory's own
/// last component, and the canonical one only when the given path has none
/// (`..`, a root). The entry's name is the identity everywhere else —
/// adoption's namespace, the pin, `plugins trust`, the vouch rows — so
/// naming by the canonical *target* is how a symlinked plugin directory
/// came to have two names: with `plugins/browser` pointing at a directory
/// called `browser-0.1.0`, the listing said `browser` while the door
/// refused as `browser-0.1.0` and advised a `plugins trust` that could not
/// be run (2026-09-28). Canonicalize for reading; never for naming.
pub(crate) fn plugin_name(dir: &Path) -> anyhow::Result<String> {
    if let Some(name) = dir.file_name() {
        return Ok(name.to_string_lossy().into_owned());
    }
    let canonical = dir
        .canonicalize()
        .with_context(|| format!("finding the plugin at {}", dir.display()))?;
    canonical
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .context("the plugin is a directory, and its name is the run's `plugin`")
}

/// The door's check, ruled 2026-09-28 in place of the warrant: a workflow
/// run starts only if **every verb in the plugin's derived set is vouched**.
/// The set is derived from the directory — `tools/*.rn`, what adoption would
/// register as `<plugin>_<stem>` — and never from `claims.tools`, because
/// contents are authoritative everywhere else in this design and they are
/// authoritative here: a plugin that under-declares must not thereby shrink
/// the set it has to get approved. The vouch is the stage's own test — the
/// row exists *and* the script is byte-for-byte what was vouched — so an
/// edited verb counts as unvouched here too. Nothing changes inside the run:
/// each step still rides the one table, and a step reaching outside the
/// plugin's own set is classified fresh on its own merits.
fn vouch_check(
    permits: &eidolon_rune::permits::Permits,
    program: &Program,
) -> anyhow::Result<()> {
    let tools = program.dir.join("tools");
    let stems = plugin_stems(&program.dir, &program.plugin)?;
    if stems.is_empty() {
        return Ok(());
    }
    let missing: Vec<String> = stems
        .into_iter()
        .filter(|stem| {
            let verb = plugin_verb(&program.plugin, stem);
            let vouched = matches!(permits.vouched(&verb), Some(row) if {
                std::fs::read_to_string(tools.join(format!("{stem}.rn")))
                    .map(|src| eidolon_rune::permits::content_hash(&src))
                    .ok()
                    .as_deref()
                    == Some(row.hash.as_str())
            });
            !vouched
        })
        .map(|stem| plugin_verb(&program.plugin, &stem))
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            // The verb's real spelling, which is now the nested one — a
            // hint naming a command that errors would send the operator in
            // a circle the refusal just caught them in.
            "{}: workflow run refused — not vouched: {} (`eidolon plugins trust {}`)",
            program.plugin,
            missing.join(", "),
            program.plugin
        );
    }
    Ok(())
}

/// The plugin's record says whether it runs. Presence is not permission
/// anywhere else in this design, and enablement is the operator's one
/// lifecycle bit — so the door reads `plugins.toml` rather than the
/// directory's existence (`crates/rune/src/store.rs`). A plugin absent from
/// the record is enabled, which is what makes deleting the file leave
/// everything working.
fn ensure_enabled(cfg: &config::Config, program: &Program) -> anyhow::Result<()> {
    if eidolon_rune::store::Store::is_enabled(&cfg.plugins_dir(), &program.plugin) {
        return Ok(());
    }
    bail!(
        "{}: the plugin is disabled — `eidolon plugins enable {}` first. Its vouches and grants were left alone, so re-enabling asks nobody again",
        program.plugin,
        program.plugin
    )
}

/// A run holding a step whose result is journaled as an error cannot be
/// resumed into a different ending. The memo replays a journaled result
/// verbatim — that is the contract, and it is why a run is never run twice —
/// so the same failure comes back, and a re-vouch or a re-answer afterwards
/// changes nothing the log already said (found the hard way, 2026-09-28: an
/// operator re-trusted the plugin, resumed, and read the replay as if it
/// were fresh). Refused before the answer is recorded, with the remedy
/// named: a new run. Not a retry-from-step-N, which would be a second truth
/// beside the log.
fn ensure_no_failed_step(branch: &[&Record], run: RecordId) -> anyhow::Result<()> {
    let Some(back) = eidolon_core::workflow::run(branch, run) else {
        return Ok(());
    };
    let Some(step) = back
        .steps
        .iter()
        .find(|s| s.result.as_ref().is_some_and(|res| res.is_error))
    else {
        return Ok(());
    };
    let content = step.result.as_ref().map(|r| r.content.as_str()).unwrap_or_default();
    bail!(
        "refusing run #{run}: step {} ({} {}) is journaled as a failure — {}. The memo replays a journaled result verbatim, so resuming cannot change it: start a new run (`eidolon workflow run {} {}`)",
        step.index,
        step.call_id,
        step.tool,
        preview_line(content),
        back.pin.plugin,
        back.pin.workflow,
    )
}

/// One line of a journaled result, for a refusal that has to quote what went
/// wrong without pasting a page into the operator's terminal.
fn preview_line(s: &str) -> String {
    let flat = s.replace('\n', " ");
    let cut = flat.chars().take(120).collect::<String>();
    if flat.chars().count() > 120 {
        format!("{cut}…")
    } else {
        cut
    }
}

/// The program at hand: the workflow's source, every `lib/*.rn` compiled in
/// front of it, and the plugin's name — its directory's, which is the
/// identity the design gives a plugin.
struct Program {
    /// The plugin directory, canonical — what the reads below use.
    dir: PathBuf,
    /// The plugin's name, from the path the run was **asked for** — the
    /// pin's `plugin`, and the namespace every verb it ships registers
    /// under.
    plugin: String,
    /// The workflow's file stem — the compile's `name`.
    workflow: String,
    src: String,
    lib: Vec<String>,
}

impl Program {
    fn load(plugin: &Path, workflow: &str) -> anyhow::Result<Program> {
        // The name comes from the path **as given** and the directory is
        // canonical only for reading: a symlinked plugin directory keeps
        // the name its entry has (see `plugin_name`).
        let name = plugin_name(plugin)?;
        let dir = plugin
            .canonicalize()
            .with_context(|| format!("finding the plugin at {}", plugin.display()))?;
        let path = dir.join("workflows").join(format!("{workflow}.rn"));
        let src = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        // `lib` in name order: the pin's hash is over the ordered source
        // set, so the order has to be a fact of the directory and not of
        // the filesystem's mood.
        let mut lib = Vec::new();
        match std::fs::read_dir(dir.join("lib")) {
            Ok(entries) => {
                let mut paths: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "rn"))
                    .collect();
                paths.sort();
                for p in paths {
                    lib.push(std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", dir.join("lib").display()));
            }
        }
        Ok(Program {
            dir,
            plugin: name,
            workflow: workflow.to_string(),
            src,
            lib,
        })
    }

    /// The compile that refuses before a session exists, on a throwaway
    /// host. The run itself compiles again against the session's own host —
    /// the step door rides that host's dispatcher — from the same sources,
    /// so the second compile cannot disagree with the first.
    ///
    /// "Cannot disagree" includes the scope: the real session attaches a
    /// completion path unconditionally (build does, whatever model ends up
    /// resolving), and the workflow scope registers `ask_model` when — and
    /// only when — the host carries one. A preflight host without it
    /// refused every model-naming workflow before a session existed, which
    /// the interpreter's first live run found (2026-09-27): the door's
    /// refusal fired where the run would have compiled clean. The handle
    /// here is a shell — the catalog a real call reads is the session's,
    /// built later — and nothing compiles against more than the fact of
    /// its presence.
    fn preflight(&self, cwd: &Path) -> anyhow::Result<Arc<Workflow>> {
        let host = Host::new(cwd.to_path_buf());
        host.attach_completion(eidolon_rune::host::Completion {
            catalog: Arc::new(eidolon_providers::Catalog::new()),
            default_model: String::new(),
            max_tokens: 0,
        });
        Workflow::compile(&host, &self.plugin, "", &self.workflow, &self.src, &self.lib)
    }
}

/// The plugin's tools, registered as the unit they ship in: every
/// `tools/*.rn` compiles or the door refuses — the plugin directory is the
/// unit, and half a plugin is not it — and a name already taken is a
/// refusal, because plugins never shadow anything. The operator's own
/// directory keeps the override power by registering first.
///
/// One already-taken name is not a refusal: this same directory's tool,
/// registered by adoption at session build. A plugin installed under the
/// plugins directory has its `tools/*.rn` adopted as `<plugin>_<stem>`
/// before the door runs, and this compiles the same files to the same name
/// with the same attribution — same name and same plugin is the same tool
/// registered twice, not a conflict, and adopting's copy keeps the seat.
/// Any other collision — a built-in's, the operator's, or another plugin's
/// — refuses, naming the holder when the registry knows one. An installed
/// plugin is not required for the run: a directory the operator names
/// directly registers the same way, which is the case the door's own
/// registration had to be able to answer on its own (2026-09-28).
pub(crate) fn register_plugin_tools(
    reg: &mut ToolRegistry,
    host: &Arc<Host>,
    dir: &Path,
    plugin: &str,
) -> anyhow::Result<()> {
    // The verbs this directory ships, by the **same derivation the door's
    // check uses** (`plugin_stems`), so the set that must be vouched and the
    // set that gets registered are one set and cannot drift apart again.
    let stems = plugin_stems(dir, plugin)?;
    let tools = dir.join("tools");
    for stem in stems {
        let path = tools.join(format!("{stem}.rn"));
        let src =
            std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let name = plugin_verb(plugin, &stem);
        // Compiled under its namespaced name and attributed to the plugin,
        // exactly as adoption does it. One tool has one name wherever it can
        // be observed — the registry, the gate's question, the log's record,
        // the vouch row — and the file's own declaration does not win,
        // because the layout is what a log can hold against the directory.
        // Before this, the door registered the *declared* name and no
        // attribution: a bare-declaring tool ran as `echo`, and the vouched
        // row for `<plugin>_echo` could never match what the gate was asked
        // about, so a vouched run asked anyway.
        let mut tool = ScriptTool::compile(&name, &src, host.clone())
            .with_context(|| format!("compiling {}", path.display()))?;
        tool.attribute(&name, plugin);
        if let Some(existing) = reg.get(&name) {
            if existing.manifest().plugin.as_deref() == Some(plugin) {
                // Adoption registered this same directory's tool already; the
                // attributed copy keeps the seat — same name and same plugin
                // is the same tool registered twice, not a conflict.
                continue;
            }
            match existing.manifest().plugin.as_deref() {
                Some(holder) => bail!(
                    "{}: tool `{name}` is already registered by plugin `{holder}` — a plugin never shadows another plugin's",
                    path.display()
                ),
                None => bail!(
                    "{}: tool `{name}` is already registered — a plugin never shadows a built-in, the operator's tools, or another plugin's",
                    path.display()
                ),
            }
        }
        reg.register(Arc::new(tool));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workflow the scope gate refuses to compile without a completion
    /// path: the preflight must see the same scope the run's own compile
    /// sees, or the door refuses model-naming workflows that the real
    /// session would have run (the first live run found this, 2026-09-27).
    const MAIN_ASKS: &str = r#"
pub fn workflow() {
    #{ name: "main", description: "asks the model face" }
}

pub async fn run(args) {
    eidolon::ask_model("one prompt", #{}).await?;
    "done"
}
"#;

    const ECHO: &str = r#"
pub fn manifest() {
    #{
        // Deliberately the *bare* name: the layout is what names a plugin's
        // contribution, so the fixtures exercise the shape a door that
        // trusted the declaration used to register — and the shape that
        // made a vouched run ask anyway.
        name: "echo",
        description: "returns its `text` input",
        approval: "read_only",
        input_schema: #{ "type": "object", "properties": #{ "text": #{ "type": "string" } }, "required": ["text"] },
    }
}

pub async fn call(input) {
    input.text
}
"#;

    /// Two steps, no args: the completed run's report is the two echoes'
    /// answers joined.
    const MAIN: &str = r#"
pub fn workflow() {
    #{ name: "main", description: "echoes twice and reports" }
}

pub async fn run(args) {
    let a = eidolon::tool_call("plugin_echo", #{ text: "one" }).await?;
    let b = eidolon::tool_call("plugin_echo", #{ text: "two" }).await?;
    a + "/" + b
}
"#;

    /// One step, a park, and a step that carries the answer out — the
    /// parked run's whole life, first pass and resume.
    const ASK: &str = r#"
pub fn workflow() {
    #{ name: "ask", description: "parks on a question" }
}

pub async fn run(args) {
    let a = eidolon::tool_call("plugin_echo", #{ text: "before" }).await?;
    let answer = eidolon::park("Ship it?").await?;
    let b = eidolon::tool_call("plugin_echo", #{ text: answer }).await?;
    a + "/" + b
}
"#;

    /// A plugin file naming a built-in: it lands as `<plugin>_read`, so the
    /// built-in is untouched — shadowing is impossible by construction now,
    /// which is what this fixture exists to keep true.
    const SHADOW: &str = r#"
pub fn manifest() {
    #{
        name: "read",
        description: "tries to take a built-in's name",
        approval: "read_only",
        input_schema: #{ "type": "object" },
    }
}

pub async fn call(input) {
    "no"
}
"#;

    /// A tool declaring the plugin-namespaced name — the shape the spec
    /// tells a shipped tool to carry, and the shape that meets adoption's
    /// registration of the same directory head-on.
    const NAMESPACED: &str = r#"
pub fn manifest() {
    #{
        name: "plugin_ping",
        description: "pings, under the plugin's own namespace",
        approval: "read_only",
        input_schema: #{ "type": "object" },
    }
}

pub async fn call(input) {
    "pong"
}
"#;

    /// A config with every directory it would otherwise find under the
    /// operator's home pointed into the test's own root, and the swarm off:
    /// a test that wrote to the real home would be a test worth deleting.
    fn cfg(root: &Path) -> config::Config {
        toml::from_str(&format!(
            "sessions_dir = '{}'\n\
             tools_dir = '{}'\n\
             providers_dir = '{}'\n\
             secrets_dir = '{}'\n\
             plugins_dir = '{}'\n\
             [swarm]\n\
             enabled = false\n\
             [policy]\n\
             enabled = false\n\
             path = '{}'\n",
            root.join("sessions").display(),
            root.join("tools").display(),
            root.join("providers").display(),
            root.join("secrets").display(),
            root.join("plugins").display(),
            root.join("policy.rn").display(),
        ))
        .unwrap()
    }

    /// Vouch every verb the fixture plugin ships, through the store at the
    /// cfg's policy path — what `eidolon plugins trust <plugin>` does, and
    /// what the door now requires before a run starts. A test that ran a
    /// workflow past the door unvouched would be a test worth deleting.
    fn vouch_fixture(cfg: &config::Config, dir: &Path) {
        let tools = dir.join("tools");
        let mut stems: Vec<String> = std::fs::read_dir(&tools)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "rn"))
            .filter_map(|e| e.path().file_stem()?.to_str().map(String::from))
            .collect();
        stems.sort();
        let plugin = dir.file_name().unwrap().to_string_lossy();
        let permits = eidolon_rune::permits::Permits::from_policy_path(&cfg.policy_path());
        let host = eidolon_rune::Host::new(std::env::current_dir().unwrap());
        permits.attach_checker(std::sync::Arc::new(move |src| {
            eidolon_rune::permits::check_composed(src, eidolon_rune::policy::DEFAULT_POLICY, &host)
        }));
        let mut rows = eidolon_rune::permits::Rows::default();
        for stem in stems {
            let src = std::fs::read_to_string(tools.join(format!("{stem}.rn"))).unwrap();
            let verb = format!("{plugin}_{stem}");
            rows.vouch.push(eidolon_rune::permits::VouchRow {
                plugin: plugin.to_string(),
                verb,
                hash: eidolon_rune::permits::content_hash(&src),
                granted_by: "noah".into(),
                granted_at: "2026-09-28T00:00:00Z".into(),
            });
        }
        permits.write(&rows).unwrap();
    }

    fn plugin(root: &Path) -> PathBuf {
        let dir = root.join("plugin");
        std::fs::create_dir_all(dir.join("workflows")).unwrap();
        std::fs::create_dir_all(dir.join("tools")).unwrap();
        std::fs::write(dir.join("tools").join("echo.rn"), ECHO).unwrap();
        std::fs::write(dir.join("workflows").join("main.rn"), MAIN).unwrap();
        std::fs::write(dir.join("workflows").join("ask.rn"), ASK).unwrap();
        dir
    }

    fn work(root: &Path) -> PathBuf {
        let dir = root.join("work");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn vouch_row(verb: &str, src: &str) -> eidolon_rune::permits::VouchRow {
        eidolon_rune::permits::VouchRow {
            plugin: "plugin".into(),
            verb: verb.into(),
            hash: eidolon_rune::permits::content_hash(src),
            granted_by: "noah".into(),
            granted_at: "2026-09-28T00:00:00Z".into(),
        }
    }

    /// A step that goes down as a failure. The tool it calls is not
    /// registered, so the run ends `Failed` with an error result journaled
    /// on step 0 — the shape an operator's re-vouch cannot change.
    const BOOM: &str = r#"
pub fn workflow() {
    #{ name: "boom", description: "calls a tool that is not registered" }
}

pub async fn run(args) {
    eidolon::tool_call("plugin_nope", #{}).await
}
"#;

    /// A disabled plugin does not run. The door reads the record rather than
    /// the directory's presence — presence is not permission anywhere else
    /// in this design — and `plugins_dir` being aimable is what makes that
    /// testable at all: before it, the plugin surface could only be
    /// hand-run against a scratch `XDG_CONFIG_HOME`.
    #[tokio::test]
    async fn a_disabled_plugin_does_not_run() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let cfg = cfg(tmp.path());
        let plugins = cfg.plugins_dir();
        let dir = plugins.join("plugin");
        std::fs::create_dir_all(dir.join("tools"))?;
        std::fs::create_dir_all(dir.join("workflows"))?;
        std::fs::write(
            dir.join(eidolon_rune::plugins::PLUGIN_FILE),
            r#"pub fn manifest() { #{ name: "plugin", description: "t" } }"#,
        )?;
        std::fs::write(dir.join("tools").join("echo.rn"), ECHO)?;
        std::fs::write(dir.join("workflows").join("main.rn"), MAIN)?;
        vouch_fixture(&cfg, &dir);

        // Absent from the record is enabled: an unmanaged directory is one
        // nobody has said no to, which is what keeps a hand-install working.
        let out = start(&cfg, run_req(dir.clone(), "main", tmp.path()), false).await?;
        assert_eq!(out.code, COMPLETED, "unmanaged runs: {}", out.line);

        let mut store = eidolon_rune::store::Store::load(&plugins)?;
        store.set_enabled("plugin", false);
        store.write()?;

        let out = door(start(&cfg, run_req(dir, "main", tmp.path()), false).await);
        assert_eq!(out.code, REFUSED, "a disabled plugin does not run: {}", out.line);
        assert!(out.line.contains("the plugin is disabled"), "{}", out.line);
        assert!(
            out.line.contains("plugins enable plugin"),
            "the refusal names the remedy: {}",
            out.line
        );
        Ok(())
    }

    /// A run whose step went down as a failure cannot be resumed into a
    /// different ending: the memo replays a journaled result verbatim, so a
    /// re-vouch or a re-answer afterwards changes nothing the log said —
    /// 2026-09-28's operator loop (re-trust, resume, read the replay as
    /// fresh) is what this refusal exists to prevent. Not a retry-from-step-N:
    /// that would be a second truth beside the log.
    #[tokio::test]
    async fn a_run_with_a_failed_step_cannot_be_resumed() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = plugin(tmp.path());
        vouch_fixture(&cfg(tmp.path()), &dir);
        std::fs::write(dir.join("workflows").join("boom.rn"), BOOM)?;

        let out = start(&cfg(tmp.path()), run_req(dir.clone(), "boom", tmp.path()), false).await?;
        assert_eq!(out.code, FAILED, "the step failed: {}", out.line);
        let v: serde_json::Value = serde_json::from_str(&out.line)?;
        let session = PathBuf::from(v["session"].as_str().unwrap());
        let run = v["run"].as_u64().unwrap();

        let out = door(
            resume(
                &cfg(tmp.path()),
                ResumeRequest {
                    session,
                    run,
                    plugin: dir,
                    answer: None,
                    provider: Some("mock".into()),
                    cwd: None,
                },
                false,
            )
            .await,
        );
        assert_eq!(out.code, REFUSED, "{}", out.line);
        assert!(
            out.line.contains("journaled as a failure"),
            "the refusal names the step: {}",
            out.line
        );
        assert!(
            out.line.contains("start a new run"),
            "and the remedy: {}",
            out.line
        );
        Ok(())
    }

    /// The door's check, ruled 2026-09-28: a run starts only if every verb
    /// the plugin ships is vouched — the set derived from the directory,
    /// each vouch hash-fresh, and a refusal naming what is missing rather
    /// than asking. Vouching one of two names only the other; editing a
    /// vouched script un-vouches it.
    #[test]
    fn the_door_refuses_a_run_until_every_verb_the_plugin_ships_is_vouched() {
        let root = tempfile::tempdir().unwrap();
        let dir = plugin(root.path());
        std::fs::write(dir.join("tools").join("extra.rn"), ECHO).unwrap();
        let permits = eidolon_rune::permits::Permits::at(root.path().join("policy.permits.rn"));
        let host = eidolon_rune::Host::new(root.path().to_path_buf());
        permits.attach_checker(std::sync::Arc::new(move |src| {
            eidolon_rune::permits::check_composed(src, eidolon_rune::policy::DEFAULT_POLICY, &host)
        }));
        let program = Program::load(&dir, "main").unwrap();

        let err = vouch_check(&permits, &program).unwrap_err().to_string();
        assert!(err.contains("plugin_echo"), "{err}");
        assert!(err.contains("plugin_extra"), "{err}");

        let src = std::fs::read_to_string(dir.join("tools").join("echo.rn")).unwrap();
        let mut rows = eidolon_rune::permits::Rows::default();
        rows.vouch.push(vouch_row("plugin_echo", &src));
        permits.write(&rows).unwrap();
        let err = vouch_check(&permits, &program).unwrap_err().to_string();
        assert!(err.contains("plugin_extra"), "{err}");
        assert!(!err.contains("plugin_echo, "), "{err}");

        rows.vouch.push(vouch_row("plugin_extra", &src));
        permits.write(&rows).unwrap();
        vouch_check(&permits, &program).expect("both vouched, both fresh: the run starts");

        std::fs::write(dir.join("tools").join("extra.rn"), format!("{ECHO}\n// edited")).unwrap();
        let err = vouch_check(&permits, &program).unwrap_err().to_string();
        assert!(err.contains("plugin_extra"), "{err}");
    }

    fn run_req(dir: PathBuf, workflow: &str, root: &Path) -> RunRequest {
        RunRequest {
            plugin: dir,
            workflow: workflow.into(),
            args: None,
            version: None,
            model: None,
            provider: Some("mock".into()),
            cwd: Some(work(root)),
            budget_steps: None,
            budget_spend: None,
        }
    }

    /// main's fold: whatever the door returns, the caller reads an Outcome.
    fn door(out: anyhow::Result<Outcome>) -> Outcome {
        out.unwrap_or_else(|e| refused(format!("{e:#}")))
    }

    fn answer_records(s: &eidolon_core::Session) -> Vec<(String, String, Option<String>)> {
        s.branch()
            .into_iter()
            .filter_map(|rec| match &rec.kind {
                RecordKind::AskUser {
                    call_id,
                    prompt,
                    answer,
                } => Some((call_id.clone(), prompt.clone(), answer.clone())),
                _ => None,
            })
            .collect()
    }

    /// The preflight's scope is the run compile's scope: a workflow naming
    /// `eidolon::ask_model` passes the door's pre-session compile, against
    /// a real plugin directory, because the real session's host carries a
    /// completion path and the throwaway one must say so too. The hash it
    /// returns is the one the pin carries, so a resume's preflight compares
    /// equal on the same sources.
    #[test]
    fn preflight_sees_the_scope_the_run_compiles_with() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join("model-namer");
        std::fs::create_dir_all(dir.join("workflows"))?;
        std::fs::write(dir.join("workflows").join("main.rn"), MAIN_ASKS)?;
        let program = Program::load(&dir, "main")?;
        let pre = program.preflight(&tmp.path().join("work"))?;
        assert_eq!(pre.name(), "main");
        let real = Workflow::compile(
            &{
                let host = eidolon_rune::Host::new(tmp.path().join("work"));
                host.attach_completion(eidolon_rune::host::Completion {
                    catalog: Arc::new(eidolon_providers::Catalog::new()),
                    default_model: "mock".into(),
                    max_tokens: 64,
                });
                host
            },
            &program.plugin,
            "",
            &program.workflow,
            &program.src,
            &program.lib,
        )?;
        assert_eq!(pre.hash(), real.hash(), "same sources, same pin");
        Ok(())
    }

    #[test]
    fn the_exit_codes_are_the_contract() {
        assert_eq!(COMPLETED, 0);
        assert_ne!(PARKED, COMPLETED, "a parked run that exits zero is a lie about a run waiting on a person");
        assert_ne!(FAILED, COMPLETED);
        assert_ne!(PARKED, FAILED);
        assert_ne!(REFUSED, FAILED, "a refusal journals nothing and is not a run's failure");
        assert_ne!(REFUSED, PARKED);
    }

    #[tokio::test]
    async fn a_completed_run_prints_its_session_its_run_and_its_report() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = plugin(tmp.path());
        vouch_fixture(&cfg(tmp.path()), &dir);
        let out = start(&cfg(tmp.path()), run_req(dir, "main", tmp.path()), false).await?;
        assert_eq!(out.code, COMPLETED);
        let v: serde_json::Value = serde_json::from_str(&out.line)?;
        assert_eq!(v["status"], "completed");
        assert_eq!(v["report"], "one/two");

        // The session named is a journal that exists, holds the run, and
        // says what the line says — the resume's way back in.
        let session = PathBuf::from(v["session"].as_str().context("the line names the session")?);
        assert!(session.exists(), "the session named is on disk");
        let s = eidolon_core::Session::open(&session)?;
        let runs = s.workflow_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, v["run"].as_u64().context("the line names the run")?);
        assert_eq!(runs[0].end, Some(RunStatus::Completed));

        // The steps dispatched under the run's own ids — the gate's
        // journal, not a side channel.
        let ids: Vec<String> = s
            .branch()
            .into_iter()
            .filter_map(|rec| match &rec.kind {
                RecordKind::ToolResult { tool_use_id, .. } if tool_use_id.starts_with(&format!("w{}-", runs[0].id)) => {
                    Some(tool_use_id.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec![format!("w{}-0", runs[0].id), format!("w{}-1", runs[0].id)]);
        Ok(())
    }

    #[tokio::test]
    async fn a_parked_run_is_a_non_zero_exit_with_the_question_and_the_answered_resume_completes() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = plugin(tmp.path());
        vouch_fixture(&cfg(tmp.path()), &dir);
        let out = start(&cfg(tmp.path()), run_req(dir.clone(), "ask", tmp.path()), false).await?;
        assert_eq!(out.code, PARKED, "parked is its own status and its own code");
        let v: serde_json::Value = serde_json::from_str(&out.line)?;
        assert_eq!(v["status"], "parked");
        assert_eq!(v["question"], "Ship it?");
        let session = PathBuf::from(v["session"].as_str().unwrap());
        let run = v["run"].as_u64().unwrap();

        // The answer goes back through the same door and the same shape.
        let out = door(
            resume(
                &cfg(tmp.path()),
                ResumeRequest {
                    session: session.clone(),
                    run,
                    plugin: dir,
                    answer: Some("yes".into()),
                    provider: Some("mock".into()),
                    cwd: None,
                },
                false,
            )
            .await,
        );
        assert_eq!(out.code, COMPLETED);
        let v: serde_json::Value = serde_json::from_str(&out.line)?;
        assert_eq!(v["status"], "completed");
        assert_eq!(v["report"], "before/yes", "the answer flowed into the run");

        // The log holds the question and the answer under the park's own
        // id, and the step after the park fired exactly once.
        let s = eidolon_core::Session::open(&session)?;
        let asks = answer_records(&s);
        let park_id = format!("w{run}-park0");
        assert_eq!(asks.len(), 2, "the question's record and the answer's");
        assert_eq!(asks[0], (park_id.clone(), "Ship it?".into(), None));
        assert_eq!(asks[1], (park_id, "Ship it?".into(), Some("yes".into())));
        let after: Vec<&String> = s
            .branch()
            .into_iter()
            .filter_map(|rec| match &rec.kind {
                RecordKind::ToolResult { content, tool_use_id, .. } if tool_use_id.ends_with("-1") => Some(content),
                _ => None,
            })
            .collect();
        assert_eq!(after, [&"yes".to_string()], "the resumed step ran once with the answer");
        Ok(())
    }

    #[tokio::test]
    async fn a_resume_refuses_a_changed_program_before_the_answer_is_recorded() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = plugin(tmp.path());
        vouch_fixture(&cfg(tmp.path()), &dir);
        let out = start(&cfg(tmp.path()), run_req(dir.clone(), "ask", tmp.path()), false).await?;
        assert_eq!(out.code, PARKED);
        let v: serde_json::Value = serde_json::from_str(&out.line)?;
        let session = PathBuf::from(v["session"].as_str().unwrap());
        let run = v["run"].as_u64().unwrap();

        // The program at hand is no longer the program the run started
        // under: the resume refuses, and the answer is not recorded.
        std::fs::write(dir.join("workflows").join("ask.rn"), ASK.replace("Ship it?", "Ship them?"))?;
        let out = door(
            resume(
                &cfg(tmp.path()),
                ResumeRequest {
                    session: session.clone(),
                    run,
                    plugin: dir,
                    answer: Some("yes".into()),
                    provider: Some("mock".into()),
                    cwd: None,
                },
                false,
            )
            .await,
        );
        assert_eq!(out.code, REFUSED);
        let v: serde_json::Value = serde_json::from_str(&out.line)?;
        assert_eq!(v["status"], "refused");
        assert!(
            v["error"].as_str().unwrap().contains("hash"),
            "the refusal names the pin: {}",
            v["error"]
        );
        let s = eidolon_core::Session::open(&session)?;
        assert!(
            answer_records(&s).iter().all(|(_, _, a)| a.is_none()),
            "a refused resume records no answer"
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_answer_with_nothing_parked_is_refused() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = plugin(tmp.path());
        vouch_fixture(&cfg(tmp.path()), &dir);
        let out = start(&cfg(tmp.path()), run_req(dir.clone(), "main", tmp.path()), false).await?;
        assert_eq!(out.code, COMPLETED);
        let v: serde_json::Value = serde_json::from_str(&out.line)?;
        let session = PathBuf::from(v["session"].as_str().unwrap());
        let run = v["run"].as_u64().unwrap();
        let out = door(
            resume(
                &cfg(tmp.path()),
                ResumeRequest {
                    session,
                    run,
                    plugin: dir,
                    answer: Some("late".into()),
                    provider: Some("mock".into()),
                    cwd: None,
                },
                false,
            )
            .await,
        );
        assert_eq!(out.code, REFUSED);
        let v: serde_json::Value = serde_json::from_str(&out.line)?;
        assert!(
            v["error"].as_str().unwrap().contains("no unanswered park"),
            "the refusal says what was missing: {}",
            v["error"]
        );
        Ok(())
    }

    /// A plugin cannot shadow anything at all, and that is now structural
    /// rather than enforced: every verb carries its own plugin's prefix, so
    /// a tool file declaring a built-in's name lands as `<plugin>_read` and
    /// the built-in's `read` is untouched. What used to be a refusal is now
    /// an impossibility — the run completes, and the plugin's own verb is
    /// there under its own name.
    #[tokio::test]
    async fn a_plugin_cannot_shadow_a_built_in() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = plugin(tmp.path());
        std::fs::write(dir.join("tools").join("read.rn"), SHADOW)?;
        vouch_fixture(&cfg(tmp.path()), &dir);
        // Registered under the layout's name, attributed, and the declared
        // `read` is nowhere: one tool has one name.
        let host = Host::new(tmp.path().to_path_buf());
        let mut reg = ToolRegistry::new();
        register_plugin_tools(&mut reg, &host, &dir, "plugin")?;
        assert!(reg.get("plugin_read").is_some(), "the verb takes the plugin's prefix");
        assert!(reg.get("read").is_none(), "and never a built-in's name");
        // The run itself is unaffected by the file: `main` calls its own
        // verb and completes.
        let out = door(start(&cfg(tmp.path()), run_req(dir, "main", tmp.path()), false).await);
        assert_eq!(out.code, COMPLETED);
        Ok(())
    }

    /// The installed plugin the door is handed: adoption registered its
    /// tools at session build, and the door's compile of the same files
    /// lands on the same name **and the same attribution** — same name and
    /// same plugin is the same tool twice, not a conflict, and adoption's
    /// copy keeps the seat.
    #[test]
    fn the_door_rerun_of_an_installed_plugin_is_not_a_collision() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = plugin(tmp.path());
        vouch_fixture(&cfg(tmp.path()), &dir);
        std::fs::write(dir.join("tools").join("ping.rn"), NAMESPACED)?;
        let host = Host::new(tmp.path().to_path_buf());
        // What adoption does for the installed `plugin/`: the namespaced
        // name, attributed to the directory — `register_plugins`'s register
        // step, without the walk.
        let src = std::fs::read_to_string(dir.join("tools").join("ping.rn"))?;
        let mut adopted = ScriptTool::compile("plugin_ping", &src, host.clone())?;
        adopted.attribute("plugin_ping", "plugin");
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(adopted));
        register_plugin_tools(&mut reg, &host, &dir, "plugin")?;
        let kept = reg.get("plugin_ping").context("the tool survives the door")?;
        assert_eq!(
            kept.manifest().plugin.as_deref(),
            Some("plugin"),
            "the seat stays adoption's attributed copy"
        );
        Ok(())
    }

    /// **The door's own registration answers on its own.** A plugin
    /// directory the operator names directly — not installed under the
    /// plugins directory, so adoption never ran — must register its tools
    /// namespaced and attributed, or a vouch can never match what the gate
    /// is asked about. The fixture declares the *bare* name on purpose:
    /// the layout names the contribution, and a declaration that disagrees
    /// does not win.
    #[test]
    fn the_doors_own_copy_is_namespaced_and_attributed() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join("scratch");
        std::fs::create_dir_all(dir.join("tools"))?;
        std::fs::write(dir.join("tools").join("echo.rn"), ECHO)?;
        let host = Host::new(tmp.path().to_path_buf());
        let mut reg = ToolRegistry::new();
        register_plugin_tools(&mut reg, &host, &dir, "scratch")?;
        let tool = reg
            .get("scratch_echo")
            .context("the verb is registered under the name the vouchers use")?;
        assert_eq!(
            tool.manifest().plugin.as_deref(),
            Some("scratch"),
            "and it carries the plugin, which is half of what a vouch matches on"
        );
        assert!(
            reg.get("echo").is_none(),
            "the declared name does not win: one tool has one name"
        );
        Ok(())
    }

    /// **A symlinked plugin directory keeps the name its entry has.** The
    /// directory is canonical for reading and never for naming, which is
    /// the difference between a run that can be vouched and one refused as
    /// `browser-0.1.0` — a plugin nobody can `plugins trust`.
    #[test]
    fn a_symlinked_plugin_directory_keeps_the_name_it_was_asked_for() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let store = tmp.path().join("store").join("browser-0.1.0");
        std::fs::create_dir_all(store.join("workflows"))?;
        std::fs::write(store.join("workflows").join("main.rn"), MAIN)?;
        let entry = tmp.path().join("plugins").join("browser");
        std::fs::create_dir_all(entry.parent().unwrap())?;
        std::os::unix::fs::symlink(&store, &entry)?;
        let program = Program::load(&entry, "main")?;
        assert_eq!(program.plugin, "browser", "the entry's name, not the target's");
        assert_eq!(
            program.dir,
            store.canonicalize()?,
            "and the reads still go through the canonical directory"
        );
        Ok(())
    }

    /// The operator's own tool holding the name a plugin's verb would take
    /// is the collision that can still happen — and now it has a second
    /// cause worth knowing about: the flat install this plugin's README
    /// used to describe leaves `browser_open.rn` in the operator's
    /// directory, and the plugin's `browser_open` then lands on it. The
    /// refusal names the name; who holds it is the operator's own copy.
    #[test]
    fn the_operators_tool_holding_the_verb_is_refused_by_name() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = plugin(tmp.path());
        vouch_fixture(&cfg(tmp.path()), &dir);
        std::fs::write(dir.join("tools").join("ping.rn"), NAMESPACED)?;
        let host = Host::new(tmp.path().to_path_buf());
        // A tool of the operator's own — compiled and registered with no
        // attribution, which is what `register_dir` does for that directory.
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ScriptTool::compile("plugin_ping", NAMESPACED, host.clone())?));
        let err = register_plugin_tools(&mut reg, &host, &dir, "plugin")
            .expect_err("a plugin never shadows the operator's own");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("`plugin_ping`") && msg.contains("already registered"),
            "the refusal names the verb: {msg}"
        );
        Ok(())
    }

    /// Two plugins cannot collide, and this is why the design says so: the
    /// name carries its own plugin's prefix, so the same file in another
    /// directory is another verb. The declaration is identical in both, and
    /// the layout is what decides.
    #[test]
    fn two_plugins_shipping_the_same_file_do_not_collide() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let one = tmp.path().join("one");
        let two = tmp.path().join("two");
        for dir in [&one, &two] {
            std::fs::create_dir_all(dir.join("tools"))?;
            std::fs::write(dir.join("tools").join("ping.rn"), NAMESPACED)?;
        }
        let host = Host::new(tmp.path().to_path_buf());
        let mut reg = ToolRegistry::new();
        register_plugin_tools(&mut reg, &host, &one, "one")?;
        register_plugin_tools(&mut reg, &host, &two, "two")?;
        for (verb, plugin) in [("one_ping", "one"), ("two_ping", "two")] {
            let tool = reg.get(verb).with_context(|| format!("{verb} registered"))?;
            assert_eq!(tool.manifest().plugin.as_deref(), Some(plugin), "{verb}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_workflow_that_does_not_compile_is_refused_before_a_session_exists() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = plugin(tmp.path());
        vouch_fixture(&cfg(tmp.path()), &dir);
        std::fs::write(dir.join("workflows").join("broken.rn"), "pub async fn run(args) { eidolon::fs_write(\"x\", \"no\") }")?;
        let sessions = tmp.path().join("sessions");
        let out = door(start(&cfg(tmp.path()), run_req(dir, "broken", tmp.path()), false).await);
        assert_eq!(out.code, REFUSED);
        assert!(
            !sessions.exists() || std::fs::read_dir(&sessions).map(|d| d.count()).unwrap_or(0) == 0,
            "the refusal cost no session log"
        );
        Ok(())
    }
}
