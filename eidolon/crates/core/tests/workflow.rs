//! A **workflow run**'s records, and the reading of them that makes resume
//! replay: the log is the checkpoint, so everything a resumed run needs must
//! be *here* and nowhere else — the pin it was started under, the request and
//! the answer of every step, and how it stopped.
//!
//! `pins.rs` owns the bytes; this file owns the invariant. The shape it
//! exercises is the one the interpreter must use: a step is journaled under
//! the run's own deterministic call id (`w<run>-<index>`), and `dispatch`
//! then writes the `ToolResult` for that same id — which is what lets a
//! resume find an answer it already has instead of firing the side effect
//! again.

use std::path::{Path, PathBuf};

use eidolon_core::{Json, RecordKind, RunPin, RunStatus, Session};

fn dir_under(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn pin() -> RunPin {
    RunPin {
        plugin: "jira".into(),
        workflow: "triage".into(),
        version: "0.1.0".into(),
        hash: "ab12".into(),
        args: Json("{\"since\":\"2026-09-01\"}".into()),
    }
}

/// One step, journaled the way the interpreter does it: the request under the
/// run's own deterministic call id, then the result the chokepoint writes for
/// that same id.
fn step(s: &mut Session, run: u64, index: u32, tool: &str, input: &str, out: &str) {
    let call_id = format!("w{run}-{index}");
    s.workflow_step(run, index, &call_id, tool, Json(input.into()))
        .unwrap();
    s.append(RecordKind::ToolResult {
        tool_use_id: call_id,
        content: out.into(),
        is_error: false,
    })
    .unwrap();
}

/// A run of two steps, finished, in a session on disk. Returns the path and
/// the run's id.
fn finished_run(dir: &Path) -> (PathBuf, u64) {
    let path = dir.join("run.eid");
    let mut s = Session::create(&path, "prov:m", dir, None).unwrap();
    let run = s.start_workflow(pin()).unwrap();
    step(&mut s, run, 0, "jira_search", "{\"jql\":\"open\"}", "3 issues");
    step(&mut s, run, 1, "jira_comment", "{\"id\":\"J-1\"}", "commented");
    s.end_workflow(run, RunStatus::Completed).unwrap();
    (path, run)
}

#[test]
fn a_run_reads_back_as_the_pin_and_the_answers_it_already_has() {
    let dir = tempfile::tempdir().unwrap();
    let (path, run) = finished_run(dir.path());

    // From the file, which is the only thing a resume has.
    let s = Session::open(&path).unwrap();
    let back = s.workflow_run(run).expect("the run is on the branch");
    assert_eq!(back.id, run);
    assert_eq!(back.pin, pin(), "the pin is what the run was started as");
    assert_eq!(back.end, Some(RunStatus::Completed));
    assert_eq!(back.gap(), None);
    assert_eq!(back.steps.len(), 2);
    for (i, st) in back.steps.iter().enumerate() {
        assert_eq!(st.index as usize, i, "steps read in index order");
        assert_eq!(st.call_id, format!("w{run}-{i}"));
        let result = st.result.as_ref().expect("every step's answer is here");
        assert!(!result.is_error);
    }
    assert_eq!(back.steps[0].tool, "jira_search");
    assert_eq!(back.steps[0].result.as_ref().unwrap().content, "3 issues");
    assert_eq!(s.workflow_runs().len(), 1);
}

/// The answer is found by the step's own call id, never by "the next result
/// on the branch". A tool that calls a tool journals its own results in
/// between, and pairing by adjacency would hand a step its neighbour's
/// output — the memoization returning the wrong answer, which is worse than
/// re-running.
#[test]
fn a_step_takes_the_answer_with_its_own_id_and_not_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.eid");
    let mut s = Session::create(&path, "prov:m", dir.path(), None).unwrap();
    let run = s.start_workflow(pin()).unwrap();

    let call_id = format!("w{run}-0");
    s.workflow_step(run, 0, &call_id, "jira_search", Json("{}".into()))
        .unwrap();
    // A nested call's result lands first, as it does when a tool calls a tool.
    s.append(RecordKind::ToolResult {
        tool_use_id: "inner-1".into(),
        content: "the nested call's output".into(),
        is_error: false,
    })
    .unwrap();
    s.append(RecordKind::ToolResult {
        tool_use_id: call_id,
        content: "the step's own output".into(),
        is_error: false,
    })
    .unwrap();

    let back = s.workflow_run(run).unwrap();
    assert_eq!(back.steps.len(), 1);
    assert_eq!(
        back.steps[0].result.as_ref().unwrap().content,
        "the step's own output"
    );
}

#[test]
fn a_resumed_run_appends_its_own_ending_and_the_latest_one_wins() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.eid");
    let mut s = Session::create(&path, "prov:m", dir.path(), None).unwrap();
    let run = s.start_workflow(pin()).unwrap();
    step(&mut s, run, 0, "jira_search", "{}", "3 issues");
    s.end_workflow(run, RunStatus::Parked).unwrap();
    assert_eq!(s.workflow_run(run).unwrap().end, Some(RunStatus::Parked));

    // The person answered; the run carried on and finished. A record is a
    // thing that happened, so this is a second ending, not an edit.
    step(&mut s, run, 1, "jira_comment", "{}", "commented");
    s.end_workflow(run, RunStatus::Completed).unwrap();
    let back = s.workflow_run(run).unwrap();
    assert_eq!(back.end, Some(RunStatus::Completed));
    assert_eq!(back.steps.len(), 2);
    assert_eq!(back.gap(), None);
}

/// A step struck from the replay leaves a **hole**, and a hole is a refusal.
/// The alternative — an interpreter that walked past it — would dispatch the
/// step again, which is the side effect memoization exists to prevent.
#[test]
fn a_struck_step_is_a_hole_and_not_a_re_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.eid");
    let mut s = Session::create(&path, "prov:m", dir.path(), None).unwrap();
    let run = s.start_workflow(pin()).unwrap();
    let call_id = format!("w{run}-0");
    let first = s
        .workflow_step(run, 0, &call_id, "jira_search", Json("{}".into()))
        .unwrap();
    step(&mut s, run, 1, "jira_comment", "{}", "commented");

    s.set_excluded(first, true).unwrap();
    let back = s.workflow_run(run).unwrap();
    assert_eq!(back.steps.len(), 1, "the struck step is out of the replay");
    assert_eq!(back.gap(), Some(0), "and its index is the hole");
    // Struck from the replay, never from the log: the record is still there.
    assert!(s.records().iter().any(|r| r.id == first));
}

/// A request whose answer never landed is a **result hole**, and it is the
/// same refusal an index hole is: re-dispatching the step would fire the
/// effect a second time, which is the double the memo exists to prevent.
/// Only the *last* step may stand without an answer — that is a run stopped
/// mid-step, and the next test is about that one.
#[test]
fn a_step_whose_answer_never_landed_is_a_hole_too() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.eid");
    let mut s = Session::create(&path, "prov:m", dir.path(), None).unwrap();
    let run = s.start_workflow(pin()).unwrap();
    step(&mut s, run, 0, "jira_search", "{}", "3 issues");
    // Step 1's request is journaled; the result under its call id never is —
    // struck from the branch, or the log cut mid-step on the other machine.
    let call_id = format!("w{run}-1");
    s.workflow_step(run, 1, &call_id, "jira_comment", Json("{}".into()))
        .unwrap();
    step(&mut s, run, 2, "jira_label", "{}", "labelled");

    let back = s.workflow_run(run).unwrap();
    assert_eq!(back.steps.len(), 3, "the request is on the branch");
    assert!(back.steps[1].result.is_none(), "its answer is not");
    assert_eq!(
        back.gap(),
        Some(1),
        "and that step is the hole a resume refuses at"
    );
}

/// The legitimate shape of *parked*: the last step's answer is not here,
/// because the run stopped to ask. `gap` does not report it — and it does
/// not bless it either: the same shape under a `Completed` ending passes
/// `gap` too, because the ending is the caller's check to make where the
/// resume actually reads it, and deliberately not `gap`'s.
#[test]
fn a_resultless_last_step_is_the_parked_shape_and_not_a_hole() {
    let dir = tempfile::tempdir().unwrap();

    let parked = {
        let path = dir.path().join("parked.eid");
        let mut s = Session::create(&path, "prov:m", dir.path(), None).unwrap();
        let run = s.start_workflow(pin()).unwrap();
        step(&mut s, run, 0, "jira_search", "{}", "3 issues");
        let call_id = format!("w{run}-1");
        s.workflow_step(run, 1, &call_id, "choices_user", Json("{}".into()))
            .unwrap();
        s.end_workflow(run, RunStatus::Parked).unwrap();
        s.workflow_run(run).unwrap()
    };
    assert_eq!(parked.end, Some(RunStatus::Parked));
    assert!(parked.steps[1].result.is_none());
    assert_eq!(parked.gap(), None, "the parked shape is replayable");

    let abandoned = {
        let path = dir.path().join("ended.eid");
        let mut s = Session::create(&path, "prov:m", dir.path(), None).unwrap();
        let run = s.start_workflow(pin()).unwrap();
        step(&mut s, run, 0, "jira_search", "{}", "3 issues");
        let call_id = format!("w{run}-1");
        s.workflow_step(run, 1, &call_id, "jira_comment", Json("{}".into()))
            .unwrap();
        s.end_workflow(run, RunStatus::Completed).unwrap();
        s.workflow_run(run).unwrap()
    };
    assert_eq!(abandoned.end, Some(RunStatus::Completed));
    assert!(abandoned.steps[1].result.is_none());
    assert_eq!(
        abandoned.gap(),
        None,
        "gap never consults the ending; refusing this resume is the caller's check"
    );
}

/// A run that is a session (ruling 8), so it hops like one: what arrives on the
/// other machine is the pin and the answers, which is what lets a venue
/// refuse a workflow it does not have at the hash the run was started with.
#[test]
fn a_run_that_hopped_keeps_its_pin_and_its_answers() {
    let dir = tempfile::tempdir().unwrap();
    let (path, run) = finished_run(dir.path());
    let here = dir_under(dir.path(), "here");

    let s = Session::open_projected(&path, &here, Some("desktop")).unwrap();
    let back = s.workflow_run(run).expect("the run arrived with the session");
    assert_eq!(back.pin, pin());
    assert_eq!(back.end, Some(RunStatus::Completed));
    assert_eq!(back.steps.len(), 2);
    assert_eq!(
        back.steps[1].result.as_ref().unwrap().content,
        "commented",
        "the answers came across, not just the requests"
    );
}

#[test]
fn an_id_that_names_no_run_is_no_run() {
    let dir = tempfile::tempdir().unwrap();
    let (path, run) = finished_run(dir.path());
    let s = Session::open(&path).unwrap();

    assert!(s.workflow_run(run).is_some());
    assert!(s.workflow_run(0).is_none(), "the SessionStart is not a run");
    assert!(s.workflow_run(run + 99).is_none());
    assert!(s.workflow_runs().iter().all(|r| r.id == run));
}

