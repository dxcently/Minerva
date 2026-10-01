//! The service against a real Chromium, over its real HTTP interface.
//!
//! Skipped unless `EIDOLON_BROWSER_CHROME` names a Chromium binary:
//!
//! ```text
//! EIDOLON_BROWSER_CHROME=$(nix build nixpkgs#chromium --no-link --print-out-paths)/bin/chromium cargo test
//! ```
//!
//! Each test starts its own service on its own port, and the confinement
//! tests two tiny HTTP servers (A and B) that record every path they serve,
//! so "B was never reached" is a checked fact, not a hope.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Service {
    child: Child,
    port: u16,
}

impl Service {
    fn start() -> Option<Service> {
        std::env::var_os("EIDOLON_BROWSER_CHROME")?;
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = Command::new(env!("CARGO_BIN_EXE_eidolon-browser"))
            .env("EIDOLON_SERVICE_PORT", port.to_string())
            .env("EIDOLON_SERVICE_TOKEN", "test-token")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let s = Service { child, port };
        let deadline = Instant::now() + Duration::from_secs(10);
        while s.raw("GET", "/health", None, None).is_err() {
            assert!(Instant::now() < deadline, "service never came up");
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(s)
    }

    fn raw(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&str>,
    ) -> std::io::Result<(u16, Value)> {
        let mut sock = TcpStream::connect(("127.0.0.1", self.port))?;
        let body = body.unwrap_or("");
        let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
        write!(
            sock,
            "{method} {path} HTTP/1.1\r\nHost: x\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )?;
        let mut out = String::new();
        sock.read_to_string(&mut out)?;
        let status = out[9..12].parse().unwrap();
        let json = out.split_once("\r\n\r\n").map_or(Value::Null, |(_, b)| {
            serde_json::from_str(b).unwrap_or(Value::Null)
        });
        Ok((status, json))
    }

    /// `result` on success; panics with the error otherwise.
    fn ok(&self, method: &str, args: Value) -> Value {
        let r = self.call(method, args);
        assert_eq!(r["ok"], true, "{method}: {r}");
        r["result"].clone()
    }

    /// The error text; panics if the call succeeded.
    fn err(&self, method: &str, args: Value) -> String {
        let r = self.call(method, args);
        assert_eq!(r["ok"], false, "{method} should have failed: {r}");
        r["error"].as_str().unwrap().to_string()
    }

    fn call(&self, method: &str, args: Value) -> Value {
        let body = json!({ "method": method, "args": args }).to_string();
        self.raw("POST", "/call", Some("test-token"), Some(&body))
            .unwrap()
            .1
    }

    fn snapshot(&self, args: Value) -> (String, String) {
        let text = self.ok("snapshot", args);
        let (head, body) = text.as_str().unwrap().split_once("\n\n").unwrap();
        (head.to_string(), body.to_string())
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
    }
}

/// The ref on the first line of `body` that contains `needle`.
fn ref_of(body: &str, needle: &str) -> String {
    let line = body
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no {needle:?} in:\n{body}"));
    let start = line.find("[ref=").unwrap() + 5;
    line[start..start + line[start..].find(']').unwrap()].to_string()
}

fn chars(head: &str) -> usize {
    head.lines()
        .find_map(|l| l.strip_prefix("chars: "))
        .unwrap()
        .parse()
        .unwrap()
}

// --- fixture servers --------------------------------------------------------

type Route = Box<dyn Fn(&str) -> (u16, Vec<(String, String)>, Vec<u8>) + Send + Sync>;

struct Site {
    origin: String,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Site {
    fn start(route: Route) -> Site {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let (h, route) = (hits.clone(), Arc::new(route));
        std::thread::spawn(move || {
            for sock in listener.incoming().flatten() {
                let (h, route) = (h.clone(), route.clone());
                std::thread::spawn(move || serve(sock, &h, &route));
            }
        });
        Site { origin, hits }
    }

    fn hit(&self, path: &str) -> bool {
        self.hits.lock().unwrap().iter().any(|p| p == path)
    }
}

fn serve(mut sock: TcpStream, hits: &Mutex<Vec<String>>, route: &Route) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match sock.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let path = head.split(' ').nth(1).unwrap_or("/").to_string();
    hits.lock()
        .unwrap()
        .push(path.split('?').next().unwrap().to_string());
    let (status, headers, body) = route(&path);
    let mut out = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let _ = sock.write_all(out.as_bytes());
    let _ = sock.write_all(&body);
}

fn html(body: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
    (
        200,
        vec![("Content-Type".into(), "text/html".into())],
        body.as_bytes().to_vec(),
    )
}

/// B: somewhere a confined page must never reach.
fn site_b() -> Site {
    Site::start(Box::new(|path| match path {
        "/x.html" => html("<html><head><title>X</title></head><body>x</body></html>"),
        "/probe" => html("probed"),
        "/pixel.png" => (
            200,
            vec![("Content-Type".into(), "image/png".into())],
            b"\x89PNG\r\n\x1a\n".to_vec(),
        ),
        _ => (404, vec![], vec![]),
    }))
}

/// A: the confined page's own site, with every way out of it.
fn site_a(b: &str) -> Site {
    let b = b.to_string();
    Site::start(Box::new(move |path| {
        match path.split('?').next().unwrap() {
            "/a.html" => html(&format!(
                "<html><head><title>A</title></head><body><main><h1>A</h1>\
             <a href='/b.html'>same</a> <a href='{b}/x.html'>other</a> <a href='/redirect'>away</a>\
             <a href='/d.bin' download>dl</a> <a href='/b.html' target='_blank'>popup</a>\
             <img src='{b}/pixel.png'><script>fetch('{b}/probe').catch(() => {{}})</script></main></body></html>"
            )),
            "/b.html" => html("<html><head><title>B-page-on-A</title></head><body>b</body></html>"),
            "/form.html" => html(
                "<html><head><title>Form</title></head><body><main><form action='/b.html'><input name='q' aria-label='q'></form></main></body></html>",
            ),
            "/redirect" => (
                302,
                vec![("Location".into(), format!("{b}/x.html"))],
                vec![],
            ),
            "/d.bin" => (
                200,
                vec![
                    ("Content-Type".into(), "application/octet-stream".into()),
                    (
                        "Content-Disposition".into(),
                        "attachment; filename=\"d.bin\"".into(),
                    ),
                ],
                b"binary-payload".to_vec(),
            ),
            _ => (404, vec![], vec![]),
        }
    }))
}

const PAGE: &str = "data:text/html,<html><head><title>Ref Test</title></head><body><header><a href='/home'>Home</a></header><main><h1>Welcome</h1><p><a href='https://example.com/next'>the next link</a></p><button>Press me</button><input type='text' placeholder='say something' oninput='document.title=this.value'></main></body></html>";

const ROLES_PAGE: &str = "data:text/html,<html><head><title>Roles</title></head><body><header><a href='/home'>Chrome</a></header><main><h1>T</h1><a href='/direct'>Direct</a><button>Press</button><nav><a href='/nested'>Nested</a></nav></main></body></html>";

const SECTIONS_PAGE: &str = "data:text/html,<html><head><title>Sections</title></head><body><main><h1>T</h1><div role='region' aria-label='e'><h2>Etymology</h2><a href='/word'>Word</a></div><div role='region' aria-label='s'><h2>See also</h2><a href='/domestication'>Domestication</a><a href='/felidae'>Felidae</a><nav><a href='/portal1'>Portal1</a><a href='/portal2'>Portal2</a></nav></div></main></body></html>";

// --- the HTTP contract ------------------------------------------------------

#[test]
fn auth_and_bad_bodies_are_refused_before_anything_runs() {
    let Some(s) = Service::start() else { return };
    let (status, body) = s.raw("GET", "/health", None, None).unwrap();
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");

    for token in [None, Some("wrong")] {
        let (status, body) = s
            .raw("POST", "/call", token, Some(r#"{"method":"read"}"#))
            .unwrap();
        assert_eq!(
            (status, body),
            (401, json!({ "ok": false, "error": "bad token" }))
        );
    }
    for bad in ["not json", "[1,2,3]", "\"just a string\"", "42", "null", ""] {
        let (status, _) = s
            .raw("POST", "/call", Some("test-token"), Some(bad))
            .unwrap();
        assert_eq!(status, 400, "{bad}");
    }
    assert!(s.err("nope", json!({})).contains("nope"));
    assert!(s.err("open", json!({})).contains("url"));
    assert!(s.err("click", json!({})).contains("ref"));
    assert!(s.err("type", json!({ "ref": "e1" })).contains("text"));
}

// --- the basic walk ---------------------------------------------------------

#[test]
fn open_snapshot_click_type_read_back() {
    let Some(s) = Service::start() else { return };
    assert_eq!(s.ok("open", json!({ "url": PAGE }))["title"], "Ref Test");

    let (head, body) = s.snapshot(json!({}));
    assert!(head.contains("title: Ref Test"), "{head}");
    assert!(head.contains("heading: Welcome"), "{head}");
    assert_eq!(chars(&head), body.chars().count());
    assert!(body.contains("link \"Home\""), "{body}");
    let button = ref_of(&body, "button \"Press me\"");
    let link = ref_of(&body, "link \"the next link\"");

    assert_eq!(
        s.ok("click", json!({ "ref": button }))["ref"],
        button.as_str()
    );
    let again = s.err("click", json!({ "ref": button }));
    assert!(
        again.contains(&button) && again.contains("browser_snapshot"),
        "{again}"
    );

    let (_, body) = s.snapshot(json!({}));
    let textbox = ref_of(&body, "textbox");
    let typed = s.ok(
        "type",
        json!({ "ref": textbox, "text": "hello ref", "submit": false }),
    );
    assert_eq!(typed["title"], "hello ref");
    assert!(
        s.err("click", json!({ "ref": link }))
            .contains("browser_snapshot")
    );

    let read = s.ok("read", json!({ "max": 8000 }));
    assert!(read["text"].as_str().unwrap().contains("Welcome"));
    assert_eq!(read["truncated"], false);
    let short = s.ok("read", json!({ "max": 5 }));
    assert_eq!(short["text"].as_str().unwrap().chars().count(), 5);
    assert_eq!(short["truncated"], true);

    assert_eq!(s.ok("back", json!({}))["went_back"], false);
}

#[test]
fn back_between_two_http_pages_lands_and_says_so() {
    let Some(s) = Service::start() else { return };
    let b = site_b();
    let a = site_a(&b.origin);
    s.ok("open", json!({ "url": format!("{}/a.html", a.origin) }));
    s.ok("open", json!({ "url": format!("{}/b.html", a.origin) }));
    let back = s.ok("back", json!({}));
    assert_eq!(back["went_back"], true);
    assert!(back["url"].as_str().unwrap().ends_with("/a.html"), "{back}");
    // The document we came back to is really the one being read now.
    let (_, body) = s.snapshot(json!({}));
    assert!(body.contains("link \"other\""), "{body}");
}

#[test]
fn refs_from_a_later_navigation_click_and_old_ones_do_not() {
    let Some(s) = Service::start() else { return };
    s.ok("open", json!({ "url": PAGE }));
    let (_, first) = s.snapshot(json!({}));
    let old = ref_of(&first, "button \"Press me\"");
    s.ok("open", json!({ "url": ROLES_PAGE }));
    assert_eq!(s.ok("back", json!({}))["went_back"], true);
    s.ok("open", json!({ "url": ROLES_PAGE }));
    let (_, second) = s.snapshot(json!({}));
    assert!(
        s.err("click", json!({ "ref": old }))
            .contains("browser_snapshot")
    );
    s.ok(
        "click",
        json!({ "ref": ref_of(&second, "button \"Press\"") }),
    );
}

#[test]
fn a_dialog_does_not_hang_the_page() {
    let Some(s) = Service::start() else { return };
    s.ok("open", json!({ "url": "data:text/html,<title>D</title><button onclick=\"alert('hi');document.title='after'\">Alert</button>" }));
    let (_, body) = s.snapshot(json!({}));
    let clicked = s.ok("click", json!({ "ref": ref_of(&body, "button") }));
    assert_eq!(clicked["title"], "after");
}

// --- scoped and filtered snapshots --------------------------------------------

#[test]
fn within_roots_the_tree_and_names_exactly_one() {
    let Some(s) = Service::start() else { return };
    s.ok("open", json!({ "url": "data:text/html,<title>S</title><nav><a href='/skip'>Skip</a></nav><main><h1>T</h1><a href='/in'>In</a></main>" }));
    let (head, body) = s.snapshot(json!({ "within": "main" }));
    assert!(head.contains("scope: main"));
    assert_eq!(chars(&head), body.chars().count());
    assert!(body.starts_with("- main"), "{body}");
    assert!(
        !body.contains("Skip") && !body.contains("navigation"),
        "{body}"
    );
    s.ok("click", json!({ "ref": ref_of(&body, "link \"In\"") }));

    s.ok("open", json!({ "url": "about:blank" }));
    let err = s.err("snapshot", json!({ "within": "main" }));
    assert!(
        err.contains("main") && err.contains('0') && err.contains("about:blank"),
        "{err}"
    );

    s.ok("open", json!({ "url": "data:text/html,<section role='region' aria-label='a'>a</section><section role='region' aria-label='b'>b</section>" }));
    assert!(
        s.err("snapshot", json!({ "within": "region" }))
            .contains('2')
    );
}

#[test]
fn roles_filter_by_nearest_landmark() {
    let Some(s) = Service::start() else { return };
    s.ok("open", json!({ "url": ROLES_PAGE }));

    let (head, body) = s.snapshot(json!({ "within": "main", "roles": ["link"] }));
    assert!(
        head.contains("scope: main") && head.contains("roles: link") && head.contains("heading: T"),
        "{head}"
    );
    assert_eq!(chars(&head), body.chars().count());
    assert!(body.contains("Direct"), "{body}");
    for gone in ["Chrome", "Press", "Nested"] {
        assert!(!body.contains(gone), "{gone} in {body}");
    }
    s.ok("click", json!({ "ref": ref_of(&body, "Direct") }));

    s.ok("open", json!({ "url": ROLES_PAGE }));
    let (head, body) = s.snapshot(json!({ "roles": ["link"] }));
    assert!(!head.contains("scope:"));
    assert!(
        body.contains("Chrome")
            && body.contains("Direct")
            && body.contains("Nested")
            && !body.contains("Press")
    );

    let (_, body) = s.snapshot(json!({ "within": "main", "roles": ["link"], "depth": 1 }));
    assert!(
        body.contains("Direct") && !body.contains("Nested"),
        "{body}"
    );
}

#[test]
fn max_caps_a_filtered_list() {
    let Some(s) = Service::start() else { return };
    let links: String = (0..10)
        .map(|i| format!("<a href='/l{i}'>L{i}</a> "))
        .collect();
    s.ok(
        "open",
        json!({ "url": format!("data:text/html,<main>{links}</main>") }),
    );
    let (_, body) = s.snapshot(json!({ "within": "main", "roles": ["link"], "max": 3 }));
    let kept: Vec<&str> = body.lines().filter(|l| l.contains("link \"L")).collect();
    assert_eq!(kept.len(), 3, "{body}");
    assert!(kept[0].contains("L0") && kept[2].contains("L2"));
}

#[test]
fn a_section_narrows_to_one_landmark() {
    let Some(s) = Service::start() else { return };
    s.ok("open", json!({ "url": SECTIONS_PAGE }));
    let (head, body) =
        s.snapshot(json!({ "within": "main", "roles": ["link"], "section": "See also" }));
    assert!(head.contains("section: See also"), "{head}");
    assert_eq!(chars(&head), body.chars().count());
    assert!(
        body.contains("Domestication") && body.contains("Felidae"),
        "{body}"
    );
    for gone in ["Word", "Portal1", "Portal2"] {
        assert!(!body.contains(gone), "{gone} in {body}");
    }
    s.ok("click", json!({ "ref": ref_of(&body, "Domestication") }));
}

#[test]
fn bad_snapshot_arguments_are_refused() {
    let Some(s) = Service::start() else { return };
    for bad in [
        json!("link"),
        json!([]),
        json!([1]),
        json!([""]),
        json!(123),
    ] {
        assert!(s.err("snapshot", json!({ "roles": bad })).contains("roles"));
    }
    for bad in [json!(0), json!(-1), json!(1.5), json!("64"), json!(true)] {
        assert!(
            s.err("snapshot", json!({ "roles": ["link"], "max": bad }))
                .contains("max")
        );
    }
    assert!(s.err("snapshot", json!({ "max": 3 })).contains("roles"));
    assert!(
        s.err("snapshot", json!({ "section": "x" }))
            .contains("roles")
    );
    assert!(
        s.err("snapshot", json!({ "roles": ["link"], "section": "x" }))
            .contains("within")
    );
}

// --- confinement --------------------------------------------------------------

#[test]
fn confinement_keeps_a_page_on_its_origins() {
    let Some(s) = Service::start() else { return };
    let b = site_b();
    let a = site_a(&b.origin);

    let opened = s.ok(
        "open",
        json!({ "url": format!("{}/a.html", a.origin), "confine": [a.origin] }),
    );
    assert_eq!(opened["title"], "A");
    assert_eq!(opened["confined"], json!([a.origin]));
    std::thread::sleep(Duration::from_millis(300));
    assert!(a.hit("/a.html"));
    assert!(!b.hit("/pixel.png"), "the <img> reached B");
    assert!(!b.hit("/probe"), "the fetch() reached B");

    // Same origin: fine.
    let (_, body) = s.snapshot(json!({}));
    let same = s.ok("click", json!({ "ref": ref_of(&body, "link \"same\"") }));
    assert!(same["url"].as_str().unwrap().ends_with("/b.html"));

    // Another origin, directly: refused before any request.
    let err = s.err(
        "open",
        json!({ "url": format!("{}/x.html", b.origin), "confine": [a.origin] }),
    );
    assert!(err.contains(&a.origin) && err.contains(&b.origin), "{err}");

    // Another origin, by link.
    s.ok(
        "open",
        json!({ "url": format!("{}/a.html", a.origin), "confine": [a.origin] }),
    );
    let (_, body) = s.snapshot(json!({}));
    let err = s.err("click", json!({ "ref": ref_of(&body, "link \"other\"") }));
    assert!(err.contains("blocked") && err.contains(&b.origin), "{err}");

    // Another origin, by redirect.
    s.ok(
        "open",
        json!({ "url": format!("{}/a.html", a.origin), "confine": [a.origin] }),
    );
    let (_, body) = s.snapshot(json!({}));
    let err = s.err("click", json!({ "ref": ref_of(&body, "link \"away\"") }));
    assert!(err.contains("blocked") && err.contains(&b.origin), "{err}");
    assert!(a.hit("/redirect"));

    assert!(
        !b.hit("/x.html"),
        "B was reached: {:?}",
        b.hits.lock().unwrap()
    );
}

#[test]
fn a_confined_first_open_that_redirects_away_is_blocked() {
    let Some(s) = Service::start() else { return };
    let b = site_b();
    let a = site_a(&b.origin);
    let err = s.err(
        "open",
        json!({ "url": format!("{}/redirect", a.origin), "confine": [a.origin] }),
    );
    assert!(err.contains("blocked"), "{err}");
    assert!(a.hit("/redirect"));
    assert!(!b.hit("/x.html"));
}

#[test]
fn a_lookalike_host_never_resolves() {
    let Some(s) = Service::start() else { return };
    let b = site_b();
    let a = site_a(&b.origin);
    let port = a.origin.rsplit(':').next().unwrap();
    s.ok(
        "open",
        json!({ "url": format!("{}/a.html", a.origin), "confine": [a.origin] }),
    );
    let err = s.err(
        "open",
        json!({ "url": format!("http://127.0.0.1.evil.example:{port}/"), "confine": [a.origin] }),
    );
    assert!(err.contains("not in confine"), "{err}");
}

#[test]
fn a_confined_form_submit_stays_home_and_unconfining_lets_go() {
    let Some(s) = Service::start() else { return };
    let b = site_b();
    let a = site_a(&b.origin);
    s.ok(
        "open",
        json!({ "url": format!("{}/form.html", a.origin), "confine": [a.origin] }),
    );
    let (_, body) = s.snapshot(json!({}));
    let typed = s.ok(
        "type",
        json!({ "ref": ref_of(&body, "textbox"), "text": "hi", "submit": true }),
    );
    assert!(
        typed["url"].as_str().unwrap().contains("/b.html?q=hi"),
        "{typed}"
    );

    // A download is refused, and the page stays usable.
    s.ok(
        "open",
        json!({ "url": format!("{}/a.html", a.origin), "confine": [a.origin] }),
    );
    let (_, body) = s.snapshot(json!({}));
    s.ok("click", json!({ "ref": ref_of(&body, "link \"dl\"") }));
    s.ok("read", json!({}));

    let free = s.ok("open", json!({ "url": format!("{}/x.html", b.origin) }));
    assert_eq!(free["title"], "X");
    assert_eq!(free["confined"], Value::Null);
}
