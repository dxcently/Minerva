//! `GET /hub/runs/health` (slice R1a): the probes of `--jev-health` and `--browser-health`, a second at most each, cached for a few.
//!
//! ```text
//!   GET /hub/runs/health  {"jev": S, "browser": S, "at_ms": N}
//!     S = {"configured": bool, "up": bool, "start": "<the command that starts it>", "why"?: "<why it is down>", ...its own fields}
//! ```
//!
//! **Only what jev's `/health` says is passed on.** jev's J1 fields (`status`, `jevlike`, `openjev`, `decisions_log`, `ckpt`, `loaded`,
//! `openjev_dtype`) ride along **verbatim** inside the service's object: the perch does not read them, and nothing here refuses a run for
//! one of them (the operator ruled on 2026-10-07 that a run is never refused over `decisions_log`; the graph tab *shows* it). A field an
//! old jev lacks is simply absent, and the page says "unknown". The browser service passes on `status` and nothing else. Any other key is
//! left out, so a service cannot make the answer large or put its own words in front of a person.
//!
//! **What the perch will connect to.** A health URL is `http://127.0.0.1:<port>/<path>` (or `localhost`, which is read as the same
//! address, 127.0.0.1): the flag is the operator's, but the probe is a request the perch makes, so any other host is refused as a probe, not
//! connected to. A down service is `up: false` with a `why` (refused, timed out, an answer that is not JSON); a service with no flag is
//! `configured: false` and counts as nothing to wait for.
//!
//! **The cache** holds the answer for [`TTL`]; the lock is held across the probe, so two tabs asking together cost one probe. The start
//! gate ([`gate`]) does not use it: a start must see the service as it is now, and a stale "up" would let a run begin against a jev
//! that has just gone. The gate's probe refreshes the cache, so the next poll shows what the gate saw.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty};
use hyper::body::Incoming;
use hyper::header::HOST;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Map, Value};
use tokio::net::TcpStream;

use crate::serve::{status, BoxBody, Ctx};

/// How long one probe may take, connect to last byte.
const WITHIN: Duration = Duration::from_secs(1);
/// How long an answer is trusted.
pub const TTL: Duration = Duration::from_secs(5);
/// A `/health` is a few hundred bytes; a service that sends more than this is not asked further.
const BODY_CAP: usize = 64 * 1024;

/// jev's `/health` fields the page shows (J1), passed on as they came.
const JEV_FIELDS: &[&str] = &["status", "jevlike", "openjev", "decisions_log", "ckpt", "loaded", "openjev_dtype"];
const BROWSER_FIELDS: &[&str] = &["status"];

/// What the page is told to run to bring a service up. Nothing in the page starts one.
pub const START: &str = "minerva services start";

/// The last answer and when it was made. `Arc`, so a hit is a clone of a pointer.
#[derive(Default)]
pub struct Cache {
    slot: tokio::sync::Mutex<Option<(Instant, Arc<Value>)>>,
}

/// One service as the last probe saw it.
struct Seen {
    name: &'static str,
    configured: bool,
    up: bool,
    why: Option<String>,
    fields: Map<String, Value>,
}

impl Seen {
    fn json(&self) -> Value {
        let mut out = Map::new();
        out.insert("configured".into(), self.configured.into());
        out.insert("up".into(), self.up.into());
        out.insert("start".into(), START.into());
        if let Some(why) = &self.why {
            out.insert("why".into(), why.clone().into());
        }
        for (key, value) in &self.fields {
            // A service's own field never shadows the perch's.
            out.entry(key.clone()).or_insert_with(|| value.clone());
        }
        Value::Object(out)
    }

    /// The sentence for a start refused because this service is down; `None` when it is up or there is nothing to wait for.
    fn refusal(&self) -> Option<String> {
        if !self.configured || self.up {
            return None;
        }
        let name = if self.name == "jev" { "jev" } else { "the browser service" };
        let why = self.why.as_deref().map(|why| format!(" ({why})")).unwrap_or_default();
        Some(format!("{name} is not running{why}; start it with: {START}"))
    }
}

/// `http://127.0.0.1:<port>/<path>` as `(port, path)`; `None` for anything else. The host is a loopback name or address and nothing the
/// perch has to resolve: a health flag is not a way to make the perch connect elsewhere.
pub(crate) fn parse_url(url: &str) -> Option<(u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let (host, port) = authority.rsplit_once(':')?;
    if !matches!(host, "127.0.0.1" | "localhost") || port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port: u16 = port.parse().ok().filter(|port| *port != 0)?;
    // The path goes into a request line: printable ASCII and no space, so it cannot break out of the line.
    if !path.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    Some((port, path.to_string()))
}

/// One GET of a loopback `/health`, bounded by [`WITHIN`] as a whole. The body is a JSON object or the probe failed.
async fn fetch(port: u16, path: &str) -> Result<Map<String, Value>, String> {
    let work = async {
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::ConnectionRefused => "connection refused".to_string(),
            _ => format!("could not connect: {e}"),
        })?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.map_err(|e| format!("not HTTP: {e}"))?;
        // Aborted below, which drops the socket: the probe leaves nothing open.
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        let request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header(HOST, format!("127.0.0.1:{port}"))
            .header("accept", "application/json")
            .body(Empty::<Bytes>::new())
            .map_err(|e| format!("bad request: {e}"))?;
        let answered = async {
            let response = sender.send_request(request).await.map_err(|e| format!("no answer: {e}"))?;
            if !response.status().is_success() {
                return Err(format!("answered {}", response.status().as_u16()));
            }
            // Frame by frame to a cap: this is a response the perch asked for, not a request body, so it is not `serve::body_of`'s.
            let mut incoming = response.into_body();
            let mut bytes: Vec<u8> = Vec::new();
            while let Some(frame) = incoming.frame().await {
                let frame = frame.map_err(|_| "the answer broke off".to_string())?;
                if let Some(data) = frame.data_ref() {
                    if bytes.len() + data.len() > BODY_CAP {
                        return Err("the answer is too long".to_string());
                    }
                    bytes.extend_from_slice(data);
                }
            }
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Object(map)) => Ok(map),
                _ => Err("the answer is not a JSON object".to_string()),
            }
        }
        .await;
        driver.abort();
        answered
    };
    tokio::time::timeout(WITHIN, work).await.unwrap_or_else(|_| Err("did not answer in time".to_string()))
}

async fn probe(name: &'static str, url: Option<&str>, keep: &[&str]) -> Seen {
    let mut seen = Seen { name, configured: url.is_some(), up: false, why: None, fields: Map::new() };
    let Some(url) = url else { return seen };
    let Some((port, path)) = parse_url(url) else {
        seen.why = Some("its health address is not a loopback http address".into());
        return seen;
    };
    match fetch(port, &path).await {
        Ok(body) => {
            seen.up = true;
            seen.fields = body.into_iter().filter(|(key, _)| keep.contains(&key.as_str())).collect();
        }
        Err(why) => seen.why = Some(why),
    }
    seen
}

/// Both probes, side by side: a dead jev must not cost the browser's second.
async fn probe_all(ctx: &Ctx) -> (Seen, Seen) {
    tokio::join!(
        probe("jev", ctx.opts.jev_health.as_deref(), JEV_FIELDS),
        probe("browser", ctx.opts.browser_health.as_deref(), BROWSER_FIELDS),
    )
}

fn report(jev: &Seen, browser: &Seen) -> Value {
    json!({ "jev": jev.json(), "browser": browser.json(), "at_ms": crate::archive::now_ms() })
}

/// `GET /hub/runs/health`.
pub async fn get(ctx: &Ctx, req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    if req.method() != Method::GET {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    }
    let mut slot = ctx.runs.health.slot.lock().await;
    if let Some((at, value)) = slot.as_ref()
        && at.elapsed() < TTL
    {
        return crate::serve::json(StatusCode::OK, &**value);
    }
    let (jev, browser) = probe_all(ctx).await;
    let value = Arc::new(report(&jev, &browser));
    *slot = Some((Instant::now(), value.clone()));
    crate::serve::json(StatusCode::OK, &*value)
}

/// The start gate (and the answer's): both services must be up, as they are right now. `Err` is the 503, with the sentence and each
/// service's start command. A service with no health flag is not waited for.
pub(crate) async fn gate(ctx: &Ctx) -> Result<(), Box<Response<BoxBody>>> {
    let (jev, browser) = probe_all(ctx).await;
    *ctx.runs.health.slot.lock().await = Some((Instant::now(), Arc::new(report(&jev, &browser))));
    let down: Vec<String> = [jev.refusal(), browser.refusal()].into_iter().flatten().collect();
    if down.is_empty() {
        return Ok(());
    }
    Err(Box::new(super::state::refuse(StatusCode::SERVICE_UNAVAILABLE, &down.join(" · "), json!({}))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_health_address_is_a_loopback_http_url_and_nothing_else() {
        assert_eq!(parse_url("http://127.0.0.1:8091/health"), Some((8091, "/health".to_string())));
        assert_eq!(parse_url("http://localhost:48140/health?x=1"), Some((48140, "/health?x=1".to_string())));
        assert_eq!(parse_url("http://127.0.0.1:8091"), Some((8091, "/".to_string())), "no path is the root");
        for bad in [
            "https://127.0.0.1:8091/health",
            "http://10.0.0.5:8091/health",
            "http://example.com:80/health",
            "http://127.0.0.1.evil.test:8091/health",
            "http://user@127.0.0.1:8091/health",
            "http://127.0.0.1/health",
            "http://[::1]:9/health",
            "http://127.0.0.1:0/health",
            "http://127.0.0.1:99999/health",
            "http://127.0.0.1:80a/health",
            "http://127.0.0.1:8091/he alth",
            "http://127.0.0.1:8091/h\r\nx: y",
            "127.0.0.1:8091/health",
            "",
        ] {
            assert_eq!(parse_url(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_down_service_says_why_and_how_to_start_it_and_an_unconfigured_one_says_nothing() {
        let down = Seen { name: "jev", configured: true, up: false, why: Some("connection refused".into()), fields: Map::new() };
        assert_eq!(down.refusal().as_deref(), Some("jev is not running (connection refused); start it with: minerva services start"));
        let shown = down.json();
        assert_eq!((shown["configured"].as_bool(), shown["up"].as_bool(), shown["start"].as_str()), (Some(true), Some(false), Some(START)));
        let browser = Seen { name: "browser", configured: true, up: false, why: None, fields: Map::new() };
        assert!(browser.refusal().unwrap().starts_with("the browser service is not running; start it with:"));
        let up = Seen { name: "jev", configured: true, up: true, why: None, fields: Map::new() };
        let none = Seen { name: "jev", configured: false, up: false, why: None, fields: Map::new() };
        assert!(up.refusal().is_none() && none.refusal().is_none(), "up, or nothing to wait for");
    }

    #[test]
    fn a_services_own_fields_never_shadow_the_perchs() {
        let mut fields = Map::new();
        fields.insert("up".into(), json!("lies"));
        fields.insert("ckpt".into(), json!("jev-base-v0.pt"));
        let seen = Seen { name: "jev", configured: true, up: true, why: None, fields };
        let shown = seen.json();
        assert_eq!(shown["up"], json!(true));
        assert_eq!(shown["ckpt"], json!("jev-base-v0.pt"));
    }
}
