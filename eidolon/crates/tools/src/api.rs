//! The authenticated-HTTP primitive: one request to a *service* the operator
//! declared, with the credential put in a header here and never a value a
//! script, a tool result or a log can see.
//!
//! An endpoint is a base URL and how a credential rides a request to it: where
//! it is, what header the key goes in, which extra headers every request
//! carries.
//! The *script* carries it — a tool file declares the endpoint it reaches and
//! names the secret to use, so a tool is one file that can be handed to somebody
//! else, and the key stays in the recipient's own store (see
//! `eidolon_rune::endpoint`, which resolves the name and never the value).
//!
//! ## What a caller may name
//!
//! A method, a path and an optional body. A header a call needs and the base URL
//! does not carry (`x-no-cache`, a media type, a version) is the descriptor's:
//! Melete's `api_request` established that a second named endpoint over the same
//! base URL costs less than a per-call header argument, and the trade is the
//! same here — headers are the author's, the arguments are the caller's.
//!
//! The path is appended to the base URL *textually*, so it cannot move the
//! request off the host the descriptor named however it is shaped — and it is
//! frequently a whole URL: `https://r.jina.ai/<a page>` is the reader's own
//! contract, and a tool that reads pages hands its caller's URL straight
//! through.
//!
//! ## What the credential is and is not
//!
//! The resolved token is a parameter: [`request`] takes it as `Option<&str>`,
//! resolved by the caller that holds the store, and the one thing this module
//! does with it is `format!("{prefix}{token}")` into a header value. Nothing
//! here reads a secret store, a file or the environment.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::web::{
    DEFAULT_TIMEOUT, FetchPolicy, approve_target, check_scheme, nofollow_client, pinned_builder,
    read_capped, send_raced,
};

/// The methods a call may name. A method outside this list is refused before a
/// connection opens rather than sent to be misunderstood.
pub const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];

/// One service the operator declared: where it is, and how a key rides a
/// request to it.
///
/// Deserialized from the map a script passes `api_request`, so every field is
/// the *script's* to name — and none of them is ever a value the script holds.
#[derive(Debug, Clone, Deserialize)]
pub struct Endpoint {
    /// What an error quotes this endpoint as. Optional, and usually absent: the
    /// endpoint is right there in the call, and a name would only be for a
    /// person reading a message about it.
    #[serde(default)]
    pub name: String,
    /// The service's root. Every request this primitive makes is this URL plus
    /// the caller's path, and the host is checked afterwards rather than
    /// trusted.
    pub base_url: String,
    /// The name of an entry in the custodied store (`eidolon secret set NAME`).
    /// Preferred for a real key: the value is injected in Rust and is never a
    /// string a script can see.
    #[serde(default)]
    pub token_secret: Option<String>,
    /// A file to read the key from, for an operator who would rather keep it
    /// somewhere they can overwrite by hand (`0600`, like a provider's
    /// `token_file`).
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// An environment variable, for a key the environment already holds.
    #[serde(default)]
    pub token_env: Option<String>,
    /// How the credential rides a request. Absent for a service that needs no
    /// key at all — Jina's reader answers an anonymous request, slower.
    #[serde(default)]
    pub auth: Option<Auth>,
    /// Headers on every request to this service.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Seconds to wait for the response. The descriptor's, so a wrapper that
    /// reaches a slow endpoint says so in the file that reaches it; absent is
    /// [`DEFAULT_TIMEOUT`].
    #[serde(default)]
    pub timeout_s: Option<u64>,
    /// Most characters of the body the *script* gets. The wire read is capped
    /// regardless ([`crate::web::MAX_DOWNLOAD`]); this is the smaller, saner cap
    /// for an endpoint whose list answers are longer than anything a model
    /// wanted, and it sets [`Response::truncated`] when it bites.
    #[serde(default)]
    pub max: Option<usize>,
}

impl Endpoint {
    /// Where a key would have come from, for the error a call gets when the
    /// service wants one and nothing is there. Empty when the service names no
    /// source at all — which is a service that declares `auth` and nowhere to
    /// put a key, worth saying out loud too.
    pub fn credential_hint(&self) -> String {
        let mut places = Vec::new();
        if let Some(name) = &self.token_secret {
            places.push(format!("the secret `{name}` (`eidolon secret set {name}`)"));
        }
        if let Some(path) = &self.token_file {
            places.push(format!("the file {}", path.display()));
        }
        if let Some(var) = &self.token_env {
            places.push(format!("the variable ${var}"));
        }
        if places.is_empty() {
            return "it names no `token_secret`, `token_file` or `token_env`".to_string();
        }
        format!("nothing is in {}", places.join(" or "))
    }
}

/// How a service's credential rides a request.
#[derive(Debug, Clone, Deserialize)]
pub struct Auth {
    /// The header the credential goes in: `Authorization`, `x-api-key`.
    pub header: String,
    /// What goes in front of it. `Bearer ` for a bearer token, empty for a
    /// header that takes the raw key.
    #[serde(default)]
    pub prefix: String,
}

/// One response, as it came back.
///
/// Crosses to a script as JSON text (the host's convention for a structured
/// result), so a caller that wants to branch on the status can and a caller
/// that wants to print it can use [`Response::render`]'s shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// What was asked for, after the service's base URL was applied.
    pub url: String,
    pub status: u16,
    #[serde(default)]
    pub content_type: String,
    pub body: String,
    /// The body was longer than [`crate::web::MAX_DOWNLOAD`] and was cut. Said
    /// out loud
    /// rather than returned as a short answer that looks whole.
    #[serde(default)]
    pub truncated: bool,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Render for a model: what is worth saying about the response, then the
    /// response. The shape is [`crate::web::Page::render`]'s — a status that is
    /// not the ordinary one is a bracketed note above the content, because a
    /// 404 body is frequently the answer to why there was a 404.
    pub fn render(&self) -> String {
        let mut s = String::new();
        if !self.ok() {
            s.push_str(&format!("[HTTP {}]\n", self.status));
        }
        s.push_str(self.body.trim_end());
        if self.truncated {
            s.push_str("\n[content truncated; ask for less, or for one page of it]");
        }
        if s.is_empty() {
            s.push_str("[no content]");
        }
        s
    }
}

/// One request as the caller makes it: what to send, and the session's rule for
/// where it may go. Grouped rather than passed one by one — they travel
/// together, and the signature they would make has outgrown a call site.
#[derive(Debug, Clone, Copy)]
pub struct Call<'a> {
    pub method: &'a str,
    /// Appended to the endpoint's `base_url`, textually.
    pub path: &'a str,
    pub body: Option<&'a str>,
    /// The session's address rule, which this call keeps exactly as `fetch`
    /// does: an endpoint reached from a session nobody reads over is checked
    /// and pinned, because a wrapper is not a way around the posture the
    /// session was assembled with.
    pub policy: FetchPolicy,
}

/// One request, with the credential for `def` if it has one.
///
/// `token` is already resolved: [`request`] formats it into a header and reads
/// it nowhere else. The store, the file and the environment are the caller's
/// business (see `eidolon_rune::endpoint`). How long to wait is the
/// *descriptor's* ([`Endpoint::timeout_s`]), because the caller is a script and
/// the endpoint is the thing that knows.
pub async fn request(
    def: &Endpoint,
    token: Option<&str>,
    call: &Call<'_>,
    cancel: &CancellationToken,
) -> anyhow::Result<Response> {
    let Call {
        method,
        path,
        body,
        policy,
    } = *call;
    let method = method.trim().to_ascii_uppercase();
    if !METHODS.contains(&method.as_str()) {
        bail!(
            "`{method}` is not a method this primitive sends; one of {}",
            METHODS.join(", ")
        );
    }
    let url = url_for(def, path)?;
    // The descriptor's own timeout: a wrapper that reaches a slow endpoint knows
    // it better than a script with no way to say so.
    let timeout = def
        .timeout_s
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT);
    // A call already cancelled opens no connection at all — the same order
    // `web::fetch` keeps, and for the same reason: a race would still dial.
    if cancel.is_cancelled() {
        return Err(anyhow!("cancelled"));
    }

    // Every header, gathered once so the same request can be built against
    // whichever client the policy implies.
    let mut headers = reqwest::header::HeaderMap::new();
    for (k, v) in &def.headers {
        headers.insert(
            reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .with_context(|| format!("{k:?} is not a header name"))?,
            reqwest::header::HeaderValue::from_str(v)
                .with_context(|| format!("{k:?} has a value that cannot be a header"))?,
        );
    }
    // The one place a credential touches a request.
    if let (Some(auth), Some(token)) = (&def.auth, token) {
        let value = format!("{}{}", auth.prefix, token);
        headers.insert(
            reqwest::header::HeaderName::from_bytes(auth.header.as_bytes())
                .with_context(|| format!("{:?} is not a header name", auth.header))?,
            reqwest::header::HeaderValue::from_str(&value)
                .context("this credential is not a usable header value")?,
        );
    }
    let mut payload = None;
    if let Some(body) = body {
        // A body nobody named a type for is JSON: the shape most of these
        // services answer a POST with, and the one a service that wants
        // another declares in its own `headers`. Nothing else is assumed —
        // no `Accept`, since an endpoint that answers differently per media
        // type is a descriptor of its own (Melete's own convention).
        if !def
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("content-type"))
        {
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("application/json"),
            );
        }
        payload = Some(body.to_string());
    }
    let build = |client: &reqwest::Client| -> anyhow::Result<reqwest::RequestBuilder> {
        let mut rb = client
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).context("method")?,
                url.clone(),
            )
            .timeout(timeout)
            .headers(headers.clone());
        if let Some(body) = &payload {
            rb = rb.body(body.clone());
        }
        Ok(rb)
    };

    // No redirects, either branch: a hop would hand the credential to a host
    // the file never named. A 3xx comes back as itself and the caller sees it.
    let what = format!("{method} {url}");
    let resp = match policy {
        FetchPolicy::AnyAddress => {
            let rb = build(nofollow_client()?)?;
            send_raced(rb.send(), &what, cancel).await?
        }
        FetchPolicy::PublicOnly => {
            // Check the target the way `web::fetch` does, dial it pinned to the
            // addresses that passed, and read the response — no walk, because
            // there is no redirect to follow here.
            let (host, addrs) = tokio::select! {
                r = approve_target(&url) => r?,
                _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
            };
            let client = pinned_builder(&host, &addrs)
                .build()
                .context("building the http client")?;
            let rb = build(&client)?;
            send_raced(rb.send(), &what, cancel).await?
        }
    };
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let (bytes, mut truncated) = read_capped(resp, cancel).await?;
    let mut body = String::from_utf8_lossy(&bytes).into_owned();
    // The descriptor's cap is on what the script gets, so it is applied to the
    // decoded text rather than to the wire read: a body this cuts is reported
    // as truncated, the same as one the download cap cut.
    if let Some(max) = def.max
        && let Some(cut) = body.char_indices().nth(max).map(|(i, _)| i)
    {
        body.truncate(cut);
        truncated = true;
    }
    Ok(Response {
        url: url.to_string(),
        status,
        content_type,
        body,
        truncated,
    })
}

/// The URL one call reaches: the descriptor's base URL and the caller's path,
/// joined textually.
///
/// Textually is the point. The path is frequently a URL of its own — the
/// reader's contract is `https://r.jina.ai/<a page>`, so a tool that reads
/// pages passes its caller's URL through — and a join that *parsed* the path
/// (`Url::join`) would read `//host/…` or a whole URL as a new authority and
/// carry the credential off the endpoint the author named. Prepending the base
/// makes that impossible; [`same_authority`] is the guard that says so and
/// fails loudly if a future edit ever reaches for a join that can move it.
fn url_for(def: &Endpoint, path: &str) -> anyhow::Result<Url> {
    let base = Url::parse(&def.base_url).with_context(|| {
        format!(
            "`{}` is not a URL: {:?}",
            def.name, def.base_url
        )
    })?;
    check_scheme(&base)?;
    let joined = format!(
        "{}/{}",
        def.base_url.trim_end_matches('/'),
        path.trim().trim_start_matches('/')
    );
    let url = Url::parse(&joined).with_context(|| format!("not a URL: {joined:?}"))?;
    if !same_authority(&base, &url) {
        bail!(
            "`{path}` would leave {} for {url}; a credential reaches only the endpoint its descriptor names",
            def.base_url
        );
    }
    Ok(url)
}

/// Two URLs sharing a scheme, host and port. The guard [`url_for`] describes:
/// unreachable while the join is a prepend, and the reason that must stay true.
fn same_authority(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn def(base_url: &str) -> Endpoint {
        Endpoint {
            name: "svc".into(),
            base_url: base_url.into(),
            token_secret: Some("svc".into()),
            token_file: None,
            token_env: None,
            auth: Some(Auth {
                header: "Authorization".into(),
                prefix: "Bearer ".into(),
            }),
            headers: BTreeMap::new(),
            timeout_s: None,
            max: None,
        }
    }

    /// One HTTP/1.1 request read and one response written, on loopback — a
    /// build sandbox has no other network. The request line and headers come
    /// back from the task so a test can read what actually went out.
    async fn serve_once(body: &'static str) -> (SocketAddr, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
                return request;
            }
            String::new()
        });
        (addr, handle)
    }

    #[test]
    fn a_path_lands_under_the_base_url() {
        let url = url_for(&def("https://r.jina.ai"), "/https://example.com").unwrap();
        assert_eq!(url.as_str(), "https://r.jina.ai/https://example.com");
        let url = url_for(&def("https://api.example.com/v1/"), "things").unwrap();
        assert_eq!(url.as_str(), "https://api.example.com/v1/things");
    }

    /// A URL-shaped path is the reader's own contract and the normal case for a
    /// tool that reads pages: it stays on the descriptor's host because the
    /// join is a prepend, whatever the path is shaped like.
    #[test]
    fn a_url_shaped_path_stays_on_the_descriptors_host() {
        for path in [
            "https://example.com/page",
            "http://example.com",
            "//example.com/page",
            "example.com/page",
        ] {
            let url = url_for(&def("https://r.jina.ai"), path).unwrap();
            assert_eq!(url.host_str(), Some("r.jina.ai"), "{path} → {url}");
            assert!(url.as_str().starts_with("https://r.jina.ai/"), "{path} → {url}");
        }
    }

    #[tokio::test]
    async fn the_credential_rides_the_header_the_service_names() {
        let (addr, server) = serve_once("hello").await;
        let out = request(
            &def(&format!("http://{addr}")),
            Some("sekrit"),
            &Call { method: "GET", path: "/docs", body: None, policy: FetchPolicy::AnyAddress },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.status, 200);
        assert_eq!(out.body, "hello");
        assert!(out.ok());
        let request = server.await.unwrap();
        assert!(request.starts_with("GET /docs "), "{request}");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer sekrit"),
            "{request}"
        );
    }

    /// The descriptor's own headers go out on every request, which is the only
    /// way a call can ask for something the base URL does not carry
    /// (`x-no-cache`, a version, a media type) without the credential coming
    /// within reach of an argument.
    #[tokio::test]
    async fn the_descriptors_headers_ride_every_request() {
        let (addr, server) = serve_once("hello").await;
        let mut def = def(&format!("http://{addr}"));
        def.headers
            .insert("x-no-cache".to_string(), "true".to_string());
        let out = request(
            &def,
            None,
            &Call { method: "GET", path: "/x", body: None, policy: FetchPolicy::AnyAddress },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(out.ok());
        let request = server.await.unwrap().to_ascii_lowercase();
        assert!(request.contains("x-no-cache: true"), "{request}");
    }

    /// Jina's reader answers without a key, and a service declared with no
    /// `auth` must not grow one: an unauthenticated request that carried a
    /// stale stored key would be a different bill.
    #[tokio::test]
    async fn a_service_with_no_auth_sends_no_credential() {
        let (addr, server) = serve_once("hello").await;
        let mut anonymous = def(&format!("http://{addr}"));
        anonymous.auth = None;
        let out = request(
            &anonymous,
            Some("a-key-nobody-asked-for"),
            &Call { method: "GET", path: "/x", body: None, policy: FetchPolicy::AnyAddress },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(out.ok());
        let request = server.await.unwrap();
        assert!(
            !request.to_ascii_lowercase().contains("authorization"),
            "{request}"
        );
    }

    /// A session assembled for nobody to read over gets the address rule on
    /// every method, not just on `fetch`'s GET: a wrapper is not a way around
    /// the posture the session was built with. The listener is what makes
    /// "without dialling" an assertion rather than a reading of the error.
    #[tokio::test]
    async fn a_public_only_request_refuses_a_private_address() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let e = request(
            &def(&format!("http://{addr}")),
            Some("sekrit"),
            &Call { method: "POST", path: "/x", body: Some("{}"), policy: FetchPolicy::PublicOnly },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let e = format!("{e:#}");
        assert!(e.contains("public internet"), "{e}");
        assert!(
            tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err(),
            "the refused request dialled anyway"
        );
    }

    /// The descriptor's timeout is the one used: a socket that accepts and then
    /// never answers has to end the call in about a second, not in the thirty
    /// the default would take.
    #[tokio::test]
    async fn the_descriptors_timeout_is_the_one_used() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let held = tokio::spawn(async move {
            let _sock = listener.accept().await.unwrap();
            std::future::pending::<()>().await
        });
        let mut def = def(&format!("http://{addr}"));
        def.timeout_s = Some(1);

        let started = std::time::Instant::now();
        let e = request(
            &def,
            None,
            &Call { method: "GET", path: "/x", body: None, policy: FetchPolicy::AnyAddress },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the default timeout was used instead of the descriptor's: {:?}",
            started.elapsed()
        );
        assert!(format!("{e:#}").contains("timed out"), "{e:#}");
        held.abort();
    }

    /// The descriptor's cap is on what the script gets, and a body it cut says
    /// so rather than arriving looking whole.
    #[tokio::test]
    async fn the_descriptors_max_caps_what_the_script_gets() {
        let (addr, _server) = serve_once("0123456789abcdefghij").await;
        let mut def = def(&format!("http://{addr}"));
        def.max = Some(10);
        let out = request(
            &def,
            None,
            &Call { method: "GET", path: "/x", body: None, policy: FetchPolicy::AnyAddress },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(out.body, "0123456789");
        assert!(out.truncated, "a cut body must not look whole");
        assert!(out.render().contains("truncated"), "{}", out.render());
    }

    /// A method outside the list is refused before anything is dialled: the
    /// port here has no listener, so reaching the network would report a
    /// refused connection instead of the method.
    #[tokio::test]
    async fn an_unknown_method_never_opens_a_connection() {
        let e = request(
            &def("http://127.0.0.1:1"),
            None,
            &Call { method: "BREW", path: "/", body: None, policy: FetchPolicy::AnyAddress },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(format!("{e:#}").contains("not a method"), "{e:#}");
    }

    #[test]
    fn the_render_notes_a_status_that_is_not_the_ordinary_one() {
        let page = Response {
            url: "https://r.jina.ai/x".into(),
            status: 404,
            content_type: "text/plain".into(),
            body: "Not Found\n".into(),
            truncated: false,
        };
        assert_eq!(page.render(), "[HTTP 404]\nNot Found");
    }
}
