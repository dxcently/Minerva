//! Two sessions, end to end: registration, discovery, delivery, drain.
//!
//! The unit tests cover each half on its own; this is the one that would
//! have caught a socket bound in the wrong place or a roster that cannot
//! see a live neighbour, because it runs two of everything.

use std::path::Path;
use std::sync::Arc;

use eidolon_core::peer::PeerInbox;
use eidolon_swarm::presence::{Delivery, Presence};
use eidolon_swarm::{api, socket};
use tokio_util::sync::CancellationToken;

/// Register a session and answer its doorbell, as a live one does.
///
/// The receiver comes back with it and must be kept: a session whose
/// consumer has hung up answers a `ping` but refuses a `notify`, which is
/// exactly right — it is running, and it is not taking messages — and a
/// test that dropped it would be testing that case by accident.
async fn live(
    root: &Path,
    cwd: &Path,
    name: &str,
    cancel: &CancellationToken,
) -> (Arc<Presence>, Ring) {
    let p = Presence::register(root, &cwd.join(format!("{name}.eid")), cwd, "mock", name).unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    socket::serve(&p.socket_path(), tx, cancel.clone())
        .await
        .unwrap();
    (p, rx)
}

type Ring = tokio::sync::mpsc::UnboundedReceiver<()>;

#[tokio::test]
async fn two_sessions_find_each_other_and_one_leaves_the_other_a_message() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("run");
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(proj.join(".git")).unwrap();
    let cancel = CancellationToken::new();

    let (a, _ring_a) = live(&root, &proj, "a", &cancel).await;
    let (b, mut ring_b) = live(&root, &proj, "b", &cancel).await;

    // Each sees exactly the other, and the roster says so in words.
    let (aid, bid) = (a.id().to_string(), b.id().to_string());
    let seen: Vec<String> = {
        let a = a.clone();
        tokio::task::spawn_blocking(move || a.peers().into_iter().map(|p| p.meta.id).collect())
            .await
            .unwrap()
    };
    assert_eq!(
        seen,
        vec![bid.clone()],
        "one live neighbour, and it is the other one"
    );

    let text = "I am in crates/tui/src/render.rs; it is yours after I say so";
    let (report, drained) = {
        let (a, b) = (a.clone(), b.clone());
        let bid = bid.clone();
        tokio::task::spawn_blocking(move || {
            let report = api::send(&a, &bid, text, None).unwrap();
            (report, b.drain())
        })
        .await
        .unwrap()
    };
    assert!(
        report.contains("delivered to") && report.contains(&bid),
        "{report}"
    );
    ring_b
        .recv()
        .await
        .expect("the recipient was told there is mail, not left to find it");
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].from, aid, "the recipient learns who sent it");
    assert_eq!(drained[0].text, text);
    assert_eq!(
        drained[0].channel, None,
        "a direct message is not a channel one"
    );
    assert!(
        drained[0].wake,
        "a direct message wakes — mid-turn and idle alike — without being asked to"
    );

    // A prefix is enough to name a session, and the roster is prose the
    // model can act on rather than a struct dump.
    let r = {
        let a = a.clone();
        tokio::task::spawn_blocking(move || api::roster(&a))
            .await
            .unwrap()
    };
    assert!(r.contains(&bid) && r.contains("on this project"), "{r}");
    assert!(
        r.contains("idle"),
        "a session with no turn in flight says so: {r}"
    );

    // The roster carries the model a neighbour is on *now*, which is the
    // thing the whole note is for: a session that moved off its launch
    // model and published nothing left its peers reading a stale one.
    assert!(
        r.contains("mock"),
        "the neighbour's model is what it launched on, before any switch: {r}"
    );
    b.set_model("deepseek:deepseek-v4-pro");
    let r = {
        let a = a.clone();
        tokio::task::spawn_blocking(move || api::roster(&a))
            .await
            .unwrap()
    };
    assert!(
        r.contains("deepseek:deepseek-v4-pro"),
        "a switch reaches the roster a peer reads: {r}"
    );

    cancel.cancel();
}

/// The fan-out, and the reason it keys on the repository: one worktree per
/// agent gives every agent a different `cwd`, and a cwd-keyed channel
/// would put each of them on a channel of one.
#[tokio::test]
async fn the_channel_reaches_a_worktree_and_stops_at_another_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("run");
    let cancel = CancellationToken::new();

    // A checkout, a worktree of it, and an unrelated project.
    let repo = tmp.path().join("repo");
    let git = repo.join(".git");
    std::fs::create_dir_all(git.join("worktrees/wt")).unwrap();
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", git.join("worktrees/wt").display()),
    )
    .unwrap();
    std::fs::write(git.join("worktrees/wt/commondir"), "../..\n").unwrap();
    let other = tmp.path().join("other");
    std::fs::create_dir_all(other.join(".git")).unwrap();

    let (main, _ring_main) = live(&root, &repo, "main", &cancel).await;
    let (side, _ring_side) = live(&root, &wt, "side", &cancel).await;
    let (stranger, _ring_stranger) = live(&root, &other, "stranger", &cancel).await;

    let (report, in_wt, in_other) = {
        let (main, side, stranger) = (main.clone(), side.clone(), stranger.clone());
        tokio::task::spawn_blocking(move || {
            let report = api::send(&main, api::CHANNEL, "rebased onto master", None).unwrap();
            (report, side.drain(), stranger.drain())
        })
        .await
        .unwrap()
    };

    assert!(
        report.contains(side.id()),
        "the worktree is on the channel: {report}"
    );
    assert!(
        !report.contains(stranger.id()),
        "another repository is not: {report}"
    );
    assert_eq!(in_wt.len(), 1);
    assert_eq!(
        in_wt[0].channel.as_deref(),
        Some(git.canonicalize().unwrap().display().to_string().as_str())
    );
    assert!(
        !in_wt[0].wake,
        "an announcement is read at the next boundary; it does not start a turn in everybody"
    );

    // Not that it cannot: `wake` is the same parameter on both kinds and only
    // its default differs, so a fan-out everybody must act on now says so.
    let urgent = {
        let (main, side) = (main.clone(), side.clone());
        tokio::task::spawn_blocking(move || {
            api::send(
                &main,
                api::CHANNEL,
                "master is broken, stop pushing",
                Some(true),
            )
            .unwrap();
            side.drain()
        })
        .await
        .unwrap()
    };
    assert_eq!(urgent.len(), 1);
    assert!(
        urgent[0].wake,
        "a channel message that asks to wake wakes everyone it reaches"
    );
    assert!(in_other.is_empty(), "a stranger hears nothing");

    cancel.cancel();
}

/// A recipient that is registered but not answering. The message is
/// already on disk before the doorbell is rung, so this is a *report*
/// rather than a failure — and the report has to say which it was, or an
/// operator reading "sent" has been told something untrue.
#[tokio::test]
async fn a_session_that_is_not_answering_is_queued_rather_than_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("run");
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let cancel = CancellationToken::new();

    let (a, _ring_a) = live(&root, &proj, "a", &cancel).await;
    // Registered, with an inbox, but nothing is listening on its socket.
    let b = Presence::register(&root, &proj.join("b.eid"), &proj, "mock", "b").unwrap();

    let (report, got) = {
        let (a, b) = (a.clone(), b.clone());
        let bid = b.id().to_string();
        tokio::task::spawn_blocking(move || {
            let peer = eidolon_swarm::presence::Peer {
                meta: b.meta(),
                dir: b.dir().to_path_buf(),
            };
            let report = a.deliver(&peer, &a.envelope("are you there?")).unwrap();
            assert_eq!(bid, peer.meta.id);
            (report, b.drain())
        })
        .await
        .unwrap()
    };
    assert_eq!(report, Delivery::Queued);
    assert_eq!(
        got.len(),
        1,
        "and it is still there to be read when it recovers"
    );

    cancel.cancel();
}
