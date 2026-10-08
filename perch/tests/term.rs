//! `/hub/term`: the operator's terminal routes (slice T1b), over a real `/bin/sh`.
//!
//! Every test that starts a shell runs its body inside [`with_shells`], which stops the perch with SIGTERM after the body, panic or
//! not: the perch's shutdown ends every terminal, so no `sh` or `sleep` that a test started outlives it. The gate and the refusals
//! that need no shell use a perch of their own. The harness's `Request` adds a JSON `Content-Type` to every body, so the tests that
//! send their own type, or a length that is not the body's, build the request themselves ([`request_head`] and [`send`]).

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

/// A perch whose `--root` is its own tree (so `perch.work` is a folder a terminal may start in), whose login shell is `/bin/sh`, and
/// whose HOME is its own. The body runs against it; the perch is then stopped, whatever the body did.
fn with_shells(body: impl FnOnce(&Perch)) {
    with_perch(Perch::builder().hub().env("SHELL", "/bin/sh"), body);
}

/// [`with_shells`] for a perch the test built itself: its own `--home`, or its own `HOME`. Stopped the same way, whatever the body did.
fn with_perch(builder: Builder, body: impl FnOnce(&Perch)) {
    let mut perch = builder.start();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&perch)));
    // SIGTERM, as a supervisor sends it: the shutdown ends every terminal before the perch exits.
    let _ = perch.stop();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// Opens a terminal in the perch's default place (its HOME) and returns its id.
fn open_one(perch: &Perch) -> String {
    let answer = perch.post_json("/hub/term", json!({}));
    assert_eq!(answer.status, 201, "{}: {}", answer.head, answer.body);
    body_id(&answer)
}

/// `GET /hub/term`, as the page reads it.
fn list(perch: &Perch) -> Vec<Value> {
    let answer = perch.get("/hub/term");
    assert_eq!(answer.status, 200, "{}", answer.head);
    assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{}", answer.head);
    serde_json::from_str(&answer.body).unwrap()
}

/// The row of terminal `id` in the list, if it is there.
fn row(perch: &Perch, id: &str) -> Option<Value> {
    list(perch).into_iter().find(|row| row["id"] == id)
}

/// The screen's text, read until `want` is true of it or ten seconds pass: a shell answers a moment after its input.
fn screen_until(perch: &Perch, id: &str, want: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let answer = perch.get(&format!("/hub/term/{id}/text?lines=50"));
        assert_eq!(answer.status, 200, "{}", answer.head);
        let text = serde_json::from_str::<Value>(&answer.body).unwrap()["text"].as_str().unwrap().to_string();
        if want(&text) || Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The pid a shell printed for its background job, after `bg=` (the typed line shows `bg=$!`, which is not a number, and the prompt may
/// sit in front of the answer on the same line).
fn background_pid(text: &str) -> Option<u32> {
    text.lines().find_map(|line| line.split("bg=").nth(1)?.trim().parse().ok())
}

/// Types `echo ab$((a*b))` and reads `ab<a*b>` back: the echoed line holds the expression, not the product, so the product on the
/// screen is the shell's own answer.
fn ask_the_shell(perch: &Perch, id: &str, a: u32, b: u32) -> String {
    assert_eq!(type_into(perch, id, format!("echo ab$(({a}*{b}))\r").as_bytes()).status, 204);
    let product = format!("ab{}", a * b);
    let text = screen_until(perch, id, |text| text.contains(&product));
    assert!(text.contains(&product), "the shell's answer {product} is on the screen: {text:?}");
    text
}

/// A process that is gone, or a zombie: what a close leaves once the kernel has taken the process's memory. Whether `/proc` still
/// lists a zombie depends on who reaps it, so only its state counts.
fn gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => {
            let state = stat.rsplit(')').next().and_then(|rest| rest.trim_start().chars().next());
            matches!(state, Some('Z' | 'X'))
        }
    }
}

fn wait_for_gone(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if gone(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    gone(pid)
}

/// The head of a request the harness's `Request` cannot make: the request line, the perch's `Host` and bearer, and `headers` as given.
fn request_head(perch: &Perch, method: &str, path: &str, headers: &[(&str, &str)]) -> Vec<u8> {
    let mut text = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAuthorization: {}\r\n", perch.addr, perch.auth());
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str("\r\n");
    text.into_bytes()
}

/// `body` in chunked transfer coding: one chunk, and the last.
fn chunked(body: &[u8]) -> Vec<u8> {
    let mut framed = format!("{:x}\r\n", body.len()).into_bytes();
    framed.extend_from_slice(body);
    framed.extend_from_slice(b"\r\n0\r\n\r\n");
    framed
}

/// Sends `head`, then `body` on another thread, and reads the answer. A refusal stops reading the body, so a write can fail: that is
/// the refusal, not a failure (as `send_oversize` in the harness treats it).
fn send(perch: &Perch, head: Vec<u8>, body: Vec<u8>) -> Answer {
    let mut sock = TcpStream::connect(&perch.addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    sock.write_all(&head).unwrap();
    let mut writer = sock.try_clone().unwrap();
    writer.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let sender = std::thread::spawn(move || writer.write_all(&body).is_ok());
    let mut raw = Vec::new();
    let _ = sock.read_to_end(&mut raw);
    let _ = sender.join();
    let answer = Answer::parse(&raw);
    assert!(!answer.has_cors(), "a CORS header: {}", answer.head);
    answer
}

/// `POST path` with `content_type` (none when `None`) and `body`, its length declared.
fn post_bytes(perch: &Perch, path: &str, content_type: Option<&str>, body: &[u8]) -> Answer {
    let length = body.len().to_string();
    let mut headers = vec![("Content-Length", length.as_str())];
    if let Some(content_type) = content_type {
        headers.push(("Content-Type", content_type));
    }
    send(perch, request_head(perch, "POST", path, &headers), body.to_vec())
}

/// `POST /hub/term/<id>/input` with `bytes` as an octet stream: what the page types.
fn type_into(perch: &Perch, id: &str, bytes: &[u8]) -> Answer {
    post_bytes(perch, &format!("/hub/term/{id}/input"), Some("application/octet-stream"), bytes)
}

/// A login cookie, traded the way a page does it: the bearer mints a code, and the code (with no credential) is redeemed for a cookie.
fn login_cookie(perch: &Perch) -> String {
    let minted = perch.post("/hub/code");
    assert_eq!(minted.status, 200, "{}", minted.head);
    let code = serde_json::from_str::<Value>(&minted.body).unwrap()["code"].as_str().unwrap().to_string();
    let body = serde_json::to_vec(&json!({ "code": code })).unwrap();
    let redeemed = Request::new(&perch.addr, "POST", "/hub/redeem").body(body).send(&perch.addr);
    assert_eq!(redeemed.status, 204, "{}", redeemed.head);
    // `minerva_<port>=<value>`, the part before the attributes.
    let set = redeemed.header("set-cookie").expect("a session cookie");
    set.split(';').next().unwrap().to_string()
}

#[test]
fn a_shell_takes_input_and_its_screen_reads_back() {
    with_shells(|perch| {
        let id = open_one(perch);
        let listed = row(perch, &id).expect("the new terminal is listed");
        assert_eq!(listed["by"], "operator");
        assert_eq!(listed["alive"], true);
        assert_eq!((listed["cols"].as_u64(), listed["rows"].as_u64()), (Some(120), Some(32)), "the default size");
        assert!(listed.get("chat").is_none(), "no chat was named, so none is recorded: {listed}");

        let answer = type_into(perch, &id, b"echo hi\r");
        assert_eq!(answer.status, 204, "{}", answer.body);
        let text = screen_until(perch, &id, |text| text.contains("hi"));
        assert!(text.contains("hi"), "the shell's answer is on the screen: {text:?}");
        // `hi` is also in the typed line, so the product of an expression is the proof that the shell ran it.
        ask_the_shell(perch, &id, 6, 7);

        let read = serde_json::from_str::<Value>(&perch.get(&format!("/hub/term/{id}/text")).body).unwrap();
        let keys: Vec<&str> = read.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 3, "the text answer is text, cursor and alt_screen: {read}");
        assert_eq!(read["alt_screen"], false);
        assert!(read["cursor"]["row"].is_u64() && read["cursor"]["col"].is_u64(), "a live screen has a cursor: {read}");
    });
}

#[test]
fn resize_answers_the_size_now_and_clamps_it() {
    with_shells(|perch| {
        let id = open_one(perch);
        let path = format!("/hub/term/{id}/resize");
        let answer = perch.post_json(&path, json!({ "cols": 100, "rows": 30 }));
        assert_eq!(answer.status, 200, "{}", answer.body);
        assert_eq!(serde_json::from_str::<Value>(&answer.body).unwrap(), json!({ "cols": 100, "rows": 30 }));
        assert_eq!((row(perch, &id).unwrap()["cols"].as_u64(), row(perch, &id).unwrap()["rows"].as_u64()), (Some(100), Some(30)));

        // T1a's clamp: 20-400 columns, 5-200 rows.
        let clamped = perch.post_json(&path, json!({ "cols": 1, "rows": 1000 }));
        assert_eq!(serde_json::from_str::<Value>(&clamped.body).unwrap(), json!({ "cols": 20, "rows": 200 }));
        let listed = row(perch, &id).unwrap();
        assert_eq!((listed["cols"].as_u64(), listed["rows"].as_u64()), (Some(20), Some(200)));
    });
}

#[test]
fn a_size_body_that_is_not_an_object_of_unsigned_integers_is_400() {
    with_shells(|perch| {
        let id = open_one(perch);
        let path = format!("/hub/term/{id}/resize");
        let bodies = [
            json!([]),
            json!({ "cols": "80", "rows": 24 }),
            json!({ "cols": -1, "rows": 24 }),
            json!({ "cols": 80.5, "rows": 24 }),
            json!({ "cols": 80 }),
            json!({ "cols": 80, "rows": 24, "extra": 1 }),
        ];
        for body in bodies {
            let answer = perch.post_json(&path, body.clone());
            assert_eq!(answer.status, 400, "resize {body}: {}", answer.body);
        }
        let empty = perch.apireq("POST", &path).send(&perch.addr);
        assert_eq!(empty.status, 400, "an empty resize body is not a size: {}", empty.body);
    });
}

#[test]
fn hold_answers_the_new_state_and_shows_in_the_list() {
    with_shells(|perch| {
        let id = open_one(perch);
        let path = format!("/hub/term/{id}/hold");
        assert_eq!(row(perch, &id).unwrap()["hold"], false);

        let on = perch.post_json(&path, json!({ "hold": true }));
        assert_eq!(on.status, 200, "{}", on.body);
        assert_eq!(serde_json::from_str::<Value>(&on.body).unwrap(), json!({ "hold": true }));
        assert_eq!(row(perch, &id).unwrap()["hold"], true);

        let off = perch.post_json(&path, json!({ "hold": false }));
        assert_eq!(serde_json::from_str::<Value>(&off.body).unwrap(), json!({ "hold": false }));
        assert_eq!(row(perch, &id).unwrap()["hold"], false);

        let bad = perch.post_json(&path, json!({ "hold": "yes" }));
        assert_eq!(bad.status, 400, "a hold that is not a boolean: {}", bad.body);
        let missing = perch.post_json("/hub/term/t999/hold", json!({ "hold": true }));
        assert_eq!(missing.status, 404, "{}", missing.head);
        assert_eq!(missing.body, "");
    });
}

#[test]
fn close_hangs_up_the_session_and_its_background_job() {
    with_shells(|perch| {
        let id = open_one(perch);
        assert_eq!(type_into(perch, &id, b"sleep 1000 & echo bg=$!\r").status, 204);
        let text = screen_until(perch, &id, |text| background_pid(text).is_some());
        let pid = background_pid(&text).unwrap_or_else(|| panic!("the shell named its background job: {text:?}"));
        assert!(!gone(pid), "the background sleep {pid} runs before the close");

        let answer = perch.post(&format!("/hub/term/{id}/close"));
        assert_eq!(answer.status, 202, "{}", answer.head);
        assert_eq!(answer.body, "", "a close answers with no body");
        assert!(wait_for_gone(pid, Duration::from_secs(10)), "the background job {pid} outlived its terminal's close");
        assert!(row(perch, &id).is_none(), "a closed terminal is no longer listed");
        assert_eq!(perch.get(&format!("/hub/term/{id}/text")).status, 404, "a closed terminal has no screen to read");
        assert_eq!(perch.post(&format!("/hub/term/{id}/close")).status, 404, "a second close finds nothing");
    });
}

#[test]
fn a_cwd_outside_every_root_is_422_and_one_inside_is_taken() {
    with_shells(|perch| {
        for cwd in ["/", "/does/not/exist"] {
            let answer = perch.post_json("/hub/term", json!({ "cwd": cwd }));
            assert_eq!(answer.status, 422, "cwd {cwd}: {}", answer.body);
            let why = serde_json::from_str::<Value>(&answer.body).unwrap();
            assert!(why["error"].as_str().is_some_and(|error| !error.is_empty()), "a refusal says why: {why}");
        }

        let inside = perch.work.canonicalize().unwrap().display().to_string();
        let answer = perch.post_json("/hub/term", json!({ "cwd": perch.work.display().to_string() }));
        assert_eq!(answer.status, 201, "{}", answer.body);
        let id = body_id(&answer);
        assert_eq!(row(perch, &id).unwrap()["cwd"], inside.as_str(), "the shell starts in the folder it was given");

        let unknown = perch.post_json("/hub/term", json!({ "chat": "c0ffee00" }));
        assert_eq!(unknown.status, 404, "a chat nobody knows: {}", unknown.body);
        let shape = perch.post_json("/hub/term", json!({ "cwd": 7 }));
        assert_eq!(shape.status, 400, "a cwd that is not a string: {}", shape.body);
        let extra = perch.post_json("/hub/term", json!({ "shell": "/bin/bash" }));
        assert_eq!(extra.status, 400, "a field the route does not take: {}", extra.body);
    });
}

#[test]
fn input_is_octet_stream_only_and_capped_before_it_is_read() {
    with_shells(|perch| {
        let id = open_one(perch);
        let path = format!("/hub/term/{id}/input");
        assert_eq!(post_bytes(perch, &path, Some("text/plain"), b"echo no\r").status, 415, "text/plain");
        assert_eq!(post_bytes(perch, &path, None, b"echo no\r").status, 415, "no Content-Type");

        let twice = request_head(
            perch,
            "POST",
            &path,
            &[("Content-Type", "application/octet-stream"), ("Content-Type", "application/json"), ("Content-Length", "2")],
        );
        assert_eq!(send(perch, twice, b"ok".to_vec()).status, 415, "two Content-Type headers are ambiguous");

        // A parameter after the media type is allowed.
        let typed = post_bytes(perch, &path, Some("application/octet-stream; charset=binary"), b"true\r");
        assert_eq!(typed.status, 204, "{}", typed.body);

        // Over 64 KiB by a declared length: refused, and the body is not read.
        let big = vec![b'x'; 65 * 1024];
        assert_eq!(type_into(perch, &id, &big).status, 413, "65 KiB, declared");

        // Over 64 KiB with no length but a chunked body: refused at the first frame past the cap.
        let framed = chunked(&big);
        let head = request_head(perch, "POST", &path, &[("Content-Type", "application/octet-stream"), ("Transfer-Encoding", "chunked")]);
        assert_eq!(send(perch, head, framed).status, 413, "65 KiB, chunked");

        // A declared length over the cap is refused with no body sent at all.
        let head = request_head(perch, "POST", &path, &[("Content-Type", "application/octet-stream"), ("Content-Length", "70000")]);
        assert_eq!(send(perch, head, Vec::new()).status, 413, "a declared 70000 with nothing after it");

        // The shell still takes input after the refusals (none of the refused bodies reached it: a `true` that was taken is the only
        // input before this line, and the product below is the shell's answer to this line alone).
        ask_the_shell(perch, &id, 2, 21);

        let missing = type_into(perch, "t999", b"echo nobody\r");
        assert_eq!(missing.status, 404, "a terminal that is not there: {}", missing.head);
    });
}

#[test]
fn the_seventeenth_terminal_is_a_409() {
    with_shells(|perch| {
        for _ in 0..16 {
            open_one(perch);
        }
        let refused = perch.post_json("/hub/term", json!({}));
        assert_eq!(refused.status, 409, "{}", refused.body);
        assert!(serde_json::from_str::<Value>(&refused.body).unwrap()["error"].is_string(), "the 409 says why: {}", refused.body);
        assert_eq!(list(perch).len(), 16, "the perch still holds exactly sixteen");
    });
}

#[test]
fn an_ended_shell_refuses_input_and_keeps_its_screen() {
    with_shells(|perch| {
        let id = open_one(perch);
        ask_the_shell(perch, &id, 3, 7);
        assert_eq!(type_into(perch, &id, b"exit\r").status, 204);
        let deadline = Instant::now() + Duration::from_secs(10);
        while row(perch, &id).is_some_and(|row| row["alive"] == true) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(row(perch, &id).unwrap()["alive"], false, "the shell ended");

        let refused = type_into(perch, &id, b"echo late\r");
        assert_eq!(refused.status, 409, "input to an ended shell: {}", refused.body);
        let read = perch.get(&format!("/hub/term/{id}/text?lines=50"));
        assert_eq!(read.status, 200, "an ended terminal keeps its screen: {}", read.head);
        let text = serde_json::from_str::<Value>(&read.body).unwrap()["text"].as_str().unwrap().to_string();
        assert!(text.contains("ab21"), "the screen the shell left is readable: {text:?}");
    });
}

#[test]
fn text_takes_lines_as_digits_given_once() {
    with_shells(|perch| {
        let id = open_one(perch);
        let path = format!("/hub/term/{id}/text");
        for query in ["lines=abc", "lines=", "lines=-1", "lines=1.5", "lines=10&lines=20", "lines=%31%30"] {
            let answer = perch.get(&format!("{path}?{query}"));
            assert_eq!(answer.status, 400, "?{query}: {}", answer.body);
            assert!(answer.body.contains("\"error\""), "?{query} answers a reason: {}", answer.body);
        }
        // A number too big for a count is clamped, not refused (the counts themselves are checked in text_is_clamped_...).
        let huge = perch.get(&format!("{path}?lines=99999999999999999999999"));
        assert_eq!(huge.status, 200, "a huge number is clamped, not refused: {}", huge.body);
        assert_eq!(perch.get(&path).status, 200, "no query is the default read");
    });
}

#[test]
fn ids_that_are_not_terminals_are_the_empty_404() {
    with_shells(|perch| {
        for id in ["t0x", "T1", "t1234567", "events", "t999"] {
            for (method, path) in [("GET", format!("/hub/term/{id}/text")), ("POST", format!("/hub/term/{id}/close"))] {
                let answer = perch.apireq(method, &path).send(&perch.addr);
                assert_eq!(answer.status, 404, "{method} {path}: {}", answer.head);
                assert_eq!(answer.body, "", "{method} {path}: the 404 has no body");
            }
        }
        // `a` is never an id: a bare `a` is nobody's.
        for path in ["/hub/term/a", "/hub/term/a/", "/hub/term/t1", "/hub/term/t1/nothing"] {
            let answer = perch.get(path);
            assert_eq!(answer.status, 404, "GET {path}: {}", answer.head);
            assert_eq!(answer.body, "", "GET {path}: the 404 has no body");
        }
        // A route that exists, with the wrong method, is a 405 that names the method it takes.
        let wrong = perch.get("/hub/term/t1/close");
        assert_eq!(wrong.status, 405, "{}", wrong.head);
        assert_eq!(wrong.header("allow").as_deref(), Some("POST"));
    });
}

#[test]
fn the_answers_carry_no_environment_and_no_token() {
    with_shells(|perch| {
        let id = open_one(perch);
        assert_eq!(type_into(perch, &id, b"echo ready\r").status, 204);
        let _ = screen_until(perch, &id, |text| text.lines().any(|line| line.trim() == "ready"));
        let token = perch.token.trim();
        for path in ["/hub/term".to_string(), format!("/hub/term/{id}/text?lines=50")] {
            let answer = perch.get(&path);
            assert_eq!(answer.status, 200, "{path}: {}", answer.head);
            assert!(!answer.body.contains(token), "{path} carries no token");
            for leak in ["MINERVA_", "XDG_", "HOME=", "PATH=", "environ", "argv", "--bind", "--root", "--ui-dir"] {
                assert!(!answer.body.contains(leak), "{path} carries {leak}: {}", answer.body);
            }
        }
    });
}

#[test]
fn the_gate_is_the_existing_one() {
    let perch = Perch::start();
    // No credential: the empty 401 before any route is asked, for the list, the create and a terminal's verb.
    for (method, path) in [("GET", "/hub/term"), ("POST", "/hub/term"), ("POST", "/hub/term/t1/input"), ("GET", "/hub/term/t1/text")] {
        let answer = Request::new(&perch.addr, method, path).send(&perch.addr);
        assert_eq!(answer.status, 401, "{method} {path} with no credential: {}", answer.head);
        assert_eq!(answer.body, "", "{method} {path}: the 401 has no body");
    }
    // The master bearer lists nothing yet, as JSON.
    assert_eq!(perch.get("/hub/term").body, "[]");

    // A cross-site page has no bearer; a bearer it somehow sent is refused by the fetch-metadata gate, and so is a create.
    let cross = perch.apireq("POST", "/hub/term").header("Sec-Fetch-Site", "cross-site").send(&perch.addr);
    assert_eq!(cross.status, 403, "a cross-site create with the bearer: {}", cross.head);
    assert_eq!(cross.body, "");
    assert_eq!(perch.get("/hub/term").body, "[]", "the refused create made nothing");

    // The login cookie: same-origin is the page's own fetch, and it works.
    let cookie = login_cookie(&perch);
    let same = Request::new(&perch.addr, "GET", "/hub/term")
        .header("Cookie", &cookie)
        .header("Sec-Fetch-Site", "same-origin")
        .send(&perch.addr);
    assert_eq!(same.status, 200, "the page's same-origin cookie: {}", same.head);
    // A cookie that comes with a cross-site fetch does not authenticate: gate 1 answers it 401, before the fetch-metadata gate.
    let cross_cookie = Request::new(&perch.addr, "GET", "/hub/term")
        .header("Cookie", &cookie)
        .header("Sec-Fetch-Site", "cross-site")
        .send(&perch.addr);
    assert_eq!(cross_cookie.status, 401, "a cross-site cookie: {}", cross_cookie.head);

    // DELETE is 405, for the list and for a terminal's verb, with the bearer.
    assert_eq!(perch.apireq("DELETE", "/hub/term").send(&perch.addr).status, 405);
    assert_eq!(perch.apireq("DELETE", "/hub/term/t1/input").send(&perch.addr).status, 405);
}

#[test]
fn a_create_takes_its_size_and_its_title() {
    with_shells(|perch| {
        let answer = perch.post_json("/hub/term", json!({ "cols": 100, "rows": 30, "title": "build" }));
        assert_eq!(answer.status, 201, "{}", answer.body);
        let listed = row(perch, &body_id(&answer)).expect("the new terminal is listed");
        assert_eq!((listed["cols"].as_u64(), listed["rows"].as_u64()), (Some(100), Some(30)), "the size asked for: {listed}");
        assert_eq!(listed["title"], "build", "the title asked for: {listed}");
    });
}

#[test]
fn a_chat_starts_in_the_folder_it_remembers_and_one_with_no_folder_in_home() {
    // `--home` keeps the chats' folders in `<home>/chat-cwds.json`, and a chat's log in `<home>/sessions`: a chat with a log and no folder
    // is one the list knows, and starts in `$HOME`, which this test names.
    let home = tempfile::tempdir().unwrap();
    let home_dir = home.path().to_path_buf();
    let folder = home_dir.join("chat-folder");
    std::fs::create_dir_all(&folder).unwrap();
    let folder = folder.canonicalize().unwrap();
    std::fs::write(home_dir.join("chat-cwds.json"), json!({ "c0ffee01": folder.display().to_string() }).to_string()).unwrap();
    let shell_home = home_dir.join("shell-home");
    std::fs::create_dir_all(&shell_home).unwrap();
    std::fs::create_dir_all(home_dir.join("sessions")).unwrap();
    std::fs::write(home_dir.join("sessions").join("c0ffee02.eid"), "").unwrap();

    let builder = Perch::builder().hub().env("SHELL", "/bin/sh").env("HOME", shell_home.display().to_string()).arg("--home").arg(&home_dir);
    with_perch(builder, |perch| {
        let remembered = perch.post_json("/hub/term", json!({ "chat": "c0ffee01" }));
        assert_eq!(remembered.status, 201, "{}", remembered.body);
        let listed = row(perch, &body_id(&remembered)).unwrap();
        assert_eq!(listed["cwd"], folder.display().to_string().as_str(), "the chat's remembered folder: {listed}");
        assert_eq!(listed["chat"], "c0ffee01", "the chat is recorded: {listed}");

        let listed_only = perch.post_json("/hub/term", json!({ "chat": "c0ffee02" }));
        assert_eq!(listed_only.status, 201, "{}", listed_only.body);
        let listed = row(perch, &body_id(&listed_only)).unwrap();
        assert_eq!(listed["cwd"], shell_home.display().to_string().as_str(), "a listed chat with no folder starts in HOME: {listed}");
        assert_eq!(listed["chat"], "c0ffee02");

        let unknown = perch.post_json("/hub/term", json!({ "chat": "c0ffee03" }));
        assert_eq!(unknown.status, 404, "a chat in neither place: {}", unknown.body);
        assert!(unknown.body.contains("\"error\""), "the 404 for a chat says why: {}", unknown.body);
    });
}

#[test]
fn a_shell_that_reads_nothing_refuses_input_with_a_503_once_its_queue_is_full() {
    with_shells(|perch| {
        let id = open_one(perch);
        // `sleep` reads nothing from the terminal: what is written to it waits in the kernel, then in the perch's 64 KiB queue.
        assert_eq!(type_into(perch, &id, b"sleep 1000\r").status, 204);
        let big = vec![b'x'; 64 * 1024];
        let refusal = (0..256).map(|_| type_into(perch, &id, &big)).find(|answer| answer.status != 204);
        let refusal = refusal.expect("a 503 within 256 sends of 64 KiB");
        assert_eq!(refusal.status, 503, "{}", refusal.head);
        assert!(refusal.body.contains("\"error\""), "the 503 says why: {}", refusal.body);
    });
}

#[test]
fn text_is_clamped_to_400_lines_and_7000_characters_and_keeps_the_end() {
    with_shells(|perch| {
        let id = open_one(perch);
        assert_eq!(type_into(perch, &id, b"seq 1 600; echo END\r").status, 204);
        // The prompt is the row after END, so the screen is read once that row is there.
        let _ = screen_until(perch, &id, |text| text.lines().any(|line| line == "END") && text.lines().last() != Some("END"));
        let path = format!("/hub/term/{id}/text");
        let read = |query: &str| -> String {
            let answer = perch.get(&format!("{path}?{query}"));
            assert_eq!(answer.status, 200, "?{query}: {}", answer.body);
            serde_json::from_str::<Value>(&answer.body).unwrap()["text"].as_str().unwrap().to_string()
        };
        // The scrollback holds the 600 lines, so a count over 400 is 400 of them, not the screen's 32 rows.
        let most = read("lines=99999");
        assert_eq!(most.lines().count(), 400, "a count over 400 reads 400 lines");
        assert_eq!(most.lines().nth_back(1), Some("END"), "the end is kept, the prompt is the last row");
        // Ten rows: 593 to 600, END, and the prompt.
        let ten = read("lines=10");
        let rows: Vec<&str> = ten.lines().collect();
        assert_eq!(rows.len(), 10, "lines=10 is ten rows: {ten:?}");
        assert_eq!((rows[0], rows[7], rows[8]), ("593", "600", "END"), "the ten rows before the prompt: {ten:?}");

        // 9000 characters with no line break, then a line the shell answers: over 400 rows of them would be more than 7000 characters, so
        // the characters are what cut it, and the end is kept.
        assert_eq!(type_into(perch, &id, b"head -c 9000 /dev/zero | tr '\\0' x; echo; echo ab$((6*7))\r").status, 204);
        let _ = screen_until(perch, &id, |text| text.lines().any(|line| line == "ab42"));
        let wide = read("lines=400");
        assert_eq!(wide.chars().count(), 7000, "more than 7000 characters are cut to exactly 7000");
        assert_eq!(wide.lines().nth_back(1), Some("ab42"), "the end of the screen is kept");
    });
}

#[test]
fn an_ended_terminal_read_back_from_terms_json_refuses_input_resize_and_hold() {
    let home = tempfile::tempdir().unwrap();
    let home_dir = home.path().to_path_buf();
    let mut id = String::new();
    // The first perch is stopped with its shell alive: the shutdown ends the shell and keeps its screen in `<home>/terms.json`.
    with_perch(Perch::builder().hub().env("SHELL", "/bin/sh").arg("--home").arg(&home_dir), |perch| {
        id = open_one(perch);
        ask_the_shell(perch, &id, 5, 8);
    });
    // The second perch reads the file back: the terminal is listed as ended, and nothing can be typed, resized or held in it.
    with_perch(Perch::builder().hub().env("SHELL", "/bin/sh").arg("--home").arg(&home_dir), |perch| {
        let listed = row(perch, &id).expect("the terminal is listed from terms.json");
        assert_eq!(listed["alive"], false, "its shell is not alive: {listed}");
        assert_eq!(listed["ended"], true, "ended when the perch stopped: {listed}");

        let typed = type_into(perch, &id, b"echo late\r");
        assert_eq!(typed.status, 409, "input to it: {}", typed.body);
        let resized = perch.post_json(&format!("/hub/term/{id}/resize"), json!({ "cols": 80, "rows": 24 }));
        assert_eq!(resized.status, 409, "resize: {}", resized.body);
        let held = perch.post_json(&format!("/hub/term/{id}/hold"), json!({ "hold": true }));
        assert_eq!(held.status, 409, "hold: {}", held.body);

        let read = perch.get(&format!("/hub/term/{id}/text?lines=50"));
        assert_eq!(read.status, 200, "its screen is still read: {}", read.head);
        let answer = serde_json::from_str::<Value>(&read.body).unwrap();
        assert!(answer["cursor"].is_null(), "an ended terminal has no cursor: {answer}");
        assert!(answer["text"].as_str().unwrap().contains("ab40"), "the screen it left: {answer}");
        assert_eq!(perch.post(&format!("/hub/term/{id}/close")).status, 202, "it can still be closed");
        assert!(row(perch, &id).is_none(), "and then it is not listed");
    });
}
