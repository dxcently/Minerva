//! `/hub/sessions/<id>/allow` and `/revoke`: allow rules for a chat (slice A1b), over the fake door's `FAKE_ASKS`.
//!
//! The matcher's clauses are unit tests beside it (`src/allow/matcher.rs`), the store's too (`store.rs`), and the answerer is tested
//! against a socket that plays the door (`src/allow/mod.rs`). What is tested here is the whole thing over a real perch and the stand-in
//! door: a scripted ask, a rule made from it, the next ask answered with the note in the body, and the lifetime of a rule (D10 a: memory
//! only, bound to the door).
//!
//! A door's asks come from a file the test owns (`FAKE_ASKS`), read again at every `say`: the first `say` emits the file as written and
//! each later one raises the ask ids by the highest id emitted before, so a script that always writes id 1 for its first ask gets 1, 2, 3
//! ... one `say` after another. What the perch answered is read from the stand-in's own answer log (`answer ask_id=N answer=yes note=..`,
//! and `answer-refused` for a 409 or 422).
//!
//! The first test needs no door. The rest need `--features test-bins`.

mod common;

use common::*;

/// The three routes are behind the token, the host check and the fetch-metadata check, and a chat with no live door is the empty 404 on
/// all of them, a kept chat and a made-up id and a bad id alike. Mutation: answer a route before the chat is looked up and the 404s go.
#[test]
fn the_routes_are_behind_the_gate_and_a_chat_with_no_door_is_the_empty_404() {
    let perch = Perch::start();
    perch.keep_a_log("kept");
    let routes = [("GET", "allow"), ("POST", "allow"), ("POST", "revoke")];
    for chat in ["kept", "nobody", "ctf-pwn-3"] {
        for (method, verb) in routes {
            let path = format!("/hub/sessions/{chat}/{verb}");
            let bare = Request::new(&perch.addr, method, &path).send(&perch.addr);
            assert_eq!((bare.status, bare.body.as_str()), (401, ""), "{method} {path} with no credential");
            let wrong = Request::new(&perch.addr, method, &path).bearer("Bearer nope").send(&perch.addr);
            assert_eq!(wrong.status, 401, "{method} {path} with the wrong token");
            let gated = perch.apireq(method, &path).body(b"{\"ask_id\":1,\"rid\":1}".to_vec()).send(&perch.addr);
            assert_eq!((gated.status, gated.body.as_str()), (404, ""), "{method} {path} for a chat with no door");
            let foreign = perch.apireq(method, &path).header("Sec-Fetch-Site", "cross-site").send(&perch.addr);
            assert_eq!(foreign.status, 403, "{method} {path} from another site");
            let rebound = perch.apireq(method, &path).host("evil.example").send(&perch.addr);
            assert_eq!(rebound.status, 403, "{method} {path} under another name");
        }
    }
    // Paths and methods that are not the routes: never a seam, never an answer of the module's.
    for (method, path) in [
        ("GET", "/hub/sessions/kept/revoke"),
        ("POST", "/hub/sessions/kept/allowed"),
        ("POST", "/hub/sessions/kept/allow/1"),
        ("GET", "/hub/sessions/kept/allow?x=1"),
        ("POST", "/hub/sessions/..%2Fx/allow"),
        ("POST", "/hub/sessions/%2e%2e/allow"),
        ("POST", "/hub/sessions//allow"),
    ] {
        let answer = perch.apireq(method, path).send(&perch.addr);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "{method} {path}");
    }
    for method in ["PUT", "DELETE", "PATCH"] {
        assert_eq!(perch.apireq(method, "/hub/sessions/kept/allow").send(&perch.addr).status, 405, "{method}");
    }
    assert_eq!(perch.apireq("OPTIONS", "/hub/sessions/kept/allow").send(&perch.addr).status, 405);
}

// A plain `cargo test` builds this file with nothing else to run; the helpers are used only by the tests below.
#[cfg(feature = "test-bins")]
mod with_doors {
    use super::common::*;
    use serde_json::{json, Value};
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(10);

    fn body_of_answer(answer: &Answer) -> Value {
        serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON body ({e}): {}{}", answer.head, answer.body))
    }

    /// The note the stand-in's log shows for a rule's answer: `Debug` of the text, as `asks.rs` writes it.
    fn note_in_log() -> String {
        format!("{:?}", perch::allow::RULE_NOTE)
    }

    // -----------------------------------------------------------------------
    // The script a door plays, and a perch to play it
    // -----------------------------------------------------------------------

    fn call(id: &str, tool: &str, origin: &str) -> String {
        json!({"type": "tool-call-started", "id": id, "name": tool, "input": {}, "origin": origin}).to_string()
    }

    fn approval(ask_id: u64, call_id: &str, tool: &str, input: Value, structural: bool) -> String {
        json!({
            "type": "ask", "kind": "approval", "ask_id": ask_id, "call_id": call_id, "tool": tool, "input": input,
            "prompt": format!("{tool}. Run it?"), "reason": "outside-cwd", "structural": structural, "judged": null, "yolo": false,
            "answers": ["yes", "no"]
        })
        .to_string()
    }

    fn bash(ask_id: u64, call_id: &str, command: &str) -> String {
        approval(ask_id, call_id, "bash", json!({ "command": command }), false)
    }

    /// A model's bash call and the ask about it: the two lines a door says.
    fn model_bash(ask_id: u64, call_id: &str, command: &str) -> Vec<String> {
        vec![call(call_id, "bash", "model"), bash(ask_id, call_id, command)]
    }

    /// A perch in front of the stand-in door, whose asks are read from `script` at every `say`.
    struct Rig {
        // Declared first so it is dropped first: the perch is gone before the files it was pointed at.
        perch: Perch,
        script: PathBuf,
        env: Vec<(String, String)>,
        _dir: tempfile::TempDir,
        home: Option<PathBuf>,
    }

    /// One chat of a rig.
    struct Chat {
        id: String,
        pid: u32,
        port: u16,
    }

    /// A perch whose doors are the stand-in, with `script` as their asks, `env` as their environment and, for a home, the Minerva home
    /// `minerva` (its parent is `--root`). The same call again is a perch restart over the same home.
    fn start_perch(script: &Path, env: &[(String, String)], minerva: Option<&Path>) -> Perch {
        let mut builder = Perch::builder().hub().env("FAKE_ASKS", script.display().to_string());
        for (name, value) in env {
            builder = builder.env(name.clone(), value.clone());
        }
        if let Some(minerva) = minerva {
            builder = builder.member_config("").arg("--root").arg(minerva.parent().unwrap()).arg("--home").arg(minerva);
        }
        builder.start()
    }

    impl Rig {
        fn new(env: &[(&str, &str)]) -> Rig {
            Self::build(env, false)
        }

        /// With a Minerva home (`default_mode = auto`), which is what a manual chat, a fork and a perch restart need.
        fn with_home(env: &[(&str, &str)]) -> Rig {
            Self::build(env, true)
        }

        fn build(env: &[(&str, &str)], home: bool) -> Rig {
            let dir = tempfile::tempdir().unwrap();
            let script = dir.path().join("asks");
            std::fs::write(&script, "").unwrap();
            let env: Vec<(String, String)> = env.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect();
            let mut minerva = None;
            if home {
                let home_dir = dir.path().join("base").join("Minerva");
                std::fs::create_dir_all(&home_dir).unwrap();
                std::fs::write(home_dir.join("minerva.toml"), "default_model = \"mock\"\ndefault_mode = \"auto\"\n").unwrap();
                minerva = Some(home_dir.canonicalize().unwrap());
            }
            Rig { perch: start_perch(&script, &env, minerva.as_deref()), script, env, _dir: dir, home: minerva }
        }

        /// A new chat in `mode` (`"yolo"` is the default of a perch with no home).
        fn chat(&self, mode: &str) -> Chat {
            let mut body = json!({ "cwd": self.perch.work.display().to_string(), "mode": mode });
            if self.home.is_some() {
                body["model"] = json!("mock");
            }
            let made = self.perch.post_json("/hub/sessions", body);
            assert_eq!(made.status, 201, "{}{}", made.head, made.body);
            let (pid, port) = self.perch.fake_last_start();
            Chat { id: body_id(&made), pid, port }
        }

        /// The door plays `lines` (written to the script file first), and says so.
        fn say(&self, chat: &Chat, lines: &[String]) {
            std::fs::write(&self.script, lines.join("\n")).unwrap();
            let said = self.perch.post_json(&format!("/s/{}/api/say", chat.id), json!({ "text": "go" }));
            assert_eq!(said.status, 202, "{}{}", said.head, said.body);
        }

        /// How many asks the sink has been told of so far: the perch says each in its log, after the watcher holds it.
        fn told(&self) -> usize {
            self.perch.err().matches("the sink is told of an ask").count()
        }

        fn wait_told(&self, n: usize) {
            assert!(wait_until(WAIT, || self.told() >= n), "the sink was told of {} asks, not {n}: {}", self.told(), self.perch.err());
        }

        /// The stand-in's answer log, in order: `answer ...` and `answer-refused ...`.
        fn answers(&self) -> Vec<String> {
            self.perch.fake_records().into_iter().filter(|line| line.starts_with("answer")).collect()
        }

        fn answered(&self, ask_id: u64) -> Vec<String> {
            self.answers().into_iter().filter(|line| line.starts_with(&format!("answer ask_id={ask_id} "))).collect()
        }

        fn wait_answered(&self, ask_id: u64) -> String {
            assert!(wait_until(WAIT, || !self.answered(ask_id).is_empty()), "no answer for ask {ask_id}: {:?}", self.answers());
            self.answered(ask_id).remove(0)
        }

        fn allow(&self, chat: &Chat, body: Value) -> Answer {
            self.perch.post_json(&format!("/hub/sessions/{}/allow", chat.id), body)
        }

        fn revoke(&self, chat: &Chat, rid: u64) -> Answer {
            self.perch.post_json(&format!("/hub/sessions/{}/revoke", chat.id), json!({ "rid": rid }))
        }

        fn listed(&self, chat: &Chat) -> Value {
            self.perch.listed(&chat.id)
        }

        /// `POST /allow` for `ask_id`, expected to make a rule and answer: the rule.
        fn allowed(&self, chat: &Chat, ask_id: u64) -> Value {
            let made = self.allow(chat, json!({ "ask_id": ask_id }));
            assert_eq!(made.status, 201, "{}{}", made.head, made.body);
            let body = body_of_answer(&made);
            assert_eq!(body["answered"], "yes", "{body}");
            body["rule"].clone()
        }
    }

    impl Perch {
        /// `GET /hub/sessions/<id>/allow`, expected to be a 200.
        fn listed(&self, id: &str) -> Value {
            let answer = self.get(&format!("/hub/sessions/{id}/allow"));
            assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
            assert_eq!(answer.header("content-type").as_deref(), Some("application/json"));
            body_of_answer(&answer)
        }
    }

    fn wait_until(within: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A moment in which a thing that must not happen could have: the sink has been told, and then some.
    fn settle() {
        std::thread::sleep(Duration::from_millis(500));
    }

    fn rules_of(listed: &Value) -> &Vec<Value> {
        listed["rules"].as_array().unwrap_or_else(|| panic!("a rules array in {listed}"))
    }

    fn files_under(root: &Path, into: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(root) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => files_under(&path, into),
                Ok(kind) if kind.is_file() => into.push(path),
                _ => {}
            }
        }
    }

    fn question(ask_id: u64, call_id: &str) -> String {
        json!({
            "type": "ask", "kind": "question", "ask_id": ask_id, "call_id": call_id, "prompt": "which?",
            "options": [{ "label": "a" }, { "label": "b" }]
        })
        .to_string()
    }

    /// The `ask-settled` frame for `ask_id` that a pane is handed, waited for.
    fn settled_frame(pane: &mut Pane, ask_id: u64) -> Option<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(frame) = pane.frames().into_iter().find(|f| f["type"] == "ask-settled" && f["ask_id"] == ask_id) {
                return Some(frame);
            }
            if Instant::now() >= deadline || pane.eof {
                return None;
            }
            pane.pump();
        }
    }

    /// Sends SIGCONT to a stopped door when dropped, so a failed assertion cannot leave one frozen.
    struct Thaw(u32);

    impl Drop for Thaw {
        fn drop(&mut self) {
            unsafe { libc::kill(self.0 as i32, libc::SIGCONT) };
        }
    }

    /// Every file under `roots` (named relative to its root) whose path says "allow" or whose bytes hold `marker`.
    fn files_that_keep_a_rule(roots: &[&Path], marker: &str) -> Vec<String> {
        let mut found = Vec::new();
        for root in roots {
            let mut files = Vec::new();
            files_under(root, &mut files);
            for file in files {
                let relative = file.strip_prefix(root).unwrap_or(&file).display().to_string();
                let bytes = std::fs::read(&file).unwrap_or_default();
                let holds = bytes.windows(marker.len()).any(|window| window == marker.as_bytes());
                if relative.to_ascii_lowercase().contains("allow") || holds {
                    found.push(format!("{} ({})", file.display(), if holds { "holds the command" } else { "named for a rule" }));
                }
            }
        }
        found
    }

    // -----------------------------------------------------------------------
    // Allow, and the next ask
    // -----------------------------------------------------------------------

    /// The live check, as a test. An ask is allowed (the perch answers it yes itself, with the note), and the same command asked again by
    /// the model is answered by the perch with the note in the body, with no request from anyone. The rule says what it was made from, its
    /// use is counted after the door took the answer, the settled frame a pane would see carries the note (the page's "allowed by this
    /// chat's rule"), and neither the door's token nor the command is in the perch's log.
    /// Mutations: answer without the note; never answer a second ask; count a use before the door took the answer; log the command.
    #[test]
    fn a_rule_answers_its_own_ask_and_then_the_next_identical_one_with_the_note() {
        let rig = Rig::new(&[]);
        let chat = rig.chat("auto");
        let command = "cargo test --marker-1f4e";
        rig.say(&chat, &model_bash(1, "c1", command));
        rig.wait_told(1);
        assert_eq!(rig.listed(&chat), json!({ "mode": "auto", "rules": [] }));

        let made = rig.allow(&chat, json!({ "ask_id": 1 }));
        assert_eq!(made.status, 201, "{}{}", made.head, made.body);
        assert_eq!(made.header("content-type").as_deref(), Some("application/json"));
        let body = body_of_answer(&made);
        assert_eq!(body["answered"], "yes");
        let rule = &body["rule"];
        let cwd = std::fs::canonicalize(&rig.perch.work).unwrap().display().to_string();
        assert_eq!((&rule["tool"], &rule["origin"], &rule["command"], &rule["cwd"], &rule["uses"]), (&json!("bash"), &json!("model"), &json!(command), &json!(cwd), &json!(0)));
        assert!(rule["added_ms"].as_u64().unwrap() > 0 && rule.get("last_ms").is_none(), "{rule}");
        let rid = rule["rid"].as_u64().unwrap();
        // The first ask was answered by the perch, once, with the note.
        assert_eq!(rig.wait_answered(1), format!("answer ask_id=1 answer=yes note={}", note_in_log()));
        let listed = rig.listed(&chat);
        assert_eq!(listed["mode"], "auto");
        assert_eq!(rules_of(&listed).len(), 1);
        assert_eq!(listed["rules"][0]["rid"], rid);

        // The same command, again, from the model: answered with no request from the test.
        let mut pane = Pane::open(&rig.perch, &chat.id);
        rig.say(&chat, &model_bash(1, "c2", command));
        assert_eq!(rig.wait_answered(2), format!("answer ask_id=2 answer=yes note={}", note_in_log()));
        let settled = settled_frame(&mut pane, 2).unwrap_or_else(|| panic!("ask 2 settled on the pane: {:?}", pane.data().0));
        assert_eq!(
            (settled["how"].as_str(), settled["answer"].as_str(), settled["note"].as_str()),
            (Some("answered"), Some("yes"), Some(perch::allow::RULE_NOTE)),
            "the settled frame carries the note the page shows"
        );
        assert!(wait_until(WAIT, || rig.listed(&chat)["rules"][0]["uses"] == 1), "the use is counted: {}", rig.listed(&chat));
        let used = &rig.listed(&chat)["rules"][0];
        assert!(used["last_ms"].as_u64().unwrap() >= used["added_ms"].as_u64().unwrap(), "{used}");

        assert_eq!(rig.answers().len(), 2, "one answer for each ask and no other: {:?}", rig.answers());
        let log = rig.perch.err();
        assert!(!log.contains(&rig.perch.door_token(chat.port)), "the door's token is in the perch's log");
        assert!(!log.contains("marker-1f4e"), "the operator's command is in the perch's log");
        assert!(log.contains("a rule answers an ask"), "the perch says it answered, by ids and tool only");
    }

    /// A revoke ends the rule at once: the same command is then left to the person (and is still pending, so it can be allowed again,
    /// which is a new rule with a new id). A rid is revoked once, from its own chat, and a bad body is a 400.
    /// Mutations: keep the rule after a revoke; reuse a rid; revoke from another chat.
    #[test]
    fn a_revoke_stops_the_rule_and_the_ask_is_left_to_the_person() {
        let rig = Rig::new(&[]);
        let (one, other) = (rig.chat("auto"), rig.chat("auto"));
        let command = "make check";
        rig.say(&one, &model_bash(1, "c1", command));
        rig.wait_told(1);
        let rid = rig.allowed(&one, 1)["rid"].as_u64().unwrap();
        rig.wait_answered(1);

        // Another chat cannot revoke it, and neither can a wrong rid or a bad body.
        assert_eq!(rig.revoke(&other, rid).status, 404);
        assert_eq!(rig.revoke(&one, rid + 1).status, 404);
        for bad in [json!({}), json!({ "rid": "1" }), json!({ "rid": -1 }), json!({ "rid": 1.5 }), json!([1]), json!("x")] {
            let refused = rig.perch.post_json(&format!("/hub/sessions/{}/revoke", one.id), bad.clone());
            assert_eq!((refused.status, refused.header("content-type").as_deref()), (400, Some("application/json")), "{bad}: {}", refused.body);
        }
        assert_eq!(rules_of(&rig.listed(&one)).len(), 1, "none of that touched the rule");
        let revoked = rig.revoke(&one, rid);
        assert_eq!((revoked.status, revoked.body.as_str()), (204, ""));
        assert_eq!(rig.revoke(&one, rid).status, 404, "a rid is revoked once");
        assert_eq!(rig.listed(&one)["rules"], json!([]));

        rig.say(&one, &model_bash(1, "c2", command));
        rig.wait_told(2);
        settle();
        assert!(rig.answered(2).is_empty(), "the revoked rule answered: {:?}", rig.answers());
        let again = rig.allowed(&one, 2);
        assert!(again["rid"].as_u64().unwrap() > rid, "a revoked rid is not handed out again");
    }

    /// Everything that is almost the command is left to the person, and the exact one that follows is answered, so the silence is the
    /// rule's and not a stalled pipeline: a trailing space, a script's nested call, a background call (as `true` and as `"true"`), a
    /// field the rule never saw, an ask whose call was never seen (unknown origin), another tool of the same name but for case, and a
    /// question. The sink hears them in order, so by the time the last is answered the rest have been decided.
    #[test]
    fn near_misses_are_left_to_the_person_and_the_exact_one_is_answered() {
        let rig = Rig::new(&[]);
        let chat = rig.chat("auto");
        rig.say(&chat, &model_bash(1, "c1", "cargo test"));
        rig.wait_told(1);
        rig.allowed(&chat, 1);
        rig.wait_answered(1);

        let near = vec![
            call("c2", "bash", "model"),
            bash(1, "c2", "cargo test "),
            call("c3", "bash", "script"),
            bash(2, "c3", "cargo test"),
            call("c4", "bash", "model"),
            approval(3, "c4", "bash", json!({ "command": "cargo test", "background": true }), false),
            call("c5", "bash", "model"),
            approval(4, "c5", "bash", json!({ "command": "cargo test", "background": "true" }), false),
            call("c6", "bash", "model"),
            approval(5, "c6", "bash", json!({ "command": "cargo test", "workdir": "/" }), false),
            // No call frame for this one: its origin is unknown.
            bash(6, "never-started", "cargo test"),
            call("c7", "Bash", "model"),
            approval(7, "c7", "Bash", json!({ "command": "cargo test" }), false),
            question(8, "c9"),
            call("c8", "bash", "model"),
            bash(9, "c8", "cargo test"),
        ];
        rig.say(&chat, &near);
        // Ask ids 2.. in the order written: the exact one is the ninth, so id 10.
        rig.wait_told(1 + 9);
        let line = rig.wait_answered(10);
        assert!(line.contains(&note_in_log()), "{line}");
        settle();
        assert_eq!(rig.answers().len(), 2, "only the first ask (by hand) and the exact one: {:?}", rig.answers());
        assert!(wait_until(WAIT, || rig.listed(&chat)["rules"][0]["uses"] == 1), "{}", rig.listed(&chat));
    }

    /// The body of `POST /allow` is read for `ask_id` and nothing else: a body that names another tool, another command, another origin,
    /// another folder and a rid cannot shape the rule, which is the watcher's copy of the ask. The rule made answers the command asked
    /// about, not the one the body named. Bad bodies are refused before anything is made.
    #[test]
    fn a_body_that_names_a_tool_a_command_or_an_origin_cannot_shape_the_rule() {
        let rig = Rig::new(&[]);
        let chat = rig.chat("auto");
        rig.say(&chat, &model_bash(1, "c1", "cargo test"));
        rig.wait_told(1);

        for bad in [json!({}), json!({ "ask_id": "1" }), json!({ "ask_id": -1 }), json!({ "ask_id": 1.5 }), json!({ "ask_id": null }), json!([1]), json!(1)] {
            let refused = rig.allow(&chat, bad.clone());
            assert_eq!(refused.status, 400, "{bad}: {}{}", refused.head, refused.body);
        }
        let not_json = rig.perch.apireq("POST", &format!("/hub/sessions/{}/allow", chat.id)).body(b"ask_id=1".to_vec()).send(&rig.perch.addr);
        assert_eq!(not_json.status, 400);
        let big = rig.allow(&chat, json!({ "ask_id": 1, "pad": "x".repeat(5000) }));
        assert_eq!((big.status, big.body.as_str()), (413, ""), "a body over 4 KiB is refused unread");
        assert_eq!(rig.listed(&chat)["rules"], json!([]), "none of those made a rule");
        assert!(rig.answers().is_empty(), "or answered anything");

        let hostile = json!({
            "ask_id": 1, "tool": "write", "command": "rm -rf /", "origin": "user", "cwd": "/", "structural": false, "background": false,
            "input": { "command": "rm -rf /" }, "rid": 99, "mode": "yolo", "kind": "question", "uses": 50, "note": "mine",
        });
        let made = rig.allow(&chat, hostile);
        assert_eq!(made.status, 201, "{}{}", made.head, made.body);
        let rule = body_of_answer(&made)["rule"].clone();
        let cwd = std::fs::canonicalize(&rig.perch.work).unwrap().display().to_string();
        assert_eq!((&rule["tool"], &rule["origin"], &rule["command"], &rule["cwd"], &rule["uses"]), (&json!("bash"), &json!("model"), &json!("cargo test"), &json!(cwd), &json!(0)));
        assert_ne!(rule["rid"], 99);
        assert_eq!(rig.wait_answered(1), format!("answer ask_id=1 answer=yes note={}", note_in_log()), "and the note is the perch's, not the body's");

        let mut script = model_bash(1, "c2", "rm -rf /");
        script.extend(model_bash(2, "c3", "cargo test"));
        rig.say(&chat, &script);
        rig.wait_answered(3);
        settle();
        assert!(rig.answered(2).is_empty(), "the command the body named was answered: {:?}", rig.answers());
        assert_eq!(rig.answers().len(), 2);
    }

    /// Every ask a rule cannot be made from is refused with its own status, makes no rule and answers nothing: a question (409), a
    /// structural ask by name (422), a background call (422), a call of unknown origin (422), a bash call with a field a rule never saw
    /// (422), an ask that is not there (409). Mutations: make a name rule from a structural ask; allow a background call.
    #[test]
    fn what_a_rule_cannot_be_made_from_is_refused_and_answers_nothing() {
        let rig = Rig::new(&[]);
        let chat = rig.chat("auto");
        let script = vec![
            call("c1", "bash", "model"),
            question(1, "c1"),
            call("c2", "write", "model"),
            approval(2, "c2", "write", json!({ "path": "/etc/passwd" }), true),
            call("c3", "bash", "model"),
            approval(3, "c3", "bash", json!({ "command": "sleep 99", "background": true }), false),
            approval(4, "never-started", "bash", json!({ "command": "ls" }), false),
            call("c5", "bash", "model"),
            approval(5, "c5", "bash", json!({ "command": "ls", "env": { "A": "1" } }), false),
            call("c6", "bash", "model"),
            approval(6, "c6", "bash", json!({ "command": ["ls"] }), false),
        ];
        rig.say(&chat, &script);
        rig.wait_told(6);
        let refused = |ask_id: u64| {
            let answer = rig.allow(&chat, json!({ "ask_id": ask_id }));
            (answer.status, answer.header("content-type"), body_of_answer(&answer)["error"].as_str().unwrap_or("").to_string())
        };
        let (status, kind, why) = refused(1);
        assert_eq!((status, kind.as_deref()), (409, Some("application/json")));
        assert!(why.contains("question"), "{why}");
        let (status, _, why) = refused(2);
        assert_eq!(status, 422);
        assert!(why.contains("shape"), "{why}");
        let (status, _, why) = refused(3);
        assert_eq!(status, 422);
        assert!(why.contains("background"), "{why}");
        let (status, _, why) = refused(4);
        assert_eq!(status, 422);
        assert!(why.contains("not known"), "{why}");
        assert_eq!(refused(5).0, 422);
        assert_eq!(refused(6).0, 422);
        assert_eq!(refused(99).0, 409, "an ask the door never made");
        assert_eq!(refused(0).0, 409);
        assert_eq!(rig.listed(&chat)["rules"], json!([]));
        assert!(rig.answers().is_empty(), "a refusal answers nothing: {:?}", rig.answers());
    }

    /// A yolo chat has nothing to allow: the door answers every ask itself. The route says so (409), the list says yolo, and nothing is
    /// answered. (The stand-in does not answer for a yolo door; the perch is what must not.)
    #[test]
    fn a_yolo_chat_has_nothing_to_allow() {
        let rig = Rig::new(&[]);
        let chat = rig.chat("yolo");
        rig.say(&chat, &model_bash(1, "c1", "cargo test"));
        rig.wait_told(1);
        let refused = rig.allow(&chat, json!({ "ask_id": 1 }));
        assert_eq!(refused.status, 409, "{}{}", refused.head, refused.body);
        assert!(refused.body.contains("yolo"), "{}", refused.body);
        assert_eq!(rig.listed(&chat), json!({ "mode": "yolo", "rules": [] }));
        settle();
        assert!(rig.answers().is_empty(), "{:?}", rig.answers());
    }

    /// At most 32 rules per chat: the 33rd is a 409 and its ask stays the person's, a revoke makes room, and a rule that says what another
    /// already says is not a second one (200, not 201). Another chat has its own 32.
    #[test]
    fn a_chat_holds_at_most_32_rules() {
        let rig = Rig::new(&[]);
        let chat = rig.chat("auto");
        let first: Vec<String> = (1..=30).flat_map(|n| model_bash(n, &format!("c{n}"), &format!("cmd-{n}"))).collect();
        rig.say(&chat, &first);
        rig.wait_told(30);
        let mut rids = Vec::new();
        for n in 1..=30 {
            rids.push(rig.allowed(&chat, n)["rid"].as_u64().unwrap());
        }
        let second: Vec<String> = (1..=3).flat_map(|n| model_bash(n, &format!("d{n}"), &format!("cmd-{}", 30 + n))).collect();
        rig.say(&chat, &second);
        rig.wait_told(33);
        rig.allowed(&chat, 31);
        rig.allowed(&chat, 32);
        assert_eq!(rules_of(&rig.listed(&chat)).len(), 32);
        let full = rig.allow(&chat, json!({ "ask_id": 33 }));
        assert_eq!(full.status, 409, "{}{}", full.head, full.body);
        assert!(full.body.contains("32 rules"), "{}", full.body);
        assert!(rig.answered(33).is_empty(), "the refused ask is the person's");
        assert_eq!(rig.answers().len(), 32);

        assert_eq!(rig.revoke(&chat, rids[0]).status, 204);
        let fits = rig.allowed(&chat, 33);
        assert!(fits["rid"].as_u64().unwrap() > rids[29]);
        assert_eq!(rules_of(&rig.listed(&chat)).len(), 32);

        // Another chat has its own count.
        let other = rig.chat("auto");
        assert_eq!(rig.listed(&other)["rules"], json!([]), "another chat has none of these");
    }

    /// A rule made from an ask whose command is the same as a rule's already is the same rule (200, the same rid), and the second ask
    /// is answered once: an ask told twice or allowed twice is not answered twice.
    #[test]
    fn the_same_rule_twice_is_one_rule_and_each_ask_is_answered_once() {
        let rig = Rig::new(&[]);
        let chat = rig.chat("auto");
        // Two asks about the same command, both pending when the first is allowed.
        let mut script = model_bash(1, "c1", "make");
        script.extend(model_bash(2, "c2", "make"));
        rig.say(&chat, &script);
        rig.wait_told(2);
        let rid = rig.allowed(&chat, 1)["rid"].as_u64().unwrap();
        rig.wait_answered(1);
        settle();
        assert!(rig.answered(2).is_empty(), "an ask that was already pending is not the new rule's: {:?}", rig.answers());
        let again = rig.allow(&chat, json!({ "ask_id": 2 }));
        assert_eq!(again.status, 200, "the same rule is not made twice: {}{}", again.head, again.body);
        assert_eq!(body_of_answer(&again)["rule"]["rid"], rid);
        assert_eq!(body_of_answer(&again)["answered"], "yes");
        assert_eq!(rules_of(&rig.listed(&chat)).len(), 1);
        // Ask 1 is gone, so allowing it again is a 409, whichever of "settled" and "being answered" says so.
        assert_eq!(rig.allow(&chat, json!({ "ask_id": 1 })).status, 409);
        settle();
        assert_eq!((rig.answered(1).len(), rig.answered(2).len()), (1, 1), "{:?}", rig.answers());
    }

    // -----------------------------------------------------------------------
    // Whose rule it is, and how long it lives (D10 a: in memory, bound to the door)
    // -----------------------------------------------------------------------

    /// A rule belongs to the chat it was made in: the same command in another chat, in the same folder and mode, is left to the person,
    /// and that chat's own rule answers only that chat. Mutations: key a rule by cwd and mode, not by door; list every chat's rules.
    #[test]
    fn a_rule_answers_only_its_own_chat() {
        let rig = Rig::new(&[]);
        let (one, two) = (rig.chat("auto"), rig.chat("auto"));
        let command = "cargo test -p perch";
        rig.say(&one, &model_bash(1, "c1", command));
        rig.wait_told(1);
        rig.allowed(&one, 1);
        assert_eq!(rig.answers().len(), 1);

        rig.say(&two, &model_bash(1, "c1", command));
        rig.wait_told(2);
        settle();
        assert_eq!(rig.answers().len(), 1, "the other chat's rule answered: {:?}", rig.answers());
        assert_eq!(rig.listed(&two)["rules"], json!([]));
        assert_eq!(rules_of(&rig.listed(&one)).len(), 1);

        // Its own rule, made from its own pending ask, answers it, and then answers only it.
        rig.allowed(&two, 1);
        assert_eq!(rig.answers().len(), 2);
        let before = rig.told();
        rig.say(&one, &model_bash(1, "c2", command));
        rig.wait_told(before + 1);
        assert!(wait_until(WAIT, || rig.answers().len() == 3), "{:?}", rig.answers());
        rig.say(&two, &model_bash(1, "c2", command));
        assert!(wait_until(WAIT, || rig.answers().len() == 4), "{:?}", rig.answers());
        assert!(
            wait_until(WAIT, || [&one, &two].iter().all(|chat| rig.listed(chat)["rules"][0]["uses"] == 1)),
            "each rule answered one ask of its own chat: {} {}",
            rig.listed(&one),
            rig.listed(&two)
        );
    }

    /// D10 (a): every way the chat's door goes away ends its rules. A stop and a resume, a switch of model and a switch of mode (the
    /// last two are restarts) each give the chat a new door, and a new door starts with no rules: the same command is the person's
    /// again, and the list is empty. Each round proves the rule was live before the door went (it answered a second ask), so a list that is
    /// empty because it never worked cannot pass. Mutations: carry rules to a door with the same chat id; end them on a stop only.
    #[test]
    fn a_stop_a_model_switch_and_a_mode_switch_each_end_the_rules() {
        let rig = Rig::with_home(&[]);
        let mut chat = rig.chat("auto");
        let command = "cargo test --workspace";
        let mut answers = 0;
        let mut mode = "auto";
        for (round, event) in ["stop and resume", "model", "manual", "auto"].into_iter().enumerate() {
            // The door as it is: no rule, so its ask is the person's; then a rule from that ask, which answers the next one.
            let before = rig.told();
            rig.say(&chat, &model_bash(1, "c1", command));
            rig.wait_told(before + 1);
            settle();
            assert_eq!(rig.answers().len(), answers, "round {round}: a new door has no rule, so the ask is left to the person: {:?}", rig.answers());
            assert_eq!(rig.listed(&chat)["rules"], json!([]), "round {round}");
            rig.allowed(&chat, 1);
            answers += 1;
            rig.say(&chat, &model_bash(1, "c2", command));
            assert!(wait_until(WAIT, || rig.answers().len() == answers + 1), "round {round}: the rule answers the same command: {:?}", rig.answers());
            answers += 1;
            assert_eq!(rules_of(&rig.listed(&chat)).len(), 1, "round {round}");

            chat = match event {
                "stop and resume" => {
                    assert_eq!(rig.perch.post(&format!("/hub/sessions/{}/stop", chat.id)).status, 202);
                    assert!(wait_gone(chat.pid, WAIT), "the door went");
                    let gone = rig.perch.get(&format!("/hub/sessions/{}/allow", chat.id));
                    assert_eq!((gone.status, gone.body.as_str()), (404, ""), "a chat with no door has no list");
                    let resumed = rig.perch.post(&format!("/hub/sessions/{}/resume", chat.id));
                    assert_eq!(resumed.status, 200, "{}{}", resumed.head, resumed.body);
                    let (pid, port) = rig.perch.fake_last_start();
                    Chat { id: chat.id.clone(), pid, port }
                }
                other => {
                    let body = match other {
                        "model" => json!({ "model": "mock:beta" }),
                        word => {
                            mode = word;
                            json!({ "mode": word })
                        }
                    };
                    let restarted = rig.perch.post_json(&format!("/hub/sessions/{}/restart", chat.id), body.clone());
                    assert_eq!(restarted.status, 200, "{body}: {}{}", restarted.head, restarted.body);
                    let (pid, port) = rig.perch.fake_last_start();
                    Chat { id: chat.id.clone(), pid, port }
                }
            };
            assert_eq!(rig.listed(&chat)["rules"], json!([]), "round {round}: after {event}");
            assert_eq!(rig.listed(&chat)["mode"], mode, "round {round}: after {event}");
        }
        assert!(
            wait_until(WAIT, || rig.perch.err().matches("its allow rules ended with it").count() >= 4),
            "each door that went took its rule with it, and the perch said so"
        );
    }

    /// A fork is a new chat on a copy of the log: it starts with no rules, and the chat it was copied from keeps its own and goes on
    /// answering. Mutation: copy the rules to the fork.
    #[test]
    fn a_fork_starts_with_no_rules_and_the_source_keeps_its_own() {
        let rig = Rig::with_home(&[]);
        let source = rig.chat("auto");
        let command = "make -j8";
        rig.say(&source, &model_bash(1, "c1", command));
        rig.wait_told(1);
        rig.allowed(&source, 1);
        let made = rig.perch.post(&format!("/hub/sessions/{}/fork", source.id));
        assert_eq!(made.status, 201, "{}{}", made.head, made.body);
        let (pid, port) = rig.perch.fake_last_start();
        let fork = Chat { id: body_id(&made), pid, port };
        assert_ne!(fork.id, source.id);
        assert_eq!(rig.listed(&fork), json!({ "mode": "auto", "rules": [] }));

        rig.say(&fork, &model_bash(1, "c2", command));
        rig.wait_told(2);
        settle();
        assert_eq!(rig.answers().len(), 1, "the fork answered by its source's rule: {:?}", rig.answers());
        assert_eq!(rules_of(&rig.listed(&source)).len(), 1, "the source keeps its rule");
        rig.say(&source, &model_bash(1, "c3", command));
        assert!(wait_until(WAIT, || rig.answers().len() == 2), "and it still answers: {:?}", rig.answers());
    }

    /// D10 (a): a perch restart ends every rule, and nothing is written for the next one to find. After the stop, no file under the
    /// home, `$HOME`, the runtime, data and sessions dirs or the perch's tree has "allow" in its path or holds the command a rule was made
    /// from; the restarted perch lists no rule for the resumed chat and leaves the same command to the person. Mutation: write the rules to
    /// `<home>/allow.json` (the spec's (b)) and the scan finds the file.
    #[test]
    fn a_perch_restart_leaves_no_rule_behind_and_writes_no_allow_file() {
        // The scan finds what it looks for: a file named for a rule, and a file that holds the command.
        let decoy = tempfile::tempdir().unwrap();
        std::fs::write(decoy.path().join("Allow.json"), "{}").unwrap();
        std::fs::create_dir(decoy.path().join("deep")).unwrap();
        std::fs::write(decoy.path().join("deep/notes.txt"), "run printf marker-8c1d4e now").unwrap();
        assert_eq!(files_that_keep_a_rule(&[decoy.path()], "marker-8c1d4e").len(), 2, "the scan sees both kinds of file");

        let userhome = tempfile::tempdir().unwrap();
        let mut rig = Rig::with_home(&[("HOME", userhome.path().to_str().unwrap())]);
        let chat = rig.chat("auto");
        let command = "printf marker-8c1d4e";
        rig.say(&chat, &model_bash(1, "c1", command));
        rig.wait_told(1);
        rig.allowed(&chat, 1);
        rig.say(&chat, &model_bash(1, "c2", command));
        assert!(wait_until(WAIT, || rig.answers().len() == 2), "the rule is live: {:?}", rig.answers());
        let home = rig.home.clone().unwrap();
        let scan = |perch: &Perch| {
            files_that_keep_a_rule(
                &[userhome.path(), home.as_path(), perch.runtime.path(), perch.data.path(), perch.sessions.path(), perch.root.path()],
                "marker-8c1d4e",
            )
        };
        assert_eq!(scan(&rig.perch), Vec::<String>::new(), "while it runs");

        let status = rig.perch.stop();
        assert!(status.success(), "the perch stopped cleanly: {status:?}");
        assert_eq!(scan(&rig.perch), Vec::<String>::new(), "after it stopped");
        rig.perch = start_perch(&rig.script, &rig.env, rig.home.as_deref());

        let kept = rig.perch.get(&format!("/hub/sessions/{}/allow", chat.id));
        assert_eq!((kept.status, kept.body.as_str()), (404, ""), "the chat is kept: it has no door and no list");
        let resumed = rig.perch.post(&format!("/hub/sessions/{}/resume", chat.id));
        assert_eq!(resumed.status, 200, "{}{}", resumed.head, resumed.body);
        let (pid, port) = rig.perch.fake_last_start();
        let chat = Chat { id: chat.id, pid, port };
        assert_eq!(rig.listed(&chat)["rules"], json!([]), "the restarted perch has no rule for the chat");
        rig.say(&chat, &model_bash(1, "c3", command));
        rig.wait_told(1);
        settle();
        assert!(rig.answers().is_empty(), "a rule survived the restart: {:?}", rig.answers());
        assert_eq!(scan(&rig.perch), Vec::<String>::new(), "after the restarted perch ran");
    }

    // -----------------------------------------------------------------------
    // The perch's stop, a reconnect, a long line, and the person who answers first
    // -----------------------------------------------------------------------

    /// A stop signal does not wait for a door that is not answering. The door is frozen with an allow in flight; the perch is told to stop
    /// and the page's request is answered 503 (empty) at once, with the door still frozen, instead of at the answer's own timeout (a 502).
    /// The perch then goes with the door, which is let go to take its SIGTERM. Mutations: an answer that ignores `ctx.stop`.
    #[test]
    fn a_stop_signal_does_not_wait_for_a_door_that_is_not_answering() {
        let mut rig = Rig::new(&[]);
        let chat = rig.chat("auto");
        rig.say(&chat, &model_bash(1, "c1", "cargo build"));
        rig.wait_told(1);
        let thaw = Thaw(chat.pid);
        assert_eq!(unsafe { libc::kill(chat.pid as i32, libc::SIGSTOP) }, 0, "freezing the door");

        let (addr, auth, path) = (rig.perch.addr.clone(), rig.perch.auth(), format!("/hub/sessions/{}/allow", chat.id));
        let waiting = std::thread::spawn(move || {
            let mut sock = std::net::TcpStream::connect(&addr).unwrap();
            sock.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            let body = r#"{"ask_id":1}"#;
            let head = format!(
                "POST {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(head.as_bytes()).unwrap();
            let mut raw = Vec::new();
            let _ = sock.read_to_end(&mut raw);
            (Instant::now(), String::from_utf8_lossy(&raw).to_string())
        });
        // The perch has the rule and is waiting on the door.
        std::thread::sleep(Duration::from_millis(400));
        let (began, perch_pid) = (Instant::now(), rig.perch.child.id() as i32);
        assert_eq!(unsafe { libc::kill(perch_pid, libc::SIGTERM) }, 0);

        let (answered_at, raw) = waiting.join().unwrap();
        let took = answered_at.duration_since(began);
        let line = raw.lines().next().unwrap_or("").to_string();
        assert!(line.contains(" 503 "), "the request in flight is a 503, not what the door's silence would have made: {raw:?}");
        assert!(raw.ends_with("\r\n\r\n"), "and its body is empty: {raw:?}");
        assert!(took < Duration::from_secs(3), "the request waited {took:?} for a door that was frozen");
        drop(thaw);
        let exit_by = Instant::now() + Duration::from_secs(20);
        while rig.perch.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < exit_by, "the perch did not stop once the door could take its SIGTERM");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A stream the door cut with `lagged` is read again from a replay of what is open, and a rule goes on working across it: the rule made
    /// before the first lag answers the ask that comes after the second, each ask is answered once, the sink is told of each once (the
    /// replay lists an ask it already holds, which is not a new ask), and the door never has to refuse a second answer. (An ask the watcher
    /// first learns of in a replay is stamped as pending before any rule and is left to the person, which a unit test in `watch.rs` pins.)
    /// Mutations: no claim set (an ask told twice is answered twice and the stand-in logs the second as refused); rules ended on a reconnect.
    #[test]
    fn a_rule_goes_on_working_across_a_lagged_stream_and_answers_each_ask_once() {
        let rig = Rig::new(&[("FAKE_LAG", "2")]);
        let chat = rig.chat("auto");
        let command = "cargo clippy";
        let hellos = || rig.perch.fake_records().iter().filter(|line| line.starts_with("hello-sent")).count();
        // The watcher is back when the door has sent a hello to a new stream, and has read the replay behind it a moment later.
        let back = |streams: usize| {
            assert!(wait_until(WAIT, || hellos() >= streams), "the watcher is back (stream {streams}): {:?}", rig.perch.fake_records());
            std::thread::sleep(Duration::from_millis(300));
        };
        rig.say(&chat, &model_bash(1, "c1", command));
        back(2);
        rig.allowed(&chat, 1);
        rig.say(&chat, &model_bash(1, "c2", command));
        assert!(wait_until(WAIT, || !rig.answered(2).is_empty()), "ask 2, told on a live stream: {:?}", rig.answers());
        back(3);
        rig.say(&chat, &model_bash(1, "c3", command));
        assert!(wait_until(WAIT, || !rig.answered(3).is_empty()), "ask 3, after two lags: {:?}", rig.answers());
        settle();
        for n in 1..=3 {
            assert_eq!(rig.answered(n).len(), 1, "ask {n} was answered once: {:?}", rig.answers());
        }
        assert!(rig.answers().iter().all(|line| !line.starts_with("answer-refused")), "the door refused an answer: {:?}", rig.answers());
        assert_eq!(rig.told(), 3, "each ask was told to the sink once, replays or not");
        assert_eq!(rules_of(&rig.listed(&chat)).len(), 1);
        assert!(wait_until(WAIT, || rig.listed(&chat)["rules"][0]["uses"] == 2), "{}", rig.listed(&chat));
    }

    /// An ask line over the watcher's bound goes by unread: the perch never holds it, so no rule can be made from it and none answers it,
    /// and the ask after it is read and answered as usual. Mutation: parse the long line (the ask is then answered or allowable).
    #[test]
    fn an_ask_over_the_line_bound_is_left_to_the_person_and_the_next_one_is_answered() {
        let rig = Rig::new(&[]);
        let chat = rig.chat("auto");
        rig.say(&chat, &model_bash(1, "c1", "cargo test"));
        rig.wait_told(1);
        rig.allowed(&chat, 1);

        let long = "cargo test ".repeat(10 * 1024);
        assert!(long.len() > 100 * 1024);
        let mut script = model_bash(1, "c2", &long);
        script.extend(model_bash(2, "c3", "cargo test"));
        rig.say(&chat, &script);
        // Ask 2 is the long one and ask 3 the short, both raised by the 1 of the first say.
        assert!(wait_until(WAIT, || !rig.answered(3).is_empty()), "the ask after the long line is answered: {:?}", rig.answers());
        settle();
        assert!(rig.answered(2).is_empty(), "the long ask was answered: {:?}", rig.answers());
        let refused = rig.allow(&chat, json!({ "ask_id": 2 }));
        assert_eq!(refused.status, 409, "{}{}", refused.head, refused.body);
        assert_eq!(rig.answers().len(), 2);
        assert!(rig.perch.err().contains("went by unread"), "the perch said a line was skipped");
    }

    /// The person's answer is final. Eight asks are answered "no" through the page and allowed at once, so some allows reach the perch
    /// before it has heard the ask settled. Each is a 409 (it is settled) or a rule whose answer is reported `lost`; the door is never
    /// told yes. Mutation: answer without checking the door's reply (a rule that "wins" a race it lost).
    #[test]
    fn the_person_who_answers_first_is_never_overruled() {
        let rig = Rig::new(&[]);
        let chat = rig.chat("auto");
        let script: Vec<String> = (1..=8).flat_map(|n| model_bash(n, &format!("c{n}"), &format!("job-{n}"))).collect();
        rig.say(&chat, &script);
        rig.wait_told(8);
        let mut statuses = Vec::new();
        for n in 1..=8u64 {
            let no = rig.perch.post_json(&format!("/s/{}/api/answer", chat.id), json!({ "ask_id": n, "answer": "no" }));
            assert_eq!(no.status, 204, "ask {n}: {}{}", no.head, no.body);
            let allowed = rig.allow(&chat, json!({ "ask_id": n }));
            match allowed.status {
                409 => {}
                201 => assert_eq!(body_of_answer(&allowed)["answered"], "lost", "ask {n}: {}", allowed.body),
                other => panic!("ask {n}: a {other}: {}{}", allowed.head, allowed.body),
            }
            statuses.push(allowed.status);
        }
        settle();
        let answers = rig.answers();
        for n in 1..=8 {
            let person: Vec<&String> = answers.iter().filter(|line| line.starts_with(&format!("answer ask_id={n} answer=no "))).collect();
            assert_eq!(person.len(), 1, "ask {n}, allows {statuses:?}: {answers:?}");
        }
        assert!(answers.iter().all(|line| !line.contains("answer=yes")), "a rule overruled the person (allows {statuses:?}): {answers:?}");
    }
}
