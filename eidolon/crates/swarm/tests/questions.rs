//! The asking pair's swarm half: the roster's advertisement of a
//! question — which exists exactly while the asking session's registry
//! holds it — and the answer client, whose refusals are the asking
//! session's own words carried back verbatim.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use eidolon_swarm::presence::{PeerQuestion, Presence};
use eidolon_swarm::{answer, channel_key_of, roster_of};

fn register(root: &std::path::Path, name: &str) -> Arc<Presence> {
    let cwd = root.join(name);
    std::fs::create_dir_all(&cwd).unwrap();
    Presence::register(root, &cwd.join(format!("{name}.eid")), &cwd, "mock", name).unwrap()
}

/// A question is on the roster exactly while the registry holds it: the
/// board writes it, the scan reads it, the roster renders it with the id
/// and the kind — and an emptied board takes it back off.
/// *Requirement: nothing advertised before the inner seam agrees to ask.*
#[tokio::test]
async fn the_roster_shows_a_question_exactly_while_it_stands() {
    let root = tempfile::tempdir().unwrap();
    let a = register(root.path(), "eidolon-aaa");

    let board = a.question_board();
    board
        .publish(&[eidolon_core::dispatch::OutstandingQuestion {
            id: "w1-park0".into(),
            kind: eidolon_core::dispatch::QuestionKind::Respond,
            text: "continue past the gate?".into(),
        }]);

    let peers = a.peers();
    assert_eq!(peers.len(), 0, "a session does not roster itself");

    // From the other side — a second registration sees it.
    let b = register(root.path(), "eidolon-bbb");
    let peers = b.peers();
    assert_eq!(peers.len(), 1);
    assert_eq!(
        peers[0].questions,
        vec![PeerQuestion {
            id: "w1-park0".into(),
            kind: "respond".into(),
            text: "continue past the gate?".into(),
        }]
    );

    let text = roster_of(
        b.peers(),
        &channel_key_of(&root.path().join("eidolon-bbb")),
        Some((b.id(), &root.path().join("eidolon-bbb").display().to_string())),
    );
    assert!(
        text.contains("asking (respond) `w1-park0` — continue past the gate?"),
        "{text}"
    );
    assert!(text.contains("(answer with the `answer` tool)"), "{text}");

    // The wait ended; the advertisement ends with it.
    board.publish(&[]);
    let peers = b.peers();
    assert!(peers[0].questions.is_empty(), "nothing advertised after");
    let text = roster_of(
        b.peers(),
        &channel_key_of(&root.path().join("eidolon-bbb")),
        Some((b.id(), &root.path().join("eidolon-bbb").display().to_string())),
    );
    assert!(!text.contains("asking"), "{text}");
}

/// The answer client walks to the asking session's door and brings its
/// words back: a delivery is said, and every refusal — gate, already
/// answered, unknown, self — is the teaching, not a shrug.
/// *Requirements: self-answer refused; refusals teach.*
#[tokio::test]
async fn an_answer_brings_the_doors_words_back() {
    let root = tempfile::tempdir().unwrap();
    let a = register(root.path(), "eidolon-aaa");
    let b = register(root.path(), "eidolon-bbb");

    // The asking session's door, as the dispatcher's registry would
    // answer: one open question, one tombstone, one gate question.
    let book: Mutex<HashMap<String, Value>> = Mutex::new(HashMap::new());
    book.lock().unwrap().insert(
        "q-open".into(),
        json!({"ok": true}),
    );
    book.lock().unwrap().insert(
        "q-taken".into(),
        json!({"ok": false, "error": "the question `q-taken` was already answered by `eidolon-ccc`"}),
    );
    book.lock().unwrap().insert(
        "q-gate".into(),
        json!({"ok": false, "error": "the question `q-gate` is a gate question — approval decides what runs on this session, and that is the operator's alone; a peer's answer is refused and the operator's dialog still stands"}),
    );
    let handler: eidolon_swarm::socket::OpHandler = Arc::new(move |req: &Value, _| {
        let id = req.get("id").and_then(Value::as_str).unwrap_or("");
        book.lock().unwrap().get(id).cloned()
    });
    let (ring, _rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let _server = eidolon_swarm::socket::serve_with(
        &a.socket_path(),
        ring,
        cancel,
        Some(handler),
    )
    .await
    .unwrap();

    // The blocking client must not starve the serve task on the test's
    // single-threaded runtime: the calls run on the blocking pool, as the
    // socket tests' own clients do.
    let a_id = a.id().to_string();
    let b2 = b.clone();
    let results = tokio::task::spawn_blocking(move || {
        let delivered = answer(&b2, &a_id, "q-open", "left").unwrap();
        let loser = answer(&b2, &a_id, "q-taken", "right").unwrap_err().to_string();
        let gate = answer(&b2, &a_id, "q-gate", "yes").unwrap_err().to_string();
        let unknown = answer(&b2, "eidolon-zzz", "q-open", "left").unwrap_err().to_string();
        (delivered, loser, gate, unknown)
    })
    .await
    .unwrap();
    let (said, loser, gate, unknown) = results;

    // The delivered answer is said.
    assert!(said.contains("delivered to"), "{said}");

    // The loser is told who won.
    assert!(loser.contains("already answered by `eidolon-ccc`"), "{loser}");

    // A gate question is refused with the reason that teaches.
    assert!(gate.contains("the operator's alone"), "{gate}");

    // An unknown name lists who is there, one retry instead of a shrug.
    assert!(unknown.contains("eidolon-aaa"), "{unknown}");

    // The asker cannot answer itself — said as the case, not as a generic
    // refusal.
    let why = answer(&a, a.id(), "q-open", "my own word")
        .unwrap_err()
        .to_string();
    assert!(why.contains("does not answer its own question"), "{why}");
}
