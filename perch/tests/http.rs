//! The perch over a real socket.
//!
//! The binary is spawned as a child with `--bind 127.0.0.1:0`, so what is tested is
//! the argv, the two boot lines and the `0600` token file a supervisor actually gets;
//! the token is then read out of the file the child printed, never out of its output.
//! The requests are hand-written HTTP/1.1, because `Request<Incoming>` cannot be built
//! outside a connection — which is also why every gate has a case here (hub.md §10.2).
//!
//! Every request carries the token unless the test is about the token, and every
//! response is checked for a `Access-Control-*` header before the test sees it.
//!
//! The H1b tests drive a real child process, so they need the stand-in door
//! (`src/bin/fake_eidolon.rs`) and are `#[cfg(feature = "test-bins")]`: `cargo test` alone
//! still builds and passes everything that needs no door, and the helpers only those tests
//! call are allowed to be unused there.
#![cfg_attr(not(feature = "test-bins"), allow(dead_code))]

// The harness this file was written around (the perch child, the hand-written requests, the stand-in door's log, the pane reader) is
// `tests/common/mod.rs`, shared with every other test file; this file keeps the cases.
mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
#[cfg_attr(not(feature = "test-bins"), allow(unused_imports))]
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::*;

// ---------------------------------------------------------------------------
// The process: argv, the two boot lines, the token file
// ---------------------------------------------------------------------------

#[test]
fn stdout_is_the_two_boot_lines_and_the_token_is_not_in_them() {
    let perch = Perch::start();
    let lines: Vec<&str> = perch.boot.lines().collect();
    assert_eq!(lines.len(), 2, "stdout is the two boot lines, nothing else: {:?}", perch.boot);
    assert_eq!(lines[0], format!("listening on http://{}/", perch.addr));
    assert_eq!(lines[1], format!("token file: {}", perch.token_file.display()));
    assert!(!perch.boot.contains(perch.token.trim()), "the token is never printed: {:?}", perch.boot);
}

#[test]
fn the_token_file_is_owner_only_and_named_for_the_bound_port() {
    use std::os::unix::fs::PermissionsExt as _;
    let perch = Perch::start();
    let mode = std::fs::metadata(&perch.token_file).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "the token file is owner-only, got {mode:o}");
    assert_eq!(mode & 0o777, 0o600, "…and only the owner reads it, got {mode:o}");
    assert_eq!(perch.token_file, perch.runtime.path().join("minerva").join(format!("perch-{}.token", perch.port)));
    // What `random_token` mints: 43 base64url characters.
    let token = perch.token.trim();
    assert_eq!(token.len(), 43, "{}", perch.token);
    assert!(token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{token}");
}

#[test]
fn sigterm_removes_the_token_file_and_stops_the_listener() {
    let mut perch = Perch::start();
    assert_eq!(Request::new(&perch.addr, "GET", "/").send(&perch.addr).status, 200);
    let status = perch.stop();
    assert!(status.success(), "a signalled stop is a clean exit: {status}");
    assert!(!perch.token_file.exists(), "the token file outlived the listener that honoured it");
    assert!(TcpStream::connect(&perch.addr).is_err(), "nothing is listening any more");
}

#[test]
fn a_bind_that_is_not_loopback_is_refused_before_anything_listens() {
    // Gate 0: the refusal is the process's, not a request's.
    let runtime = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_perch"))
        .arg("--bind")
        .arg("0.0.0.0:0")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "a perch tried to bind 0.0.0.0");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("loopback only"), "the refusal says why: {err}");
    assert!(out.stdout.is_empty(), "nothing was printed as a boot line: {:?}", out.stdout);
    assert!(std::fs::read_dir(runtime.path()).unwrap().next().is_none(), "no token file was minted for a refused bind");
}

#[test]
fn a_missing_runtime_dir_refuses_the_start() {
    // The door falls back to the shared temp dir (`swarm::Presence::root`); the perch
    // requires the variable, because a shared directory is one another user can
    // pre-create for the token.
    let ui = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_perch"))
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--ui-dir")
        .arg(ui.path())
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "a perch without a runtime dir came up");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("XDG_RUNTIME_DIR"), "the refusal names the variable: {err}");
    assert!(out.stdout.is_empty(), "no boot line from a refused start: {:?}", out.stdout);
    assert!(std::fs::read_dir(ui.path()).unwrap().next().is_none(), "nothing was written into --ui-dir");
}

#[test]
fn without_a_ui_dir_every_static_path_is_404() {
    let perch = Perch::start_without_ui_dir();
    for path in ["/", "/index.html", "/assets/app.js"] {
        let answer = Request::new(&perch.addr, "GET", path).send(&perch.addr);
        assert_eq!(answer.status, 404, "{path} without --ui-dir: {}", answer.head);
        answer.assert_static_headers("a 404 without a bundle");
    }
}

#[test]
fn a_runtime_dir_that_is_not_owner_only_refuses_the_start() {
    let runtime = tempfile::tempdir().unwrap();
    own(runtime.path(), 0o755);
    let out = Command::new(env!("CARGO_BIN_EXE_perch"))
        .arg("--bind")
        .arg("127.0.0.1:0")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "a world-readable runtime dir was accepted");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("755"), "the refusal names the mode it found: {err}");
    assert!(out.stdout.is_empty(), "no boot line from a refused start: {:?}", out.stdout);
}

// ---------------------------------------------------------------------------
// Statics: no token, `Host` checked, headers on every response
// ---------------------------------------------------------------------------

#[test]
fn a_static_file_is_served_without_a_token_with_the_bundle_headers() {
    let perch = Perch::start();
    let answer = Request::new(&perch.addr, "GET", "/").send(&perch.addr);
    assert_eq!(answer.status, 200, "{}", answer.head);
    assert_eq!(answer.body, std::fs::read_to_string(perch.ui.join("index.html")).unwrap(), "`/` is index.html");
    assert_eq!(answer.header("content-type").as_deref(), Some("text/html; charset=utf-8"));
    answer.assert_static_headers("the index");
    assert!(!answer.has_cors());

    let asset = Request::new(&perch.addr, "GET", "/assets/app.js").send(&perch.addr);
    assert_eq!(asset.status, 200);
    assert_eq!(asset.header("content-type").as_deref(), Some("text/javascript; charset=utf-8"));
    asset.assert_static_headers("an asset");
}

#[test]
fn the_static_route_answers_head_405_and_404_as_the_route_table_says() {
    let perch = Perch::start();
    let head = Request::new(&perch.addr, "HEAD", "/").send(&perch.addr);
    assert_eq!(head.status, 200);
    assert_eq!(head.header("content-length").as_deref(), Some(INDEX.len().to_string().as_str()));
    assert_eq!(head.body, "", "HEAD carries no body");
    head.assert_static_headers("a HEAD");

    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        let refusal = Request::new(&perch.addr, method, "/").send(&perch.addr);
        assert_eq!(refusal.status, 405, "{method} / is not a static method");
        assert_eq!(refusal.header("allow").as_deref(), Some("GET, HEAD"));
        refusal.assert_static_headers(&format!("a {method} refusal"));
    }

    // A 404 carries the static headers too: the browser that asked is still running
    // the bundle, and a refusal without a policy is a page without one.
    for path in ["/missing.js", "/assets", "/assets/", "/again.html%"] {
        let missing = Request::new(&perch.addr, "GET", path).send(&perch.addr);
        assert_eq!(missing.status, 404, "{path}");
        missing.assert_static_headers(&format!("the 404 for {path}"));
    }

    // A link that stays inside the UI dir is a file; the door's rules, over the wire.
    assert_eq!(Request::new(&perch.addr, "GET", "/again.html").send(&perch.addr).status, 200);
}

#[test]
fn every_escape_from_the_ui_dir_is_refused_over_the_wire() {
    let perch = Perch::start();
    // The secret the escapes below must not reach, named on disk so a 404 for it means
    // something: it is one directory above `--ui-dir`.
    assert_eq!(std::fs::read_to_string(perch.root.path().join("secret.txt")).unwrap().trim(), "s3cret");
    for path in [
        // the ported files.rs cases, asked of a real server
        "/../secret.txt",
        "/%2e%2e/secret.txt",
        "/..%2fsecret.txt",
        "/%2e%2e%2fsecret.txt",
        "/assets/../../secret.txt",
        "/assets/..%2f..%2fsecret.txt",
        "/..",
        "/.",
        "/assets/..",
        "//secret.txt",
        "/assets//secret.txt",
        "/..\\secret.txt",
        "/%5c..%5csecret.txt",
        "/assets\\app.js",
        "/%00",
        "/assets/%00.js",
        "/secret.txt%",
        "/a%zz",
        // absolute-looking paths resolve *inside* the UI dir, and these are not there
        "/etc/passwd",
        "/%2e%2e%2f%2e%2e%2fetc%2fpasswd",
        // a symlink out, and the file beside the UI dir by its plain name
        "/escape.txt",
        "/secret.txt",
    ] {
        let answer = Request::new(&perch.addr, "GET", path).send(&perch.addr);
        assert_eq!(answer.status, 404, "{path}: {}", answer.head);
        assert!(!answer.body.contains("s3cret"), "{path} served a file outside the UI dir");
    }
    // …and the regular file under the root is still served, so the refusals above are
    // the containment rule and not a server that serves nothing.
    assert_eq!(Request::new(&perch.addr, "GET", "/index.html").send(&perch.addr).status, 200);
}

// ---------------------------------------------------------------------------
// Gate 1: the token
// ---------------------------------------------------------------------------

#[test]
fn a_gated_route_without_a_token_is_401_with_an_empty_body() {
    let perch = Perch::start();
    // The last two are the same namespaces spelled with a percent escape. The static route
    // decodes once before it names a file, so `/%68ub/x` is `hub/x` to it; the gate decodes
    // too, or the spelling would reach the UI dir (and answer 404 where §3 promises 401).
    for path in ["/hub", "/hub/", "/hub/sessions", "/hub/tree", "/s", "/s/", "/s/x/api/events", "/%68ub/x", "/%73/x"] {
        for method in ["GET", "POST"] {
            let answer = Request::new(&perch.addr, method, path).send(&perch.addr);
            assert_eq!(answer.status, 401, "{method} {path} without a token: {}", answer.head);
            assert_eq!(answer.body, "", "a 401 says nothing: {path}");
        }
    }
}

#[test]
fn a_file_under_the_gated_namespaces_is_not_a_static_in_any_spelling() {
    let perch = Perch::start();
    // Both of these exist on disk, under `--ui-dir` (`ui/hub/name.txt`, `ui/s/name.txt`).
    // hub.md §3 keeps `/hub` and `/s` out of the statics, and the gate decodes the path the
    // way the static route does, so no spelling of either namespace serves them.
    assert!(perch.ui.join("hub/name.txt").is_file(), "the fixture file is really there");
    for path in ["/hub/name.txt", "/%68ub/name.txt", "/%68ub%2fname.txt", "/s/name.txt", "/%73/name.txt"] {
        let answer = Request::new(&perch.addr, "GET", path).send(&perch.addr);
        assert_eq!(answer.status, 401, "{path} without a token: {}", answer.head);
        assert_eq!(answer.body, "", "{path} reached a file under the gated namespace");
    }
    // With the token it is the 404 of a route that does not exist, not the file either.
    let auth = format!("Bearer {}", perch.token.trim());
    for path in ["/%68ub/name.txt", "/%73/name.txt"] {
        let answer = Request::new(&perch.addr, "GET", path).bearer(auth.clone()).send(&perch.addr);
        assert_eq!(answer.status, 404, "{path} with the token: {}", answer.head);
        assert_eq!(answer.body, "", "{path} served a file under the gated namespace");
    }
}

#[test]
fn a_gated_route_with_a_wrong_token_is_401() {
    let perch = Perch::start();
    let token = perch.token.trim().to_string();
    for wrong in [
        "Bearer wrong".to_string(),
        format!("Bearer {}", &token[..token.len() - 1]),
        format!("Bearer {token}x"),
        format!("Bearer {}", token.to_uppercase()),
        token.clone(),
        format!("bearer {token}"),
    ] {
        let answer = Request::new(&perch.addr, "GET", "/hub/anything").bearer(wrong.clone()).send(&perch.addr);
        assert_eq!(answer.status, 401, "{wrong:?} is not the token: {}", answer.head);
        assert_eq!(answer.body, "");
    }
}

#[test]
fn a_gated_route_that_is_not_built_yet_is_404_with_the_right_token() {
    // The point of H1a, and it still holds for every route nobody owns: the gate
    // passed, and what is behind it is not built (`/hub/events`: H2/H3; `GET /hub/tree`
    // is built since H2 and has its own tests at the end of this file). The routes a later slice owns
    // (`/hub/mesh`, `/hub/runs`, `/hub/term`, ...) are pinned the same way in that slice's own file, `tests/<slice>.rs`, so the
    // slice that builds one replaces its pin there and not in this list.
    // `/s` is built since H1c, but `x` is no session of this perch, so the proxy answers the
    // same empty 404 for it: a session route for a session that is not there.
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    for (method, path) in [
        ("GET", "/hub"),
        ("POST", "/hub/tree"),
        ("GET", "/hub/events"),
        ("GET", "/s"),
        ("GET", "/s/x/api/events"),
        ("POST", "/s/x/api/say"),
    ] {
        let answer = Request::new(&perch.addr, method, path).bearer(auth.clone()).send(&perch.addr);
        assert_eq!(answer.status, 404, "{method} {path} with the token: {}", answer.head);
        assert_eq!(answer.body, "", "no unbuilt route has a body: {path}");
    }
    // …and the routes that are built are not among them: they answer, which is what makes
    // the 404s above "not built yet" rather than "not for you".
    let sessions = Request::new(&perch.addr, "GET", "/hub/sessions").bearer(auth).send(&perch.addr);
    assert_eq!(sessions.status, 200, "{}", sessions.head);
    assert_eq!(sessions.body, "[]", "a perch with an empty sessions dir lists nothing");
}

#[test]
fn a_query_token_is_not_a_credential() {
    let perch = Perch::start();
    let token = perch.token.trim();
    for path in [
        format!("/hub/anything?token={token}"),
        format!("/hub/sessions?token={token}"),
        format!("/s?token={token}"),
        format!("/s/x/api/events?token={token}"),
    ] {
        let answer = Request::new(&perch.addr, "GET", &path).send(&perch.addr);
        assert_eq!(answer.status, 401, "{path} was taken as a credential: {}", answer.head);
    }
    // The header is still the header, query or not.
    let answer = Request::new(&perch.addr, "GET", &format!("/hub/anything?token={token}"))
        .bearer(format!("Bearer {token}"))
        .send(&perch.addr);
    assert_eq!(answer.status, 404, "{}", answer.head);
}

#[test]
fn a_query_string_on_a_session_route_is_400() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    for (method, path) in [
        ("GET", "/s/x/api/y?session=1"),
        ("POST", "/s/x/api/y?a=1"),
        ("GET", "/s/x/api/y?token=1"),
        ("GET", "/s?x=1"),
    ] {
        let answer = Request::new(&perch.addr, method, path).bearer(auth.clone()).send(&perch.addr);
        assert_eq!(answer.status, 400, "{method} {path} carries a query: {}", answer.head);
    }
    // Without the query it is the 404 of a route that does not exist, so the 400 above
    // is the query refusal and not a blanket refusal of the path.
    let plain = Request::new(&perch.addr, "GET", "/s/x/api/y").bearer(auth).send(&perch.addr);
    assert_eq!(plain.status, 404, "{}", plain.head);
    // `/hub` keeps its query strings: `/hub/tree?session=…&path=…` is one (H1b).
    // It is built since H2, so the query reaches the route: an unknown session is its 404.
    let hub = |path: &str| Request::new(&perch.addr, "GET", path).bearer(format!("Bearer {}", perch.token.trim())).send(&perch.addr).status;
    assert_eq!(hub("/hub/tree?session=nobody&path=/tmp"), 404);
    assert_eq!(hub("/hub/sessions?x=1"), 200);
}

// ---------------------------------------------------------------------------
// Gate 2 and 3: Host, Sec-Fetch-Site
// ---------------------------------------------------------------------------

#[test]
fn a_bad_host_is_403_on_both_a_static_and_a_gated_request() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    let port = perch.port;

    // The static route first: no token needed, and the `Host` check is all there is. The
    // last two are forms the piece-wise comparison admitted — a stray bracket and a
    // bracketed v4 — and neither is a string the bound authority renders.
    for host in [
        format!("evil.example:{port}"),
        format!("127.0.0.1:{}", port + 1),
        "127.0.0.1".to_string(),
        "perch.example".to_string(),
        format!("[127.0.0.1]:{port}"),
        format!("127.0.0.1]:{port}"),
    ] {
        let answer = Request::new(&perch.addr, "GET", "/").host(host.clone()).send(&perch.addr);
        assert_eq!(answer.status, 403, "Host: {host} on a static: {}", answer.head);
        // A refusal is still an answer from this origin, so it carries the policy.
        answer.assert_static_headers(&format!("the 403 for Host: {host}"));
    }

    // Then behind the token gate: the gate order means the 403 needs the token, and a
    // caller without one gets the 401 that hides the route table.
    for (method, path) in [("GET", "/hub/anything"), ("POST", "/s/x/api/y")] {
        let answer = Request::new(&perch.addr, method, path).bearer(auth.clone()).host(format!("evil.example:{port}")).send(&perch.addr);
        assert_eq!(answer.status, 403, "{method} {path} with a bad Host: {}", answer.head);
        let no_token = Request::new(&perch.addr, method, path).host(format!("evil.example:{port}")).send(&perch.addr);
        assert_eq!(no_token.status, 401, "the token gate comes first: {}", no_token.head);
    }
}

#[test]
fn localhost_is_the_one_other_name_the_host_may_carry() {
    let perch = Perch::start();
    for host in [format!("localhost:{}", perch.port), format!("127.0.0.1:{}", perch.port)] {
        let answer = Request::new(&perch.addr, "GET", "/").host(host.clone()).send(&perch.addr);
        assert_eq!(answer.status, 200, "Host: {host}: {}", answer.head);
    }
    for host in [format!("localhost.localdomain:{}", perch.port), format!("localhost:{}", perch.port + 1), "localhost".to_string()] {
        let answer = Request::new(&perch.addr, "GET", "/").host(host.clone()).send(&perch.addr);
        assert_eq!(answer.status, 403, "Host: {host} is another name or port: {}", answer.head);
    }
}

#[test]
fn a_cross_site_sec_fetch_site_is_403_and_an_absent_one_passes() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    // A page from another origin, on a static path and behind the token.
    for (method, path, bearer) in [
        ("GET", "/", None),
        ("GET", "/hub/anything", Some(auth.clone())),
        ("POST", "/s/x/api/y", Some(auth.clone())),
    ] {
        let mut request = Request::new(&perch.addr, method, path).header("Sec-Fetch-Site", "cross-site");
        if let Some(bearer) = bearer {
            request = request.bearer(bearer);
        }
        let answer = request.send(&perch.addr);
        assert_eq!(answer.status, 403, "{method} {path} cross-site: {}", answer.head);
    }
    // Same-origin and `none` (a typed URL), and no header at all: all pass.
    for site in ["same-origin", "none"] {
        let answer = Request::new(&perch.addr, "GET", "/").header("Sec-Fetch-Site", site).send(&perch.addr);
        assert_eq!(answer.status, 200, "Sec-Fetch-Site: {site}: {}", answer.head);
    }
    assert_eq!(Request::new(&perch.addr, "GET", "/").send(&perch.addr).status, 200, "curl sends none");
    // `same-site` is another origin on this host, which is not ours.
    let answer = Request::new(&perch.addr, "GET", "/").header("Sec-Fetch-Site", "same-site").send(&perch.addr);
    assert_eq!(answer.status, 403, "{}", answer.head);
}

// ---------------------------------------------------------------------------
// Gate 4 and 5: method shape, and the body cap
// ---------------------------------------------------------------------------

#[test]
fn options_is_405_and_no_cors_header_answers_a_preflight() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    // On a static path, with no token: 405, and GET/HEAD are what it would answer.
    let static_options = Request::new(&perch.addr, "OPTIONS", "/").send(&perch.addr);
    assert_eq!(static_options.status, 405, "{}", static_options.head);
    assert_eq!(static_options.header("allow").as_deref(), Some("GET, HEAD"));
    static_options.assert_static_headers("an OPTIONS on a static");
    // The preflight headers a browser would send; none of them is answered.
    let preflight = Request::new(&perch.addr, "OPTIONS", "/hub/sessions")
        .header("Origin", "https://evil.example")
        .header("Access-Control-Request-Method", "POST")
        .header("Access-Control-Request-Headers", "authorization")
        .send(&perch.addr);
    assert_eq!(preflight.status, 401, "the token gate is ahead of the method: {}", preflight.head);
    let preflight = Request::new(&perch.addr, "OPTIONS", "/hub/sessions").bearer(auth.clone()).send(&perch.addr);
    assert_eq!(preflight.status, 405, "{}", preflight.head);

    for answer in [&static_options, &preflight] {
        assert!(!answer.has_cors(), "a preflight was answered with CORS: {}", answer.head);
        assert!(answer.header("allow-origin").is_none());
        assert!(answer.header("access-control-allow-origin").is_none());
        assert!(answer.header("access-control-allow-headers").is_none());
    }
}

#[test]
fn a_gated_method_that_is_not_get_or_post_is_405_before_its_body_is_read() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    // hub.md §3: only GET and POST have a route behind the token — the proxy takes those
    // two and nothing else — and the route table says 404/405 after the token for the rest.
    for method in ["PUT", "PATCH", "DELETE", "HEAD"] {
        let answer = Request::new(&perch.addr, method, "/hub/anything").bearer(auth.clone()).body(vec![b'x'; 16]).send(&perch.addr);
        assert_eq!(answer.status, 405, "{method} /hub/anything with the token: {}", answer.head);
        assert_eq!(answer.body, "", "a 405 says nothing: {method}");
    }
    // Over the cap it is still the method's answer, which is what "before the body step"
    // means: a PUT's body is never read, by the cap or by a route.
    let over = send_oversize(&perch.addr, "PUT", "/hub/anything", Some(&auth), 8 * 1024 * 1024 + 1);
    assert_eq!(over.status, 405, "a PUT over the cap is refused as a method: {}", over.head);
    // Without a token the method is not looked at at all: gate 1 is ahead of gate 4.
    let no_token = Request::new(&perch.addr, "PUT", "/hub/anything").send(&perch.addr);
    assert_eq!(no_token.status, 401, "the token gate is ahead of the method: {}", no_token.head);
}

#[test]
fn a_body_over_the_cap_behind_the_gate_is_413() {
    let perch = Perch::start();
    let auth = format!("Bearer {}", perch.token.trim());
    // One byte over 8 MiB: refused before it is buffered, so the answer is the cap's
    // 413 and not the 404 a missing route would give.
    let over = send_oversize(&perch.addr, "POST", "/hub/anything", Some(&auth), 8 * 1024 * 1024 + 1);
    assert_eq!(over.status, 413, "{}", over.head);
    assert_eq!(over.body, "");
    // At the cap it is read and then the route that does not exist answers, which is
    // what makes the 413 above a boundary and not a blanket refusal of POSTs.
    let under = Request::new(&perch.addr, "POST", "/hub/anything").bearer(auth).body(vec![b'x'; 1024]).send(&perch.addr);
    assert_eq!(under.status, 404, "{}", under.head);
}

// ---------------------------------------------------------------------------
// Secrets: nothing anywhere ever prints the token
// ---------------------------------------------------------------------------

#[test]
fn the_token_never_appears_in_the_output_at_trace() {
    let mut perch = Perch::start();
    let token = perch.token.trim().to_string();
    let auth = format!("Bearer {token}");
    // Every way a token could reach a log line: a good header, a wrong one, a query
    // parameter, and a body over the cap. What the log holds is perch's own lines — hyper 1
    // emits no `tracing` events unless it is built with `--cfg hyper_unstable_tracing` — and
    // the run is at `RUST_LOG=trace`, so none of perch's lines is filtered out of it.
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/anything").bearer(auth.clone()).send(&perch.addr).status, 404);
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/anything").bearer("Bearer wrong".to_string()).send(&perch.addr).status, 401);
    assert_eq!(Request::new(&perch.addr, "GET", &format!("/hub/anything?token={token}")).send(&perch.addr).status, 401);
    assert_eq!(send_oversize(&perch.addr, "POST", "/hub/anything", Some(&auth), 8 * 1024 * 1024 + 1).status, 413);
    assert_eq!(Request::new(&perch.addr, "GET", "/").send(&perch.addr).status, 200);

    // Stop it, so the shutdown path's own lines are in the output too.
    let status = perch.stop();
    assert!(status.success(), "{status}");
    // The pump threads read the pipes to EOF once the child is gone.
    let deadline = Instant::now() + Duration::from_secs(10);
    while (!perch.err().contains("stopping") || perch.out().lines().count() < 2) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    let stdout = perch.out();
    let stderr = perch.err();
    assert_eq!(stdout.lines().count(), 2, "stdout is only the two boot lines: {stdout:?}");
    // Non-empty, which "stopping" on its own satisfies: a guard against a log this test
    // never sees, not evidence that a request was logged.
    assert!(!stderr.is_empty(), "the trace output is not empty, so this test looks at something");
    assert!(!stdout.contains(&token), "the token is in stdout: {stdout:?}");
    assert!(!stderr.contains(&token), "the token is in the stderr log: {stderr:?}");
    // …and the request that carried it as a header, a query and a body left no trace of
    // the header itself either.
    assert!(!stdout.contains("Authorization"), "{stdout:?}");
    assert!(!stderr.contains("Authorization"), "{stderr:?}");
    assert!(!perch.boot.contains(&token), "the boot lines carry the token: {:?}", perch.boot);
}

/// hub.md §10.4 row 1 names this test: "read `/proc/<pid>/cmdline` and `environ` of hub and
/// children". The perch hands its token to nothing and takes one from nothing, so its own
/// is the only token `/proc` could show — and if it were passed as an argument or a
/// variable, the 0600 file would no longer be the only place it lives.
#[test]
fn the_token_is_not_in_the_childs_argv_or_environment() {
    let perch = Perch::start();
    let token = perch.token.trim().to_string();
    let pid = perch.child.id();
    let argv = std::fs::read(format!("/proc/{pid}/cmdline")).expect("the child's cmdline");
    let environ = std::fs::read(format!("/proc/{pid}/environ")).expect("the child's environ");
    // `cmdline` and `environ` are NUL-separated byte strings, so the token is a substring of
    // them if it is in them at all; read lossily, every entry is visible and a byte that is
    // not UTF-8 does not fail the read.
    let argv = String::from_utf8_lossy(&argv).to_string();
    let environ = String::from_utf8_lossy(&environ).to_string();
    // What was read is this test's own spawn: an empty or wrong read cannot pass.
    assert!(argv.contains("--bind"), "the child's argv was read: {argv:?}");
    assert!(environ.contains("XDG_RUNTIME_DIR"), "the child's environment was read");
    assert!(!argv.contains(&token), "the token is in the child's argv: {argv:?}");
    assert!(!environ.contains(&token), "the token is in the child's environment");
    // …nor the header a caller would present it in, which is a spelling a leak could use.
    assert!(!argv.contains("Authorization"), "the child's argv names the header: {argv:?}");
    assert!(!environ.contains("Authorization"), "the child's environment names the header");
}

// ---------------------------------------------------------------------------
// H1b: spawn, list, resume, stop — against the stand-in door (hub.md §4, §12)
// ---------------------------------------------------------------------------

/// The four routes H1b adds are behind the same gates as the namespace they live in, and the
/// cap still answers before any route does.
#[test]
fn the_session_routes_are_behind_the_same_gates_as_the_namespace() {
    let perch = Perch::start_hub(&[]);
    for (method, path) in [
        ("GET", "/hub/sessions"),
        ("POST", "/hub/sessions"),
        ("POST", "/hub/sessions/x/resume"),
        ("POST", "/hub/sessions/x/stop"),
    ] {
        // Gate 1: no token, and the answer is empty — a route that exists and one that does
        // not are the same 401.
        let answer = Request::new(&perch.addr, method, path).send(&perch.addr);
        assert_eq!(answer.status, 401, "{method} {path} without a token: {}", answer.head);
        assert_eq!(answer.body, "");
        // Gate 1 again: a wrong token is not a token.
        let wrong = Request::new(&perch.addr, method, path).bearer("Bearer wrong").send(&perch.addr);
        assert_eq!(wrong.status, 401, "{method} {path} with a wrong token: {}", wrong.head);
        // Gate 2: DNS rebinding, on the routes that spawn and stop processes too.
        let rebound = Request::new(&perch.addr, method, path).bearer(perch.auth()).host("evil.example").send(&perch.addr);
        assert_eq!(rebound.status, 403, "{method} {path} with a bad Host: {}", rebound.head);
        // Gate 3.
        let cross = Request::new(&perch.addr, method, path).bearer(perch.auth()).header("Sec-Fetch-Site", "cross-site").send(&perch.addr);
        assert_eq!(cross.status, 403, "{method} {path} cross-site: {}", cross.head);
    }
    // Gate 4: only POST reaches a verb, and a gated method that is neither GET nor POST is
    // 405 before its body is read.
    assert_eq!(perch.get("/hub/sessions/x/stop").status, 404, "a verb is POST only");
    assert_eq!(perch.apireq("DELETE", "/hub/sessions").send(&perch.addr).status, 405);
    // …and a query string is not part of either verb.
    assert_eq!(perch.post("/hub/sessions/x/stop?now=1").status, 404);
    // Gate 5: a body past the cap is still 413 on a route that exists.
    let over = send_oversize(&perch.addr, "POST", "/hub/sessions", Some(&perch.auth()), 8 * 1024 * 1024 + 1);
    assert_eq!(over.status, 413, "{}", over.head);
    assert!(perch.fake_records().is_empty(), "nothing was spawned for a refused body");
}

/// A new session's cwd is checked against `--root` (hub.md §4 step 1) before anything is
/// forked, and a body that is not the shape the route takes is refused before that.
#[test]
fn a_cwd_outside_every_root_is_422() {
    let perch = Perch::start_hub(&[]);
    let outside = tempfile::tempdir().unwrap();
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": outside.path().display().to_string() }));
    assert_eq!(answer.status, 422, "{}", answer.head);
    assert!(answer.body.contains("error"), "a refusal says why: {}", answer.body);
    // Not there at all, and not a directory: the same 422, because neither can be under a
    // root.
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({ "cwd": "/nowhere/at/all" })).status, 422);
    // Inside the root but not a directory: the same 422, and nothing is forked for it. (A
    // *directory* inside the root is exactly what this route is for; the UI dir is one.)
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.ui.join("index.html").display().to_string() })).status, 422);
    // A body that is not the shape the route takes, and one that is not JSON at all: 400.
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({})).status, 400);
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({ "cwd": 7 })).status, 400);
    assert_eq!(perch.apireq("POST", "/hub/sessions").body(b"not json".to_vec()).send(&perch.addr).status, 400);
    // Nothing was forked for any of them.
    assert!(perch.fake_records().is_empty(), "{:?}", perch.fake_records());
}

/// The log's stem, and nothing else: a bad id is a 404 (never a 400, so it cannot be told
/// from a route that does not exist) and never a path.
#[test]
fn a_bad_id_on_resume_or_stop_is_404_and_never_a_path() {
    let perch = Perch::start_hub(&[]);
    for id in ["..%2Fescape", "../escape", ".hidden", "-lead", "a%20b", "%2e%2e", &"x".repeat(129)] {
        for verb in ["resume", "stop"] {
            let path = format!("/hub/sessions/{id}/{verb}");
            let answer = perch.post(&path);
            assert_eq!(answer.status, 404, "POST {path}: {}", answer.head);
        }
    }
    // `..` as a whole path segment collapses the route itself, which is still a 404.
    assert_eq!(perch.post("/hub/sessions/../escape/resume").status, 404);
    assert_eq!(perch.post("/hub/sessions/ctf-pwn-3/../ctf-pwn-3/resume").status, 404);
    // An id that is well-formed but names no log is a 404 too, and one that is well-formed
    // and not live is a 409: the two are different answers on purpose.
    assert_eq!(perch.post("/hub/sessions/ctf-pwn-3/resume").status, 404);
    assert_eq!(perch.post("/hub/sessions/ctf-pwn-3/stop").status, 409);
}

/// The kept half of the list: `*.eid` in the sessions dir, by stem, with an mtime and no
/// cwd (the perch has not read the log, and reading it is not H1b's business).
#[test]
fn the_list_shows_kept_logs_and_nothing_else_in_the_directory() {
    let perch = Perch::start_hub(&[]);
    perch.keep_a_log("ctf-pwn-3");
    std::fs::write(perch.sessions.path().join("notes.txt"), "not a log\n").unwrap();
    std::fs::write(perch.sessions.path().join("-leading-dash.eid"), "{}\n").unwrap();
    let list = perch.list();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["id"], "ctf-pwn-3");
    assert_eq!(list[0]["state"], "kept");
    assert_eq!(list[0]["panes"], 0, "nothing proxied is open, so no pane is");
    assert!(list[0]["mtime"].as_u64().is_some(), "a kept log has an mtime: {}", list[0]);
    assert!(list[0].get("cwd").is_none(), "a kept log's cwd is not read: {}", list[0]);
}

/// A log the roster says another consumer holds is `live-elsewhere` in the list, and a
/// resume of it is refused (hub.md §4 step 2's roster half) — the perch never adopts a door
/// it did not start (Q5).
#[test]
fn a_log_another_consumer_holds_is_listed_and_not_resumable() {
    let perch = Perch::start_hub(&[]);
    let id = perch.keep_a_log("ctf-pwn-3");
    let log = perch.sessions.path().join("ctf-pwn-3.eid");
    perch.roster_row("eidolon-9f2c", &log, std::process::id());
    let row = perch.row(&id).expect("a kept log is listed");
    assert_eq!(row["state"], "live-elsewhere", "{row}");
    assert_eq!(row["title"], "a peer", "{row}");
    let refused = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(refused.status, 409, "{}", refused.head);
    assert!(perch.fake_records().is_empty(), "no door was forked for a log someone else holds");
}

// ---------------------------------------------------------------------------
// H1b: the lifecycle against the real stand-in door (spawn -> live -> stop -> resume)
// ---------------------------------------------------------------------------

/// The whole of hub.md §4 in one test, because the steps are only meaningful in order:
/// spawn a new session, see it `live` through the perch's *own* watcher, stop it cleanly,
/// and resume the kept log.
#[cfg(feature = "test-bins")]
#[test]
fn a_new_session_spawns_reaches_live_and_stops_clean() {
    let perch = Perch::start_hub(&[]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let id = body_id(&answer);
    assert!(!id.is_empty() && !id.contains('/'), "the answer's id is a log stem: {id}");

    // (a) §4 step 3: the argv is the door's own, with `--cwd` and no token anywhere in it.
    let (pid, port) = perch.fake_last_start();
    let argv = perch.fake_argv();
    assert!(argv.contains("--cwd"), "the door is told where to work: {argv}");
    assert!(argv.contains(&perch.work.display().to_string()), "{argv}");
    assert!(!argv.contains("--session"), "a new session is not a resume: {argv}");
    assert!(!argv.contains("--ui-dir"), "the door serves no statics under the perch: {argv}");
    assert!(!argv.contains(perch.token.trim()), "the perch's token is not in the argv: {argv}");
    assert!(!argv.contains("Bearer") && !argv.contains("Authorization"), "…nor a header: {argv}");
    // …and the same for the child's bytes, which is hub.md §10.4 row 1's own test.
    let (cmdline, environ) = proc_of(pid);
    assert!(cmdline.contains("--cwd"), "the child's argv was read: {cmdline:?}");
    assert!(environ.contains("XDG_RUNTIME_DIR"), "the child's environment was read");
    assert!(!cmdline.contains(perch.token.trim()), "the perch's token is in the child's argv");
    assert!(!environ.contains(perch.token.trim()), "the perch's token is in the child's environment");
    assert!(!cmdline.contains("Authorization") && !environ.contains("Authorization"));

    // The door's own token: minted by the door, read from its `0600` file, and in the
    // watcher's `Authorization` header — which is the only place it ever goes.
    let door_token = perch.door_token(port);
    assert!(door_token.len() >= 20, "the door minted a real token: {door_token:?}");
    assert!(!cmdline.contains(&door_token), "the door's token is not in its own argv either");
    assert!(!environ.contains(&door_token), "…nor its environment");
    let records = perch.fake_records();
    assert!(
        records.iter().any(|line| line == &format!("GET /api/events auth=Bearer {door_token}")),
        "the watcher asked for the door's own stream with the door's own token: {records:?}"
    );

    // (b) §4 step 7: `hello` was read, so the door is live and its log is a kept session.
    let row = perch.row(&id).expect("the new session is listed");
    assert_eq!(row["state"], "live", "{row}");
    assert_eq!(row["cwd"], perch.work.canonicalize().unwrap().display().to_string(), "hello's cwd is the row's: {row}");
    assert_eq!(row["panes"], 0, "nothing proxied is open, so no pane is");
    assert!(perch.sessions.path().join(format!("{id}.eid")).is_file(), "the door wrote the log it named");

    // (c) The Stop finding: the watcher is closed *before* the door is signalled, so the
    // door is not holding a stream open and dies at once.
    let started = Instant::now();
    let stopped = perch.post(&format!("/hub/sessions/{id}/stop"));
    let took = started.elapsed();
    assert_eq!(stopped.status, 202, "{}", stopped.head);
    assert!(took < Duration::from_secs(5), "a clean stop does not wait the grace out: {took:?}");
    assert!(wait_gone(pid, Duration::from_secs(5)), "the door is gone, and reaped");
    assert!(!perch.door_token_file(port).exists(), "the door removed its own token file");
    assert!(perch.door_token_files().is_empty(), "no orphan token file: {:?}", perch.door_token_files());
    let records = perch.fake_records();
    let closed = records.iter().position(|line| line == "stream-closed").expect("the watcher was closed");
    let term = records.iter().position(|line| line.starts_with("sigterm ")).expect("the door was signalled");
    assert!(closed < term, "close the watcher, then signal: {records:?}");
    assert!(records.iter().any(|line| line == "sigterm streams=0"), "…with no stream left open: {records:?}");

    // (d) The log is kept, so the session is still there — and resumable.
    let row = perch.row(&id).expect("a stopped session is still listed");
    assert_eq!(row["state"], "kept", "a cleanly stopped session is kept (hub.md section 4), not dead: {row}");
    let resumed = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(resumed.status, 200, "{}{}", resumed.head, resumed.body);
    assert_eq!(body_id(&resumed), id, "a resume answers with the id it resumed");
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));
    let (again_pid, again_port) = perch.fake_last_start();
    assert_ne!(again_pid, pid, "a resume is a new door, not the old one");
    let argv = perch.fake_argv();
    assert!(argv.contains("--session"), "{argv}");
    assert!(argv.contains(&format!("{id}.eid")), "on the log it was asked for: {argv}");
    assert!(perch.door_token(again_port).len() >= 20, "the new door minted a token of its own");

    // Leave nothing running: the same ordered stop, once more.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(wait_gone(again_pid, Duration::from_secs(5)));
    assert!(perch.door_token_files().is_empty());
}

/// The Minerva home: the perch generates `<home>/config.toml` from the member's config, every door's
/// argv carries it (new and resumed), a new session with no cwd works in its own `<home>/scratch/<stamp>`,
/// and without `--home` that same body is the 400 it always was. Reverting any of these fails here.
#[cfg(feature = "test-bins")]
#[test]
fn the_minerva_home_gives_every_door_its_config_and_a_cwd_less_session_its_scratch() {
    // Created inside `--root` (the hub harness's), where the perch can canonicalize it at start.
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("minerva.toml"), "default_model = \"mock\"\ndefault_mode = \"auto\"\n").unwrap();
    let home = home.canonicalize().unwrap();
    let config = home.join("config.toml");
    // A stale hand-written config.toml is regenerated away, not honoured.
    std::fs::write(&config, "sessions_dir = \"/hand/edited\"\n").unwrap();
    let member = "default_model = \"theirs\"\nsessions_dir = \"~/elsewhere\"\nthinking_budget = 4000\n\n[swarm]\nenabled = false\n\n[[providers]]\nname = \"mine\"\n";
    let perch = Perch::start_hub_member(
        &["--root".into(), base.path().canonicalize().unwrap().into(), "--home".into(), home.clone().into()],
        member,
    );
    let generated: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
    assert_eq!(generated["sessions_dir"].as_str(), Some(home.join("sessions").to_str().unwrap()), "forced, absolute");
    assert_eq!(generated["default_model"].as_str(), Some("mock"), "minerva.toml beats the member's own default_model");
    assert_eq!(generated["thinking_budget"].as_integer(), Some(4000), "the member's keys survive");
    assert_eq!(generated["swarm"]["enabled"].as_bool(), Some(false));
    assert_eq!(generated["providers"][0]["name"].as_str(), Some("mine"));

    let answer = perch.post_json("/hub/sessions", serde_json::json!({}));
    assert_eq!(answer.status, 201, "no cwd + a home is a scratch dir: {}{}", answer.head, answer.body);
    let id = body_id(&answer);
    let argv = perch.fake_argv();
    let scratch = home.join("scratch");
    assert!(argv.starts_with(&format!("argv --config {} web ", config.display())), "--config leads the door's argv: {argv}");
    let row = perch.list().into_iter().find(|row| row["id"] == id.as_str()).expect("the new session is listed");
    let cwd = PathBuf::from(row["cwd"].as_str().unwrap());
    assert_eq!(cwd.parent(), Some(scratch.as_path()), "a fresh directory under <home>/scratch: {cwd:?}");
    assert!(cwd.is_dir() && argv.contains(&format!("--cwd {}", cwd.display())), "{argv}");

    // A second chat gets its own directory, so two chats never share files.
    let other = perch.post_json("/hub/sessions", serde_json::json!({}));
    assert_eq!(other.status, 201, "{}{}", other.head, other.body);
    let other_id = body_id(&other);
    let other_row = perch.list().into_iter().find(|row| row["id"] == other_id.as_str()).unwrap();
    let other_cwd = PathBuf::from(other_row["cwd"].as_str().unwrap());
    assert_ne!(other_cwd, cwd, "one scratch directory per chat");
    assert_eq!(other_cwd.parent(), Some(scratch.as_path()));
    assert_eq!(perch.post(&format!("/hub/sessions/{other_id}/stop")).status, 202);

    // A named cwd still wins, and is still judged against the roots.
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({ "cwd": "/nowhere/at/all" })).status, 422);

    // Resume: the same config on the reopened door.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    let (pid, _) = perch.fake_last_start();
    assert!(wait_gone(pid, Duration::from_secs(5)));
    let resumed = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(resumed.status, 200, "{}{}", resumed.head, resumed.body);
    let argv = perch.fake_argv();
    assert!(argv.starts_with(&format!("argv --config {} web ", config.display())), "a resume carries the config too: {argv}");
    assert!(argv.contains("--session"), "{argv}");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// The listing follows the config the doors run with. With `--home` that is `<home>/sessions` whatever
/// the member's config says; with an explicit `--eidolon-config` and no `--sessions-dir` it is the file's own
/// (absolute) `sessions_dir`. Mutations: list the shared default dir instead (the `a` row is missing).
#[test]
fn the_listing_follows_the_config_the_doors_run_with() {
    // --home: a log in <home>/sessions is listed, and the member's own sessions_dir is not.
    let base = tempfile::tempdir().unwrap();
    let theirs = base.path().join("theirs");
    std::fs::create_dir(&theirs).unwrap();
    std::fs::write(theirs.join("t.eid"), "{}\n").unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(home.join("sessions/a.eid"), "{}\n").unwrap();
    let member = format!("sessions_dir = \"{}\"\n", theirs.display());
    let perch = Perch::launch_with_member(true, None, &["--home".into(), home.clone().into(), "--root".into(), base.path().into()], Some(&member));
    let ids: Vec<String> = perch.list().iter().map(|row| row["id"].as_str().unwrap().to_string()).collect();
    // The member's own dir is eidolon's: the read-only second source, listed and flagged, never the home's.
    assert_eq!(ids, ["a", "t"], "<home>/sessions, then the member's directory as the second source");
    let rows = perch.list();
    assert!(rows.iter().find(|r| r["id"] == "a").unwrap().get("origin").is_none());
    assert_eq!(rows.iter().find(|r| r["id"] == "t").unwrap()["origin"], "other");
    drop(perch);

    // An explicit config, no home: its absolute sessions_dir is the listing.
    let config = base.path().join("c.toml");
    std::fs::write(&config, format!("sessions_dir = '''{}'''\n", theirs.display())).unwrap();
    let mut args: Vec<std::ffi::OsString> = vec!["--eidolon-config".into(), config.clone().into()];
    let perch = Perch::launch(true, None, &args);
    let ids: Vec<String> = perch.list().iter().map(|row| row["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids, ["t"], "the explicit config's own sessions_dir");
    drop(perch);

    // A `~` or relative sessions_dir would be a different directory to the door: the perch refuses to start.
    for bad in ["~/x", "rel/x"] {
        std::fs::write(&config, format!("sessions_dir = \"{bad}\"\n")).unwrap();
        args = vec!["--eidolon-config".into(), config.clone().into()];
        let out = Perch::refused_start(&args);
        assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("not an absolute path"), "{bad}: {:?}", out);
    }
}

/// Without `--home` and `--eidolon-config` nothing changes: no `--config` in the argv, and a
/// body with no cwd is refused.
#[cfg(feature = "test-bins")]
#[test]
fn without_a_home_the_door_argv_has_no_config_and_a_cwd_is_required() {
    let perch = Perch::start_hub(&[]);
    assert_eq!(perch.post_json("/hub/sessions", serde_json::json!({})).status, 400);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let argv = perch.fake_argv();
    assert!(argv.starts_with("argv --yolo web "), "no --config, and a new chat is yolo unless it says otherwise: {argv}");
    let id = body_id(&answer);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// The second half of the Stop finding: a door that ignores SIGTERM is killed after the
/// grace, and the token file it could not remove is the perch's to remove.
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_ignores_sigterm_is_killed_and_the_perch_removes_its_token_file() {
    let perch = Perch::start_hub(&[("FAKE_IGNORE_SIGTERM", "1")]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let id = body_id(&answer);
    let (pid, port) = perch.fake_last_start();
    assert!(perch.door_token_file(port).exists(), "the door wrote its token file");

    let started = Instant::now();
    let stopped = perch.post(&format!("/hub/sessions/{id}/stop"));
    let took = started.elapsed();
    assert_eq!(stopped.status, 202, "{}", stopped.head);
    // hub.md §4 "Stop": SIGTERM, then 10 s, then SIGKILL. The wait is the perch's, and the
    // record of it is this duration.
    assert!(took >= Duration::from_secs(10), "the perch waited its grace out: {took:?}");
    assert!(took < Duration::from_secs(20), "…and not longer: {took:?}");
    assert!(wait_gone(pid, Duration::from_secs(5)), "the SIGKILL landed");
    assert!(
        perch.fake_records().iter().any(|line| line == "sigterm-ignored"),
        "the door ignored the SIGTERM it was sent: {:?}",
        perch.fake_records()
    );
    // The door never got to run its own `remove_file`, so this is the perch's work.
    assert!(!perch.door_token_file(port).exists(), "an orphaned door token file was left behind");
    assert!(perch.door_token_files().is_empty(), "{:?}", perch.door_token_files());
    // A SIGKILL is not a clean exit: the row stays `dead` and the proxy answers 503.
    assert_eq!(perch.state_of(&id).as_deref(), Some("dead"));
    let gone = perch.apireq("GET", &format!("/s/{id}/api/events")).send(&perch.addr);
    assert_eq!((gone.status, gone.body.as_str()), (503, "{\"state\":\"dead\"}"), "{}", gone.head);
}

/// A door that dies by itself is a `dead` row, and its token file does not outlive it.
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_exits_on_its_own_leaves_a_dead_row() {
    let perch = Perch::start_hub(&[("FAKE_EXIT_AFTER_MS", "2000")]);
    let id = perch.keep_a_log("crashy");
    let resumed = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(resumed.status, 200, "the door reached hello first: {}", resumed.head);
    let (pid, port) = perch.fake_last_start();
    assert!(perch.wait_state(&id, "dead", Duration::from_secs(15)), "the child exited: {:?}", perch.state_of(&id));
    assert!(wait_gone(pid, Duration::from_secs(5)));
    assert!(!perch.door_token_file(port).exists(), "a door that died left its token file to the perch");
    // The log is still there, so a resume is still allowed after the crash.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// hub.md §4 step 2: two writers on one log are unguarded upstream, so the perch refuses the
/// second one — ours, and someone else's.
#[cfg(feature = "test-bins")]
#[test]
fn a_second_writer_on_one_log_is_409() {
    let perch = Perch::start_hub(&[]);
    let id = perch.keep_a_log("one-writer");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    let (pid, _port) = perch.fake_last_start();
    let held = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(held.status, 409, "{}", held.head);
    assert!(held.body.contains("error"), "{}", held.body);
    // Only one door was forked for the two requests.
    let starts = perch.fake_records().iter().filter(|line| line.starts_with("start ")).count();
    assert_eq!(starts, 1, "{:?}", perch.fake_records());
    // A live-elsewhere log is refused as well, before anything is forked.
    perch.roster_row("eidolon-9f2c", &perch.sessions.path().join("one-writer.eid"), std::process::id());
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(wait_gone(pid, Duration::from_secs(5)));
    let after = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(after.status, 409, "the roster still holds the log: {}", after.head);
    // With the roster gone (the peer stopped), the log is resumable again.
    std::fs::remove_dir_all(perch.runtime.path().join("eidolon/eidolon-9f2c")).unwrap();
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// §4 step 5: a door that never prints its boot lines is killed at the startup timeout, the
/// spawn is a 503, and the row it reserved is `dead` rather than stuck `starting`.
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_never_boots_is_503_and_dead() {
    let perch = Perch::start_hub(&[("FAKE_SILENT", "1")]);
    let id = perch.keep_a_log("silent-session");
    let started = Instant::now();
    let answer = perch.post(&format!("/hub/sessions/{id}/resume"));
    let took = started.elapsed();
    assert_eq!(answer.status, 503, "{}", answer.head);
    assert!(took >= Duration::from_secs(20), "the perch waited its startup timeout: {took:?}");
    assert!(took < Duration::from_secs(40), "…and not longer: {took:?}");
    assert_eq!(perch.state_of(&id).as_deref(), Some("dead"), "a door that never came up is dead, not starting");
    assert!(perch.door_token_files().is_empty(), "the silent door removed its own token file: {:?}", perch.door_token_files());
}

/// The same startup timeout on the *new session* path, where there is no id to reserve: the
/// answer is a 503 and the list is unchanged (nothing names a session that never was).
#[cfg(feature = "test-bins")]
#[test]
fn a_new_session_that_never_boots_is_503() {
    let perch = Perch::start_hub(&[("FAKE_SILENT", "1")]);
    let started = Instant::now();
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 503, "{}", answer.head);
    assert!(started.elapsed() >= Duration::from_secs(20), "the startup timeout, again");
    // No session was *invented*: a silent door did write a log of its own (a real one does,
    // before it prints), but nothing is live or starting, because nothing reached hello.
    let live: Vec<_> = perch.list().into_iter().filter(|row| row["state"] == "live" || row["state"] == "starting").collect();
    assert!(live.is_empty(), "nothing was adopted: {live:?}");
    assert!(perch.door_token_files().is_empty(), "{:?}", perch.door_token_files());
}

/// §4 step 6: a door token that is world-readable is refused, not read. The door ignores
/// SIGTERM here, so the file's removal is the perch's and not the door's.
#[cfg(feature = "test-bins")]
#[test]
fn a_door_token_that_is_not_owner_only_is_refused() {
    let perch = Perch::start_hub(&[("FAKE_MODE", "0644"), ("FAKE_IGNORE_SIGTERM", "1")]);
    let id = perch.keep_a_log("loose-token");
    let answer = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(answer.status, 503, "{}", answer.head);
    assert_eq!(perch.state_of(&id).as_deref(), Some("dead"));
    let (_pid, port) = perch.fake_last_start();
    let token_file = perch.door_token_file(port);
    if token_file.exists() {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&token_file).unwrap().permissions().mode() & 0o777, 0o644, "the fake really wrote a loose file");
    }
    assert!(!token_file.exists(), "the perch removed the token file it refused to read");
}

/// §4 step 6: a token file outside `<runtime>/eidolon` is refused, and the path is not one
/// the perch will touch either — a path a child printed is not a file to delete.
#[cfg(feature = "test-bins")]
#[test]
fn a_token_file_outside_the_runtime_dir_is_refused() {
    let perch = Perch::start_hub(&[("FAKE_TOKEN_ELSEWHERE", "1")]);
    let id = perch.keep_a_log("elsewhere-token");
    let answer = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(answer.status, 503, "{}", answer.head);
    assert!(answer.body.contains("eidolon"), "the refusal names what it expected: {}", answer.body);
    assert_eq!(perch.state_of(&id).as_deref(), Some("dead"));
    let (_pid, port) = perch.fake_last_start();
    assert!(!perch.door_token_file(port).exists(), "nothing was adopted into the runtime dir");
    let _ = std::fs::remove_file(std::env::temp_dir().join(format!("fake-door-{port}.token")));
}

/// hub.md §2's "Shutdown" row: SIGTERM the perch and its doors go with it — the watcher is
/// closed first, so a door that would otherwise wait its stream out exits at once.
#[cfg(feature = "test-bins")]
#[test]
fn a_perch_shutdown_stops_its_doors_and_leaves_no_token_file() {
    let mut perch = Perch::start_hub(&[]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let (pid, port) = perch.fake_last_start();
    assert!(perch.door_token_file(port).exists());

    let started = Instant::now();
    let status = perch.stop();
    assert!(status.success(), "a signalled stop is a clean exit: {status}");
    assert!(started.elapsed() < Duration::from_secs(5), "the perch does not wait its children out: {:?}", started.elapsed());
    assert!(wait_gone(pid, Duration::from_secs(5)), "the door did not outlive the perch");
    assert!(!perch.door_token_file(port).exists(), "the door's token file outlived the listener");
    assert!(!perch.token_file.exists(), "the perch's own token file outlived the listener");
    let records = perch.fake_records();
    assert!(records.iter().any(|line| line == "sigterm streams=0"), "the shutdown closed the watcher first: {records:?}");
}

/// hub.md §2's PDEATHSIG hypothesis, measured the way the row says to: `kill -9` the perch
/// and the doors must not be left behind. The child is forked by the perch's one long-lived
/// thread precisely so this signal arrives when the *process* dies.
#[cfg(feature = "test-bins")]
#[test]
fn kill_minus_nine_on_the_perch_leaves_no_door_behind() {
    let perch = Perch::start_hub(&[]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let (pid, port) = perch.fake_last_start();
    let perch_pid = perch.child.id() as i32;
    assert_eq!(unsafe { libc::kill(perch_pid, libc::SIGKILL) }, 0, "kill -9 the perch");
    assert!(wait_gone(pid, Duration::from_secs(10)), "PDEATHSIG did not reach the door");
    // A door that dies of SIGTERM removes its own token file, so nothing is orphaned here.
    let deadline = Instant::now() + Duration::from_secs(5);
    while perch.door_token_file(port).exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!perch.door_token_file(port).exists(), "the door never removed its token file");
}

/// hub.md §10.4: neither token — the perch's or a door's — reaches the perch's output, and
/// no answer body carries one. Run with a whole lifecycle so every path's log lines exist.
#[cfg(feature = "test-bins")]
#[test]
fn neither_token_reaches_the_output_or_an_answer() {
    // `FAKE_REFUSE` makes `cancel` a door 401, so a 502 is among the proxied answers below.
    let mut perch = Perch::start_hub(&[("FAKE_REFUSE", "/api/cancel")]);
    let perch_token = perch.token.trim().to_string();
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let id = body_id(&answer);
    let (pid, port) = perch.fake_last_start();
    let door_token = perch.door_token(port);

    // H1c: proxied traffic of every kind — a say, an answer, an open stream, and a 502 — and
    // neither token in any head or body the page was handed. (The door's token is in exactly
    // one header, the perch -> door one; the page's is in the one it sent.)
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));
    let said = perch.post_json(&format!("/s/{id}/api/say"), serde_json::json!({ "text": "hi" }));
    let answered = perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": 1, "answer": "ok", "note": "n" }));
    let refused = perch.post_json(&format!("/s/{id}/api/cancel"), serde_json::json!({}));
    assert_eq!((said.status, answered.status, refused.status), (202, 204, 502), "{}{}{}", said.head, answered.head, refused.head);
    assert!(pane.read_until("ask-settled", Duration::from_secs(5)));
    for (what, head, body) in [
        ("a say", said.head.as_str(), said.body.as_str()),
        ("an answer", answered.head.as_str(), answered.body.as_str()),
        ("a 502", refused.head.as_str(), refused.body.as_str()),
        ("a pane", pane.head.as_str(), pane.data().0.as_str()),
    ] {
        for token in [&door_token, &perch_token] {
            assert!(!head.contains(token.as_str()) && !body.contains(token.as_str()), "a token is in {what}: {head} {body}");
        }
        assert!(!head.to_ascii_lowercase().contains("authorization"), "{what} names the header: {head}");
    }
    drop(pane);

    // Every answer a page can read, including the refusals.
    let list = perch.get("/hub/sessions");
    assert_eq!(list.status, 200);
    assert!(!list.body.contains(&door_token), "a door token is in the list: {}", list.body);
    assert!(!list.body.contains(&perch_token), "the perch token is in the list");
    let conflict = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(conflict.status, 409, "{}", conflict.head);
    assert!(!conflict.body.contains(&door_token) && !conflict.body.contains(&perch_token));
    let refused = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": "/nowhere" }));
    assert_eq!(refused.status, 422);
    assert!(!refused.body.contains(&door_token) && !refused.body.contains(&perch_token));
    assert!(!perch.get("/hub/sessions/../x/resume").body.contains(&door_token));

    // …and the process's own output. The door is stopped first, so the shutdown path's own
    // lines are in it too, and then the perch.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(wait_gone(pid, Duration::from_secs(5)));
    let status = perch.stop();
    assert!(status.success(), "{status}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !perch.err().contains("stopping") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let stdout = perch.out();
    let stderr = perch.err();
    assert_eq!(stdout.lines().count(), 2, "stdout is only the two boot lines: {stdout:?}");
    assert!(!stderr.is_empty(), "the trace output is not empty, so this test looks at something");
    for (what, token) in [("the perch's token", &perch_token), ("a door's token", &door_token)] {
        assert!(!stdout.contains(token.as_str()), "{what} is in stdout: {stdout:?}");
        assert!(!stderr.contains(token.as_str()), "{what} is in stderr");
        assert!(!perch.boot.contains(token.as_str()), "{what} is in the boot lines");
    }
    assert!(!stdout.contains("Authorization") && !stderr.contains("Authorization"), "{stderr}");
}

// ---------------------------------------------------------------------------
// H1c: the proxy, `/s/<id>/api/<route>` (hub.md §5, §6)
// ---------------------------------------------------------------------------

/// A body of base64-looking characters, as a `say` with two images carries: deterministic, and
/// not a repeating byte, so a body that was truncated, reordered or re-chunked wrongly differs.
fn images_body(total: usize) -> Vec<u8> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut seed: u32 = 0x1234_5678;
    let mut image = |len: usize| -> String {
        (0..len)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                alphabet[(seed >> 24) as usize % alphabet.len()] as char
            })
            .collect()
    };
    let (a, b) = (image(total / 2), image(total / 2));
    format!("{{\"text\":\"look\",\"images\":[{{\"mime\":\"image/png\",\"data\":\"{a}\"}},{{\"mime\":\"image/png\",\"data\":\"{b}\"}}]}}").into_bytes()
}

/// A chunked POST of `len` bytes with the body going out on another thread, like
/// [`send_oversize`] but with no `Content-Length` for anything to refuse up front: only the
/// cap on the stream can stop it. A write error is the refusal, not a failure.
fn send_chunked(addr: &str, path: &str, auth: &str, len: usize) -> Answer {
    let head = format!("POST {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n");
    let mut sock = TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    sock.write_all(head.as_bytes()).unwrap();
    let mut writer = sock.try_clone().unwrap();
    writer.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let body = std::thread::spawn(move || {
        let chunk = vec![b'x'; 64 * 1024];
        let mut written = 0;
        while written < len {
            let take = chunk.len().min(len - written);
            let framed = [format!("{take:x}\r\n").as_bytes(), &chunk[..take], b"\r\n"].concat();
            if writer.write_all(&framed).is_err() {
                return;
            }
            written += take;
        }
        let _ = writer.write_all(b"0\r\n\r\n");
    });
    let mut raw = Vec::new();
    let _ = sock.read_to_end(&mut raw);
    let _ = body.join();
    Answer::parse(&raw)
}

/// 1. The rewrite table (hub.md §5). A page request with every header a browser would add is
///    turned into a door request with five facts and nothing else.
///
/// Mutations: drop the `Authorization` rewrite (the stand-in 401s, the perch answers 502);
/// forward `Cookie` (the `cookie=` field is not `-`); drop the `Host` rewrite (the stand-in
/// 403s a POST whose `Host` is not its own).
#[cfg(feature = "test-bins")]
#[test]
fn a_proxied_request_is_rebuilt_and_carries_the_doors_bearer_and_nothing_of_the_pages() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, port) = live(&perch);
    let door_token = perch.door_token(port);
    let before = perch.fake_records().len();

    let answer = perch
        .apireq("POST", &format!("/s/{id}/api/say"))
        .header("Cookie", "sid=abc")
        .header("Origin", "https://evil.example")
        .header("Referer", "https://evil.example/x")
        .header("Sec-Fetch-Site", "same-origin")
        .header("Last-Event-ID", "42")
        .header("Accept", "application/json")
        .body(b"{\"text\":\"hi\"}".to_vec())
        .send(&perch.addr);
    assert_eq!(answer.status, 202, "{}{}", answer.head, answer.body);
    assert_eq!(answer.body, "{\"queued\":false}", "the door's body, unchanged");

    let records = since(&perch, before);
    let at = records.iter().position(|line| line == &format!("POST /api/say auth=Bearer {door_token}")).unwrap_or_else(|| panic!("the door's own Bearer: {records:?}"));
    assert_eq!(
        records[at + 1],
        format!("hdr host=127.0.0.1:{port} content-type=application/json accept=application/json cookie=- origin=- referer=- sec-fetch-site=- last-event-id=-"),
        "five facts, and nothing of the page's: {records:?}"
    );
    let perch_token = perch.token.trim();
    assert!(records.iter().all(|line| !line.contains(perch_token)), "the perch's token reached the door: {records:?}");
    assert!(perch.fake_records().iter().all(|line| !line.contains(perch_token)), "…or its log");

    // The response: the door's status and content type, `no-store`, and nothing that sets state.
    assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{}", answer.head);
    assert_eq!(answer.header("cache-control").as_deref(), Some("no-store"), "{}", answer.head);
    assert_eq!(answer.header("x-content-type-options").as_deref(), Some("nosniff"));
    assert_eq!(answer.header("referrer-policy").as_deref(), Some("no-referrer"));
    assert!(answer.header("set-cookie").is_none(), "nothing that sets state: {}", answer.head);
    assert!(!answer.head.contains(&door_token) && !answer.body.contains(&door_token), "a door token reached the page");
}

/// 2. The door's contract at 0194f2b passes through with no route-specific code: notes on an
///    answer, `launch`, a multi-MiB `say` byte for byte, a refused `documents`, a route the
///    perch has never heard of, and every malformed path shape.
///
/// Mutations: an allowlist of route names turns (e) red; touching the body turns (c) red;
/// parsing or rewriting a frame turns (a) red.
#[cfg(feature = "test-bins")]
#[test]
fn the_doors_current_contract_passes_through_with_no_route_specific_code() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);

    // (a) An ask note round-trips: opaque to the perch, and byte for byte on the pane.
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)), "{:?}", pane.data());
    let note = "because \"quotes\", a \\ backslash, a newline\n and é ☃";
    let answered = perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": 7, "answer": "yes", "note": note }));
    assert_eq!(answered.status, 204, "{}{}", answered.head, answered.body);
    assert_eq!(answered.body, "");
    assert!(pane.read_until("ask-settled", Duration::from_secs(5)), "{:?}", pane.data());
    let settled = pane.frames().into_iter().find(|f| f["type"] == "ask-settled").expect("the ask-settled frame");
    assert_eq!((settled["ask_id"].as_u64(), settled["answer"].as_str(), settled["how"].as_str()), (Some(7), Some("yes"), Some("answered")));
    assert_eq!(settled["note"].as_str(), Some(note), "the note, byte for byte");
    // …and without a note the frame has none.
    let bare = perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": 8, "answer": "no" }));
    assert_eq!(bare.status, 204);
    assert!(pane.read_until("\"ask_id\":8", Duration::from_secs(5)));
    assert!(pane.frames().iter().find(|f| f["ask_id"] == 8).is_some_and(|f| f.get("note").is_none()));
    // A body the door refuses is the door's 400, passed through.
    assert_eq!(perch.apireq("POST", &format!("/s/{id}/api/answer")).body(b"not json".to_vec()).send(&perch.addr).status, 400);

    // (b) launch: 200, JSON, and the door's own body.
    let launched = perch.post_json(&format!("/s/{id}/api/launch"), serde_json::json!({ "window": "editor" }));
    assert_eq!(launched.status, 200, "{}{}", launched.head, launched.body);
    assert_eq!(launched.header("content-type").as_deref(), Some("application/json"), "{}", launched.head);
    let opened: serde_json::Value = serde_json::from_str(&launched.body).unwrap();
    assert_eq!(opened["opened"], "fake editor");

    // (c) A `say` with images totalling ~6 MiB of base64: the door got every byte.
    let body = images_body(6 * 1024 * 1024);
    assert!(body.len() > 6 * 1024 * 1024, "a multi-MiB say: {}", body.len());
    let said = perch.apireq("POST", &format!("/s/{id}/api/say")).body(body.clone()).send(&perch.addr);
    assert_eq!(said.status, 202, "{}{}", said.head, said.body);
    let received = std::fs::read(format!("{}.body", perch.fake_log.as_ref().unwrap().display())).expect("the door's copy of the body");
    assert_eq!(received.len(), body.len(), "the door got the whole body");
    assert!(received == body, "…and it is byte-identical");

    // (d) `documents` is refused by name, and the door's reason is the page's.
    let refused = perch.post_json(&format!("/s/{id}/api/say"), serde_json::json!({ "text": "x", "documents": [{ "name": "a.pdf" }] }));
    assert_eq!(refused.status, 422, "{}{}", refused.head, refused.body);
    assert!(refused.body.contains("this door takes images only"), "{}", refused.body);
    assert_eq!(perch.post_json(&format!("/s/{id}/api/cancel"), serde_json::json!({})).status, 409);

    // (e) A route the perch has never heard of reaches the door, and the door's 404 comes back.
    let before = perch.fake_records().len();
    for method in ["GET", "POST"] {
        let unknown = perch.apireq(method, &format!("/s/{id}/api/commands")).send(&perch.addr);
        assert_eq!(unknown.status, 404, "{method}: {}", unknown.head);
        assert_eq!(unknown.body, "no such route", "{method}: the door's 404, not the perch's empty one");
    }
    assert!(since(&perch, before).iter().any(|line| line.starts_with("GET /api/commands ")), "{:?}", since(&perch, before));
    assert!(since(&perch, before).iter().any(|line| line.starts_with("POST /api/commands ")));
    let nested = perch.apireq("GET", &format!("/s/{id}/api/commands/list-all")).send(&perch.addr);
    assert_eq!((nested.status, nested.body.as_str()), (404, "no such route"), "a nested route is a shape too");

    // (f) Malformed shapes never leave the perch: an empty 404, and the door sees nothing.
    let before = perch.fake_records().len();
    for path in [
        format!("/s/{id}/x/events"),
        format!("/s/{id}/api/"),
        format!("/s/{id}/api"),
        format!("/s/{id}/api/Say"),
        format!("/s/{id}/api/a//b"),
        format!("/s/{id}/api/a/../b"),
        format!("/s/{id}/api/%65vents"),
        format!("/s/{id}"),
        format!("/s/{id}/"),
        "/s".to_string(),
        "/s/".to_string(),
    ] {
        for method in ["GET", "POST"] {
            let answer = perch.apireq(method, &path).send(&perch.addr);
            assert_eq!(answer.status, 404, "{method} {path}: {}", answer.head);
            assert_eq!(answer.body, "", "{method} {path} is the perch's own empty 404");
        }
    }
    assert_eq!(since(&perch, before), Vec::<String>::new(), "no malformed path reached the door");
}

/// 3. The body cap, where it is and is not. Never stricter than the door (exactly 8 MiB gets
///    through), never looser (one byte over, declared or streamed, is refused), and a
///    declared length over the cap makes no connection at all.
///
/// Mutations: remove `Limited` from the stream path (the chunked case reaches the stand-in's
/// own cap, which records `body-refused`); a cap of `MAX_BODY - 1` (the exact case goes red);
/// check the declared length after connecting (the first case sees a request).
#[cfg(feature = "test-bins")]
#[test]
fn the_body_cap_is_the_doors_cap_and_holds_on_a_stream() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    let path = format!("/s/{id}/api/say");
    const CAP: usize = 8 * 1024 * 1024;

    // A declared length one byte over: 413, empty, and the door was never asked.
    let before = perch.fake_records().len();
    let over = send_oversize(&perch.addr, "POST", &path, Some(&perch.auth()), CAP + 1);
    assert_eq!(over.status, 413, "{}", over.head);
    assert_eq!(over.body, "");
    assert_eq!(since(&perch, before), Vec::<String>::new(), "no upstream request for a declared body over the cap");

    // Exactly the cap: the door gets all of it.
    let exact = perch.apireq("POST", &path).body(vec![b'x'; CAP]).send(&perch.addr);
    assert_eq!(exact.status, 202, "{}{}", exact.head, exact.body);
    assert!(since(&perch, before).iter().any(|line| line == "body-len=8388608"), "{:?}", since(&perch, before));

    // A chunked body over the cap has no length to refuse up front: only the cap on the
    // stream stops it, and it stops it before the door's own cap would (`body-refused`).
    let before = perch.fake_records().len();
    let chunked = send_chunked(&perch.addr, &path, &perch.auth(), CAP + 64 * 1024);
    assert_eq!(chunked.status, 413, "{}", chunked.head);
    let records = since(&perch, before);
    assert!(records.iter().any(|line| line.starts_with("POST /api/say ")), "the body was streamed to the door until the cap: {records:?}");
    assert!(!records.iter().any(|line| line.starts_with("body-len=")), "a body over the cap was not completed at the door: {records:?}");
    assert!(!records.iter().any(|line| line == "body-refused"), "the door never saw a body over its own cap: {records:?}");
    // The perch is fine afterwards.
    assert_eq!(perch.post_json(&path, serde_json::json!({ "text": "again" })).status, 202);
}

/// 4. Streaming, not collecting. The door sees the request head while the page is still
///    sending the body.
///
/// Mutation: collect the body before connecting (the head never reaches the stand-in until the
/// whole body has arrived, so the bounded poll below fails).
#[cfg(feature = "test-bins")]
#[test]
fn a_body_is_streamed_to_the_door_while_it_is_still_arriving() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    let before = perch.fake_records().len();
    const WHOLE: usize = 1024 * 1024;

    let head = format!(
        "POST /s/{id}/api/say HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAuthorization: {}\r\nContent-Type: application/json\r\nContent-Length: {WHOLE}\r\n\r\n",
        perch.addr,
        perch.auth()
    );
    let mut sock = TcpStream::connect(&perch.addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    sock.write_all(head.as_bytes()).unwrap();
    sock.write_all(&vec![b'x'; WHOLE / 2]).unwrap();

    // Half the body is out and the other half is held back: the door must already have the head.
    let arrived = wait_record(&perch, Duration::from_secs(5), |records| records.iter().skip(before).any(|line| line.starts_with("POST /api/say ")));
    assert!(arrived, "the door never saw the request while the body was still arriving: {:?}", since(&perch, before));
    assert!(!since(&perch, before).iter().any(|line| line.starts_with("body-len=")), "the body was not complete yet");

    sock.write_all(&vec![b'y'; WHOLE / 2]).unwrap();
    let mut raw = Vec::new();
    let _ = sock.read_to_end(&mut raw);
    let answer = Answer::parse(&raw);
    assert_eq!(answer.status, 202, "{}{}", answer.head, answer.body);
    assert!(since(&perch, before).iter().any(|line| line == &format!("body-len={WHOLE}")), "{:?}", since(&perch, before));
}

/// 5. A door's 401 is the perch's 502: the page never concludes its own token is bad.
///
/// Mutation: pass the status through (the page sees 401).
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_refuses_the_perch_is_502_and_never_401() {
    let perch = Perch::start_hub(&[("FAKE_REFUSE", "/api/say")]);
    let (id, _pid, _port) = live(&perch);
    let refused = perch.post_json(&format!("/s/{id}/api/say"), serde_json::json!({ "text": "x" }));
    assert_eq!(refused.status, 502, "{}{}", refused.head, refused.body);
    assert_eq!(refused.body, "{\"error\":\"door refused the perch\"}");
    assert_eq!(refused.header("content-type").as_deref(), Some("application/json"));
    // Only that path: the rest of the door answers the same Bearer.
    assert_eq!(perch.post_json(&format!("/s/{id}/api/cancel"), serde_json::json!({})).status, 409);
}

/// 6. SSE passes through unbuffered: `hello` arrives while the stream is still open, and a
///    frame the door broadcasts afterwards arrives on the same pane.
///
/// Mutation: buffer the body (collect it, or hold frames until the stream ends): `hello` never
/// arrives while the stream is open.
#[cfg(feature = "test-bins")]
#[test]
fn a_pane_gets_frames_as_they_happen_and_the_stream_stays_open() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    let mut pane = Pane::open(&perch, &id);
    assert_eq!(pane.head.lines().find(|l| l.to_ascii_lowercase().starts_with("content-type:")).map(str::to_ascii_lowercase).as_deref(), Some("content-type: text/event-stream"));
    assert!(pane.head.to_ascii_lowercase().contains("cache-control: no-store"), "{}", pane.head);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)), "hello did not arrive while the stream was open: {:?}", pane.data());
    assert!(!pane.eof && !pane.data().1, "…and the stream is still open");
    // A frame the door produces now reaches the same pane, live.
    assert_eq!(perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": 1, "answer": "go" })).status, 204);
    assert!(pane.read_until("ask-settled", Duration::from_secs(5)), "{:?}", pane.data());
    assert!(!pane.eof, "still open after a live frame");
}

/// 7. Mirrors: several panes on one door, each its own stream, each counted; closing a tab
///    brings the count down.
///
/// Mutation: never decrement the count (the drop-one case stays at 2).
#[cfg(feature = "test-bins")]
#[test]
fn several_panes_on_one_session_are_counted_and_a_closed_tab_is_not() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    assert_eq!(perch.row(&id).unwrap()["panes"], 0);
    let mut one = Pane::open(&perch, &id);
    let mut two = Pane::open(&perch, &id);
    assert!(one.read_until("\"hello\"", Duration::from_secs(5)) && two.read_until("\"hello\"", Duration::from_secs(5)), "both panes get a hello of their own");
    assert_eq!(perch.row(&id).unwrap()["panes"], 2, "{:?}", perch.row(&id));

    // One answer reaches both mirrors.
    assert_eq!(perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": 3, "answer": "ok" })).status, 204);
    assert!(one.read_until("ask-settled", Duration::from_secs(5)) && two.read_until("ask-settled", Duration::from_secs(5)));

    let panes_within = |want: u64, within: Duration| {
        let deadline = Instant::now() + within;
        loop {
            if perch.row(&id).unwrap()["panes"] == want {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    drop(one);
    assert!(panes_within(1, Duration::from_secs(2)), "a closed tab is one pane fewer: {:?}", perch.row(&id));
    drop(two);
    assert!(panes_within(0, Duration::from_secs(2)), "{:?}", perch.row(&id));
}

/// 8. The Stop finding for panes: a proxied `/api/events` is an open stream exactly like the
///    watcher, so a stop closes it BEFORE it signals the door.
///
/// Two panes: one read normally, and one the perch is still setting up when the stop lands (the
/// stand-in holds its head back 400 ms with the stream already counted open). The second is what
/// makes "before" provable: the stop must WAIT for it, so the stand-in sees no open stream the
/// moment the signal lands (`sigterm-received streams=0`).
///
/// Mutation: do not cancel the panes in `Registry::stop`: the stand-in (which, like a real door,
/// holds SIGTERM while a stream is open) logs `sigterm streams=1`, the stop waits out the 10 s
/// grace, and the time and the log assertions below both go red.
/// Mutation: remove `panes.drained(PANE_DRAIN)` from `Registry::stop`: the signal lands while the
/// late pane's stream is open, `sigterm-received streams=1`, and the last assertion goes red.
#[cfg(feature = "test-bins")]
#[test]
fn stopping_a_session_closes_its_panes_before_it_signals_the_door() {
    let perch = Perch::start_hub(&[("FAKE_LATE_PANE_MS", "400")]);
    let (id, pid, port) = live(&perch);
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));
    let late = Pane::begin(&perch, &id);
    assert!(wait_record(&perch, Duration::from_secs(5), |lines| lines.iter().any(|l| l == "late-stream-held")), "{:?}", perch.fake_records());
    assert_eq!(perch.row(&id).unwrap()["panes"], 2);

    let started = Instant::now();
    let stopped = perch.post(&format!("/hub/sessions/{id}/stop"));
    let took = started.elapsed();
    assert_eq!(stopped.status, 202, "{}", stopped.head);
    assert!(took < Duration::from_secs(5), "a stop with a pane open does not wait the grace out: {took:?}");

    assert!(pane.read_to_end(Duration::from_secs(5)), "the pane was ended: {:?}", pane.data());
    let end = pane.end_frame();
    assert_eq!((end["session"].as_str(), end["door"].as_str()), (Some(id.as_str()), Some("stopping")), "{end}");
    let (text, finished) = pane.data();
    assert!(finished && text.ends_with("}\n\n"), "the page's stream ended cleanly after the frame: {text:?}");
    // The pane that was still being set up is ended the same way, once.
    let mut late = Pane::adopt(late);
    assert!(late.read_to_end(Duration::from_secs(5)), "{:?}", late.data());
    assert_eq!(late.end_frame()["door"], "stopping");

    assert!(wait_gone(pid, Duration::from_secs(5)));
    let records = perch.fake_records();
    assert!(records.iter().any(|line| line == "sigterm-received streams=0"), "every stream was closed BEFORE the signal landed: {records:?}");
    assert!(records.iter().any(|line| line == "sigterm streams=0"), "{records:?}");
    assert!(perch.door_token_files().is_empty(), "no token file left: {:?}", perch.door_token_files());
    assert!(!perch.door_token_file(port).exists());
    assert_eq!(perch.state_of(&id).as_deref(), Some("kept"), "a requested clean stop leaves the log kept (hub.md section 4)");
}

/// 9. A door that dies mid-frame: the page gets the half frame, then the perch's own frame (its
///    leading blank line terminates the half one), then EOF; and the dead door's port is never
///    connected to again.
///
/// Mutation: look the port up without the state check (a connect to the dead port arrives at
/// the listener this test binds there).
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_dies_mid_frame_ends_the_pane_as_dead_and_is_never_connected_to_again() {
    let perch = Perch::start_hub(&[("FAKE_EXIT_AFTER_MS", "3000")]);
    let id = perch.keep_a_log("crashy");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    let (pid, port) = perch.fake_last_start();
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));

    assert!(pane.read_to_end(Duration::from_secs(15)), "the door's death ended the pane: {:?}", pane.data());
    let (text, finished) = pane.data();
    assert!(finished, "EOF after the frame");
    // The key order of the perch's JSON is not part of the contract; the blank lines around it are.
    assert!(text.contains("data: {\"type\":\"turn-st\n\ndata: {"), "the half frame, then a blank line, then the perch's frame: {text:?}");
    assert!(text.contains("\"type\":\"hub-stream-end\""), "{text:?}");
    assert!(text.ends_with("}\n\n"), "{text:?}");
    let end = pane.end_frame();
    assert_eq!((end["door"].as_str(), end["exit"].as_str(), end["session"].as_str()), (Some("dead"), Some("code:1"), Some(id.as_str())), "{end}");
    assert!(pane.frames().iter().all(|f| f["type"] != "turn-st"), "the half frame did not parse, and a page drops it");

    assert!(perch.wait_state(&id, "dead", Duration::from_secs(10)), "{:?}", perch.state_of(&id));
    assert!(wait_gone(pid, Duration::from_secs(5)));
    // The door's port is free now. Hold it, and prove the perch never connects to it.
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            listener.set_nonblocking(true).unwrap();
            let again = perch.apireq("POST", &format!("/s/{id}/api/say")).body(b"{}".to_vec()).send(&perch.addr);
            assert_eq!(again.status, 503, "{}{}", again.head, again.body);
            assert_eq!(again.body, "{\"state\":\"dead\"}");
            let get = perch.apireq("GET", &format!("/s/{id}/api/events")).send(&perch.addr);
            assert_eq!((get.status, get.body.as_str()), (503, "{\"state\":\"dead\"}"));
            match listener.accept() {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                other => panic!("the perch connected to a dead door's port: {:?}", other.map(|(_, peer)| peer)),
            }
        }
        Err(e) => {
            // Some other process took the port in the meantime; the 503 is still checked.
            eprintln!("could not bind the dead door's port {port}: {e}; checking the 503 only");
            let again = perch.apireq("POST", &format!("/s/{id}/api/say")).body(b"{}".to_vec()).send(&perch.addr);
            assert_eq!(again.status, 503, "{}", again.head);
        }
    }
}

/// 10. The perch's own shutdown with a pane open: every pane is closed, and ended with
///     `stopping`, before any door is signalled, so the quit is fast and leaves no token file.
///
/// Mutation: as test 8, through `stop_all` (both of its mutations).
#[cfg(feature = "test-bins")]
#[test]
fn a_perch_shutdown_with_a_pane_open_ends_the_pane_and_leaves_no_token_file() {
    let mut perch = Perch::start_hub(&[("FAKE_LATE_PANE_MS", "400")]);
    let (id, pid, port) = live(&perch);
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));
    let late = Pane::begin(&perch, &id);
    assert!(wait_record(&perch, Duration::from_secs(5), |lines| lines.iter().any(|l| l == "late-stream-held")), "{:?}", perch.fake_records());

    let started = Instant::now();
    let status = perch.stop();
    assert!(status.success(), "{status}");
    assert!(started.elapsed() < Duration::from_secs(5), "a quit with a pane open does not wait the grace out: {:?}", started.elapsed());
    assert!(pane.read_to_end(Duration::from_secs(5)), "{:?}", pane.data());
    let end = pane.end_frame();
    assert_eq!((end["door"].as_str(), end["session"].as_str()), (Some("stopping"), Some(id.as_str())), "{end}");
    let mut late = Pane::adopt(late);
    assert!(late.read_to_end(Duration::from_secs(5)), "{:?}", late.data());
    assert_eq!(late.end_frame()["door"], "stopping");
    assert!(wait_gone(pid, Duration::from_secs(5)));
    assert!(!perch.door_token_file(port).exists(), "the door's token file");
    assert!(!perch.token_file.exists(), "the perch's own token file");
    let records = perch.fake_records();
    assert!(records.iter().any(|line| line == "sigterm-received streams=0"), "every stream was closed BEFORE the signal landed: {records:?}");
    assert!(records.iter().any(|line| line == "sigterm streams=0"), "{records:?}");
}

/// A page that has stopped reading: it said `GET events`, read `hello`, and then nothing, while
/// the door broadcasts many MiB of frames, so the perch's write to it backs up and hyper stops
/// polling the response body. Returns the pane (kept open by the caller: the socket is the page).
#[cfg(feature = "test-bins")]
fn a_pane_that_stopped_reading(perch: &Perch, id: &str) -> Pane {
    let mut pane = Pane::open(perch, id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));
    // 1000 `ask-settled` frames of 32 KiB each, ~32 MB: far past what the sockets between
    // the door and the page can hold. Many small frames, not a few huge ones: hyper only stops
    // polling the body once it has a queue of buffers it cannot write, and a handful of 4 MiB
    // frames did not get it there.
    let note = "n".repeat(32 * 1024);
    for n in 0..1000 {
        let sent = perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": n, "answer": "go", "note": note }));
        assert_eq!(sent.status, 204, "{}", sent.head);
    }
    // Time for the backlog to build and the page's socket to fill. The page does not read.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(perch.row(id).unwrap()["panes"], 1);
    pane
}

/// H1c review finding 1: a pane that has stopped reading must still be cancellable. hyper does not
/// poll a response body while its write buffer is full, so the stop's cancellation cannot rely on
/// the body being polled: the pane's own task closes the door's stream, and the stop takes no
/// longer than its pane drain (2 s), where it took the door's 10 s grace and a SIGKILL before.
///
/// Mutation: give the pane no task of its own (cancel only from `Proxied::poll_frame`): the stop
/// takes ~12 s, the stand-in logs `sigterm streams=1` and the time assertion goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_stop_does_not_wait_for_a_pane_that_has_stopped_reading() {
    let perch = Perch::start_hub(&[]);
    let (id, pid, port) = live(&perch);
    let pane = a_pane_that_stopped_reading(&perch, &id);

    let started = Instant::now();
    let stopped = perch.post(&format!("/hub/sessions/{id}/stop"));
    let took = started.elapsed();
    assert_eq!(stopped.status, 202, "{}", stopped.head);
    assert!(took < Duration::from_secs(2), "a stop is not held up by a page that is not reading: {took:?}");
    assert!(wait_gone(pid, Duration::from_secs(5)));
    let records = perch.fake_records();
    assert!(records.iter().any(|line| line == "sigterm-received streams=0"), "the pane's stream was closed before SIGTERM: {records:?}");
    assert!(!records.iter().any(|line| line == "sigterm-ignored"), "{records:?}");
    assert!(perch.door_token_files().is_empty(), "{:?}", perch.door_token_files());
    assert!(!perch.door_token_file(port).exists());
    assert_eq!(perch.state_of(&id).as_deref(), Some("kept"), "a requested clean stop leaves the log kept (hub.md section 4)");
    assert!(!perch.err().contains("a pane did not close in time"), "{}", perch.err());
    drop(pane);
}

/// H1c review finding 1, the other half: the perch's own shutdown with a page that is not reading
/// must not hang on hyper's graceful drain. It exits within `serve::RESPONSE_DRAIN` (3 s) of the
/// door being stopped, with the page's socket still open, and leaves no token file.
///
/// Mutation: an unbounded `graceful.shutdown().await` in `serve::run`: the perch is still running
/// when `Perch::stop`'s own 10 s deadline passes, and it panics. (Without the pane's own task the
/// door stop takes ~12 s first, which the time assertion also catches.)
#[cfg(feature = "test-bins")]
#[test]
fn a_perch_shutdown_is_not_held_up_by_a_pane_that_has_stopped_reading() {
    let mut perch = Perch::start_hub(&[]);
    let (id, pid, port) = live(&perch);
    let pane = a_pane_that_stopped_reading(&perch, &id);

    let started = Instant::now();
    let status = perch.stop();
    let took = started.elapsed();
    assert!(status.success(), "{status}");
    assert!(took < Duration::from_secs(6), "the quit is bounded though a page is not reading: {took:?}");
    assert!(wait_gone(pid, Duration::from_secs(5)));
    assert!(!perch.door_token_file(port).exists(), "the door's token file");
    assert!(!perch.token_file.exists(), "the perch's own token file");
    drop(pane);
}

/// 11. Session states that are not live: each refusal is its own answer, and none of them
///     opens a connection to a door.
#[cfg(feature = "test-bins")]
#[test]
fn a_session_that_is_not_live_is_refused_without_a_connection() {
    let perch = Perch::start_hub(&[]);
    let (alive, _pid, _port) = live(&perch);
    let kept = perch.keep_a_log("kept-one");
    let held = perch.keep_a_log("held-elsewhere");
    perch.roster_row("eidolon-9f2c", &perch.sessions.path().join("held-elsewhere.eid"), std::process::id());
    let before = perch.fake_records().len();

    for (method, path) in [("GET", "events"), ("POST", "say")] {
        let unknown = perch.apireq(method, &format!("/s/nobody/api/{path}")).send(&perch.addr);
        assert_eq!((unknown.status, unknown.body.as_str()), (404, ""), "{method} unknown: {}", unknown.head);
        // A kept log, and one a peer holds (never adopted: Q5): the page can offer a resume.
        for id in [&kept, &held] {
            let refused = perch.apireq(method, &format!("/s/{id}/api/{path}")).send(&perch.addr);
            assert_eq!(refused.status, 409, "{method} {id}: {}", refused.head);
            assert_eq!(refused.body, "{\"state\":\"kept\"}");
            assert_eq!(refused.header("content-type").as_deref(), Some("application/json"));
        }
        // Not ids at all: never a path, never a connection.
        for id in ["..", "-x", ".hidden", "a%2Fb"] {
            let bad = perch.apireq(method, &format!("/s/{id}/api/{path}")).send(&perch.addr);
            assert_eq!((bad.status, bad.body.as_str()), (404, ""), "{method} {id}: {}", bad.head);
        }
    }
    assert_eq!(since(&perch, before), Vec::<String>::new(), "no refusal opened a connection to a door");
    // The live one was untouched by any of it.
    assert_eq!(perch.apireq("POST", &format!("/s/{alive}/api/cancel")).body(b"{}".to_vec()).send(&perch.addr).status, 409);

    // A cleanly stopped session is `kept` again (hub.md section 4): 409, and no connection.
    assert_eq!(perch.post(&format!("/hub/sessions/{alive}/stop")).status, 202);
    let gone = perch.apireq("GET", &format!("/s/{alive}/api/events")).send(&perch.addr);
    assert_eq!((gone.status, gone.body.as_str()), (409, "{\"state\":\"kept\"}"), "{}", gone.head);
}

/// A clean stop lists the session `kept`, answers 409 `kept` on the proxy and connects to
/// nothing; a resume then works. (A SIGKILLed or crashed door stays `dead`: see
/// `a_door_that_ignores_sigterm_is_killed_and_the_perch_removes_its_token_file` and
/// `a_door_that_exits_on_its_own_leaves_a_dead_row`.)
///
/// Mutation: drop the clean-exit rule (`Exit::clean` always false) and the first assertion goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_clean_stop_lists_the_session_kept_and_connects_to_nothing() {
    let perch = Perch::start_hub(&[]);
    let (id, pid, port) = live(&perch);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(wait_gone(pid, Duration::from_secs(5)));
    assert_eq!(perch.state_of(&id).as_deref(), Some("kept"));
    let row = perch.row(&id).unwrap();
    assert!(row["mtime"].as_u64().is_some(), "the kept row carries the log's mtime: {row}");

    let before = perch.fake_records().len();
    let refused = perch.apireq("GET", &format!("/s/{id}/api/events")).send(&perch.addr);
    assert_eq!((refused.status, refused.body.as_str()), (409, "{\"state\":\"kept\"}"), "{}", refused.head);
    assert_eq!(since(&perch, before), Vec::<String>::new(), "no connection to a stopped door");
    // The port is free: if the perch connected to it, a listener here would see it.
    if let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port)) {
        listener.set_nonblocking(true).unwrap();
        let _ = perch.apireq("GET", &format!("/s/{id}/api/events")).send(&perch.addr);
        assert!(listener.accept().is_err(), "the perch connected to a door that exited cleanly");
    }

    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200, "a clean stop is resumable");
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// A crashed door's leftover `meta.json` (its pid is gone) does not make the log
/// `live-elsewhere`, and the resume is not a 409; a live pid still does.
///
/// Mutation: drop the `pid_alive` check in `Registry::roster` and this goes red at the first resume.
#[cfg(feature = "test-bins")]
#[test]
fn a_stale_roster_row_does_not_block_a_resume() {
    let perch = Perch::start_hub(&[]);
    let id = perch.keep_a_log("stale-roster");
    let log = perch.sessions.path().join("stale-roster.eid");
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = gone.id();
    gone.wait().unwrap();
    perch.roster_row("web-stale", &log, dead_pid);
    assert_eq!(perch.state_of(&id).as_deref(), Some("kept"), "a dead pid's row is not live-elsewhere");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(perch.runtime.path().join("eidolon/web-stale/meta.json").is_file(), "someone else's row is left alone");
}

/// 13. H1b review finding 1: a new session whose door names a log outside the sessions dir is
///     rejected, the door that ignores SIGTERM is killed, and its token file does not outlive
///     it. No row was ever written for it, so the reap task has nothing to find it by: the
///     signal path owns the file.
///
/// Mutation: drop `drop_token_file` from `serve::give_up` (the file stays, for good).
#[cfg(feature = "test-bins")]
#[test]
fn a_rejected_new_session_leaves_no_door_token_file_behind() {
    let elsewhere = tempfile::tempdir().unwrap();
    let perch = Perch::start_hub(&[("FAKE_IGNORE_SIGTERM", "1"), ("FAKE_SESSIONS_DIR", elsewhere.path().to_str().unwrap())]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() }));
    assert_eq!(answer.status, 503, "{}{}", answer.head, answer.body);
    assert!(answer.body.contains("not in the sessions dir"), "{}", answer.body);
    let (pid, port) = perch.fake_last_start();
    assert!(wait_gone(pid, Duration::from_secs(5)), "the door that ignored SIGTERM was killed");
    // The token file is the perch's to remove, and it is removed by the time the 503 is out.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !perch.door_token_files().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(perch.door_token_files().is_empty(), "an orphaned door token file: {:?}", perch.door_token_files());
    assert!(!perch.door_token_file(port).exists());
}

// ---------------------------------------------------------------------------
// H2: GET /hub/tree (hub.md section 7)
// ---------------------------------------------------------------------------

#[cfg(feature = "test-bins")]
impl Perch {
    fn tree(&self, id: &str, path: &str) -> Answer {
        self.get(&format!("/hub/tree?session={id}&path={path}"))
    }

    /// The names in a 200 listing, in the order sent.
    fn tree_names(answer: &Answer) -> Vec<String> {
        let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON listing: {e}: {:?}", answer.body));
        body["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect()
    }
}

/// The fixture under the session's cwd: what a project looks like, plus everything the
/// listing must hide or refuse.
#[cfg(feature = "test-bins")]
fn tree_fixture(work: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt as _;
    std::fs::create_dir(work.join("src")).unwrap();
    std::fs::write(work.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(work.join("src/.env"), "KEY=1\n").unwrap();
    std::fs::write(work.join("src/Secret.TXT"), "s\n").unwrap();
    std::fs::create_dir(work.join("out")).unwrap();
    std::fs::write(work.join("a.txt"), "12345").unwrap();
    std::fs::write(work.join(".env"), "TOKEN=1\n").unwrap();
    std::fs::write(work.join("id_rsa"), "k\n").unwrap();
    std::fs::write(work.join("secret.txt"), "s\n").unwrap();
    std::fs::write(work.join("x.pem"), "p\n").unwrap();
    std::fs::create_dir(work.join(".git")).unwrap();
    std::fs::write(work.join(".git/config"), "[core]\n").unwrap();
    std::os::unix::fs::symlink("..", work.join("up")).unwrap();
    std::os::unix::fs::symlink("/etc", work.join("etc")).unwrap();
    std::os::unix::fs::symlink(".git", work.join("history")).unwrap();
    std::os::unix::fs::symlink("src", work.join("inside")).unwrap();
    std::fs::write(work.join(std::ffi::OsStr::from_bytes(b"bad-\xff-name")), "x").unwrap();
}

/// The root listing: kinds and sizes, dirs first, denied names absent, links without a
/// target, and a non-UTF-8 name counted rather than sent.
///
/// Mutation: drop the `denied(&name)` filter in `tree::list` and the hidden-names line goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_lists_names_and_kinds_and_hides_what_the_deny_list_names() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    tree_fixture(&perch.work);

    let answer = perch.tree(&id, "");
    assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
    assert_eq!(answer.header("content-type").as_deref(), Some("application/json"));
    let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(body["path"], "");
    assert_eq!(body["skipped"], 1, "the non-UTF-8 name is counted: {body}");
    assert_eq!(body["truncated"], false);
    let rows: Vec<(String, String, Option<u64>)> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["name"].as_str().unwrap().to_string(), e["kind"].as_str().unwrap().to_string(), e["size"].as_u64()))
        .collect();
    let want: Vec<(String, String, Option<u64>)> = [
        ("out", "dir", None),
        ("src", "dir", None),
        ("a.txt", "file", Some(5)),
        ("etc", "link", None),
        ("history", "link", None),
        ("inside", "link", None),
        ("up", "link", None),
    ]
    .into_iter()
    .map(|(n, k, s)| (n.to_string(), k.to_string(), s))
    .collect();
    // `history` is a link named like nothing on the list, so it is listed (as a link); it is
    // asking to open it that is refused (see the escape test).
    assert_eq!(rows, want, "dirs first, then by name: {body}");
    for hidden in [".env", "id_rsa", "secret.txt", "x.pem", ".git"] {
        assert!(!rows.iter().any(|(name, _, _)| name == hidden), "{hidden} must not be listed: {body}");
    }
    // A link shows no target, and nothing in the answer is an absolute path or a file's text.
    for entry in body["entries"].as_array().unwrap() {
        assert!(entry.get("target").is_none() && entry.get("path").is_none(), "{entry}");
    }
    for leak in ["/etc", "KEY=1", "TOKEN=1", "fn main", &perch.work.display().to_string()] {
        assert!(!answer.body.contains(leak), "{leak} leaked: {}", answer.body);
    }

    // A subdirectory, and the deny list inside it (case-insensitive, by name).
    let sub = perch.tree(&id, "src");
    assert_eq!(sub.status, 200, "{}", sub.body);
    assert_eq!(Perch::tree_names(&sub), vec!["main.rs"], "{}", sub.body);
    assert!(sub.body.contains("\"path\":\"src\""), "{}", sub.body);
    // A link that stays inside the tree is followed when it is asked for.
    let inside = perch.tree(&id, "inside");
    assert_eq!((inside.status, Perch::tree_names(&inside)), (200, vec!["main.rs".to_string()]), "{}", inside.body);
}

/// Every way out of the cwd, and every way to a denied name, is the same empty 404.
///
/// Mutations: remove the `starts_with(root)` check in `tree::list` and `up`/`etc` go red;
/// remove the canonical-remainder deny check and `history` goes red; remove the per-segment
/// deny check and `.env`/`.git` go red.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_path_cannot_leave_the_cwd_or_name_a_denied_entry() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    tree_fixture(&perch.work);

    for path in [
        "up",
        "etc",
        "..",
        "%2e%2e",
        "%2E%2e",
        ".",
        "/etc",
        "a%00b",
        "a%5Cb",
        "src//x",
        "src/",
        "src/..",
        "src/../..",
        ".git",
        ".GIT",
        ".env",
        "history",
        "src/.env",
        "src/.ENV",
        "src/Secret.TXT",
        "id_rsa",
        "x.pem",
        "secret.txt",
        "a.txt",
        "nowhere",
        "src/main.rs",
        "up/ui",
        "up/secret.txt",
        "etc/passwd",
    ] {
        let answer = perch.tree(&id, path);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "path={path}: {}", answer.head);
    }
    // 4098 bytes is not a path.
    let long = "a/".repeat(2049);
    assert_eq!(perch.tree(&id, &long).status, 404);
    // …and a good one still works, so the 404s above are the rules and not a broken route.
    assert_eq!(perch.tree(&id, "src").status, 200);
}

/// 2 001 entries come back as 2 000 and `truncated`; exactly 2 000 do not.
///
/// Mutation: raise `MAX_ENTRIES` or drop the `truncate` and this goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_of_2001_entries_is_cut_at_2000() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    let many = perch.work.join("many");
    std::fs::create_dir(&many).unwrap();
    for n in 0..2000 {
        std::fs::write(many.join(format!("f{n:04}")), "").unwrap();
    }
    let exact: serde_json::Value = serde_json::from_str(&perch.tree(&id, "many").body).unwrap();
    assert_eq!(exact["entries"].as_array().unwrap().len(), 2000);
    assert_eq!(exact["truncated"], false);
    std::fs::write(many.join("f2000"), "").unwrap();
    let cut: serde_json::Value = serde_json::from_str(&perch.tree(&id, "many").body).unwrap();
    assert_eq!(cut["entries"].as_array().unwrap().len(), 2000);
    assert_eq!(cut["truncated"], true);
}

/// The session and query rules: only a live door of ours is listed, and the query is
/// exactly `session` and `path`.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_is_for_a_live_session_with_a_strict_query_behind_the_token() {
    let perch = Perch::start_hub(&[]);
    let (id, pid, _port) = live(&perch);
    let kept = perch.keep_a_log("kept-one");
    let held = perch.keep_a_log("held-elsewhere");
    perch.roster_row("eidolon-9f2c", &perch.sessions.path().join("held-elsewhere.eid"), std::process::id());

    let unknown = perch.tree("nobody", "");
    assert_eq!((unknown.status, unknown.body.as_str()), (404, ""), "{}", unknown.head);
    for bad in ["..", "-x", "a%2Fb"] {
        assert_eq!(perch.tree(bad, "").status, 404, "{bad}");
    }
    let refused = perch.tree(&kept, "");
    assert_eq!((refused.status, refused.body.as_str()), (409, "{\"error\":\"this chat's folder isn't known\",\"state\":\"kept\"}"), "a kept chat with no known folder: {}", refused.head);
    let elsewhere = perch.tree(&held, "");
    assert_eq!((elsewhere.status, elsewhere.body.as_str()), (409, "{\"state\":\"live-elsewhere\"}"), "{}", elsewhere.head);

    // No token, wrong token: 401 with nothing in the body, whatever the query.
    let none = Request::new(&perch.addr, "GET", &format!("/hub/tree?session={id}")).send(&perch.addr);
    assert_eq!((none.status, none.body.as_str()), (401, ""), "{}", none.head);
    let wrong = Request::new(&perch.addr, "GET", &format!("/hub/tree?session={id}")).bearer("Bearer nope").send(&perch.addr);
    assert_eq!(wrong.status, 401);

    // The query is exact.
    for query in [
        String::new(),
        "path=src".to_string(),
        format!("session={id}&other=1"),
        format!("session={id}&session={id}"),
        format!("session={id}&path=a&path=b"),
        format!("session={id}&path=%zz"),
        format!("session={id}&path=%ff"),
        format!("session={id}&"),
        format!("session={id}&path"),
        format!("token=x&session={id}"),
    ] {
        let answer = perch.get(&format!("/hub/tree?{query}"));
        assert_eq!(answer.status, 400, "{query}: {}", answer.head);
    }
    // `path` may be left out: that is the root. Percent-escaped keys decode once.
    assert_eq!(perch.get(&format!("/hub/tree?session={id}")).status, 200);
    assert_eq!(perch.get(&format!("/hub/tree?%73ession={id}&p%61th=")).status, 200);
    assert_eq!(Request::new(&perch.addr, "POST", &format!("/hub/tree?session={id}")).bearer(perch.auth()).send(&perch.addr).status, 404);

    // A stopped session is kept again: now a saved chat, listed read-only from the folder the perch remembers.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(wait_gone(pid, Duration::from_secs(5)));
    let stopped = perch.tree(&id, "");
    assert_eq!(stopped.status, 200, "{}{}", stopped.head, stopped.body);
}

// ---------------------------------------------------------------------------
// H2: GET /hub/tree, after the post-H1c review (logs/agents/perch-post-h1c.review.md)
// ---------------------------------------------------------------------------

/// A head with the `date` header taken out: the one header that changes between two answers.
#[cfg(feature = "test-bins")]
fn head_without_date(answer: &Answer) -> String {
    answer.head.lines().filter(|line| !line.to_ascii_lowercase().starts_with("date:")).collect::<Vec<_>>().join("\r\n")
}

/// Finding 1: on NTFS (`/mnt/c`) `GIT~1` is `.git`'s second spelling. The shape is refused as a
/// segment anywhere in `path`, with the empty 404 a missing entry gets, escaped or not.
///
/// Mutation: make `short_name` always false in `tree.rs` and every row here goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_path_with_an_ntfs_short_name_is_the_same_empty_404() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    tree_fixture(&perch.work);
    // These really exist, so it is the shape and not a missing entry that is refused.
    std::fs::create_dir_all(perch.work.join("GIT~1/objects")).unwrap();
    std::fs::create_dir_all(perch.work.join("a/SSH~1/x")).unwrap();
    std::os::unix::fs::symlink("GIT~1", perch.work.join("alias")).unwrap();
    for path in ["GIT~1", "git~1", "GIT~1/objects", "a/SSH~1/x", "a/SSH~1", "GIT%7E1", "src/AOIDE~2", "alias"] {
        let answer = perch.tree(&id, path);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "path={path}: {}", answer.head);
    }
    assert_eq!(perch.tree(&id, "a").status, 200, "a good sibling still lists");
}

/// Findings 1 and 3: a session whose cwd *is* a denied name or an alias, or sits under one,
/// lists nothing at all, root included.
///
/// Mutation: remove the cwd-segment check in `tree::list_bounded` and every row goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_tree_of_a_session_whose_cwd_is_a_denied_name_is_not_listable() {
    let perch = Perch::start_hub(&[]);
    for cwd in [".ssh", "GIT~1", ".git/sub", "keys/.gnupg"] {
        let dir = perch.work.join(cwd);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("known_hosts"), "x").unwrap();
        let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": dir.display().to_string() }));
        assert_eq!(answer.status, 201, "{cwd}: {}{}", answer.head, answer.body);
        let id = body_id(&answer);
        for path in ["", "known_hosts"] {
            let listed = perch.tree(&id, path);
            assert_eq!((listed.status, listed.body.as_str()), (404, ""), "cwd={cwd} path={path}: {}", listed.head);
        }
    }
    // The control: the same perch lists an ordinary cwd.
    let (id, _pid, _port) = live(&perch);
    assert_eq!(perch.tree(&id, "").status, 200);
}

/// Finding 2: a resumed session's cwd is whatever its log said. The stand-in door reports the
/// sessions dir, which is outside `--root`, so the tree refuses it where a new session's works.
///
/// Mutation: drop the `tree_cwd` call in `serve::tree` (list `root` as stored) and this goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_resumed_session_whose_cwd_is_outside_root_is_not_listable() {
    let perch = Perch::start_hub(&[]);
    let id = perch.keep_a_log("resumed-elsewhere");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));
    std::fs::write(perch.sessions.path().join("visible.txt"), "x").unwrap();
    let answer = perch.tree(&id, "");
    assert_eq!((answer.status, answer.body.as_str()), (404, ""), "{}", answer.head);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// A denied name, an alias and a missing one are one answer, headers included (the `date` apart).
///
/// Mutation: give the denied path a different status or an extra header in `serve::tree` and
/// this goes red.
#[cfg(feature = "test-bins")]
#[test]
fn denied_and_missing_tree_answers_are_byte_identical() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    tree_fixture(&perch.work);
    let missing = perch.tree(&id, "nope");
    assert_eq!(missing.status, 404);
    let want = (head_without_date(&missing), missing.body.clone());
    for path in [".env", "src/.env", ".git", "id_rsa", "GIT~1", "history", "up", "a.txt"] {
        let got = perch.tree(&id, path);
        assert_eq!((head_without_date(&got), got.body.clone()), want, "path={path}");
    }
}

/// Finding 4: at most `MAX_LISTINGS` listings run at once; the rest are a 503 with a short body,
/// not a queue. A burst of 48 over a 40 000-entry directory sees both, and when it is over the
/// slots are free again.
///
/// Mutation: drop the `tree_slots.take()` in `serve::tree` and no 503 is ever seen.
#[cfg(feature = "test-bins")]
#[test]
fn concurrent_tree_listings_are_capped_with_a_503() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    let many = perch.work.join("many");
    std::fs::create_dir(&many).unwrap();
    for n in 0..40_000 {
        std::fs::write(many.join(format!("f{n:05}")), "").unwrap();
    }
    let (addr, auth) = (perch.addr.clone(), perch.auth());
    let mut seen_503 = None;
    for _round in 0..5 {
        let threads: Vec<_> = (0..48)
            .map(|_| {
                let (addr, auth, id) = (addr.clone(), auth.clone(), id.clone());
                std::thread::spawn(move || Request::new(&addr, "GET", &format!("/hub/tree?session={id}&path=many")).bearer(auth).send(&addr))
            })
            .collect();
        let answers: Vec<Answer> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(answers.iter().all(|a| a.status == 200 || a.status == 503), "{:?}", answers.iter().map(|a| a.status).collect::<Vec<_>>());
        assert!(answers.iter().any(|a| a.status == 200), "some listings still run");
        if let Some(refused) = answers.into_iter().find(|a| a.status == 503) {
            seen_503 = Some(refused);
            break;
        }
    }
    let refused = seen_503.expect("a burst of 48 over 4 slots is refused at least once");
    assert_eq!(refused.header("content-type").as_deref(), Some("application/json"));
    assert!(refused.body.len() < 100 && refused.body.contains("listing"), "a short reason: {:?}", refused.body);
    let again = perch.tree(&id, "many");
    assert_eq!(again.status, 200, "the slots are free again: {}", again.head);
}

/// Finding 8: a door that exits 0 *on its own* while live is a stop as far as the list goes
/// (`kept`, `/s` answers 409 kept), and the pane is still told how it ended.
///
/// Mutation: make `Exit::clean` false for code 0 and this goes red (it is `dead`).
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_exits_zero_on_its_own_while_live_ends_kept() {
    let perch = Perch::start_hub(&[("FAKE_EXIT_AFTER_MS", "3000"), ("FAKE_EXIT_CODE", "0")]);
    let id = perch.keep_a_log("quits-politely");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    let (pid, port) = perch.fake_last_start();
    let mut pane = Pane::open(&perch, &id);
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)));
    assert!(pane.read_to_end(Duration::from_secs(15)), "{:?}", pane.data());
    let end = pane.end_frame();
    assert_eq!((end["door"].as_str(), end["exit"].as_str()), (Some("dead"), Some("code:0")), "{end}");
    assert!(perch.wait_state(&id, "kept", Duration::from_secs(10)), "{:?}", perch.state_of(&id));
    assert!(wait_gone(pid, Duration::from_secs(5)));
    assert!(!perch.door_token_file(port).exists());
    let gone = perch.apireq("GET", &format!("/s/{id}/api/events")).send(&perch.addr);
    assert_eq!((gone.status, gone.body.as_str()), (409, "{\"state\":\"kept\"}"), "{}", gone.head);
}

/// ...and exit 0 while still `starting` is a failed spawn, which stays `dead`.
///
/// Mutation: drop the `came_up &&` in `Registry::mark_exited` and this goes red (it is `kept`).
#[cfg(feature = "test-bins")]
#[test]
fn a_door_that_exits_zero_while_starting_ends_dead() {
    let perch = Perch::start_hub(&[("FAKE_EXIT_BEFORE_BOOT", "0")]);
    let id = perch.keep_a_log("never-came-up");
    let answer = perch.post(&format!("/hub/sessions/{id}/resume"));
    assert_eq!(answer.status, 503, "{}{}", answer.head, answer.body);
    assert_eq!(perch.state_of(&id).as_deref(), Some("dead"), "a spawn that failed is not a stop");
}

// ---------------------------------------------------------------------------
// The login code and the session cookie (hub.md section 10.5)
// ---------------------------------------------------------------------------

impl Perch {
    /// `POST /hub/code`, with the token, as the launcher does. The code, not its answer.
    fn mint_code(&self) -> String {
        let answer = self.post("/hub/code");
        assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
        let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap();
        body["code"].as_str().expect("a code").to_string()
    }

    /// `POST /hub/redeem` with no credential at all, as a page sends it.
    fn redeem(&self, code: &str) -> Answer {
        Request::new(&self.addr, "POST", "/hub/redeem").body(serde_json::to_vec(&serde_json::json!({ "code": code })).unwrap()).send(&self.addr)
    }

    /// `name=value` of the cookie a redeem set, as a browser would send it back.
    fn cookie_of(&self, answer: &Answer) -> String {
        let set = answer.header("set-cookie").unwrap_or_else(|| panic!("no Set-Cookie: {}", answer.head));
        set.split(';').next().unwrap().to_string()
    }

    /// A cookie request as the page sends it: the browser says `same-origin`.
    fn with_cookie(&self, method: &str, path: &str, cookie: &str) -> Answer {
        Request::new(&self.addr, method, path).header("Cookie", cookie).header("Sec-Fetch-Site", "same-origin").send(&self.addr)
    }

    /// A cookie request from a browser that sends no fetch metadata at all.
    fn with_cookie_bare(&self, method: &str, path: &str, cookie: &str) -> Answer {
        Request::new(&self.addr, method, path).header("Cookie", cookie).send(&self.addr)
    }
}

/// 50 ms is the lifetime; the test waits 300 ms.
fn short_lived_codes() -> Vec<std::ffi::OsString> {
    vec!["--code-ttl-ms".into(), "50".into()]
}

/// code -> redeem -> cookie, and the cookie is a credential everywhere the token is.
/// Mutations: drop the cookie arm of the gate (the cookie request is a 401); make the cookie the
/// token itself (the value assertion); drop `HttpOnly` or `SameSite=Strict` (the attribute checks).
#[test]
fn a_code_is_traded_for_a_cookie_that_works_like_the_token() {
    let perch = Perch::launch(true, None, &[]);
    assert_eq!(perch.get("/hub/sessions").status, 200, "the token still works");
    // Nothing about a code is open without the token.
    assert_eq!(Request::new(&perch.addr, "POST", "/hub/code").send(&perch.addr).status, 401);
    let code = perch.mint_code();
    assert!(code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !code.is_empty(), "the shape the launcher prints");

    let answer = perch.redeem(&code);
    assert_eq!((answer.status, answer.body.as_str()), (204, ""), "{}", answer.head);
    let set = answer.header("set-cookie").unwrap();
    assert!(set.starts_with(&format!("minerva_{}=", perch.port)), "the port is in the name: {set}");
    assert!(set.contains("; HttpOnly") && set.contains("; SameSite=Strict") && set.contains("; Path=/"), "{set}");
    assert!(!set.contains("Secure") && !set.to_ascii_lowercase().contains("max-age"), "{set}");
    let cookie = perch.cookie_of(&answer);
    assert!(!cookie.contains(perch.token.trim()), "the cookie is its own secret, not the token");

    // No Authorization header anywhere below.
    let list = perch.with_cookie("GET", "/hub/sessions", &cookie);
    assert_eq!((list.status, list.body.as_str()), (200, "[]"), "{}", list.head);
    assert_eq!(perch.with_cookie("GET", "/hub/anything", &cookie).status, 404, "past the gate, not refused by it");
    // The cookie is not a way to mint more logins: that is the token's (the launcher's) alone.
    let minted = perch.with_cookie("POST", "/hub/code", &cookie);
    assert_eq!((minted.status, minted.body.as_str()), (403, ""), "{}", minted.head);
    assert_eq!(perch.post("/hub/code").status, 200, "the bearer token still mints");
    assert_eq!(perch.with_cookie("GET", "/s/x/api/events", &cookie).status, 404, "the proxy takes the cookie too");
    // Among other cookies.
    assert_eq!(perch.with_cookie("GET", "/hub/sessions", &format!("a=b; {cookie}; c=d")).status, 200);
    // Wrong value, another port's name, a prefix of the name, a prefix of the value: all 401.
    let (name, value) = cookie.split_once('=').unwrap();
    for bad in [
        format!("{name}=wrong"),
        format!("minerva_{}={value}", perch.port.wrapping_add(1)),
        format!("x{name}={value}"),
        format!("{name}={}", &value[..value.len() - 1]),
        format!("{name}="),
    ] {
        assert_eq!(perch.with_cookie("GET", "/hub/sessions", &bad).status, 401, "{bad}");
    }
    // The cookie is attached by the browser, so it counts only on a request that says `same-origin`:
    // no fetch metadata (an old browser on a sibling port of this host), `same-site` and `none` are all refused.
    for refused in [perch.with_cookie_bare("GET", "/hub/sessions", &cookie), perch.with_cookie_bare("POST", "/hub/sessions", &cookie)] {
        assert_eq!(refused.status, 401, "{}", refused.head);
    }
    for site in ["same-site", "none", "cross-site"] {
        let refused = Request::new(&perch.addr, "GET", "/hub/sessions").header("Cookie", &cookie).header("Sec-Fetch-Site", site).send(&perch.addr);
        assert_eq!(refused.status, 401, "cookie + {site}: {}", refused.head);
    }
    assert_eq!(perch.with_cookie("GET", "/hub/sessions", &cookie).status, 200, "same-origin passes");
    // The bearer keeps its rules: no `Sec-Fetch-Site` passes (curl), `same-site` is still the gate 3 403.
    assert_eq!(perch.get("/hub/sessions").status, 200);
    let sibling = perch.apireq("GET", "/hub/sessions").header("Sec-Fetch-Site", "same-site").send(&perch.addr);
    assert_eq!(sibling.status, 403, "{}", sibling.head);
    // Gated answers are never a document: no script, no frame, nothing loaded.
    assert_eq!(list.header("content-security-policy").as_deref(), Some("default-src 'none'; sandbox"), "{}", list.head);
    assert_eq!(perch.get("/hub/sessions").header("content-security-policy").as_deref(), Some("default-src 'none'; sandbox"));
    let rebound = Request::new(&perch.addr, "GET", "/hub/sessions").header("Cookie", &cookie).header("Sec-Fetch-Site", "same-origin").host("evil.example:80").send(&perch.addr);
    assert_eq!(rebound.status, 403, "{}", rebound.head);

    // Neither the code nor the secret is in the output.
    for out in [perch.out(), perch.err()] {
        assert!(!out.contains(&code) && !out.contains(value), "a login secret reached the output");
    }
}

/// Mutations: leave the code in place after a redeem (the second use is a 204).
#[test]
fn a_code_is_single_use_and_a_bad_one_is_refused() {
    let perch = Perch::launch(true, None, &[]);
    let code = perch.mint_code();
    assert_eq!(perch.redeem(&code).status, 204);
    let again = perch.redeem(&code);
    assert_eq!((again.status, again.body.as_str()), (403, ""), "{}", again.head);
    assert!(again.header("set-cookie").is_none());
    for bad in ["", "nope", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"] {
        assert_eq!(perch.redeem(bad).status, 403, "{bad:?}");
    }
    // A malformed body is the same refusal, not a different oracle.
    for body in [&b"not json"[..], b"{}", b"{\"code\":7}", b"[]"] {
        let answer = Request::new(&perch.addr, "POST", "/hub/redeem").body(body).send(&perch.addr);
        assert_eq!(answer.status, 403, "{:?}", String::from_utf8_lossy(body));
    }
    // Any other method on the route is an ordinary gated 401.
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/redeem").send(&perch.addr).status, 401);
    // A wrong try does not burn a good code.
    let code = perch.mint_code();
    assert_eq!(perch.redeem("nope").status, 403);
    assert_eq!(perch.redeem(&code).status, 204);
}

/// Mutations: ignore the age (the late redeem is a 204).
#[test]
fn an_expired_code_is_refused() {
    // Only the late direction is asserted here: a slow machine makes the code older, never younger, so
    // this cannot flake. "Inside its lifetime" is every other test in this section, at the real 30 s.
    let perch = Perch::launch(true, None, &short_lived_codes());
    let stale = perch.mint_code();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(perch.redeem(&stale).status, 403, "past its lifetime");
}

/// The open route keeps every gate but the token. Mutations: skip the host, fetch-metadata or
/// body-cap gate for it.
#[test]
fn redeem_is_open_only_to_the_token_gate() {
    let perch = Perch::launch(true, None, &[]);
    let code = perch.mint_code();
    let body = serde_json::to_vec(&serde_json::json!({ "code": code })).unwrap();
    let cross = Request::new(&perch.addr, "POST", "/hub/redeem").header("Sec-Fetch-Site", "cross-site").body(body.clone()).send(&perch.addr);
    assert_eq!(cross.status, 403, "{}", cross.head);
    let rebound = Request::new(&perch.addr, "POST", "/hub/redeem").host("evil.example:80").body(body.clone()).send(&perch.addr);
    assert_eq!(rebound.status, 403, "{}", rebound.head);
    let big = send_oversize(&perch.addr, "POST", "/hub/redeem", None, 4096);
    assert_eq!(big.status, 413, "{}", big.head);
    // None of those spent the code; the page's own `fetch` (`same-origin`) can.
    let typed = Request::new(&perch.addr, "POST", "/hub/redeem").header("Sec-Fetch-Site", "same-origin").body(body).send(&perch.addr);
    assert_eq!(typed.status, 204, "{}", typed.head);
    // No credential, no cookie: still the 401 the gates promise.
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/sessions").send(&perch.addr).status, 401);
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/sessions").header("Cookie", "").send(&perch.addr).status, 401);
}

/// The proxy's SSE route takes the cookie, and nothing of it reaches the door.
/// Mutations: leave the cookie arm out of the proxy path (the pane is refused).
#[cfg(feature = "test-bins")]
#[test]
fn the_cookie_opens_a_pane() {
    let mut perch = Perch::start_hub(&[]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string(), "model": "mock" }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let id = body_id(&answer);
    let cookie = perch.cookie_of(&perch.redeem(&perch.mint_code()));
    let mut pane = Pane::adopt(Pane::begin_with(&perch, &id, &format!("Cookie: {cookie}\r\nSec-Fetch-Site: same-origin")));
    assert!(pane.read_until("\"hello\"", Duration::from_secs(5)), "the stream opened on the cookie alone");
    drop(pane);
    let said = Request::new(&perch.addr, "POST", &format!("/s/{id}/api/say")).header("Cookie", &cookie).header("Sec-Fetch-Site", "same-origin").body(br#"{"text":"hi"}"#.to_vec()).send(&perch.addr);
    assert_eq!(said.status, 202, "{}{}", said.head, said.body);
    let cookie_value = cookie.split_once('=').unwrap().1.to_string();
    assert!(perch.fake_records().iter().all(|line| !line.contains(&cookie_value)), "the door was handed the cookie");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    perch.stop();
}

// ---------------------------------------------------------------------------
// Titles, saved chats you can read, and the second (read-only) source
// ---------------------------------------------------------------------------

/// eidolon's own JSON lines for a chat (`eidolon log --json`), as the stand-in prints them: a start, and, when `first` is given,
/// that message and a reply.
fn chat_log(first: Option<&str>) -> String {
    let mut lines = vec![serde_json::json!({ "id": 0, "parent": null, "ts_ms": 1, "kind": { "SessionStart": { "model": "mock", "cwd": "/tmp", "system": null } } })];
    if let Some(text) = first {
        let said = |role: &str, text: &str| serde_json::json!({ "role": role, "content": [{ "type": "text", "text": text }] });
        lines.push(serde_json::json!({ "id": 1, "parent": 0, "ts_ms": 2, "kind": { "UserMessage": said("user", text) } }));
        lines.push(serde_json::json!({ "id": 2, "parent": 1, "ts_ms": 3, "kind": { "AssistantMessage": said("assistant", "ok") } }));
    }
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// A perch with a Minerva home, the stand-in as eidolon, and the member's config given. Eidolon's own sessions dir (the second
/// source) is `<perch.data>/eidolon/sessions`, where the stand-in's `sessions` reads it too.
#[cfg(feature = "test-bins")]
fn start_home(member: &str) -> (tempfile::TempDir, PathBuf, Perch) {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("minerva.toml"), "default_model = \"mock\"\ndefault_mode = \"auto\"\n").unwrap();
    let home = home.canonicalize().unwrap();
    let perch = Perch::start_hub_member(&["--root".into(), base.path().canonicalize().unwrap().into(), "--home".into(), home.clone().into()], member);
    (base, home, perch)
}

#[cfg(feature = "test-bins")]
impl Perch {
    /// Eidolon's own sessions dir, as the stand-in and the perch both resolve it from `$XDG_DATA_HOME`.
    fn shared(&self) -> PathBuf {
        let dir = self.data.path().join("eidolon").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

/// A saved chat's title is what `eidolon sessions` says its first message is (flattened, never decoded by the perch); one with no
/// message yet is `<folder> · HH:MM`; and the question is asked once per change, not once per list.
/// Mutations: drop the cache (the helper count climbs on every list); ignore `sessions` (the title is the fallback).
#[cfg(feature = "test-bins")]
#[test]
fn a_saved_chat_is_titled_by_eidolons_own_listing_and_asked_once() {
    let (_base, home, perch) = start_home("");
    let sessions = home.join("sessions");
    std::fs::write(sessions.join("asked.eid"), chat_log(Some("fix the  login\nbug"))).unwrap();
    std::fs::write(sessions.join("blank.eid"), chat_log(None)).unwrap();
    let asked = perch.row("asked").unwrap();
    assert_eq!(asked["title"], "fix the login bug", "{asked}");
    let blank = perch.row("blank").unwrap()["title"].as_str().unwrap().to_string();
    let (name, at) = blank.split_once(" · ").unwrap_or_else(|| panic!("a fallback is `<name> · HH:MM`: {blank:?}"));
    assert_eq!(name, "chat", "a log from before this perch has no cwd the perch may read");
    assert!(at.len() == 5 && at.as_bytes()[2] == b':' && at.chars().filter(|c| c.is_ascii_digit()).count() == 4, "{blank:?}");
    for _ in 0..3 {
        perch.list();
    }
    let helpers = perch.fake_helpers();
    assert_eq!(helpers.len(), 1, "four listings, one `eidolon sessions`: {helpers:?}");
    assert!(helpers[0].starts_with("helper sessions ") && helpers[0].ends_with("config=true"), "{helpers:?}");
    // A new log is the one thing that makes it ask again, and it asks once for the whole directory.
    std::fs::write(sessions.join("later.eid"), chat_log(Some("second chat"))).unwrap();
    assert_eq!(perch.row("later").unwrap()["title"], "second chat");
    assert_eq!(perch.fake_helpers().len(), 2);
}

/// A live chat is titled by the roster's `meta.json` (eidolon's own), and wears `<folder> · HH:MM` until it has one; a failing
/// `eidolon sessions` loses no row and is not retried on every poll.
/// Mutations: read no roster title for a door of ours (the fallback stays); retry a failed listing each time.
#[cfg(feature = "test-bins")]
#[test]
fn a_live_chat_takes_the_roster_title_and_a_failing_listing_loses_no_row() {
    let perch = Perch::start_hub(&[("FAKE_HELPER_FAIL", "1")]);
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string(), "model": "mock" }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    let id = body_id(&answer);
    let title = perch.row(&id).unwrap()["title"].as_str().unwrap().to_string();
    assert!(title.starts_with("work · "), "the door's cwd names the folder: {title:?}");
    let (pid, _) = perch.fake_last_start();
    perch.roster_row("eidolon-live", &perch.sessions.path().join(format!("{id}.eid")), pid);
    assert_eq!(perch.row(&id).unwrap()["title"], "a peer", "the roster's title wins while the door is live");
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));
    perch.keep_a_log("old-one");
    for _ in 0..3 {
        perch.list();
    }
    assert!(perch.row("old-one").unwrap()["title"].as_str().unwrap().starts_with("chat · "), "a failed `sessions` is the fallback name");
    assert_eq!(perch.fake_helpers().len(), 1, "a failed listing is not asked again at every poll: {:?}", perch.fake_helpers());
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// `GET /hub/sessions/<id>/log` is eidolon's `log --json` for a saved chat: the bytes as printed, 404 for an id that is not one,
/// 413 over the size cap, and it starts nothing (opening a saved chat never resumes it). `[ continue ]` is the resume route,
/// with the home's config. Mutations: no cap; a resume that drops `--config`; a log route that starts a door.
#[cfg(feature = "test-bins")]
#[test]
fn a_saved_chat_is_readable_without_a_door_and_continues_by_resume() {
    let (_base, home, perch) = start_home("");
    let sessions = home.join("sessions");
    let text = chat_log(Some("what is a nonce"));
    std::fs::write(sessions.join("saved.eid"), &text).unwrap();
    let got = perch.get("/hub/sessions/saved/log");
    assert_eq!(got.status, 200, "{}", got.head);
    assert_eq!(got.header("content-type").as_deref(), Some("application/x-ndjson"), "{}", got.head);
    assert_eq!(got.body, text, "eidolon's lines, as printed");
    assert!(perch.fake_helpers().iter().any(|line| line.starts_with("helper log --json ") && line.ends_with("config=true")), "{:?}", perch.fake_helpers());
    assert!(perch.fake_records().iter().all(|line| !line.starts_with("start ")), "reading a chat starts no door: {:?}", perch.fake_records());
    assert_eq!(perch.state_of("saved").as_deref(), Some("kept"));
    // Not a chat: unknown, not an id, a query, and the other method.
    assert_eq!(perch.get("/hub/sessions/nope/log").status, 404);
    assert_eq!(perch.get("/hub/sessions/..%2Fsaved/log").status, 404);
    assert_eq!(perch.get("/hub/sessions/saved/log?after=3").status, 404, "no query: a route that is not there");
    assert_eq!(perch.post("/hub/sessions/saved/log").status, 404);
    // The size cap: a 17 MiB log is a 413 with a reason, not a truncated transcript.
    std::fs::write(sessions.join("huge.eid"), vec![b'x'; 17 * 1024 * 1024]).unwrap();
    let refused = perch.get("/hub/sessions/huge/log");
    assert_eq!(refused.status, 413, "{}", refused.head);
    assert!(refused.body.contains("too large"), "{}", refused.body);
    // Continue = the resume route, which is what spends: with the home's config, on that log.
    let resumed = perch.post("/hub/sessions/saved/resume");
    assert_eq!(resumed.status, 200, "{}{}", resumed.head, resumed.body);
    let argv = perch.fake_argv();
    assert!(argv.starts_with(&format!("argv --config {} web ", home.join("config.toml").display())), "{argv}");
    assert!(argv.contains(&format!("--session {}", sessions.join("saved.eid").display())), "{argv}");
    assert_eq!(perch.state_of("saved").as_deref(), Some("live"));
    assert_eq!(perch.post("/hub/sessions/saved/stop").status, 202);
}

/// Under `--home`, eidolon's own sessions dir is a second source: its chats are listed `origin: "other"`, titled the same way
/// (a `sessions` run with no `--config`), readable through the same route (also with no config), never written, and `continue`
/// resumes one with the home's config; after which it is just a live chat, and `other` again once it stops. An id both dirs have
/// is the home's. Without `--home` there is no such source. Mutations: no second source; origin missing; `--config` passed for the
/// other dir's `sessions`; the other source listed without `--home`.
#[cfg(feature = "test-bins")]
#[test]
fn eidolons_own_dir_is_a_second_read_only_source_flagged_other() {
    let (_base, home, perch) = start_home("");
    let shared = perch.shared();
    let old = chat_log(Some("an old question"));
    std::fs::write(shared.join("old.eid"), &old).unwrap();
    std::fs::write(shared.join("dup.eid"), chat_log(Some("theirs"))).unwrap();
    std::fs::write(home.join("sessions/dup.eid"), chat_log(Some("ours"))).unwrap();
    std::fs::write(home.join("sessions/new.eid"), chat_log(Some("mine"))).unwrap();
    let row = perch.row("old").expect("the other source is listed");
    assert_eq!((row["state"].as_str(), row["origin"].as_str(), row["title"].as_str()), (Some("kept"), Some("other"), Some("an old question")), "{row}");
    assert!(perch.row("new").unwrap().get("origin").is_none(), "the home's own chats are not flagged");
    let dup = perch.row("dup").unwrap();
    assert_eq!((dup["title"].as_str(), dup.get("origin")), (Some("ours"), None), "an id both dirs have is the home's: {dup}");
    assert_eq!(perch.list().iter().filter(|r| r["id"] == "dup").count(), 1);
    let helpers = perch.fake_helpers();
    assert!(helpers.iter().any(|h| h.ends_with("config=true")) && helpers.iter().any(|h| h.ends_with("config=false")), "one listing per source: {helpers:?}");
    // Readable, with eidolon's own config (none is passed).
    let got = perch.get("/hub/sessions/old/log");
    assert_eq!((got.status, got.body.as_str()), (200, old.as_str()), "{}", got.head);
    assert!(perch.fake_helpers().iter().any(|h| h.starts_with("helper log --json ") && h.contains("/eidolon/sessions/old.eid") && h.ends_with("config=false")));
    assert_eq!(perch.get("/s/old/api/events").status, 409, "a pane on it is told it is kept, like any saved chat");
    assert_eq!(std::fs::read_to_string(shared.join("old.eid")).unwrap(), old, "nothing in it is written");
    // Continue: the member's normal config, on that log, in place.
    let resumed = perch.post("/hub/sessions/old/resume");
    assert_eq!(resumed.status, 200, "{}{}", resumed.head, resumed.body);
    let argv = perch.fake_argv();
    assert!(argv.starts_with(&format!("argv --config {} web ", home.join("config.toml").display())), "{argv}");
    assert!(argv.contains(&format!("--session {}", shared.join("old.eid").display())), "{argv}");
    let live = perch.row("old").unwrap();
    assert_eq!((live["state"].as_str(), live.get("origin")), (Some("live"), None), "{live}");
    assert_eq!(perch.post("/hub/sessions/old/stop").status, 202);
    assert!(perch.wait_state("old", "kept", Duration::from_secs(5)));
    assert_eq!(perch.row("old").unwrap()["origin"], "other", "stopped, it is eidolon's own again");
    drop(perch);

    // Without `--home` there is no second source, whatever sits in the data dir.
    let plain = Perch::start_hub(&[]);
    std::fs::write(plain.shared().join("stray.eid"), chat_log(Some("not listed"))).unwrap();
    plain.keep_a_log("here");
    let ids: Vec<String> = plain.list().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids, ["here"], "{ids:?}");
    assert_eq!(plain.get("/hub/sessions/stray/log").status, 404);
}

// ---------------------------------------------------------------------------
// Projects and the folder picker (hub.md section 7a)
// ---------------------------------------------------------------------------

#[cfg(feature = "test-bins")]
fn json_of(answer: &Answer) -> serde_json::Value {
    serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON body: {e}: {:?} ({})", answer.body, answer.head))
}

/// A project is a named folder under the roots, kept in `<home>/projects.json` (0600): one entry per folder, the name
/// defaults to the folder's, removing one never touches the folder, and a folder outside the roots is a 422.
/// Mutations: drop the dedupe (a second 201), the roots check (the outside 422 becomes 201), or have DELETE remove the folder.
#[cfg(feature = "test-bins")]
#[test]
fn projects_are_named_folders_kept_once_and_forgotten_without_touching_files() {
    use std::os::unix::fs::PermissionsExt as _;
    let (base, home, perch) = start_home("");
    let root = base.path().canonicalize().unwrap();
    assert_eq!(json_of(&perch.get("/hub/projects")), serde_json::json!([]));
    let proj = root.join("my-app");
    std::fs::create_dir(&proj).unwrap();
    std::fs::write(proj.join("keep.txt"), "x").unwrap();

    let made = perch.post_json("/hub/projects", serde_json::json!({ "path": proj }));
    assert_eq!(made.status, 201, "{}{}", made.head, made.body);
    let entry = json_of(&made);
    assert_eq!((entry["name"].as_str(), entry["path"].as_str()), (Some("my-app"), proj.to_str()), "the name is the folder's");
    assert!(entry["created"].as_u64().unwrap() > 0);
    // The same folder again (trailing slash and a new name included) is the existing entry.
    let again = perch.post_json("/hub/projects", serde_json::json!({ "path": format!("{}/", proj.display()), "name": "Other" }));
    assert_eq!((again.status, json_of(&again)), (200, entry.clone()), "{}", again.body);
    let named = root.join("two");
    std::fs::create_dir(&named).unwrap();
    let two = perch.post_json("/hub/projects", serde_json::json!({ "path": named, "name": "  The Two " }));
    assert_eq!((two.status, json_of(&two)["name"].clone()), (201, serde_json::json!("The Two")));
    let listed = json_of(&perch.get("/hub/projects"));
    assert_eq!(listed.as_array().unwrap().len(), 2, "{listed}");
    let file = home.join("projects.json");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(&file).unwrap()).unwrap(), listed, "what is listed is what is on disk");

    // Refusals.
    let outside = tempfile::tempdir().unwrap();
    for bad in [outside.path().to_str().unwrap().to_string(), "/etc".into(), format!("{}/nonexistent", root.display()), format!("{}/..", proj.display())] {
        let refused = perch.post_json("/hub/projects", serde_json::json!({ "path": bad }));
        assert_eq!(refused.status, 422, "{bad}: {}{}", refused.head, refused.body);
    }
    std::fs::create_dir(root.join(".git")).unwrap();
    assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "path": root.join(".git") })).status, 422, "a denied folder is not a project");
    let fresh = root.join("three");
    std::fs::create_dir(&fresh).unwrap();
    assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "path": fresh, "name": "a\nb" })).status, 400, "a bad name");
    assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "nope": 1 })).status, 400);
    assert_eq!(json_of(&perch.get("/hub/projects")).as_array().unwrap().len(), 2, "a refusal adds nothing");

    // Delete forgets the entry by its path, and only that: a stale list cannot remove a different one.
    let delete = |body: serde_json::Value| perch.apireq("DELETE", "/hub/projects").body(serde_json::to_vec(&body).unwrap()).send(&perch.addr);
    assert_eq!(delete(serde_json::json!({ "path": format!("{}/", proj.display()) })).status, 204);
    assert!(proj.join("keep.txt").is_file() && proj.is_dir(), "the folder and its files are untouched");
    let left = json_of(&perch.get("/hub/projects"));
    assert_eq!(left.as_array().unwrap().len(), 1);
    assert_eq!(left[0]["name"], "The Two");
    assert_eq!(delete(serde_json::json!({ "path": proj })).status, 404, "a second tab asking again removes nothing else");
    assert_eq!(json_of(&perch.get("/hub/projects")).as_array().unwrap().len(), 1, "the other entry survived the stale delete");
    assert_eq!(delete(serde_json::json!({ "nope": 1 })).status, 400);
    assert_eq!(perch.apireq("DELETE", "/hub/projects").send(&perch.addr).status, 400, "no body names nothing");
    for gone in ["/hub/projects/0", "/hub/projects/x", "/hub/projects/0/1"] {
        assert_eq!(perch.apireq("DELETE", gone).send(&perch.addr).status, 405, "{gone}: removal by position no longer exists");
    }
    assert_eq!(Request::new(&perch.addr, "DELETE", "/hub/projects").body(b"{}".to_vec()).send(&perch.addr).status, 401, "behind the token like the rest");
    assert_eq!(perch.apireq("DELETE", "/hub/sessions/x").send(&perch.addr).status, 405, "no other DELETE exists");
}

/// A chat in a project is titled by the project, a scratch chat is `new chat`, never the stamp; and a saved chat
/// still knows its folder (the cwd is remembered by the perch, not decoded from the journal).
/// Mutations: put the folder name back in the scratch title; drop the project lookup; drop the kept row's cwd.
#[cfg(feature = "test-bins")]
#[test]
fn chat_titles_name_the_project_or_new_chat_and_never_a_scratch_stamp() {
    let (base, home, perch) = start_home("");
    let root = base.path().canonicalize().unwrap();
    let proj = root.join("folder-name");
    std::fs::create_dir(&proj).unwrap();
    assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "path": proj, "name": "Pretty Name" })).status, 201);

    let in_project = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "cwd": proj })));
    let scratch_a = body_id(&perch.post_json("/hub/sessions", serde_json::json!({})));
    let scratch_b = body_id(&perch.post_json("/hub/sessions", serde_json::json!({})));
    let title = |id: &str| perch.row(id).unwrap()["title"].as_str().unwrap().to_string();
    assert!(title(&in_project).starts_with("Pretty Name · "), "{}", title(&in_project));
    let cwd_of = |id: &str| perch.row(id).unwrap()["cwd"].as_str().unwrap().to_string();
    let (ca, cb) = (cwd_of(&scratch_a), cwd_of(&scratch_b));
    // Before it has a title an inbox chat is called by its birth name, which is its folder's: three lowercase words, no time, no stamp.
    for (id, cwd) in [(&scratch_a, &ca), (&scratch_b, &cb)] {
        let t = title(id);
        assert_eq!(Some(t.as_str()), std::path::Path::new(cwd).file_name().and_then(|n| n.to_str()), "{t}");
        assert!(t.split('-').count() == 3 && t.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{t}");
    }
    assert_ne!(ca, cb, "two [new chat] are two folders");
    assert!(ca.starts_with(home.join("scratch").to_str().unwrap()));

    // Saved: the row keeps its folder, so the page can put it under its project.
    assert_eq!(perch.post(&format!("/hub/sessions/{in_project}/stop")).status, 202);
    assert!(perch.wait_state(&in_project, "kept", Duration::from_secs(5)));
    assert_eq!(perch.row(&in_project).unwrap()["cwd"].as_str(), proj.to_str(), "{}", perch.row(&in_project).unwrap());
    assert!(std::fs::read_to_string(home.join("chat-cwds.json")).unwrap().contains(proj.to_str().unwrap()));
    for id in [&scratch_a, &scratch_b] {
        assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    }
}

/// The picker lists subfolders only, starts at the roots, and every way out (`..`, a link, a denied name, an 8.3 alias,
/// outside the roots, a file) is the same empty 404. Mutations: list files; drop the starts_with check; drop the deny test.
#[cfg(feature = "test-bins")]
#[test]
fn the_folder_picker_lists_subfolders_under_the_roots_only() {
    let (base, _home, perch) = start_home("");
    let root = base.path().canonicalize().unwrap();
    for sub in ["work/b", "work/a", "work/.hidden", "work/.git", "work/GIT~1", "work/secrets"] {
        std::fs::create_dir_all(root.join(sub)).unwrap();
    }
    std::fs::write(root.join("work/file.txt"), "x").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), root.join("work/escape")).unwrap();

    // No path: the roots (the harness gives the tempdir and the home).
    let top = perch.get("/hub/dirs");
    assert_eq!((top.status, top.header("content-type").as_deref()), (200, Some("application/json")), "{}", top.head);
    let top = json_of(&top);
    assert!(top["parent"].is_null() && top["path"] == "");
    let paths: Vec<&str> = top["dirs"].as_array().unwrap().iter().map(|d| d["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&root.to_str().unwrap()), "{paths:?}");
    assert!(top["dirs"].as_array().unwrap().iter().all(|d| !d["name"].as_str().unwrap().is_empty()), "every root is labelled");
    assert_eq!(json_of(&perch.get("/hub/dirs?path=")), top, "an empty path is the roots too");

    let q = |path: &std::path::Path| format!("/hub/dirs?path={}", path.display().to_string().replace('/', "%2F"));
    let got = json_of(&perch.get(&q(&root.join("work"))));
    let names: Vec<&str> = got["dirs"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["a", "b"], "no file, link, dot folder, denied name or alias: {got}");
    assert_eq!(got["dirs"][0]["path"].as_str(), root.join("work/a").to_str());
    assert_eq!(got["parent"].as_str(), root.to_str(), "up from work is its parent");
    assert_eq!(got["crumbs"].as_array().unwrap().last().unwrap()["name"], "work");
    assert_eq!(json_of(&perch.get(&q(&root)))["parent"], "", "above a root is the roots");

    for bad in [
        root.join("work/escape"),
        root.join("work/.git"),
        root.join("work/GIT~1"),
        root.join("work/secrets"),
        root.join("work/file.txt"),
        root.join("work/nope"),
        root.join("work/b/../a"),
        root.parent().unwrap().to_path_buf(),
        "/".into(),
        "/etc".into(),
        elsewhere.path().to_path_buf(),
    ] {
        let refused = perch.get(&q(&bad));
        assert_eq!((refused.status, refused.body.as_str()), (404, ""), "{}", bad.display());
    }
    assert_eq!(perch.get("/hub/dirs?path=relative%2Fdir").status, 404);
    assert_eq!(perch.get("/hub/dirs?path=a%5Cb").status, 404, "a backslash is not a separator here: the page converts");
    for bad in ["/hub/dirs?x=1", "/hub/dirs?path=1&path=2", "/hub/dirs?path=%zz", "/hub/dirs?path"] {
        assert_eq!(perch.get(bad).status, 400, "{bad}");
    }
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/dirs").send(&perch.addr).status, 401);
}

/// `POST /hub/dirs` makes one folder, in one root, with a plain name. Mutations: allow `/` or `..` in the name; drop the parent
/// check (the outside folder appears); use create_dir_all (a missing parent is made).
#[cfg(feature = "test-bins")]
#[test]
fn the_folder_picker_makes_one_folder_with_a_plain_name() {
    let (base, _home, perch) = start_home("");
    let root = base.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("work")).unwrap();
    let make = |parent: &std::path::Path, name: &str| perch.post_json("/hub/dirs", serde_json::json!({ "parent": parent, "name": name }));

    let ok = make(&root.join("work"), "new project");
    assert_eq!(ok.status, 201, "{}{}", ok.head, ok.body);
    assert_eq!(json_of(&ok)["path"].as_str(), root.join("work/new project").to_str());
    assert!(root.join("work/new project").is_dir());
    assert_eq!(make(&root.join("work"), "new project").status, 409, "it exists");

    let long = "n".repeat(101);
    for bad in ["", " ", "a/b", "a\\b", "..", "a..b", ".hidden", "a\nb", "a\u{7}b", " x", "x ", "GIT~1", "secrets", long.as_str()] {
        let refused = make(&root.join("work"), bad);
        assert_eq!(refused.status, 400, "{bad:?}: {}{}", refused.head, refused.body);
    }
    assert_eq!(std::fs::read_dir(root.join("work")).unwrap().count(), 1, "nothing but the one folder was made");

    let outside = tempfile::tempdir().unwrap();
    for parent in [outside.path().to_path_buf(), root.join("work/missing"), root.join("work/.."), root.parent().unwrap().to_path_buf(), "/etc".into(), "rel".into()] {
        let refused = make(&parent, "evil");
        assert_eq!(refused.status, 422, "{}: {}{}", parent.display(), refused.head, refused.body);
    }
    assert!(!outside.path().join("evil").exists() && !root.join("work/missing").exists() && !root.parent().unwrap().join("evil").exists());
    std::fs::create_dir(root.join(".git")).unwrap();
    assert_eq!(make(&root.join(".git"), "x").status, 422, "not inside a denied folder");
    assert_eq!(perch.post_json("/hub/dirs", serde_json::json!({ "parent": root })).status, 400);
    assert_eq!(Request::new(&perch.addr, "POST", "/hub/dirs").send(&perch.addr).status, 401);
}

// ---------------------------------------------------------------------------
// The composer's model and mode menus: GET /hub/models, POST /hub/sessions/<id>/restart
// ---------------------------------------------------------------------------

/// `GET /hub/models` is eidolon's own listing (`eidolon models`, run with the Minerva config), parsed into keys with a name and a
/// note, and asked once for ten minutes; a hung command is a 504 inside the bound and a failed one a 502, and neither is cached.
/// Mutations: no cache (the helper count climbs); no timeout (the hung case never answers); cache a failure.
#[cfg(feature = "test-bins")]
#[test]
fn the_model_list_is_eidolons_own_cached_and_bounded() {
    let (_base, home, perch) = start_home("");
    let first = perch.get("/hub/models");
    assert_eq!(first.status, 200, "{}{}", first.head, first.body);
    assert_eq!(first.header("content-type").as_deref(), Some("application/json"), "{}", first.head);
    let rows: serde_json::Value = serde_json::from_str(&first.body).unwrap();
    assert_eq!(rows[0], serde_json::json!({ "key": "mock:alpha", "name": "Alpha", "note": "mock · 10k ctx · free" }), "{rows}");
    assert_eq!(rows[1]["key"], "mock:beta");
    assert_eq!(rows.as_array().unwrap().len(), 2, "the provider table and the notice are not models: {rows}");
    assert_eq!(perch.get("/hub/models").body, first.body);
    let helpers = perch.fake_helpers();
    assert_eq!(helpers.len(), 1, "two requests, one `eidolon models`: {helpers:?}");
    assert!(helpers[0].starts_with("helper models") && helpers[0].ends_with("config=true"), "run with the Minerva config: {helpers:?}");
    assert!(home.join("config.toml").is_file());
    // Behind the token, like every /hub route.
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/models").send(&perch.addr).status, 401);

    // A command that does not answer in time: 504, inside the bound, and asked again next time (a failure is not cached).
    let slow = Perch::start_hub(&[("FAKE_MODELS_SLEEP_MS", "5000"), ("PERCH_MODELS_TIMEOUT_MS", "400")]);
    let began = Instant::now();
    let late = slow.get("/hub/models");
    assert_eq!(late.status, 504, "{}{}", late.head, late.body);
    assert!(began.elapsed() < Duration::from_secs(4), "the bound held: {:?}", began.elapsed());
    assert_eq!(slow.get("/hub/models").status, 504);
    assert_eq!(slow.fake_helpers().len(), 2, "a timeout is asked again, not cached");

    let failing = Perch::start_hub(&[("FAKE_HELPER_FAIL", "1")]);
    let broken = failing.get("/hub/models");
    assert_eq!(broken.status, 502, "{}{}", broken.head, broken.body);
    assert!(broken.body.contains("could not list its models"), "{}", broken.body);
}

/// The restart route: the same log and the Minerva config, the new `-m` / `--yolo`, the old flags kept when the request names
/// one of them, no restart for flags the door already has, a revert when the new ones do not come up, and the refusals.
/// Mutations: drop `--yolo` or `-m` from the new argv; resume with `--cwd` (a new chat) instead of `--session`; forget the
/// model across a yolo-only restart; no revert; accept `-m --yolo`.
#[cfg(feature = "test-bins")]
#[test]
fn a_restart_resumes_the_same_log_with_the_new_flags_and_keeps_the_rest() {
    let (_base, home, perch) = start_home("");
    let config = home.join("config.toml");
    let created = perch.post_json("/hub/sessions", serde_json::json!({}));
    assert_eq!(created.status, 201, "{}{}", created.head, created.body);
    let id = body_id(&created);
    let log = home.join("sessions").join(format!("{id}.eid"));
    let starts = |perch: &Perch| perch.fake_records().iter().filter(|line| line.starts_with("start ")).count();
    assert_eq!(starts(&perch), 1);

    let restart = |body: serde_json::Value| perch.post_json(&format!("/hub/sessions/{id}/restart"), body);
    // Model and mode in one: the same log, the home's config, `--yolo` before the subcommand, `-m` after the target.
    let first = restart(serde_json::json!({ "model": "mock:alpha", "yolo": true }));
    assert_eq!(first.status, 200, "{}{}", first.head, first.body);
    let said: serde_json::Value = serde_json::from_str(&first.body).unwrap();
    assert_eq!(said, serde_json::json!({ "id": id, "model": "mock:alpha", "mode": "yolo", "yolo": true, "restarted": true }));
    let argv = perch.fake_argv();
    assert!(argv.starts_with(&format!("argv --config {} --yolo web ", config.display())), "{argv}");
    // The same log (not a new chat), and the folder the perch remembers for it as eidolon's reopen-time `--cwd`.
    let cwd = perch.row(&id).unwrap()["cwd"].as_str().unwrap().to_string();
    assert!(argv.contains(&format!("--session {} --cwd {cwd} ", log.display())), "the same log and its folder: {argv}");
    assert!(argv.ends_with(" -m mock:alpha"), "{argv}");
    assert_eq!(starts(&perch), 2);
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"), "the same id is live again");
    assert_eq!(perch.list().iter().filter(|row| row["id"] == id.as_str()).count(), 1, "one row, not a new chat beside it");

    // Only the mode changes: the model the door has stays, `--yolo` goes.
    let second = restart(serde_json::json!({ "yolo": false }));
    assert_eq!(second.status, 200, "{}{}", second.head, second.body);
    let argv = perch.fake_argv();
    assert!(!argv.contains("--yolo") && argv.ends_with(" -m mock:alpha"), "the model is kept across a mode change: {argv}");
    assert_eq!(starts(&perch), 3);
    // Only the model changes: yolo is off, stays off.
    assert_eq!(restart(serde_json::json!({ "model": "mock:beta" })).status, 200);
    assert!(perch.fake_argv().ends_with(" -m mock:beta") && !perch.fake_argv().contains("--yolo"), "{}", perch.fake_argv());
    assert_eq!(starts(&perch), 4);

    // What it already is: nothing is restarted.
    let same = restart(serde_json::json!({ "model": "mock:beta", "yolo": false }));
    assert_eq!(same.status, 200);
    assert!(same.body.contains("\"restarted\":false"), "{}", same.body);
    assert_eq!(starts(&perch), 4, "no new door for the flags it already has");

    // A model that does not start: the chat is brought back on the one it had, and the answer says so.
    let nope = restart(serde_json::json!({ "model": "nope" }));
    assert_eq!(nope.status, 503, "{}{}", nope.head, nope.body);
    let said: serde_json::Value = serde_json::from_str(&nope.body).unwrap();
    assert_eq!(said["reverted"], true, "{said}");
    assert!(said["error"].as_str().unwrap().contains("back on mock:beta"), "{said}");
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));
    assert!(perch.fake_argv().ends_with(" -m mock:beta"), "the revert is the old model: {}", perch.fake_argv());

    // Refusals. A body that names nothing, and a model that could be read as a flag, never reach a door.
    let before = starts(&perch);
    assert_eq!(restart(serde_json::json!({})).status, 400);
    assert_eq!(restart(serde_json::json!({ "model": "--yolo" })).status, 400);
    assert_eq!(restart(serde_json::json!({ "model": "a b" })).status, 400);
    assert_eq!(perch.post_json("/hub/sessions/nothing-here/restart", serde_json::json!({ "yolo": true })).status, 404);
    assert_eq!(perch.post_json("/hub/sessions/..%2Fx/restart", serde_json::json!({ "yolo": true })).status, 404);
    std::fs::write(home.join("sessions").join("saved.eid"), chat_log(None)).unwrap();
    let kept = perch.post_json("/hub/sessions/saved/restart", serde_json::json!({ "yolo": true }));
    assert_eq!(kept.status, 409, "a saved chat is continued, not restarted: {}{}", kept.head, kept.body);
    assert!(kept.body.contains("not running"), "{}", kept.body);
    assert_eq!(perch.get(&format!("/hub/sessions/{id}/restart")).status, 404, "POST only");
    assert_eq!(starts(&perch), before);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// A restart in the middle of a turn would cut it off: a running turn (the door's own `turn-state`, read off the perch's stream)
/// is a 409 with a sentence, and nothing is stopped. When the turn ends the same request goes through, on a **resumed** chat
/// too (its stream is kept, which it was not before). Mutations: ignore `turn-state`; do not keep a resumed door's stream.
#[cfg(feature = "test-bins")]
#[test]
fn a_restart_waits_for_the_turn_to_end() {
    let perch = Perch::start_hub(&[("FAKE_SAY_RUNS", "1")]);
    let (id, _, _) = live(&perch);
    // A resumed door is the harder case: stop, resume, and only then run a turn.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    let starts = || perch.fake_records().iter().filter(|line| line.starts_with("start ")).count();
    assert_eq!(starts(), 2);
    let said = perch.post_json(&format!("/s/{id}/api/say"), serde_json::json!({ "text": "go" }));
    assert_eq!(said.status, 202, "{}{}", said.head, said.body);
    assert!(wait_record(&perch, Duration::from_secs(3), |lines| lines.iter().any(|line| line == "turn-state true")), "the door announced the turn");
    std::thread::sleep(Duration::from_millis(300));
    let busy = perch.post_json(&format!("/hub/sessions/{id}/restart"), serde_json::json!({ "mode": "auto" }));
    assert_eq!(busy.status, 409, "{}{}", busy.head, busy.body);
    assert!(busy.body.contains("wait for the turn to end"), "{}", busy.body);
    assert_eq!(starts(), 2, "a refused restart stops nothing");
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));

    assert_eq!(perch.post(&format!("/s/{id}/api/cancel")).status, 204);
    let deadline = Instant::now() + Duration::from_secs(3);
    let done = loop {
        let answer = perch.post_json(&format!("/hub/sessions/{id}/restart"), serde_json::json!({ "mode": "auto" }));
        if answer.status != 409 || Instant::now() > deadline {
            break answer;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(done.status, 200, "{}{}", done.head, done.body);
    assert!(!perch.fake_argv().contains("--yolo"), "a new chat is yolo; the restart made it auto: {}", perch.fake_argv());
    assert_eq!(starts(), 3);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

// ---------------------------------------------------------------------------
// Archive, pin and trash (hub.md section 7b)
// ---------------------------------------------------------------------------

/// Sets a log's mtime to `days` ago, as a chat that was last used then.
#[cfg(feature = "test-bins")]
fn age(path: &std::path::Path, days: u64) {
    let at = std::time::SystemTime::now() - Duration::from_secs(days * 86_400 + 60);
    std::fs::OpenOptions::new().write(true).open(path).unwrap().set_modified(at).unwrap();
}

/// `(archived, auto_other, pinned)` of a listed chat.
#[cfg(feature = "test-bins")]
fn flags_of(perch: &Perch, id: &str) -> (bool, bool, bool) {
    let row = perch.row(id).unwrap_or_else(|| panic!("{id} is listed"));
    let flag = |name: &str| row[name].as_bool().unwrap_or_else(|| panic!("{name} on {row}"));
    (flag("archived"), flag("auto_other"), flag("pinned"))
}

/// A Minerva chat idle for 7 days is `auto_other` (computed at listing time, no file moves); a fresh one and a live one never
/// are; archive puts one in `other` now and unarchive brings it back with a fresh week; continuing an archived chat unarchives it.
/// Mutations: change 7 days; drop the live exemption; drop `unarchived_at` from the idle clock; leave the mark on a resumed chat.
#[cfg(feature = "test-bins")]
#[test]
fn an_idle_week_is_other_and_archive_unarchive_and_the_grace_work() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_base, home, perch) = start_home("");
    let sessions = home.join("sessions");
    for id in ["fresh", "six", "eight", "live", "mine"] {
        std::fs::write(sessions.join(format!("{id}.eid")), chat_log(Some(id))).unwrap();
    }
    age(&sessions.join("six.eid"), 6);
    age(&sessions.join("eight.eid"), 8);
    age(&sessions.join("live.eid"), 30);
    age(&sessions.join("mine.eid"), 30);
    assert_eq!(flags_of(&perch, "fresh"), (false, false, false));
    assert_eq!(flags_of(&perch, "six"), (false, false, false), "6 days is still active");
    assert_eq!(flags_of(&perch, "eight"), (false, true, false), "8 days idle is `other`");
    assert!(sessions.join("eight.eid").is_file(), "nothing is moved for it");
    // A live chat never ages out, however old its log is.
    assert_eq!(perch.post("/hub/sessions/live/resume").status, 200);
    assert_eq!(flags_of(&perch, "live"), (false, false, false));
    assert_eq!(perch.post("/hub/sessions/live/archive").status, 409, "a running chat is not archived");
    assert_eq!(perch.post("/hub/sessions/live/stop").status, 202);
    assert!(perch.wait_state("live", "kept", Duration::from_secs(5)));
    // Archive by hand, and back.
    assert_eq!(perch.post("/hub/sessions/fresh/archive").status, 204);
    assert_eq!(flags_of(&perch, "fresh"), (true, false, false));
    let file = home.join("archive.json");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    let on_disk: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(on_disk["fresh"]["archived"], true, "{on_disk}");
    assert_eq!(perch.post("/hub/sessions/fresh/unarchive").status, 204);
    assert_eq!(flags_of(&perch, "fresh"), (false, false, false));
    // An old chat, un-archived, is not back in `other` the next moment: it has a fresh week from the unarchive.
    assert_eq!(perch.post("/hub/sessions/eight/archive").status, 204);
    assert_eq!(flags_of(&perch, "eight"), (true, false, false));
    assert_eq!(perch.post("/hub/sessions/eight/unarchive").status, 204);
    assert_eq!(flags_of(&perch, "eight"), (false, false, false), "the grace after unarchive");
    // Continuing an archived chat brings it out of `other`.
    assert_eq!(perch.post("/hub/sessions/mine/archive").status, 204);
    assert_eq!(perch.post("/hub/sessions/mine/resume").status, 200);
    assert_eq!(perch.post("/hub/sessions/mine/stop").status, 202);
    assert!(perch.wait_state("mine", "kept", Duration::from_secs(5)));
    assert_eq!(flags_of(&perch, "mine"), (false, false, false), "continuing it is activity");
    // Bad ids and unknown chats are the 404 of a route that is not there; the token gate is ahead of them.
    for verb in ["archive", "unarchive", "pin", "unpin", "trash"] {
        assert_eq!(perch.post(&format!("/hub/sessions/nope/{verb}")).status, 404, "{verb}");
        assert_eq!(perch.post(&format!("/hub/sessions/..%2Fsaved/{verb}")).status, 404, "{verb} with a path piece");
        assert_eq!(Request::new(&perch.addr, "POST", &format!("/hub/sessions/fresh/{verb}")).send(&perch.addr).status, 401, "{verb} needs the token");
    }
}

/// A pinned chat never ages into `other`; archiving unpins, pinning an archived chat brings it out; a shared-dir chat refuses
/// archive, pin and trash and is not touched. Mutations: let `auto_other` win over a pin; keep the pin through archive; allow a
/// shared-dir pin.
#[cfg(feature = "test-bins")]
#[test]
fn a_pinned_chat_stays_out_of_other_and_a_shared_dir_chat_cannot_be_marked() {
    let (_base, home, perch) = start_home("");
    let sessions = home.join("sessions");
    std::fs::write(sessions.join("old.eid"), chat_log(Some("old"))).unwrap();
    age(&sessions.join("old.eid"), 40);
    let shared = perch.shared();
    std::fs::write(shared.join("theirs.eid"), chat_log(Some("theirs"))).unwrap();
    assert_eq!(flags_of(&perch, "old"), (false, true, false));
    assert_eq!(perch.post("/hub/sessions/old/pin").status, 204);
    assert_eq!(flags_of(&perch, "old"), (false, false, true), "pinned: never `other`, however old");
    assert_eq!(perch.post("/hub/sessions/old/archive").status, 204);
    assert_eq!(flags_of(&perch, "old"), (true, false, false), "archiving unpins");
    assert_eq!(perch.post("/hub/sessions/old/pin").status, 204);
    assert_eq!(flags_of(&perch, "old"), (false, false, true), "pinning brings it out of `other`");
    assert_eq!(perch.post("/hub/sessions/old/unpin").status, 204);
    assert_eq!(flags_of(&perch, "old"), (false, true, false), "unpinned, it is as idle as it was");
    // Shared dir: refused, and nothing changed on disk or in the list.
    for verb in ["archive", "pin", "trash"] {
        assert_eq!(perch.post(&format!("/hub/sessions/theirs/{verb}")).status, 403, "{verb}");
    }
    let row = perch.row("theirs").unwrap();
    assert_eq!((row["origin"].as_str(), row["pinned"].as_bool(), row["archived"].as_bool()), (Some("other"), Some(false), Some(false)), "{row}");
    assert!(shared.join("theirs.eid").is_file());
    assert!(!home.join("trash").join("theirs.eid").exists());
}

/// Delete is a move into `<home>/trash`: the chat leaves the list and appears in `GET /hub/trash`; restore puts it back (409 when
/// the name is taken; not pinned again); empty needs `confirm` and removes only regular `.eid` files inside the trash dir; a
/// live chat is 409 unless `stop`. Mutations: copy instead of move; empty without the confirm; empty following a symlink out of
/// the trash; trash a live chat without stopping it.
#[cfg(feature = "test-bins")]
#[test]
fn delete_moves_to_trash_and_restore_and_empty_touch_only_the_trash() {
    let (base, home, perch) = start_home("");
    let sessions = home.join("sessions");
    let trash = home.join("trash");
    for id in ["a", "b", "run"] {
        std::fs::write(sessions.join(format!("{id}.eid")), chat_log(Some(id))).unwrap();
    }
    assert_eq!(perch.post("/hub/sessions/a/pin").status, 204);
    assert_eq!(json_of(&perch.get("/hub/trash")), serde_json::json!([]));
    assert_eq!(perch.post("/hub/sessions/a/trash").status, 204);
    assert!(!sessions.join("a.eid").exists() && trash.join("a.eid").is_file(), "moved, not copied");
    assert!(perch.row("a").is_none(), "gone from the list");
    let listed = json_of(&perch.get("/hub/trash"));
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed[0]["id"], "a");
    assert!(listed[0]["title"].as_str().is_some_and(|t| !t.is_empty()) && listed[0]["mtime"].as_u64().is_some(), "{listed}");
    assert_eq!(perch.post("/hub/sessions/a/trash").status, 404, "already gone");
    // Restore: back, not pinned, a fresh week; and 409 when the name is taken.
    assert_eq!(perch.post("/hub/trash/a/restore").status, 204);
    assert!(sessions.join("a.eid").is_file() && !trash.join("a.eid").exists());
    assert_eq!(flags_of(&perch, "a"), (false, false, false), "a restored chat is not pinned again");
    assert_eq!(perch.post("/hub/sessions/a/trash").status, 204);
    std::fs::write(sessions.join("a.eid"), chat_log(Some("new a"))).unwrap();
    assert_eq!(perch.post("/hub/trash/a/restore").status, 409, "a chat of that name is there");
    assert!(trash.join("a.eid").is_file() && sessions.join("a.eid").is_file(), "neither was overwritten");
    assert_eq!(perch.post("/hub/trash/zzz/restore").status, 404);
    assert_eq!(perch.post("/hub/trash/..%2Fa/restore").status, 404);
    assert_eq!(perch.get("/hub/trash/a/restore").status, 404);
    // A live chat: 409 unless it is stopped first.
    assert_eq!(perch.post("/hub/sessions/run/resume").status, 200);
    assert_eq!(perch.post("/hub/sessions/run/trash").status, 409);
    assert!(sessions.join("run.eid").is_file(), "still there");
    let stopped = perch.post_json("/hub/sessions/run/trash", serde_json::json!({ "stop": true }));
    assert_eq!(stopped.status, 204, "{}{}", stopped.head, stopped.body);
    assert!(trash.join("run.eid").is_file() && !sessions.join("run.eid").exists());
    assert!(perch.row("run").is_none());
    assert!(perch.door_token_files().is_empty(), "the door was stopped cleanly");
    // Empty: needs the confirm, and removes only `.eid` regular files in the trash.
    let outside = base.path().join("outside.eid");
    std::fs::write(&outside, "keep").unwrap();
    std::os::unix::fs::symlink(&outside, trash.join("link.eid")).unwrap();
    std::fs::write(trash.join("notes.txt"), "keep").unwrap();
    std::fs::create_dir(trash.join("dir.eid")).unwrap();
    assert_eq!(perch.post("/hub/trash/empty").status, 400);
    assert_eq!(perch.post_json("/hub/trash/empty", serde_json::json!({ "confirm": false })).status, 400);
    assert!(trash.join("a.eid").is_file(), "nothing was removed without the confirm");
    let emptied = perch.post_json("/hub/trash/empty", serde_json::json!({ "confirm": true }));
    assert_eq!((emptied.status, json_of(&emptied)["removed"].clone()), (200, serde_json::json!(2)), "{}", emptied.body);
    assert!(!trash.join("a.eid").exists() && !trash.join("run.eid").exists(), "the trashed chats are gone for good");
    assert!(outside.is_file() && trash.join("notes.txt").is_file() && trash.join("dir.eid").is_dir(), "only chats in the trash");
    assert!(std::fs::symlink_metadata(trash.join("link.eid")).is_ok(), "a symlink is not followed or removed");
    assert!(sessions.join("a.eid").is_file() && sessions.join("b.eid").is_file(), "sessions/ is untouched");
    assert_eq!(json_of(&perch.get("/hub/trash")), serde_json::json!([]));
}

// ---------------------------------------------------------------------------
// Images in a chat: GET /hub/sessions/<id>/{images,image/<record>/<n>,transcript}
// ---------------------------------------------------------------------------

/// Standard padded base64, for fixtures: the bytes the tests use are ASCII, so the response body can be compared as text.
#[cfg(feature = "test-bins")]
fn b64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// One image to put in a fixture log: media type, bytes, alt.
#[cfg(feature = "test-bins")]
type Pic<'a> = (&'a str, Vec<u8>, Option<&'a str>);

/// A log as `eidolon log --json` prints it, with images: `(record, images)`, each record a user message.
#[cfg(feature = "test-bins")]
fn image_log(messages: &[(u64, Vec<Pic>)]) -> String {
    let mut text = chat_log(None);
    for (record, images) in messages {
        let mut content: Vec<serde_json::Value> = images
            .iter()
            .map(|(media, bytes, alt)| {
                let mut block = serde_json::json!({ "type": "image", "source": { "type": "base64", "media_type": media, "data": b64(bytes) } });
                if let Some(alt) = alt {
                    block["alt"] = (*alt).into();
                }
                block
            })
            .collect();
        content.push(serde_json::json!({ "type": "text", "text": "look" }));
        let line = serde_json::json!({ "id": record, "parent": record - 1, "ts_ms": 2, "kind": { "UserMessage": { "role": "user", "content": content } } });
        text.push_str(&format!("{line}\n"));
    }
    text
}

/// The list names every image block with its place, the bytes route answers them with the declared type and the headers that
/// make them inert and cacheable, and everything that is not one is a 404 (a type outside the four is a 415). Mutations: no
/// `nosniff`; a type other than the log's; an svg served; a record or index parsed loosely (`+1`, `1.0`); a query accepted.
#[cfg(feature = "test-bins")]
#[test]
fn a_chats_images_are_listed_and_served_as_what_the_log_declares() {
    let (_base, home, perch) = start_home("");
    let log = image_log(&[
        (1, vec![("image/png", b"PNG-ONE".to_vec(), Some("dot.png")), ("image/jpeg", b"JPEG-TWO".to_vec(), None)]),
        (2, vec![("image/svg+xml", b"<svg/>".to_vec(), Some("evil.svg"))]),
    ]);
    std::fs::write(home.join("sessions/pics.eid"), &log).unwrap();
    let list = perch.get("/hub/sessions/pics/images");
    assert_eq!(list.status, 200, "{}", list.head);
    assert_eq!(list.header("content-type").as_deref(), Some("application/json"), "{}", list.head);
    let rows: serde_json::Value = serde_json::from_str(&list.body).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 3, "{rows}");
    assert_eq!(rows[0], serde_json::json!({ "record": 1, "n": 0, "media_type": "image/png", "where": "user", "bytes": 7, "alt": "dot.png" }));
    assert_eq!((rows[1]["n"].as_u64(), rows[1]["media_type"].as_str(), rows[1].get("alt")), (Some(1), Some("image/jpeg"), None));
    assert_eq!(rows[2]["media_type"], "image/svg+xml");

    let one = perch.get("/hub/sessions/pics/image/1/0");
    assert_eq!((one.status, one.body.as_str()), (200, "PNG-ONE"), "{}", one.head);
    assert_eq!(one.header("content-type").as_deref(), Some("image/png"), "{}", one.head);
    assert_eq!(one.header("content-length").as_deref(), Some("7"), "{}", one.head);
    assert_eq!(one.header("x-content-type-options").as_deref(), Some("nosniff"), "{}", one.head);
    assert_eq!(one.header("cache-control").as_deref(), Some("private, max-age=3600"), "{}", one.head);
    assert_eq!(one.header("content-security-policy").as_deref(), Some("default-src 'none'; sandbox"), "{}", one.head);
    let two = perch.get("/hub/sessions/pics/image/1/1");
    assert_eq!((two.status, two.body.as_str(), two.header("content-type").as_deref()), (200, "JPEG-TWO", Some("image/jpeg")), "{}", two.head);
    // Listed, never served: the type is not one of the four.
    assert_eq!(perch.get("/hub/sessions/pics/image/2/0").status, 415);
    // Not images: a record with none, an index past the end, a record that is not there, and every spelling that is not a number.
    for path in [
        "/hub/sessions/pics/image/0/0",
        "/hub/sessions/pics/image/1/2",
        "/hub/sessions/pics/image/9/0",
        "/hub/sessions/pics/image/+1/0",
        "/hub/sessions/pics/image/1/-0",
        "/hub/sessions/pics/image/1.0/0",
        "/hub/sessions/pics/image/abc/0",
        "/hub/sessions/pics/image//0",
        "/hub/sessions/pics/image/1/0?x=1",
        "/hub/sessions/pics/images?x=1",
        "/hub/sessions/nope/image/1/0",
        "/hub/sessions/nope/images",
        "/hub/sessions/..%2Fpics/image/1/0",
    ] {
        assert_eq!(perch.get(path).status, 404, "{path}");
    }
    assert_eq!(perch.post("/hub/sessions/pics/image/1/0").status, 404);
    assert_eq!(perch.post("/hub/sessions/pics/images").status, 404);
    assert!(perch.fake_records().iter().all(|line| !line.starts_with("start ")), "reading images starts no door");
    assert_eq!(std::fs::read_to_string(home.join("sessions/pics.eid")).unwrap(), log, "nothing is written");
}

/// A transcript with fifteen images is one eidolon run, not fifteen; a chat that grew is read again, and one in the other
/// source is read without the home's config. Mutations: no cache; a cache that ignores the log changing.
#[cfg(feature = "test-bins")]
#[test]
fn many_images_cost_one_eidolon_run_until_the_log_changes() {
    let (_base, home, perch) = start_home("");
    let shots: Vec<Pic> = (0..5).map(|i| ("image/png", format!("shot-{i}").into_bytes(), None)).collect();
    let path = home.join("sessions/many.eid");
    std::fs::write(&path, image_log(&[(1, shots.clone()), (2, shots.clone()), (3, shots.clone())])).unwrap();
    let runs = |perch: &Perch| perch.fake_helpers().iter().filter(|h| h.starts_with("helper log --json ")).count();
    assert_eq!(perch.get("/hub/sessions/many/images").status, 200);
    for (record, n) in [(1, 0), (1, 4), (2, 2), (3, 0), (3, 4)] {
        let got = perch.get(&format!("/hub/sessions/many/image/{record}/{n}"));
        assert_eq!((got.status, got.body.as_str()), (200, format!("shot-{n}").as_str()), "{record}/{n}");
    }
    assert_eq!(runs(&perch), 1, "one run for the list and five images: {:?}", perch.fake_helpers());
    // The chat grows: the next request reads it again and sees the new record.
    std::fs::write(&path, image_log(&[(1, shots.clone()), (2, shots.clone()), (3, shots.clone()), (4, vec![("image/gif", b"GIF-NEW".to_vec(), None)])])).unwrap();
    let new = perch.get("/hub/sessions/many/image/4/0");
    assert_eq!((new.status, new.body.as_str(), new.header("content-type").as_deref()), (200, "GIF-NEW", Some("image/gif")), "{}", new.head);
    assert_eq!(runs(&perch), 2, "a changed log is read again, once: {:?}", perch.fake_helpers());
    // The other source: read-only, no config.
    std::fs::write(perch.shared().join("old.eid"), image_log(&[(1, vec![("image/webp", b"WEBP-OLD".to_vec(), Some("old.webp"))])])).unwrap();
    let old = perch.get("/hub/sessions/old/image/1/0");
    assert_eq!((old.status, old.body.as_str(), old.header("content-type").as_deref()), (200, "WEBP-OLD", Some("image/webp")), "{}", old.head);
    assert!(perch.fake_helpers().iter().any(|h| h.contains("/eidolon/sessions/old.eid") && h.ends_with("config=false")), "{:?}", perch.fake_helpers());
}

/// The read is bounded: a log whose output is over the cap is a 413, not a truncated picture. And a chat whose pictures alone are
/// over the plain log route's 16 MiB still has a transcript: the same lines with every `data` emptied. Mutations: no cap; a
/// transcript that keeps the bytes; one that drops the other fields.
#[cfg(feature = "test-bins")]
#[test]
fn the_image_read_is_capped_and_the_transcript_carries_no_bytes() {
    let (_base, home, perch) = start_home("");
    let big = vec![b'x'; 5 * 1024 * 1024];
    let pics: Vec<Pic> = (0..3).map(|_| ("image/png", big.clone(), Some("big.png"))).collect();
    std::fs::write(home.join("sessions/big.eid"), image_log(&[(1, pics)])).unwrap();
    assert_eq!(perch.get("/hub/sessions/big/log").status, 413, "the plain log is over its 16 MiB");
    let lean = perch.get("/hub/sessions/big/transcript");
    assert_eq!(lean.status, 200, "{}", lean.head);
    assert_eq!(lean.header("content-type").as_deref(), Some("application/x-ndjson"), "{}", lean.head);
    assert!(lean.body.len() < 4096, "{} bytes", lean.body.len());
    let user: serde_json::Value = serde_json::from_str(lean.body.lines().nth(1).unwrap()).unwrap();
    let blocks = user["kind"]["UserMessage"]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 4, "{user}");
    assert_eq!((blocks[0]["source"]["data"].as_str(), blocks[0]["source"]["media_type"].as_str(), blocks[0]["alt"].as_str()), (Some(""), Some("image/png"), Some("big.png")));
    assert_eq!(blocks[3]["text"], "look");
    assert_eq!(user["id"], 1);
    assert_eq!(perch.get("/hub/sessions/big/image/1/2").body.len(), big.len(), "and the pictures are still there");
    // Over the cap: 129 MiB of output.
    std::fs::write(home.join("sessions/huge.eid"), vec![b'x'; 129 * 1024 * 1024]).unwrap();
    for path in ["/hub/sessions/huge/images", "/hub/sessions/huge/transcript", "/hub/sessions/huge/image/1/0"] {
        let refused = perch.get(path);
        assert_eq!(refused.status, 413, "{path}: {}", refused.head);
        assert!(refused.body.contains("too large"), "{}", refused.body);
    }
}

// ---------------------------------------------------------------------------
// Fork (hub.md section 7c): a new chat copied from one, whole
// ---------------------------------------------------------------------------

/// A chat that ends on a clean turn boundary (a `TurnSettled` last), which is what a fork needs; `chat_log` ends mid-turn.
#[cfg(feature = "test-bins")]
fn settled_log(first: &str) -> String {
    let end = serde_json::json!({ "id": 3, "parent": 2, "ts_ms": 4, "kind": { "TurnSettled": { "stop_reason": "EndTurn", "usage": {} } } });
    format!("{}{end}
", chat_log(Some(first)))
}

#[cfg(feature = "test-bins")]
fn sha(path: &std::path::Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

/// Fork copies the log byte for byte to a new id in the Minerva home, brings a door up on the *copy* (`--session <copy>`, the
/// Minerva config, and no `--cwd`, since the log names the folder), lists it live, and never touches the source; two forks are two
/// ids. Mutations: open the source instead of the copy; copy into the shared dir; reuse an id; write to the source.
#[cfg(feature = "test-bins")]
#[test]
fn fork_copies_the_log_and_opens_the_copy_as_a_live_chat() {
    let (_base, home, perch) = start_home("");
    let sessions = home.join("sessions");
    std::fs::write(sessions.join("src.eid"), settled_log("hello")).unwrap();
    let before = sha(&sessions.join("src.eid"));
    let made = perch.post("/hub/sessions/src/fork");
    assert_eq!(made.status, 201, "{}{}", made.head, made.body);
    let (new, again) = (body_id(&made), body_id(&perch.post_json("/hub/sessions/src/fork", serde_json::json!({}))));
    assert_ne!(new, "src");
    assert_ne!(new, again, "two forks are two chats");
    assert_eq!(json_of(&made)["from"], "src");
    assert_eq!(sha(&sessions.join(format!("{new}.eid"))), before, "the copy is byte for byte the source");
    assert_eq!(sha(&sessions.join("src.eid")), before, "the source is untouched");
    assert_eq!(perch.state_of(&new).as_deref(), Some("live"));
    assert_eq!(perch.state_of("src").as_deref(), Some("kept"), "the original is not started by a fork");
    let argv = perch.fake_records().into_iter().find(|l| l.starts_with("argv ") && l.contains(&format!("{new}.eid"))).expect("an argv for the copy");
    assert!(argv.contains(&format!("{new}.eid")) && !argv.contains("src.eid") && !argv.contains("--cwd"), "{argv}");
    assert!(argv.contains("minerva.toml") || argv.contains("--config"), "the Minerva config: {argv}");
    assert!(!argv.contains(" --at"), "{argv}");
    // The fork is its own chat: stop it and the original can still be resumed.
    assert_eq!(perch.post(&format!("/hub/sessions/{new}/stop")).status, 202);
    assert_eq!(perch.post("/hub/sessions/src/resume").status, 200);
}

/// A source that is live is copied as it stands, and stays live and untouched; a source another process holds is copied too (the
/// perch only reads it). Mutation: refuse a live source.
#[cfg(feature = "test-bins")]
#[test]
fn a_live_source_can_be_forked_and_keeps_running() {
    let (_base, home, perch) = start_home("");
    let sessions = home.join("sessions");
    std::fs::write(sessions.join("run.eid"), settled_log("hi")).unwrap();
    assert_eq!(perch.post("/hub/sessions/run/resume").status, 200);
    let (pid, _) = perch.fake_last_start();
    let before = sha(&sessions.join("run.eid"));
    let made = perch.post("/hub/sessions/run/fork");
    assert_eq!(made.status, 201, "{}{}", made.head, made.body);
    assert_eq!(sha(&sessions.join(format!("{}.eid", body_id(&made)))), before);
    assert_eq!(perch.state_of("run").as_deref(), Some("live"), "the source door keeps going");
    assert!(!is_gone(pid));
}

/// A point that cannot be forked from (a user message, a record that is not there) is a 422 with a sentence, a `at` that is not a number
/// a 400, and neither spends anything: no file, no door. Bad ids, unknown chats and a bad body are 404, 404 and 400; a chat in the
/// trash is not forkable. Mutations: ignore `at` and fork the whole chat; let an id name a path; accept a user message.
#[cfg(feature = "test-bins")]
#[test]
fn a_point_that_cannot_be_forked_from_and_bad_requests_start_nothing() {
    let (_base, home, perch) = start_home("");
    let sessions = home.join("sessions");
    std::fs::write(sessions.join("src.eid"), settled_log("hello")).unwrap();
    let count = || std::fs::read_dir(&sessions).unwrap().count();
    let (files, starts) = (count(), perch.fake_records().iter().filter(|l| l.starts_with("start ")).count());
    let at = perch.post_json("/hub/sessions/src/fork", serde_json::json!({ "at": 1 }));
    assert_eq!(at.status, 422, "{}{}", at.head, at.body);
    assert!(at.body.contains("assistant's replies"), "a sentence for the page: {}", at.body);
    assert_eq!(perch.post_json("/hub/sessions/src/fork", serde_json::json!({ "at": 99 })).status, 422, "no such record");
    assert_eq!(perch.post_json("/hub/sessions/src/fork", serde_json::json!({ "at": "two" })).status, 400);
    assert_eq!(perch.apireq("POST", "/hub/sessions/src/fork").body(b"[1".to_vec()).send(&perch.addr).status, 400);
    for bad in ["nope", "..", "a%2fb", ".hidden"] {
        assert_eq!(perch.post(&format!("/hub/sessions/{bad}/fork")).status, 404, "{bad}");
    }
    assert_eq!(perch.post("/hub/sessions/src/../src/fork").status, 404);
    assert_eq!(count(), files, "nothing was copied");
    assert_eq!(perch.fake_records().iter().filter(|l| l.starts_with("start ")).count(), starts, "no door was forked");
    assert_eq!(perch.post("/hub/sessions/src/trash").status, 204);
    assert_eq!(perch.post("/hub/sessions/src/fork").status, 404);
}

/// A shared-dir chat (`origin: "other"`) is forkable: the copy lands in the Minerva home and the original is not touched.
/// Mutation: write the copy beside the source.
#[cfg(feature = "test-bins")]
#[test]
fn a_shared_dir_chat_can_be_forked_into_the_minerva_home() {
    let (_base, home, perch) = start_home("");
    let shared = perch.shared();
    std::fs::write(shared.join("theirs.eid"), settled_log("theirs")).unwrap();
    let before = sha(&shared.join("theirs.eid"));
    let made = perch.post("/hub/sessions/theirs/fork");
    assert_eq!(made.status, 201, "{}{}", made.head, made.body);
    let new = body_id(&made);
    assert_eq!(sha(&home.join("sessions").join(format!("{new}.eid"))), before);
    assert!(!shared.join(format!("{new}.eid")).exists());
    assert_eq!(sha(&shared.join("theirs.eid")), before);
    assert_eq!(perch.row(&new).unwrap()["origin"], serde_json::Value::Null, "the copy is the Minerva home's own");
    assert_eq!(perch.row("theirs").unwrap()["origin"], "other");
}

/// A copy whose door does not come up is removed and is a 503, never a dead row in the list. Mutation: leave the copy.
#[cfg(feature = "test-bins")]
#[test]
fn a_fork_whose_door_does_not_start_leaves_no_copy() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("minerva.toml"), "default_model = \"mock\"
default_mode = \"auto\"
").unwrap();
    let home = home.canonicalize().unwrap();
    let extra: Vec<std::ffi::OsString> = vec!["--root".into(), base.path().canonicalize().unwrap().into(), "--home".into(), home.clone().into()];
    let perch = Perch::launch_with_member(true, Some(vec![("FAKE_EXIT_BEFORE_BOOT".into(), "3".into())]), &extra, Some(""));
    let sessions = home.join("sessions");
    std::fs::write(sessions.join("src.eid"), settled_log("hello")).unwrap();
    let answer = perch.post("/hub/sessions/src/fork");
    assert_eq!(answer.status, 503, "{}{}", answer.head, answer.body);
    assert_eq!(std::fs::read_dir(&sessions).unwrap().count(), 1, "only the source is left");
    assert_eq!(perch.list().len(), 1, "{:?}", perch.list());
}

// ---------------------------------------------------------------------------
// GET /hub/file (rawfile.rs): one image under the cwd
// ---------------------------------------------------------------------------

/// The route over the wire: a png under the cwd is served with the headers that make it only drawable; svg, text, a
/// disguised file and an over-cap file are 415/413; every way out is the empty 404; no token is 401.
/// Mutation: drop the route arm and the first assert goes red; drop a header and the header asserts do.
#[cfg(feature = "test-bins")]
#[test]
fn a_file_route_serves_an_image_under_the_cwd_and_nothing_else() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    let png: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4];
    let work = &perch.work;
    std::fs::write(work.join("shot.png"), png).unwrap();
    std::fs::write(work.join("x.svg"), "<svg xmlns='http://www.w3.org/2000/svg'><script>1</script></svg>").unwrap();
    std::fs::write(work.join("a.txt"), "hello").unwrap();
    std::fs::write(work.join("fake.png"), "<html>").unwrap();
    let mut big = png.to_vec();
    big.resize(20 * 1024 * 1024 + 1, 0);
    std::fs::write(work.join("big.png"), &big).unwrap();
    std::fs::write(work.join("secret.png"), png).unwrap();
    std::fs::write(work.join("a~1.png"), png).unwrap();
    std::fs::create_dir(work.join(".git")).unwrap();
    std::fs::write(work.join(".git/h.png"), png).unwrap();
    std::fs::write(perch.root.path().join("out.png"), png).unwrap();
    std::os::unix::fs::symlink("../out.png", work.join("up.png")).unwrap();
    let file = |path: &str| perch.get(&format!("/hub/file?session={id}&path={path}"));

    let ok = file("shot.png");
    assert_eq!(ok.status, 200, "{}", ok.head);
    for (name, want) in [
        ("content-type", "image/png"),
        ("x-content-type-options", "nosniff"),
        ("content-security-policy", "default-src 'none'; sandbox"),
        ("cache-control", "private, no-cache"),
        ("content-disposition", "inline"),
        ("content-length", "12"),
    ] {
        assert_eq!(ok.header(name).as_deref(), Some(want), "{name}: {}", ok.head);
    }
    assert!(!ok.has_cors());

    for (path, want) in [("x.svg", 415), ("a.txt", 415), ("fake.png", 415), ("big.png", 413)] {
        assert_eq!(file(path).status, want, "{path}");
    }
    for path in [
        "..%2Fout.png",
        "%2Fetc%2Fpasswd",
        "up.png",
        "secret.png",
        "a~1.png",
        ".git%2Fh.png",
        "missing.png",
        "",
        "shot.png%00",
        "sub%5Cx.png",
    ] {
        let answer = file(path);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "{path}: {}", answer.head);
    }
    assert_eq!(perch.get("/hub/file?session=nobody&path=shot.png").status, 404);
    assert_eq!(perch.get("/hub/file?session=..&path=shot.png").status, 404);
    assert_eq!(perch.get("/hub/file").status, 400);
    assert_eq!(perch.get(&format!("/hub/file?session={id}&path=a&path=b")).status, 400);
    let none = Request::new(&perch.addr, "GET", &format!("/hub/file?session={id}&path=shot.png")).send(&perch.addr);
    assert_eq!((none.status, none.body.as_str()), (401, ""), "{}", none.head);
    let post = Request::new(&perch.addr, "POST", &format!("/hub/file?session={id}&path=shot.png")).bearer(perch.auth()).send(&perch.addr);
    assert_eq!(post.status, 404);
}

/// A chat that ends mid-turn (a message with no `TurnSettled` after it, as a running turn leaves it) is 409 and nothing is
/// copied, because a door on the copy would carry the turn on and spend; once the turn is settled the same chat forks. When
/// eidolon cannot say, it is 503 and nothing is copied. Mutations: skip the check; copy before checking.
#[cfg(feature = "test-bins")]
#[test]
fn a_chat_in_the_middle_of_a_turn_is_not_forked() {
    let (_base, home, perch) = start_home("");
    let sessions = home.join("sessions");
    std::fs::write(sessions.join("mid.eid"), chat_log(Some("hello"))).unwrap();
    let starts = || perch.fake_records().iter().filter(|l| l.starts_with("start ")).count();
    let answer = perch.post("/hub/sessions/mid/fork");
    assert_eq!(answer.status, 409, "{}{}", answer.head, answer.body);
    assert!(answer.body.contains("turn"), "{}", answer.body);
    assert_eq!(std::fs::read_dir(&sessions).unwrap().count(), 1, "nothing copied");
    assert_eq!(starts(), 0);
    std::fs::write(sessions.join("mid.eid"), settled_log("hello")).unwrap();
    assert_eq!(perch.post("/hub/sessions/mid/fork").status, 201);
}

#[cfg(feature = "test-bins")]
#[test]
fn a_fork_when_eidolon_cannot_say_is_503_and_copies_nothing() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("minerva.toml"), "default_model = \"mock\"\ndefault_mode = \"auto\"\n").unwrap();
    let home = home.canonicalize().unwrap();
    let extra: Vec<std::ffi::OsString> = vec!["--root".into(), base.path().canonicalize().unwrap().into(), "--home".into(), home.clone().into()];
    let perch = Perch::launch_with_member(true, Some(vec![("FAKE_HELPER_FAIL".into(), "1".into())]), &extra, Some(""));
    std::fs::write(home.join("sessions").join("src.eid"), settled_log("hello")).unwrap();
    assert_eq!(perch.post("/hub/sessions/src/fork").status, 503);
    assert_eq!(std::fs::read_dir(home.join("sessions")).unwrap().count(), 1);
}

// ---------------------------------------------------------------------------
// Names: birth names, a folder that follows the title, rename, promote (relocate.rs)
// ---------------------------------------------------------------------------

/// [`start_home`] with the stand-in's own knobs (`FAKE_SAY_RUNS`, ...).
#[cfg(feature = "test-bins")]
fn start_home_with(knobs: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, Perch) {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("minerva.toml"), "default_model = \"mock\"\ndefault_mode = \"auto\"\n").unwrap();
    let home = home.canonicalize().unwrap();
    let extra: Vec<std::ffi::OsString> = vec!["--root".into(), base.path().canonicalize().unwrap().into(), "--home".into(), home.clone().into()];
    let knobs = knobs.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect();
    let perch = Perch::launch_with_member(true, Some(knobs), &extra, Some(""));
    (base, home, perch)
}

/// Gives a live chat's log a first message, the way a turn would, with an mtime a few seconds on so `eidolon sessions` is asked again.
#[cfg(feature = "test-bins")]
fn say_in_log(home: &std::path::Path, id: &str, first: &str) {
    let log = home.join("sessions").join(format!("{id}.eid"));
    std::fs::write(&log, settled_log(first)).unwrap();
    let at = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::OpenOptions::new().write(true).open(&log).unwrap().set_modified(at).unwrap();
}

#[cfg(feature = "test-bins")]
fn cwd_of(perch: &Perch, id: &str) -> PathBuf {
    PathBuf::from(perch.row(id).unwrap_or_else(|| panic!("{id} is listed"))["cwd"].as_str().expect("a cwd"))
}

/// The list, polled until the chat's folder is `want` and it is live.
#[cfg(feature = "test-bins")]
fn wait_cwd(perch: &Perch, id: &str, want: &std::path::Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if perch.row(id).is_some_and(|row| row["cwd"].as_str() == want.to_str() && row["state"] == "live") {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(feature = "test-bins")]
fn starts_of(perch: &Perch) -> usize {
    perch.fake_records().iter().filter(|line| line.starts_with("start ")).count()
}

/// A new chat works in `scratch/<three-word-name>`; once it has a title and is between turns its folder is renamed to the title's slug
/// (`juice-shop-recon`), the door comes back on the SAME log with `--cwd <new>`, the model and `--yolo` it had, and the files go along.
/// Two chats with one title get `-2`. Mutations: leave the folder alone; reopen without `--cwd`; drop the model or yolo on the way.
#[cfg(feature = "test-bins")]
#[test]
fn a_new_chat_is_born_with_a_three_word_folder_and_follows_its_title_between_turns() {
    let (_base, home, perch) = start_home_with(&[]);
    let scratch = home.join("scratch");
    let id = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "model": "mock:alpha" })));
    let birth = cwd_of(&perch, &id);
    assert_eq!(birth.parent(), Some(scratch.as_path()));
    let name = birth.file_name().unwrap().to_str().unwrap().to_string();
    assert!(name.split('-').count() == 3 && name.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "a birth name: {name}");
    assert_eq!(perch.row(&id).unwrap()["title"], name.as_str(), "called by its birth name until it has a title");
    assert_eq!(perch.row(&id).unwrap()["inbox"], true);
    std::fs::write(birth.join("notes.txt"), "kept").unwrap();
    // Yolo on, so the way back has something to keep.
    assert_eq!(perch.post_json(&format!("/hub/sessions/{id}/restart"), serde_json::json!({ "yolo": true })).status, 200);
    let before = starts_of(&perch);

    say_in_log(&home, &id, "juice shop recon");
    let want = scratch.join("juice-shop-recon");
    assert!(wait_cwd(&perch, &id, &want, Duration::from_secs(12)), "the folder follows the title: {:?}", perch.row(&id));
    assert!(!birth.exists(), "the birth folder is gone");
    assert_eq!(std::fs::read_to_string(want.join("notes.txt")).unwrap(), "kept", "the files went with it");
    assert_eq!(perch.row(&id).unwrap()["title"], "juice shop recon");
    assert_eq!(starts_of(&perch), before + 1, "one reopen, and no more (a settled name is left alone)");
    let argv = perch.fake_argv();
    let log = home.join("sessions").join(format!("{id}.eid"));
    assert!(argv.contains("--yolo web "), "yolo kept: {argv}");
    assert!(argv.contains(&format!("--session {} --cwd {} -m mock:alpha", log.display(), want.display())), "same log, new folder, same model: {argv}");
    assert!(std::fs::read_to_string(home.join("chat-cwds.json")).unwrap().contains("juice-shop-recon"));
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(starts_of(&perch), before + 1, "settled: no further restart");

    // Another chat with the same title: its folder is `-2`.
    let other = body_id(&perch.post_json("/hub/sessions", serde_json::json!({})));
    say_in_log(&home, &other, "Juice shop RECON");
    assert!(wait_cwd(&perch, &other, &scratch.join("juice-shop-recon-2"), Duration::from_secs(12)), "{:?}", perch.row(&other));
    for id in [&id, &other] {
        assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    }
}

/// A running turn is never cut off for a folder: the rename waits for the turn to end. Mutation: rename while `turn-state` is running.
#[cfg(feature = "test-bins")]
#[test]
fn a_busy_chat_keeps_its_folder_until_the_turn_ends() {
    let (_base, home, perch) = start_home_with(&[("FAKE_SAY_RUNS", "1")]);
    let id = body_id(&perch.post_json("/hub/sessions", serde_json::json!({})));
    let birth = cwd_of(&perch, &id);
    assert_eq!(perch.post_json(&format!("/s/{id}/api/say"), serde_json::json!({ "text": "go" })).status, 202);
    assert!(wait_record(&perch, Duration::from_secs(3), |lines| lines.iter().any(|line| line == "turn-state true")));
    say_in_log(&home, &id, "slow recon");
    let starts = starts_of(&perch);
    std::thread::sleep(Duration::from_millis(3500));
    assert_eq!(cwd_of(&perch, &id), birth, "mid-turn: not touched");
    assert!(birth.is_dir() && starts_of(&perch) == starts);
    // A promote is a 409 for the same reason.
    let busy = perch.post_json(&format!("/hub/sessions/{id}/promote"), serde_json::json!({ "parent": home.parent().unwrap(), "name": "slow" }));
    assert_eq!(busy.status, 409, "{}{}", busy.head, busy.body);
    assert!(busy.body.contains("turn is running"), "{}", busy.body);
    assert!(birth.is_dir());
    assert_eq!(perch.post(&format!("/s/{id}/api/cancel")).status, 204);
    assert!(wait_cwd(&perch, &id, &home.join("scratch").join("slow-recon"), Duration::from_secs(12)), "{:?}", perch.row(&id));
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// Only inbox folders are renamed: a chat in a project folder, or in a scratch folder that is registered as a project, keeps its
/// folder when it is titled or renamed. Mutations: rename any live chat's folder; skip the project check.
#[cfg(feature = "test-bins")]
#[test]
fn project_folders_are_never_renamed() {
    let (base, home, perch) = start_home_with(&[]);
    let root = base.path().canonicalize().unwrap();
    let proj = root.join("my-app");
    std::fs::create_dir(&proj).unwrap();
    assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "path": proj })).status, 201);
    let in_project = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "cwd": proj })));
    let in_inbox = body_id(&perch.post_json("/hub/sessions", serde_json::json!({})));
    let inbox_dir = cwd_of(&perch, &in_inbox);
    assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "path": inbox_dir, "name": "kept" })).status, 201, "a scratch folder that is a project");
    say_in_log(&home, &in_project, "ship the thing");
    say_in_log(&home, &in_inbox, "ship the other thing");
    assert_eq!(perch.post_json(&format!("/hub/sessions/{in_project}/rename"), serde_json::json!({ "title": "Shipping" })).status, 200);
    std::thread::sleep(Duration::from_millis(3500));
    assert_eq!(cwd_of(&perch, &in_project), proj);
    assert_eq!(cwd_of(&perch, &in_inbox), inbox_dir, "a project in scratch is still a project");
    assert!(proj.is_dir() && inbox_dir.is_dir() && !home.join("scratch").join("ship-the-other-thing").exists());
    assert_eq!(perch.row(&in_project).unwrap()["title"], "Shipping");
    for id in [&in_project, &in_inbox] {
        assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    }
}

/// `POST /hub/sessions/<id>/rename`: the name is stored (0600), beats the derived title in the list, clears with an empty one, and
/// refuses bad names and shared-dir chats; a saved inbox chat's folder follows at once, and every way of reopening it (resume, restart,
/// fork) passes the new folder as `--cwd`. Mutations: ignore the stored name; rename no folder; reopen without `--cwd`.
#[cfg(feature = "test-bins")]
#[test]
fn a_rename_beats_the_title_and_the_folder_follows_and_every_reopen_passes_cwd() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_base, home, perch) = start_home_with(&[]);
    let scratch = home.join("scratch");
    let id = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "model": "mock:beta" })));
    let birth = cwd_of(&perch, &id);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(perch.wait_state(&id, "kept", Duration::from_secs(5)));
    say_in_log(&home, &id, "derived title");
    assert_eq!(perch.row(&id).unwrap()["title"], "derived title");

    let rename = |body: serde_json::Value| perch.post_json(&format!("/hub/sessions/{id}/rename"), body);
    let done = rename(serde_json::json!({ "title": "  Cozy Name  " }));
    assert_eq!(done.status, 200, "{}{}", done.head, done.body);
    assert_eq!(json_of(&done)["title"], "Cozy Name");
    assert_eq!(perch.row(&id).unwrap()["title"], "Cozy Name", "the custom name beats the derived one");
    assert_eq!(std::fs::metadata(home.join("names.json")).unwrap().permissions().mode() & 0o777, 0o600);
    let folder = scratch.join("cozy-name");
    assert_eq!(cwd_of(&perch, &id), folder, "a saved inbox chat's folder follows at once");
    assert!(folder.is_dir() && !birth.exists());
    assert_eq!(starts_of(&perch), 1, "a saved chat has no door to restart");

    // Every reopen passes the folder: resume, restart, and a fork's door.
    let log = home.join("sessions").join(format!("{id}.eid"));
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    let want = format!("--session {} --cwd {}", log.display(), folder.display());
    assert!(perch.fake_argv().contains(&want), "resume: {}", perch.fake_argv());
    assert_eq!(perch.post_json(&format!("/hub/sessions/{id}/restart"), serde_json::json!({ "model": "mock:beta", "yolo": true })).status, 200);
    assert!(perch.fake_argv().contains(&want), "restart: {}", perch.fake_argv());
    std::fs::write(&log, settled_log("derived title")).unwrap();
    let forked = body_id(&perch.post(&format!("/hub/sessions/{id}/fork")));
    let argv = perch.fake_records().into_iter().rev().find(|l| l.starts_with("argv ") && l.contains(&format!("{forked}.eid"))).expect("the fork's argv");
    assert!(argv.contains(&format!("--cwd {} ", folder.display())), "fork: {argv}");
    // The fork keeps the source's model and yolo, and is called `<title> (fork)`.
    assert!(argv.contains("--yolo web ") && argv.ends_with(" -m mock:beta"), "{argv}");
    assert_eq!(perch.row(&forked).unwrap()["title"], "Cozy Name (fork)");
    // They share one folder, so a rename of either moves it for both, and both doors come back in it.
    let before = starts_of(&perch);
    assert_eq!(perch.post_json(&format!("/hub/sessions/{forked}/rename"), serde_json::json!({ "title": "Forked Off" })).status, 200);
    let moved = scratch.join("forked-off");
    assert!(wait_cwd(&perch, &forked, &moved, Duration::from_secs(8)) && wait_cwd(&perch, &id, &moved, Duration::from_secs(8)), "{:?} {:?}", perch.row(&forked), perch.row(&id));
    assert!(moved.is_dir() && !folder.exists());
    assert_eq!(starts_of(&perch), before + 2, "each door came back once");
    let argv = perch.fake_argv();
    assert!(argv.contains(&format!("--cwd {}", moved.display())), "{argv}");
    // Two titles never take turns renaming a folder they share: the tidy loop leaves it be.
    std::thread::sleep(Duration::from_millis(3500));
    assert_eq!((cwd_of(&perch, &forked), cwd_of(&perch, &id)), (moved.clone(), moved.clone()));
    assert_eq!(starts_of(&perch), before + 2);

    // An empty name clears; a bad one is a 400; a shared-dir chat is read-only; no such chat is a 404.
    assert_eq!(json_of(&rename(serde_json::json!({ "title": "" })))["title"], "derived title");
    assert_eq!(rename(serde_json::json!({ "title": "a\nb" })).status, 400);
    assert_eq!(rename(serde_json::json!({ "title": "x".repeat(121) })).status, 400);
    assert_eq!(rename(serde_json::json!({ "name": "x" })).status, 400);
    assert_eq!(rename(serde_json::json!({ "title": "x".repeat(120) })).status, 200);
    std::fs::write(perch.shared().join("theirs.eid"), chat_log(Some("shared"))).unwrap();
    assert_eq!(perch.post_json("/hub/sessions/theirs/rename", serde_json::json!({ "title": "mine now" })).status, 403);
    assert_eq!(perch.post_json("/hub/sessions/nothing-here/rename", serde_json::json!({ "title": "x" })).status, 404);
    assert_eq!(perch.get(&format!("/hub/sessions/{id}/rename")).status, 404, "POST only");
    for id in [&id, &forked] {
        assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    }
}

/// `POST /hub/projects/rename`: relabels a project, never touches the folder; 404 unknown, 400 a bad name.
#[cfg(feature = "test-bins")]
#[test]
fn a_project_can_be_renamed_and_its_folder_is_left_alone() {
    let (base, _home, perch) = start_home_with(&[]);
    let proj = base.path().canonicalize().unwrap().join("my-app");
    std::fs::create_dir(&proj).unwrap();
    assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "path": proj })).status, 201);
    let done = perch.post_json("/hub/projects/rename", serde_json::json!({ "path": proj, "name": " Pretty App " }));
    assert_eq!(done.status, 200, "{}{}", done.head, done.body);
    assert_eq!(json_of(&done)["name"], "Pretty App");
    assert_eq!(json_of(&perch.get("/hub/projects"))[0]["name"], "Pretty App");
    assert!(proj.is_dir());
    assert_eq!(perch.post_json("/hub/projects/rename", serde_json::json!({ "path": "/nowhere", "name": "x" })).status, 404);
    assert_eq!(perch.post_json("/hub/projects/rename", serde_json::json!({ "path": proj, "name": "a\nb" })).status, 400);
    assert_eq!(perch.post_json("/hub/projects/rename", serde_json::json!({ "path": proj, "name": "  " })).status, 400);
    assert_eq!(perch.get("/hub/projects/rename").status, 404);
}

/// `POST /hub/sessions/<id>/promote`: the inbox folder is moved under a chosen parent, registered as a project, and the chat reopens in
/// it; refused for a chat that is not in the inbox, a parent outside the roots or inside the inbox, a target that exists, a bad name and
/// a shared folder. Mutations: copy instead of move; skip the project; skip any refusal.
#[cfg(feature = "test-bins")]
#[test]
fn a_promoted_chat_moves_into_a_project_and_reopens_there() {
    let (base, home, perch) = start_home_with(&[]);
    let root = base.path().canonicalize().unwrap();
    let projects = root.join("projects");
    std::fs::create_dir(&projects).unwrap();
    let id = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "model": "mock:alpha" })));
    let birth = cwd_of(&perch, &id);
    std::fs::create_dir(birth.join("loot")).unwrap();
    std::fs::write(birth.join("loot/a.txt"), "alpha").unwrap();
    let promote = |body: serde_json::Value| perch.post_json(&format!("/hub/sessions/{id}/promote"), body);

    // Refusals first: nothing may have moved.
    assert_eq!(promote(serde_json::json!({ "parent": "/etc", "name": "x" })).status, 422, "outside the roots");
    assert_eq!(promote(serde_json::json!({ "parent": home.join("scratch"), "name": "x" })).status, 422, "inside the inbox");
    assert_eq!(promote(serde_json::json!({ "parent": projects, "name": "a/b" })).status, 400);
    assert_eq!(promote(serde_json::json!({ "parent": projects, "name": ".." })).status, 400);
    assert_eq!(promote(serde_json::json!({ "parent": projects })).status, 400);
    std::fs::create_dir(projects.join("taken")).unwrap();
    assert_eq!(promote(serde_json::json!({ "parent": projects, "name": "taken" })).status, 409, "the target exists");
    assert!(birth.join("loot/a.txt").is_file() && perch.state_of(&id).as_deref() == Some("live"));
    assert_eq!(starts_of(&perch), 1, "a refusal stops nothing");

    let done = promote(serde_json::json!({ "parent": projects, "name": "recon-kit" }));
    assert_eq!(done.status, 200, "{}{}", done.head, done.body);
    let target = projects.join("recon-kit");
    assert_eq!(json_of(&done)["path"], target.to_str().unwrap());
    assert_eq!(json_of(&done)["restarted"], true);
    assert!(!birth.exists(), "moved, not copied");
    assert_eq!(std::fs::read_to_string(target.join("loot/a.txt")).unwrap(), "alpha");
    assert!(wait_cwd(&perch, &id, &target, Duration::from_secs(5)));
    let listed = json_of(&perch.get("/hub/projects"));
    assert_eq!(listed[0]["name"], "recon-kit");
    assert_eq!(listed[0]["path"], target.to_str().unwrap());
    let log = home.join("sessions").join(format!("{id}.eid"));
    assert!(perch.fake_argv().contains(&format!("--session {} --cwd {} -m mock:alpha", log.display(), target.display())), "{}", perch.fake_argv());
    assert_eq!(perch.row(&id).unwrap()["inbox"], false);
    // Now it works in a project folder: not an inbox chat any more, and its folder follows nothing.
    assert_eq!(promote(serde_json::json!({ "parent": projects, "name": "again" })).status, 409);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);

    // A chat in a folder of its own that was never in the inbox, and a shared-dir chat.
    let in_work = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string() })));
    assert_eq!(perch.post_json(&format!("/hub/sessions/{in_work}/promote"), serde_json::json!({ "parent": projects, "name": "w" })).status, 409);
    std::fs::write(perch.shared().join("theirs.eid"), chat_log(Some("shared"))).unwrap();
    assert_eq!(perch.post_json("/hub/sessions/theirs/promote", serde_json::json!({ "parent": projects, "name": "w" })).status, 403);

    // A fork shares its source's folder: promoting the source moves it for both, and the fork's door comes back there.
    let saved = body_id(&perch.post_json("/hub/sessions", serde_json::json!({})));
    let saved_dir = cwd_of(&perch, &saved);
    assert_eq!(perch.post(&format!("/hub/sessions/{saved}/stop")).status, 202);
    assert!(perch.wait_state(&saved, "kept", Duration::from_secs(5)));
    let fork = body_id(&perch.post(&format!("/hub/sessions/{saved}/fork")));
    let shared = perch.post_json(&format!("/hub/sessions/{saved}/promote"), serde_json::json!({ "parent": projects, "name": "shared" }));
    assert_eq!(shared.status, 200, "{}{}", shared.head, shared.body);
    let target = projects.join("shared");
    assert!(!saved_dir.exists() && target.is_dir(), "moved once, for both");
    assert_eq!(cwd_of(&perch, &saved), target);
    assert!(wait_cwd(&perch, &fork, &target, Duration::from_secs(8)), "{:?}", perch.row(&fork));
    assert!(perch.fake_argv().contains(&format!("--cwd {}", target.display())), "{}", perch.fake_argv());
    assert_eq!(perch.post(&format!("/hub/sessions/{fork}/stop")).status, 202);

    // A saved chat is promoted with no door to restart.
    std::fs::remove_dir(projects.join("taken")).unwrap();
    let lone = body_id(&perch.post_json("/hub/sessions", serde_json::json!({})));
    assert_eq!(perch.post(&format!("/hub/sessions/{lone}/stop")).status, 202);
    assert!(perch.wait_state(&lone, "kept", Duration::from_secs(5)));
    let before = starts_of(&perch);
    let moved = perch.post_json(&format!("/hub/sessions/{lone}/promote"), serde_json::json!({ "parent": projects, "name": "taken" }));
    assert_eq!(moved.status, 200, "{}{}", moved.head, moved.body);
    assert_eq!(json_of(&moved)["restarted"], false);
    assert_eq!(starts_of(&perch), before, "promoting a saved chat starts nothing");
    assert_eq!(cwd_of(&perch, &lone), projects.join("taken"));
    assert_eq!(perch.state_of(&lone).as_deref(), Some("kept"));
}

/// A fork of a chat starts with the source's model and yolo and is called `<title> (fork)`. Mutation: a fork with no `-m`.
#[cfg(feature = "test-bins")]
#[test]
fn a_fork_keeps_the_model_and_the_mode_and_is_named_for_its_source() {
    let (_base, home, perch) = start_home_with(&[]);
    let id = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "model": "mock:beta" })));
    assert_eq!(perch.post_json(&format!("/hub/sessions/{id}/restart"), serde_json::json!({ "yolo": true })).status, 200);
    say_in_log(&home, &id, "plain old title");
    let forked = body_id(&perch.post(&format!("/hub/sessions/{id}/fork")));
    let argv = perch.fake_records().into_iter().rev().find(|l| l.starts_with("argv ") && l.contains(&format!("{forked}.eid"))).unwrap();
    assert!(argv.contains("--yolo web ") && argv.ends_with(" -m mock:beta"), "{argv}");
    assert_eq!(perch.row(&forked).unwrap()["title"], "plain old title (fork)");
    // A saved source is forked on what this run remembers of it.
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(perch.wait_state(&id, "kept", Duration::from_secs(5)));
    let again = body_id(&perch.post(&format!("/hub/sessions/{id}/fork")));
    let argv = perch.fake_records().into_iter().rev().find(|l| l.starts_with("argv ") && l.contains(&format!("{again}.eid"))).unwrap();
    assert!(argv.contains("--yolo web ") && argv.ends_with(" -m mock:beta"), "{argv}");
    for id in [&forked, &again] {
        assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    }
}

// ---------------------------------------------------------------------------
// GET /hub/tree and /hub/file for a SAVED chat (read-only, same path checks)
// ---------------------------------------------------------------------------

#[cfg(feature = "test-bins")]
const SAVED_PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4];

/// A chat started in `dir` (under the roots) and stopped: kept, with its folder remembered.
#[cfg(feature = "test-bins")]
fn saved_in(perch: &Perch, dir: &std::path::Path) -> String {
    let id = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "cwd": dir })));
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(perch.wait_state(&id, "kept", Duration::from_secs(5)));
    id
}

/// A saved chat whose folder is known lists and serves an image exactly as a live one does, with the deny list still on.
/// Mutation: put back the 409 for a kept row in `tree::base` and the first two asserts go red.
#[cfg(feature = "test-bins")]
#[test]
fn a_saved_chat_tree_and_image_are_served_with_the_same_checks() {
    let (base, _home, perch) = start_home("");
    let proj = base.path().canonicalize().unwrap().join("proj");
    std::fs::create_dir(&proj).unwrap();
    std::fs::write(proj.join("shot.png"), SAVED_PNG).unwrap();
    std::fs::write(proj.join("secret.png"), SAVED_PNG).unwrap();
    std::fs::write(proj.join(".env"), "K=1").unwrap();
    std::fs::create_dir(proj.join(".git")).unwrap();
    std::fs::write(proj.join(".git/h.png"), SAVED_PNG).unwrap();
    let id = saved_in(&perch, &proj);

    let tree = perch.get(&format!("/hub/tree?session={id}&path="));
    assert_eq!(tree.status, 200, "{}{}", tree.head, tree.body);
    assert_eq!(Perch::tree_names(&tree), vec!["shot.png".to_string()], "denied names are still hidden");
    let file = perch.get(&format!("/hub/file?session={id}&path=shot.png"));
    assert_eq!(file.status, 200, "{}", file.head);
    assert_eq!(file.header("content-type").as_deref(), Some("image/png"));
    for path in ["secret.png", ".git%2Fh.png", "..%2Fx.png", "missing.png"] {
        assert_eq!(perch.get(&format!("/hub/file?session={id}&path={path}")).status, 404, "{path}");
    }
}

/// The folder is judged again on every request: gone or moved is a 404 with a sentence; a link that now leaves the roots too.
/// Mutation: skip `registry.tree_cwd` for a saved row and the outside case lists /tmp.
#[cfg(feature = "test-bins")]
#[test]
fn a_saved_chat_whose_folder_moved_or_left_the_roots_is_refused_with_a_sentence() {
    let (base, _home, perch) = start_home("");
    let root = base.path().canonicalize().unwrap();
    let (moved, left) = (root.join("moved"), root.join("left"));
    for dir in [&moved, &left] {
        std::fs::create_dir(dir).unwrap();
        std::fs::write(dir.join("shot.png"), SAVED_PNG).unwrap();
    }
    let (a, b) = (saved_in(&perch, &moved), saved_in(&perch, &left));
    assert_eq!(perch.get(&format!("/hub/tree?session={a}&path=")).status, 200);
    std::fs::rename(&moved, root.join("moved-away")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("shot.png"), SAVED_PNG).unwrap();
    std::fs::remove_dir_all(&left).unwrap();
    std::os::unix::fs::symlink(outside.path(), &left).unwrap();
    for id in [&a, &b] {
        for url in [format!("/hub/tree?session={id}&path="), format!("/hub/file?session={id}&path=shot.png")] {
            let answer = perch.get(&url);
            assert_eq!(answer.status, 404, "{url}: {}{}", answer.head, answer.body);
            assert!(answer.body.contains("folder"), "a sentence for the page: {}", answer.body);
        }
    }
}

/// A trashed chat is no row: 404. A saved chat from the shared dir whose folder the perch does not know is 409 with a sentence.
/// Mutation: answer 200/404 for a kept row with no cwd and the second half goes red.
#[cfg(feature = "test-bins")]
#[test]
fn a_trashed_saved_chat_is_404_and_one_with_no_known_folder_is_409() {
    let (base, home, perch) = start_home("");
    let proj = base.path().canonicalize().unwrap().join("proj");
    std::fs::create_dir(&proj).unwrap();
    let id = saved_in(&perch, &proj);
    assert_eq!(perch.get(&format!("/hub/tree?session={id}&path=")).status, 200);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/trash")).status, 204);
    assert_eq!(perch.get(&format!("/hub/tree?session={id}&path=")).status, 404);
    assert_eq!(perch.get(&format!("/hub/file?session={id}&path=shot.png")).status, 404);

    std::fs::write(home.join("sessions").join("nocwd.eid"), chat_log(Some("hello"))).unwrap();
    for url in ["/hub/tree?session=nocwd&path=", "/hub/file?session=nocwd&path=shot.png"] {
        let answer = perch.get(url);
        assert_eq!(answer.status, 409, "{url}: {}{}", answer.head, answer.body);
        assert!(answer.body.contains("isn't known") && answer.body.contains("kept"), "{}", answer.body);
    }
}

// ---------------------------------------------------------------------------
// Fork from a point (hub.md section 7c): a pending copy, then a first message that runs `eidolon resume --at`
// ---------------------------------------------------------------------------

/// Three turns, as eidolon writes them: ids 1/2/3 are the first (user, reply, settled), 4/5/6 the second, 7/8/9 the third; record 2 is a
/// reply that ended its turn, 5 too. Every record's parent is the one before.
#[cfg(feature = "test-bins")]
fn branchy_log() -> String {
    let said = |role: &str, text: &str| serde_json::json!({ "role": role, "content": [{ "type": "text", "text": text }] });
    let rec = |id: u64, kind: serde_json::Value| serde_json::json!({ "id": id, "parent": if id == 0 { serde_json::Value::Null } else { serde_json::json!(id - 1) }, "ts_ms": id, "kind": kind });
    let mut lines = vec![rec(0, serde_json::json!({ "SessionStart": { "model": "mock", "cwd": "/tmp", "system": null } }))];
    for (turn, first) in [(0u64, "one"), (1, "two"), (2, "three")] {
        let base = 1 + turn * 3;
        lines.push(rec(base, serde_json::json!({ "UserMessage": said("user", first) })));
        lines.push(rec(base + 1, serde_json::json!({ "AssistantMessage": said("assistant", &format!("answer {first}")) })));
        lines.push(rec(base + 2, serde_json::json!({ "TurnSettled": { "stop_reason": "EndTurn", "usage": {} } })));
    }
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// A saved chat in `perch.work` that ran with `mock:beta` and `--yolo`, whose log is [`branchy_log`]. Returns its id.
#[cfg(feature = "test-bins")]
fn branchy_chat(perch: &Perch, home: &std::path::Path) -> String {
    let id = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string(), "model": "mock:beta" })));
    assert_eq!(perch.post_json(&format!("/hub/sessions/{id}/restart"), serde_json::json!({ "yolo": true })).status, 200);
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(perch.wait_state(&id, "kept", Duration::from_secs(5)));
    std::fs::write(home.join("sessions").join(format!("{id}.eid")), branchy_log()).unwrap();
    id
}

#[cfg(feature = "test-bins")]
fn first_state(perch: &Perch, id: &str) -> serde_json::Value {
    json_of(&perch.get(&format!("/hub/sessions/{id}/first")))
}

/// The state of a fork's first message, polled until it is `want`.
#[cfg(feature = "test-bins")]
fn wait_first(perch: &Perch, id: &str, want: &str, within: Duration) -> serde_json::Value {
    let deadline = Instant::now() + within;
    loop {
        let state = first_state(perch, id);
        if state["state"] == want {
            return state;
        }
        assert!(Instant::now() < deadline, "the first message of {id} never reached {want}: {state}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(feature = "test-bins")]
fn transcript_ids(perch: &Perch, id: &str) -> Vec<u64> {
    let answer = perch.get(&format!("/hub/sessions/{id}/transcript"));
    assert_eq!(answer.status, 200, "{}", answer.head);
    answer.body.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["id"].as_u64().unwrap()).collect()
}

#[cfg(feature = "test-bins")]
fn resumes(perch: &Perch) -> Vec<String> {
    perch.fake_records().into_iter().filter(|line| line.starts_with("resume ")).collect()
}

/// A fork at a reply is a PENDING copy: byte for byte the source, no door, listed with `pending` and its source, shows only the chain up
/// to the point, and cannot be resumed, restarted or forked whole (a door on it would open at the end of the whole copy). The source is
/// untouched, and nothing was spent. A pending fork can be trashed. Mutations: start a door for it; serve the whole copy as its
/// transcript; let a resume through; touch the source.
#[cfg(feature = "test-bins")]
#[test]
fn a_fork_from_a_reply_is_a_pending_copy_with_no_door() {
    let (_base, home, perch) = start_home_with(&[]);
    let id = branchy_chat(&perch, &home);
    let log = home.join("sessions").join(format!("{id}.eid"));
    let before = sha(&log);
    let starts = starts_of(&perch);
    let made = perch.post_json(&format!("/hub/sessions/{id}/fork"), serde_json::json!({ "at": 2 }));
    assert_eq!(made.status, 201, "{}{}", made.head, made.body);
    let new = body_id(&made);
    let said = json_of(&made);
    assert_eq!((said["pending"].clone(), said["at"].clone(), said["from"].clone()), (true.into(), 2.into(), id.clone().into()));
    assert_eq!(sha(&home.join("sessions").join(format!("{new}.eid"))), before, "the copy is whole until its first message");
    assert_eq!(sha(&log), before, "the source is untouched");
    assert_eq!(starts_of(&perch), starts, "no door for a pending fork");
    assert!(resumes(&perch).is_empty(), "and no turn");
    let row = perch.row(&new).unwrap();
    assert_eq!((row["state"].clone(), row["pending"].clone(), row["fork_of"].clone()), ("kept".into(), true.into(), id.clone().into()), "{row}");
    assert_eq!(row["cwd"], perch.work.display().to_string(), "it works where its source does, so it sits in the source's group");
    assert!(row["title"].as_str().unwrap().ends_with(" (fork)"), "{row}");
    assert_eq!(perch.row(&id).unwrap()["pending"], false);
    assert_eq!(transcript_ids(&perch, &new), [0, 1, 2], "history up to and including the point, not the whole copy");
    assert_eq!(transcript_ids(&perch, &id), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9], "the source shows everything");
    assert_eq!(first_state(&perch, &new)["state"], "none");
    // Nothing may open a door on it, or fork it whole, or fork from it.
    for path in [format!("/hub/sessions/{new}/resume"), format!("/hub/sessions/{new}/fork")] {
        let refused = perch.post(&path);
        assert_eq!(refused.status, 409, "{path}: {}{}", refused.head, refused.body);
        assert!(refused.body.contains("first message"), "{}", refused.body);
    }
    assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/fork"), serde_json::json!({ "at": 2 })).status, 409);
    assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/restart"), serde_json::json!({ "yolo": true })).status, 409);
    assert_eq!(starts_of(&perch), starts);
    // `first` is only for a pending fork.
    assert_eq!(perch.post_json(&format!("/hub/sessions/{id}/first"), serde_json::json!({ "text": "x" })).status, 404);
    assert_eq!(perch.get(&format!("/hub/sessions/{id}/first")).status, 404);
    // Trash works on it, and it is still pending when it comes back.
    assert_eq!(perch.post(&format!("/hub/sessions/{new}/trash")).status, 204);
    assert!(perch.row(&new).is_none());
    assert_eq!(perch.post(&format!("/hub/trash/{new}/restore")).status, 204);
    assert_eq!(perch.row(&new).unwrap()["pending"], true);
}

/// The first message runs stock `eidolon --config C --yolo resume <copy> --at 2 --cwd <dir> -- <text>` (no `-m`: resume has none), with
/// stdin closed, and when it exits 0 with the turn settled the fork is an ordinary live chat: a door on the copy with the source's
/// model and mode, pending gone, its history the old branch plus the new turn; the original is unchanged. Mutations: a door before the
/// run; the message as a flag; no `--at`/`--cwd`/`--config`; leave the fork pending after it ran.
#[cfg(feature = "test-bins")]
#[test]
fn the_first_message_runs_resume_at_the_point_and_then_the_door_comes_up() {
    let (_base, home, perch) = start_home_with(&[]);
    let id = branchy_chat(&perch, &home);
    let log = home.join("sessions").join(format!("{id}.eid"));
    let before = sha(&log);
    let new = body_id(&perch.post_json(&format!("/hub/sessions/{id}/fork"), serde_json::json!({ "at": 2 })));
    let copy = home.join("sessions").join(format!("{new}.eid"));
    // Refusals first: an empty message, a bad body, an image type the door does not take. None of them runs anything.
    for body in [serde_json::json!({ "text": "   " }), serde_json::json!({ "words": "x" }), serde_json::json!({ "text": "x", "images": [{ "media_type": "image/bmp", "data": "AAAA" }] })] {
        assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/first"), body.clone()).status, 400, "{body}");
    }
    assert!(resumes(&perch).is_empty());
    let started = perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "-rf go on" }));
    assert_eq!(started.status, 202, "{}{}", started.head, started.body);
    let done = wait_first(&perch, &new, "done", Duration::from_secs(10));
    assert!(done["out"].as_str().unwrap().contains("echo: -rf go on"), "the run's output is kept for the page: {done}");
    let records = perch.fake_records();
    let ran: Vec<&String> = records.iter().filter(|l| l.starts_with("resume ")).collect();
    assert_eq!(ran.len(), 1, "{records:?}");
    let line = ran[0];
    let want = format!("config=true yolo=true log={} at=2 cwd={} images= text=-rf go on", copy.display(), perch.work.display());
    assert!(line.contains(&want), "{line}");
    assert!(!line.contains(" -m "), "resume takes no -m");
    let door = records.iter().position(|l| l.starts_with("argv ") && l.contains(&format!("{new}.eid"))).expect("a door for the copy");
    assert!(records.iter().position(|l| l.starts_with("resume ")).unwrap() < door, "the turn ran before the door came up");
    assert!(records[door].contains("--yolo web ") && records[door].contains(&format!("--cwd {}", perch.work.display())) && records[door].ends_with(" -m mock:beta"), "{}", records[door]);
    let row = perch.row(&new).unwrap();
    assert_eq!((row["state"].clone(), row["pending"].clone(), row["fork_of"].clone()), ("live".into(), false.into(), id.clone().into()), "{row}");
    assert_eq!(transcript_ids(&perch, &new), [0, 1, 2, 10, 11, 12], "the old branch up to the point, then the new turn");
    assert_eq!(sha(&log), before, "the original is unchanged");
    assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "again" })).status, 404, "it is not pending any more");
    assert_eq!(perch.post(&format!("/hub/sessions/{new}/stop")).status, 202);
    assert!(perch.wait_state(&new, "kept", Duration::from_secs(5)));
    assert_eq!(perch.row(&new).unwrap()["pending"], false);
    assert_eq!(transcript_ids(&perch, &new), [0, 1, 2, 10, 11, 12], "a saved fork shows its own branch too");
}

/// A run that fails (eidolon exits non-zero) leaves the fork pending with the sentence, appends nothing, starts no door, and can be
/// tried again; a turn that stops part way (no `TurnSettled`) is a failure too, because a door on it would carry the turn on by itself.
/// Mutations: bring the door up anyway; drop the pending mark on failure; ignore the settled check.
#[cfg(feature = "test-bins")]
#[test]
fn a_failed_first_message_keeps_the_fork_pending_and_can_be_retried() {
    let (_base, home, perch) = start_home_with(&[("FAKE_RESUME_FAIL", "3")]);
    let id = branchy_chat(&perch, &home);
    let new = body_id(&perch.post_json(&format!("/hub/sessions/{id}/fork"), serde_json::json!({ "at": 5 })));
    let copy = home.join("sessions").join(format!("{new}.eid"));
    let before = sha(&copy);
    let starts = starts_of(&perch);
    for round in 1..=2 {
        assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "try" })).status, 202, "round {round}");
        let failed = wait_first(&perch, &new, "failed", Duration::from_secs(10));
        let why = failed["error"].as_str().unwrap();
        assert!(why.contains("exited") && why.contains("provider refused"), "the sentence says what eidolon said: {why}");
        assert_eq!(resumes(&perch).len(), round);
    }
    assert_eq!((sha(&copy), starts_of(&perch)), (before, starts), "nothing appended, no door");
    assert_eq!(perch.row(&new).unwrap()["pending"], true);
    assert_eq!(transcript_ids(&perch, &new), [0, 1, 2, 3, 4, 5]);
    assert_eq!(perch.post(&format!("/hub/sessions/{new}/trash")).status, 204, "a failed fork can still be trashed");

    let (_base, home, perch) = start_home_with(&[("FAKE_RESUME_UNSETTLED", "1")]);
    let id = branchy_chat(&perch, &home);
    let new = body_id(&perch.post_json(&format!("/hub/sessions/{id}/fork"), serde_json::json!({ "at": 2 })));
    assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "half" })).status, 202);
    let failed = wait_first(&perch, &new, "failed", Duration::from_secs(10));
    assert!(failed["error"].as_str().unwrap().contains("did not finish"), "{failed}");
    assert_eq!(perch.row(&new).unwrap()["pending"], true);
    assert!(perch.fake_records().iter().all(|l| !(l.starts_with("argv ") && l.contains(&format!("{new}.eid")))), "no door on a log that ends mid-turn");
}

/// A first message that runs too long is killed (its process is gone) and the fork stays pending, with a sentence that says so.
/// While it runs, a second first message and a trash are refused. Mutations: no timeout; leave the child running; allow a second run.
#[cfg(feature = "test-bins")]
#[test]
fn a_first_message_that_runs_too_long_is_killed_and_a_running_one_blocks_the_rest() {
    let (_base, home, perch) = start_home_with(&[("FAKE_RESUME_SLEEP_MS", "30000"), ("MINERVA_PERCH_FIRST_MS", "1500")]);
    let id = branchy_chat(&perch, &home);
    let new = body_id(&perch.post_json(&format!("/hub/sessions/{id}/fork"), serde_json::json!({ "at": 2 })));
    assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "slow" })).status, 202);
    let running = wait_first(&perch, &new, "running", Duration::from_secs(5));
    assert!(running["elapsed_ms"].as_u64().is_some());
    assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "again" })).status, 409, "one run at a time");
    let trash = perch.post(&format!("/hub/sessions/{new}/trash"));
    assert_eq!(trash.status, 409, "{}{}", trash.head, trash.body);
    let failed = wait_first(&perch, &new, "failed", Duration::from_secs(10));
    assert!(failed["error"].as_str().unwrap().contains("longer than"), "{failed}");
    let line = resumes(&perch).pop().expect("a resume was run");
    let pid: u32 = line.split_whitespace().find_map(|w| w.strip_prefix("pid=")).unwrap().parse().unwrap();
    assert!(wait_gone(pid, Duration::from_secs(5)), "the timed-out eidolon was killed, not left running");
    assert_eq!(perch.row(&new).unwrap()["pending"], true);
    assert_eq!(perch.post(&format!("/hub/sessions/{new}/trash")).status, 204);
}

/// Images in the first message go to `--image <file>` (written `0600` in a folder of the home, gone when the run ends); the unsupported
/// kinds and too many are a 400 with nothing run. Mutations: drop the images; leave the files behind; pass base64 in argv.
#[cfg(feature = "test-bins")]
#[test]
fn a_first_message_can_carry_images_as_files_that_do_not_outlive_the_run() {
    let (_base, home, perch) = start_home_with(&[]);
    let id = branchy_chat(&perch, &home);
    let new = body_id(&perch.post_json(&format!("/hub/sessions/{id}/fork"), serde_json::json!({ "at": 2 })));
    let pics: Vec<serde_json::Value> = (0..9).map(|_| serde_json::json!({ "media_type": "image/png", "data": "QUJD" })).collect();
    assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "x", "images": pics })).status, 400, "nine is too many");
    assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "x", "images": [{ "media_type": "image/png", "data": "not base64!" }] })).status, 400);
    assert!(resumes(&perch).is_empty());
    let sent = perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "look", "images": [{ "media_type": "image/png", "data": "QUJD" }, { "media_type": "image/webp", "data": "QUJDRA==" }] }));
    assert_eq!(sent.status, 202, "{}{}", sent.head, sent.body);
    wait_first(&perch, &new, "done", Duration::from_secs(10));
    let line = resumes(&perch).pop().unwrap();
    assert!(line.contains(" images=3,4 text=look"), "the fake read both files by the paths it was given: {line}");
    assert!(!home.join(format!("first-{new}")).exists(), "the picture files are gone");
}

/// A folder several chats share moves for ALL of them: renaming the source moves the folder under its fork (live) and under a pending
/// fork (only its remembered folder changes), the live ones come back with `--cwd <new>`, the same model and mode; a chat that is
/// mid-turn among them refuses the rename, by name, and nothing moves or is renamed. Mutations: refuse a shared folder as before; move
/// it for the asked chat only; ignore a busy sharer.
#[cfg(feature = "test-bins")]
#[test]
fn renaming_a_shared_folder_moves_it_for_every_chat_and_refuses_when_one_is_busy() {
    let (_base, home, perch) = start_home_with(&[("FAKE_SAY_RUNS", "1")]);
    let scratch = home.join("scratch");
    let src = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "model": "mock:alpha" })));
    let birth = cwd_of(&perch, &src);
    assert_eq!(perch.post_json(&format!("/hub/sessions/{src}/restart"), serde_json::json!({ "yolo": true })).status, 200);
    // The log the forks copy: settled, with records (the stand-in door's own log has none).
    std::fs::write(home.join("sessions").join(format!("{src}.eid")), branchy_log()).unwrap();
    let fork = body_id(&perch.post(&format!("/hub/sessions/{src}/fork")));
    assert_eq!(cwd_of(&perch, &fork), birth, "a fork shares its source's folder");
    // A fork from a point beside them: saved, never had a door.
    let pending = body_id(&perch.post_json(&format!("/hub/sessions/{src}/fork"), serde_json::json!({ "at": 2 })));
    assert_eq!(cwd_of(&perch, &pending), birth);

    // A turn in the fork: the rename of the SOURCE is refused and says whose turn it is, and nothing changed.
    assert_eq!(perch.post_json(&format!("/s/{fork}/api/say"), serde_json::json!({ "text": "work" })).status, 202);
    assert!(wait_record(&perch, Duration::from_secs(3), |lines| lines.iter().any(|line| line == "turn-state true")));
    let busy = perch.post_json(&format!("/hub/sessions/{src}/rename"), serde_json::json!({ "title": "Moved Together" }));
    assert_eq!(busy.status, 409, "{}{}", busy.head, busy.body);
    assert!(busy.body.contains("(fork)") && busy.body.contains("wait for it to finish"), "it names the chat to wait for: {}", busy.body);
    assert_eq!(cwd_of(&perch, &src), birth);
    assert!(birth.is_dir() && !scratch.join("moved-together").exists());
    assert_ne!(perch.row(&src).unwrap()["title"], "Moved Together", "the name was not kept either");
    assert_eq!(perch.post(&format!("/s/{fork}/api/cancel")).status, 204);
    assert!(wait_record(&perch, Duration::from_secs(3), |lines| lines.iter().any(|line| line == "turn-state false")));

    let before = starts_of(&perch);
    let done = perch.post_json(&format!("/hub/sessions/{src}/rename"), serde_json::json!({ "title": "Moved Together" }));
    assert_eq!(done.status, 200, "{}{}", done.head, done.body);
    let moved = scratch.join("moved-together");
    assert!(moved.is_dir() && !birth.exists(), "the folder moved once");
    for id in [&src, &fork] {
        assert!(wait_cwd(&perch, id, &moved, Duration::from_secs(8)), "{id}: {:?}", perch.row(id));
    }
    assert_eq!(cwd_of(&perch, &pending), moved, "a pending fork's remembered folder follows");
    assert_eq!(perch.row(&pending).unwrap()["state"], "kept");
    assert_eq!(starts_of(&perch), before + 2, "the two live doors came back, the pending one has none");
    let argv: Vec<String> = perch.fake_records().into_iter().filter(|l| l.starts_with("argv ")).rev().take(2).collect();
    assert!(argv.iter().all(|l| l.contains(&format!("--cwd {}", moved.display())) && l.contains("--yolo web ")), "{argv:?}");
    assert!(argv.iter().any(|l| l.contains(&format!("{src}.eid")) && l.ends_with(" -m mock:alpha")), "{argv:?}");
    // The pending fork still opens where its source now works: its first message is run with the new folder.
    assert_eq!(perch.post_json(&format!("/hub/sessions/{pending}/first"), serde_json::json!({ "text": "go" })).status, 202);
    wait_first(&perch, &pending, "done", Duration::from_secs(10));
    assert!(resumes(&perch).pop().unwrap().contains(&format!(" cwd={} ", moved.display())));
    for id in [&src, &fork, &pending] {
        assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    }
}

/// Promoting a shared inbox folder moves it for every chat that uses it, and a chat mid-turn among them refuses it, by name.
/// Mutations: refuse a shared folder as before; move it for the asked chat only.
#[cfg(feature = "test-bins")]
#[test]
fn promoting_a_shared_folder_moves_every_chat_and_waits_for_a_busy_one() {
    let (base, home, perch) = start_home_with(&[("FAKE_SAY_RUNS", "1")]);
    let projects = base.path().canonicalize().unwrap().join("projects");
    std::fs::create_dir(&projects).unwrap();
    let src = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "model": "mock:alpha" })));
    let birth = cwd_of(&perch, &src);
    std::fs::write(home.join("sessions").join(format!("{src}.eid")), branchy_log()).unwrap();
    assert_eq!(perch.post(&format!("/hub/sessions/{src}/stop")).status, 202);
    assert!(perch.wait_state(&src, "kept", Duration::from_secs(5)));
    std::fs::write(home.join("sessions").join(format!("{src}.eid")), branchy_log()).unwrap();
    let fork = body_id(&perch.post(&format!("/hub/sessions/{src}/fork")));
    assert_eq!(perch.post_json(&format!("/s/{fork}/api/say"), serde_json::json!({ "text": "work" })).status, 202);
    assert!(wait_record(&perch, Duration::from_secs(3), |lines| lines.iter().any(|line| line == "turn-state true")));
    let promote = |name: &str| perch.post_json(&format!("/hub/sessions/{src}/promote"), serde_json::json!({ "parent": projects, "name": name }));
    let busy = promote("both");
    assert_eq!(busy.status, 409, "{}{}", busy.head, busy.body);
    assert!(busy.body.contains("same folder") && busy.body.contains("wait for it to finish"), "{}", busy.body);
    assert!(birth.is_dir() && !projects.join("both").exists());
    assert_eq!(perch.post(&format!("/s/{fork}/api/cancel")).status, 204);
    assert!(wait_record(&perch, Duration::from_secs(3), |lines| lines.iter().any(|line| line == "turn-state false")));
    let done = promote("both");
    assert_eq!(done.status, 200, "{}{}", done.head, done.body);
    let target = projects.join("both");
    assert!(target.is_dir() && !birth.exists());
    assert_eq!(cwd_of(&perch, &src), target);
    assert!(wait_cwd(&perch, &fork, &target, Duration::from_secs(8)), "{:?}", perch.row(&fork));
    assert_eq!(json_of(&done)["restarted"], false, "the source was saved: no door to bring back");
    assert_eq!(perch.post(&format!("/hub/sessions/{fork}/stop")).status, 202);
}

// ---------------------------------------------------------------------------
// The second review round (2026-10-06): unsettled doors, model order, runs that outlive the perch, process groups, the home's own folders
// ---------------------------------------------------------------------------

/// Writes `text` as a chat's log with an mtime a few seconds on, so `eidolon sessions` is asked again.
#[cfg(feature = "test-bins")]
fn write_log(home: &std::path::Path, id: &str, text: &str) {
    let log = home.join("sessions").join(format!("{id}.eid"));
    std::fs::write(&log, text).unwrap();
    let at = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::OpenOptions::new().write(true).open(&log).unwrap().set_modified(at).unwrap();
}

/// A live door whose log ends mid-turn (a message unanswered: a provider error, a cancel) is never stopped to move its folder or to
/// switch its model: reopening that log would carry the turn on by itself, a paid turn nobody asked for. The tidy loop skips it
/// quietly, promote and restart are a 409 with a sentence, and nothing is started or moved; once the log is settled the folder follows.
/// Mutations: drop the settled check in `relocate` (the folder moves and a door is started); drop it in `restart`.
#[cfg(feature = "test-bins")]
#[test]
fn a_door_whose_log_ended_mid_turn_is_not_stopped_to_be_moved_or_restarted() {
    let (_base, home, perch) = start_home_with(&[]);
    let id = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "model": "mock:alpha" })));
    let birth = cwd_of(&perch, &id);
    write_log(&home, &id, &chat_log(Some("slow recon")));
    let starts = starts_of(&perch);
    std::thread::sleep(Duration::from_millis(3500));
    assert_eq!(cwd_of(&perch, &id), birth, "the tidy loop leaves an unsettled chat's folder alone");
    assert_eq!(starts_of(&perch), starts, "and starts no door on its log");
    let promote = perch.post_json(&format!("/hub/sessions/{id}/promote"), serde_json::json!({ "parent": home.parent().unwrap(), "name": "slow" }));
    assert_eq!(promote.status, 409, "{}{}", promote.head, promote.body);
    assert!(promote.body.contains("ended mid-turn"), "{}", promote.body);
    let restart = perch.post_json(&format!("/hub/sessions/{id}/restart"), serde_json::json!({ "yolo": true }));
    assert_eq!(restart.status, 409, "{}{}", restart.head, restart.body);
    assert!(restart.body.contains("ended mid-turn"), "{}", restart.body);
    assert!(birth.is_dir() && starts_of(&perch) == starts, "nothing moved, nothing reopened");
    // Settled again: the same chat is moved as before.
    say_in_log(&home, &id, "slow recon");
    assert!(wait_cwd(&perch, &id, &home.join("scratch").join("slow-recon"), Duration::from_secs(15)), "{:?}", perch.row(&id));
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// A new chat that names no model gets the Minerva config's: no `-m` in the door's argv at all. A bad model name is a 400 and starts nothing.
#[cfg(feature = "test-bins")]
#[test]
fn a_new_chat_with_no_model_passes_none_and_a_bad_model_is_a_400() {
    let (_base, _home, perch) = start_home_with(&[]);
    let starts = starts_of(&perch);
    let bad = perch.post_json("/hub/sessions", serde_json::json!({ "model": "mock; rm -rf" }));
    assert_eq!(bad.status, 400, "{}{}", bad.head, bad.body);
    assert_eq!(starts_of(&perch), starts);
    let id = body_id(&perch.post_json("/hub/sessions", serde_json::json!({})));
    let argv = perch.fake_argv();
    assert!(!argv.contains(" -m ") && !argv.contains("--model"), "the config's default_model decides, not the request: {argv}");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// The Minerva home is a root so the inbox works, but its own folders are not a chat's folder, a project, a place to make folders, or
/// something the picker lists; `scratch/<chat>` still is. Mutation: drop `is_home_internal` from `cwd_under_a_root`.
#[cfg(feature = "test-bins")]
#[test]
fn the_minerva_homes_own_folders_are_not_offered() {
    let (_base, home, perch) = start_home_with(&[]);
    for dir in [home.clone(), home.join("sessions"), home.join("scratch")] {
        let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": dir }));
        assert_eq!(answer.status, 422, "{}: {}{}", dir.display(), answer.head, answer.body);
        assert_eq!(perch.post_json("/hub/projects", serde_json::json!({ "path": dir })).status, 422, "{}", dir.display());
    }
    assert_eq!(perch.get(&format!("/hub/dirs?path={}", home.join("sessions").display())).status, 404, "not listed");
    assert_eq!(perch.get(&format!("/hub/dirs?path={}", home.display())).status, 404);
    assert_eq!(perch.post_json("/hub/dirs", serde_json::json!({ "parent": home, "name": "mine" })).status, 422);
    assert!(!home.join("mine").exists());
    let made = home.join("scratch").join("a-chat-folder");
    std::fs::create_dir_all(&made).unwrap();
    let id = body_id(&perch.post_json("/hub/sessions", serde_json::json!({ "cwd": made })));
    assert_eq!(perch.get(&format!("/hub/dirs?path={}", home.join("scratch").display())).status, 200, "scratch is listed, to reach its chats");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

#[cfg(feature = "test-bins")]
fn pgid_of(pid: u32) -> u32 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_else(|e| panic!("/proc/{pid}/stat: {e}"));
    stat.rsplit_once(')').unwrap().1.split_whitespace().nth(2).unwrap().parse().unwrap()
}

/// The pid a fork's first-message run reported (`resume pid=<n> ...`), waited for.
#[cfg(feature = "test-bins")]
fn run_pid(perch: &Perch) -> u32 {
    let line = (0..50).find_map(|_| resumes(perch).pop().or_else(|| { std::thread::sleep(Duration::from_millis(100)); None })).expect("a resume");
    line.split_whitespace().find_map(|w| w.strip_prefix("pid=")).unwrap().parse().unwrap()
}

/// A door and a fork's first-message run are each their own process group, so a stop or a timeout can signal the group and the tools
/// they started die too. Mutation: drop `process_group(0)` in `spawn::fork` or in `pending::ran`.
#[cfg(feature = "test-bins")]
#[test]
fn a_door_and_a_first_message_run_are_their_own_process_groups() {
    let (_base, home, perch) = start_home_with(&[("FAKE_RESUME_SLEEP_MS", "6000")]);
    let id = branchy_chat(&perch, &home);
    let live = body_id(&perch.post_json("/hub/sessions", serde_json::json!({})));
    let (door, _) = perch.fake_last_start();
    assert_eq!(pgid_of(door), door, "a door leads its own group");
    assert_eq!(perch.post(&format!("/hub/sessions/{live}/stop")).status, 202);
    let new = body_id(&perch.post_json(&format!("/hub/sessions/{id}/fork"), serde_json::json!({ "at": 2 })));
    assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "slow" })).status, 202);
    wait_first(&perch, &new, "running", Duration::from_secs(5));
    let pid = run_pid(&perch);
    assert_eq!(pgid_of(pid), pid, "the run leads its own group");
}

/// forks.json as a map.
#[cfg(feature = "test-bins")]
fn forks_json(home: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(home.join("forks.json")).unwrap()).unwrap()
}

/// A second perch on the same home as one that was killed, with the stand-in's own knobs.
#[cfg(feature = "test-bins")]
fn relaunch(base: &tempfile::TempDir, home: &std::path::Path, knobs: &[(&str, &str)]) -> Perch {
    let extra: Vec<std::ffi::OsString> = vec!["--root".into(), base.path().canonicalize().unwrap().into(), "--home".into(), home.to_path_buf().into()];
    let knobs = knobs.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect();
    Perch::launch_with_member(true, Some(knobs), &extra, Some(""))
}

/// A first message under way: the pending fork, the run's pid, and the perch killed with `kill -9` while the run goes on.
#[cfg(feature = "test-bins")]
fn orphaned_first_message(knobs: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, String, u32, Perch) {
    let (base, home, perch) = start_home_with(knobs);
    let id = branchy_chat(&perch, &home);
    let new = body_id(&perch.post_json(&format!("/hub/sessions/{id}/fork"), serde_json::json!({ "at": 2 })));
    assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "while away" })).status, 202);
    wait_first(&perch, &new, "running", Duration::from_secs(5));
    let pid = run_pid(&perch);
    let running = forks_json(&home)[&new]["running"].clone();
    assert_eq!(running["pid"], pid, "the run is written down while it runs: {running}");
    assert_eq!(running["base"], 9, "with the highest record the copy had");
    assert_eq!(unsafe { libc::kill(perch.child.id() as i32, libc::SIGKILL) }, 0, "kill -9 the perch");
    assert!(!is_gone(pid), "the run outlives the perch");
    // Returned so its work folder (where the run was told to work) outlives the run.
    (base, home, new, pid, perch)
}

/// The perch dies while a first message runs and a new one starts on the same home: the run is still going, so the fork reads `running`,
/// a second first message is a 409, and when the run ends having written a settled turn the fork is an ordinary saved chat, with no
/// door brought up and nothing continued. Mutations: keep `running` in memory only (the restart says `none` and lets a second run in).
#[cfg(feature = "test-bins")]
#[test]
fn a_run_that_outlives_the_perch_is_found_and_settled_without_bringing_a_door_up() {
    let (base, home, new, pid, _first) = orphaned_first_message(&[("FAKE_RESUME_SLEEP_MS", "5000")]);
    let perch = relaunch(&base, &home, &[]);
    let state = first_state(&perch, &new);
    assert_eq!(state["state"], "running", "{state}");
    let again = perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "a second one" }));
    assert_eq!(again.status, 409, "{}{}", again.head, again.body);
    assert!(resumes(&perch).is_empty(), "no second run");
    assert!(wait_gone(pid, Duration::from_secs(20)));
    wait_first(&perch, &new, "none", Duration::from_secs(10));
    let row = perch.row(&new).unwrap();
    assert_eq!((row["pending"].clone(), row["state"].clone()), (false.into(), "kept".into()), "{row}");
    assert_eq!(starts_of(&perch), 0, "nothing was brought up");
    assert!(resumes(&perch).is_empty());
    assert!(forks_json(&home)[&new].get("running").is_none(), "the mark is cleared");
}

/// The run is dead when the perch comes back, and wrote a turn that stopped part way: the fork says so, stays pending, and nothing is
/// continued (no door, no resume). Mutation: bring a door up on it, or settle it.
#[cfg(feature = "test-bins")]
#[test]
fn a_run_that_died_part_way_is_failed_with_a_sentence_and_never_continued() {
    let (base, home, new, pid, _first) = orphaned_first_message(&[("FAKE_RESUME_SLEEP_MS", "1500"), ("FAKE_RESUME_UNSETTLED", "1")]);
    assert!(wait_gone(pid, Duration::from_secs(10)));
    let perch = relaunch(&base, &home, &[]);
    let failed = wait_first(&perch, &new, "failed", Duration::from_secs(10));
    assert!(failed["error"].as_str().unwrap().contains("did not finish"), "{failed}");
    assert_eq!(perch.row(&new).unwrap()["pending"], true);
    assert_eq!(starts_of(&perch), 0);
    assert!(resumes(&perch).is_empty());
}

/// The run was killed before it wrote anything: the fork is simply pending again (state none) and a new first message can be sent.
#[cfg(feature = "test-bins")]
#[test]
fn a_run_that_wrote_nothing_leaves_the_fork_pending_and_free_to_try_again() {
    let (base, home, new, pid, _first) = orphaned_first_message(&[("FAKE_RESUME_SLEEP_MS", "30000")]);
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGKILL) }, 0);
    assert!(wait_gone(pid, Duration::from_secs(10)));
    let perch = relaunch(&base, &home, &[]);
    wait_first(&perch, &new, "none", Duration::from_secs(10));
    assert_eq!(perch.row(&new).unwrap()["pending"], true);
    assert_eq!(perch.post_json(&format!("/hub/sessions/{new}/first"), serde_json::json!({ "text": "again" })).status, 202);
    wait_first(&perch, &new, "done", Duration::from_secs(10));
}

// ---------------------------------------------------------------------------
// Approval modes: manual, auto, yolo (mode.rs)
// ---------------------------------------------------------------------------

/// A home whose `minerva.toml` is exactly `minerva` (plus the mock model), over a member config of `member`.
#[cfg(feature = "test-bins")]
fn start_modes(minerva: &str, member: &str) -> (tempfile::TempDir, PathBuf, Perch) {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("minerva.toml"), format!("default_model = \"mock\"\n{minerva}")).unwrap();
    let home = home.canonicalize().unwrap();
    let perch = Perch::start_hub_member(&["--root".into(), base.path().canonicalize().unwrap().into(), "--home".into(), home.clone().into()], member);
    (base, home, perch)
}

/// A new chat in `mode` (`None`: the request says nothing), and the argv its door got.
#[cfg(feature = "test-bins")]
fn new_in(perch: &Perch, mode: Option<&str>) -> (String, String) {
    let mut body = serde_json::json!({ "cwd": perch.work.display().to_string(), "model": "mock" });
    if let Some(mode) = mode {
        body["mode"] = mode.into();
    }
    let made = perch.post_json("/hub/sessions", body);
    assert_eq!(made.status, 201, "{}{}", made.head, made.body);
    (body_id(&made), perch.fake_argv())
}

/// The three modes are three argvs: manual is the generated `config-manual.toml` and no `--yolo`; auto is `config.toml` and no
/// `--yolo`; yolo is `config.toml` and `--yolo`. A request that says nothing gets `default_mode` from `<home>/minerva.toml`, and with
/// no such key that is yolo. The list says which mode each chat is in. Mutations: manual passes `--yolo` or `config.toml`; auto
/// passes `--yolo`; the default is auto; the list leaves the mode out; a bad word is accepted.
#[cfg(feature = "test-bins")]
#[test]
fn each_mode_is_its_own_config_and_yolo_flag_and_the_default_is_yolo() {
    let (_base, home, perch) = start_modes("", "");
    let (config, manual) = (home.join("config.toml"), home.join("config-manual.toml"));
    let (yolo_id, argv) = new_in(&perch, None);
    assert!(argv.starts_with(&format!("argv --config {} --yolo web ", config.display())), "no mode and no default_mode is yolo: {argv}");
    let (auto_id, argv) = new_in(&perch, Some("auto"));
    assert!(argv.starts_with(&format!("argv --config {} web ", config.display())), "auto is eidolon's own rules: {argv}");
    let (manual_id, argv) = new_in(&perch, Some("manual"));
    assert!(argv.starts_with(&format!("argv --config {} web ", manual.display())), "manual is the generated config, no --yolo: {argv}");
    assert!(!argv.contains("--yolo"), "{argv}");
    let modes: Vec<String> = [&yolo_id, &auto_id, &manual_id].iter().map(|id| perch.row(id).unwrap()["mode"].as_str().unwrap().to_string()).collect();
    assert_eq!(modes, ["yolo", "auto", "manual"]);
    let bad = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string(), "mode": "loud" }));
    assert_eq!(bad.status, 400, "{}{}", bad.head, bad.body);
    assert_eq!(starts_of(&perch), 3, "a refused request starts nothing");
    for id in [yolo_id, auto_id, manual_id] {
        assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    }
}

/// `default_mode` in `<home>/minerva.toml` is what a new chat gets when it names no mode; a value that is not one of the three
/// refuses the start rather than falling back to something looser. Mutations: ignore the key; fall back on a bad value.
#[cfg(feature = "test-bins")]
#[test]
fn default_mode_in_minerva_toml_decides_and_a_bad_one_refuses_the_start() {
    let (_base, home, perch) = start_modes("default_mode = \"manual\"\n", "");
    let (id, argv) = new_in(&perch, None);
    assert!(argv.starts_with(&format!("argv --config {} web ", home.join("config-manual.toml").display())), "{argv}");
    assert_eq!(perch.row(&id).unwrap()["mode"], "manual");
    let (id2, argv) = new_in(&perch, Some("yolo"));
    assert!(argv.contains(" --yolo web "), "a request beats the default: {argv}");
    for id in [id, id2] {
        assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    }
    for bad in ["default_mode = \"loud\"\n", "default_mode = true\n"] {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("Minerva");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("minerva.toml"), bad).unwrap();
        let out = Perch::refused_start(&["--home".into(), home.clone().into(), "--root".into(), base.path().into()]);
        assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("default_mode"), "{bad}: {out:?}");
    }
}

/// The restart route takes `{mode}`; `{yolo: true}` is yolo and `{yolo: false}` is auto (the old spelling); `mode` wins when both are
/// there; the answer carries `mode` and `yolo`; the same mode again is no restart; a word that is not a mode is a 400. The model is
/// kept across a mode change. Mutations: `{yolo:false}` meaning manual; manual restarted with `config.toml`; no-op restarted.
#[cfg(feature = "test-bins")]
#[test]
fn a_restart_moves_a_chat_between_the_three_modes() {
    let (_base, home, perch) = start_modes("default_mode = \"auto\"\n", "");
    let (id, _) = new_in(&perch, None);
    let restart = |body: serde_json::Value| perch.post_json(&format!("/hub/sessions/{id}/restart"), body);
    let (config, manual) = (home.join("config.toml"), home.join("config-manual.toml"));
    let step = |body: serde_json::Value, mode: &str, argv_start: String| {
        let answer = restart(body.clone());
        assert_eq!(answer.status, 200, "{body}: {}{}", answer.head, answer.body);
        let said = json_of(&answer);
        assert_eq!((said["mode"].as_str(), said["yolo"].as_bool(), said["restarted"].as_bool()), (Some(mode), Some(mode == "yolo"), Some(true)), "{said}");
        let argv = perch.fake_argv();
        assert!(argv.starts_with(&argv_start) && argv.ends_with(" -m mock"), "{body}: {argv}");
        assert_eq!(perch.row(&id).unwrap()["mode"], mode);
    };
    step(serde_json::json!({ "mode": "manual" }), "manual", format!("argv --config {} web ", manual.display()));
    step(serde_json::json!({ "mode": "yolo" }), "yolo", format!("argv --config {} --yolo web ", config.display()));
    step(serde_json::json!({ "yolo": false }), "auto", format!("argv --config {} web ", config.display()));
    step(serde_json::json!({ "yolo": true }), "yolo", format!("argv --config {} --yolo web ", config.display()));
    step(serde_json::json!({ "mode": "manual", "yolo": true }), "manual", format!("argv --config {} web ", manual.display()));
    let starts = starts_of(&perch);
    let same = restart(serde_json::json!({ "mode": "manual" }));
    assert!(same.status == 200 && same.body.contains("\"restarted\":false"), "{}{}", same.head, same.body);
    assert_eq!(starts_of(&perch), starts, "the mode it already has restarts nothing");
    for bad in [serde_json::json!({ "mode": "gated" }), serde_json::json!({}), serde_json::json!({ "mode": 3 })] {
        assert_eq!(restart(bad.clone()).status, 400, "{bad}");
    }
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// A reopened chat comes back in the mode it had (kept in `<home>/chat-modes.json`, 0600, so a perch restart does not loosen it),
/// a whole fork starts in its source's mode, and a chat with no remembered mode gets `default_mode`. Mutations: resume with the
/// default; fork with the default; do not write the file.
#[cfg(feature = "test-bins")]
#[test]
fn a_reopened_chat_and_a_fork_keep_the_mode_they_had() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_base, home, perch) = start_modes("default_mode = \"yolo\"\n", "");
    let manual = home.join("config-manual.toml");
    let (id, _) = new_in(&perch, Some("manual"));
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(perch.wait_state(&id, "kept", Duration::from_secs(5)));
    assert_eq!(perch.row(&id).unwrap()["mode"], "manual", "a saved chat says the mode it last had");
    let file = home.join("chat-modes.json");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    let kept: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(kept[&id], "manual");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/resume")).status, 200);
    let argv = perch.fake_argv();
    assert!(argv.starts_with(&format!("argv --config {} web ", manual.display())) && !argv.contains("--yolo"), "reopened in manual, not the default: {argv}");
    // A whole fork of a settled chat.
    std::fs::write(home.join("sessions").join(format!("{id}.eid")), settled_log("hello")).unwrap();
    let forked = body_id(&perch.post(&format!("/hub/sessions/{id}/fork")));
    let argv = perch.fake_records().into_iter().rev().find(|l| l.starts_with("argv ") && l.contains(&format!("{forked}.eid"))).expect("the fork's argv");
    assert!(argv.starts_with(&format!("argv --config {} web ", manual.display())), "the fork is manual too: {argv}");
    assert_eq!(perch.row(&forked).unwrap()["mode"], "manual");
    // A chat nobody remembers a mode for is the default: a log dropped into the sessions dir.
    std::fs::write(home.join("sessions").join("stranger.eid"), settled_log("hi")).unwrap();
    assert_eq!(perch.post("/hub/sessions/stranger/resume").status, 200);
    assert!(perch.fake_argv().contains(" --yolo web "), "{}", perch.fake_argv());
    for id in [id, forked, "stranger".to_string()] {
        assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    }
}

/// A fork from a point runs its first message with the source's mode: manual and auto get no `--yolo` and their own `--config` (the
/// headless run has nobody to answer an ask, so a call that needs a yes is declined), yolo gets `--yolo`; `{mode}` in the first
/// message's body overrides it, and the door that comes up afterwards has the mode the message ran with. `GET .../first` says the mode.
/// Mutations: the run uses config.toml for manual; the override is ignored; the door comes up in the source's mode instead.
#[cfg(feature = "test-bins")]
#[test]
fn the_first_message_of_a_fork_runs_under_the_sources_mode_or_the_one_it_names() {
    let (_base, home, perch) = start_modes("", "");
    let manual = home.join("config-manual.toml");
    let (id, _) = new_in(&perch, Some("manual"));
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
    assert!(perch.wait_state(&id, "kept", Duration::from_secs(5)));
    std::fs::write(home.join("sessions").join(format!("{id}.eid")), branchy_log()).unwrap();
    let fork = |at: u64| body_id(&perch.post_json(&format!("/hub/sessions/{id}/fork"), serde_json::json!({ "at": at })));
    let (one, two) = (fork(2), fork(2));
    assert_eq!(first_state(&perch, &one)["mode"], "manual");
    assert_eq!(perch.row(&one).unwrap()["mode"], "manual", "a pending fork says its mode too");
    assert_eq!(perch.post_json(&format!("/hub/sessions/{one}/first"), serde_json::json!({ "text": "go" })).status, 202);
    wait_first(&perch, &one, "done", Duration::from_secs(10));
    let line = resumes(&perch).into_iter().find(|l| l.contains(&format!("{one}.eid"))).expect("the first run");
    assert!(line.contains("cfg=config-manual.toml") && line.contains("yolo=false"), "{line}");
    let door = perch.fake_records().into_iter().find(|l| l.starts_with("argv ") && l.contains(&format!("{one}.eid"))).expect("a door");
    assert!(door.starts_with(&format!("argv --config {} web ", manual.display())), "{door}");
    // The override: this one first message in yolo, and the door is yolo.
    let said = perch.post_json(&format!("/hub/sessions/{two}/first"), serde_json::json!({ "text": "go", "mode": "yolo" }));
    assert_eq!(said.status, 202, "{}{}", said.head, said.body);
    wait_first(&perch, &two, "done", Duration::from_secs(10));
    let line = resumes(&perch).into_iter().find(|l| l.contains(&format!("{two}.eid"))).expect("the second run");
    assert!(line.contains("cfg=config.toml") && line.contains("yolo=true"), "{line}");
    let door = perch.fake_records().into_iter().find(|l| l.starts_with("argv ") && l.contains(&format!("{two}.eid"))).expect("a door");
    assert!(door.contains(" --yolo web "), "{door}");
    assert_eq!(perch.row(&two).unwrap()["mode"], "yolo");
    let three = fork(2);
    assert_eq!(perch.post_json(&format!("/hub/sessions/{three}/first"), serde_json::json!({ "text": "x", "mode": "loud" })).status, 400);
}

/// `config-manual.toml` is the home's `config.toml` plus a `[policy]` naming `<home>/policy-manual.rn`, and nothing else: same
/// `sessions_dir`, same `default_model`, same everything. A member's `[policy] enabled = false`, `yolo = true` or `judge` must not
/// make manual looser, so the manual file forces the first two and drops the third while `config.toml` keeps them. The policy is the
/// member's table with its four entry points renamed and the wrappers appended, 0600. Mutations: the policy path missing; `enabled`
/// not forced; a judge left in; the manual file readable by others; the member's other `[policy]` keys dropped.
#[cfg(feature = "test-bins")]
#[test]
fn the_manual_config_is_the_home_config_plus_a_policy_that_cannot_be_loosened() {
    use std::os::unix::fs::PermissionsExt as _;
    let member = "thinking_budget = 4000\n\n[policy]\nenabled = false\nyolo = true\njudge = \"fau:cheap\"\ndefault = \"flag\"\n\n[swarm]\nenabled = false\n";
    let (_base, home, _perch) = start_modes("", member);
    let read = |name: &str| std::fs::read_to_string(home.join(name)).unwrap().parse::<toml::Table>().unwrap();
    let (auto, manual) = (read("config.toml"), read("config-manual.toml"));
    assert_eq!(auto["policy"]["yolo"].as_bool(), Some(true), "auto is the member's own, untouched");
    assert_eq!(auto["policy"]["judge"].as_str(), Some("fau:cheap"));
    let policy = manual["policy"].as_table().unwrap();
    assert_eq!(policy["path"].as_str(), Some(home.join("policy-manual.rn").to_str().unwrap()));
    assert_eq!((policy["enabled"].as_bool(), policy["yolo"].as_bool(), policy.get("judge")), (Some(true), Some(false), None));
    assert_eq!(policy["default"].as_str(), Some("flag"), "the member's other policy keys stay");
    let (mut a, mut m) = (auto.clone(), manual.clone());
    a.remove("policy");
    m.remove("policy");
    assert_eq!(a, m, "everything outside [policy] is identical, sessions_dir and default_model included");
    assert_eq!(manual["sessions_dir"], auto["sessions_dir"]);
    for name in ["config-manual.toml", "policy-manual.rn", "config.toml"] {
        assert_eq!(std::fs::metadata(home.join(name)).unwrap().permissions().mode() & 0o777, 0o600, "{name}");
    }
    let rn = std::fs::read_to_string(home.join("policy-manual.rn")).unwrap();
    assert!(rn.contains("fn minerva_stock_classify_tool(") && rn.contains("pub fn classify_tool(tool, approval, input) {\n    let r = minerva_stock_classify_tool"), "{rn}");
    assert!(rn.contains("pub fn shell_arg(tool)"), "the rest of the member's table is untouched: {rn}");
}

/// Manual that cannot be made says so in words and takes nothing down with it: a new chat in manual is a 503 with the reason (and no
/// door), auto and yolo are unaffected, and a restart into manual is refused before the chat is stopped. Here the table named by the
/// member's `[policy] path` is missing and eidolon has none to unpack (the stand-in has no `policy` command), so there is nothing to wrap.
/// Mutations: manual falls back to `config.toml`; the restart stops the chat first.
#[cfg(feature = "test-bins")]
#[test]
fn manual_without_a_table_to_wrap_is_refused_in_words_and_changes_nothing() {
    let (_base, _home, perch) = start_modes("", "[policy]\npath = \"/nonexistent/policy.rn\"\n");
    let refused = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": perch.work.display().to_string(), "mode": "manual" }));
    assert_eq!(refused.status, 503, "{}{}", refused.head, refused.body);
    assert!(refused.body.contains("manual mode is not available"), "{}", refused.body);
    assert_eq!(starts_of(&perch), 0, "no door was started for it");
    let (id, argv) = new_in(&perch, Some("auto"));
    assert!(!argv.contains("manual") && !argv.contains("--yolo"), "{argv}");
    let starts = starts_of(&perch);
    let restart = perch.post_json(&format!("/hub/sessions/{id}/restart"), serde_json::json!({ "mode": "manual" }));
    assert_eq!(restart.status, 503, "{}{}", restart.head, restart.body);
    assert_eq!(starts_of(&perch), starts, "nothing was stopped or started");
    assert_eq!(perch.state_of(&id).as_deref(), Some("live"));
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

// ---------------------------------------------------------------------------
// The stand-in's own commands
// ---------------------------------------------------------------------------

/// `eidolon log --json --after N PATH`, which R1b's graph poll runs, prints only the records whose id is above N, in file order. The fake
/// printed the whole file whatever `--after` said, so a poll built on it would have been shown every record again each time. Without
/// `--json` the same `--after` is still fork.rs's settled check, and without `--after` `--json` is still the file as it is.
/// Mutation: drop the id filter (the fake prints all five records for `--after 3`).
#[cfg(feature = "test-bins")]
#[test]
fn fake_log_json_after_filters_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("run.eid");
    // Record 3 has no `kind`, and the last line is a record with no id at all: that one is not above anything.
    let text = "{\"id\":1,\"kind\":\"a\"}\n{\"id\":2,\"kind\":\"b\"}\n{\"id\":3}\n{\"id\":4,\"kind\":\"d\"}\n{\"id\":5,\"kind\":\"e\"}\n{\"fake\":true}\n";
    std::fs::write(&log, text).unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(fake_eidolon().expect("the stand-in door")).args(["--config", "/nonexistent"]).args(args).arg(&log).output().unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    let ids = |out: &str| -> Vec<u64> { out.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok()?["id"].as_u64()).collect() };

    assert_eq!(ids(&run(&["log", "--json", "--after", "3"])), [4, 5]);
    assert_eq!(ids(&run(&["log", "--json", "--after", "0"])), [1, 2, 3, 4, 5]);
    assert_eq!(run(&["log", "--json", "--after", "5"]), "", "nothing after the last record is an empty answer, not a head line");
    assert_eq!(run(&["log", "--json"]), text, "without --after it is the file, byte for byte");
    // fork.rs's form: no --json, the log first, a far-away --after. It is the settled check and still is.
    let out = Command::new(fake_eidolon().unwrap()).args(["log"]).arg(&log).args(["--after", "9999999999"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("head: None  settled: "), "{out:?}");
    // A value that is not an id is the usage error a real command gives, not an empty answer.
    let bad = Command::new(fake_eidolon().unwrap()).args(["log", "--json", "--after", "soon"]).arg(&log).output().unwrap();
    assert_eq!(bad.status.code(), Some(2), "{bad:?}");
}
