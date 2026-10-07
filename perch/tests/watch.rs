//! The watcher's ask reader and reconnect, and the stand-in door's asks (slice A1a): `FAKE_ASKS`, `FAKE_LAG`, the 409 and 422 for an answer.
//!
//! Two kinds of test live here. The watcher is a library type, so most of them start `fake_eidolon` as a child of the test (with the same
//! two boot lines and `0600` token file a perch reads) and run `perch::watch::open` against it with a sink that writes down what it is told:
//! that is the real reader against the real stand-in, over a real socket. The last ones start a whole perch, to read what it logs and to
//! stop a chat whose watcher has reconnected. The frame reader's own edge cases (byte-at-a-time reads, every bound, a hello for another
//! session, a sink that panics) are unit tests beside the code, in `src/watch.rs`, against a door that runs a script.
//!
//! Everything needs `--features test-bins`, for the stand-in.

mod common;

// A plain `cargo test` builds this file with nothing to run; the helpers are used only by the tests below.
#[cfg(feature = "test-bins")]
mod with_doors {
    use super::common::*;
    use std::io::{BufRead as _, BufReader};
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use perch::watch::{self, AskNote, AskView, DoorKey, Hello, Origin, Sink};
    use serde_json::{Value, json};

    /// The stand-in door as a child of the test: its own runtime dir, its token file read the way the perch reads it, its request log.
    struct Door {
        child: Child,
        port: u16,
        token: String,
        token_file: PathBuf,
        log: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Door {
        fn start(env: &[(&str, String)]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (runtime, sessions, work) = (dir.path().join("run"), dir.path().join("sessions"), dir.path().join("work"));
            for path in [&runtime, &sessions, &work] {
                std::fs::create_dir(path).unwrap();
            }
            own(&runtime, 0o700);
            let log = dir.path().join("fake.log");
            let mut command = Command::new(fake_eidolon().expect("built with --features test-bins"));
            let mut child = command
                .args(["web", "--bind", "127.0.0.1:0", "--cwd"])
                .arg(&work)
                .env_clear()
                .env("XDG_RUNTIME_DIR", &runtime)
                .env("FAKE_LOG", &log)
                .env("FAKE_SESSIONS_DIR", &sessions)
                .envs(env.iter().map(|(name, value)| (name, value)))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawning the stand-in door");
            // The two boot lines, read on a thread so a door that never prints them fails the test instead of hanging it.
            let stdout = child.stdout.take().unwrap();
            let (lines, boot) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                for _ in 0..2 {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || lines.send(line.trim().to_string()).is_err() {
                        return;
                    }
                }
                // Keep reading so the door never blocks on a full pipe.
                let mut rest = String::new();
                while reader.read_line(&mut rest).is_ok_and(|n| n > 0) {
                    rest.clear();
                }
            });
            let first = boot.recv_timeout(Duration::from_secs(20)).expect("the door printed its first boot line");
            let second = boot.recv_timeout(Duration::from_secs(20)).expect("the door printed its second boot line");
            let port = first.strip_prefix("listening on http://127.0.0.1:").map(|rest| rest.trim_end_matches('/'));
            let port: u16 = port.and_then(|port| port.parse().ok()).expect("a port in the first boot line");
            let token_file = PathBuf::from(second.strip_prefix("token file: ").expect("the token file line"));
            let token = std::fs::read_to_string(&token_file).expect("the door's token file").trim().to_string();
            Self { child, port, token, token_file, log, _dir: dir }
        }

        fn addr(&self) -> String {
            format!("127.0.0.1:{}", self.port)
        }

        fn key(&self) -> DoorKey {
            DoorKey { pid: self.child.id(), port: self.port }
        }

        /// The token reaches the watcher the way the registry hands it over: in memory, never through anything printed.
        fn token(&self) -> Arc<str> {
            Arc::from(self.token.as_str())
        }

        fn post(&self, path: &str, body: Value) -> Answer {
            Request::new(&self.addr(), "POST", path).bearer(format!("Bearer {}", self.token)).body(body.to_string().into_bytes()).send(&self.addr())
        }

        fn say(&self) -> Answer {
            self.post("/api/say", json!({"text": "go"}))
        }

        fn log_lines(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log).unwrap_or_default().lines().map(str::to_string).collect()
        }

        fn count(&self, line: &str) -> usize {
            self.log_lines().iter().filter(|l| *l == line).count()
        }

        fn sigterm(&mut self) -> std::process::ExitStatus {
            assert_eq!(unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) }, 0);
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                assert!(Instant::now() < deadline, "the door ignored SIGTERM for 3 s: {:?}", self.log_lines());
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for Door {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// A door that has no script: `Door::start` takes its environment, and a script is a file in the door's own directory, so a door with
    /// one is started in two steps — the file first, in a directory the test owns.
    struct Scripted {
        _dir: tempfile::TempDir,
        env: Vec<(&'static str, String)>,
    }

    fn scripted(lines: &[String], knobs: &[(&'static str, &str)]) -> Scripted {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asks");
        std::fs::write(&path, lines.join("\n")).unwrap();
        let mut env = vec![("FAKE_ASKS", path.display().to_string())];
        env.extend(knobs.iter().map(|(name, value)| (*name, value.to_string())));
        Scripted { _dir: dir, env }
    }

    fn call_line(id: &str, origin: &str) -> String {
        json!({"type": "tool-call-started", "id": id, "name": "bash", "input": {}, "origin": origin}).to_string()
    }

    fn ask_line(ask_id: u64, call_id: &str, command: &str) -> String {
        json!({
            "type": "ask", "kind": "approval", "ask_id": ask_id, "call_id": call_id, "tool": "bash", "input": {"command": command},
            "prompt": "bash. Run it?", "reason": "outside-cwd", "structural": false, "judged": null, "yolo": false, "answers": ["yes", "no"]
        })
        .to_string()
    }

    fn turn_line(running: bool) -> String {
        json!({"type": "turn-state", "running": running}).to_string()
    }

    /// What a sink was told, in order.
    #[derive(Debug, Clone, PartialEq)]
    enum Ev {
        Hello,
        Ask(u64, Option<Origin>),
        Settled(u64),
        Closed,
    }

    #[derive(Default)]
    struct Told {
        events: Mutex<Vec<Ev>>,
        asks: Mutex<Vec<AskNote>>,
        view: Mutex<Option<AskView>>,
    }

    impl Told {
        fn events(&self) -> Vec<Ev> {
            self.events.lock().unwrap().clone()
        }

        fn asks(&self) -> Vec<AskNote> {
            self.asks.lock().unwrap().clone()
        }

        fn view(&self) -> AskView {
            self.view.lock().unwrap().clone().expect("the sink was given a view")
        }
    }

    impl Sink for Told {
        fn hello(&self, _key: DoorKey, _hello: &Hello) {
            self.events.lock().unwrap().push(Ev::Hello);
        }
        fn watching(&self, _key: DoorKey, asks: AskView) {
            *self.view.lock().unwrap() = Some(asks);
        }
        fn ask(&self, _key: DoorKey, ask: &AskNote) {
            self.events.lock().unwrap().push(Ev::Ask(ask.ask_id, ask.origin));
            self.asks.lock().unwrap().push(ask.clone());
        }
        fn settled(&self, _key: DoorKey, ask_id: u64) {
            self.events.lock().unwrap().push(Ev::Settled(ask_id));
        }
        fn closed(&self, _key: DoorKey) {
            self.events.lock().unwrap().push(Ev::Closed);
        }
    }

    async fn eventually(within: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + within;
        while !done() {
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        true
    }

    const WAIT: Duration = Duration::from_secs(10);

    async fn watch_door(door: &Door, sink: Arc<dyn Sink>) -> watch::Watcher {
        watch::open(door.key(), door.token(), WAIT, sink).await.expect("the stand-in said hello").0
    }

    // -----------------------------------------------------------------------
    // The ask reader
    // -----------------------------------------------------------------------

    /// Scripted asks reach the sink with the origin of their call; an ask whose call frame never came has none; a 100 KiB ask line goes by
    /// unread (a person is asked) and the frames after it are read. Mutation: take the origin from anywhere but the call's frame, or stop
    /// reading at a long line, or parse it, and this goes red.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scripted_asks_arrive_with_their_origin_and_a_long_line_is_skipped() {
        let big = "x".repeat(100 * 1024);
        let script = scripted(
            &[
                call_line("c1", "model"),
                ask_line(1, "c1", "cargo test "),
                call_line("c2", "script"),
                ask_line(2, "c2", "ls"),
                ask_line(3, "never-started", "ls"),
                call_line("c4", "model"),
                ask_line(4, "c4", &big),
                ask_line(5, "c1", "pwd"),
                turn_line(true),
            ],
            &[],
        );
        let door = Door::start(&script.env);
        let told = Arc::new(Told::default());
        let watcher = watch_door(&door, told.clone()).await;
        assert_eq!(door.say().status, 202);
        assert!(eventually(WAIT, || told.events().contains(&Ev::Ask(5, Some(Origin::Model)))).await, "{:?}", told.events());
        assert_eq!(
            told.events(),
            [Ev::Hello, Ev::Ask(1, Some(Origin::Model)), Ev::Ask(2, Some(Origin::Script)), Ev::Ask(3, None), Ev::Ask(5, Some(Origin::Model))],
            "ask 4 is not among them: its line was over the bound"
        );
        assert!(eventually(WAIT, || watcher.running()).await, "the turn-state after the long line was read");
        let view = told.view();
        assert!(view.ask(4).is_none());
        let first = view.ask(1).expect("ask 1 is open");
        assert_eq!(first.input(), json!({"command": "cargo test "}), "the command comes through byte for byte, trailing space and all");
        assert_eq!(first.input_json.as_ref(), r#"{"command":"cargo test "}"#, "and is held as the text it came as");
        assert_eq!((first.tool.as_deref(), first.call_id.as_deref(), first.structural), (Some("bash"), Some("c1"), false));
        assert_eq!(view.asks().len(), 4);
        assert_eq!(view.seq(), 4);
        let told_asks = told.asks();
        assert_eq!(told_asks.iter().map(|a| a.ask_id).collect::<Vec<_>>(), [1, 2, 3, 5]);
        assert!(told_asks.windows(2).all(|pair| pair[0].seen_seq < pair[1].seen_seq), "each ask is stamped later than the one before it");
        assert_eq!(first.reason.as_deref(), Some("outside-cwd"));
        assert_eq!(told_asks[0].origin, Some(Origin::Model), "the origin told is the one at the time of telling; the view's is read at match time");
        watcher.close().await;
    }

    /// The stand-in's own rule: an ask is answered once. The first answer settles it (the sink hears that), a second is a 409, so is an
    /// ask the script never opened, and every answer, the note included, is in the door's log. Without a script a repeat is a 409 too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_answer_is_a_409_and_the_answer_log_has_the_note() {
        let script = scripted(&[call_line("c1", "model"), ask_line(1, "c1", "ls"), ask_line(2, "c1", "pwd")], &[]);
        let door = Door::start(&script.env);
        let told = Arc::new(Told::default());
        let watcher = watch_door(&door, told.clone()).await;
        door.say();
        assert!(eventually(WAIT, || told.view().asks().len() == 2).await);

        let yes = door.post("/api/answer", json!({"ask_id": 1, "answer": "yes", "note": "allowed by the operator's rule"}));
        assert_eq!(yes.status, 204, "{}", yes.head);
        assert!(eventually(WAIT, || told.events().contains(&Ev::Settled(1))).await, "{:?}", told.events());
        let again = door.post("/api/answer", json!({"ask_id": 1, "answer": "yes"}));
        assert_eq!(again.status, 409, "{}", again.head);
        assert_eq!(door.post("/api/answer", json!({"ask_id": 77, "answer": "yes"})).status, 409, "an ask the script never opened");
        assert_eq!(door.post("/api/answer", json!({"ask_id": 2, "answer": "no"})).status, 204, "the other ask is still open");
        let log = door.log_lines();
        assert!(log.contains(&"answer ask_id=1 answer=yes note=\"allowed by the operator's rule\"".to_string()), "{log:?}");
        assert!(log.contains(&"answer ask_id=2 answer=no note=-".to_string()), "{log:?}");
        assert_eq!(log.iter().filter(|l| l.starts_with("answer-refused ")).count(), 2, "{log:?}");
        assert!(eventually(WAIT, || told.events().contains(&Ev::Settled(2))).await);
        assert!(told.view().asks().is_empty());
        watcher.close().await;

        // No script at all: an id is taken once, as before, and no more.
        let plain = Door::start(&[]);
        assert_eq!(plain.post("/api/answer", json!({"ask_id": 5, "answer": "yes"})).status, 204);
        assert_eq!(plain.post("/api/answer", json!({"ask_id": 5, "answer": "yes"})).status, 409);
    }

    /// The real door's other refusal: an answer the ask did not offer is a 422 and the ask stays open (an approval offers its `answers`, a
    /// question the `label` of each of its `options`), so a rule that answers with the wrong word learns it was not accepted. Mutation:
    /// settle on any answer (the shape this replaced) and the first assertion below goes red.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_answer_the_ask_did_not_offer_is_a_422_and_the_ask_stays_open() {
        let question = json!({
            "type": "ask", "kind": "question", "ask_id": 2, "prompt": "pick one",
            "options": [{"label": "red", "description": "warm"}, {"label": "blue"}]
        })
        .to_string();
        let script = scripted(&[call_line("c1", "model"), ask_line(1, "c1", "ls"), question], &[]);
        let door = Door::start(&script.env);
        let told = Arc::new(Told::default());
        let watcher = watch_door(&door, told.clone()).await;
        door.say();
        assert!(eventually(WAIT, || told.view().asks().len() == 2).await);

        let maybe = door.post("/api/answer", json!({"ask_id": 1, "answer": "maybe"}));
        assert_eq!(maybe.status, 422, "{}", maybe.head);
        let green = door.post("/api/answer", json!({"ask_id": 2, "answer": "green"}));
        assert_eq!(green.status, 422, "{}", green.head);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(told.view().asks().len(), 2, "both are still open: {:?}", told.events());
        assert!(!told.events().iter().any(|ev| matches!(ev, Ev::Settled(_))), "{:?}", told.events());
        let log = door.log_lines();
        assert!(log.contains(&"answer-refused ask_id=1 status=422".to_string()), "{log:?}");
        assert!(log.contains(&"answer-refused ask_id=2 status=422".to_string()), "{log:?}");
        assert!(!log.iter().any(|line| line.starts_with("answer ")), "a refused answer is not in the answer log: {log:?}");

        assert_eq!(door.post("/api/answer", json!({"ask_id": 2, "answer": "blue"})).status, 204, "a label the question offered");
        assert_eq!(door.post("/api/answer", json!({"ask_id": 1, "answer": "no"})).status, 204, "an answer the approval offered");
        assert!(eventually(WAIT, || told.view().asks().is_empty()).await);
        assert_eq!(door.post("/api/answer", json!({"ask_id": 1, "answer": "maybe"})).status, 409, "settled is a 409 before it is a 422");
        watcher.close().await;
    }

    /// Ask ids are unique per process, as the real door's are: a script that is read again by a second `say` opens new asks, and an id
    /// that was settled is never open again. Mutation: emit the file as written each time and the second say's ask is `Ask(1)` again, which
    /// the door then answers 409.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_script_read_again_opens_new_ask_ids() {
        let script = scripted(&[call_line("c1", "model"), ask_line(1, "c1", "ls")], &[]);
        let door = Door::start(&script.env);
        let told = Arc::new(Told::default());
        let watcher = watch_door(&door, told.clone()).await;
        door.say();
        assert!(eventually(WAIT, || told.events().contains(&Ev::Ask(1, Some(Origin::Model)))).await, "{:?}", told.events());
        assert_eq!(door.post("/api/answer", json!({"ask_id": 1, "answer": "yes"})).status, 204);
        assert!(eventually(WAIT, || told.events().contains(&Ev::Settled(1))).await);
        door.say();
        assert!(eventually(WAIT, || told.events().contains(&Ev::Ask(2, Some(Origin::Model)))).await, "{:?}", told.events());
        assert_eq!(door.post("/api/answer", json!({"ask_id": 1, "answer": "yes"})).status, 409, "1 is not open again");
        assert_eq!(door.post("/api/answer", json!({"ask_id": 2, "answer": "yes"})).status, 204);
        assert_eq!(told.view().max_ask_id(), 2);
        watcher.close().await;
    }

    // -----------------------------------------------------------------------
    // Reconnect
    // -----------------------------------------------------------------------

    /// `FAKE_LAG=1`: the first say's frames are followed by a `lagged`. The watcher opens another stream within a second, the replay lists
    /// the same asks again, and the sink hears each once. Mutation: do not dedupe by ask id, or do not reconnect, and this goes red.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lag_is_one_reconnect_and_the_replayed_asks_reach_the_sink_once() {
        let script = scripted(&[call_line("c1", "model"), ask_line(1, "c1", "ls"), ask_line(2, "c1", "pwd")], &[("FAKE_LAG", "1")]);
        let door = Door::start(&script.env);
        let told = Arc::new(Told::default());
        let watcher = watch_door(&door, told.clone()).await;
        assert_eq!(door.count("hello-sent"), 1);
        door.say();
        let said = Instant::now();
        assert!(eventually(Duration::from_secs(3), || door.count("hello-sent") == 2).await, "no second stream: {:?}", door.log_lines());
        assert!(said.elapsed() < Duration::from_secs(1), "reconnected {:?} after the lag", said.elapsed());
        tokio::time::sleep(Duration::from_millis(700)).await;
        let each_once = [Ev::Hello, Ev::Ask(1, Some(Origin::Model)), Ev::Ask(2, Some(Origin::Model))];
        assert_eq!(told.events(), each_once, "each ask once, though two streams listed them");
        assert_eq!(door.count("hello-sent"), 2, "one reconnect");
        assert_eq!(door.count("stream-closed"), 1, "and the lagged stream was closed from this side");
        assert_eq!(told.view().asks().len(), 2);
        watcher.close().await;
    }

    /// `turn-state` across the reconnect: what the door said before the lag is still true after it, and the new stream's frames move it.
    /// Restart refuses on this value, so a reconnect that reset it would let a restart cut a turn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn turn_state_is_right_after_a_reconnect() {
        let script = scripted(&[call_line("c1", "model"), ask_line(1, "c1", "ls")], &[("FAKE_LAG", "1"), ("FAKE_SAY_RUNS", "1")]);
        let door = Door::start(&script.env);
        let watcher = watch_door(&door, Arc::new(Told::default())).await;
        assert!(!watcher.running());
        door.say();
        assert!(eventually(WAIT, || watcher.running()).await, "the say started a turn");
        assert!(eventually(Duration::from_secs(3), || door.count("hello-sent") == 2).await, "{:?}", door.log_lines());
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(watcher.running(), "still running on the new stream");
        assert_eq!(door.post("/api/cancel", json!({})).status, 204);
        assert!(eventually(WAIT, || !watcher.running()).await, "the turn-state false on the second stream is read");
        watcher.close().await;
    }

    fn marker_line(kind: &str) -> String {
        json!({"type": kind, "text": "go"}).to_string()
    }

    /// A replay never clears `running`: the door's log, which is what a replay is made of, can be missing the turn the door is in, so a
    /// replay that ends on a settled turn says nothing about it. The stand-in's replay carries the turn markers in order, as the real
    /// door's does; the say starts a turn, the script settles one in the log, the lag reconnects, and the turn is still running until
    /// the live `turn-state` says otherwise. Mutation: infer idle from a settled replay (the shape this replaced) and the first
    /// assertion after the reconnect goes red.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_replay_that_ends_on_a_settled_turn_does_not_clear_running() {
        let lines = [call_line("c1", "model"), marker_line("user-message"), marker_line("turn-settled"), ask_line(1, "c1", "ls")];
        let script = scripted(&lines, &[("FAKE_LAG", "1"), ("FAKE_SAY_RUNS", "1")]);
        let door = Door::start(&script.env);
        let watcher = watch_door(&door, Arc::new(Told::default())).await;
        door.say();
        assert!(eventually(WAIT, || watcher.running()).await, "the say started a turn");
        assert!(eventually(Duration::from_secs(3), || door.count("hello-sent") == 2).await, "{:?}", door.log_lines());
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(watcher.running(), "the replay's settled turn does not clear it");
        assert_eq!(door.post("/api/cancel", json!({})).status, 204);
        assert!(eventually(WAIT, || !watcher.running()).await, "the live turn-state does");
        watcher.close().await;
    }

    /// And a replay whose log ends on a turn begun and not settled sets it: a watcher that came up after the turn started (this one saw no
    /// `turn-state`, the stand-in is not told to run turns) learns of it from the replay. Mutation: never set it and this goes red.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_replay_that_ends_on_a_turn_begun_sets_running() {
        let lines = [call_line("c1", "model"), marker_line("user-message"), ask_line(1, "c1", "ls")];
        let script = scripted(&lines, &[("FAKE_LAG", "1")]);
        let door = Door::start(&script.env);
        let watcher = watch_door(&door, Arc::new(Told::default())).await;
        door.say();
        assert!(eventually(Duration::from_secs(3), || door.count("hello-sent") == 2).await, "{:?}", door.log_lines());
        assert!(eventually(WAIT, || watcher.running()).await, "the replay ended on a turn begun");
        watcher.close().await;
    }

    /// A close that lands as the watcher is pausing to reconnect: nothing is opened after it, no stream is left on the door, and the
    /// door's SIGTERM ends it at once with its token file gone — the stop tests' promise, with a reconnect in the way. Mutation: let the
    /// pause and the connect ignore the close flag and a stream is opened behind the close, so the `hello-sent` count below goes red.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_close_during_the_backoff_never_reopens_and_the_door_stops_cleanly() {
        let script = scripted(&[call_line("c1", "model"), ask_line(1, "c1", "ls")], &[("FAKE_LAG", "1")]);
        let mut door = Door::start(&script.env);
        let watcher = watch_door(&door, Arc::new(Told::default())).await;
        door.say();
        assert!(eventually(WAIT, || door.count("stream-closed") == 1).await, "the lagged stream was closed: {:?}", door.log_lines());
        // The watcher has ended the lagged stream from its side and is pausing (250 ms) before the next one; the close lands in the pause.
        let opened = door.count("hello-sent");
        assert_eq!(opened, 1, "the pause had not run out yet: {:?}", door.log_lines());
        watcher.close().await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(door.count("hello-sent"), opened, "no stream was opened after the close: {:?}", door.log_lines());
        assert_eq!(door.count("stream-closed"), opened, "and none is left open");

        let status = door.sigterm();
        assert!(status.success(), "{status:?}");
        assert!(!door.token_file.exists(), "the door removed its token file");
        let log = door.log_lines();
        assert!(log.contains(&"sigterm-received streams=0".to_string()), "{log:?}");
    }

    /// A sink that takes two seconds over an ask: the frames after it keep flowing (the turn-state is read while it sleeps), and a close does
    /// not wait for it. One runtime thread on purpose: a sink called on it, from the reader or from a task, would stop everything else.
    /// Mutation: call the sink on the runtime instead of a blocking thread and this goes red.
    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_sink_does_not_stall_the_frames_behind_it() {
        use std::sync::atomic::{AtomicBool, Ordering};
        #[derive(Default)]
        struct Slow {
            began: AtomicBool,
            done: AtomicBool,
        }
        impl Sink for Slow {
            fn ask(&self, _key: DoorKey, _ask: &AskNote) {
                self.began.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_secs(2));
                self.done.store(true, Ordering::SeqCst);
            }
        }
        let script = scripted(&[call_line("c1", "model"), ask_line(1, "c1", "ls"), "#sleep 100".to_string(), turn_line(true)], &[]);
        let door = Door::start(&script.env);
        let slow = Arc::new(Slow::default());
        let watcher = watch_door(&door, slow.clone()).await;
        door.say();
        assert!(eventually(WAIT, || slow.began.load(Ordering::SeqCst)).await);
        assert!(eventually(Duration::from_millis(1500), || watcher.running()).await, "the frame after the ask was read while the sink slept");
        assert!(!slow.done.load(Ordering::SeqCst), "the sink was still asleep");
        let closing = Instant::now();
        watcher.close().await;
        assert!(closing.elapsed() < Duration::from_millis(500), "close did not wait for the sink: {:?}", closing.elapsed());
        assert!(eventually(Duration::from_secs(4), || slow.done.load(Ordering::SeqCst)).await, "and the sink was let finish");
    }

    // -----------------------------------------------------------------------
    // A whole perch
    // -----------------------------------------------------------------------

    /// The live check, as a test: a perch in front of the stand-in, a chat, a say whose frames end in a lag. The perch logs one reconnect
    /// and tells its sink of each ask once; and a stop of the chat afterwards is the stop the other tests pin: the watcher on the new stream
    /// is closed first, the door has no stream left when SIGTERM lands, and no token file stays.
    #[test]
    fn a_perch_reconnects_once_tells_each_ask_once_and_still_stops_the_door_cleanly() {
        let script = scripted(&[call_line("c1", "model"), ask_line(1, "c1", "ls"), ask_line(2, "c1", "pwd")], &[("FAKE_LAG", "1")]);
        let mut builder = Perch::builder().hub();
        for (name, value) in &script.env {
            builder = builder.env(*name, value.clone());
        }
        let perch = builder.start();
        let (id, pid, port) = live(&perch);
        let said = perch.post_json(&format!("/s/{id}/api/say"), json!({"text": "go"}));
        assert_eq!(said.status, 202, "{}{}", said.head, said.body);
        let two_streams = |lines: &[String]| lines.iter().filter(|l| *l == "hello-sent").count() >= 2;
        assert!(wait_record(&perch, Duration::from_secs(10), two_streams), "{:?}", perch.fake_records());
        std::thread::sleep(Duration::from_millis(800));
        let err = perch.err();
        assert_eq!(err.matches("the watcher's stream to a door ended; reconnecting").count(), 1, "one reconnect in the perch's log");
        assert_eq!(err.matches("the sink is told of an ask").count(), 2, "and each ask told to the sink once");
        assert!(!err.contains(&perch.door_token(port)), "the door's token is nowhere in the perch's log");

        let stopped = perch.post(&format!("/hub/sessions/{id}/stop"));
        assert_eq!(stopped.status, 202, "{}", stopped.head);
        assert!(wait_gone(pid, Duration::from_secs(5)), "the door is gone, and reaped");
        let records = perch.fake_records();
        let last_close = records.iter().rposition(|line| line == "stream-closed").expect("the watcher was closed");
        let term = records.iter().position(|line| line.starts_with("sigterm ")).expect("the door was signalled");
        assert!(last_close < term, "close the watcher on the new stream, then signal: {records:?}");
        assert!(records.iter().any(|line| line == "sigterm streams=0"), "…with no stream left open: {records:?}");
        assert!(records.iter().any(|line| line == "sigterm-received streams=0"), "{records:?}");
        assert!(perch.door_token_files().is_empty(), "{:?}", perch.door_token_files());
        assert_eq!(perch.state_of(&id).as_deref(), Some("kept"));
    }
}
