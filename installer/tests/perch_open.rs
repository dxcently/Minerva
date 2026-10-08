//! `bin/perch-open.sh` and `nix/minerva.sh`: the key arm (`--keys-arm` / `keys allow-web`) against a stub HTTP server on
//! 127.0.0.1:48110, and the start-up flags that only a newer perch knows. Both scripts get the same cases. Every run has a cleared
//! environment, a temp HOME holding DECOY tokens at the default places, and a `curl` shim that logs argv and refuses 47820.
#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{DECOY, Env, GIVEN, STUB_PORT, Stub, mkdir, port_lock, repo_root, text, write};
use serde_json::{Value, json};

const MINERVA: &str = env!("CARGO_BIN_EXE_minerva");
const FP: &str = "0F7A3A5C447EC7EF";
const FP_GROUPS: &str = "0F7A 3A5C 447E C7EF";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Which {
    PerchOpen,
    Nix,
}

const BOTH: [Which; 2] = [Which::PerchOpen, Which::Nix];

fn script(w: Which) -> PathBuf {
    match w {
        Which::PerchOpen => repo_root().join("bin/perch-open.sh"),
        Which::Nix => repo_root().join("nix/minerva.sh"),
    }
}

/// The arguments that ask `w` to arm `fp` (as a person would type it), with the test-only port and token file.
fn arm_args(w: Which, fp: &str, port: &str, token_file: &Path) -> Vec<String> {
    let tail = ["--port".to_string(), port.to_string(), "--token-file".to_string(), token_file.display().to_string()];
    let mut a: Vec<String> = match w {
        Which::PerchOpen => vec!["--keys-arm".into(), fp.into()],
        // the page shows four groups and hoot-less users type them as separate words
        Which::Nix => ["keys", "allow-web"].iter().map(|s| s.to_string()).chain(fp.split(' ').map(|s| s.to_string())).collect(),
    };
    a.extend(tail);
    a
}

fn run_script(env: &Env, w: Which, args: &[String]) -> Output {
    env.cmd("bash").arg(script(w)).args(args).stdin(Stdio::null()).output().unwrap()
}

fn token_file(env: &Env, text: &str, mode: u32) -> PathBuf {
    let p = env.dir.path().join("t");
    write(&p, text, mode);
    p
}

fn argv_calls(env: &Env) -> Vec<String> {
    env.argv_log().lines().filter(|l| l.starts_with("ARGV")).map(str::to_string).collect()
}

// ---- syntax ---------------------------------------------------------------------------------------------------------

#[test]
fn the_scripts_parse() {
    for w in BOTH {
        let st = Command::new("bash").arg("-n").arg(script(w)).status().unwrap();
        assert!(st.success(), "bash -n {:?}", script(w));
    }
}

#[test]
fn perch_open_help_lists_the_arm_and_old_flags_still_parse() {
    let env = Env::new();
    let out = run_script(&env, Which::PerchOpen, &["--help".into()]);
    let (o, _) = text(&out);
    assert!(out.status.success());
    assert!(o.contains("--keys-arm FP") && o.contains("--token-file F") && o.contains("--port P"), "{o}");
    assert!(o.contains("--stop") && o.contains("MINERVA_PLUGINS_DIR"));
    assert!(!o.contains("set -euo"), "the help is the header comment only");
    for (args, code, err) in [
        (vec!["--bogus"], 2, "unknown argument: --bogus"),
        (vec!["--keys-arm"], 2, "--keys-arm needs a value"),
        (vec!["--token-file", "/x"], 2, "goes with --keys-arm only"),
        (vec!["--port", "48110", "--token-file", "/x"], 2, "goes with --keys-arm only"),
        (vec!["--keys-arm", FP, "--token-file", "/x"], 2, "--token-file needs --port too"),
        (vec!["--keys-arm", FP, "--stop"], 2, "stands alone"),
        (vec!["--status", "--keys-arm", FP], 2, "stands alone"),
        (vec!["--keys-arm", FP, "--code"], 2, "stands alone"),
    ] {
        let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let ran = run_script(&env, Which::PerchOpen, &a);
        let (out, e) = text(&ran);
        assert_eq!((ran.status.code(), out.as_str()), (Some(code), ""), "{args:?}");
        assert!(e.contains(err), "{args:?}: {e}");
    }
    // --status and --stop with nothing running keep their answers (and their exit codes)
    let st = run_script(&env, Which::PerchOpen, &["--status".into()]);
    assert_eq!(st.status.code(), Some(1));
    assert!(text(&st).0.contains("down: nothing of ours on port 47820"));
    let sp = run_script(&env, Which::PerchOpen, &["--stop".into()]);
    assert_eq!(sp.status.code(), Some(0));
    assert!(text(&sp).0.contains("not running on port 47820"));
    // --port moves them to another perch's files
    let st = run_script(&env, Which::PerchOpen, &["--status".into(), "--port".into(), "48110".into()]);
    assert_eq!(st.status.code(), Some(1));
    assert!(text(&st).0.contains("down: nothing of ours on port 48110"));
    let sp = run_script(&env, Which::PerchOpen, &["--stop".into(), "--port".into(), "48110".into()]);
    assert!(text(&sp).0.contains("not running on port 48110"));
    assert_eq!(env.argv_log(), "", "none of them touched the network");
}

// ---- the arm --------------------------------------------------------------------------------------------------------------

#[test]
fn the_arm_sends_the_given_token_only_through_a_0600_header_file() {
    let _g = port_lock();
    for w in BOTH {
        let env = Env::new();
        let tok = token_file(&env, GIVEN, 0o600);
        let stub = Stub::start(STUB_PORT, 200, r#"{"armed":true}"#);
        let out = run_script(&env, w, &arm_args(w, "0f7a 3a5c 447e c7ef", &STUB_PORT.to_string(), &tok));
        let (o, e) = text(&out);
        assert!(out.status.success(), "{w:?}: {o}{e}");
        assert!(o.contains(FP_GROUPS) && o.contains("2 minutes"), "{w:?}: {o}");
        for secret in [GIVEN, DECOY] {
            assert!(!o.contains(secret) && !e.contains(secret), "{w:?}: a token reached the output");
        }

        // the stub got exactly the override token, once, with the fingerprint normalised
        let reqs = stub.requests();
        assert_eq!(reqs.len(), 1, "{w:?}: {reqs:?}");
        let r = &reqs[0];
        assert_eq!(r.request_line, "POST /hub/install/keys/arm HTTP/1.1", "{w:?}");
        assert_eq!(r.header("authorization"), Some(format!("Bearer {GIVEN}").as_str()), "{w:?}");
        assert!(r.header("content-type").unwrap_or("").starts_with("application/json"), "{w:?}");
        assert_eq!(r.header("host"), Some(format!("127.0.0.1:{STUB_PORT}").as_str()));
        assert_eq!(serde_json::from_str::<Value>(&r.body).unwrap(), json!({ "fingerprint": FP }), "{w:?}");

        // curl was called once, with the token in a header FILE (0600, in the private runtime dir), never in argv
        let calls = argv_calls(&env);
        assert_eq!(calls.len(), 1, "{w:?}: {calls:?}");
        let argv = env.argv_log();
        assert!(!argv.contains(GIVEN) && !argv.contains(DECOY), "{w:?}: the token is in argv:\n{argv}");
        assert!(!argv.contains("47820") && !argv.contains("REFUSED"), "{w:?}: {argv}");
        assert!(calls[0].contains(&format!("[http://127.0.0.1:{STUB_PORT}/hub/install/keys/arm]")), "{w:?}: {}", calls[0]);
        let hdr = argv.lines().find(|l| l.starts_with("HDRFILE ")).unwrap_or_else(|| panic!("{w:?}: no header file in:\n{argv}"));
        let path = hdr["HDRFILE ".len()..].split(" mode=").next().unwrap();
        assert!(path.starts_with(&format!("{}/minerva/hdr.", env.run.display())), "{w:?}: {path}");
        assert!(hdr.ends_with("mode=600"), "{w:?}: {hdr}");
        assert!(calls[0].contains(&format!("[-H] [@{path}]")), "{w:?}: {}", calls[0]);
        assert_eq!(env.headers_log(), format!("Authorization: Bearer {GIVEN}\n"), "{w:?}: the header file held the override token");
        assert_eq!(env.leftover_headers(), Vec::<String>::new(), "{w:?}: the header file is removed");
        drop(stub);
    }
}

#[test]
fn the_arm_refuses_bad_input_before_it_calls_curl() {
    let _g = port_lock();
    for w in BOTH {
        let env = Env::new();
        let stub = Stub::start(STUB_PORT, 200, "{}");
        let good = token_file(&env, GIVEN, 0o600);
        let port = STUB_PORT.to_string();
        let mut cases: Vec<(String, Vec<String>)> = Vec::new();
        for fp in ["", "xyz", "0F7A3A5C447EC7E", "0F7A3A5C447EC7EFF", "0F7A3A5C447EC7EG", "0F7A3A5C447EC7EF;id", "../../x"] {
            cases.push((format!("fingerprint {fp:?}"), arm_args(w, fp, &port, &good)));
        }
        // one spelling of a port: 1 to 65535, digits, no leading zero (047820 is octal to bash) and no run of digits that wraps
        for p in ["abc", "0", "70000", "65536", "-1", "+5", "", "047820", "99999999999999999999", " 48110", "48110 ", "4811\n0", "0x10"] {
            cases.push((format!("port {p:?}"), arm_args(w, FP, p, &good)));
        }
        // a token file is one perch's token: not read without the port that perch answers on
        let no_port: Vec<String> = match w {
            Which::PerchOpen => vec!["--keys-arm".into(), FP.into(), "--token-file".into(), good.display().to_string()],
            Which::Nix => vec!["keys".into(), "allow-web".into(), FP.into(), "--token-file".into(), good.display().to_string()],
        };
        cases.push(("a token file without a port".into(), no_port));
        // token files that are not the perch's: group-readable, a link, missing, not a token, a header injection, too short
        let loose = env.dir.path().join("loose");
        write(&loose, GIVEN, 0o644);
        cases.push(("a world-readable token file".into(), arm_args(w, FP, &port, &loose)));
        let link = env.dir.path().join("link");
        std::os::unix::fs::symlink(&good, &link).unwrap();
        cases.push(("a symlinked token file".into(), arm_args(w, FP, &port, &link)));
        cases.push(("a missing token file".into(), arm_args(w, FP, &port, &env.dir.path().join("nope"))));
        cases.push(("a directory".into(), arm_args(w, FP, &port, env.dir.path())));
        for (label, body) in [("injection", "good-token-0123456789abcdef\nX-Evil: 1"), ("short", "short"), ("spaces", "has a space in it 0123456789"), ("empty", "")] {
            let f = env.dir.path().join(format!("bad-{label}"));
            write(&f, body, 0o600);
            cases.push((format!("a token file with {label}"), arm_args(w, FP, &port, &f)));
        }
        for (label, args) in &cases {
            let out = run_script(&env, w, args);
            assert!(!out.status.success(), "{w:?}: {label} was accepted");
            let (o, e) = text(&out);
            assert!(!o.contains("allowed") && !e.contains(GIVEN), "{w:?}: {label}: {o}{e}");
        }
        // the port in the environment is held to the same spelling, and not echoed back
        for p in ["abc", "0", "70000", "047820", "99999999999999999999", "1e3", "-5"] {
            let mut c = env.cmd("bash");
            c.arg(script(w));
            match w {
                Which::PerchOpen => c.args(["--keys-arm", FP]),
                Which::Nix => c.args(["keys", "allow-web", FP]),
            };
            let out = c.env("MINERVA_PERCH_PORT", p).stdin(Stdio::null()).output().unwrap();
            let (o, e) = text(&out);
            assert_eq!(out.status.code(), Some(1), "{w:?}: MINERVA_PERCH_PORT={p:?}: {o}{e}");
            assert!(e.contains("MINERVA_PERCH_PORT must be a port number"), "{w:?} {p:?}: {e}");
            assert!(p.len() < 2 || !e.contains(p), "{w:?}: the value was echoed: {e}");
        }
        assert_eq!(env.argv_log(), "", "{w:?}: curl was never reached");
        assert!(stub.requests().is_empty(), "{w:?}");
        assert_eq!(env.leftover_headers(), Vec::<String>::new());
    }
}

#[test]
fn the_arm_says_what_the_perch_answered() {
    let _g = port_lock();
    for w in BOTH {
        let env = Env::new();
        let tok = token_file(&env, GIVEN, 0o600);
        let args = arm_args(w, FP, &STUB_PORT.to_string(), &tok);
        for (status, want) in [(403, "answered 403"), (409, "answered 409"), (401, "answered 401"), (500, "answered 500"), (404, "no update-key routes")] {
            let stub = Stub::start(STUB_PORT, status, r#"{"why":"x"}"#);
            let out = run_script(&env, w, &args);
            let (o, e) = text(&out);
            assert_eq!(out.status.code(), Some(1), "{w:?} {status}: {o}{e}");
            assert!(e.contains(want) && !o.contains("allowed"), "{w:?} {status}: {o}{e}");
            assert!(!e.contains(GIVEN) && !o.contains(GIVEN));
            assert_eq!(stub.requests().len(), 1);
        }
        // nothing listening
        let out = run_script(&env, w, &args);
        let (o, e) = text(&out);
        assert_eq!(out.status.code(), Some(1), "{w:?}: {o}{e}");
        assert!(e.contains("did not answer"), "{w:?}: {e}");
        assert_eq!(env.leftover_headers(), Vec::<String>::new());
    }
}

#[test]
fn without_a_token_file_the_arm_needs_a_perch_of_ours_and_never_reads_the_decoys() {
    let _g = port_lock();
    for w in BOTH {
        let env = Env::new();
        let stub = Stub::start(STUB_PORT, 200, "{}");
        for port in [None, Some(STUB_PORT)] {
            let mut c = env.cmd("bash");
            c.arg(script(w));
            match w {
                Which::PerchOpen => c.args(["--keys-arm", FP]),
                Which::Nix => c.args(["keys", "allow-web", FP]),
            };
            if let Some(p) = port {
                c.env("MINERVA_PERCH_PORT", p.to_string());
            }
            let out = c.output().unwrap();
            let (o, e) = text(&out);
            assert_eq!(out.status.code(), Some(1), "{w:?} {port:?}: {o}{e}");
            assert!(e.contains("not running"), "{w:?}: {e}");
            assert!(!e.contains(DECOY));
        }
        assert_eq!(env.argv_log(), "", "{w:?}: no pid file, so not one call, to 47820 or anywhere");
        assert!(stub.requests().is_empty());
    }
}

#[test]
fn with_our_perch_running_the_arm_uses_its_own_token_on_its_own_port() {
    let _g = port_lock();
    for w in BOTH {
        let env = Env::new();
        // "our perch": a live process in the pid file, a token file at the default place, a server that answers 200
        let mut alive = Command::new("sleep").arg("30").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        write(&env.run.join(format!("minerva/perch-{STUB_PORT}.pid")), &format!("{}\n", alive.id()), 0o600);
        let stub = Stub::start(STUB_PORT, 200, r#"{"armed":true}"#);
        let mut c = env.cmd("bash");
        c.arg(script(w)).env("MINERVA_PERCH_PORT", STUB_PORT.to_string());
        match w {
            Which::PerchOpen => c.args(["--keys-arm", FP]),
            Which::Nix => c.args(["keys", "allow-web", FP]),
        };
        let out = c.output().unwrap();
        alive.kill().unwrap();
        alive.wait().unwrap();
        let (o, e) = text(&out);
        assert!(out.status.success(), "{w:?}: {o}{e}");
        let reqs = stub.requests();
        let lines: Vec<&str> = reqs.iter().map(|r| r.request_line.as_str()).collect();
        assert_eq!(lines, ["GET /hub/sessions HTTP/1.1", "POST /hub/install/keys/arm HTTP/1.1"], "{w:?}: the liveness probe, then the arm");
        for r in &reqs {
            assert_eq!(r.header("authorization"), Some(format!("Bearer {DECOY}").as_str()), "{w:?}: the perch's own token, from its own file");
        }
        assert!(!env.argv_log().contains("47820") && !env.argv_log().contains(DECOY));
        assert_eq!(env.leftover_headers(), Vec::<String>::new());
    }
}

#[test]
fn a_token_file_that_is_not_the_perchs_is_never_sent_anywhere_on_any_path() {
    // Our perch "is running" (a live pid in its pid file, a server on the port) but the token file at the default place is not a
    // token file as the perch writes it: not even the liveness probe may carry what is in it.
    let _g = port_lock();
    for w in BOTH {
        for (label, make) in [("group-readable", 0), ("a link", 1), ("not a token", 2), ("a directory", 3)] {
            let env = Env::new();
            let mut alive = Command::new("sleep").arg("30").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
            write(&env.run.join(format!("minerva/perch-{STUB_PORT}.pid")), &format!("{}\n", alive.id()), 0o600);
            let tokf = env.run.join(format!("minerva/perch-{STUB_PORT}.token"));
            fs::remove_file(&tokf).unwrap();
            match make {
                0 => write(&tokf, GIVEN, 0o644),
                1 => {
                    let target = env.dir.path().join("elsewhere");
                    write(&target, GIVEN, 0o600);
                    std::os::unix::fs::symlink(&target, &tokf).unwrap();
                }
                2 => write(&tokf, "x y\nX-Evil: 1", 0o600),
                _ => mkdir(&tokf, 0o700),
            }
            let stub = Stub::start(STUB_PORT, 200, "{}");
            let mut runs: Vec<Vec<&str>> = vec![match w {
                Which::PerchOpen => vec!["--keys-arm", FP],
                Which::Nix => vec!["keys", "allow-web", FP],
            }];
            runs.push(match w {
                Which::PerchOpen => vec!["--status"],
                Which::Nix => vec!["status"],
            });
            for args in runs {
                let mut c = env.cmd("bash");
                c.arg(script(w)).args(&args).env("MINERVA_PERCH_PORT", STUB_PORT.to_string());
                let out = c.stdin(Stdio::null()).output().unwrap();
                let (o, e) = text(&out);
                assert_eq!(out.status.code(), Some(1), "{w:?} {label} {args:?}: {o}{e}");
                assert!(!o.contains(GIVEN) && !e.contains(GIVEN), "{w:?} {label}");
            }
            alive.kill().unwrap();
            alive.wait().unwrap();
            assert_eq!(env.argv_log(), "", "{w:?} {label}: curl was never called, the probe included");
            assert!(stub.requests().is_empty(), "{w:?} {label}");
            assert_eq!(env.leftover_headers(), Vec::<String>::new());
        }
    }
}

/// Proxy variables as a person's shell may have them (the lowercase ones are the ones curl reads for http), with `no_proxy=localhost`,
/// which does not cover 127.0.0.1.
fn with_proxy_env(c: &mut Command, proxy: &str) {
    for k in ["http_proxy", "https_proxy", "all_proxy", "ALL_PROXY", "HTTPS_PROXY"] {
        c.env(k, proxy);
    }
    c.env("no_proxy", "localhost");
}

#[test]
fn the_token_goes_neither_through_a_proxy_nor_into_a_curlrc_listing() {
    let _g = port_lock();
    let proxy = format!("http://127.0.0.1:{STUB_PORT}");
    // The control: plain curl with this HOME and these variables DOES send the header through the "proxy" (the stub, which then sees
    // the absolute-form target) and DOES print it, because the ~/.curlrc says `verbose`. Without this the rest would prove nothing.
    {
        let env = Env::new();
        write(&env.home.join(".curlrc"), "verbose\n", 0o600);
        let hdr = env.dir.path().join("hdr");
        write(&hdr, &format!("Authorization: Bearer {GIVEN}\n"), 0o600);
        let stub = Stub::start(STUB_PORT, 200, "{}");
        let mut c = env.cmd(env.shim.join("curl"));
        with_proxy_env(&mut c, &proxy);
        let out = c.args(["-sS", "-H"]).arg(format!("@{}", hdr.display())).arg(format!("http://127.0.0.1:{STUB_PORT}/control")).output().unwrap();
        let (_, e) = text(&out);
        assert_eq!(stub.requests().first().map(|r| r.request_line.clone()), Some(format!("GET http://127.0.0.1:{STUB_PORT}/control HTTP/1.1")), "curl used the proxy");
        assert!(e.contains(GIVEN), "the verbose curlrc printed the Authorization header, as the review said it would");
    }
    for w in BOTH {
        // the arm with a given token file, and the arm on our own perch (the liveness probe and the arm)
        for own_perch in [false, true] {
            let env = Env::new();
            write(&env.home.join(".curlrc"), "verbose\n", 0o600);
            let tok = token_file(&env, GIVEN, 0o600);
            let mut alive = Command::new("sleep").arg("30").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
            let stub = Stub::start(STUB_PORT, 200, r#"{"armed":true}"#);
            let mut c = env.cmd("bash");
            c.arg(script(w));
            if own_perch {
                write(&env.run.join(format!("minerva/perch-{STUB_PORT}.pid")), &format!("{}\n", alive.id()), 0o600);
                c.env("MINERVA_PERCH_PORT", STUB_PORT.to_string());
                match w {
                    Which::PerchOpen => c.args(["--keys-arm", FP]),
                    Which::Nix => c.args(["keys", "allow-web", FP]),
                };
            } else {
                c.args(arm_args(w, FP, &STUB_PORT.to_string(), &tok));
            }
            with_proxy_env(&mut c, &proxy);
            let out = c.stdin(Stdio::null()).output().unwrap();
            alive.kill().unwrap();
            alive.wait().unwrap();
            let (o, e) = text(&out);
            let label = format!("{w:?} own_perch={own_perch}");
            assert!(out.status.success(), "{label}: {o}{e}");
            // every request reached the stub directly: origin-form targets, none for the proxy to forward
            let lines: Vec<String> = stub.requests().iter().map(|r| r.request_line.clone()).collect();
            let want: &[&str] = if own_perch { &["GET /hub/sessions HTTP/1.1", "POST /hub/install/keys/arm HTTP/1.1"] } else { &["POST /hub/install/keys/arm HTTP/1.1"] };
            assert_eq!(lines, want, "{label}");
            // nothing was printed of the request: no curlrc `verbose`, so no header, token or otherwise
            for secret in [GIVEN, DECOY] {
                assert!(!o.contains(secret) && !e.contains(secret), "{label}: a token reached the output:\n{e}");
            }
            assert!(!e.contains("Authorization") && !e.contains("Bearer") && !e.contains("> POST"), "{label}: curl was verbose:\n{e}");
            // and the options that did it are on every call, first
            let calls = argv_calls(&env);
            assert_eq!(calls.len(), want.len(), "{label}: {calls:?}");
            for call in &calls {
                assert!(call.starts_with("ARGV [-q] [--noproxy] [*] "), "{label}: -q first, then --noproxy '*': {call}");
            }
            assert_eq!(env.leftover_headers(), Vec::<String>::new(), "{label}");
        }
    }
}

#[test]
fn an_interrupt_while_the_perch_is_being_probed_leaves_no_header_file_behind() {
    let _g = port_lock();
    for w in BOTH {
        let env = Env::new();
        let mut alive = Command::new("sleep").arg("30").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        write(&env.run.join(format!("minerva/perch-{STUB_PORT}.pid")), &format!("{}\n", alive.id()), 0o600);
        // a perch that takes 3 s to answer, so the script is inside its first curl when the signal comes
        let stub = Stub::start_slow(STUB_PORT, 200, "{}", Duration::from_secs(3));
        let mut c = env.cmd("bash");
        c.arg(script(w)).env("MINERVA_PERCH_PORT", STUB_PORT.to_string());
        match w {
            Which::PerchOpen => c.args(["--keys-arm", FP]),
            Which::Nix => c.args(["keys", "allow-web", FP]),
        };
        // its own process group, so the ^C reaches the script, the shell it forked for the probe and curl, as a terminal's does
        let mut child = c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).process_group(0).spawn().unwrap();
        let started = Instant::now();
        while !env.argv_log().contains("/hub/sessions") {
            assert!(started.elapsed() < Duration::from_secs(10), "{w:?}: the probe never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300)); // curl has connected and is waiting
        assert!(!env.leftover_headers().is_empty(), "{w:?}: the header file is there while the probe runs");
        let sent = Command::new("bash").arg("-c").arg(format!("kill -INT -- -{}", child.id())).status().unwrap();
        assert!(sent.success());
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "{w:?}: the script did not stop");
            std::thread::sleep(Duration::from_millis(20));
        };
        alive.kill().unwrap();
        alive.wait().unwrap();
        assert!(!status.success(), "{w:?}: interrupted, not finished");
        assert_eq!(env.leftover_headers(), Vec::<String>::new(), "{w:?}: the interrupted run left its header file");
        drop(stub);
    }
}

#[test]
fn minerva_web_keys_arm_hands_the_fingerprint_to_the_script_beside_it() {
    let _g = port_lock();
    let env = Env::new();
    let inst = env.dir.path().join("inst/bin");
    mkdir(&inst, 0o755);
    fs::copy(MINERVA, inst.join("minerva")).unwrap();
    fs::set_permissions(inst.join("minerva"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::copy(script(Which::PerchOpen), inst.join("perch-open.sh")).unwrap();
    let tok = token_file(&env, GIVEN, 0o600);
    let stub = Stub::start(STUB_PORT, 200, r#"{"armed":true}"#);
    let out = env
        .cmd(inst.join("minerva"))
        .args(["web", "--keys-arm", "0f7a3a5c447ec7ef", "--port", &STUB_PORT.to_string(), "--token-file", tok.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let (o, e) = text(&out);
    assert!(out.status.success(), "{o}{e}");
    assert!(o.contains(FP_GROUPS), "{o}");
    let reqs = stub.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].header("authorization"), Some(format!("Bearer {GIVEN}").as_str()));
    assert_eq!(serde_json::from_str::<Value>(&reqs[0].body).unwrap(), json!({ "fingerprint": FP }), "upper-cased before it left minerva");
    assert!(!env.argv_log().contains(GIVEN) && !env.argv_log().contains("47820"));
    // the script's own refusals come back as minerva's exit code
    let out = env.cmd(inst.join("minerva")).args(["web", "--keys-arm", FP, "--port", &STUB_PORT.to_string(), "--token-file", "/nonexistent"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
}

// ---- the flags only a newer perch knows ----------------------------------------------------------------------------------------

/// A fixture install around a stub perch whose `--help` is whatever the test says.
struct Start {
    env: Env,
    w: Which,
    ui: PathBuf,
    mhome: PathBuf,
    plugins: PathBuf,
    root: PathBuf,
    eidolon: PathBuf,
    argv: PathBuf,
}

// The stub perch's --help is the fixture; with colour forced in its environment it wraps each option name in colour codes, as clap
// does. It writes its own pid ($$) to $STUB_PID, which is the only process the test kills when it is done.
const STUB_PERCH: &str = r#"#!/bin/bash
if [ "${1:-}" = --help ]; then
  if [ -n "${CLICOLOR_FORCE:-}${FORCE_COLOR:-}" ]; then sed -E 's/--([a-z-]+)/\x1b[1m--\1\x1b[0m/' "$STUB_HELP"; else cat "$STUB_HELP"; fi
  exit 0
fi
echo $$ >"$STUB_PID"
printf '%s\n' "$@" >"$STUB_ARGV"
bind=; prev=
for a in "$@"; do if [ "$prev" = --bind ]; then bind=$a; fi; prev=$a; done
port=${bind##*:}
tokf=$XDG_RUNTIME_DIR/minerva/perch-$port.token
printf 'stub-perch-token-%s-0123456789' "$port" >"$tokf"
echo "listening on http://$bind"
echo "token file: $tokf"
trap 'rm -f "$tokf"; exit 0' TERM
while :; do sleep 0.1; done
"#;

impl Start {
    fn new(w: Which, help: &str) -> Start {
        let env = Env::new();
        let d = env.dir.path().to_path_buf();
        let (ui, mhome, plugins, root) = (d.join("ui"), d.join("mhome"), d.join("plugins"), d.join("root"));
        for p in [&ui, &plugins, &root] {
            mkdir(p, 0o755);
        }
        write(&ui.join("index.html"), "<!doctype html>", 0o644);
        write(&plugins.join("jev/plugin.rn"), "", 0o644);
        write(&d.join("help.txt"), help, 0o644);
        write(&d.join("perch"), STUB_PERCH, 0o755);
        write(&d.join("setup"), &format!("#!/bin/bash\nif [ \"${{1:-}}\" = --print-roots ]; then echo {}; fi\n", root.display()), 0o755);
        Start { env, w, ui, mhome, plugins, root, eidolon: PathBuf::from("/usr/bin/true"), argv: d.join("argv.txt") }
    }

    fn cmd(&self, arg: &str) -> Command {
        let d = self.env.dir.path();
        let mut c = self.env.cmd("bash");
        c.arg(script(self.w))
            .env("MINERVA_PERCH_BIN", d.join("perch"))
            .env("MINERVA_SETUP_BIN", d.join("setup"))
            .env("MINERVA_UI_DIR", &self.ui)
            .env("MINERVA_HOME", &self.mhome)
            .env("MINERVA_PLUGINS_DIR", &self.plugins)
            .env("MINERVA_PERCH_PORT", STUB_PORT.to_string())
            .env("MINERVA_NO_PLUGIN_SYNC", "1")
            .env("MINERVA_EIDOLON_BIN", &self.eidolon)
            .env("STUB_HELP", d.join("help.txt"))
            .env("STUB_PID", d.join("stub.pid"))
            .env("STUB_ARGV", &self.argv)
            .stdin(Stdio::null());
        match (self.w, arg) {
            (Which::PerchOpen, a) => c.arg(a),
            (Which::Nix, "--no-browser") => c.args(["web", "--no-browser"]),
            (Which::Nix, "--stop") => c.arg("stop"),
            (Which::Nix, a) => c.arg(a),
        };
        c
    }

    /// Start (the perch is a stub that records its argv) and return that argv.
    fn started(&self) -> Vec<String> {
        let out = self.cmd("--no-browser").output().unwrap();
        let (o, e) = text(&out);
        assert!(out.status.success(), "{:?}: {o}{e}", self.w);
        assert!(o.contains("minerva perch on"), "{o}");
        fs::read_to_string(&self.argv).unwrap().lines().map(str::to_string).collect()
    }

    /// The command line of a perch that knows nothing of the newer flags, as the script built it before this slice.
    fn base_argv(&self) -> Vec<String> {
        let mut v: Vec<String> = vec!["--bind".into(), format!("127.0.0.1:{STUB_PORT}"), "--ui-dir".into(), self.ui.display().to_string()];
        if self.w == Which::Nix {
            v.extend(["--eidolon".into(), self.eidolon.display().to_string()]);
        }
        v.extend(["--root".into(), self.root.display().to_string(), "--home".into(), self.mhome.display().to_string()]);
        v
    }

    fn home_eidolon(&self) -> PathBuf {
        self.env.home.join(".config/eidolon")
    }

    /// The command line of a perch whose `--help` lists every flag, started on the default port, with an aoide binary in place.
    fn newer_argv(&self) -> Vec<String> {
        let dir = self.home_eidolon();
        let mut want = self.base_argv();
        want.extend([
            "--term-token-file".into(),
            format!("{}/minerva-term.token", dir.display()),
            "--mail-token-file".into(),
            format!("{}/minerva-mail.token", dir.display()),
            "--agent-url-file".into(),
            format!("{}/minerva-perch.url", dir.display()),
            "--aoide".into(),
            format!("{}/.local/bin/aoide", self.env.home.display()),
            "--plugins-dir".into(),
            self.plugins.display().to_string(),
        ]);
        want
    }
}

impl Drop for Start {
    /// Stops the stub perch, and nothing else: the pid is the one the stub wrote about itself, and only while that process is still
    /// the stub script (a pid that has since been reused is left alone).
    fn drop(&mut self) {
        let Ok(pid) = fs::read_to_string(self.env.dir.path().join("stub.pid")) else { return };
        let pid = pid.trim();
        if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            return;
        }
        let stub = self.env.dir.path().join("perch").display().to_string();
        let still_the_stub = || fs::read(format!("/proc/{pid}/cmdline")).map(|c| String::from_utf8_lossy(&c).contains(&stub)).unwrap_or(false);
        if !still_the_stub() {
            return;
        }
        let _ = Command::new("kill").arg("-TERM").arg(pid).stderr(Stdio::null()).status();
        for _ in 0..100 {
            if !still_the_stub() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = Command::new("kill").arg("-9").arg(pid).stderr(Stdio::null()).status();
    }
}

const HELP_OLD: &str = include_str!("fixtures/perch-help-old.txt");
const HELP_NEW: &str = include_str!("fixtures/perch-help-new.txt");

#[test]
fn the_fixtures_are_what_they_claim() {
    // captured from the perch an operator has installed today, and from a build of master
    let new_flags = [
        "--term-token-file", "--mail-token-file", "--agent-url-file", "--aoide", "--plugins-dir", "--aoide-audit-log", "--aoide-daemon-events",
    ];
    for f in new_flags {
        assert!(!HELP_OLD.lines().any(|l| l.trim_start().starts_with(f)), "{f} in the old help");
        assert!(HELP_NEW.lines().any(|l| l.trim_start().starts_with(f)), "{f} missing from the new help");
    }
    assert!(HELP_OLD.contains("--home") && HELP_OLD.contains("--eidolon-config"), "old enough to start, not old enough to be refused");
    assert!(HELP_NEW.contains("--aoide-home"), "the prefix case is in the fixture");
}

#[test]
fn a_perch_built_before_the_flags_starts_with_the_command_line_it_always_had() {
    let _g = port_lock();
    for w in BOTH {
        let s = Start::new(w, HELP_OLD);
        // an eidolon config dir that does not exist must not be made for a perch that will not write there
        fs::remove_dir_all(s.env.home.join(".config")).unwrap();
        write(&s.env.home.join(".local/bin/aoide"), "#!/bin/sh\n", 0o755); // there, but this perch has no --aoide
        let argv = s.started();
        assert_eq!(argv, s.base_argv(), "{w:?}");
        assert!(!s.home_eidolon().exists(), "{w:?}: nothing was created for flags that were not passed");
    }
}

#[test]
fn a_newer_perch_gets_every_flag_with_its_value() {
    let _g = port_lock();
    for w in BOTH {
        let s = Start::new(w, HELP_NEW);
        write(&s.env.home.join(".local/bin/aoide"), "#!/bin/sh\n", 0o755);
        let argv = s.started();
        let dir = s.home_eidolon();
        assert_eq!(argv, s.newer_argv(), "{w:?}");
        // the token files themselves are the perch's to write; the script never creates them
        for f in ["minerva-term.token", "minerva-mail.token"] {
            assert_eq!(fs::read_to_string(dir.join(f)).unwrap(), DECOY, "{w:?}: {f} was left alone");
        }
        assert!(!dir.join("minerva-perch.url").exists());
    }
}

#[test]
fn aoide_and_the_plugins_dir_go_in_only_when_there_is_something_to_name() {
    let _g = port_lock();
    for w in BOTH {
        // no aoide binary, no plugins dir
        let s = Start::new(w, HELP_NEW);
        fs::remove_dir_all(&s.plugins).unwrap();
        let argv = s.started();
        assert!(!argv.contains(&"--aoide".to_string()) && !argv.contains(&"--plugins-dir".to_string()), "{w:?}: {argv:?}");
        assert!(argv.contains(&"--term-token-file".to_string()));
        drop(s);
        // an aoide that is not executable is not named either
        let s = Start::new(w, HELP_NEW);
        write(&s.env.home.join(".local/bin/aoide"), "x", 0o644);
        assert!(!s.started().contains(&"--aoide".to_string()), "{w:?}");
    }
}

#[test]
fn a_flag_is_found_by_being_an_option_not_by_being_mentioned() {
    let _g = port_lock();
    // --aoide-home without --aoide; and every new flag named in the prose of another option
    let help = concat!(
        "Usage: perch [OPTIONS]\n\nOptions:\n",
        "      --home <DIR>         The home. Not with --term-token-file, --mail-token-file, --agent-url-file, --aoide or --plugins-dir\n",
        "      --eidolon-config <FILE>  A config\n",
        "      --aoide-home <DIR>   Aoide's home\n",
        "      --aoide-audit-log <PATH>  Its log\n",
        "  -h, --help               Print help\n",
    );
    for w in BOTH {
        let s = Start::new(w, help);
        write(&s.env.home.join(".local/bin/aoide"), "#!/bin/sh\n", 0o755);
        assert_eq!(s.started(), s.base_argv(), "{w:?}");
    }
    // the last option of the list, with nothing after it on its line, and one with a short form
    let help = "Options:\n      --home <DIR>  x\n      --eidolon-config <FILE>  y\n      --plugins-dir\n  -a, --aoide <PATH>  z\n";
    for w in BOTH {
        let s = Start::new(w, help);
        write(&s.env.home.join(".local/bin/aoide"), "#!/bin/sh\n", 0o755);
        let argv = s.started();
        assert!(argv.contains(&"--plugins-dir".to_string()) && argv.contains(&"--aoide".to_string()), "{w:?}: {argv:?}");
        assert!(!argv.contains(&"--term-token-file".to_string()));
    }
}

#[test]
fn a_perch_too_old_for_the_home_flags_is_still_told_so() {
    let _g = port_lock();
    let s = Start::new(Which::PerchOpen, "Options:\n      --bind <BIND>  x\n");
    let out = s.cmd("--no-browser").output().unwrap();
    let (_, e) = text(&out);
    assert_eq!(out.status.code(), Some(1));
    assert!(e.contains("is an old build (it has no --home)"), "{e}");
    assert!(!s.argv.exists(), "it was never started");
}

#[test]
fn colour_forced_in_the_environment_does_not_hide_the_flags_from_the_probe() {
    let _g = port_lock();
    for w in BOTH {
        let s = Start::new(w, HELP_NEW);
        write(&s.env.home.join(".local/bin/aoide"), "#!/bin/sh\n", 0o755);
        // the control: asked for colour, the stub's help really is wrapped in colour codes, which a line-anchored probe cannot read
        let coloured = Command::new(s.env.dir.path().join("perch")).arg("--help").env("CLICOLOR_FORCE", "1").env("STUB_HELP", s.env.dir.path().join("help.txt")).output().unwrap();
        assert!(String::from_utf8_lossy(&coloured.stdout).contains("\x1b[1m--plugins-dir\x1b[0m"), "{w:?}");
        // the script asks for plain text whatever its environment says, so the newer flags are still passed
        let out = s.cmd("--no-browser").env("CLICOLOR_FORCE", "1").env("FORCE_COLOR", "1").output().unwrap();
        let (o, e) = text(&out);
        assert!(out.status.success(), "{w:?}: {o}{e}");
        let argv: Vec<String> = fs::read_to_string(&s.argv).unwrap().lines().map(str::to_string).collect();
        assert_eq!(argv, s.newer_argv(), "{w:?}");
    }
}

#[test]
fn a_perch_on_another_port_than_the_default_gets_no_agent_files() {
    let _g = port_lock();
    for w in BOTH {
        // the default (MINERVA_PERCH_PORT) is 48111; this perch is started on 48110 with --port: a second perch
        let s = Start::new(w, HELP_NEW);
        write(&s.env.home.join(".local/bin/aoide"), "#!/bin/sh\n", 0o755);
        let out = s.cmd("--no-browser").env("MINERVA_PERCH_PORT", "48111").args(["--port", &STUB_PORT.to_string()]).output().unwrap();
        let (o, e) = text(&out);
        assert!(out.status.success(), "{w:?}: {o}{e}");
        let argv: Vec<String> = fs::read_to_string(&s.argv).unwrap().lines().map(str::to_string).collect();
        for f in ["--term-token-file", "--mail-token-file", "--agent-url-file"] {
            assert!(!argv.contains(&f.to_string()), "{w:?}: {f} went to a second perch: {argv:?}");
        }
        // what is not the member's single set of files still goes in
        let mut want = s.base_argv();
        want.extend(["--aoide".into(), format!("{}/.local/bin/aoide", s.env.home.display()), "--plugins-dir".into(), s.plugins.display().to_string()]);
        assert_eq!(argv, want, "{w:?}");
        // and the files the member already has were not touched
        for f in ["minerva-term.token", "minerva-mail.token"] {
            assert_eq!(fs::read_to_string(s.home_eidolon().join(f)).unwrap(), DECOY, "{w:?}: {f}");
        }
    }
    // --port equal to the default is the default perch: it gets them
    for w in BOTH {
        let s = Start::new(w, HELP_NEW);
        write(&s.env.home.join(".local/bin/aoide"), "#!/bin/sh\n", 0o755);
        let out = s.cmd("--no-browser").args(["--port", &STUB_PORT.to_string()]).output().unwrap();
        assert!(out.status.success(), "{w:?}: {}", text(&out).1);
        let argv: Vec<String> = fs::read_to_string(&s.argv).unwrap().lines().map(str::to_string).collect();
        assert_eq!(argv, s.newer_argv(), "{w:?}");
    }
}
