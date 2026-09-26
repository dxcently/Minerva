//! The venue side of a hop: a session shipped from another machine is
//! reopened here — every record checksum-verified on the way in, the birth
//! machine's backend session marks dropped, the projection noted on the
//! branch — and runs under the cwd it arrived under. Version refusals are
//! typed, so a receiver can tell "needs a different build" from "not a
//! session log at all" without parsing sentences.
//!
//! Nothing here knows Melete exists: a hop between two standalone eidolon
//! boxes takes exactly these paths.

use std::path::Path;

use eidolon_core::session::Refusal;
use eidolon_core::{Message, RecordKind, Session};

fn dir_under(dir: &Path, name: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// The typed refusal behind an open that was meant to fail. `Session`
/// carries no `Debug`, so the refusal is pulled out of the error rather
/// than the result unwrapped.
fn refusal_of(r: anyhow::Result<Session>) -> Refusal {
    match r {
        Ok(_) => panic!("opened; expected a refusal"),
        Err(e) => e
            .downcast_ref::<Refusal>()
            .cloned()
            .unwrap_or_else(|| panic!("not a Refusal: {e:#}")),
    }
}

fn seeded_log(path: &Path, birth: &Path) {
    let mut s = Session::create(path, "prov:m", birth, None).unwrap();
    // A keeping backend on the birth machine holds the branch so far.
    s.append(RecordKind::BackendSession {
        backend: "claude-cli".into(),
        id: "birth-machine-session".into(),
    })
    .unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("hello")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        eidolon_core::ContentBlock::text("hi"),
    ])))
    .unwrap();
}

#[test]
fn a_session_that_arrives_from_another_machine_runs_where_it_landed() {
    let dir = tempfile::tempdir().unwrap();
    let birth = dir_under(dir.path(), "birth");
    let here = dir_under(dir.path(), "here");
    let path = dir.path().join("shipped.eid");
    seeded_log(&path, &birth);

    let s = Session::open_projected(&path, &here, Some("desktop")).unwrap();
    // It runs here, and remembers where it was born.
    assert_eq!(s.cwd().as_deref(), Some(here.to_str().unwrap()));
    let (_, start_cwd, _) = s.start().unwrap();
    assert_eq!(start_cwd, birth.to_str().unwrap());
    assert_eq!(s.model().as_deref(), Some("prov:m"));
    // The birth machine's mark is gone: the next turn on that backend
    // here replays the whole branch.
    assert_eq!(s.backend_session("claude-cli"), None);
    assert_eq!(s.backend_session_mark("claude-cli"), None);
    // The projection is a note on the branch naming the hop — and beside
    // it, the model-facing brief: the same facts as a message, because a
    // `Note` rides no replay and a model left without it would keep every
    // birth-machine path in its history without knowing the ground moved.
    let records = s.records();
    match &records[records.len() - 2].kind {
        RecordKind::Note { text } => {
            assert!(text.contains("desktop"), "note: {text}");
            assert!(text.contains(birth.to_str().unwrap()), "note: {text}");
            assert!(text.contains(here.to_str().unwrap()), "note: {text}");
            assert!(text.contains("claude-cli"), "note: {text}");
        }
        other => panic!("second-to-last record is {other:?}, not the projection note"),
    }
    match &records.last().unwrap().kind {
        RecordKind::ExternalMessage { from, text, .. } => {
            assert_eq!(from, "eidolon import");
            assert!(text.contains(birth.to_str().unwrap()), "brief: {text}");
            assert!(text.contains(here.to_str().unwrap()), "brief: {text}");
            assert!(text.contains("will not resolve"), "brief: {text}");
        }
        other => panic!("last record is {other:?}, not the landing brief"),
    }
    // The brief *is* a message — the one new thing the model is told — and
    // the conversation is otherwise exactly the two turns it was.
    assert_eq!(s.messages().len(), 3);
    assert_eq!(s.messages()[0].text(), "hello");
    assert_eq!(s.messages()[1].text(), "hi");
    assert!(
        s.messages()[2].text().contains("projected onto this machine"),
        "the model is told it moved: {}",
        s.messages()[2].text()
    );
}

#[test]
fn a_session_with_no_backend_marks_hops_free() {
    let dir = tempfile::tempdir().unwrap();
    let birth = dir_under(dir.path(), "birth");
    let here = dir_under(dir.path(), "here");
    let path = dir.path().join("native.eid");
    {
        let mut s = Session::create(&path, "prov:m", &birth, None).unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("hello")))
            .unwrap();
    }
    let before = Session::open(&path).unwrap().records().len();

    let s = Session::open_projected(&path, &here, None).unwrap();
    // Exactly two records were appended — the projection note and the
    // model-facing landing brief, with no clearing marks ahead of either
    // and no origin named.
    assert_eq!(s.records().len(), before + 2);
    let records = s.records();
    match &records[records.len() - 2].kind {
        RecordKind::Note { text } => {
            assert!(text.contains("another machine"), "note: {text}");
            assert!(text.contains("none held"), "note: {text}");
        }
        other => panic!("second-to-last record is {other:?}, not the projection note"),
    }
    match &records.last().unwrap().kind {
        RecordKind::ExternalMessage { from, text, .. } => {
            assert_eq!(from, "eidolon import");
            assert!(
                text.contains("projected onto this machine"),
                "brief: {text}"
            );
        }
        other => panic!("last record is {other:?}, not the landing brief"),
    }
}

#[test]
fn a_reopen_may_override_the_cwd_without_a_projection() {
    let dir = tempfile::tempdir().unwrap();
    let birth = dir_under(dir.path(), "birth");
    let here = dir_under(dir.path(), "here");
    let path = dir.path().join("moved.eid");
    seeded_log(&path, &birth);

    // A plain reopen stands in the birth directory.
    let s = Session::open(&path).unwrap();
    assert_eq!(s.cwd().as_deref(), Some(birth.to_str().unwrap()));
    // An overridden reopen runs where it was told, and the birth facts do
    // not move.
    let s = Session::open_with_cwd(&path, &here).unwrap();
    assert_eq!(s.cwd().as_deref(), Some(here.to_str().unwrap()));
    let (_, start_cwd, _) = s.start().unwrap();
    assert_eq!(start_cwd, birth.to_str().unwrap());
}

#[test]
fn clearing_backend_sessions_names_the_backends_it_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.eid");
    let mut s = Session::create(&path, "prov:m", dir.path(), None).unwrap();
    for backend in ["b-one", "b-two", "b-one"] {
        s.append(RecordKind::BackendSession {
            backend: backend.into(),
            id: format!("{backend}-session"),
        })
        .unwrap();
    }
    // A cleared mark is not a live one: it names nothing to drop.
    s.clear_backend_session("b-two").unwrap();

    assert_eq!(s.clear_backend_sessions().unwrap(), vec!["b-one".to_string()]);
    // A second pass finds nothing live.
    assert!(s.clear_backend_sessions().unwrap().is_empty());
    assert_eq!(s.backend_session("b-one"), None);
    assert_eq!(s.backend_session("b-two"), None);
}

#[test]
fn a_transfer_that_lost_bytes_refuses_to_open() {
    let dir = tempfile::tempdir().unwrap();
    let birth = dir_under(dir.path(), "birth");
    let here = dir_under(dir.path(), "here");
    let path = dir.path().join("damaged.eid");
    seeded_log(&path, &birth);

    // Flip one byte inside the first record's body — exactly what a
    // damaged copy looks like from the outside.
    let mut bytes = std::fs::read(&path).unwrap();
    let header = 6;
    assert!(bytes.len() > header + 16);
    bytes[header + 12] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();

    let e = Session::open_projected(&path, &here, None).err().expect("should refuse");
    assert!(
        e.to_string().contains("checksum"),
        "unexpected refusal: {e:#}"
    );
}

#[test]
fn a_version_mismatch_is_a_typed_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let birth = dir_under(dir.path(), "birth");
    let here = dir_under(dir.path(), "here");
    let path = dir.path().join("future.eid");
    seeded_log(&path, &birth);

    // A journal written by a build whose VERSION differs: the format's
    // version field moved on without this reader.
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[4] = 9;
    std::fs::write(&path, bytes).unwrap();

    assert_eq!(
        refusal_of(Session::open(&path)),
        Refusal::VersionMismatch {
            path: path.clone(),
            found: 9,
            supported: 1,
        }
    );
    // A hop across versions refuses whole, before any record is trusted.
    assert!(matches!(
        refusal_of(Session::open_projected(&path, &here, None)),
        Refusal::VersionMismatch { .. }
    ));
}

#[test]
fn a_file_that_is_not_a_session_log_refuses_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-a-log.eid");
    std::fs::write(&path, b"definitely not a session log").unwrap();

    assert_eq!(
        refusal_of(Session::open(&path)),
        Refusal::NotASessionLog { path: path.clone() }
    );
}

/// What "finish an unsettled turn" means on a reopened log. The shapes a
/// continuation answers — an ask with no reply, a cancel mid-flight, tool
/// uses a crash left dangling — and the one it must not re-ask: a reply
/// standing boundary-less, the shape every journal from a harness that
/// records its row accounting as a `Note` instead of a boundary arrives
/// in, and which a continuation would answer with a paid duplicate.
#[test]
fn needs_finishing_reads_the_direction_of_an_unsettled_branch() {
    use eidolon_core::ContentBlock;
    use serde_json::json;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("directions.eid");
    let mut s = Session::create(&path, "prov:m", dir.path(), None).unwrap();
    // Freshly created: nothing owed, nothing asked.
    assert!(s.is_settled());
    assert!(!s.needs_finishing());

    // The projection shape: reply, then a bookkeeping note, no boundary.
    // Unsettled in the log-shape sense, yet nothing to finish.
    s.append(RecordKind::UserMessage(Message::user_text("hello")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::text("a complete answer"),
    ])))
    .unwrap();
    s.append(RecordKind::Note {
        text: "melete-row:{\"cost_usd\":0.01}".into(),
    })
    .unwrap();
    assert!(!s.is_settled(), "the missing boundary is still the truth");
    assert!(!s.needs_finishing(), "the reply stands; finishing would re-ask it");

    // An ask with no reply does want finishing.
    let ask = {
        let mut s = Session::create(&dir.path().join("ask.eid"), "prov:m", dir.path(), None)
            .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("unanswered")))
            .unwrap();
        s
    };
    assert!(ask.needs_finishing());

    // Tool uses a crash left dangling do too — even though an assistant
    // message sits below them on the branch.
    let dangling = {
        let mut s =
            Session::create(&dir.path().join("dangling.eid"), "prov:m", dir.path(), None).unwrap();
        s.append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::ToolUse {
                id: "tu_1".into(),
                name: "bash".into(),
                input: json!({ "command": "ls" }).into(),
            },
        ])))
        .unwrap();
        s
    };
    assert!(!dangling.is_settled());
    assert!(dangling.needs_finishing());
}
