//! What the gate writes down, and what the second stage is allowed to do
//! about it.
//!
//! Two properties, and they are the pair the whole arrangement rests on:
//! the log distinguishes a call the operator approved from one a model
//! approved on their behalf, and the model cannot reach anything but the
//! questions.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use eidolon_core::escalate::Judge;
use eidolon_core::policy::{AllowAll, Approval, Ruling};
use eidolon_core::session::{PolicyOutcome, RecordKind};
use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
use eidolon_core::*;

struct Echo(ToolManifest);

#[async_trait]
impl Tool for Echo {
    fn manifest(&self) -> &ToolManifest {
        &self.0
    }
    async fn call(&self, _: serde_json::Value, _: CallContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::ok("ran"))
    }
}

fn echo() -> Arc<dyn Tool> {
    Arc::new(Echo(ToolManifest {
        name: "echo".into(),
        description: "echo".into(),
        input_schema: json!({"type":"object"}),
        approval: Approval::Mutating,
        prompt: None,
        render: None,
        deferred: false,
    }))
}

/// A hook that hands back one prepared ruling, whatever it is asked.
struct Fixed(Ruling);

#[async_trait]
impl PolicyHook for Fixed {
    async fn pre_tool(&self, _: &ToolCall, _: &ToolManifest, _: &std::path::Path) -> Ruling {
        self.0.clone()
    }
}

fn flagged(reason: &str) -> Ruling {
    Ruling {
        verdict: Verdict::Ask(format!("echo — {reason}. Run it?")),
        reason: Some(reason.into()),
        structural: false,
        judged: None,
        yolo: false,
    }
}

fn refused(reason: &str) -> Ruling {
    Ruling {
        verdict: Verdict::Deny(reason.into()),
        reason: Some(reason.into()),
        structural: false,
        judged: None,
        yolo: false,
    }
}

fn call() -> ToolCall {
    ToolCall {
        id: "c1".into(),
        name: "echo".into(),
        input: json!({}),
        origin: CallOrigin::Model,
    }
}

/// A second call, for a test that dispatches twice: the chokepoint replays
/// a call whose id already has a result on the branch.
fn call2() -> ToolCall {
    ToolCall {
        id: "c2".into(),
        name: "echo".into(),
        input: json!({}),
        origin: CallOrigin::Model,
    }
}

fn session(dir: &std::path::Path) -> Arc<Mutex<Session>> {
    Arc::new(Mutex::new(
        Session::create(&dir.join("s.eid"), "m", dir, None).unwrap(),
    ))
}

fn dispatcher(
    dir: &std::path::Path,
    session: Arc<Mutex<Session>>,
    policy: Arc<dyn PolicyHook>,
    user: Arc<dyn UserIo>,
) -> Dispatcher {
    let mut reg = ToolRegistry::new();
    reg.register(echo());
    Dispatcher::new(
        reg,
        policy,
        user,
        EventBus::default(),
        session,
        dir.to_path_buf(),
    )
}

/// Every `PolicyVerdict` on the branch, in order.
async fn verdicts(session: &Arc<Mutex<Session>>) -> Vec<(PolicyOutcome, String, Option<String>)> {
    session
        .lock()
        .await
        .branch()
        .into_iter()
        .filter_map(|r| match &r.kind {
            RecordKind::PolicyVerdict {
                outcome,
                reason,
                note,
                ..
            } => Some((outcome.clone(), reason.clone(), note.clone())),
            _ => None,
        })
        .collect()
}

// --- what gets written ----------------------------------------------------

/// The log does not fill up with the word "allow". A gate that said so on
/// every `read` would double the length of every session's log to record
/// that nothing happened.
#[tokio::test]
async fn a_silent_yes_is_silent_in_the_log_too() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(dir.path());
    let d = dispatcher(
        dir.path(),
        s.clone(),
        Arc::new(AllowAll),
        ScriptedUser::new(true),
    );
    let out = d.dispatch(call(), CancellationToken::new()).await;
    assert!(!out.is_error);
    assert!(verdicts(&s).await.is_empty(), "an allow is not a record");
}

/// A refusal records the classifier's own reason, not the sentence built
/// around it for a person to read — that reason is the unit a table edit
/// flips, and so the only unit a ledger can group by.
#[tokio::test]
async fn a_refusal_records_the_rule_and_not_the_prose() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(dir.path());
    let policy = Arc::new(Fixed(refused("privilege escalation")));
    let d = dispatcher(dir.path(), s.clone(), policy, ScriptedUser::new(true));
    let out = d.dispatch(call(), CancellationToken::new()).await;
    assert!(out.is_error);

    let v = verdicts(&s).await;
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].0, PolicyOutcome::Refused);
    assert_eq!(v[0].1, "privilege escalation");
    assert_eq!(v[0].2, None);
}

/// The record survives the file. A journaled variant that encodes but does
/// not decode is the exact failure the append-only rule exists to prevent,
/// and it is invisible until someone reopens an old session.
#[tokio::test]
async fn the_verdict_reads_back_off_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = {
        let s = session(dir.path());
        let policy = Arc::new(Fixed(refused("powering down the box")));
        let d = dispatcher(dir.path(), s.clone(), policy, ScriptedUser::new(true));
        d.dispatch(call(), CancellationToken::new()).await;
        // The block is what closes the log: the session has to be dropped
        // before it is reopened, or this reads a handle rather than a file.
        s.lock().await.path().to_path_buf()
    };

    let reopened = Session::open(&path).unwrap();
    let found: Vec<_> = reopened
        .branch()
        .into_iter()
        .filter_map(|r| match &r.kind {
            RecordKind::PolicyVerdict {
                tool,
                reason,
                outcome,
                structural,
                tool_use_id,
                note,
            } => Some((
                tool.clone(),
                reason.clone(),
                outcome.clone(),
                *structural,
                tool_use_id.clone(),
                note.clone(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "echo");
    assert_eq!(found[0].1, "powering down the box");
    assert_eq!(found[0].2, PolicyOutcome::Refused);
    assert!(!found[0].3);
    assert_eq!(found[0].4, "c1");
    assert_eq!(found[0].5, None);
}

/// The two answers a person can give are told apart, because "you were
/// asked forty times and said yes to all forty" and "…and said no every
/// time" are opposite arguments about the same rule.
#[tokio::test]
async fn a_yes_and_a_no_from_the_operator_are_different_records() {
    for (confirm, expected) in [
        (true, PolicyOutcome::Approved),
        (false, PolicyOutcome::Declined),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let s = session(dir.path());
        let policy = Arc::new(Fixed(flagged("writes outside the working directory")));
        let d = dispatcher(dir.path(), s.clone(), policy, ScriptedUser::new(confirm));
        let out = d.dispatch(call(), CancellationToken::new()).await;
        assert_eq!(out.is_error, !confirm);

        let v = verdicts(&s).await;
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].0, expected);
        assert_eq!(v[0].1, "writes outside the working directory");
    }
}

// --- what the judge may do ------------------------------------------------

fn judge(inner: Arc<dyn PolicyHook>, reply: &str, session: Arc<Mutex<Session>>) -> Judge {
    Judge::new(
        inner,
        ScriptedProvider::new(vec![ScriptedProvider::text(reply)]),
        "judge-model",
        Duration::from_secs(5),
        session,
    )
}

async fn ruling_for(judge: &Judge) -> Ruling {
    judge
        .pre_tool(&call(), echo().manifest(), std::path::Path::new("/ws"))
        .await
}

/// The layer is reachable only from a question. A call the table allowed
/// outright must never cost a model call — that is the tool loop's whole
/// cost argument — and a call it refused on the merits is not up for
/// reconsideration by anything.
#[tokio::test]
async fn the_judge_cannot_reach_an_allow_or_a_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(dir.path());
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text(
        "ALLOW - I would allow anything",
    )]);
    let j = Judge::new(
        Arc::new(AllowAll),
        provider.clone(),
        "judge-model",
        Duration::from_secs(5),
        s.clone(),
    );
    let r = ruling_for(&j).await;
    assert_eq!(r.verdict, Verdict::Allow);
    assert_eq!(r.judged, None);
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "an allow costs no model call"
    );

    let dir = tempfile::tempdir().unwrap();
    let s = session(dir.path());
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text(
        "ALLOW - I would allow anything",
    )]);
    let j = Judge::new(
        Arc::new(Fixed(refused("privilege escalation"))),
        provider.clone(),
        "judge-model",
        Duration::from_secs(5),
        s,
    );
    let r = ruling_for(&j).await;
    assert_eq!(r.verdict, Verdict::Deny("privilege escalation".into()));
    assert_eq!(r.judged, None, "the table's refusal is not an opening bid");
    assert!(provider.requests.lock().unwrap().is_empty());
}

/// What it can do: answer, without touching the verdict underneath. The
/// deterministic pass's answer stays on the ruling exactly as it made it,
/// which is what lets the record say a model and not a person said yes.
#[tokio::test]
async fn the_judge_answers_a_question_without_rewriting_it() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(dir.path());
    let inner = Arc::new(Fixed(flagged("recursive delete")));
    let j = judge(inner, "ALLOW - it is the build directory", s);
    let r = ruling_for(&j).await;
    assert!(
        matches!(r.verdict, Verdict::Ask(_)),
        "the question is still the question"
    );
    assert_eq!(r.judged.as_deref(), Some("it is the build directory"));
    assert_eq!(
        r.reason.as_deref(),
        Some("recursive delete"),
        "and the rule that raised it survives"
    );
}

/// Every way of not saying ALLOW is a question for the operator: saying
/// ASK, saying something unparseable, saying nothing. There is no reply
/// that produces a refusal, because refusing is not one of its answers.
#[tokio::test]
async fn anything_but_allow_leaves_the_question_standing() {
    for reply in [
        "ASK - I am not sure",
        "",
        "Hmm, I would probably allow this",
        "DENY - absolutely not",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let s = session(dir.path());
        let j = judge(Arc::new(Fixed(flagged("recursive delete"))), reply, s);
        let r = ruling_for(&j).await;
        assert!(matches!(r.verdict, Verdict::Ask(_)), "{reply:?}");
        assert_eq!(r.judged, None, "{reply:?}");
    }
}

/// A judge that never answers is a judge that costs one prompt. The
/// timeout is short because somebody is waiting behind it.
#[tokio::test]
async fn a_judge_that_times_out_hands_the_question_back() {
    struct Silent;
    impl provider::Provider for Silent {
        fn name(&self) -> &str {
            "silent"
        }
        fn stream<'a>(
            &'a self,
            _: provider::ChatRequest,
            _: CancellationToken,
        ) -> provider::EventStream<'a> {
            Box::pin(futures_util::stream::pending())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let s = session(dir.path());
    let j = Judge::new(
        Arc::new(Fixed(flagged("recursive delete"))),
        Arc::new(Silent),
        "judge-model",
        Duration::from_millis(50),
        s,
    );
    let r = ruling_for(&j).await;
    assert!(matches!(r.verdict, Verdict::Ask(_)));
    assert_eq!(r.judged, None);
}

/// End to end: the operator is never asked, the call runs, and the log
/// says which of the two of them said yes. Counting a model's yes as a
/// person's would be the boundary quietly moving itself.
#[tokio::test]
async fn a_judged_call_runs_unasked_and_says_so_on_the_branch() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(dir.path());
    let user = ScriptedUser::new(false);
    let policy = Arc::new(judge(
        Arc::new(Fixed(flagged("recursive delete"))),
        "ALLOW - the operator asked for a clean rebuild",
        s.clone(),
    ));
    let d = dispatcher(dir.path(), s.clone(), policy, user.clone());
    let out = d.dispatch(call(), CancellationToken::new()).await;

    // The user declines everything, so a call that reached them would have
    // failed. It ran.
    assert!(!out.is_error, "{out:?}");
    assert!(
        user.asked.lock().unwrap().is_empty(),
        "the operator was not interrupted"
    );

    let v = verdicts(&s).await;
    assert_eq!(v.len(), 1);
    assert_eq!(
        v[0].0,
        PolicyOutcome::Judged,
        "not Approved: a person did not say this"
    );
    assert_eq!(v[0].1, "recursive delete");
    assert_eq!(
        v[0].2.as_deref(),
        Some("the operator asked for a clean rebuild")
    );
}

/// And the operator can see it happened. A call waved through leaves no
/// other trace — it ran, it produced output, and nothing on screen
/// separates it from a call the table allowed outright.
#[test]
fn only_the_judges_yes_is_drawn() {
    let note = eidolon_core::policy::verdict_note(
        &PolicyOutcome::Judged,
        "bash",
        Some("the build directory"),
    );
    assert_eq!(
        note.as_deref(),
        Some("bash — allowed without asking you: the build directory")
    );
    for quiet in [
        PolicyOutcome::Approved,
        PolicyOutcome::Declined,
        PolicyOutcome::Refused,
    ] {
        assert_eq!(
            eidolon_core::policy::verdict_note(&quiet, "bash", None),
            None,
            "{quiet:?}"
        );
    }
}

// --- what yolo may do -----------------------------------------------------

/// The third way a question becomes a yes, and it is told apart from the
/// other two for the same reason they are told apart from each other: a
/// ledger that counted a switch being on as an operator's approval would be
/// promoting a table entry on no evidence at all.
///
/// It also journals, which the silent yes above does not — the record of
/// what ran while the gate was blind is the whole of what a yolo session
/// leaves behind, and the classifier's own reason survives onto it, so the
/// question nobody was asked is still legible afterwards.
#[tokio::test]
async fn yolo_answers_the_question_and_says_so_in_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(dir.path());
    let switch = eidolon_core::yolo::Switch::new(true);
    let policy = Arc::new(eidolon_core::yolo::Yolo::new(
        Arc::new(Fixed(flagged("writes outside the working directory"))),
        switch.clone(),
    ));
    // The user declines everything, so a call that reached them would have
    // failed.
    let user = ScriptedUser::new(false);
    let d = dispatcher(dir.path(), s.clone(), policy, user.clone());
    let out = d.dispatch(call(), CancellationToken::new()).await;
    assert!(!out.is_error, "{out:?}");
    assert!(
        user.asked.lock().unwrap().is_empty(),
        "the operator was not interrupted"
    );

    let v = verdicts(&s).await;
    assert_eq!(v.len(), 1);
    assert_eq!(
        v[0].0,
        PolicyOutcome::Yolo,
        "not Approved and not Judged: nothing read this call"
    );
    assert_eq!(v[0].1, "writes outside the working directory");
    assert_eq!(
        v[0].2, None,
        "there was no reasoning; that is the point of it"
    );

    // And disarming it puts the question back, on the same hook.
    switch.set(false);
    let out = d.dispatch(call2(), CancellationToken::new()).await;
    assert!(out.is_error, "the gate asks again, and this user says no");
}

/// A refusal is not a question, so the switch does not reach it. The table
/// denies privilege escalation, powering the box down and destroying a
/// filesystem, and none of those is what an operator arming yolo is
/// thinking about — `[policy] enabled = false` is the way to have no gate.
#[tokio::test]
async fn yolo_does_not_overturn_a_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(dir.path());
    let policy = Arc::new(eidolon_core::yolo::Yolo::new(
        Arc::new(Fixed(refused("privilege escalation"))),
        eidolon_core::yolo::Switch::new(true),
    ));
    let d = dispatcher(dir.path(), s.clone(), policy, ScriptedUser::new(true));
    let out = d.dispatch(call(), CancellationToken::new()).await;
    assert!(out.is_error);

    let v = verdicts(&s).await;
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].0, PolicyOutcome::Refused);
}

/// Under the judge and not over it: an armed switch has already answered
/// the question by the time the judge looks, so the judge — which only ever
/// acts on an `Ask` — calls nothing. An operator running yolo has said they
/// do not want to spend that.
#[tokio::test]
async fn an_armed_switch_costs_no_judge_call() {
    let dir = tempfile::tempdir().unwrap();
    let s = session(dir.path());
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("ALLOW - anything at all")]);
    let j = Judge::new(
        Arc::new(eidolon_core::yolo::Yolo::new(
            Arc::new(Fixed(flagged("recursive delete"))),
            eidolon_core::yolo::Switch::new(true),
        )),
        provider.clone(),
        "judge-model",
        Duration::from_secs(5),
        s,
    );
    let r = ruling_for(&j).await;
    assert_eq!(r.verdict, Verdict::Allow);
    assert!(r.yolo);
    assert_eq!(r.judged, None, "nothing read the call");
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "the model was never asked"
    );
}

/// Nothing is drawn per waived call. Yolo is a posture the operator armed
/// and which says so continuously on the status line; a line per call in
/// the mode built to stop interrupting is the interruption again.
#[test]
fn a_yolo_yes_draws_nothing() {
    assert_eq!(
        eidolon_core::policy::verdict_note(&PolicyOutcome::Yolo, "bash", None),
        None
    );
}
