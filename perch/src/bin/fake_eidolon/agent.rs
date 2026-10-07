//! `FAKE_AGENT_CALL`: the fake door calling the perch as an agent's tool would (slice AG).
//!
//! After a `say` has been answered 202, this door itself POSTs `/hub/<scope>/a/<verb>` to the perch, **from a socket this process
//! owns**, with the scoped token the perch's flags wrote to a file, and records what came back to `$FAKE_LOG`. That is the case the
//! perch's attribution must accept: the peer socket belongs to a live door's pid. With `FAKE_AGENT_VIA=child` the same call is made by
//! `sh -c curl`, so it arrives from a child of the door, which the perch must refuse.
//!
//! The knobs are all environment, which a door inherits from the perch (`Perch::builder().env(..)`, `FAKE_PERCH_ENV`):
//!
//! ```text
//!   FAKE_AGENT_CALL=<call>[\n<call>...]    what to call on every say, in order, each after the one before has answered
//!   FAKE_AGENT_CALL=@<file>                the same, read from that file at every say, so a test can change the script between says
//!   FAKE_AGENT_URL_FILE=<file>             what --agent-url-file wrote:        http://127.0.0.1:<port>/
//!   FAKE_AGENT_TERM_TOKEN_FILE=<file>      what --term-token-file wrote        (a term call's Bearer)
//!   FAKE_AGENT_MAIL_TOKEN_FILE=<file>      what --mail-token-file wrote        (a mail call's Bearer)
//!   FAKE_AGENT_VIA=door|child              door (default): this process's own socket;  child: `sh -c curl`, a child's socket
//! ```
//!
//! A `<call>` is `[<mods>][<scope>/]<verb>:<json body>`: `whoami:{}` and `term/open:{"title":"a"}` are term calls (the default scope),
//! and `mail/send:{"node":"n","subject":"s","text":"t"}` is a mail call, because `send` is a verb of both. Blank lines and lines
//! starting with `#` are skipped. In a body, the literal `$id` is replaced by the `id` of the last answer that had one, so a script can
//! open a terminal and then write to it: `term/open:{}` then `term/send:{"id":"$id","text":"echo hi","keys":["enter"]}`.
//!
//! `<mods>` is `[k=v,k=v]` in front of the call, to make it the way a caller that is not a tool would: `[method=GET]term/whoami:{}`,
//! `[host=evil.example]..`, `[sec-fetch-site=cross-site]..`. The method defaults to POST and the host to the perch's own authority.
//!
//! The door's HTTP is hand-written, and does what a client built on hyper-util or reqwest does about proxies: with `http_proxy`,
//! `HTTP_PROXY`, `all_proxy` or `ALL_PROXY` set and the perch's host not in `no_proxy`/`NO_PROXY`, it connects to the proxy and not
//! to the perch. (The perch's fork gives every door a `NO_PROXY` that lists loopback; a test that sets a dead proxy shows it works.)
//!
//! Each call is one line of `$FAKE_LOG`, which a test reads with `Perch::fake_records()`:
//!
//! ```text
//!   agent pid=<door pid> via=<door|child> <scope>/<verb> <http status> <answer body, on one line>
//!   agent pid=<door pid> via=<door|child> <scope>/<verb> error <why>          (no connection, no file, a bad call line)
//! ```
//!
//! No token is ever in argv, in the environment or in the log: it is read from its file, and `child` hands curl a `0600` header file.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use super::Fake;

/// The longest a call may take: a terminal read can wait a minute on the perch's side.
const CALL_LIMIT: Duration = Duration::from_secs(90);

/// `POST /api/say` was accepted and answered 202. Make the calls `FAKE_AGENT_CALL` names, if it is set; spawn rather than block, because
/// this runs on the connection that carried the `say`, and the perch's answer to the agent call may depend on that connection being free.
pub fn after_say(fake: &Arc<Fake>) {
    let Some(script) = std::env::var("FAKE_AGENT_CALL").ok().filter(|script| !script.trim().is_empty()) else { return };
    let fake = fake.clone();
    tokio::spawn(async move { run(&fake, &script).await });
}

async fn run(fake: &Fake, script: &str) {
    let text = match script.strip_prefix('@') {
        Some(file) => std::fs::read_to_string(file).unwrap_or_default(),
        None => script.to_string(),
    };
    let via = if std::env::var("FAKE_AGENT_VIA").is_ok_and(|via| via == "child") { "child" } else { "door" };
    let pid = std::process::id();
    let mut last_id: Option<String> = None;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let (mods, line) = match split_mods(line) {
            Ok(split) => split,
            Err(why) => {
                fake.record(&format!("agent pid={pid} via={via} {line} error {why}"));
                continue;
            }
        };
        let Some((target, body)) = line.split_once(':') else {
            fake.record(&format!("agent pid={pid} via={via} {line} error a call is <verb>:<json>"));
            continue;
        };
        let (scope, verb) = target.split_once('/').unwrap_or(("term", target));
        let named = format!("{scope}/{verb}");
        if !matches!(scope, "term" | "mail") || verb.is_empty() || !verb.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') {
            fake.record(&format!("agent pid={pid} via={via} {named} error the scope is term or mail and the verb is lower case"));
            continue;
        }
        let body = match &last_id {
            Some(id) => body.replace("$id", id),
            None => body.to_string(),
        };
        let called = tokio::time::timeout(CALL_LIMIT, async {
            if via == "child" { call_child(scope, verb, &body, &mods).await } else { call_door(scope, verb, &body, &mods).await }
        })
        .await
        .unwrap_or_else(|_| Err("no answer in time".into()));
        match called {
            Ok((status, answer)) => {
                fake.record(&format!("agent pid={pid} via={via} {named} {status} {}", answer.replace(['\r', '\n'], " ")));
                if let Some(id) = serde_json::from_str::<serde_json::Value>(&answer).ok().and_then(|v| v["id"].as_str().map(str::to_string)) {
                    last_id = Some(id);
                }
            }
            Err(why) => fake.record(&format!("agent pid={pid} via={via} {named} error {why}")),
        }
    }
}

/// A file a flag of the perch wrote, named by one of this door's variables, read and trimmed.
fn read_named(variable: &str) -> Result<String, String> {
    let path = std::env::var_os(variable).map(PathBuf::from).ok_or_else(|| format!("{variable} is not set"))?;
    std::fs::read_to_string(&path).map(|text| text.trim().to_string()).map_err(|e| format!("{}: {e}", path.display()))
}

fn token_variable(scope: &str) -> &'static str {
    if scope == "mail" { "FAKE_AGENT_MAIL_TOKEN_FILE" } else { "FAKE_AGENT_TERM_TOKEN_FILE" }
}

/// `http://127.0.0.1:<port>/` -> `127.0.0.1:<port>`.
fn authority_of(url: &str) -> Result<&str, String> {
    url.strip_prefix("http://").and_then(|rest| rest.strip_suffix('/')).ok_or_else(|| format!("the url file holds {url:?}, not http://<authority>/"))
}

/// How a call differs from a tool's: the method, the `Host`, the fetch metadata. Each is what a test sets to ask one gate one question.
#[derive(Default)]
struct Mods {
    method: Option<String>,
    host: Option<String>,
    site: Option<String>,
}

/// `[k=v,k=v]call` -> the modifiers and the call. A line with no `[` has none.
fn split_mods(line: &str) -> Result<(Mods, &str), String> {
    let Some(rest) = line.strip_prefix('[') else { return Ok((Mods::default(), line)) };
    let (list, call) = rest.split_once(']').ok_or("a [ with no ]")?;
    let mut mods = Mods::default();
    for item in list.split(',').map(str::trim).filter(|item| !item.is_empty()) {
        let (key, value) = item.split_once('=').ok_or_else(|| format!("{item:?} is not k=v"))?;
        if value.is_empty() || value.bytes().any(|b| b < 0x20 || b == 0x7f) {
            return Err(format!("the value of {key} is empty or has a control character"));
        }
        match key {
            "method" => mods.method = Some(value.to_ascii_uppercase()),
            "host" => mods.host = Some(value.to_string()),
            "sec-fetch-site" => mods.site = Some(value.to_string()),
            other => return Err(format!("{other:?} is not a modifier (method, host, sec-fetch-site)")),
        }
    }
    Ok((mods, call))
}

/// The host of an `authority` (`127.0.0.1:4477` -> `127.0.0.1`, `[::1]:4477` -> `::1`).
fn host_of(authority: &str) -> &str {
    let host = authority.rsplit_once(':').map_or(authority, |(host, _)| host);
    host.trim_start_matches('[').trim_end_matches(']')
}

/// The proxy a hyper-util or reqwest client would send a request for `host` to, as an authority, or `None` for a direct connection:
/// the first of `http_proxy`, `HTTP_PROXY`, `all_proxy`, `ALL_PROXY` that is set, unless `host` is in the first of `no_proxy`,
/// `NO_PROXY` that is set (or the list is `*`). `env` is the variable lookup, so a test can give it one.
fn proxy_choice(env: impl Fn(&str) -> Option<String>, host: &str) -> Option<String> {
    let first = |names: &[&str]| names.iter().find_map(|name| env(name).filter(|value| !value.trim().is_empty()));
    let proxy = first(&["http_proxy", "HTTP_PROXY", "all_proxy", "ALL_PROXY"])?;
    let skip = first(&["no_proxy", "NO_PROXY"]).unwrap_or_default();
    if skip.split(',').map(str::trim).any(|entry| entry == "*" || entry.eq_ignore_ascii_case(host)) {
        return None;
    }
    Some(proxy.trim().strip_prefix("http://").unwrap_or(proxy.trim()).trim_end_matches('/').to_string())
}

/// The call from this process's own socket: one connection, one request, read to the close.
async fn call_door(scope: &str, verb: &str, body: &str, mods: &Mods) -> Result<(u16, String), String> {
    let url = read_named("FAKE_AGENT_URL_FILE")?;
    let authority = authority_of(&url)?;
    let token = read_named(token_variable(scope))?;
    let method = mods.method.as_deref().unwrap_or("POST");
    let host = mods.host.as_deref().unwrap_or(authority);
    let site = mods.site.as_deref().map(|site| format!("Sec-Fetch-Site: {site}\r\n")).unwrap_or_default();
    // Through a proxy, a request names the whole URL; direct, only the path.
    let (connect_to, target, through) = match proxy_choice(|name| std::env::var(name).ok(), host_of(authority)) {
        Some(proxy) => (proxy, format!("http://{authority}/hub/{scope}/a/{verb}"), " (the proxy)"),
        None => (authority.to_string(), format!("/hub/{scope}/a/{verb}"), ""),
    };
    let mut sock = TcpStream::connect(&connect_to).await.map_err(|e| format!("connecting to {connect_to}{through}: {e}"))?;
    let request = format!(
        "{method} {target} HTTP/1.1\r\nHost: {host}\r\n{site}Authorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(request.as_bytes()).await.map_err(|e| format!("sending: {e}"))?;
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.map_err(|e| format!("reading: {e}"))?;
    parse_answer(&raw)
}

/// The same call made by `sh -c curl`: a child of this door, with a socket of its own. The token reaches curl in a `0600` header file
/// (`-H @file`), never in argv.
async fn call_child(scope: &str, verb: &str, body: &str, mods: &Mods) -> Result<(u16, String), String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::io::Write as _;
    let url = read_named("FAKE_AGENT_URL_FILE")?;
    let authority = authority_of(&url)?;
    let token = read_named(token_variable(scope))?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let header = std::env::temp_dir().join(format!("fake-agent-{}-{nanos}.hdr", std::process::id()));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&header).map_err(|e| format!("{}: {e}", header.display()))?;
    writeln!(file, "Authorization: Bearer {token}").map_err(|e| format!("{}: {e}", header.display()))?;
    for (name, value) in [("Host", &mods.host), ("Sec-Fetch-Site", &mods.site)] {
        if let Some(value) = value {
            writeln!(file, "{name}: {value}").map_err(|e| format!("{}: {e}", header.display()))?;
        }
    }
    drop(file);
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(r#"curl --silent --show-error --max-time 80 --request "$4" --header @"$1" --header 'Content-Type: application/json' --data-binary "$2" --write-out '\n%{http_code}' "$3""#)
        .arg("sh")
        .arg(&header)
        .arg(body)
        .arg(format!("http://{authority}/hub/{scope}/a/{verb}"))
        .arg(mods.method.as_deref().unwrap_or("POST"))
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    let _ = std::fs::remove_file(&header);
    let output = output.map_err(|e| format!("running sh: {e}"))?;
    if !output.status.success() {
        return Err(format!("curl exited {}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let (answer, status) = text.rsplit_once('\n').ok_or_else(|| format!("curl printed {text:?}"))?;
    Ok((status.trim().parse().map_err(|_| format!("curl's status line was {status:?}"))?, answer.to_string()))
}

/// The status and the body of one HTTP/1.1 answer read to the end, a chunked body decoded.
fn parse_answer(raw: &[u8]) -> Result<(u16, String), String> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| format!("no head in {:?}", String::from_utf8_lossy(raw)))?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head.split_whitespace().nth(1).and_then(|code| code.parse().ok()).ok_or_else(|| format!("no status in {head:?}"))?;
    let rest = &raw[split + 4..];
    let chunked = head.lines().any(|line| line.to_ascii_lowercase().replace(' ', "") == "transfer-encoding:chunked");
    let body = if chunked { dechunk(rest) } else { rest.to_vec() };
    Ok((status, String::from_utf8_lossy(&body).to_string()))
}

fn dechunk(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(eol) = raw[at..].windows(2).position(|w| w == b"\r\n") {
        let size = String::from_utf8_lossy(&raw[at..at + eol]).to_string();
        let Ok(size) = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16) else { break };
        let start = at + eol + 2;
        if size == 0 || raw.len() < start + size {
            break;
        }
        out.extend_from_slice(&raw[start..start + size]);
        at = start + size + 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_its_status_and_its_body_chunked_or_not() {
        let plain = b"HTTP/1.1 403 Forbidden\r\ncontent-type: application/json\r\ncontent-length: 5\r\n\r\n{\"a\"}";
        assert_eq!(parse_answer(plain).unwrap(), (403, "{\"a\"}".to_string()));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse_answer(chunked).unwrap(), (200, "abcde".to_string()));
        assert!(parse_answer(b"nonsense").is_err());
    }

    #[test]
    fn modifiers_come_before_the_call_and_a_bad_one_is_refused() {
        let (mods, call) = split_mods("[method=get, host=evil.example:80,sec-fetch-site=cross-site]term/whoami:{}").unwrap();
        assert_eq!((mods.method.as_deref(), mods.host.as_deref(), mods.site.as_deref()), (Some("GET"), Some("evil.example:80"), Some("cross-site")));
        assert_eq!(call, "term/whoami:{}");
        let (none, call) = split_mods("mail/send:{}").unwrap();
        assert!(none.method.is_none() && none.host.is_none() && none.site.is_none());
        assert_eq!(call, "mail/send:{}");
        for bad in ["[method=GET term/whoami:{}", "[nothing]whoami:{}", "[cookie=x]whoami:{}", "[host=]whoami:{}", "[host=a\rb]whoami:{}"] {
            assert!(split_mods(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_client_goes_to_its_proxy_unless_the_host_is_in_no_proxy() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| value.to_string())
        };
        assert_eq!(host_of("127.0.0.1:4477"), "127.0.0.1");
        assert_eq!(host_of("[::1]:4477"), "::1");
        // No proxy configured: direct.
        assert_eq!(proxy_choice(env(&[]), "127.0.0.1"), None);
        // A proxy, and nothing about loopback: the call goes to the proxy. This is what the perch's fork has to prevent.
        assert_eq!(proxy_choice(env(&[("http_proxy", "http://127.0.0.1:3128/")]), "127.0.0.1").as_deref(), Some("127.0.0.1:3128"));
        assert_eq!(proxy_choice(env(&[("ALL_PROXY", "proxy.corp:8080")]), "127.0.0.1").as_deref(), Some("proxy.corp:8080"));
        assert_eq!(proxy_choice(env(&[("HTTP_PROXY", "http://p:1"), ("NO_PROXY", "corp.example")]), "127.0.0.1").as_deref(), Some("p:1"));
        // Loopback listed (either spelling of either variable): direct.
        assert_eq!(proxy_choice(env(&[("http_proxy", "http://p:1"), ("NO_PROXY", "corp.example,127.0.0.1,localhost")]), "127.0.0.1"), None);
        assert_eq!(proxy_choice(env(&[("HTTP_PROXY", "http://p:1"), ("no_proxy", "*")]), "127.0.0.1"), None);
        assert_eq!(proxy_choice(env(&[("http_proxy", "http://p:1"), ("no_proxy", "::1")]), "::1"), None);
        // An empty variable is not set.
        assert_eq!(proxy_choice(env(&[("http_proxy", " ")]), "127.0.0.1"), None);
    }

    #[test]
    fn the_url_file_names_an_authority() {
        assert_eq!(authority_of("http://127.0.0.1:48112/").unwrap(), "127.0.0.1:48112");
        assert!(authority_of("127.0.0.1:48112").is_err());
        assert!(authority_of("http://127.0.0.1:48112/x").is_err());
    }
}
