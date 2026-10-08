//! `/hub/install`: the install notice and the update keys (slice R4), driven through `fake_minerva`.
//!
//! The perch asks the `minerva` named by `--minerva` with five fixed argv. The test perch is not an installed copy (its exe is in a cargo
//! `target/debug`, not `<prefix>/opt/minerva/<ver>/bin/perch`), so a `minerva` beside it is never taken; the tests that need an installed
//! copy make one in a temp dir. The stand-in (`src/bin/fake_minerva.rs`) logs every call as one JSON line (`argv`, `stdin`, the names of its
//! environment variables, `refused`), and a test reads that log to see exactly what the perch ran, with what on stdin, and whether it ran
//! anything at all. Everything that needs the stand-in is gated on `--features test-bins`; the gate and the "no minerva here" answers are not.
//!
//! The arm is a person's step in a terminal (`hoot keys allow-web <FP>`); here it is `POST /hub/install/keys/arm` with the bearer, as the
//! script sends it. Time is real, so the 120 s edge is a unit test in `src/install/mod.rs` and the tests here shorten the arm and the child's
//! timeout with `--knob install_arm_ms=N` and `install_timeout_ms=N`, which can only shorten.

mod common;

use common::*;
use serde_json::{json, Value};

/// The six routes.
const ROUTES: [(&str, &str); 6] = [
    ("GET", "/hub/install"),
    ("GET", "/hub/install/keys"),
    ("POST", "/hub/install/keys/inspect"),
    ("POST", "/hub/install/keys/add"),
    ("POST", "/hub/install/keys/remove"),
    ("POST", "/hub/install/keys/arm"),
];

fn json_of(answer: &Answer) -> Value {
    assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{}", answer.head);
    serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON answer: {e}: {:?}", answer.body))
}

fn why_of(answer: &Answer) -> String {
    json_of(answer)["why"].as_str().unwrap_or_else(|| panic!("a `why` in {:?}", answer.body)).to_string()
}

#[test]
fn install_is_behind_the_token_and_knows_only_its_six_routes() {
    let perch = Perch::start();
    for (method, path) in ROUTES {
        let bare = Request::new(&perch.addr, method, path).send(&perch.addr);
        assert_eq!((bare.status, bare.body.as_str()), (401, ""), "{method} {path} with no credential: {}", bare.head);
        let wrong = Request::new(&perch.addr, method, path).bearer("Bearer nope").send(&perch.addr);
        assert_eq!((wrong.status, wrong.body.as_str()), (401, ""), "{method} {path} with a wrong token: {}", wrong.head);
    }
    // Nobody's: the other verbs a person might hope for (an update, a rotation, jev's setup), another method on a known path, a trailing
    // slash, and the spellings that decode to a route but are not one.
    for (method, path) in [
        ("GET", "/hub/install/jev"),
        ("POST", "/hub/install/jev"),
        ("POST", "/hub/install/update"),
        ("GET", "/hub/install/update"),
        ("POST", "/hub/install/keys/update"),
        ("POST", "/hub/install/keys/remove-all"),
        ("POST", "/hub/install/keys/rotate"),
        ("POST", "/hub/install/rollback"),
        ("GET", "/hub/install/keys/"),
        ("POST", "/hub/install/keys/add/x"),
        ("POST", "/hub/install/keys"),
        ("POST", "/hub/install"),
        ("GET", "/hub/install/keys/add"),
        ("GET", "/hub/install/keys/arm"),
        ("GET", "/hub/installer"),
        ("GET", "/hub/install%2Fkeys"),
        ("GET", "/hub/%69nstall"),
        ("GET", "/hub/install/%6Beys"),
        ("POST", "/hub/install/keys/%61dd"),
    ] {
        let answer = perch.apireq(method, path).send(&perch.addr);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "{method} {path}: {}", answer.head);
    }
    // `serve` strips one slash below a namespace, so the bare path with a trailing slash is the bare path (the same route, not another)
    assert_eq!(perch.get("/hub/install/").status, perch.get("/hub/install").status);
    // a query is never part of a route here
    for (method, path) in [("GET", "/hub/install?x=1"), ("GET", "/hub/install/keys?"), ("POST", "/hub/install/keys/add?x=1"), ("POST", "/hub/install/keys/arm?confirm=1")] {
        let answer = perch.apireq(method, path).send(&perch.addr);
        assert_eq!((answer.status, answer.body.as_str()), (400, ""), "{method} {path}");
    }
    assert_eq!(perch.apireq("GET", "/hub/install/jev?x=1").send(&perch.addr).status, 404);
}

/// No `minerva` beside the perch and none named: the notice says so, and every key route is a 404 with a sentence. A `--minerva` that is
/// not an executable file is the same as none.
#[test]
fn without_a_minerva_the_notice_says_so_and_every_key_route_is_a_404_with_a_sentence() {
    let dir = tempfile::tempdir().unwrap();
    let plain_file = dir.path().join("minerva");
    std::fs::write(&plain_file, "#!/bin/sh\nexit 0\n").unwrap();
    let missing = dir.path().join("nowhere/minerva");
    for flag in [None, Some(missing), Some(plain_file)] {
        let mut builder = Perch::builder();
        if let Some(path) = &flag {
            builder = builder.arg("--minerva").arg(path);
        }
        let perch = builder.start();
        let notice = perch.get("/hub/install");
        assert_eq!(notice.status, 200, "{}", notice.head);
        assert_eq!(json_of(&notice), json!({"installed": false, "why": "no installed Minerva here (development server)"}), "{flag:?}");
        for (method, path) in &ROUTES[1..] {
            let body = json!({"pubkey": "k", "confirm": "0F7A3A5C447EC7EF", "id": "0123456789ABCDEF", "fingerprint": "0F7A3A5C447EC7EF"});
            let answer = perch.apireq(method, path).body(serde_json::to_vec(&body).unwrap()).send(&perch.addr);
            assert_eq!(answer.status, 404, "{method} {path} {flag:?}: {}", answer.head);
            assert_eq!(why_of(&answer), "no installed Minerva here (development server)");
        }
    }
}

#[cfg(feature = "test-bins")]
mod fake {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    const ID: &str = "0123456789ABCDEF";
    /// The fingerprint of the key under test, as a person types it, and as the perch shows it.
    const FP_TYPED: &str = "0f7a 3a5c 447e c7ef";
    const FP: &str = "0F7A3A5C447EC7EF";
    const FP_SHOWN: &str = "0F7A 3A5C 447E C7EF";
    const COMPILED: &str = "compiled in";
    const BY_HAND: &str = "added by hand";
    const ROTATION: &str = "added by a signed rotation";
    const RELEASE: (&str, &str) = ("AAAAAAAAAAAAAAAA", "1111222233334444");
    const ROTATED: (&str, &str) = ("BBBBBBBBBBBBBBBB", "5555666677778888");
    const HAND: (&str, &str) = ("CCCCCCCCCCCCCCCC", "9999aaaabbbbcccc");

    /// A perch whose `minerva` is the stand-in over a fixture dir, and the dir. `perch` is declared first so it is gone before its dir is.
    struct Rig {
        perch: Perch,
        dir: tempfile::TempDir,
    }

    impl Rig {
        fn start() -> Self {
            Self::with(&[])
        }

        /// `knobs` are `--knob` words. The fixture starts with the compiled-in release key, a key from a rotation and one added by hand.
        fn with(knobs: &[&str]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let fixture = dir.path().join("fixture");
            std::fs::create_dir_all(fixture.join("keys")).unwrap();
            // Variables a perch may have in its environment and a child of it must never see.
            let mut builder = Perch::builder()
                .arg("--minerva")
                .arg(fake_minerva().expect("built with --features test-bins"))
                .env("MINERVA_FAKE_DIR", fixture.display().to_string())
                .env("MINERVA_NO_UPDATE_CHECK", "1")
                .env("MINERVA_KEYS_DIR", "/sentinel/keys")
                .env("ANTHROPIC_API_KEY", "sentinel-provider-key")
                .env("HTTPS_PROXY", "http://127.0.0.1:9")
                .env("SSH_AUTH_SOCK", "/sentinel/agent.sock");
            for knob in knobs {
                builder = builder.arg("--knob").arg(*knob);
            }
            let rig = Rig { perch: builder.start(), dir };
            rig.key(RELEASE.0, RELEASE.1, "the release key", COMPILED);
            rig.key(ROTATED.0, ROTATED.1, "second release key", ROTATION);
            rig.key(HAND.0, HAND.1, "a member", BY_HAND);
            rig
        }

        fn fixture(&self) -> PathBuf {
            self.dir.path().join("fixture")
        }

        fn key(&self, id: &str, fp: &str, comment: &str, source: &str) {
            let keys = self.fixture().join("keys");
            std::fs::write(keys.join(format!("{id}.pub")), pubkey(id, fp, comment)).unwrap();
            std::fs::write(keys.join(format!("{id}.source")), source).unwrap();
        }

        fn behave(&self, word: &str) {
            std::fs::write(self.fixture().join("behave"), word).unwrap();
        }

        /// Every call the perch made to the stand-in, in order. A line still being written has no newline yet, so only whole lines count.
        fn calls(&self) -> Vec<Value> {
            let text = std::fs::read_to_string(self.fixture().join("log")).unwrap_or_default();
            text.split_inclusive('\n')
                .filter(|line| line.ends_with('\n'))
                .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("a log line: {e}: {line:?}")))
                .collect()
        }

        /// The calls whose argv starts with these words.
        fn calls_of(&self, prefix: &[&str]) -> Vec<Value> {
            self.calls().into_iter().filter(|call| argv(call).iter().map(String::as_str).take(prefix.len()).eq(prefix.iter().copied())).collect()
        }

        /// Every file of the fixture's keys dir, by name: equal before and after means nothing was written, changed or removed.
        fn snapshot(&self) -> BTreeMap<String, String> {
            let mut found = BTreeMap::new();
            for entry in std::fs::read_dir(self.fixture().join("keys")).unwrap().flatten() {
                found.insert(entry.file_name().to_string_lossy().into_owned(), std::fs::read_to_string(entry.path()).unwrap());
            }
            found
        }

        fn arm(&self, fingerprint: &str) -> Answer {
            self.perch.post_json("/hub/install/keys/arm", json!({ "fingerprint": fingerprint }))
        }

        fn add(&self, pubkey: &str, confirm: &str) -> Answer {
            self.perch.post_json("/hub/install/keys/add", json!({ "pubkey": pubkey, "confirm": confirm }))
        }

        fn remove(&self, id: &str, confirm: &str) -> Answer {
            self.perch.post_json("/hub/install/keys/remove", json!({ "id": id, "confirm": confirm }))
        }
    }

    /// The stand-in's key text.
    fn pubkey(id: &str, fp: &str, comment: &str) -> String {
        format!("untrusted comment: {comment}\nFAKE {id} {fp}\n")
    }

    fn argv(call: &Value) -> Vec<String> {
        call["argv"].as_array().unwrap().iter().map(|word| word.as_str().unwrap().to_string()).collect()
    }

    /// A page's cookie, got the way a browser gets it: a code from the bearer, redeemed with no credential.
    fn page_cookie(perch: &Perch) -> String {
        let code = json_of(&perch.post("/hub/code"))["code"].as_str().unwrap().to_string();
        let redeemed = Request::new(&perch.addr, "POST", "/hub/redeem").body(serde_json::to_vec(&json!({ "code": code })).unwrap()).send(&perch.addr);
        assert_eq!(redeemed.status, 204, "{}", redeemed.head);
        redeemed.header("set-cookie").unwrap().split(';').next().unwrap().to_string()
    }

    /// A request as the page makes it: the cookie and `Sec-Fetch-Site: same-origin`, which a script cannot leave out or change.
    fn as_page(perch: &Perch, cookie: &str, method: &str, path: &str, body: Value) -> Answer {
        let request = Request::new(&perch.addr, method, path).header("Cookie", cookie).header("Sec-Fetch-Site", "same-origin");
        let request = if body.is_null() { request } else { request.body(serde_json::to_vec(&body).unwrap()) };
        request.send(&perch.addr)
    }

    // -----------------------------------------------------------------------------------------------------------------------------------
    // The notice and the list
    // -----------------------------------------------------------------------------------------------------------------------------------

    #[test]
    fn the_notice_is_what_minerva_status_says_and_only_that_is_ever_asked() {
        let rig = Rig::start();
        let got = rig.perch.get("/hub/install");
        assert_eq!(got.status, 200, "{}", got.head);
        assert_eq!(
            json_of(&got),
            json!({"installed": true, "version": "0.1.0-gabc1234", "notice": null,
                   "plugins": [{"name": "browser", "version": "0.3.0", "how": "stock"}], "jev": {"set_up": true, "running": true, "pid": 4242}}),
        );
        let calls = rig.calls();
        assert_eq!(calls.len(), 1, "one status call for one request");
        assert_eq!(argv(&calls[0]), ["status", "--json"]);
        assert_eq!(calls[0]["stdin"], Value::Null, "the key routes' stdin is not the status's");

        // newer, equal, stale, and the corrupt or absent cache (minerva says `null`)
        let status = |notice: Value| {
            let text = json!({"version": "0.1.0", "notice": notice, "plugins": [], "jev": {"set_up": false, "running": false}}).to_string();
            std::fs::write(rig.fixture().join("status.json"), text).unwrap();
            let got = rig.perch.get("/hub/install");
            assert_eq!(got.status, 200, "{}", got.head);
            json_of(&got)
        };
        let newer = json!({"line": "Minerva 0.2.0 available -- run: hoot update", "latest": "0.2.0", "stale": null, "checked": 1791269513u64});
        assert_eq!(status(newer.clone())["notice"], newer);
        let equal = json!({"line": null, "latest": "0.1.0", "stale": null, "checked": 1791269513u64});
        assert_eq!(status(equal.clone())["notice"], equal, "equal: a notice record with no line");
        let stale = json!({"line": "release info is stale (signed 2026-08-01) -- the maintainer should re-sign", "latest": null,
                           "stale": "release info is stale (signed 2026-08-01) -- the maintainer should re-sign", "checked": 5});
        assert_eq!(status(stale.clone())["notice"], stale);
        let corrupt = status(Value::Null);
        assert_eq!((&corrupt["installed"], &corrupt["notice"], &corrupt["plugins"]), (&json!(true), &Value::Null, &json!([])), "a corrupt cache is no notice");
        assert_eq!(corrupt["jev"], json!({"set_up": false, "running": false}));
        // nothing but `status --json` was ever run for any of them: not an update, not a check
        let calls = rig.calls();
        assert_eq!(calls.len(), 5);
        assert!(calls.iter().all(|call| argv(call) == ["status", "--json"]), "{calls:?}");
    }

    /// What minerva printed is read as the typed shape of the contract and re-serialised: an extra field, a control character or a
    /// bidirectional override in a string does not reach the page.
    #[test]
    fn the_status_is_re_serialised_and_not_relayed() {
        let rig = Rig::start();
        let text = json!({"version": "1.0\u{202e}", "token": "SECRET-EXTRA",
                          "notice": {"line": "a\u{1b}[31mred", "latest": "x", "stale": null, "checked": 1, "extra": "SECRET-EXTRA"},
                          "plugins": [{"name": "browser", "version": "1", "how": "stock", "extra": "SECRET-EXTRA"}],
                          "jev": {"set_up": true, "running": false, "pid": 7, "extra": "SECRET-EXTRA"}})
        .to_string();
        std::fs::write(rig.fixture().join("status.json"), text).unwrap();
        let got = rig.perch.get("/hub/install");
        assert_eq!(got.status, 200, "{}", got.head);
        assert!(!got.body.contains("SECRET-EXTRA"), "{}", got.body);
        let v = json_of(&got);
        assert_eq!(v["version"], "1.0");
        assert_eq!(v["notice"]["line"], "a[31mred");
        assert_eq!(v["plugins"], json!([{"name": "browser", "version": "1", "how": "stock"}]));
        assert_eq!(v["jev"], json!({"set_up": true, "running": false}), "a pid is only for a service that runs");
        assert!(!got.body.contains('\u{1b}') && !got.body.contains('\u{202e}'));
    }

    #[test]
    fn a_status_the_perch_cannot_read_is_a_502_with_a_sentence() {
        let rig = Rig::start();
        for text in ["", "not json", "[]", "{}", r#"{"version":"1"}"#, r#"{"version":1,"notice":null,"plugins":[],"jev":{"set_up":false,"running":false}}"#] {
            std::fs::write(rig.fixture().join("status.json"), text).unwrap();
            let got = rig.perch.get("/hub/install");
            assert_eq!(got.status, 502, "{text:?}: {}", got.body);
            assert!(!why_of(&got).is_empty());
        }
        std::fs::remove_file(rig.fixture().join("status.json")).unwrap();
        for word in ["garbage", "empty", "big", "lie-ok"] {
            rig.behave(word);
            let got = rig.perch.get("/hub/install");
            assert_eq!(got.status, 502, "{word}: {}", got.body);
        }
        rig.behave("");
        assert_eq!(rig.perch.get("/hub/install").status, 200, "and the perch is as it was");
    }

    #[test]
    fn a_minerva_that_does_not_answer_is_a_504_and_is_killed() {
        let rig = Rig::with(&["install_timeout_ms=500"]);
        rig.behave("hang");
        let started = Instant::now();
        let got = rig.perch.get("/hub/install");
        assert_eq!(got.status, 504, "{}", got.body);
        assert!(started.elapsed() < Duration::from_secs(5), "the timeout held: {:?}", started.elapsed());
        let pid = rig.calls().last().unwrap()["pid"].as_u64().unwrap() as u32;
        assert!(wait_gone(pid, Duration::from_secs(5)), "the child that did not answer is gone");
    }

    #[test]
    fn the_key_list_is_minervas_rows_in_the_form_the_page_reads() {
        let rig = Rig::start();
        let got = rig.perch.get("/hub/install/keys");
        assert_eq!(got.status, 200, "{}", got.head);
        assert_eq!(
            json_of(&got),
            json!([
                {"id": RELEASE.0, "fingerprint": "1111 2222 3333 4444", "state": "trusted", "source": COMPILED, "expires": null},
                {"id": ROTATED.0, "fingerprint": "5555 6666 7777 8888", "state": "trusted", "source": ROTATION, "expires": null},
                {"id": HAND.0, "fingerprint": "9999 AAAA BBBB CCCC", "state": "trusted", "source": BY_HAND, "expires": null},
            ])
        );
        assert_eq!(rig.calls().len(), 1);
        assert_eq!(argv(&rig.calls()[0]), ["keys", "list", "--json"]);
        // a list the perch cannot read whole is not shown in part; a label it does not know is `other`
        let row = |state: &str, source: &str| json!({"id": ID, "fingerprint": "0f7a 3a5c 447e c7ef", "state": state, "source": source, "expires": "2028-01-01"});
        let put = |rows: Vec<Value>| std::fs::write(rig.fixture().join("list.json"), Value::Array(rows).to_string()).unwrap();
        put(vec![row("trusted", "added by hand"), row("bogus", "added by hand")]);
        assert_eq!(rig.perch.get("/hub/install/keys").status, 502);
        put(vec![row("trusted", "added by hand\u{202e}"), row("revoked", "from the moon")]);
        let listed = json_of(&rig.perch.get("/hub/install/keys"));
        assert_eq!((&listed[0]["source"], &listed[1]["source"]), (&json!("other"), &json!("other")));
        assert_eq!(listed[0]["expires"], "2028-01-01");
        put(vec![row("trusted", BY_HAND); 257]);
        assert_eq!(rig.perch.get("/hub/install/keys").status, 502, "more rows than a key list holds");
        std::fs::write(rig.fixture().join("list.json"), "{\"keys\":[]}").unwrap();
        assert_eq!(rig.perch.get("/hub/install/keys").status, 502);
    }

    // -----------------------------------------------------------------------------------------------------------------------------------
    // The key goes on stdin; the arm; the guard
    // -----------------------------------------------------------------------------------------------------------------------------------

    #[test]
    fn the_key_is_on_stdin_and_never_in_argv_and_a_confirm_is_always_there() {
        let rig = Rig::start();
        let text = pubkey(ID, FP, "SENTINEL-COMMENT-7c1e");
        let inspected = rig.perch.post_json("/hub/install/keys/inspect", json!({"pubkey": text}));
        assert_eq!(inspected.status, 200, "{}", inspected.body);
        assert_eq!(json_of(&inspected), json!({"id": ID, "fingerprint": FP_SHOWN, "comment": "SENTINEL-COMMENT-7c1e"}));
        assert_eq!(rig.arm(FP_TYPED).status, 200);
        let added = rig.add(&text, "0F7A 3a5c 447E c7ef");
        assert_eq!(added.status, 200, "{}", added.body);
        assert_eq!(json_of(&added), json!({"added": ID}));
        assert!(rig.fixture().join("keys").join(format!("{ID}.pub")).exists(), "the stand-in took the key");
        assert_eq!(rig.arm(FP).status, 200);
        let removed = rig.remove(&ID.to_lowercase(), FP_TYPED);
        assert_eq!(removed.status, 200, "{}", removed.body);
        assert_eq!(json_of(&removed), json!({"removed": ID}));

        let calls = rig.calls();
        let shapes: Vec<Vec<String>> = calls.iter().map(argv).collect();
        // the inspect route; then an add, which looks before it spends (inspect, list) and writes; then a remove, which lists and writes
        assert_eq!(
            shapes,
            [
                vec!["keys", "inspect", "-", "--json"],
                vec!["keys", "inspect", "-", "--json"],
                vec!["keys", "list", "--json"],
                vec!["keys", "add", "-", "--confirm", FP, "--json"],
                vec!["keys", "list", "--json"],
                vec!["keys", "remove", ID, "--confirm", FP, "--json"],
            ]
        );
        for at in [0, 1, 3] {
            assert_eq!(calls[at]["stdin"], text.as_str(), "{:?}: the key is on stdin, whole", argv(&calls[at]));
        }
        assert_eq!((&calls[2]["stdin"], &calls[4]["stdin"], &calls[5]["stdin"]), (&Value::Null, &Value::Null, &Value::Null), "list and remove read nothing");
        for call in &calls {
            assert_eq!(call["refused"], false);
            for word in argv(call) {
                assert!(!word.contains("SENTINEL") && !word.contains("FAKE") && !word.contains("untrusted"), "the key's text is in argv: {word}");
            }
        }
    }

    #[test]
    fn an_inspected_comment_is_cleaned_and_cut() {
        let rig = Rig::start();
        let nasty = format!("evil\u{202e}txt\u{1b}[31m\u{7}\u{200b}{}", "x".repeat(500));
        let got = rig.perch.post_json("/hub/install/keys/inspect", json!({"pubkey": pubkey(ID, FP, &nasty)}));
        assert_eq!(got.status, 200, "{}", got.body);
        let comment = json_of(&got)["comment"].as_str().unwrap().to_string();
        assert!(comment.starts_with("eviltxt[31mxxx"), "{comment:?}");
        assert_eq!(comment.chars().count(), 200);
        assert!(!comment.contains('\u{1b}') && !comment.contains('\u{202e}') && !comment.contains('\u{200b}'));
        // text minerva cannot read as a key is a 422 with a sentence, not a 502 and not an echo
        let bad = rig.perch.post_json("/hub/install/keys/inspect", json!({"pubkey": "SENTINEL-NOT-A-KEY"}));
        assert_eq!(bad.status, 422, "{}", bad.body);
        assert!(!bad.body.contains("SENTINEL"));
    }

    #[test]
    fn an_add_without_an_arm_is_a_403_that_names_the_command_and_runs_nothing() {
        let rig = Rig::start();
        let before = rig.snapshot();
        let refused = rig.add(&pubkey(ID, FP, "k"), FP_TYPED);
        assert_eq!(refused.status, 403, "{}", refused.body);
        assert_eq!(why_of(&refused), format!("in a terminal: hoot keys allow-web {FP_SHOWN}, then add within 2 minutes"));
        // an arm for another fingerprint is no arm for this one
        assert_eq!(rig.arm("1111 2222 3333 4444").status, 200);
        assert_eq!(rig.add(&pubkey(ID, FP, "k"), FP).status, 403);
        let removal = rig.remove(HAND.0, HAND.1);
        assert_eq!(removal.status, 403);
        assert_eq!(why_of(&removal), "in a terminal: hoot keys allow-web 9999 AAAA BBBB CCCC, then remove within 2 minutes");
        assert!(rig.calls().is_empty(), "no child ran for any of them: {:?}", rig.calls());
        assert_eq!(rig.snapshot(), before);
    }

    /// The perch looks at the key before it spends the arm: a pasted key whose own fingerprint is not the typed-back one is a 409 and keeps
    /// the arm, and so does a text that is not a key (a 422), so a junk add can never burn one.
    #[test]
    fn a_wrong_confirm_or_a_text_that_is_not_a_key_is_refused_before_the_arm_is_spent() {
        let rig = Rig::start();
        let before = rig.snapshot();
        // the person armed `other` and typed it back, but the key they pasted is FP's
        let other = "dead beef 0000 1111";
        assert_eq!(rig.arm(other).status, 200);
        let refused = rig.add(&pubkey(ID, FP, "k"), other);
        assert_eq!(refused.status, 409, "{}", refused.body);
        let v = json_of(&refused);
        assert_eq!(v["kind"], "mismatch");
        assert!(!v["why"].as_str().unwrap().is_empty());
        // not a key at all, in a few shapes minerva will not read
        for junk in ["SENTINEL-NOT-A-KEY", "untrusted comment: only a comment\n", "FAKE zz zz\n", "FAKE 0123456789ABCDEF\n"] {
            let refused = rig.add(junk, other);
            assert_eq!(refused.status, 422, "{junk:?}: {}", refused.body);
            assert!(!refused.body.contains("SENTINEL"));
        }
        assert_eq!(rig.snapshot(), before, "the keys dir is untouched");
        assert!(rig.calls_of(&["keys", "add"]).is_empty(), "no write was ever started: {:?}", rig.calls());
        // the arm is still there: a key that really is `other` goes through under it
        let added = rig.add(&pubkey("DDDDDDDDDDDDDDDD", "deadbeef00001111", "k"), other);
        assert_eq!(added.status, 200, "{}", added.body);
        let adds = rig.calls_of(&["keys", "add"]);
        assert_eq!(adds.len(), 1);
        assert_eq!(argv(&adds[0]), ["keys", "add", "-", "--confirm", "DEADBEEF00001111", "--json"]);
        // and that one use was the arm's only use
        assert_eq!(rig.add(&pubkey("EEEEEEEEEEEEEEEE", "deadbeef00001111", "k"), other).status, 403);
        // a right fingerprint, armed, goes through
        assert_eq!(rig.arm(FP).status, 200);
        assert_eq!(rig.add(&pubkey(ID, FP, "k"), FP).status, 200);
    }

    /// An add of a key that is already in the list (by id, or by fingerprint under a fresh id: the same 32 bytes) is a 409 `listed`, runs no
    /// write and keeps the arm. The compiled-in key and a rotation's key are the two that matter.
    #[test]
    fn a_key_that_is_already_listed_is_a_409_listed_that_runs_no_write_and_keeps_the_arm() {
        let rig = Rig::start();
        let before = rig.snapshot();
        let fresh = "DDDDDDDDDDDDDDDD";
        let cases = [
            ("a rotation key again", pubkey(ROTATED.0, ROTATED.1, "again"), ROTATED.1),
            ("the compiled key under a fresh id", pubkey(fresh, RELEASE.1, "same bytes"), RELEASE.1),
            ("the compiled key again", pubkey(RELEASE.0, RELEASE.1, "again"), RELEASE.1),
            ("a key added by hand, again", pubkey(HAND.0, HAND.1, "again"), HAND.1),
            ("a listed id under a new fingerprint", pubkey(HAND.0, "0000111122223333", "forged"), "0000111122223333"),
            ("a rotation key under a fresh id", pubkey(fresh, ROTATED.1, "same bytes"), ROTATED.1),
        ];
        for (what, text, confirm) in &cases {
            assert_eq!(rig.arm(confirm).status, 200);
            // twice: the second finds the arm where the first left it
            for round in 0..2 {
                let refused = rig.add(text, confirm);
                assert_eq!(refused.status, 409, "{what} (round {round}): {}", refused.body);
                let v = json_of(&refused);
                assert_eq!((v["kind"].as_str(), v["why"].as_str()), (Some("listed"), Some("that key is already in the list")), "{what}");
            }
        }
        assert_eq!(rig.snapshot(), before, "the keys dir is untouched");
        assert!(rig.calls_of(&["keys", "add"]).is_empty(), "no write was ever started: {:?}", rig.calls());
        let kinds: Vec<Vec<String>> = rig.calls().iter().map(argv).collect();
        assert!(kinds.iter().all(|call| call[..2] == ["keys", "inspect"] || call[..2] == ["keys", "list"]), "only reads: {kinds:?}");
        // a fingerprint nobody armed is still a 403; armed, a key that is new goes through
        let new = pubkey(fresh, "0123456789abcdef", "new");
        assert_eq!(rig.add(&new, "0123456789abcdef").status, 403);
        assert_eq!(rig.arm("0123456789abcdef").status, 200);
        assert_eq!(rig.add(&new, "0123456789abcdef").status, 200);
        // and the compiled key's arm was never spent: it is still a 409, not a 403
        assert_eq!(rig.add(&pubkey("EEEEEEEEEEEEEEEE", RELEASE.1, "same bytes"), RELEASE.1).status, 409);
    }

    #[test]
    fn the_arm_is_for_a_terminal_and_the_page_cannot_make_one() {
        let rig = Rig::start();
        let p = &rig.perch;
        let arm = "/hub/install/keys/arm";
        let body = serde_json::to_vec(&json!({"fingerprint": FP})).unwrap();
        let cookie = page_cookie(p);
        // the page's cookie: past the gate (same-origin), refused here
        let refused = as_page(p, &cookie, "POST", arm, json!({"fingerprint": FP}));
        assert_eq!(refused.status, 403, "{}", refused.body);
        // and a cookie that is not same-origin does not even pass the gate
        let bare = Request::new(&p.addr, "POST", arm).header("Cookie", &cookie).body(body.clone()).send(&p.addr);
        assert_eq!(bare.status, 401);
        // the bearer from a browser: a page opened with #token= holds it, and a browser says what it is
        let origin = format!("http://{}", p.addr);
        for (name, value) in [("Sec-Fetch-Site", "same-origin"), ("Sec-Fetch-Site", "none"), ("Sec-Fetch-Mode", "cors"), ("Sec-Fetch-Dest", "empty"), ("Origin", origin.as_str())] {
            let refused = p.apireq("POST", arm).header(name, value).body(body.clone()).send(&p.addr);
            assert_eq!(refused.status, 403, "{name}: {value}: {}", refused.body);
        }
        // none of those armed anything
        assert_eq!(rig.add(&pubkey(ID, FP, "k"), FP).status, 403);
        assert!(rig.calls().is_empty());
        // the bearer, as curl sends it
        let ok = p.apireq("POST", arm).body(body.clone()).send(&p.addr);
        assert_eq!(ok.status, 200, "{}", ok.body);
        assert_eq!(json_of(&ok), json!({"armed": true, "fingerprint": FP_SHOWN, "expires_in": 120}));
        // the page can read the notice and the list, and once the terminal has armed it can add: that is the whole design
        assert_eq!(as_page(p, &cookie, "GET", "/hub/install", Value::Null).status, 200);
        assert_eq!(as_page(p, &cookie, "GET", "/hub/install/keys", Value::Null).status, 200);
        let added = as_page(p, &cookie, "POST", "/hub/install/keys/add", json!({"pubkey": pubkey(ID, FP, "k"), "confirm": FP_TYPED}));
        assert_eq!(added.status, 200, "{}", added.body);
        // the body of an arm
        let bad: [Vec<u8>; 9] = [
            Vec::new(),
            b"{}".to_vec(),
            b"not json".to_vec(),
            br#"{"fingerprint":"zz"}"#.to_vec(),
            br#"{"fingerprint":"0F7A3A5C447EC7E"}"#.to_vec(),
            br#"{"fingerprint":"0F7A3A5C447EC7EF0"}"#.to_vec(),
            br#"{"fingerprint":"0F7A3A5C447EC7EF","x":1}"#.to_vec(),
            br#"{"fingerprint":7}"#.to_vec(),
            br#"{"fingerprint":"--confirm"}"#.to_vec(),
        ];
        for body in bad {
            let refused = p.apireq("POST", arm).body(body.clone()).send(&p.addr);
            assert_eq!(refused.status, 400, "{:?}: {}", String::from_utf8_lossy(&body), refused.body);
        }
        let big = p.apireq("POST", arm).body(vec![b' '; 2000]).send(&p.addr);
        assert_eq!(big.status, 413, "{}", big.head);
        // spelled the way a person types it, it is the same fingerprint
        let spelled = rig.arm("  0f7a 3A5C\t447e c7EF ");
        assert_eq!(json_of(&spelled)["fingerprint"], FP_SHOWN);
    }

    #[test]
    fn an_arm_is_spent_by_one_add_and_is_for_its_own_fingerprint() {
        let rig = Rig::start();
        let key = |n: u8| (format!("{n:016X}"), format!("{:016x}", u64::from(n) * 0x1111_1111_1111));
        let (id1, fp1) = key(1);
        let (id2, fp2) = key(2);
        let (id3, fp3) = key(3);
        let (id4, fp4) = key(4);
        assert_eq!(rig.arm(&fp1).status, 200);
        // armed for fp1: a key of another fingerprint cannot ride on it
        assert_eq!(rig.add(&pubkey(&id2, &fp2, "two"), &fp2).status, 403);
        assert!(rig.calls().is_empty());
        // the armed one goes through, once (the key is in the list now, and the second add finds no arm before it finds that)
        assert_eq!(rig.add(&pubkey(&id1, &fp1, "one"), &fp1).status, 200);
        assert_eq!(rig.add(&pubkey(&id1, &fp1, "one"), &fp1).status, 403, "the second add finds the arm spent");
        // arming twice is one arm: it is not two uses
        assert_eq!(rig.arm(&fp3).status, 200);
        assert_eq!(rig.arm(&fp3).status, 200);
        assert_eq!(rig.add(&pubkey(&id3, &fp3, "three"), &fp3).status, 200);
        assert_eq!(rig.add(&pubkey("5555555555555555", &fp3, "three again"), &fp3).status, 403, "one use, not two");
        // two fingerprints, two arms, independent: spending one leaves the other
        assert_eq!(rig.arm(&fp2).status, 200);
        assert_eq!(rig.arm(&fp4).status, 200);
        assert_eq!(rig.add(&pubkey(&id2, &fp2, "two"), &fp2).status, 200);
        assert_eq!(rig.add(&pubkey(&id2, &fp2, "two"), &fp2).status, 403);
        assert_eq!(rig.add(&pubkey(&id4, &fp4, "four"), &fp4).status, 200);
        assert_eq!(rig.calls_of(&["keys", "add"]).len(), 4);
    }

    #[test]
    fn an_arm_runs_out_and_a_knob_cannot_lengthen_it() {
        let rig = Rig::with(&["install_arm_ms=1500"]);
        let armed = rig.arm(FP);
        assert_eq!(json_of(&armed)["expires_in"], 2, "1.5 s is shown as 2");
        assert_eq!(rig.add(&pubkey(ID, FP, "k"), FP).status, 200, "inside the window");
        assert_eq!(rig.arm(FP).status, 200);
        std::thread::sleep(Duration::from_millis(2000));
        let late = rig.add(&pubkey(ID, FP, "k"), FP);
        assert_eq!(late.status, 403, "{}", late.body);
        assert_eq!(rig.calls_of(&["keys", "add"]).len(), 1, "the late add ran nothing");
        // a knob that asks for longer than 120 s gets 120 s
        let long = Rig::with(&["install_arm_ms=99999999"]);
        assert_eq!(json_of(&long.arm(FP))["expires_in"], 120);
    }

    // -----------------------------------------------------------------------------------------------------------------------------------
    // Remove, and what minerva refuses or gets wrong
    // -----------------------------------------------------------------------------------------------------------------------------------

    /// Ruling 6: the page removes what a person added by hand and nothing else. The stand-in would remove a rotation key if asked, so the
    /// refusal seen here is the perch's. What the perch refuses itself does not spend the arm.
    #[test]
    fn remove_is_only_for_keys_added_by_hand_and_a_refusal_of_the_perchs_own_keeps_the_arm() {
        let rig = Rig::start();
        // a key whose source label is almost, but not exactly, "added by hand"
        rig.key("EEEEEEEEEEEEEEEE", "0000000011111111", "odd", "added by hand (probably)");
        let before = rig.snapshot();
        let kind_of = |answer: &Answer| json_of(answer)["kind"].as_str().unwrap().to_string();
        assert_eq!(rig.arm("0000 0000 1111 1111").status, 200);
        let odd = rig.remove("EEEEEEEEEEEEEEEE", "0000 0000 1111 1111");
        assert_eq!((odd.status, kind_of(&odd).as_str()), (409, "failed"), "only the exact label opens the door: {}", odd.body);
        assert_eq!(rig.arm(RELEASE.1).status, 200);
        let compiled = rig.remove(RELEASE.0, RELEASE.1);
        assert_eq!((compiled.status, kind_of(&compiled).as_str()), (409, "compiled"), "{}", compiled.body);
        assert_eq!(rig.arm(ROTATED.1).status, 200);
        let rotated = rig.remove(ROTATED.0, ROTATED.1);
        assert_eq!((rotated.status, kind_of(&rotated).as_str()), (409, "rotation"), "{}", rotated.body);
        // an id nobody has, and a key that is there under the wrong fingerprint
        let strange = "1234 5678 9abc def0";
        assert_eq!(rig.arm(strange).status, 200);
        let missing = rig.remove("FFFFFFFFFFFFFFFF", strange);
        assert_eq!((missing.status, kind_of(&missing).as_str()), (409, "not-found"), "{}", missing.body);
        let wrong = rig.remove(HAND.0, strange);
        assert_eq!((wrong.status, kind_of(&wrong).as_str()), (409, "mismatch"), "{}", wrong.body);
        assert_eq!(rig.snapshot(), before, "nothing changed");
        assert!(rig.calls_of(&["keys", "remove"]).is_empty(), "no remove ever ran: {:?}", rig.calls());
        assert!(rig.calls().iter().all(|call| argv(call) == ["keys", "list", "--json"]), "the perch only read the list");
        // those four refusals left the arm for `strange` alone: it is still good for an add of a key with that fingerprint
        assert_eq!(rig.add(&pubkey("DDDDDDDDDDDDDDDD", "123456789abcdef0", "z"), strange).status, 200);

        // a key added by hand, under its own arm
        assert_eq!(rig.arm(HAND.1).status, 200);
        let removed = rig.remove(&HAND.0.to_lowercase(), HAND.1);
        assert_eq!(removed.status, 200, "{}", removed.body);
        assert_eq!(json_of(&removed), json!({"removed": HAND.0}));
        assert!(!rig.snapshot().contains_key(&format!("{}.pub", HAND.0)), "the key is gone");
        let runs = rig.calls_of(&["keys", "remove"]);
        assert_eq!(runs.len(), 1);
        assert_eq!(argv(&runs[0]), ["keys", "remove", HAND.0, "--confirm", "9999AAAABBBBCCCC", "--json"]);
        assert_eq!(rig.remove(HAND.0, HAND.1).status, 403, "the arm is spent");
    }

    /// What minerva refuses, the page is told, with minerva's own kind and sentence. The perch looks first, so for minerva's `collision` and
    /// `compiled` to be asked at all the list the perch reads is made empty (`list.json`): this is a test of the relay, not of the look.
    #[test]
    fn minervas_refusals_come_back_as_a_409_with_their_kind_and_change_nothing() {
        let rig = Rig::start();
        let before = rig.snapshot();
        std::fs::write(rig.fixture().join("revoked"), "EEEEEEEEEEEEEEEE\n").unwrap();
        std::fs::write(rig.fixture().join("list.json"), "[]").unwrap();
        let mut cases: Vec<(&str, String, String)> = vec![
            ("revoked", pubkey("EEEEEEEEEEEEEEEE", "aaaabbbbccccdddd", "k"), "aaaabbbbccccdddd".into()),
            ("collision", pubkey(HAND.0, "0000111122223333", "forged"), "0000111122223333".into()),
            ("compiled", pubkey(RELEASE.0, RELEASE.1, "k"), RELEASE.1.into()),
        ];
        for (kind, text, confirm) in cases.drain(..) {
            assert_eq!(rig.arm(&confirm).status, 200);
            let refused = rig.add(&text, &confirm);
            assert_eq!(refused.status, 409, "{kind}: {}", refused.body);
            let v = json_of(&refused);
            assert_eq!(v["kind"], kind);
            assert!(!v["why"].as_str().unwrap().is_empty());
            assert_eq!(rig.add(&text, &confirm).status, 403, "{kind}: a refusal that reached minerva spent the arm");
        }
        std::fs::write(rig.fixture().join("unsafe"), "").unwrap();
        assert_eq!(rig.arm(FP).status, 200);
        let unsafe_dir = rig.add(&pubkey(ID, FP, "k"), FP);
        assert_eq!((unsafe_dir.status, json_of(&unsafe_dir)["kind"].as_str()), (409, Some("unsafe")), "{}", unsafe_dir.body);
        assert_eq!(rig.snapshot(), before);
        assert_eq!(rig.calls_of(&["keys", "add"]).len(), 4, "each of the four reached minerva's write");
    }

    /// minerva's sentence is shown whole up to 600 characters, and a longer one is cut and says so.
    #[test]
    fn a_long_refusal_sentence_is_cut_at_600_and_ends_in_an_ellipsis() {
        let rig = Rig::start();
        std::fs::write(rig.fixture().join("revoked"), "EEEEEEEEEEEEEEEE\n").unwrap();
        std::fs::write(rig.fixture().join("list.json"), "[]").unwrap();
        assert_eq!(rig.arm(FP).status, 200);
        std::fs::write(rig.fixture().join("sentence"), "word ".repeat(400)).unwrap();
        let refused = rig.add(&pubkey("EEEEEEEEEEEEEEEE", FP, "k"), FP);
        assert_eq!(refused.status, 409, "{}", refused.body);
        let why = why_of(&refused);
        assert_eq!(why.chars().count(), 600);
        assert!(why.ends_with("word…"), "{}", &why[why.len() - 20..]);
    }

    /// A minerva that prints rubbish, too much, nothing, a success with a failing exit or a refusal with a clean one, or that hangs, has
    /// not added anything as far as the page is told, and the perch is as it was.
    #[test]
    fn a_minerva_that_misbehaves_claims_nothing_and_does_not_hang_the_perch() {
        let rig = Rig::with(&["install_timeout_ms=600", "install_write_cap_ms=1500"]);
        let before = rig.snapshot();
        // the write misbehaves (the reads before it are fine): the arm is spent, so the page is told the change may have been made
        for (word, want) in [("garbage", 502), ("empty", 502), ("big", 502), ("lie-ok", 502), ("lie-error", 502), ("hang", 504)] {
            rig.behave(&format!("write:{word}"));
            assert_eq!(rig.arm(FP).status, 200);
            let started = Instant::now();
            let answer = rig.add(&pubkey(ID, FP, "k"), FP);
            assert_eq!(answer.status, want, "{word}: {}", answer.body);
            assert!(started.elapsed() < Duration::from_secs(5), "{word}: {:?}", started.elapsed());
            let v = json_of(&answer);
            assert_eq!(v["kind"], "unknown", "{word}: no answer to trust is not a refusal");
            assert!(v["why"].as_str().unwrap().ends_with("; list the keys to see whether the change was made"), "{word}: {v}");
            assert!(v.get("added").is_none(), "{word}: {v}");
            assert_eq!(rig.add(&pubkey(ID, FP, "k"), FP).status, 403, "{word}: the write was started, so the arm is spent");
            // a write that is still running holds the one writer lock until its child has exited
            let pid = rig.calls().last().unwrap()["pid"].as_u64().unwrap() as u32;
            assert!(wait_gone(pid, Duration::from_secs(5)), "{word}: the child is gone");
        }
        assert_eq!(rig.snapshot(), before);
        // the hung one was killed by the hard cap (1.5 s here, 60 s in life), and only then was the writer lock let go
        rig.behave("");
        let mut free = false;
        for _ in 0..100 {
            assert_eq!(rig.arm(FP).status, 200);
            let tried = rig.add(&pubkey(ID, FP, "k"), FP);
            if tried.status != 429 {
                assert_eq!(tried.status, 200, "{}", tried.body);
                free = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(free, "the writer lock was let go once the child had exited");
        // a read that misbehaves before the arm is spent is not the arm's loss
        rig.behave("garbage");
        assert_eq!(rig.arm(HAND.1).status, 200);
        let unreadable = rig.remove(HAND.0, HAND.1);
        assert_eq!(unreadable.status, 502);
        assert!(json_of(&unreadable).get("kind").is_none(), "a read that failed is not a write that may have happened: {}", unreadable.body);
        rig.behave("");
        assert_eq!(rig.remove(HAND.0, HAND.1).status, 200, "the arm outlived a list that could not be read");
        assert_eq!(rig.perch.get("/hub/install").status, 200);
    }

    // -----------------------------------------------------------------------------------------------------------------------------------
    // Limits, the child's world, and what is never written down
    // -----------------------------------------------------------------------------------------------------------------------------------

    /// One key write at a time: a second one while the first is running is a 429 and keeps its arm.
    #[test]
    fn one_key_write_at_a_time() {
        let rig = Rig::start();
        let (fp2, id2) = ("1234567890abcdef", "2222222222222222");
        assert_eq!(rig.arm(FP).status, 200);
        assert_eq!(rig.arm(fp2).status, 200);
        rig.behave("write:slow");
        let (addr, auth) = (rig.perch.addr.clone(), rig.perch.auth());
        let first = std::thread::spawn(move || {
            let body = serde_json::to_vec(&json!({"pubkey": pubkey(ID, FP, "k"), "confirm": FP})).unwrap();
            Request::new(&addr, "POST", "/hub/install/keys/add").bearer(auth).body(body).send(&addr)
        });
        std::thread::sleep(Duration::from_millis(250));
        let second = rig.add(&pubkey(id2, fp2, "k"), fp2);
        assert_eq!(second.status, 429, "{}", second.body);
        assert!(!why_of(&second).is_empty());
        assert_eq!(first.join().unwrap().status, 200);
        assert_eq!(rig.add(&pubkey(id2, fp2, "k"), fp2).status, 200, "the refused one kept its arm");
        assert_eq!(rig.calls_of(&["keys", "add"]).len(), 2);
    }

    /// A key write is never killed because its request ended: `minerva` makes the keys directory and then its state file, and a kill between
    /// the two leaves an install that refuses every release. The client goes away while minerva is writing; the child still finishes (the
    /// key is there), the writer lock is held until it has exited (a second write is a 429 meanwhile), and then it is free.
    #[test]
    fn a_key_write_outlives_its_client_and_holds_the_writer_lock_until_it_exits() {
        use std::io::Write as _;
        let rig = Rig::start();
        rig.behave("write:linger");
        let (id2, fp2) = ("2222222222222222", "1234567890abcdef");
        assert_eq!(rig.arm(FP).status, 200);
        assert_eq!(rig.arm(fp2).status, 200);
        // a raw client: send the request, wait until minerva's add has started, hang up
        let body = serde_json::to_vec(&json!({"pubkey": pubkey(ID, FP, "k"), "confirm": FP})).unwrap();
        let request = Request::new(&rig.perch.addr, "POST", "/hub/install/keys/add").bearer(rig.perch.auth()).body(body.clone());
        let mut bytes = request.head().into_bytes();
        bytes.extend_from_slice(&body);
        let mut sock = std::net::TcpStream::connect(&rig.perch.addr).unwrap();
        sock.write_all(&bytes).unwrap();
        let started = Instant::now();
        let add = loop {
            if let Some(call) = rig.calls_of(&["keys", "add"]).into_iter().next() {
                break call;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "the add never started: {:?}", rig.calls());
            std::thread::sleep(Duration::from_millis(10));
        };
        let pid = add["pid"].as_u64().unwrap() as u32;
        drop(sock);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!is_gone(pid), "the child was not killed when its client went");
        let second = rig.add(&pubkey(id2, fp2, "k"), fp2);
        assert_eq!(second.status, 429, "the lock is the child's, not the request's: {}", second.body);
        // it finishes: the key is there although nobody was waiting for the answer
        let key_file = rig.fixture().join("keys").join(format!("{ID}.pub"));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !key_file.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(key_file.exists(), "the write finished");
        assert!(wait_gone(pid, Duration::from_secs(5)), "and the child exited");
        // then the lock is free, and the second add kept its arm through its 429
        let mut status = 429;
        for _ in 0..100 {
            status = rig.add(&pubkey(id2, fp2, "k"), fp2).status;
            if status != 429 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(status, 200);
    }

    /// The request stops waiting at the read timeout (a 504 that says to list the keys); the write does not stop.
    #[test]
    fn a_write_that_outlasts_the_timeout_is_a_504_that_says_to_list_and_still_finishes() {
        let rig = Rig::with(&["install_timeout_ms=400"]);
        rig.behave("write:linger");
        assert_eq!(rig.arm(FP).status, 200);
        let started = Instant::now();
        let answer = rig.add(&pubkey(ID, FP, "k"), FP);
        assert_eq!(answer.status, 504, "{}", answer.body);
        assert!(started.elapsed() < Duration::from_millis(1200), "the answer did not wait for the child: {:?}", started.elapsed());
        let v = json_of(&answer);
        assert_eq!(v["kind"], "unknown");
        assert!(why_of(&answer).ends_with("; list the keys to see whether the change was made"), "{}", answer.body);
        // not killed: the key is in the list a moment later
        let pid = rig.calls_of(&["keys", "add"])[0]["pid"].as_u64().unwrap() as u32;
        assert!(!is_gone(pid), "the child was not killed at the timeout");
        assert!(wait_gone(pid, Duration::from_secs(10)), "it finishes by itself");
        let listed = json_of(&rig.perch.get("/hub/install/keys"));
        assert!(listed.as_array().unwrap().iter().any(|row| row["id"] == ID), "{listed}");
    }

    /// Reads are bounded too: a fifth at once is a 503, and the ones in flight still answer.
    #[test]
    fn reads_are_bounded() {
        let rig = Rig::start();
        rig.behave("slow");
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (addr, auth) = (rig.perch.addr.clone(), rig.perch.auth());
                std::thread::spawn(move || Request::new(&addr, "GET", "/hub/install").bearer(auth).send(&addr).status)
            })
            .collect();
        let statuses: Vec<u16> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(statuses.iter().all(|s| *s == 200 || *s == 503), "{statuses:?}");
        assert!(statuses.contains(&200) && statuses.contains(&503), "{statuses:?}");
        assert!(statuses.iter().filter(|s| **s == 200).count() <= 4, "{statuses:?}");
        rig.behave("");
        assert_eq!(rig.perch.get("/hub/install").status, 200);
    }

    #[test]
    fn the_child_gets_home_the_config_home_and_three_named_minerva_variables_and_nothing_else() {
        let rig = Rig::start();
        assert_eq!(rig.perch.get("/hub/install").status, 200);
        let call = rig.calls().remove(0);
        let names: Vec<&str> = call["env"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
        for passed in ["MINERVA_FAKE_DIR", "MINERVA_NO_UPDATE_CHECK"] {
            assert!(names.contains(&passed), "{passed} did not reach the child: {names:?}");
        }
        for name in &names {
            assert!(["HOME", "XDG_CONFIG_HOME", "MINERVA_HOME", "MINERVA_NO_UPDATE_CHECK", "MINERVA_FAKE_DIR"].contains(name), "{name} reached the child: {names:?}");
        }
        // the perch had these (the harness and the rig set them); none is the child's
        for never in ["ANTHROPIC_API_KEY", "HTTPS_PROXY", "SSH_AUTH_SOCK", "XDG_RUNTIME_DIR", "XDG_DATA_HOME", "RUST_LOG", "PATH", "MINERVA_KEYS_DIR"] {
            assert!(!names.contains(&never), "{never}");
        }
    }

    /// The bearer token is read from the arm request's header and nowhere written; a key is not logged either.
    #[test]
    fn nothing_secret_is_logged_and_the_token_is_nowhere_in_minervas_world() {
        let rig = Rig::start();
        let text = pubkey(ID, FP, "SENTINEL-COMMENT-55aa");
        assert_eq!(rig.perch.post_json("/hub/install/keys/inspect", json!({"pubkey": text})).status, 200);
        assert_eq!(rig.arm(FP).status, 200);
        assert_eq!(rig.add(&text, FP).status, 200);
        assert_eq!(rig.arm(FP).status, 200);
        assert_eq!(rig.remove(ID, FP).status, 200);
        let token = rig.perch.token.trim().to_string();
        std::thread::sleep(Duration::from_millis(300)); // the harness reads the perch's stderr on a thread of its own
        let log = std::fs::read_to_string(rig.fixture().join("log")).unwrap();
        let (out, err) = (rig.perch.out(), rig.perch.err());
        for (what, text) in [("the perch's stdout", &out), ("its stderr, at trace", &err)] {
            assert!(!text.contains(&token), "the token is in {what}");
            assert!(!text.contains("SENTINEL-COMMENT-55aa"), "a key's comment is in {what}");
            assert!(!text.contains("FAKE "), "a key's text is in {what}");
        }
        assert!(!log.contains(&token), "the token reached minerva's argv, stdin or environment");
        assert!(err.contains("update key was"), "the id of a key that changed is logged: {err}");
    }

    #[test]
    fn what_a_request_may_carry_is_bounded_and_refused_before_anything_runs() {
        let rig = Rig::start();
        let p = &rig.perch;
        // an arm, so every refusal below is the request's own and not the missing arm
        assert_eq!(rig.arm(FP).status, 200);
        let post = |path: &str, body: &[u8]| p.apireq("POST", path).body(body.to_vec()).send(&p.addr);
        for path in ["/hub/install/keys/inspect", "/hub/install/keys/add", "/hub/install/keys/remove"] {
            for body in ["", "not json", "[]", "null", "{}", r#"{"pubkey":"k","unknown":1}"#, r#"{"pubkey":7}"#, r#"{"id":"x","confirm":"y","z":1}"#] {
                let answer = post(path, body.as_bytes());
                assert_eq!(answer.status, 400, "{path} {body:?}: {}", answer.body);
            }
        }
        // the key: blank, with a NUL, past 4 KiB
        let add = |text: &str, confirm: &str| rig.add(text, confirm);
        for text in ["".to_string(), "  \n".to_string(), "a\u{0}b".to_string(), "x".repeat(4097), pubkey(ID, FP, &"c".repeat(4100))] {
            assert_eq!(add(&text, FP).status, 400, "{:?}", text.len());
            assert_eq!(p.post_json("/hub/install/keys/inspect", json!({"pubkey": text})).status, 400);
        }
        // the confirmation is 16 hex digits however it is spaced, and nothing else
        for confirm in ["", "zz", "0F7A3A5C447EC7E", "0F7A3A5C447EC7EF0", "0F7A-3A5C-447E-C7EF", "--help", "0F7A3A5C447EC7EF; ls", "0F7A3A5C447EC7E\u{a0}F"] {
            assert_eq!(add(&pubkey(ID, FP, "k"), confirm).status, 400, "{confirm:?}");
            assert_eq!(rig.remove(HAND.0, confirm).status, 400, "{confirm:?}");
        }
        for id in ["", "zz", "../x", "0123456789ABCDE", "0123456789ABCDEF0", "--confirm", "0123456789ABCDE\u{0}"] {
            assert_eq!(rig.remove(id, FP).status, 400, "{id:?}");
        }
        // the body: 32 KiB and a little, and the gate's own 8 MiB
        assert_eq!(post("/hub/install/keys/add", &vec![b' '; 33 * 1024]).status, 413);
        assert_eq!(post("/hub/install/keys/inspect", &vec![b' '; 33 * 1024]).status, 413);
        let auth = p.auth();
        let huge = send_oversize(&p.addr, "POST", "/hub/install/keys/add", Some(auth.as_str()), 8 * 1024 * 1024 + 1);
        assert_eq!(huge.status, 413);
        // exactly 4 KiB is a key
        let pad = 4096 - pubkey(ID, FP, "").len();
        let exact = pubkey(ID, FP, &"c".repeat(pad));
        assert_eq!(exact.len(), 4096);
        assert_eq!(p.post_json("/hub/install/keys/inspect", json!({"pubkey": exact})).status, 200);
        // none of the refusals ran anything (the one inspect above is the only call), and none of them touched the arm
        let calls = rig.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(argv(&calls[0]), ["keys", "inspect", "-", "--json"]);
        assert_eq!(rig.add(&pubkey(ID, FP, "k"), FP).status, 200, "the arm outlived every refusal");
    }

    /// Whatever is thrown at the routes, the only things the perch ever ran are the five argv, and none of them is an update.
    #[test]
    fn no_route_ever_runs_update_or_remove_all() {
        let rig = Rig::start();
        let p = &rig.perch;
        let bodies = [
            json!({"pubkey": pubkey(ID, FP, "k"), "confirm": FP}),
            json!({"id": HAND.0, "confirm": HAND.1}),
            json!({"fingerprint": FP}),
            json!({"pubkey": pubkey(ID, FP, "k")}),
        ];
        let paths = [
            "/hub/install", "/hub/install/keys", "/hub/install/keys/inspect", "/hub/install/keys/add", "/hub/install/keys/remove", "/hub/install/keys/arm",
            "/hub/install/update", "/hub/install/upgrade", "/hub/install/rollback", "/hub/install/jev", "/hub/install/jev-setup", "/hub/install/keys/update",
            "/hub/install/keys/remove-all", "/hub/install/keys/rotate", "/hub/install/keys/rotation", "/hub/install/uninstall", "/hub/install/check",
            "/hub/install/plugins", "/hub/install/services",
        ];
        for round in 0..2 {
            for path in paths {
                for method in ["GET", "POST"] {
                    for body in &bodies {
                        let request = p.apireq(method, path);
                        let request = if method == "POST" { request.body(serde_json::to_vec(body).unwrap()) } else { request };
                        request.send(&p.addr);
                    }
                }
            }
            // and the real flow, once with an arm and once without
            if round == 0 {
                assert_eq!(rig.arm(FP).status, 200);
                assert_eq!(rig.arm(HAND.1).status, 200);
            }
        }
        let is_hex16 = |s: &str| s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase());
        let calls = rig.calls();
        assert!(!calls.is_empty());
        for call in &calls {
            assert_eq!(call["refused"], false, "the stand-in refused an argv the perch ran: {call}");
            let words = argv(call);
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            let ok = match words.as_slice() {
                ["status", "--json"] | ["keys", "list", "--json"] | ["keys", "inspect", "-", "--json"] => true,
                ["keys", "add", "-", "--confirm", fp, "--json"] => is_hex16(fp),
                ["keys", "remove", id, "--confirm", fp, "--json"] => is_hex16(id) && is_hex16(fp),
                _ => false,
            };
            assert!(ok, "an argv outside the five: {words:?}");
        }
    }

    // -----------------------------------------------------------------------------------------------------------------------------------
    // The page's cookie, and a perch that is Nix's
    // -----------------------------------------------------------------------------------------------------------------------------------

    /// The page holds a cookie and nothing else: without a terminal's arm it can neither add nor remove, and no child is started to find out.
    #[test]
    fn a_page_with_only_its_cookie_cannot_add_or_remove_and_nothing_runs() {
        let rig = Rig::start();
        let before = rig.snapshot();
        let cookie = page_cookie(&rig.perch);
        let add = as_page(&rig.perch, &cookie, "POST", "/hub/install/keys/add", json!({"pubkey": pubkey(ID, FP, "k"), "confirm": FP_TYPED}));
        assert_eq!(add.status, 403, "{}", add.body);
        assert_eq!(why_of(&add), format!("in a terminal: hoot keys allow-web {FP_SHOWN}, then add within 2 minutes"));
        let remove = as_page(&rig.perch, &cookie, "POST", "/hub/install/keys/remove", json!({"id": HAND.0, "confirm": HAND.1}));
        assert_eq!(remove.status, 403, "{}", remove.body);
        assert_eq!(why_of(&remove), "in a terminal: hoot keys allow-web 9999 AAAA BBBB CCCC, then remove within 2 minutes");
        // the arm it cannot make, and an arm for something else is nothing
        assert_eq!(as_page(&rig.perch, &cookie, "POST", "/hub/install/keys/arm", json!({"fingerprint": FP})).status, 403);
        assert_eq!(rig.arm("1111 2222 3333 4444").status, 200);
        assert_eq!(as_page(&rig.perch, &cookie, "POST", "/hub/install/keys/add", json!({"pubkey": pubkey(ID, FP, "k"), "confirm": FP_TYPED})).status, 403);
        assert!(!rig.fixture().join("log").exists(), "the stand-in was never started: {:?}", rig.calls());
        assert_eq!(rig.snapshot(), before);
    }

    /// A perch that is Nix's (the test arranges it with the restricting knob: no file in `/nix/store` can be made here) says so and runs
    /// nothing, for every route, and the arm answers before it reads its body or arms anything.
    #[test]
    fn under_nix_every_route_says_so_and_no_child_is_ever_started() {
        let rig = Rig::with(&["install_managed=nix"]);
        let nix = "managed by Nix: update through nix";
        let notice = rig.perch.get("/hub/install");
        assert_eq!(notice.status, 200, "{}", notice.body);
        assert_eq!(json_of(&notice), json!({"installed": false, "managed": "nix", "why": nix}));
        let sentence = json!({"why": nix, "kind": "nix"});
        let keys = rig.perch.get("/hub/install/keys");
        assert_eq!((keys.status, json_of(&keys)), (404, sentence.clone()), "{}", keys.body);
        let bodies = [
            json!({"pubkey": pubkey(ID, FP, "k"), "confirm": FP}),
            json!({"id": HAND.0, "confirm": HAND.1}),
            json!({"fingerprint": FP}),
        ];
        for path in ["/hub/install/keys/inspect", "/hub/install/keys/add", "/hub/install/keys/remove", "/hub/install/keys/arm"] {
            for body in &bodies {
                let answer = rig.perch.post_json(path, body.clone());
                assert_eq!((answer.status, json_of(&answer)), (404, sentence.clone()), "{path} {body}: {}", answer.body);
            }
        }
        // the arm never looked at its body: rubbish, and more than its 1 KiB, are the same 404 and not a 400 or a 413
        let arm = "/hub/install/keys/arm";
        for body in [b"not json".to_vec(), b"{}".to_vec(), vec![b' '; 2000]] {
            let answer = rig.perch.apireq("POST", arm).body(body).send(&rig.perch.addr);
            assert_eq!((answer.status, json_of(&answer)), (404, sentence.clone()), "{}", answer.body);
        }
        // the page's cookie and a browser's headers are still refused first, as everywhere
        let cookie = page_cookie(&rig.perch);
        assert_eq!(as_page(&rig.perch, &cookie, "POST", arm, json!({"fingerprint": FP})).status, 403);
        assert!(!rig.fixture().join("log").exists(), "no minerva was started for any of them: {:?}", rig.calls());
    }

    // -----------------------------------------------------------------------------------------------------------------------------------
    // Which minerva: a copy of the perch in a layout of its own
    // -----------------------------------------------------------------------------------------------------------------------------------

    /// A copy of the perch binary at `<root>/<layout>`, started on its own with the environment a test perch gets (a hard link to the
    /// built one when the temp dir is on the same disk, a copy when it is not), killed when dropped. The tests that use it need the perch to
    /// be somewhere in particular, which the harness's own perch (in `target/debug`) never is.
    struct Copy {
        child: std::process::Child,
        addr: String,
        token: String,
        exe: PathBuf,
        _dirs: Vec<tempfile::TempDir>,
    }

    impl Copy {
        /// `beside` is a program put next to the copy under the name `minerva`; `args` are more perch arguments.
        fn start(layout: &str, beside: Option<&Path>, args: &[&OsStr], fixture: &Path) -> Copy {
            use std::sync::{Arc, Mutex};
            let root = tempfile::tempdir().unwrap();
            let exe = root.path().join(layout);
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            let built = env!("CARGO_BIN_EXE_perch");
            if std::fs::hard_link(built, &exe).is_err() {
                std::fs::copy(built, &exe).unwrap();
            }
            if let Some(program) = beside {
                std::fs::copy(program, exe.with_file_name("minerva")).unwrap();
            }
            let (home, runtime, config, data, sessions) = (tempdir(), tempdir(), tempdir(), tempdir(), tempdir());
            own(runtime.path(), 0o700);
            let mut child = std::process::Command::new(&exe)
                .arg("--bind")
                .arg("127.0.0.1:0")
                .arg("--sessions-dir")
                .arg(sessions.path())
                .args(args)
                .env("HOME", home.path())
                .env("XDG_RUNTIME_DIR", runtime.path())
                .env("XDG_CONFIG_HOME", config.path())
                .env("XDG_DATA_HOME", data.path())
                .env("RUST_LOG", "trace")
                .env("MINERVA_FAKE_DIR", fixture)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawning a copy of the perch");
            let stdout = Arc::new(Mutex::new(String::new()));
            pump(child.stdout.take().unwrap(), stdout.clone());
            pump(child.stderr.take().unwrap(), Arc::new(Mutex::new(String::new())));
            let boot = wait_for_lines(&mut child, &stdout, 2, Duration::from_secs(20));
            let addr = boot.lines().next().unwrap().strip_prefix("listening on http://").and_then(|rest| rest.strip_suffix('/')).unwrap().to_string();
            let token_file = boot.lines().nth(1).and_then(|line| line.strip_prefix("token file: ")).unwrap();
            let token = std::fs::read_to_string(token_file).unwrap().trim().to_string();
            Copy { child, addr, token, exe, _dirs: vec![root, home, runtime, config, data, sessions] }
        }

        fn get(&self, path: &str) -> Answer {
            Request::new(&self.addr, "GET", path).bearer(format!("Bearer {}", self.token)).send(&self.addr)
        }

        fn post_json(&self, path: &str, body: Value) -> Answer {
            Request::new(&self.addr, "POST", path).bearer(format!("Bearer {}", self.token)).body(serde_json::to_vec(&body).unwrap()).send(&self.addr)
        }
    }

    impl Drop for Copy {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// A program that records that it ran, in a file beside itself, and fails. It stands for any `minerva` that is not the one meant: the
    /// real one in a developer's `target/debug`, say.
    fn stray(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("stray-minerva");
        std::fs::write(&path, "#!/bin/sh\necho ran >> \"$(dirname \"$0\")/stray-ran\"\nexit 1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Only an installed perch (`<prefix>/opt/minerva/<ver>/bin/perch`) takes the `minerva` beside it. A perch anywhere else (a development
    /// build, where a stray `minerva` really does sit) takes `--minerva` and nothing else; the stray is never started.
    #[test]
    fn a_minerva_beside_a_development_perch_is_never_run_and_an_installed_perchs_is() {
        let fixture = tempdir();
        let home_of_strays = tempdir();
        let stray = stray(home_of_strays.path());
        let fake = Path::new(fake_minerva().expect("built with --features test-bins"));
        let ran = home_of_strays.path().join("stray-ran");
        let log = fixture.path().join("log");

        // a development perch, a `minerva` beside it, no `--minerva`: not installed, and the one beside it is not started
        let dev = Copy::start("target/debug/perch", None, &[], fixture.path());
        std::fs::copy(&stray, dev.exe.with_file_name("minerva")).unwrap();
        // (the stray records beside itself, which for this copy is the copy's own dir)
        let dev_ran = dev.exe.with_file_name("stray-ran");
        let notice = dev.get("/hub/install");
        assert_eq!((notice.status, json_of(&notice)), (200, json!({"installed": false, "why": "no installed Minerva here (development server)"})));
        assert_eq!(dev.get("/hub/install/keys").status, 404);
        assert_eq!(dev.post_json("/hub/install/keys/arm", json!({"fingerprint": FP})).status, 404);
        assert!(!dev_ran.exists() && !ran.exists() && !log.exists(), "nothing was started");

        // the same perch with `--minerva`: that one, and still not the one beside it
        let flagged = Copy::start("target/debug/perch", None, &[OsStr::new("--minerva"), fake.as_os_str()], fixture.path());
        std::fs::copy(&stray, flagged.exe.with_file_name("minerva")).unwrap();
        let notice = flagged.get("/hub/install");
        assert_eq!(notice.status, 200, "{}", notice.body);
        assert_eq!(json_of(&notice)["installed"], true);
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1, "the stand-in answered, once");
        assert!(!flagged.exe.with_file_name("stray-ran").exists(), "and the one beside the perch was not started");

        // an installed perch with a minerva beside it and a different one named by `--minerva`: the one beside it, as the installer's layout says
        let installed = Copy::start("opt/minerva/0.1.0-gabc1234/bin/perch", Some(fake), &[OsStr::new("--minerva"), stray.as_os_str()], fixture.path());
        let notice = installed.get("/hub/install");
        assert_eq!(notice.status, 200, "{}", notice.body);
        assert_eq!(json_of(&notice)["installed"], true);
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2, "the one beside the installed perch answered");
        assert!(!ran.exists(), "the flag's program was not started");
        // and without a flag at all
        let alone = Copy::start("opt/minerva/0.1.0-gabc1234/bin/perch", Some(fake), &[], fixture.path());
        assert_eq!(alone.get("/hub/install").status, 200);
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 3);
    }

    /// `minerva update` replaces the version a running perch started from; its files are gone (`/proc/self/exe` reads `... (deleted)`).
    /// The page is told to start it again, on every route, and nothing is run on its behalf.
    #[test]
    fn an_installed_perch_whose_files_were_removed_says_the_server_was_updated() {
        let fixture = tempdir();
        let fake = Path::new(fake_minerva().expect("built with --features test-bins"));
        let copy = Copy::start("opt/minerva/0.1.0-gabc1234/bin/perch", Some(fake), &[], fixture.path());
        assert_eq!(json_of(&copy.get("/hub/install"))["installed"], true);
        let log = fixture.path().join("log");
        let calls = std::fs::read_to_string(&log).unwrap().lines().count();
        std::fs::remove_file(&copy.exe).unwrap();
        let said = "the server was updated; start it again";
        let notice = copy.get("/hub/install");
        assert_eq!((notice.status, json_of(&notice)), (200, json!({"installed": false, "kind": "updated", "why": said})), "{}", notice.body);
        let sentence = json!({"why": said, "kind": "updated"});
        let keys = copy.get("/hub/install/keys");
        assert_eq!((keys.status, json_of(&keys)), (404, sentence.clone()));
        for path in ["/hub/install/keys/inspect", "/hub/install/keys/add", "/hub/install/keys/remove", "/hub/install/keys/arm"] {
            let answer = copy.post_json(path, json!({"pubkey": "k", "confirm": FP, "id": HAND.0, "fingerprint": FP}));
            assert_eq!((answer.status, json_of(&answer)), (404, sentence.clone()), "{path}: {}", answer.body);
        }
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), calls, "nothing was run after the update");
    }
}
