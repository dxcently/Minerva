//! The workflow interpreter, against the spec's test list. The fixture is
//! the shape the spec asks for: a workflow compiled from a temp dir's
//! source set, a real side effect (a line appended to a file) as the proof
//! that a resume does **not** re-fire it, and every refusal from the list
//! exercised by name. The model face's input-building and thread tests run
//! against a fake registry tool speaking C1's envelope contract — the
//! shipped mock never returns text on a first turn, so a real exchange is
//! always `ok:false` and the thread would have nothing to rebuild; the
//! real-tool tests below cover what only the real builtin and completion
//! path can: the scope gate, the verbatim real envelope, and the spend
//! paired with the run's own step id.

use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use eidolon_core::dispatch::Dispatcher;
use eidolon_core::message::Json;
use eidolon_core::session::{Record, RecordKind};
use eidolon_core::{RunPin, RunStatus};
use eidolon_core::tool::{CallContext, Tool, ToolManifest, ToolOutput};
use eidolon_core::user::{Choice, NoUser, UserIo};
use eidolon_core::*;

use eidolon_rune::{Host, RunOutcome, ScriptTool, Workflow, register_builtins};

/// The fixture workflow: `times` steps, each a real side effect. Its source
/// is a `const` so the changed-program test can derive a variant from it.
const TRIAGE: &str = r#"
pub fn workflow() {
    #{
        name: "triage",
        description: "walks the fixture tool",
        approval: "read_only",
        params: #{},
    }
}

pub async fn run(args) {
    let last = "";
    for i in 0..args.times {
        last = eidolon::tool_call("recorder", #{ note: "pass" }).await?;
    }
    last
}
"#;

/// The cap the plugin lowers for itself: three steps declared, four asked
/// for — the run stops at the declared cap, naming it.
const OVER_EAGER: &str = r#"
pub fn workflow() {
    #{ name: "over-eager", description: "asks for four", step_cap: 3 }
}

pub async fn run(args) {
    for i in 0..4 {
        eidolon::tool_call("recorder", #{ note: "pass" }).await?;
    }
    "done"
}
"#;

/// No declared cap: the harness's 200 is the ceiling, and a walk past it
/// fails naming that number rather than walking until the heat death.
const ENDLESS: &str = r#"
pub fn workflow() {
    #{ name: "endless", description: "asks for 250" }
}

pub async fn run(args) {
    for i in 0..250 {
        eidolon::tool_call("recorder", #{ note: "pass" }).await?;
    }
    "done"
}
"#;

/// The scope's refusal is a compile error: `fs_write` is not a name the
/// workflow context registers, and nothing at run time is consulted.
const OUT_OF_SCOPE: &str = r#"
pub fn workflow() {
    #{ name: "out-of-scope", description: "reaches past the scope" }
}

pub async fn run(args) {
    eidolon::fs_write("x.txt", "no").await
}
"#;

/// The fixture tool: appends a line to `effects.log` in the working
/// directory. The file is the testimony — a resume that re-fired a step
/// would grow it, and the whole point of the memo is that it does not.
struct Recorder(ToolManifest);

#[async_trait]
impl Tool for Recorder {
    fn manifest(&self) -> &ToolManifest {
        &self.0
    }
    async fn call(
        &self,
        _: serde_json::Value,
        ctx: CallContext,
    ) -> anyhow::Result<ToolOutput> {
        let path = ctx.cwd.join("effects.log");
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("opening {}", path.display()))?;
            f.write_all(b"effect\n")
                .with_context(|| format!("appending {}", path.display()))?;
        }
        Ok(ToolOutput::ok("effect appended"))
    }
}

impl Recorder {
    fn manifest() -> ToolManifest {
        ToolManifest {
            name: "recorder".into(),
            description: "appends a line to effects.log and says so".into(),
            input_schema: json!({"type":"object"}),
            approval: eidolon_core::policy::Approval::ReadOnly,
            prompt: None,
            render: None,
            deferred: false,
            script_only: false,
            plugin: None,
            source_hash: None,
        }
    }
    fn tool() -> Arc<dyn Tool> {
        Arc::new(Recorder(Self::manifest()))
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    host: Arc<Host>,
    d: Arc<Dispatcher>,
    session: Arc<AsyncMutex<Session>>,
}

fn rig(src: &str, lib: &[String]) -> anyhow::Result<(Rig, Arc<Workflow>)> {
    rig_with(&[Recorder::tool()], false, src, lib)
}

/// The same rig with tools registered beside the recorder, and — when
/// `completion` — the catalog the model face's scope gate asks for. The
/// fake's tests pass `true` so the verb compiles; the tool under it is
/// still the fake, so the face's input-building and thread tests stay
/// deterministic and provider-free.
fn rig_with(
    tools: &[Arc<dyn Tool>],
    completion: bool,
    src: &str,
    lib: &[String],
) -> anyhow::Result<(Rig, Arc<Workflow>)> {
    rig_as(tools, completion, src, lib, Arc::new(NoUser))
}

/// The same rig with the session's io named: the seam an inline park asks
/// through, and the whole difference between an attended run and a
/// headless one. `NoUser` is the default `rig_with` hands over, which is
/// why every test written before the delivery door still reads it.
fn rig_as(
    tools: &[Arc<dyn Tool>],
    completion: bool,
    src: &str,
    lib: &[String],
    user: Arc<dyn UserIo>,
) -> anyhow::Result<(Rig, Arc<Workflow>)> {
    let dir = tempfile::tempdir()?;
    let host = Host::new(dir.path().to_path_buf());
    if completion {
        host.attach_completion(eidolon_rune::host::Completion {
            catalog: Arc::new(eidolon_providers::Catalog::new()),
            default_model: "mock".into(),
            max_tokens: 1024,
        });
    }
    let wf = Workflow::compile(&host, "fixture", "0.1.0", "triage", src, lib)?;
    let mut reg = ToolRegistry::new();
    reg.register(Recorder::tool());
    for tool in tools {
        reg.register(tool.clone());
    }
    let session = Arc::new(AsyncMutex::new(Session::create(
        &dir.path().join("run.eid"),
        "prov:m",
        dir.path(),
        None,
    )?));
    let bus = EventBus::default();
    let d = Arc::new(Dispatcher::new(
        reg,
        Arc::new(policy::AllowAll),
        user,
        bus,
        session.clone(),
        dir.path().to_path_buf(),
    ));
    host.attach(&d);
    Ok((
        Rig {
            _dir: dir,
            host,
            d,
            session,
        },
        wf,
    ))
}

fn effects(rig: &Rig) -> usize {
    let path = rig._dir.path().join("effects.log");
    std::fs::read_to_string(path)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// The branch's `ToolResult`s, as (call id, content).
async fn results(rig: &Rig) -> Vec<(String, String)> {
    rig.session
        .lock()
        .await
        .branch()
        .into_iter()
        .filter_map(|rec| match &rec.kind {
            RecordKind::ToolResult {
                tool_use_id,
                content,
                ..
            } => Some((tool_use_id.clone(), content.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_run_journals_its_pin_its_steps_and_its_ending() -> anyhow::Result<()> {
    let (rig, wf) = rig(TRIAGE, &[])?;
    let outcome = wf
        .start(&rig.d, Json("{\"times\":2}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("expected a completed run: {outcome:?}");
    };
    assert_eq!(report.content, "effect appended");
    assert_eq!(effects(&rig), 2, "two steps, two real effects");

    // The journal holds the whole run: pin, two steps under the run's own
    // ids, the results paired by them, and the ending.
    let session = rig.session.lock().await;
    let runs = session.workflow_runs();
    assert_eq!(runs.len(), 1);
    let back = &runs[0];
    assert_eq!(back.pin.plugin, "fixture");
    assert_eq!(back.pin.workflow, "triage");
    assert_eq!(back.pin.hash, wf.hash());
    assert_eq!(back.end, Some(RunStatus::Completed));
    assert_eq!(back.steps.len(), 2);
    for (i, step) in back.steps.iter().enumerate() {
        assert_eq!(step.tool, "recorder");
        assert_eq!(step.input.0, "{\"note\":\"pass\"}");
        let result = step.result.as_ref().expect("every step's answer is here");
        assert_eq!(result.content, "effect appended");
        assert_eq!(step.call_id, format!("w{}-{i}", back.id));
    }
    Ok(())
}

/// The spec's proof: resume is replay, and the file is the witness. Every
/// step's answer is already on the branch, so nothing runs, the effect file
/// does not grow, and the resumed run appends its own ending.
#[tokio::test]
async fn a_resume_takes_its_answers_from_the_log_and_re_fires_nothing() -> anyhow::Result<()> {
    let (rig, wf) = rig(TRIAGE, &[])?;
    wf.start(&rig.d, Json("{\"times\":2}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    assert_eq!(effects(&rig), 2);
    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;

    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("expected a completed resume: {outcome:?}");
    };
    assert_eq!(report.content, "effect appended");
    assert_eq!(effects(&rig), 2, "the resume re-fired nothing");

    // The latest ending wins, and the answers are the journal's.
    let back = rig.session.lock().await.workflow_run(run).unwrap();
    assert_eq!(back.end, Some(RunStatus::Completed));
    assert!(back.steps.iter().all(|s| s.result.is_some()));
    let results = results(&rig).await;
    assert_eq!(
        results.iter().filter(|(id, _)| id.ends_with("-0")).count(),
        1,
        "one answer per step id, not one per pass: {results:?}"
    );
    Ok(())
}

/// A program that is not the one the run started under is refused by hash —
/// the pin exists so a replay never walks into a changed program.
#[tokio::test]
async fn a_resume_refuses_a_program_whose_hash_differs() -> anyhow::Result<()> {
    let (rig, wf) = rig(TRIAGE, &[])?;
    wf.start(&rig.d, Json("{\"times\":2}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;

    let changed = TRIAGE.replace("args.times", "3");
    assert_ne!(changed, TRIAGE);
    let other = Workflow::compile(&rig.host, "fixture", "0.1.0", "triage", &changed, &[])?;
    let err = other
        .resume(&rig.d, 1, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("hash") && format!("{err:#}").contains("refusing"),
        "{err:#}"
    );
    assert_eq!(effects(&rig), 2, "the refusal happened before any step");
    Ok(())
}

/// The same program asking something different at a recorded index is a
/// refusal, not a memo hit: the journal pairs an answer with the request
/// that earned it, and a diverged pass gets neither.
#[tokio::test]
async fn a_resume_refuses_a_step_whose_recorded_request_differs() -> anyhow::Result<()> {
    let (rig, wf) = rig(TRIAGE, &[])?;
    let run = rig
        .session
        .lock()
        .await
        .start_workflow(RunPin {
            plugin: "fixture".into(),
            workflow: "triage".into(),
            version: "0.1.0".into(),
            hash: wf.hash().to_string(),
            args: Json("{\"times\":2}".into()),
        })?;
    // The journal's step 0 asks something this pass never would.
    rig.session.lock().await.workflow_step(
        run,
        0,
        &format!("w{run}-0"),
        "recorder",
        Json("{\"note\":\"a different question\"}".into()),
    )?;

    // A mismatch is discovered mid-run — the program asked — so it is the
    // run failing at a step, not a resume that never began: the outcome is
    // `Failed` with the reason, and an ending is journaled over it.
    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    let RunOutcome::Failed { reason } = &outcome else {
        anyhow::bail!("expected the diverged pass to fail: {outcome:?}");
    };
    assert!(reason.contains("replay mismatch at step 0"), "{reason}");
    assert_eq!(effects(&rig), 0, "the mismatched step never ran");
    let back = rig.session.lock().await.workflow_run(run).unwrap();
    assert_eq!(back.end, Some(RunStatus::Failed));
    Ok(())
}

/// A hole is a refusal and never a re-run: index 1's record is absent, and
/// walking past it would dispatch index 1 again.
#[tokio::test]
async fn a_resume_refuses_a_gap() -> anyhow::Result<()> {
    let (rig, wf) = rig(TRIAGE, &[])?;
    let run = rig
        .session
        .lock()
        .await
        .start_workflow(RunPin {
            plugin: "fixture".into(),
            workflow: "triage".into(),
            version: "0.1.0".into(),
            hash: wf.hash().to_string(),
            args: Json("{\"times\":2}".into()),
        })?;
    {
        let mut s = rig.session.lock().await;
        for index in [0u32, 2] {
            s.workflow_step(
                run,
                index,
                &format!("w{run}-{index}"),
                "recorder",
                Json("{\"note\":\"pass\"}".into()),
            )?;
            s.append(RecordKind::ToolResult {
                tool_use_id: format!("w{run}-{index}"),
                content: "effect appended".into(),
                is_error: false,
            })?;
        }
    }
    let err = wf
        .resume(&rig.d, run, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("step 1"), "{err:#}");
    assert_eq!(effects(&rig), 0);
    Ok(())
}

/// A run that claimed finality over an answer it never got is refused — the
/// tail check is the interpreter's, made here where the ending is read. The
/// parked twin of this shape is the one shape that *is* replayable, and its
/// tail re-dispatches under the recorded id: the effect fires, this pass and
/// only this pass.
#[tokio::test]
async fn an_ended_run_with_an_unanswered_tail_is_refused_but_a_parked_one_resumes() ->
anyhow::Result<()> {
    let (rig, wf) = rig(TRIAGE, &[])?;
    let pin = RunPin {
        plugin: "fixture".into(),
        workflow: "triage".into(),
        version: "0.1.0".into(),
        hash: wf.hash().to_string(),
        args: Json("{\"times\":2}".into()),
    };
    let (ended, parked) = {
        let mut s = rig.session.lock().await;
        let ended = s.start_workflow(pin.clone())?;
        s.workflow_step(ended, 0, &format!("w{ended}-0"), "recorder", Json("{\"note\":\"pass\"}".into()))?;
        s.append(RecordKind::ToolResult {
            tool_use_id: format!("w{ended}-0"),
            content: "effect appended".into(),
            is_error: false,
        })?;
        s.workflow_step(ended, 1, &format!("w{ended}-1"), "recorder", Json("{\"note\":\"pass\"}".into()))?;
        s.end_workflow(ended, RunStatus::Completed)?;

        let parked = s.start_workflow(pin)?;
        s.workflow_step(parked, 0, &format!("w{parked}-0"), "recorder", Json("{\"note\":\"pass\"}".into()))?;
        s.append(RecordKind::ToolResult {
            tool_use_id: format!("w{parked}-0"),
            content: "effect appended".into(),
            is_error: false,
        })?;
        s.workflow_step(parked, 1, &format!("w{parked}-1"), "recorder", Json("{\"note\":\"pass\"}".into()))?;
        s.end_workflow(parked, RunStatus::Parked)?;
        (ended, parked)
    };
    assert_eq!(effects(&rig), 0, "neither tail ever ran");

    let err = wf
        .resume(&rig.d, ended, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("unanswered"), "{err:#}");
    assert_eq!(effects(&rig), 0);

    let outcome = wf.resume(&rig.d, parked, CancellationToken::new()).await?;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("expected the parked run to complete: {outcome:?}");
    };
    assert_eq!(report.content, "effect appended");
    assert_eq!(effects(&rig), 1, "the parked tail fired exactly once");
    let back = rig.session.lock().await.workflow_run(parked).unwrap();
    assert_eq!(back.end, Some(RunStatus::Completed), "the latest ending wins");
    Ok(())
}

/// The cap a plugin lowers for itself stops the walk with the run failed and
/// the reason named.
#[tokio::test]
async fn a_run_stops_at_the_cap_the_workflow_declared() -> anyhow::Result<()> {
    let (rig, wf) = rig(OVER_EAGER, &[])?;
    let outcome = wf
        .start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let RunOutcome::Failed { reason } = &outcome else {
        anyhow::bail!("expected a failed run: {outcome:?}");
    };
    assert!(reason.contains("step cap (3)"), "{reason}");
    assert_eq!(effects(&rig), 3, "the three steps inside the cap ran");
    let back = rig.session.lock().await.workflow_runs().pop().unwrap();
    assert_eq!(back.end, Some(RunStatus::Failed));
    Ok(())
}

/// The harness cap is the ceiling a `workflow()` cannot raise: 250 asks,
/// 200 steps, and the run fails naming the harness's number.
#[tokio::test]
async fn the_harness_cap_is_the_ceiling() -> anyhow::Result<()> {
    let (rig, wf) = rig(ENDLESS, &[])?;
    let outcome = wf
        .start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let RunOutcome::Failed { reason } = &outcome else {
        anyhow::bail!("expected a failed run: {outcome:?}");
    };
    assert!(reason.contains("step cap (200)"), "{reason}");
    assert_eq!(effects(&rig), 200);
    Ok(())
}

/// The scope's refusal is a compile error: `fs_write` is not in the
/// workflow context, and no policy question is ever asked.
#[test]
fn a_workflow_that_reaches_past_its_scope_does_not_compile() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::new(dir.path().to_path_buf());
    let err = match Workflow::compile(&host, "fixture", "0.1.0", "out-of-scope", OUT_OF_SCOPE, &[])
    {
        Ok(_) => panic!("the out-of-scope workflow is refused"),
        Err(e) => e,
    };
    assert!(
        format!("{err:#}").contains("fs_write"),
        "the error names the primitive the scope does not carry: {err:#}"
    );
}

/// The lib files compile in front of the workflow — one unit sharing a
/// namespace — and the hash covers them: a different lib is a different
/// program, which is what a resume refuses by.
#[tokio::test]
async fn lib_files_are_in_front_of_the_workflow_and_in_its_hash() -> anyhow::Result<()> {
    let lib = vec![
        "pub fn greeting() { \"hello\" }".to_string(),
    ];
    let with_lib = r#"
pub fn workflow() {
    #{ name: "triage", description: "uses the lib" }
}

pub async fn run(args) {
    eidolon::tool_call("recorder", #{ note: greeting() }).await
}
"#;
    let (rig, wf) = rig(with_lib, &lib)?;
    let outcome = wf
        .start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("expected a completed run: {outcome:?}");
    };
    assert_eq!(report.content, "effect appended");
    let back = rig.session.lock().await.workflow_runs().pop().unwrap();
    assert_eq!(
        back.steps[0].input.0,
        "{\"note\":\"hello\"}",
        "the lib's helper ran in front of the program"
    );

    // A different lib, same workflow file: a different hash.
    let other_lib = vec!["pub fn greeting() { \"goodbye\" }".to_string()];
    let other = Workflow::compile(&rig.host, "fixture", "0.1.0", "triage", with_lib, &other_lib)?;
    assert_ne!(other.hash(), wf.hash(), "the hash covers the lib");
    Ok(())
}

// --- park: run-level, memo-shaped, not an effect ---------------------------

/// A workflow that does one effect, parks on a person, and does another
/// effect with the answer — the whole reason parking is a verb the script
/// calls and not a return shape: the second pass has an answer to read.
const ASKER: &str = r#"
pub fn workflow() {
    #{ name: "asker", description: "parks on a person" }
}

pub async fn run(args) {
    eidolon::tool_call("recorder", #{ note: "before" }).await?;
    let answer = eidolon::park("Ship it?").await?;
    eidolon::tool_call("recorder", #{ note: answer }).await?;
    "done"
}
"#;

/// The question's records, for the run: (call id, prompt, answer).
async fn parks(rig: &Rig) -> Vec<(String, String, Option<String>)> {
    rig.session
        .lock()
        .await
        .branch()
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

/// A park stops the run: the effect after the park line never fires, the
/// question is journaled under the run's own deterministic id with no
/// answer, the ending is `Parked`, and the question is what the driving
/// call receives — a parked run that reported success would be a lie.
#[tokio::test]
async fn a_park_stops_the_run_and_journals_the_question() -> anyhow::Result<()> {
    let (rig, wf) = rig(ASKER, &[])?;
    let outcome = wf
        .start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let RunOutcome::Parked { question } = &outcome else {
        anyhow::bail!("expected a parked run: {outcome:?}");
    };
    assert_eq!(question, "Ship it?");
    assert_eq!(effects(&rig), 1, "the effect before the park, and only it");

    let ps = parks(&rig).await;
    assert_eq!(ps.len(), 1);
    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;
    assert_eq!(ps[0].0, format!("w{run}-park0"), "the run's own id, deterministic");
    assert_eq!(ps[0].1, "Ship it?");
    assert_eq!(ps[0].2, None, "no answer yet — the journal says so, not a guess");

    let back = rig.session.lock().await.workflow_run(run).unwrap();
    assert_eq!(back.end, Some(RunStatus::Parked));
    assert_eq!(back.steps.len(), 1, "one step journaled before the park");
    Ok(())
}

/// A resume whose person has not answered parks again identically: the
/// question is not re-journaled — it is already on the branch — and the
/// effects still stop where they stopped.
#[tokio::test]
async fn a_resume_of_an_unanswered_park_parks_again_and_journals_nothing_new() -> anyhow::Result<()>
{
    let (rig, wf) = rig(ASKER, &[])?;
    wf.start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;

    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    let RunOutcome::Parked { question } = &outcome else {
        anyhow::bail!("expected a parked resume: {outcome:?}");
    };
    assert_eq!(question, "Ship it?", "the same question, still standing");
    assert_eq!(effects(&rig), 1, "nothing re-fired");
    assert_eq!(parks(&rig).await.len(), 1, "the question is not written twice");
    Ok(())
}

/// The answer arrives as a journaled `AskUser` under the same call id, and
/// the resume returns it to the script at the park line: the run continues,
/// the second effect carries the answer, and the ending is `Completed` —
/// appended, not edited. This is memo-shaped: the park line is answered
/// from the log, the way a step is.
#[tokio::test]
async fn a_resume_with_the_answer_recorded_continues_past_the_park() -> anyhow::Result<()> {
    let (rig, wf) = rig(ASKER, &[])?;
    wf.start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;
    let park_id = format!("w{run}-park0");

    // The delivery door is the design's open fork; whoever lands it, this
    // is what it leaves in the log — the answer under the park's own id.
    rig.session.lock().await.append(RecordKind::AskUser {
        call_id: park_id,
        prompt: "Ship it?".into(),
        answer: Some("yes".into()),
    })?;

    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("expected the run to complete past its park: {outcome:?}");
    };
    assert_eq!(report.content, "done");
    assert_eq!(effects(&rig), 2, "the effect after the park fired, once");

    // The answer flowed into the program: the second step asked with it.
    let back = rig.session.lock().await.workflow_run(run).unwrap();
    assert_eq!(back.end, Some(RunStatus::Completed));
    assert_eq!(back.steps.len(), 2);
    assert_eq!(back.steps[1].input.0, "{\"note\":\"yes\"}");
    assert_eq!(back.steps[1].result.as_ref().unwrap().content, "effect appended");
    Ok(())
}

/// A person at the dialog: answers the first question put to them, then
/// declines the rest — the shape "attended" takes in a test. Everything a
/// real io contributes is the `Some`/`None` of `respond`; `NoUser` is
/// this with only the decline.
struct Person(std::sync::Mutex<Option<&'static str>>);

#[async_trait]
impl UserIo for Person {
    async fn choose(&self, _: &str, _: &[Choice], _: &CancellationToken) -> Option<Reply> {
        None
    }
    async fn respond(&self, _prompt: &str, _cancel: &CancellationToken) -> Option<String> {
        self.0.lock().unwrap().take().map(str::to_string)
    }
}

/// The workflow facts a branch holds, as comparable lines: what the run
/// was, what it asked, what it did, what it answered, how it ended. The
/// shape an inline answer and a resume answer must produce identically.
fn describe(rec: &Record) -> Option<String> {
    Some(match &rec.kind {
        RecordKind::WorkflowStart {
            plugin,
            workflow,
            version,
            hash,
            args,
        } => format!("start {plugin}/{workflow}/{version}/{hash} {}", args.0),
        RecordKind::WorkflowStep {
            run,
            index,
            call_id,
            tool,
            input,
        } => format!("step {run} {index} {call_id} {tool} {}", input.0),
        RecordKind::WorkflowEnd { run, status } => format!("end {run} {status:?}"),
        RecordKind::AskUser {
            call_id,
            prompt,
            answer,
        } => format!("ask {call_id} {prompt} {:?}", answer.as_deref()),
        RecordKind::ToolResult {
            tool_use_id,
            content,
            ..
        } if tool_use_id.starts_with('w') => format!("result {tool_use_id} {content}"),
        _ => return None,
    })
}

/// Ruling 1, the attended half: a person who is there answers inline and
/// the run continues past the park — no parked ending, no second pass.
/// The journal is the resume door's own shape: question, then answer
/// under the park's id.
#[tokio::test]
async fn an_attended_park_asks_inline_and_the_run_continues() -> anyhow::Result<()> {
    let person = Arc::new(Person(std::sync::Mutex::new(Some("yes"))));
    let io: Arc<dyn UserIo> = person.clone();
    let (rig, wf) = rig_as(&[], false, ASKER, &[], io)?;
    let outcome = wf
        .start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("expected the run to continue past its park: {outcome:?}");
    };
    assert_eq!(report.content, "done");
    assert_eq!(
        effects(&rig),
        2,
        "the effect after the park fired, in the same pass"
    );

    let ps = parks(&rig).await;
    assert_eq!(
        ps.len(),
        2,
        "question, then answer — the resume door's own shape"
    );
    assert_eq!(ps[0].2, None);
    assert_eq!(ps[1].0, ps[0].0, "the answer rides the park's own id");
    assert_eq!(ps[1].2.as_deref(), Some("yes"));

    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;
    let back = rig.session.lock().await.workflow_run(run).unwrap();
    assert_eq!(back.end, Some(RunStatus::Completed));
    assert_eq!(back.steps.len(), 2);
    assert_eq!(back.steps[1].input.0, "{\"note\":\"yes\"}");
    Ok(())
}

/// The strongest form of the no-divergence claim: a run answered inline
/// and a run parked-then-resumed reach the same state — the same records
/// in the same order with the same ending, and the only difference is the
/// `WorkflowEnd(Parked)` that answering removes. Anything else that
/// differs would be replay reading two shapes where the harness promised
/// one.
#[tokio::test]
async fn an_inline_answer_and_a_resume_reach_the_same_log() -> anyhow::Result<()> {
    // Run A: a person is there; one pass completes it.
    let (a, wfa) = rig_as(
        &[],
        false,
        ASKER,
        &[],
        Arc::new(Person(std::sync::Mutex::new(Some("yes")))),
    )?;
    wfa.start(&a.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;

    // Run B: nobody is there; it parks; the answer arrives the way the
    // resume door records it; the resume completes it.
    let (b, wfb) = rig(ASKER, &[])?;
    wfb.start(&b.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let run = b.session.lock().await.workflow_runs().pop().unwrap().id;
    b.session.lock().await.append(RecordKind::AskUser {
        call_id: format!("w{run}-park0"),
        prompt: "Ship it?".into(),
        answer: Some("yes".into()),
    })?;
    let outcome = wfb.resume(&b.d, run, CancellationToken::new()).await?;
    assert!(
        matches!(outcome, RunOutcome::Completed { .. }),
        "B completes on its resume"
    );

    let answered = {
        let s = a.session.lock().await;
        s.branch().into_iter().filter_map(describe).collect::<Vec<_>>()
    };
    let mut resumed = {
        let s = b.session.lock().await;
        s.branch().into_iter().filter_map(describe).collect::<Vec<_>>()
    };
    let parked = resumed.iter().filter(|r| r.ends_with("Parked")).count();
    assert_eq!(
        parked, 1,
        "B's only extra is the Parked ending itself: {resumed:?}"
    );
    resumed.retain(|r| !r.ends_with("Parked"));
    assert_eq!(
        answered, resumed,
        "same records, same order, same ending — bar the Parked"
    );
    Ok(())
}

/// Present but declining: a person who dismisses the question has not
/// answered it, and the run parks exactly as it does for nobody at all —
/// same journal shape, same ending, `resume --answer` still the
/// mechanism. A decline is not an answer, and it journals nothing new.
#[tokio::test]
async fn a_present_person_who_declines_parks_as_if_absent() -> anyhow::Result<()> {
    let (rig, wf) = rig_as(
        &[],
        false,
        ASKER,
        &[],
        Arc::new(Person(std::sync::Mutex::new(None))),
    )?;
    let outcome = wf
        .start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    assert!(
        matches!(&outcome, RunOutcome::Parked { question } if question == "Ship it?"),
        "a decline parks: {outcome:?}"
    );
    assert_eq!(effects(&rig), 1, "nothing past the park fired");
    let ps = parks(&rig).await;
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0].2, None);
    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;
    assert_eq!(
        rig.session
            .lock()
            .await
            .workflow_run(run)
            .unwrap()
            .end,
        Some(RunStatus::Parked)
    );
    Ok(())
}

/// The same park, a later pass, a person now there: the unanswered
/// question is asked inline instead of re-parked — the resume that needs
/// no `--answer`. The journal is the door's own: question unanswered,
/// then answered under the same id, then the run past it.
#[tokio::test]
async fn a_resume_pass_with_a_person_asks_instead_of_parking() -> anyhow::Result<()> {
    let person = Arc::new(Person(std::sync::Mutex::new(None)));
    let io: Arc<dyn UserIo> = person.clone();
    let (rig, wf) = rig_as(&[], false, ASKER, &[], io)?;
    wf.start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;
    assert_eq!(
        rig.session
            .lock()
            .await
            .workflow_run(run)
            .unwrap()
            .end,
        Some(RunStatus::Parked)
    );

    *person.0.lock().unwrap() = Some("yes");
    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    assert!(
        matches!(&outcome, RunOutcome::Completed { report } if report.content == "done"),
        "the answered resume completes: {outcome:?}"
    );
    assert_eq!(effects(&rig), 2);

    let ps = parks(&rig).await;
    assert_eq!(ps.len(), 2, "question, then the answer under its id");
    assert_eq!(ps[1].0, ps[0].0);
    assert_eq!(ps[1].2.as_deref(), Some("yes"));
    Ok(())
}

/// A script's api_request journals at the attempt: neither request here
/// can connect, and both records are on the branch anyway — a failed
/// request still left the machine, which is why the record goes down
/// before the egress. The service is the descriptor's name when it
/// carries one, the host when it does not; the consumer names the tool
/// whose script asked, and is not a plugin's, because this tool is
/// nobody's plugin.
#[tokio::test]
async fn an_api_request_journals_at_the_attempt_even_when_it_fails() -> anyhow::Result<()> {
    const CALLER: &str = r#"pub fn manifest() { #{ name: "caller", description: "calls out", approval: "read_only", input_schema: #{ "type": "object" } } }
pub async fn call(input) {
    let _ = eidolon::api_request(
        #{ base_url: "https://127.0.0.1:1", name: "shop" }, "GET", "/", None).await;
    let _ = eidolon::api_request(
        #{ base_url: "https://api.example.com/v1" }, "GET", "/", None).await;
    "both attempts journaled"
}"#;
    // The rig compiles workflows, and api_request is a tool-scope
    // primitive the workflow compiler rightly refuses — so this test
    // drives a ScriptTool the way the dispatcher would, over a session
    // the record can land in.
    let dir = tempfile::tempdir()?;
    let host = Host::new(dir.path().to_path_buf());
    let session = Arc::new(AsyncMutex::new(Session::create(
        &dir.path().join("s.eid"),
        "prov:m",
        dir.path(),
        None,
    )?));
    let d = Arc::new(Dispatcher::new(
        ToolRegistry::new(),
        Arc::new(policy::AllowAll),
        Arc::new(NoUser),
        EventBus::default(),
        session.clone(),
        dir.path().to_path_buf(),
    ));
    host.attach(&d);
    let tool = ScriptTool::compile("caller", CALLER, host)?;
    let out = tool
        .call(
            serde_json::json!({}),
            CallContext {
                cwd: dir.path().to_path_buf(),
                cancel: CancellationToken::new(),
                call_id: "t1".into(),
            },
        )
        .await?;
    assert_eq!(out.content, "both attempts journaled");
    let recs: Vec<(String, String)> = session
        .lock()
        .await
        .branch()
        .into_iter()
        .filter_map(|r| match &r.kind {
            RecordKind::ApiRequest {
                consumer,
                service,
            } => Some((consumer.clone(), service.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(recs.len(), 2, "two attempts, two records: {recs:?}");
    assert_eq!(recs[0].1, "shop", "a named descriptor names the service");
    assert_eq!(
        recs[1].1, "api.example.com",
        "an unnamed one answers with its host"
    );
    assert_eq!(
        recs[0].0, "caller",
        "the consumer names the tool whose script asked"
    );
    Ok(())
}

/// The same program parks on the same question: a diverged pass asking
/// something different under the same park id is the step mismatch's twin,
/// refused rather than answered from a question nobody asked.
#[tokio::test]
async fn a_resume_refuses_a_park_whose_recorded_question_differs() -> anyhow::Result<()> {
    let (rig, wf) = rig(ASKER, &[])?;
    wf.start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;

    // A different program, slipped past the pin by hand: the journal's
    // park 0 asked about shipping, this pass asks about labelling.
    rig.session
        .lock()
        .await
        .append(RecordKind::AskUser {
            call_id: format!("w{run}-park0"),
            prompt: "Label it first?".into(),
            answer: None,
        })
        .unwrap();

    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    let RunOutcome::Failed { reason } = &outcome else {
        anyhow::bail!("expected the diverged park to fail the run: {outcome:?}");
    };
    assert!(reason.contains("replay mismatch at park"), "{reason}");
    assert_eq!(effects(&rig), 1, "no effect past the mismatched park");
    Ok(())
}

// --- ask_model: the model face, the flags, and the thread -------------------

/// The fake registry tool: C1's `ask_model` contract, master's side of the
/// merge — one stateless model call answering Melete's envelope, always a
/// plain string, with nothing behind it. The reply names what it was asked
/// with, so the tests can see the thread arrive. When C lands the real
/// builtin registers under this name; the face and these tests do not move.
struct MockModel(ToolManifest);

#[async_trait]
impl Tool for MockModel {
    fn manifest(&self) -> &ToolManifest {
        &self.0
    }
    async fn call(
        &self,
        input: serde_json::Value,
        _: CallContext,
    ) -> anyhow::Result<ToolOutput> {
        let prompt = input.get("prompt").and_then(|p| p.as_str());
        let out = match prompt {
            Some(p) if !p.trim().is_empty() => {
                let turns = input
                    .get("history")
                    .and_then(|h| h.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                json!({ "ok": true, "reply": format!("mock reply to `{p}` riding {turns} turns") })
            }
            _ => json!({ "ok": false, "error": "`prompt` must be a non-empty string" }),
        };
        Ok(ToolOutput::ok(out.to_string()))
    }
}

impl MockModel {
    fn manifest() -> ToolManifest {
        ToolManifest {
            name: "ask_model".into(),
            description: "the fake: one stateless model call, Melete's envelope".into(),
            input_schema: json!({"type":"object"}),
            approval: eidolon_core::policy::Approval::ReadOnly,
            prompt: None,
            render: None,
            deferred: false,
            script_only: false,
            plugin: None,
            source_hash: None,
        }
    }
    fn tool() -> Arc<dyn Tool> {
        Arc::new(MockModel(Self::manifest()))
    }
}

/// The rig with the fake `ask_model` registered: the model face's tests.
/// The catalog is attached for the verb's scope gate only — the tool under
/// the verb is the fake.
fn model_rig(src: &str, lib: &[String]) -> anyhow::Result<(Rig, Arc<Workflow>)> {
    rig_with(&[MockModel::tool()], true, src, lib)
}

/// Two model calls on the defaults: the second rides the thread the first
/// joined — and the thread is the journal's, rebuilt, never stored twice.
const CHATTER: &str = r#"
pub fn workflow() {
    #{ name: "chatter", description: "two calls on the thread" }
}

pub async fn run(args) {
    let first = eidolon::ask_model("first question", #{}).await?;
    let second = eidolon::ask_model("second question", #{}).await?;
    format!("{first} then {second}")
}
"#;

/// All four thread corners in one walk: the defaults, neither flag,
/// consulting without joining, joining without consulting — and a final
/// defaults call whose thread shows exactly which exchanges joined.
const CORNERS: &str = r#"
pub fn workflow() {
    #{ name: "corners", description: "all four thread corners" }
}

pub async fn run(args) {
    let a = eidolon::ask_model("q0", #{}).await?;
    let b = eidolon::ask_model("q1", #{ use_history: false, remember: false }).await?;
    let c = eidolon::ask_model("q2", #{ remember: false }).await?;
    let d = eidolon::ask_model("q3", #{ use_history: false }).await?;
    let e = eidolon::ask_model("q4", #{}).await?;
    format!("{a} | {b} | {c} | {d} | {e}")
}
"#;

/// The face hands the envelope over verbatim, and the script branches on
/// `ok` — the json module sits in the scope for exactly this.
const BRANCHER: &str = r#"
pub fn workflow() {
    #{ name: "brancher", description: "branches on the envelope" }
}

pub async fn run(args) {
    let raw = eidolon::ask_model("", #{}).await?;
    let parsed = json::from_string(raw)?;
    if parsed.ok {
        "unexpectedly ok"
    } else {
        format!("the tool said no: {}", parsed.error)
    }
}
"#;

/// Each call's recorded input is the built one — prompt, both flags, and
/// the thread when it rides — so the gate sees what may carry the run's
/// context, and a resume compares the thread it rebuilds against the
/// thread the answer belonged to. The corners read: q1 rides nothing and
/// joins nothing; q2 consults the thread q0 joined and does not join it;
/// q3 joins without consulting; q4's thread holds q0 and q3 and only
/// those.
#[tokio::test]
async fn all_four_thread_corners_build_their_recorded_inputs() -> anyhow::Result<()> {
    let (rig, wf) = model_rig(CORNERS, &[])?;
    let outcome = wf
        .start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let RunOutcome::Completed { .. } = &outcome else {
        anyhow::bail!("expected a completed run: {outcome:?}");
    };
    let back = rig.session.lock().await.workflow_runs().pop().unwrap();
    assert_eq!(back.steps.len(), 5);
    assert!(back.steps.iter().all(|s| s.tool == "ask_model"));
    let input = |i: usize| -> serde_json::Value {
        serde_json::from_str(&back.steps[i].input.0).expect("the recorded input is JSON")
    };
    let exchange = |prompt: &str, reply: &str| {
        json!([
            {"role": "user", "content": prompt},
            {"role": "assistant", "content": reply},
        ])
    };
    let reply0 = "mock reply to `q0` riding 0 turns";
    let reply3 = "mock reply to `q3` riding 0 turns";

    assert_eq!(
        input(0),
        json!({"prompt": "q0", "use_history": true, "remember": true, "history": []}),
        "the defaults: the key is there with an empty thread — consulted, and empty"
    );
    assert_eq!(
        input(1),
        json!({"prompt": "q1", "use_history": false, "remember": false}),
        "neither: stateless, and off the record"
    );
    assert_eq!(
        input(2),
        json!({"prompt": "q2", "use_history": true, "remember": false,
               "history": exchange("q0", reply0)}),
        "consults the thread q0 joined, and does not join it"
    );
    assert_eq!(
        input(3),
        json!({"prompt": "q3", "use_history": false, "remember": true}),
        "joins without consulting"
    );
    let thread4 = json!([
        {"role": "user", "content": "q0"},
        {"role": "assistant", "content": reply0},
        {"role": "user", "content": "q3"},
        {"role": "assistant", "content": reply3},
    ]);
    assert_eq!(
        input(4),
        json!({"prompt": "q4", "use_history": true, "remember": true, "history": thread4}),
        "the thread holds q0 and q3, and only those"
    );

    // The answers are the envelopes, verbatim — the face does not parse
    // for the script.
    for step in &back.steps {
        let envelope: serde_json::Value =
            serde_json::from_str(&step.result.as_ref().unwrap().content).unwrap();
        assert_eq!(envelope["ok"], json!(true));
        assert!(envelope["reply"].as_str().unwrap().contains("mock reply"));
    }
    Ok(())
}

/// The envelope reaches the script as it left the tool, and the script —
/// not the face — branches on `ok`: a failed call is a plain string
/// carrying `{"ok":false,"error":…}`, and the json module parses it.
#[tokio::test]
async fn the_envelope_reaches_the_script_verbatim_which_branches_on_ok() -> anyhow::Result<()> {
    let (rig, wf) = model_rig(BRANCHER, &[])?;
    let outcome = wf
        .start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("expected a completed run: {outcome:?}");
    };
    assert_eq!(
        report.content,
        "the tool said no: `prompt` must be a non-empty string"
    );
    Ok(())
}

/// The thread rides in the recorded input, so a thread that diverged is
/// the request-mismatch refusal and not a quietly answered call: the
/// journal's step 1 embeds a reply nobody got, and the pass rebuilding the
/// real one from step 0's answer is refused.
#[tokio::test]
async fn a_resume_refuses_a_thread_that_diverged() -> anyhow::Result<()> {
    let (rig, wf) = model_rig(CHATTER, &[])?;
    let run = rig.session.lock().await.start_workflow(RunPin {
        plugin: "fixture".into(),
        workflow: "chatter".into(),
        version: "0.1.0".into(),
        hash: wf.hash().to_string(),
        args: Json("{}".into()),
    })?;
    let step0 = json!({"prompt": "first question", "use_history": true, "remember": true, "history": []});
    let step1 = json!({"prompt": "second question", "use_history": true, "remember": true,
                       "history": [
                           {"role": "user", "content": "first question"},
                           {"role": "assistant", "content": "a reply nobody got"},
                       ]});
    {
        let mut s = rig.session.lock().await;
        s.workflow_step(run, 0, &format!("w{run}-0"), "ask_model", Json(step0.to_string()))?;
        s.append(RecordKind::ToolResult {
            tool_use_id: format!("w{run}-0"),
            content: json!({"ok": true, "reply": "the reply the journal holds"}).to_string(),
            is_error: false,
        })?;
        s.workflow_step(run, 1, &format!("w{run}-1"), "ask_model", Json(step1.to_string()))?;
        s.append(RecordKind::ToolResult {
            tool_use_id: format!("w{run}-1"),
            content: json!({"ok": true, "reply": "mock reply to `second question` riding 2 turns"})
                .to_string(),
            is_error: false,
        })?;
    }

    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    let RunOutcome::Failed { reason } = &outcome else {
        anyhow::bail!("expected the diverged thread to fail the run: {outcome:?}");
    };
    assert!(reason.contains("replay mismatch at step 1"), "{reason}");
    Ok(())
}

/// The positive twin: a resume rebuilds the same thread from the same
/// journal, both calls answer from the memo, and nothing is asked again —
/// two answers on the branch, not four.
#[tokio::test]
async fn a_resume_rebuilds_the_same_thread_and_asks_nothing() -> anyhow::Result<()> {
    let (rig, wf) = model_rig(CHATTER, &[])?;
    wf.start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;

    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("expected a completed resume: {outcome:?}");
    };
    assert!(
        report.content.contains("mock reply to `second question`"),
        "the memo's answers reached the script: {report:?}"
    );
    let results = results(&rig).await;
    assert_eq!(results.len(), 2, "the resume asked nothing again: {results:?}");
    Ok(())
}

/// The scope gate is the completion handle's: a host with no catalog has
/// no `ask_model` tool, and the verb refuses to *compile* there — the
/// refusal the narrow scope knows how to make, answering the same
/// question `register_builtins` asks of the tool.
#[test]
fn a_workflow_naming_ask_model_does_not_compile_without_a_completion_path() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::new(dir.path().to_path_buf());
    let err = match Workflow::compile(&host, "fixture", "0.1.0", "chatter", CHATTER, &[]) {
        Ok(_) => panic!("the verb must not exist where no completion can be served"),
        Err(e) => e,
    };
    assert!(format!("{err:#}").contains("ask_model"), "{err:#}");
}

/// The real `ask_model` over a real completion path: the shipped builtin
/// registered the way `register_builtins` registers it, the recorder
/// beside it, and the mock variant `default_model` names — `mock` for the
/// no-text shape, `mock-text` for a reply a thread can remember.
struct RealRig {
    _dir: tempfile::TempDir,
    host: Arc<Host>,
    d: Arc<Dispatcher>,
    session: Arc<AsyncMutex<Session>>,
}

fn real_model_rig(default_model: &str) -> anyhow::Result<RealRig> {
    let dir = tempfile::tempdir()?;
    let host = Host::new(dir.path().to_path_buf());
    host.attach_completion(eidolon_rune::host::Completion {
        catalog: Arc::new(eidolon_providers::Catalog::new()),
        default_model: default_model.into(),
        max_tokens: 1024,
    });
    let mut reg = ToolRegistry::new();
    register_builtins(&mut reg, &host, dir.path())?;
    reg.register(Recorder::tool());
    let session = Arc::new(AsyncMutex::new(Session::create(
        &dir.path().join("run.eid"),
        "mock",
        dir.path(),
        None,
    )?));
    let d = Arc::new(Dispatcher::new(
        reg,
        Arc::new(policy::AllowAll),
        Arc::new(NoUser),
        EventBus::default(),
        session.clone(),
        dir.path().to_path_buf(),
    ));
    host.attach(&d);
    Ok(RealRig {
        _dir: dir,
        host,
        d,
        session,
    })
}

/// The real `ask_model`, through the interpreter: the shipped builtin
/// registered the way `register_builtins` registers it, over the mock
/// provider the catalog resolves. What the fixture's fake cannot prove:
/// the envelope the script branches on is the real tool's actual answer —
/// whose mock opens with a tool call and no text — and the spend the
/// completion path journals inside that answer lands paired with the
/// run's own deterministic step id, which no hand-set id can speak for.
/// Both real answers are `ok:false`, so the second call rode the empty
/// thread: the skip rule, exercised against the real envelope.
#[tokio::test]
async fn the_real_ask_model_runs_end_to_end_with_its_spend_paired_to_the_step() -> anyhow::Result<()>
{
    let rig = real_model_rig("mock")?;
    let wf = Workflow::compile(
        &rig.host,
        "fixture",
        "0.1.0",
        "chatter",
        CHATTER,
        &[],
    )?;

    let outcome = wf
        .start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("expected a completed run: {outcome:?}");
    };
    assert!(
        report.content.contains("no text"),
        "the real envelope crossed verbatim and the script branched on it: {report:?}"
    );

    let back = rig.session.lock().await.workflow_runs().pop().unwrap();
    assert_eq!(back.steps.len(), 2);
    assert!(back.steps.iter().all(|s| s.tool == "ask_model"));
    let spends: Vec<(String, String)> = rig
        .session
        .lock()
        .await
        .branch()
        .into_iter()
        .filter_map(|rec| match &rec.kind {
            RecordKind::ModelSpend { key, call_id, .. } => {
                Some((key.clone(), call_id.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(spends.len(), 2, "one call, one spend record: {spends:?}");
    assert_eq!(spends[0].0, "mock");
    assert_eq!(
        spends[0].1,
        format!("w{}-0", back.id),
        "the spend is paired with the step's deterministic id"
    );
    assert_eq!(spends[1].1, format!("w{}-1", back.id));

    // Both real answers are ok:false — the mock has no text to give — so
    // the second call rode the empty thread: a remembered step whose
    // answer carries no reply joins nothing.
    let input1: serde_json::Value = serde_json::from_str(&back.steps[1].input.0)?;
    assert_eq!(input1["history"], json!([]), "{}", back.steps[1].input.0);
    Ok(())
}

/// The compounding shape over the real tool, on the `mock-text` key: a
/// real `ok:true` reply becomes the next call's history — the assertion
/// Melete's first compounding workflow will actually depend on, exercised
/// against a real envelope rather than the fixture's fake. The reply text
/// asserted is the one the journal holds, read back out of step 0's own
/// answer, because the thread is the journal's and only those turns.
#[tokio::test]
async fn a_real_ok_reply_becomes_the_next_calls_history() -> anyhow::Result<()> {
    let rig = real_model_rig("mock-text")?;
    let wf = Workflow::compile(
        &rig.host,
        "fixture",
        "0.1.0",
        "chatter",
        CHATTER,
        &[],
    )?;

    let outcome = wf
        .start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("expected a completed run: {outcome:?}");
    };
    assert!(
        report.content.contains("one plain answer"),
        "the real reply crossed verbatim: {report:?}"
    );

    let back = rig.session.lock().await.workflow_runs().pop().unwrap();
    assert_eq!(back.steps.len(), 2);

    // Step 0's answer is a real ok:true envelope, and its reply is what
    // step 1 rode in with — user turn then assistant turn, in order.
    let envelope0: serde_json::Value =
        serde_json::from_str(&back.steps[0].result.as_ref().unwrap().content)?;
    assert_eq!(envelope0["ok"], json!(true));
    let reply = envelope0["reply"].as_str().unwrap();
    let input1: serde_json::Value = serde_json::from_str(&back.steps[1].input.0)?;
    assert_eq!(
        input1["history"],
        json!([
            {"role": "user", "content": "first question"},
            {"role": "assistant", "content": reply},
        ]),
        "the reply the journal holds is the history the next call built: {}",
        back.steps[1].input.0
    );

    // And the spend names the key that answered.
    let spends: Vec<String> = rig
        .session
        .lock()
        .await
        .branch()
        .into_iter()
        .filter_map(|rec| match &rec.kind {
            RecordKind::ModelSpend { key, .. } => Some(key.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(spends, vec!["mock-text".to_string(), "mock-text".to_string()]);
    Ok(())
}

// --- the caller's budget: steps and spend, journaled and unescapable --------

/// Three recorded steps: one more than the fixture budget in the tests
/// below, so the third is the index the budget refuses.
const THRICE: &str = r#"
pub fn workflow() {
    #{ name: "thrice", description: "three recorded steps" }
}

pub async fn run(args) {
    eidolon::tool_call("recorder", #{ note: "one" }).await?;
    eidolon::tool_call("recorder", #{ note: "two" }).await?;
    eidolon::tool_call("recorder", #{ note: "three" }).await?;
    "done"
}
"#;

/// The rig with a priced catalog on the completion handle: the spend check
/// reads its prices there, so the handle carries a row for the key the
/// fixture's spend records name. No model call is ever made — the spend
/// records are hand-journaled the way the completion path leaves them,
/// because the branch is the only book the check reads.
fn priced_rig(src: &str) -> anyhow::Result<(Rig, Arc<Workflow>)> {
    let (rig, wf) = rig_with(&[], false, src, &[])?;
    let def: eidolon_providers::ProviderDef = serde_json::from_value(json!({
        "name": "priced",
        "wire": "openai",
        "base_url": "http://x",
        "token": "t",
        "models": [{ "id": "m", "cost": { "input": 0.075, "output": 0.25 } }],
    }))?;
    let mut catalog = eidolon_providers::Catalog::new();
    catalog.add_def(def, None, "test");
    rig.host.attach_completion(eidolon_rune::host::Completion {
        catalog: Arc::new(catalog),
        default_model: String::new(),
        max_tokens: 0,
    });
    Ok((rig, wf))
}

/// A spend record, the way the completion path leaves one: keyed, paired
/// with the step id, and carrying usage. 2M fresh input at the fixture's
/// $0.075/M is $0.15 — comfortably over the $0.10 the budgets name.
async fn journal_spend(rig: &Rig, run: RecordId, index: u32, key: &str) -> anyhow::Result<()> {
    rig.session.lock().await.append(RecordKind::ModelSpend {
        call_id: format!("w{run}-{index}"),
        key: key.to_string(),
        usage: eidolon_core::message::Usage {
            input_tokens: 2_000_000,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    })?;
    Ok(())
}

/// The steps half: a run given two steps fails on the third, and the
/// reason names the caller's bound — a bound set from outside the plugin,
/// worded so a ledger reads it apart from the plugin's own cap. The
/// refused index journaled nothing, so the resume re-fails at the same
/// index with the same reason instead of refusing a hole: the budget is
/// the branch's, and a resume carries none to escape with.
#[tokio::test]
async fn a_step_budget_fails_the_run_naming_the_callers_bound() -> anyhow::Result<()> {
    let (rig, wf) = rig(THRICE, &[])?;
    let budget = eidolon_core::workflow::RunBudget {
        steps: Some(2),
        spend_micros: None,
    };
    let outcome = wf
        .start(&rig.d, Json("{}".into()), budget, CancellationToken::new())
        .await?;
    let RunOutcome::Failed { reason } = &outcome else {
        anyhow::bail!("expected the budget to fail the run: {outcome:?}");
    };
    assert!(
        reason.contains("step budget (2 steps)") && reason.contains("the caller set the bound"),
        "the refusal names the caller's bound: {reason}"
    );
    assert_eq!(effects(&rig), 2, "two steps ran, the third did not");

    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;
    let back = rig.session.lock().await.workflow_run(run).unwrap();
    assert_eq!(back.end, Some(RunStatus::Failed));
    assert_eq!(back.steps.len(), 2, "the refused index journaled nothing");

    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    let RunOutcome::Failed { reason: again } = &outcome else {
        anyhow::bail!("expected the resume to re-fail: {outcome:?}");
    };
    assert_eq!(again, reason, "the same bound, the same refusal");
    assert_eq!(effects(&rig), 2, "the resume ran nothing new");
    Ok(())
}

/// The budget is a record on the branch, read back the way the steps are:
/// a start that was given none journals none — there is nothing to
/// escape, and nothing to strike.
#[tokio::test]
async fn a_run_given_no_budget_journals_no_budget_record() -> anyhow::Result<()> {
    let (rig, wf) = rig(ASKER, &[])?;
    wf.start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;
    let budgets: Vec<()> = rig
        .session
        .lock()
        .await
        .branch()
        .into_iter()
        .filter_map(|rec| match rec.kind {
            RecordKind::WorkflowBudget { .. } => Some(()),
            _ => None,
        })
        .collect();
    assert!(budgets.is_empty(), "no budget, no record");
    assert!(rig.session.lock().await.run_budget(run).is_none());
    Ok(())
}

/// The spend half, priced: two recorded steps with the park between, a
/// $0.10 budget, and a $0.15 call journaled while the run sat parked. The
/// resume replays the first step off the memo — a memo hit costs nothing
/// and is past every check — answers the park, and fails on the second
/// step's check, the reason carrying both numbers at the keys the calls
/// resolved to. The run is failed, not refused: the spend happened. A
/// further resume re-fails identically — the branch still holds the
/// budget, and the resume carries none.
#[tokio::test]
async fn a_priced_spend_over_the_budget_fails_and_a_resume_cannot_escape() -> anyhow::Result<()> {
    let (rig, wf) = priced_rig(ASKER)?;
    let budget = eidolon_core::workflow::RunBudget {
        steps: None,
        spend_micros: Some(100_000),
    };
    let outcome = wf
        .start(&rig.d, Json("{}".into()), budget, CancellationToken::new())
        .await?;
    let RunOutcome::Parked { .. } = &outcome else {
        anyhow::bail!("expected the run to park before the spend existed: {outcome:?}");
    };
    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;

    // The spend arrived while the run waited, the way a first pass's call
    // does; and the person answered the park.
    journal_spend(&rig, run, 0, "priced:m").await?;
    rig.session.lock().await.append(RecordKind::AskUser {
        call_id: format!("w{run}-park0"),
        prompt: "Ship it?".into(),
        answer: Some("yes".into()),
    })?;

    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    let RunOutcome::Failed { reason } = &outcome else {
        anyhow::bail!("expected the spend to fail the run: {outcome:?}");
    };
    assert!(
        reason.contains("$0.1500 of the $0.1000 allowed"),
        "the refusal carries both numbers: {reason}"
    );
    assert_eq!(effects(&rig), 1, "the memo replayed; the second step never ran");

    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    let RunOutcome::Failed { reason: again } = &outcome else {
        anyhow::bail!("expected the further resume to re-fail: {outcome:?}");
    };
    assert_eq!(again, reason, "the branch still holds the budget");
    assert_eq!(effects(&rig), 1, "no effect past the budget, ever");
    Ok(())
}

/// Unpriced is not free: a call whose key has no row in the catalog is a
/// floor, and the refusal says so — the priced part alone carried the run
/// over, and the unpriced call is named as not counted rather than summed
/// in as zero.
#[tokio::test]
async fn an_unpriced_call_is_a_named_floor_in_the_spend_refusal() -> anyhow::Result<()> {
    let (rig, wf) = priced_rig(ASKER)?;
    let budget = eidolon_core::workflow::RunBudget {
        steps: None,
        spend_micros: Some(100_000),
    };
    wf.start(&rig.d, Json("{}".into()), budget, CancellationToken::new())
        .await?;
    let run = rig.session.lock().await.workflow_runs().pop().unwrap().id;
    journal_spend(&rig, run, 0, "priced:m").await?;
    // A second call the catalog has no row for — the bare mock key, whose
    // model_key carries no provider and so no price.
    journal_spend(&rig, run, 0, "mock").await?;
    rig.session.lock().await.append(RecordKind::AskUser {
        call_id: format!("w{run}-park0"),
        prompt: "Ship it?".into(),
        answer: Some("yes".into()),
    })?;

    let outcome = wf.resume(&rig.d, run, CancellationToken::new()).await?;
    let RunOutcome::Failed { reason } = &outcome else {
        anyhow::bail!("expected the spend to fail the run: {outcome:?}");
    };
    assert!(
        reason.contains("not counting 1 unpriced call(s)"),
        "the floor is named, not summed as free: {reason}"
    );
    Ok(())
}

/// The scope's one pure computation: `eidolon::sha256` answers in the vouch
/// rows' own spelling, and a run that only hashes journals its one line and
/// touches nothing else.
#[tokio::test]
async fn the_scope_hashes_text_in_the_vouch_dialect() -> anyhow::Result<()> {
    const HASHER: &str = r#"
        pub fn workflow() {
            #{
                name: "hasher",
                description: "hash one constant",
                approval: "read_only",
                params: #{ "type": "object", "properties": #{}, "required": [] },
            }
        }

        pub async fn run(args) {
            eidolon::sha256("abc")
        }
    "#;
    let (rig, wf) = rig(HASHER, &[])?;
    let outcome = wf
        .start(&rig.d, Json("{}".into()), eidolon_core::workflow::RunBudget::default(), CancellationToken::new())
        .await?;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("expected a completed run: {outcome:?}");
    };
    assert_eq!(report.content, eidolon_rune::permits::content_hash("abc"));
    assert_eq!(report.content, "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(effects(&rig), 0, "a hash is not an effect");
    Ok(())
}

/// The asking pair, end to end at the park: the inline wait races the
/// operator's dialog against the door, and the door's win continues the
/// run, journals the answer under the question it was asked under, and
/// names the peer beside it. The registry held the question only while
/// the seam waited — asserted on the way through — so the advertisement
/// a peer's roster read shows is this test's first leg.
#[tokio::test]
async fn a_park_answers_over_the_door_and_journals_the_provenance() -> anyhow::Result<()> {
    use eidolon_core::dispatch::QuestionKind;
    use eidolon_core::testing::HeldUser;

    const PARKER: &str = r#"
        pub fn workflow() {
            #{
                name: "parker",
                description: "asks, then records the answer",
                approval: "read_only",
            }
        }

        pub async fn run(args) {
            let word = eidolon::park("continue past the gate?").await?;
            eidolon::tool_call("recorder", #{ note: word }).await?;
            "done"
        }
    "#;
    let held = HeldUser::new();
    let (rig, wf) = rig_as(&[Recorder::tool()], false, PARKER, &[], held.clone())?;

    let run = {
        let wf = wf.clone();
        let d = rig.d.clone();
        tokio::spawn(async move {
            wf.start(
                &d,
                Json("{\"times\":1}".into()),
                eidolon_core::workflow::RunBudget::default(),
                CancellationToken::new(),
            )
            .await
        })
    };
    // The inline wait is holding on the operator's dialog — and the
    // registry holds the question while it does.
    held.entered.notified().await;
    let outstanding = rig.d.outstanding_questions();
    assert_eq!(outstanding.len(), 1, "{outstanding:?}");
    assert_eq!(outstanding[0].kind, QuestionKind::Respond);
    assert_eq!(outstanding[0].text, "continue past the gate?");

    // The door: a peer's answer. The run continues past the park.
    rig.d
        .deliver_answer(&outstanding[0].id, "the gate is open", "eidolon-peer")
        .unwrap();

    let outcome = run.await??;
    let RunOutcome::Completed { report } = &outcome else {
        anyhow::bail!("the answered park completes the run: {outcome:?}");
    };
    assert_eq!(report.content, "done");
    assert_eq!(effects(&rig), 1, "the step after the park ran, once");

    // The ledger: the answer rides the AskUser record the question was
    // asked under, and the provenance names the peer — the fourth
    // producer, kept distinct from the operator's answer.
    let session = rig.session.lock().await;
    let branch = session.branch();
    let (call_id, _, answer) = branch
        .iter()
        .filter_map(|rec| match &rec.kind {
            RecordKind::AskUser {
                call_id,
                prompt,
                answer,
            } if prompt == "continue past the gate?" => {
                Some((call_id.clone(), prompt.clone(), answer.clone()))
            }
            _ => None,
        })
        .next_back()
        .expect("the park's question is on the branch");
    assert_eq!(answer.as_deref(), Some("the gate is open"));
    assert!(
        branch.iter().any(|rec| matches!(
            &rec.kind,
            RecordKind::PeerAnswer { call_id: id, peer }
                if id == &call_id && peer == "eidolon-peer"
        )),
        "the provenance rides beside the answer it names"
    );
    Ok(())
}
