//! The web primitive: one GET over HTTP(S), with the body brought back as
//! *text a model can read* — and one call to a search API, which brings back
//! hits rather than a page.
//!
//! The fetch itself is unremarkable — a capped, cancellable, redirected
//! request. What the tool is for is the second half: a documentation page
//! is 300 KB of markup wrapping 8 KB of prose, and handing that to a model
//! spends the context window on `<div class="sidebar-nav-item">`. So HTML
//! is converted here ([`to_text`]) and everything else comes back as it
//! arrived.
//!
//! ## What it will connect to
//!
//! The address policy is the caller's ([`FetchPolicy`]), because the caller is
//! the one who knows whether anybody reads the URL before the model does.
//! [`FetchPolicy::AnyAddress`] is what this tool has always done: a GET of
//! whatever the URL names, redirects followed by the client. Under
//! [`FetchPolicy::PublicOnly`] every hop's target is checked — a lookup with
//! any non-public address in it is refused whole — and the connection is
//! pinned to the addresses that passed, so a name that answers privately the
//! second time cannot take the request somewhere the first answer did not.
//! [`search`] takes the same policy for its endpoint; it is the one caller
//! that carries a credential, so it takes no redirect at all — a hop would
//! hand the caller's key to a host the caller never named.
//!
//! ## Why the converter is written out rather than pulled in
//!
//! It is a few hundred lines against a dependency (and, in the crates that
//! do this properly, an HTML5 tree builder), and the requirement is
//! narrower than a general converter's: block boundaries become newlines,
//! `script`/`style` disappear, links keep their targets so the model can
//! follow one, and `pre` keeps its whitespace. A tag-scanner does all of
//! that on malformed markup without a parse tree, and it has no version to
//! track. `raw` is there for the case this reading is wrong — it returns
//! the bytes as they came.
//!
//! ## What is not converted
//!
//! Anything whose content type is not textual comes back as a *line about*
//! itself rather than as bytes. A model that asked for a PDF and got 200 KB
//! of mojibake learns nothing; a model told the type and the size can reach
//! for `bash` instead.

use std::net::{IpAddr, SocketAddr};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use reqwest::Url;
use tokio_util::sync::CancellationToken;

/// Characters of text returned when the caller names no cap. A converted
/// page is usually well under this; the ones that are not are link farms,
/// and the model can raise it for the page that is really that long.
pub const DEFAULT_MAX: usize = 40_000;
/// Bytes read off the wire before the body is abandoned, whatever the cap
/// on the text is. Nothing here needs eight megabytes; a stream that keeps
/// going is a stream to stop reading.
pub const MAX_DOWNLOAD: usize = 8 * 1024 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Redirects a guarded walk follows before it gives up — the same number the
/// permissive client's own policy allows, so a URL that redirects forever
/// ends the same way under either policy.
const MAX_REDIRECTS: usize = 10;

/// Brave's web-search endpoint: what [`search`] queries when the caller names
/// no other. A caller *may* name another, because a hermetic test has no
/// network and a search has to be shown answering something; the harness that
/// assembles a session passes `None`.
pub const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
/// Hits returned when the caller names no count.
pub const DEFAULT_HITS: usize = 5;
/// The most a caller may ask for — Brave's own ceiling, so a model that asks
/// for a hundred results gets twenty rather than an error.
const MAX_HITS: usize = 20;
/// Characters of a failed search response worth quoting back. An endpoint's
/// error body is a sentence; a page of it would spend the context on nothing.
const MAX_ERROR_BODY: usize = 300;
/// Brave's own name for the header an API key rides in.
const SUBSCRIPTION_HEADER: &str = "x-subscription-token";

/// What `fetch` is willing to connect to.
///
/// The mechanism is here; the decision belongs to whoever assembles the
/// session. A session an operator drives takes [`FetchPolicy::AnyAddress`],
/// which is what this tool has always done — asking a chat to read
/// `http://localhost:8080` is ordinary. A session nobody reads over, like an
/// unattended coding run whose tool list is not thinned, is assembled with
/// [`FetchPolicy::PublicOnly`], so a fetch cannot be turned into a request at
/// a cloud metadata endpoint or at anything else on the private network the
/// session is running in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FetchPolicy {
    /// Dial whatever the resolver answers, once the scheme is http(s). A
    /// redirect is followed by the client: one connection, and a hop nothing
    /// gets to inspect.
    #[default]
    AnyAddress,
    /// Refuse the whole lookup if *any* address the host resolves to is not on
    /// the public internet, and pin the connection to the addresses that
    /// passed. Every hop is checked, a redirect's target included.
    PublicOnly,
}

const USER_AGENT: &str = concat!("eidolon/", env!("CARGO_PKG_VERSION"));
const ACCEPT: &str =
    "text/html,application/xhtml+xml,application/json;q=0.9,text/plain;q=0.8,*/*;q=0.5";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// What was asked for.
    pub requested: String,
    /// Where the request ended up, which is not always the same thing.
    pub url: String,
    pub status: u16,
    pub content_type: String,
    pub text: String,
    pub truncated: bool,
    /// The body was HTML and this is the reading of it, not the bytes.
    pub converted: bool,
}

impl Page {
    /// Render for the model: what is worth saying about the response, then
    /// the response. The shape is [`crate::shell::Exec::render`]'s — a
    /// status that is not the ordinary one is a bracketed note above the
    /// content rather than an error, because a 404 body is frequently the
    /// answer to why there was a 404.
    pub fn render(&self) -> String {
        let mut s = String::new();
        if !(200..300).contains(&self.status) {
            s.push_str(&format!("[HTTP {}]\n", self.status));
        }
        if self.url != self.requested {
            s.push_str(&format!("[redirected to {}]\n", self.url));
        }
        s.push_str(self.text.trim_end());
        if self.truncated {
            s.push_str(
                "\n[content truncated; ask for a larger `max`, or fetch a more specific URL]",
            );
        }
        if s.is_empty() {
            s.push_str("[no content]");
        }
        s
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The client, and so the connection pool, for the life of the process, for
/// [`FetchPolicy::AnyAddress`]. Per-request timeouts go on the request rather
/// than here. It follows redirects itself, which is the trade that policy
/// makes: no hop gets looked at.
fn client() -> anyhow::Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| anyhow!("building the http client: {e}"))
}

/// The one scheme rule, for the URL the caller gave and for every hop of a
/// guarded walk. Not a refusal on safety grounds — this tool is a GET and
/// nothing else, and `file:`/`data:` reach things `read` and the model's own
/// hands already reach, by a path with no policy on it.
pub(crate) fn check_scheme(url: &Url) -> anyhow::Result<()> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        other => Err(anyhow!(
            "`{other}:` is not a scheme this tool fetches; use `read` for a file"
        )),
    }
}

/// The host of `url` as something resolvable and pinnable: the domain as the
/// URL holds it, or an IP literal without the brackets a URL wraps IPv6
/// addresses in — `getaddrinfo` (and so `lookup_host`) reads `[::1]` as a
/// name, and a name there is a lookup that fails.
fn host_of(url: &Url) -> anyhow::Result<String> {
    let host = url.host_str().context("this URL names no host")?;
    Ok(host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .to_string())
}

/// The host `url` names, and the socket addresses [`FetchPolicy::PublicOnly`]
/// is willing to dial for it. The host has to resolve, every address it
/// resolves to has to pass [`is_disallowed`], and there has to be one — so a
/// URL that cannot be looked up fails here, before anything is contacted.
pub(crate) async fn approve_target(url: &Url) -> anyhow::Result<(String, Vec<SocketAddr>)> {
    check_scheme(url)?;
    let host = host_of(url)?;
    let port = url.port_or_known_default().unwrap_or(80);
    let ips: Vec<IpAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .with_context(|| format!("resolving `{host}`"))?
        .map(|a| a.ip())
        .collect();
    approve_addrs(&host, &ips)?;
    Ok((
        host,
        ips.into_iter().map(|ip| SocketAddr::new(ip, port)).collect(),
    ))
}

/// Approve a resolved address list for `host`: every address must pass
/// [`is_disallowed`], and there must be at least one. Pulled out of the walk
/// as a pure function so the rule is testable without a lookup — including the
/// one that matters most, that a *mixed* answer is refused whole rather than
/// filtered down to the public addresses in it.
fn approve_addrs(host: &str, ips: &[IpAddr]) -> anyhow::Result<()> {
    if ips.is_empty() {
        bail!("`{host}` resolved to no addresses");
    }
    for ip in ips {
        if is_disallowed(*ip) {
            bail!("refusing to fetch `{host}`: {ip} is not on the public internet");
        }
    }
    Ok(())
}

/// Whether `ip` is outside the public internet. Covers the cloud metadata
/// address (169.254.169.254 is link-local), the shared address space
/// (100.64.0.0/10), and an IPv4-mapped IPv6 address, which is judged as the
/// v4 address it is rather than as v6.
fn is_disallowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 0x40)
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_disallowed(IpAddr::V4(mapped));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 — unique local.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // fe80::/10 — link-local.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// The client that dials `host` at exactly `addrs` — the addresses
/// [`approve_target`] just resolved and approved — and follows no redirect of
/// its own, so the walk looks at each hop's target. The resolve override is
/// what closes the gap between *the addresses that passed the check* and *the
/// address connected to*: without it reqwest resolves the name a second time
/// when it connects, and a name can answer differently the second time.
///
/// It is a *name* mechanism, and that is the whole of what it does: a URL whose
/// host is an IP literal is never resolved, so it is dialed at exactly the
/// address it names — which is safe only because [`approve_target`] resolved
/// that same literal and [`is_disallowed`] judged it. The pin closes the gap
/// for a name; the check is what closes it for a literal.
pub(crate) fn pinned_builder(host: &str, addrs: &[SocketAddr]) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, addrs)
}

/// Where a response points, if it is a redirect naming a target this tool can
/// read. `None` is where the walk stops holding that response: a status that is
/// not one of the five the client's own follow takes, a redirect with no
/// `Location`, or a `Location` that is not a URL is not a hop to take. The
/// statuses are named rather than "any 3xx" so a `304`, which carries a
/// `Location` sometimes and is a cached answer rather than a move, is handed
/// back like any other response.
fn redirect_target(
    current: &Url,
    status: reqwest::StatusCode,
    location: Option<&str>,
) -> Option<Url> {
    match status {
        reqwest::StatusCode::MOVED_PERMANENTLY
        | reqwest::StatusCode::FOUND
        | reqwest::StatusCode::SEE_OTHER
        | reqwest::StatusCode::TEMPORARY_REDIRECT
        | reqwest::StatusCode::PERMANENT_REDIRECT => {}
        _ => return None,
    }
    current.join(location?).ok()
}

/// Fetch under [`FetchPolicy::PublicOnly`]: check a target, dial it pinned to
/// the addresses checked, and do the same for a redirect — followed here
/// rather than by the client, because a client-side follow is a hop nothing
/// gets to look at. `timeout` bounds the whole walk, the way it bounded a
/// followed redirect before, rather than starting over at each hop.
async fn walk(
    start: Url,
    timeout: Duration,
    cancel: &CancellationToken,
) -> anyhow::Result<reqwest::Response> {
    let deadline = Instant::now() + timeout;
    let mut current = start.clone();
    let mut hops = 0usize;
    loop {
        // The lookup is raced with the interrupt too: a resolver that is slow
        // to answer is a call sitting there doing nothing, which is exactly
        // what an operator reaching for the interrupt is trying to stop.
        let (host, addrs) = tokio::select! {
            r = approve_target(&current) => r?,
            _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("timed out after {timeout:?} fetching {start}");
        }
        let client = pinned_builder(&host, &addrs)
            .build()
            .context("building the http client")?;
        let send = client
            .get(current.clone())
            .header(reqwest::header::ACCEPT, ACCEPT)
            .timeout(left)
            .send();
        let resp = tokio::select! {
            r = send => r.with_context(|| format!("GET {current}"))?,
            _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
        };
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let Some(next) = redirect_target(&current, resp.status(), location.as_deref()) else {
            return Ok(resp);
        };
        hops += 1;
        if hops > MAX_REDIRECTS {
            bail!("too many redirects (>{MAX_REDIRECTS}) fetching {start}");
        }
        current = next;
    }
}

/// GET `url`, and bring the body back as text.
///
/// `policy` is what this call may connect to and is the caller's decision, not
/// this function's — see [`FetchPolicy`]. `max` caps the *text*, `raw` skips
/// the HTML conversion, and `cancel` abandons the request wherever it has got
/// to — a fetch is one of the few tool calls that can sit for its whole
/// timeout doing nothing, which is exactly when an operator reaches for the
/// interrupt.
pub async fn fetch(
    url: &str,
    policy: FetchPolicy,
    max: Option<usize>,
    timeout: Option<Duration>,
    raw: bool,
    cancel: &CancellationToken,
) -> anyhow::Result<Page> {
    let parsed = Url::parse(url).with_context(|| format!("not a URL: {url:?}"))?;
    check_scheme(&parsed)?;
    let max = max.unwrap_or(DEFAULT_MAX).max(1);
    let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);

    // A call that was already cancelled when it arrived opens no
    // connection at all. Leaving this to the race below was not the same
    // thing: `select!` polls its ready branches in a random order, so an
    // abandoned fetch could still dial, and on a host with no route out
    // it came back with the *network's* complaint in place of the
    // operator's answer.
    if cancel.is_cancelled() {
        return Err(anyhow!("cancelled"));
    }

    let resp = match policy {
        FetchPolicy::AnyAddress => {
            let send = client()?
                .get(parsed.clone())
                .header(reqwest::header::ACCEPT, ACCEPT)
                .timeout(timeout)
                .send();
            tokio::select! {
                r = send => r.with_context(|| format!("GET {url}"))?,
                _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
            }
        }
        FetchPolicy::PublicOnly => walk(parsed, timeout, cancel).await?,
    };

    let status = resp.status().as_u16();
    let final_url = resp.url().to_string();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let (body, over) = read_capped(resp, cancel).await?;
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    let mut page = Page {
        requested: url.to_string(),
        url: final_url,
        status,
        content_type: content_type.clone(),
        text: String::new(),
        truncated: false,
        converted: false,
    };

    // A server that named no type gets the benefit of the doubt above; this
    // is where the doubt is settled. Nothing is learned from a megabyte of
    // replacement characters, and a model told the type and the size can
    // reach for `bash` instead.
    // `error_len() == None` is a *truncated* final character rather than a
    // bad one, which is what the download cap leaves behind — that is a body
    // this cut short, not a body that was never text.
    let typeless_binary = mime.is_empty()
        && std::str::from_utf8(&body)
            .err()
            .is_some_and(|e| e.error_len().is_some());
    if !is_textual(&mime) || typeless_binary {
        let mime = if mime.is_empty() {
            "unknown type"
        } else {
            &mime
        };
        page.text = format!("[not text: {}, {}]", mime, size(body.len(), over));
        return Ok(page);
    }

    let text = String::from_utf8_lossy(&body).into_owned();
    let text = if !raw && is_html(&mime, &text) {
        page.converted = true;
        let base = Url::parse(&page.url).ok();
        to_text(&text, base.as_ref())
    } else {
        text
    };

    let (text, cut) = truncate(text, max);
    page.text = text;
    page.truncated = cut || over;
    Ok(page)
}

/// Drain the body, stopping at [`MAX_DOWNLOAD`]. The second value says the
/// cap was reached, which is a truncation the caller has to report even
/// when the text cap was not hit.
pub(crate) async fn read_capped(
    mut resp: reqwest::Response,
    cancel: &CancellationToken,
) -> anyhow::Result<(Vec<u8>, bool)> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = tokio::select! {
            c = resp.chunk() => c.context("reading the response body")?,
            _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
        };
        match chunk {
            Some(c) => {
                if buf.len() + c.len() >= MAX_DOWNLOAD {
                    buf.extend_from_slice(&c[..MAX_DOWNLOAD.saturating_sub(buf.len())]);
                    return Ok((buf, true));
                }
                buf.extend_from_slice(&c);
            }
            None => return Ok((buf, false)),
        }
    }
}

fn size(bytes: usize, over: bool) -> String {
    let n = if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    };
    if over { format!("over {n}") } else { n }
}

/// Whether a body of this type is worth handing to a model as characters.
/// Generous on purpose: the `+json`/`+xml` suffixes and the whole `text/*`
/// tree are text, and so are the handful of `application/*` types that are
/// text in everything but their label.
fn is_textual(mime: &str) -> bool {
    if mime.is_empty() {
        // No `Content-Type` at all. Servers that omit it are usually
        // serving text; the utf-8 check below is the real guard.
        return true;
    }
    mime.starts_with("text/")
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/xhtml+xml"
                | "application/javascript"
                | "application/x-javascript"
                | "application/ecmascript"
                | "application/yaml"
                | "application/x-yaml"
                | "application/toml"
                | "application/sql"
                | "application/x-ndjson"
                | "application/graphql"
        )
}

fn is_html(mime: &str, body: &str) -> bool {
    if mime == "text/html" || mime == "application/xhtml+xml" {
        return true;
    }
    // A server that sent no type at all and then sent a document is common
    // enough to sniff for, and cheap: the first non-space of a page is `<`.
    // A declared `text/plain` is taken at its word — someone chose it.
    if !mime.is_empty() {
        return false;
    }
    let head = body.trim_start();
    head.starts_with("<!") || head.starts_with("<html") || head.starts_with("<HTML")
}

/// Cut to `max` characters on a character boundary.
fn truncate(mut text: String, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text, false);
    }
    let end = text
        .char_indices()
        .nth(max)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text.truncate(end);
    (text, true)
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// One result of [`search`]. `snippet` is the engine's own `description` of
/// the page — somebody else's summary, not text this tool read — so a model
/// that needs the page reaches for `fetch` with [`Hit::url`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Hits as text, one numbered result per group, in the shape
/// [`crate::search::render`] gives a grep's hits. An empty list is a line
/// too: nothing matched is an answer, and the model may want to say so.
pub fn render(hits: &[Hit]) -> String {
    if hits.is_empty() {
        return "[no results]".to_string();
    }
    hits.iter()
        .enumerate()
        .map(|(i, h)| format!("{}. {}\n   {}\n   {}", i + 1, h.title, h.url, h.snippet))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Brave's answer, in the part of it [`search`] reads. Every field is
/// optional: a search engine's response is not a schema this crate controls,
/// and a missing `web` block is an empty result set rather than a parse error.
/// Unknown fields (`query`, `mixed`, `type`) are ignored, which is what serde
/// does with them.
#[derive(serde::Deserialize)]
struct BraveAnswer {
    #[serde(default)]
    web: Option<BraveWeb>,
}

#[derive(serde::Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveHit>,
}

#[derive(serde::Deserialize)]
struct BraveHit {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    description: String,
}

/// A field as one line. A snippet is harvested from a page and can carry the
/// newlines that page had, which would break the shape of a rendered hit.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The client for a request that carries a credential — [`search`]'s. It is
/// [`client`]'s builder with one difference, and the difference is the whole
/// reason it exists: it follows no redirect, because what a hop would carry
/// across is somebody's key.
pub(crate) fn nofollow_client() -> anyhow::Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| anyhow!("building the http client: {e}"))
}

/// Send one request, abandoned wherever it got to if the call is cancelled
/// first — the same race every other call here runs, kept in one place because
/// a search's send has two shapes (see [`search`]).
pub(crate) async fn send_raced(
    send: impl std::future::Future<Output = reqwest::Result<reqwest::Response>>,
    what: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<reqwest::Response> {
    tokio::select! {
        r = send => r.with_context(|| what.to_string()),
        _ = cancel.cancelled() => Err(anyhow!("cancelled")),
    }
}

/// Search the web through `endpoint` (default [`BRAVE_ENDPOINT`]) and bring
/// back its hits.
///
/// The key is a *parameter*: this function is the mechanism, and whoever
/// assembles the session decides where a credential comes from. It never
/// leaves this function — Rust puts it in the header the endpoint wants, and
/// nothing returns it, journals it or logs it.
///
/// The same [`FetchPolicy`] as [`fetch`] governs the endpoint's address, so a
/// session that will not fetch a private address will not search one either.
/// Under either policy this call follows **no redirect**: the request carries
/// a key, and a `Location` would hand it to a host the caller never named.
pub async fn search(
    query: &str,
    count: Option<usize>,
    endpoint: Option<&str>,
    key: &str,
    policy: FetchPolicy,
    cancel: &CancellationToken,
) -> anyhow::Result<Vec<Hit>> {
    let query = query.trim();
    if query.is_empty() {
        bail!("search needs a query");
    }
    let key = key.trim();
    if key.is_empty() {
        bail!("search needs an API key");
    }
    let endpoint = endpoint.unwrap_or(BRAVE_ENDPOINT);
    let count = count.unwrap_or(DEFAULT_HITS).clamp(1, MAX_HITS);
    let count = count.to_string();
    let url = Url::parse_with_params(endpoint, [("q", query), ("count", count.as_str())])
        .with_context(|| format!("not a URL: {endpoint:?}"))?;
    check_scheme(&url)?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::HeaderName::from_static(SUBSCRIPTION_HEADER),
        // A key that cannot be a header value — one with a newline in it — is
        // refused here rather than smuggled to the endpoint.
        reqwest::header::HeaderValue::from_str(key)
            .context("this API key is not a usable header value")?,
    );

    let resp = match policy {
        FetchPolicy::AnyAddress => {
            let send = nofollow_client()?
                .get(url.clone())
                .headers(headers)
                .timeout(DEFAULT_TIMEOUT)
                .send();
            send_raced(send, &format!("querying {endpoint}"), cancel).await?
        }
        FetchPolicy::PublicOnly => {
            // The endpoint's address is checked and the connection pinned to
            // it, exactly as a guarded fetch does it — and the lookup is raced
            // with the interrupt, because a slow resolver is the operator
            // waiting for nothing.
            let (host, addrs) = tokio::select! {
                r = approve_target(&url) => r?,
                _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
            };
            let client = pinned_builder(&host, &addrs)
                .build()
                .context("building the http client")?;
            let send = client
                .get(url.clone())
                .headers(headers)
                .timeout(DEFAULT_TIMEOUT)
                .send();
            send_raced(send, &format!("querying {endpoint}"), cancel).await?
        }
    };

    let status = resp.status();
    if status.is_redirection() {
        let to = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(|t| format!(" to {t}"))
            .unwrap_or_default();
        bail!("the search endpoint redirected ({status}{to}); a redirect is not followed here");
    }
    let (body, _over) = read_capped(resp, cancel).await?;
    if !status.is_success() {
        let shown: String = String::from_utf8_lossy(&body)
            .chars()
            .take(MAX_ERROR_BODY)
            .collect();
        bail!("the search endpoint failed ({status}): {shown}");
    }
    let answer: BraveAnswer =
        serde_json::from_slice(&body).context("the search endpoint's answer was not JSON")?;
    Ok(answer
        .web
        .map(|w| w.results)
        .unwrap_or_default()
        .into_iter()
        .map(|r| Hit {
            title: one_line(&r.title),
            url: r.url.trim().to_string(),
            snippet: one_line(&r.description),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// HTML → text
// ---------------------------------------------------------------------------

/// Read `html` as the text it is wrapping.
///
/// A scanner, not a parser: it walks the markup looking for tags and treats
/// everything else as content. That is what makes it safe on the real web,
/// where a closing tag is optional, an attribute is unquoted and the
/// document has three `<body>`s — a tree builder has to decide what such a
/// document *is*, and this only has to decide where the paragraphs end.
///
/// `base`, when given, is what relative links are resolved against, so a
/// link the model is offered is one it can hand straight back to this tool.
pub fn to_text(html: &str, base: Option<&Url>) -> String {
    let mut out = Sink::default();
    let b = html.as_bytes();
    let mut i = 0usize;
    // The tag whose content is being thrown away (`script`, `style`, …), if
    // any. Kept as a name rather than a depth because these do not nest and
    // an unclosed one should not eat the document — see the scan below.
    let mut skipping: Option<&'static str> = None;
    // The open `<a>`: where its text starts in the output, and where it goes.
    let mut anchor: Option<(usize, String)> = None;

    while i < b.len() {
        if b[i] != b'<' {
            let end = memchr(b, i + 1, b'<').unwrap_or(b.len());
            if skipping.is_none() {
                out.text(&decode(&html[i..end]));
            }
            i = end;
            continue;
        }
        if html[i..].starts_with("<!--") {
            i = html[i..].find("-->").map(|n| i + n + 3).unwrap_or(b.len());
            continue;
        }
        let Some(close) = tag_end(b, i) else {
            // A bare `<` in prose. Treat it as prose — unless this is the
            // `a < b` inside the `<script>` whose text is being thrown away.
            if skipping.is_none() {
                out.text("<");
            }
            i += 1;
            continue;
        };
        let raw = &html[i + 1..close];
        i = close + 1;
        let closing = raw.starts_with('/');
        let name = tag_name(raw);

        if let Some(open) = skipping {
            if closing && name == open {
                skipping = None;
            }
            continue;
        }
        if !closing && let Some(dropped) = DROPPED.iter().find(|d| **d == name) {
            // Self-closing (`<svg …/>`) drops nothing but itself.
            if !raw.trim_end().ends_with('/') {
                skipping = Some(dropped);
            }
            continue;
        }

        match name.as_str() {
            "br" if !closing => out.newlines(1),
            "hr" if !closing => {
                out.newlines(2);
                out.literal("---");
                out.newlines(2);
            }
            "li" if !closing => {
                out.newlines(1);
                out.literal("- ");
            }
            "pre" => {
                if closing {
                    out.leave_pre();
                } else {
                    out.enter_pre();
                }
            }
            "a" => {
                if closing {
                    if let Some((start, href)) = anchor.take() {
                        out.close_link(start, &href);
                    }
                } else if let Some(href) = attr(raw, "href").and_then(|h| resolve(&h, base)) {
                    anchor = Some((out.open_link(), href));
                }
            }
            "title" if !closing => {
                out.newlines(2);
                out.literal("# ");
            }
            _ => {
                if let Some(level) = heading(&name) {
                    out.newlines(2);
                    if !closing {
                        out.literal(&"#".repeat(level));
                        out.literal(" ");
                    }
                } else if BLOCKS.contains(&name.as_str()) {
                    out.newlines(2);
                }
            }
        }
    }
    if let Some((start, href)) = anchor.take() {
        out.close_link(start, &href);
    }
    out.finish()
}

/// Elements whose *content* is not prose. `head` is not here: a document's
/// `<title>` is the best one-line description of it there is, and every
/// other thing in the head is a tag this scanner already ignores.
const DROPPED: &[&str] = &[
    "script", "style", "noscript", "svg", "canvas", "template", "iframe", "select", "math",
];

/// Elements that end a paragraph. Inline elements are absent on purpose:
/// anything not named here contributes its text and no whitespace, which is
/// the right default for the long tail of `<span>`-alikes.
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "aside",
    "nav",
    "blockquote",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "form",
    "fieldset",
    "figure",
    "figcaption",
    "details",
    "summary",
    "address",
    "body",
    "html",
    "head",
    "dialog",
];

fn heading(name: &str) -> Option<usize> {
    let mut c = name.chars();
    match (c.next(), c.next(), c.next()) {
        (Some('h'), Some(d @ '1'..='6'), None) => Some(d as usize - '0' as usize),
        _ => None,
    }
}

fn memchr(b: &[u8], from: usize, needle: u8) -> Option<usize> {
    b[from..]
        .iter()
        .position(|c| *c == needle)
        .map(|n| from + n)
}

/// The index of the `>` closing the tag that starts at `i`, skipping over
/// quoted attribute values so a `>` inside one does not end it early.
fn tag_end(b: &[u8], i: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (n, c) in b.iter().enumerate().skip(i + 1) {
        match *c {
            q @ (b'"' | b'\'') if quote == 0 => quote = q,
            q if q == quote => quote = 0,
            b'>' if quote == 0 => return Some(n),
            b'<' if quote == 0 => return None, // an unclosed `<`; it was prose
            _ => {}
        }
    }
    None
}

/// The lowercased element name from a tag's innards (`/DIV class=x` → `div`).
fn tag_name(raw: &str) -> String {
    raw.trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// One attribute's value, quoted or not. Case-insensitive on the name,
/// because the web is.
fn attr(raw: &str, name: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let mut from = 0;
    while let Some(n) = lower[from..].find(name) {
        let at = from + n;
        from = at + name.len();
        // A name is a name only where an attribute may start, and only if
        // what follows it is `=` — otherwise this is `data-href` or `href`
        // inside some other value.
        let before_ok = at == 0 || raw.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = raw[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let v = rest[1..].trim_start();
        let value = match v.chars().next() {
            Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or(""),
            _ => v
                .split(|c: char| c.is_ascii_whitespace() || c == '>')
                .next()
                .unwrap_or(""),
        };
        return Some(decode(value));
    }
    None
}

/// A link target worth showing: absolute, or made absolute against `base`.
/// A fragment, a `javascript:` and an empty href are not links to anywhere
/// this tool can go, so they lose their brackets and keep their text.
fn resolve(href: &str, base: Option<&Url>) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with("mailto:")
    {
        return None;
    }
    match Url::parse(href) {
        Ok(u) => Some(u.to_string()),
        Err(_) => base?.join(href).ok().map(|u| u.to_string()),
    }
}

/// The entities that actually appear, plus the numeric forms. A full table
/// is two thousand names for the benefit of `&thetasym;`.
fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(n) = rest.find('&') {
        out.push_str(&rest[..n]);
        let tail = &rest[n..];
        // An entity is short; a stray `&` in prose must not swallow a line.
        // The window is measured in *characters*, because prose is not
        // ASCII and a byte cap would land inside one.
        let window = tail
            .char_indices()
            .nth(12)
            .map(|(n, _)| n)
            .unwrap_or(tail.len());
        let end = tail[..window].find(';');
        let Some(end) = end else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let body = &tail[1..end];
        let ch = match body {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "lsquo" | "rsquo" => Some('\''),
            "ldquo" | "rdquo" => Some('"'),
            "middot" => Some('·'),
            "bull" => Some('•'),
            "copy" => Some('©'),
            "reg" => Some('®'),
            "trade" => Some('™'),
            "deg" => Some('°'),
            "times" => Some('×'),
            "laquo" => Some('«'),
            "raquo" => Some('»'),
            other => other
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The output, and the whitespace it has been promised but not yet written.
///
/// Deferring is what keeps `<div><p><span> Hi </span></p></div>` from
/// becoming six blank lines: every block boundary *raises* the number of
/// newlines owed, and they are paid once, immediately before the next
/// character that is not whitespace. Nothing is owed at the start or the
/// end of the document, so a page cannot begin with a gap.
#[derive(Default)]
struct Sink {
    out: String,
    /// Newlines owed before the next text — capped at two on the way out,
    /// so any depth of nesting is one blank line.
    breaks: usize,
    /// A space is owed: some whitespace was seen since the last character.
    space: bool,
    /// Inside `<pre>`, where whitespace is content.
    pre: usize,
    /// Nothing has been written since `<pre>` opened. The newline right
    /// after the tag is markup, not content — the HTML spec says so and
    /// every browser agrees — and keeping it puts a blank line at the top
    /// of every code block on the web.
    pre_fresh: bool,
}

impl Sink {
    fn text(&mut self, s: &str) {
        if self.pre > 0 {
            self.pay();
            let s = if std::mem::take(&mut self.pre_fresh) {
                s.strip_prefix("\r\n")
                    .or_else(|| s.strip_prefix('\n'))
                    .unwrap_or(s)
            } else {
                s
            };
            self.out.push_str(s);
            return;
        }
        for ch in s.chars() {
            if ch.is_whitespace() {
                self.space = true;
            } else {
                self.pay();
                self.out.push(ch);
            }
        }
    }

    /// Write text of our own — a bullet, a heading's hashes — which owed
    /// whitespace precedes but does not follow.
    fn literal(&mut self, s: &str) {
        self.pay();
        self.out.push_str(s);
    }

    fn newlines(&mut self, n: usize) {
        self.breaks = self.breaks.max(n);
    }

    /// Settle what is owed. Newlines outrank a space, and neither is
    /// written at the very start.
    fn pay(&mut self) {
        if self.breaks > 0 {
            if !self.out.is_empty() {
                let have = self.out.chars().rev().take_while(|c| *c == '\n').count();
                for _ in have..self.breaks.min(2) {
                    self.out.push('\n');
                }
            }
            self.breaks = 0;
            self.space = false;
        }
        if self.space {
            if !self.out.is_empty() && !self.out.ends_with(['\n', ' ']) {
                self.out.push(' ');
            }
            self.space = false;
        }
    }

    fn enter_pre(&mut self) {
        self.newlines(2);
        self.literal("```");
        self.out.push('\n');
        self.pre += 1;
        self.pre_fresh = true;
    }

    fn leave_pre(&mut self) {
        self.pre = self.pre.saturating_sub(1);
        self.pre_fresh = false;
        if !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.out.push_str("```");
        self.newlines(2);
    }

    /// Open a link: the `[` goes down now, and where it went is returned so
    /// the close can take it back if the link turned out to have no text.
    fn open_link(&mut self) -> usize {
        self.pay();
        self.out.push('[');
        self.out.len()
    }

    fn close_link(&mut self, start: usize, href: &str) {
        // `[](url)` is noise, and a link wrapped around an image — which
        // this converter drops — is where most of it would come from. Take
        // the `[` back instead; `start` is the byte after it.
        if self.out[start..].trim().is_empty() {
            self.out.truncate(start - 1);
            return;
        }
        self.out.push_str("](");
        self.out.push_str(href);
        self.out.push(')');
    }

    fn finish(mut self) -> String {
        while self.out.ends_with(char::is_whitespace) {
            self.out.pop();
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.com/docs/guide").unwrap()
    }

    #[test]
    fn markup_becomes_the_text_it_wrapped() {
        let html = r#"<html><head><title>Guide</title>
            <style>body { color: red }</style>
            <script>var x = "</p>";</script></head>
            <body><h2>Setting up</h2><p>Run   the
            installer.</p><p>Then <b>restart</b>.</p></body></html>"#;
        let t = to_text(html, Some(&base()));
        assert_eq!(
            t,
            "# Guide\n\n## Setting up\n\nRun the installer.\n\nThen restart."
        );
    }

    /// The whole point of dropping `script`: its contents are not prose,
    /// and a string inside one routinely contains what looks like a tag.
    #[test]
    fn a_tag_inside_a_dropped_element_does_not_end_it() {
        let t = to_text(
            "<p>a</p><script>if (a<b) { s = '</p>' }</script><p>b</p>",
            None,
        );
        assert_eq!(t, "a\n\nb");
    }

    #[test]
    fn links_are_kept_and_made_absolute() {
        let html = r##"<a href="../api.html">the API</a> and <a href="https://rust-lang.org">Rust</a>
                      and <a href="#top">top</a> and <a href="/x"><img src="i.png"></a>"##;
        let t = to_text(html, Some(&base()));
        assert_eq!(
            t,
            "[the API](https://example.com/api.html) and [Rust](https://rust-lang.org/) and top and"
        );
    }

    /// A relative link with no base is a link to nowhere this tool can go,
    /// so it keeps its text and loses its brackets rather than offering the
    /// model a target it cannot use.
    #[test]
    fn a_relative_link_without_a_base_keeps_only_its_text() {
        assert_eq!(
            to_text(r#"see <a href="/api">the API</a>"#, None),
            "see the API"
        );
    }

    #[test]
    fn a_list_is_a_list_and_pre_keeps_its_shape() {
        let t = to_text(
            "<ul><li>one</li><li>two</li></ul><pre>fn main() {\n    ok\n}</pre>",
            None,
        );
        assert_eq!(t, "- one\n- two\n\n```\nfn main() {\n    ok\n}\n```");
    }

    /// Nesting is where a naive converter produces a page of blank lines:
    /// every closing tag owes a paragraph break and they all get paid.
    #[test]
    fn nesting_does_not_multiply_blank_lines() {
        let t = to_text(
            "<div><div><div><p>a</p></div></div></div><div><p>b</p></div>",
            None,
        );
        assert_eq!(t, "a\n\nb");
    }

    #[test]
    fn entities_and_attributes_survive_the_web() {
        assert_eq!(
            decode("a &amp; b &lt;c&gt; &#39;d&#39; &x; &hellip;"),
            "a & b <c> 'd' &x; …"
        );
        assert_eq!(
            attr(r#"a href='/x?a=1&amp;b=2' rel=next"#, "href").as_deref(),
            Some("/x?a=1&b=2")
        );
        assert_eq!(
            attr("a data-href=/no", "href"),
            None,
            "an attribute is not a suffix of another"
        );
        assert_eq!(
            attr("a href=/bare class=x", "href").as_deref(),
            Some("/bare")
        );
        // A `>` inside a quoted value does not end the tag.
        assert_eq!(
            to_text(r#"<a href="/x" title="a > b">t</a>"#, Some(&base())),
            "[t](https://example.com/x)"
        );
    }

    /// Prose containing a `<` is prose. A scanner that treated it as a tag
    /// would eat the rest of the sentence.
    #[test]
    fn a_bare_angle_bracket_is_text() {
        assert_eq!(to_text("<p>when a < b, stop</p>", None), "when a < b, stop");
    }

    #[test]
    fn types_are_sorted_into_text_and_not() {
        assert!(is_textual("text/html"));
        assert!(is_textual("application/json"));
        assert!(is_textual("application/vnd.api+json"));
        assert!(is_textual(""), "no content-type at all is usually text");
        assert!(!is_textual("application/pdf"));
        assert!(!is_textual("image/png"));
        assert!(is_html("text/html", ""));
        assert!(
            is_html("", "<!doctype html><html>"),
            "no type, but plainly a document"
        );
        assert!(
            !is_html("text/plain", "<!doctype html>"),
            "a declared type is taken at its word"
        );
    }

    /// The spec drops the newline immediately after `<pre>`, and so does
    /// every browser; keeping it put a blank line at the top of every code
    /// block on the web.
    #[test]
    fn a_code_block_does_not_open_with_a_blank_line() {
        assert_eq!(
            to_text("<pre><code>\nfn main() {}\n</code></pre>", None),
            "```\nfn main() {}\n```"
        );
    }

    /// A `&` in prose followed by a multi-byte character used to index into
    /// the middle of one while looking for the entity's `;`.
    #[test]
    fn a_stray_ampersand_before_a_multibyte_character_is_prose() {
        assert_eq!(decode("Ben & Jerry’s — really"), "Ben & Jerry’s — really");
        assert_eq!(decode("&é"), "&é");
    }

    #[test]
    fn truncation_lands_on_a_character_boundary() {
        let (t, cut) = truncate("héllo wörld".into(), 4);
        assert!(cut);
        assert_eq!(t, "héll");
        let (t, cut) = truncate("short".into(), 40);
        assert!(!cut);
        assert_eq!(t, "short");
    }

    /// A status that is not the ordinary one is a note above the body, not
    /// an error instead of it: the body of a 404 is often why it was a 404.
    #[test]
    fn a_bad_status_is_reported_above_the_body() {
        let p = Page {
            requested: "https://example.com/a".into(),
            url: "https://example.com/b".into(),
            status: 404,
            content_type: "text/html".into(),
            text: "No such page.".into(),
            truncated: false,
            converted: true,
        };
        assert_eq!(
            p.render(),
            "[HTTP 404]\n[redirected to https://example.com/b]\nNo such page."
        );
        assert!(!p.ok());
    }

    /// The scheme rule holds under either policy: the permitting one keeps
    /// today's answer, and the strict one is not what refuses a `file:` URL.
    #[tokio::test]
    async fn only_http_is_fetched() {
        let cancel = CancellationToken::new();
        for policy in [FetchPolicy::AnyAddress, FetchPolicy::PublicOnly] {
            let e = fetch("file:///etc/passwd", policy, None, None, false, &cancel)
                .await
                .unwrap_err();
            assert!(format!("{e:#}").contains("use `read`"), "{e:#}");
            let e = fetch("not a url", policy, None, None, false, &cancel)
                .await
                .unwrap_err();
            assert!(format!("{e:#}").contains("not a URL"), "{e:#}");
        }
    }

    /// The interrupt has to beat the timeout rather than wait for it, which
    /// needs a request that is genuinely sitting there. The server is a
    /// socket that accepts and then never answers — a hang built out of
    /// loopback, because loopback is the only network a build sandbox has.
    /// This used to point at 203.0.113.1, which routes nowhere *when there
    /// is a route*: with no network at all the connect failed at once and
    /// the assertion read back the connect error.
    ///
    /// It binds loopback on purpose, which is exactly the seam this test now
    /// demonstrates: `127.0.0.1` is a URL the strict policy refuses outright,
    /// so the policy that permits it is named rather than left implicit.
    #[tokio::test]
    async fn a_cancelled_fetch_does_not_wait_for_its_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let held = tokio::spawn(async move {
            let _sock = listener.accept().await.unwrap();
            std::future::pending::<()>().await
        });

        let c = CancellationToken::new();
        let cancel = c.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c.cancel();
        });

        let t = std::time::Instant::now();
        let e = fetch(
            &format!("http://{addr}/"),
            FetchPolicy::AnyAddress,
            None,
            Some(Duration::from_secs(30)),
            false,
            &cancel,
        )
        .await
        .unwrap_err();
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "took {:?}",
            t.elapsed()
        );
        assert!(format!("{e:#}").contains("cancelled"), "{e:#}");
        held.abort();
    }

    /// And one cancelled before it starts never dials: nothing listens on
    /// port 1, so reaching the network would report the refused connection
    /// instead of the cancellation. Under either policy — the strict one has
    /// its own path to the network, and the check has to come before it too.
    #[tokio::test]
    async fn a_fetch_cancelled_before_it_starts_does_not_dial() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        for policy in [FetchPolicy::AnyAddress, FetchPolicy::PublicOnly] {
            let e = fetch(
                "http://127.0.0.1:1/",
                policy,
                None,
                Some(Duration::from_secs(30)),
                false,
                &cancel,
            )
            .await
            .unwrap_err();
            assert!(format!("{e:#}").contains("cancelled"), "{e:#}");
        }
    }

    // -----------------------------------------------------------------------
    // The address policy
    // -----------------------------------------------------------------------

    /// One HTTP/1.1 request read and one response written, on loopback — a
    /// build sandbox has no other network, so a response a test can read has
    /// to be built here rather than fetched from anywhere.
    async fn serve_once(body: &'static str) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (addr, handle)
    }

    /// The ranges the strict policy refuses, and the ones it does not. The
    /// metadata address is what this is for: an unattended session that can
    /// fetch it can read the instance's credentials.
    #[test]
    fn the_strict_policy_refuses_everything_off_the_public_internet() {
        for ip in [
            "127.0.0.1",       // loopback
            "10.1.2.3",        // private
            "192.168.0.1",     // private
            "172.16.5.5",      // private
            "169.254.169.254", // link-local: the cloud metadata endpoint
            "100.64.0.1",      // CGNAT / shared address space
            "0.0.0.0",         // unspecified
            "255.255.255.255", // broadcast
            "192.0.2.1",       // documentation
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(is_disallowed(ip), "{ip} should be refused");
        }
        for ip in ["::1", "fc00::1", "fe80::1"] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(is_disallowed(ip), "{ip} should be refused");
        }
        assert!(
            is_disallowed("::ffff:10.0.0.1".parse().unwrap()),
            "a v4-mapped address is judged as the v4 address it is"
        );
        for ip in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "2606:4700:4700::1111"] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(!is_disallowed(ip), "{ip} should be allowed");
        }
    }

    /// A name answering with a public address beside a private one is refused
    /// whole, never filtered down to the public one: the mixed answer is what
    /// a rebinding setup looks like, and the request that followed would be
    /// the one that landed on the private address.
    #[test]
    fn a_lookup_with_one_disallowed_address_is_refused_whole() {
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        let private: IpAddr = "10.1.2.3".parse().unwrap();
        let e = approve_addrs("mixed.example", &[public, private]).unwrap_err();
        assert!(format!("{e:#}").contains("10.1.2.3"), "{e:#}");
        assert!(approve_addrs("public.example", &[public]).is_ok());
        assert!(approve_addrs("private.example", &[private]).is_err());
        assert!(
            approve_addrs("empty.example", &[]).is_err(),
            "a lookup that answered nothing is not an approval"
        );
    }

    /// The strict policy refuses a loopback URL without dialing it. The
    /// listener is what makes "without dialing" an assertion rather than a
    /// reading of the error: nothing was ever accepted on it.
    #[tokio::test]
    async fn the_strict_policy_refuses_loopback_before_it_dials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let t = Instant::now();
        let e = fetch(
            &format!("http://{addr}/"),
            FetchPolicy::PublicOnly,
            None,
            Some(Duration::from_secs(5)),
            false,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let e = format!("{e:#}");
        assert!(e.contains("127.0.0.1") && e.contains("public"), "{e}");
        assert!(
            t.elapsed() < Duration::from_secs(1),
            "a refusal is not a connect timeout: {:?}",
            t.elapsed()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "nothing should have reached the listener"
        );
    }

    /// The same for the addresses a private network is made of, named as
    /// literals so no lookup is involved and the whole test is offline. Every
    /// one of these has to fail before a connection, not by timing out on one.
    #[tokio::test]
    async fn the_strict_policy_refuses_private_and_link_local_addresses() {
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://10.1.2.3/",
            "http://172.16.5.5/",
            "http://192.168.0.1/",
            "http://100.64.0.1/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:10.0.0.1]/",
        ] {
            let t = Instant::now();
            let e = fetch(
                url,
                FetchPolicy::PublicOnly,
                None,
                Some(Duration::from_secs(2)),
                false,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
            let e = format!("{e:#}");
            assert!(e.contains("public internet"), "{url}: {e}");
            assert!(t.elapsed() < Duration::from_secs(1), "{url}: {:?}", t.elapsed());
        }
    }

    /// A redirect is a hop like the first one, which is why the strict policy
    /// walks hops itself instead of letting the client follow them: an
    /// unchecked `Location: http://169.254.169.254/` is how a metadata read is
    /// spelled. The walk drives every hop — the first URL and each `Location`
    /// — through `approve_target`, and this pins that second half: the target
    /// this computes is the target that call refuses.
    ///
    /// It is driven at that seam rather than through a live redirect because
    /// none can be hermetic under this policy: the hop that *serves* the
    /// redirect would have to be on loopback, and the strict policy refuses it
    /// before it answers.
    #[tokio::test]
    async fn a_redirect_target_is_checked_the_way_the_first_hop_is() {
        let here = Url::parse("https://example.com/docs").unwrap();
        let target = redirect_target(
            &here,
            reqwest::StatusCode::FOUND,
            Some("http://169.254.169.254/latest/meta-data/"),
        )
        .expect("a 302 with a Location is a hop to check");
        assert_eq!(target.as_str(), "http://169.254.169.254/latest/meta-data/");
        let e = approve_target(&target).await.unwrap_err();
        assert!(format!("{e:#}").contains("169.254.169.254"), "{e:#}");

        // A relative Location resolves against the hop that sent it, so a
        // same-host move is checked as the URL it really is.
        let same_host = redirect_target(&here, reqwest::StatusCode::FOUND, Some("/other")).unwrap();
        assert_eq!(same_host.as_str(), "https://example.com/other");
        // And a response that is not a hop — an ordinary page, a `304` that
        // carries a `Location` without being a move, or any redirect that
        // names no target — is where the walk stops holding what it has.
        assert!(
            redirect_target(&here, reqwest::StatusCode::OK, Some("http://10.0.0.1/")).is_none()
        );
        assert!(
            redirect_target(
                &here,
                reqwest::StatusCode::NOT_MODIFIED,
                Some("http://169.254.169.254/")
            )
            .is_none()
        );
        assert!(redirect_target(&here, reqwest::StatusCode::FOUND, None).is_none());
    }

    /// The addresses the policy approved are the addresses dialed: reqwest is
    /// told to resolve `host` to exactly them, so its own lookup — which could
    /// answer differently the second time — never runs. `pinned.test` resolves
    /// nowhere, so the loopback server answering is the proof that the
    /// override is what the connection used.
    #[tokio::test]
    async fn a_host_is_dialed_at_the_address_it_was_pinned_to() {
        let (addr, server) = serve_once("hi").await;
        let client = pinned_builder(
            "pinned.test",
            &[SocketAddr::new(addr.ip(), addr.port())],
        )
        .build()
        .unwrap();
        let resp = client
            .get(format!("http://pinned.test:{}/", addr.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "hi");
        server.await.unwrap();
    }

    /// The wiring itself, without a client: the builder carries the host and
    /// the exact address approved for it, and a different host pins
    /// differently rather than ignoring what it was given.
    #[test]
    fn the_pinned_builder_carries_the_approved_address() {
        let approved: IpAddr = "93.184.216.34".parse().unwrap();
        let debug = format!(
            "{:?}",
            pinned_builder("example.com", &[SocketAddr::new(approved, 443)])
        );
        assert!(debug.contains("93.184.216.34"), "{debug}");
        assert!(debug.contains("example.com"), "{debug}");
        let other: IpAddr = "1.1.1.1".parse().unwrap();
        let other_debug = format!(
            "{:?}",
            pinned_builder("other.example", &[SocketAddr::new(other, 80)])
        );
        assert!(other_debug.contains("1.1.1.1"), "{other_debug}");
        assert!(!other_debug.contains("93.184.216.34"), "{other_debug}");
    }

    /// The strict path end to end against the real internet: a lookup, an
    /// approval, a connection pinned to what was approved, and the page read
    /// back. The only test here that needs a network, so it is ignored by
    /// default — run it with `cargo test -p eidolon-tools -- --ignored`, which
    /// is also how a redirect through the walk (rather than through a client
    /// that follows them itself) can be watched on a real chain.
    #[ignore = "live: fetches a public page over the network"]
    #[tokio::test]
    async fn a_public_page_comes_back_through_the_guarded_walk() {
        let page = fetch(
            "https://example.com/",
            FetchPolicy::PublicOnly,
            None,
            Some(Duration::from_secs(30)),
            false,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(page.ok(), "{}", page.render());
        assert!(page.text.contains("Example Domain"), "{}", page.text);
        assert_eq!(page.url, "https://example.com/");
    }

    /// And the walk taking a real redirect as a second hop:
    /// `http://github.com/` answers 301 to its https URL, so the walk
    /// re-resolves and re-approves that target and dials it on its own pinned
    /// connection rather than letting a client follow it unlooked-at. Ignored
    /// for the same reason as the test above.
    #[ignore = "live: follows a public redirect over the network"]
    #[tokio::test]
    async fn the_guarded_walk_takes_a_redirect_as_its_own_hop() {
        let page = fetch(
            "http://github.com/",
            FetchPolicy::PublicOnly,
            None,
            Some(Duration::from_secs(30)),
            false,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(page.ok(), "{}", page.render());
        assert_eq!(
            page.url, "https://github.com/",
            "the walk ended on the redirect's target"
        );
    }

    /// The refusal that motivated the policy, live: a real public host
    /// redirects to the cloud metadata address, and the walk refuses the
    /// *target* rather than the hop that served it — proof the redirect is
    /// validated before it is dialed. Needs a public redirector, so it is
    /// ignored like the two tests above; the hermetic version of the same
    /// claim drives `approve_target` directly in
    /// `a_redirect_target_is_checked_the_way_the_first_hop_is`.
    #[ignore = "live: needs a public redirector to hand out a bad Location"]
    #[tokio::test]
    async fn a_live_redirect_to_the_metadata_address_is_refused() {
        let e = fetch(
            "https://httpbin.org/redirect-to?url=http://169.254.169.254/",
            FetchPolicy::PublicOnly,
            None,
            Some(Duration::from_secs(30)),
            false,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let e = format!("{e:#}");
        assert!(
            e.contains("169.254.169.254") && e.contains("public internet"),
            "{e}"
        );
    }

    /// The permitting policy is today's tool: a loopback URL is fetched, and a
    /// redirect is followed by the client in one request, which is the
    /// behaviour every session that is not assembled strict keeps.
    #[tokio::test]
    async fn the_permitting_policy_still_reaches_loopback_and_follows_a_redirect() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // A page, and a server that moves to it: the first request gets a 302
        // and the second, on a new connection, the page.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let resp = if req.starts_with("GET /two ") {
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 3\r\nconnection: close\r\n\r\ntwo".to_string()
                } else {
                    "HTTP/1.1 302 Found\r\nlocation: /two\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let page = fetch(
            &format!("http://{addr}/"),
            FetchPolicy::AnyAddress,
            None,
            Some(Duration::from_secs(5)),
            false,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(page.ok(), "{}", page.render());
        assert_eq!(page.text, "two");
        assert!(page.url.ends_with("/two"), "{}", page.url);
        server.abort();
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    /// One request read and one response written, on loopback, with the
    /// request text handed back: a search test has to be able to say what was
    /// actually sent — the query, the count, and the header the key rides in.
    async fn serve_recording(
        status: &'static str,
        body: String,
    ) -> (SocketAddr, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let Ok((mut sock, _)) = listener.accept().await else {
                return String::new();
            };
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let resp = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
            req
        });
        (addr, handle)
    }

    /// A Brave-shaped answer for the given `(title, url, description)` triples.
    fn brave_json(hits: &[(&str, &str, &str)]) -> String {
        serde_json::json!({
            "query": "ignored",
            "web": {
                "results": hits
                    .iter()
                    .map(|(t, u, s)| serde_json::json!({ "title": t, "url": u, "description": s }))
                    .collect::<Vec<_>>(),
            },
        })
        .to_string()
    }

    fn endpoint(addr: SocketAddr) -> String {
        format!("http://{addr}/search")
    }

    /// What a search sends, and what it does with the answer: the query and
    /// the count are on the wire, the key is in the header Rust builds, and a
    /// snippet harvested from a page comes back as one line rather than as the
    /// newlines the page had.
    #[tokio::test]
    async fn a_search_sends_the_query_the_count_and_the_key() {
        let body = brave_json(&[
            ("Rust async", "https://example.com/a", "An   intro\nto async"),
            ("Two", "https://example.com/b", "second"),
        ]);
        let (addr, server) = serve_recording("200 OK", body).await;
        let hits = search(
            "  rust async  ",
            Some(2),
            Some(&endpoint(addr)),
            "test-key",
            FetchPolicy::AnyAddress,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        let request = server.await.unwrap();
        assert!(request.starts_with("GET /search?q=rust+async&count=2 "), "{request}");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("x-subscription-token: test-key"),
            "{request}"
        );

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Rust async");
        assert_eq!(hits[0].url, "https://example.com/a");
        assert_eq!(hits[0].snippet, "An intro to async", "a snippet is one line");
        assert_eq!(
            render(&hits),
            "1. Rust async\n   https://example.com/a\n   An intro to async\n\n2. Two\n   https://example.com/b\n   second"
        );
    }

    /// Nothing matched is an answer with an empty list and a line of text, and
    /// an answer with no `web` block at all reads the same way — a search
    /// engine's response is not a schema this crate controls.
    #[tokio::test]
    async fn a_search_with_nothing_to_report_is_not_a_failure() {
        for body in [r#"{"web":{"results":[]}}"#.to_string(), r#"{"query":"x"}"#.to_string()] {
            let (addr, _server) = serve_recording("200 OK", body).await;
            let hits = search(
                "nothing",
                None,
                Some(&endpoint(addr)),
                "k",
                FetchPolicy::AnyAddress,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
            assert!(hits.is_empty(), "{hits:?}");
        }
        assert_eq!(render(&[]), "[no results]");
    }

    /// A key the endpoint rejects is reported with the endpoint's own words,
    /// which is what tells an operator the difference between a bad key and a
    /// quota that has run out.
    #[tokio::test]
    async fn a_failed_search_reports_the_status_and_what_the_endpoint_said() {
        let body = r#"{"error":{"status":"401","detail":"Invalid API key"}}"#.to_string();
        let (addr, _server) = serve_recording("401 Unauthorized", body).await;
        let e = search(
            "q",
            None,
            Some(&endpoint(addr)),
            "wrong-key",
            FetchPolicy::AnyAddress,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let e = format!("{e:#}");
        assert!(e.contains("401") && e.contains("Invalid API key"), "{e}");
    }

    /// The one call here that carries a credential takes no redirect: the
    /// target of the `Location` is a second listener, and the assertion is
    /// that nothing ever reached it, because what a followed hop would carry
    /// there is the key.
    #[tokio::test]
    async fn a_search_refuses_a_redirect_rather_than_carrying_the_key_after_it() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nlocation: http://{target_addr}/search\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let e = search(
            "q",
            None,
            Some(&endpoint(addr)),
            "test-key",
            FetchPolicy::AnyAddress,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let e = format!("{e:#}");
        assert!(e.contains("302") && e.contains("redirect"), "{e}");
        server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), target.accept())
                .await
                .is_err(),
            "the key must not have been carried to the redirect's target"
        );
    }

    /// A key that cannot be a header value is refused before anything is
    /// dialed, rather than smuggled to the endpoint.
    #[tokio::test]
    async fn a_key_that_cannot_be_a_header_is_refused() {
        let e = search(
            "q",
            None,
            Some("http://127.0.0.1:1/search"),
            "key\r\nwith-a-newline",
            FetchPolicy::AnyAddress,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(format!("{e:#}").contains("header"), "{e:#}");
    }

    /// The count is clamped to what the endpoint will serve, and a caller who
    /// names none gets the default.
    #[tokio::test]
    async fn the_count_a_caller_asks_for_is_clamped() {
        for (asked, on_the_wire) in [(Some(100), "count=20"), (Some(0), "count=1"), (None, "count=5")] {
            let (addr, server) = serve_recording("200 OK", r#"{"web":{"results":[]}}"#.to_string()).await;
            search(
                "q",
                asked,
                Some(&endpoint(addr)),
                "k",
                FetchPolicy::AnyAddress,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
            let request = server.await.unwrap();
            assert!(request.contains(on_the_wire), "{asked:?}: {request}");
        }
    }

    /// A search that names no query, or no key, is refused before any lookup —
    /// there is no endpoint to be reached, so this passes with no network at
    /// all.
    #[tokio::test]
    async fn a_search_needs_a_query_and_a_key() {
        let cancel = CancellationToken::new();
        let e = search("   ", None, Some("http://127.0.0.1:1/"), "key", FetchPolicy::AnyAddress, &cancel)
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("needs a query"), "{e:#}");
        let e = search("q", None, Some("http://127.0.0.1:1/"), " ", FetchPolicy::AnyAddress, &cancel)
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("needs an API key"), "{e:#}");
    }

    /// The search endpoint goes through the same address policy a fetch does:
    /// assembled strict, a session cannot search a loopback endpoint either,
    /// and nothing is dialed before that is decided.
    #[tokio::test]
    async fn the_strict_policy_checks_the_search_endpoint_too() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let e = search(
            "q",
            None,
            Some(&endpoint(addr)),
            "k",
            FetchPolicy::PublicOnly,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(format!("{e:#}").contains("public internet"), "{e:#}");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "nothing should have reached the listener"
        );
    }
}
