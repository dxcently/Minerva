//! The endpoint a script's `api_request` reaches, and the credential it names.
//!
//! The descriptor is the script's own, written where the call is made:
//!
//! ```rune
//! pub async fn call(input) {
//!     eidolon::api_request(#{ base_url: "https://r.jina.ai",
//!                             token_secret: "jina_reader",
//!                             auth: #{ header: "Authorization", prefix: "Bearer " } },
//!                          "GET", input.url, None).await
//! }
//! ```
//!
//! A tool file that carries its endpoint is a tool file that can be handed to
//! somebody else: what travels with it is the *name* of a secret, and what that
//! name resolves to is the recipient's own key. There is deliberately no
//! registry — a central list of endpoints would be a second file to keep in sync
//! with every tool, and the tool is the thing that knows what it reaches.
//!
//! ## What that costs, said out loud
//!
//! The descriptor decides where a credential may go, so a tool file is an
//! artifact that can authorize one. The trust model is a shell script's, and it
//! is the model sharing asks for: read a tool before you run it, and give a tool
//! a secret name of its own (`jina_reader`) rather than the name of something
//! else already in the store (`deepseek`, `brave_search`).
//!
//! ## Where the value is
//!
//! Never here. `token_secret` names an entry in the custodied store
//! (`eidolon secret set NAME`), `token_file` a file to read it from, `token_env`
//! a variable — resolved by [`resolve`] at call time, and put into a header by
//! `eidolon_tools::api::request` without ever being a string a script, a tool
//! result or a log can reach. A descriptor that declares `auth` and resolves
//! nothing is refused with the places it looked rather than called anonymously:
//! an endpoint that needs no key at all (Jina's reader) simply omits `auth`.

use std::path::Path;

use anyhow::{Context as _, bail};
use zeroize::Zeroizing;

use eidolon_tools::api::Endpoint;

use crate::permits::{GrantRow, Permits};

/// A credential on its way to a header, with the grant that authorized it.
/// The token is the caller's to put in a header and nothing else's to read;
/// the grant is what the audit line names, and what a test can hold the
/// injection to.
#[derive(Debug)]
pub struct Injected {
    pub token: Zeroizing<String>,
    /// The source that produced the value, in the store's spelling.
    pub source: String,
    /// The row that answered. Always here when the endpoint reached the
    /// custodied store, the file system or the environment on a tool's
    /// behalf — `resolve` refuses a source no row covers.
    pub grant: GrantRow,
}

/// The one refusal `resolve` makes on its own: the endpoint names a source
/// and no grant covers it. The caller — the one with the operator's answer
/// seam — may ask once, write the row, and retry; nobody else may.
#[derive(Clone, Debug)]
pub struct GrantRequired {
    pub tool: String,
    pub plugin: Option<String>,
    pub source: String,
}

impl std::fmt::Display for GrantRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this endpoint names {} — {}{} has no grant covering it",
            self.source,
            self.tool,
            self.plugin
                .as_deref()
                .map(|p| format!(" (plugin {p})"))
                .unwrap_or_default()
        )
    }
}

impl std::error::Error for GrantRequired {}

/// Every source the descriptor names, in the store's spelling and resolve's
/// precedence: the custodied secret, the file with `~/` expanded, the
/// variable. The grant check runs over all of them before any value is
/// read — a tool that names three places asks for three grants, whatever
/// the descriptors in the wild actually do.
fn source_refs(def: &Endpoint) -> Vec<String> {
    let mut refs = Vec::new();
    if let Some(name) = def.token_secret.as_deref() {
        refs.push(format!("secret:{name}"));
    }
    if let Some(path) = def.token_file.as_deref() {
        let expanded = eidolon_tools::resolve(Path::new("/"), &path.to_string_lossy());
        refs.push(format!("file:{}", expanded.display()));
    }
    if let Some(var) = def.token_env.as_deref() {
        refs.push(format!("env:{var}"));
    }
    refs
}

/// One descriptor out of the value a script passed `api_request` as its first
/// argument.
///
/// The name is optional — a script's own endpoint is right there, and a name
/// would only be for a table — so an unnamed one answers to `(inline)` rather
/// than an empty string in the error a bad path or a missing key gets.
pub fn descriptor(value: &serde_json::Value) -> anyhow::Result<Endpoint> {
    let mut def: Endpoint = serde_json::from_value(value.clone()).context(
        "api_request's first argument is the endpoint itself: a map with `base_url` in it, \
         the secret's name as `token_secret`, and `auth: #{ header, prefix }` if it takes one",
    )?;
    if def.name.trim().is_empty() {
        def.name = "(inline)".to_string();
    }
    Ok(def)
}

/// What the ledger names the service a request was to: the descriptor's
/// name when the script gave one, else the base_url's host — the third
/// party a reader can recognize. Never the path, never the query, never
/// a credential; `descriptor` normalizes an unnamed endpoint's name to
/// `(inline)`, so the sentinel is what falls through to the host, and an
/// unparseable one answers `(inline)` too rather than an empty string.
pub fn service(def: &Endpoint) -> String {
    if !def.name.is_empty() && def.name != "(inline)" {
        return def.name.clone();
    }
    let rest = def.base_url.split("://").last().unwrap_or("");
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        "(inline)".to_string()
    } else {
        host.to_string()
    }
}

/// One descriptor's credential, resolved: the custodied store first, then a
/// `token_file`, then the environment.
///
/// The store reader is the caller's (`external_value`, the one door out), which
/// is what keeps the value out of this crate's reach beyond a `Zeroizing` string
/// on its way to a header. The audit line names the secret and the consumer —
/// the tool whose script asked for it, or the plugin that contributed that
/// tool as `plugin:<name>`, so the log says whose effect the injection was —
/// and never the value.
pub fn resolve(
    def: &Endpoint,
    store: Option<&harnox::secrets::SecretStore>,
    tool: &str,
    plugin: Option<&str>,
    permits: Option<&Permits>,
) -> anyhow::Result<Option<Injected>> {
    let consumer = match plugin {
        Some(p) => format!("plugin:{p}"),
        None => tool.to_string(),
    };
    // A store that is not attached grants nothing — the fail-closed
    // reading, and the honest one for a host assembled without one.
    let rows = &permits.map(|p| p.rows()).unwrap_or_default();
    // The check, before any value is read: no row, no value. The lookup is
    // by the source's *reference* — what the descriptor names — so a tool
    // that names a source it has no grant for stops here even when the
    // source would have turned up nothing; naming is the act being
    // authorized, and the ask-once exists for exactly this stop.
    let sources = source_refs(def);
    let granted: Vec<Option<GrantRow>> =
        sources.iter().map(|source| rows.granted(tool, source)).collect();
    if let Some(uncovered) = sources.iter().zip(&granted).find_map(|(s, g)| match g {
        None => Some(s),
        Some(_) => None,
    }) {
        return Err(GrantRequired {
            tool: tool.to_string(),
            plugin: plugin.map(str::to_string),
            source: uncovered.clone(),
        }
        .into());
    }
    // The value, in the same precedence the grants were checked in. The
    // audit line rides the injection and names the grant that authorized
    // it — `consumer` keeps the spelling the log has always used for whose
    // effect this is, `source` and `grant` say whose permission.
    // The row that answers for the source that actually produced the value.
    fn audited(
        sources: &[String],
        granted: &[Option<GrantRow>],
        source: &str,
    ) -> (String, GrantRow) {
        let (i, _) = sources
            .iter()
            .enumerate()
            .find(|(_, s)| *s == source)
            .expect("the source came from the same list the grants were checked against");
        let grant = granted[i].clone().expect("checked above");
        (source.to_string(), grant)
    }

    if let Some(name) = def.token_secret.as_deref()
        && let Some(value) = store.and_then(|s| s.external_value(name))
    {
        let (source, grant) = audited(&sources, &granted, &format!("secret:{name}"));
        tracing::info!(source = %source, grant = %grant_line(&grant), consumer = %consumer, "secret injected");
        return Ok(Some(Injected { token: value, source, grant }));
    }
    let from_file = def.token_file.as_deref().and_then(|path| {
        let expanded = eidolon_tools::resolve(Path::new("/"), &path.to_string_lossy());
        read_token_file(Some(path)).map(|token| (token, format!("file:{}", expanded.display())))
    });
    if let Some((token, source)) = from_file {
        let (source, grant) = audited(&sources, &granted, &source);
        tracing::info!(source = %source, grant = %grant_line(&grant), consumer = %consumer, "secret injected");
        return Ok(Some(Injected { token: Zeroizing::new(token), source, grant }));
    }
    if let Some(var) = def.token_env.as_deref()
        && let Ok(value) = std::env::var(var)
    {
        let (source, grant) = audited(&sources, &granted, &format!("env:{var}"));
        tracing::info!(source = %source, grant = %grant_line(&grant), consumer = %consumer, "secret injected");
        return Ok(Some(Injected { token: Zeroizing::new(value), source, grant }));
    }
    if def.auth.is_some() {
        bail!(
            "this endpoint wants a credential and {}",
            def.credential_hint()
        );
    }
    Ok(None)
}

/// What the audit line says about the row that answered: who granted it and
/// when, which is the whole of what a reader needs to know about a yes.
fn grant_line(row: &GrantRow) -> String {
    format!("granted by {} at {}", row.granted_by, row.granted_at)
}

/// A `token_file`, with a leading `~/` expanded the way every other path the
/// operator writes is. A file that is not there is not an error *here*: it is
/// one more place the key was not, and the call that wanted one lists them all.
fn read_token_file(path: Option<&Path>) -> Option<String> {
    let path = path?.to_path_buf();
    let path = eidolon_tools::resolve(Path::new("/"), &path.to_string_lossy());
    let value = std::fs::read_to_string(&path).ok()?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permits::Rows;

    /// A store with one grant per subject kind, written through the real
    /// checker to a temp file. The probes vouch for `probe`, the tool these
    /// tests run as.
    fn granted() -> Permits {
        let path = std::env::temp_dir().join(format!("endpoint-permits-{}", std::process::id()));
        let p = Permits::at(path);
        let host = crate::Host::new(std::path::PathBuf::from("/t"));
        p.attach_checker(std::sync::Arc::new(move |src| {
            crate::permits::check_composed(src, crate::policy::DEFAULT_POLICY, &host)
        }));
        let key = probe_file();
        std::fs::write(&key, "from-file\n").unwrap();
        let mut rows = Rows::default();
        for source in [
            "secret:stored_key".to_string(),
            format!("file:{}", key.display()),
            "env:PROBE_TOKEN".to_string(),
        ] {
            rows.grant(GrantRow {
                tool: Some("probe".into()),
                source,
                granted_by: "noah".into(),
                granted_at: "2026-09-28T00:00:00Z".into(),
            wide_plugin: None,
            });
        }
        p.write(&rows).unwrap();
        p
    }

    fn probe_file() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("endpoint-probe-{}.key", std::process::id()))
    }

    /// A descriptor comes out of the map a script wrote: the endpoint, the name
    /// of the key, the header it rides in. A name on it is optional, because
    /// there is no table for one to matter to.
    #[test]
    fn a_descriptor_comes_out_of_the_value_a_script_passed() {
        let def = descriptor(&serde_json::json!({
            "base_url": "https://r.jina.ai",
            "token_secret": "jina_reader",
            "auth": { "header": "Authorization", "prefix": "Bearer " }
        }))
        .unwrap();
        assert_eq!(def.name, "(inline)");
        assert_eq!(def.base_url, "https://r.jina.ai");
        assert_eq!(def.token_secret.as_deref(), Some("jina_reader"));
        assert_eq!(def.auth.unwrap().prefix, "Bearer ");

        let named = descriptor(&serde_json::json!({
            "name": "mine", "base_url": "https://x.example"
        }))
        .unwrap();
        assert_eq!(named.name, "mine");

        // No endpoint is the one thing that cannot be inferred: the error says
        // what the argument is for.
        let e = descriptor(&serde_json::json!({ "token_secret": "k" })).unwrap_err();
        assert!(format!("{e:#}").contains("base_url"), "{e:#}");
        let e = descriptor(&serde_json::json!("jina")).unwrap_err();
        assert!(format!("{e:#}").contains("base_url"), "{e:#}");
    }

    /// The store is the first place looked in, a file is the second, and a
    /// descriptor that declared `auth` and found neither is refused with both
    /// places named. A descriptor with no `auth` never grows a key.
    #[test]
    fn a_declared_source_with_a_grant_injects() {
        let dir = tempfile::tempdir().unwrap();
        let key = probe_file();
        // SAFETY-adjacent, and single-threaded here: the test runtime is
        // current-thread, so no other test reads the environment mid-write.
        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var("PROBE_TOKEN", "from-env")
        };
        let store = harnox::secrets::SecretStore::at(dir.path().join("secrets"));
        store.set("stored_key", "from-store", None, vec![]).unwrap();
        let permits = granted();

        let stored = descriptor(&serde_json::json!({
            "base_url": "https://s.example", "token_secret": "stored_key",
            "auth": { "header": "Authorization", "prefix": "Bearer " }
        }))
        .unwrap();
        let injected =
            resolve(&stored, Some(&store), "probe", None, Some(&permits)).unwrap().unwrap();
        assert_eq!(injected.token.as_str(), "from-store");
        // what the audit line names: the source, and the grant that
        // authorized it
        assert_eq!(injected.source, "secret:stored_key");
        assert_eq!(injected.grant.granted_by, "noah");

        let filed = descriptor(&serde_json::json!({
            "base_url": "https://f.example", "token_file": key.display().to_string(),
            "auth": { "header": "Authorization", "prefix": "Bearer " }
        }))
        .unwrap();
        let injected =
            resolve(&filed, Some(&store), "probe", None, Some(&permits)).unwrap().unwrap();
        assert_eq!(injected.token.as_str(), "from-file");
        assert_eq!(injected.source, format!("file:{}", key.display()));

        let envd = descriptor(&serde_json::json!({
            "base_url": "https://e.example", "token_env": "PROBE_TOKEN",
            "auth": { "header": "Authorization", "prefix": "Bearer " }
        }))
        .unwrap();
        let injected =
            resolve(&envd, Some(&store), "probe", None, Some(&permits)).unwrap().unwrap();
        assert_eq!(injected.token.as_str(), "from-env");
        assert_eq!(injected.source, "env:PROBE_TOKEN");
        #[allow(unused_unsafe)]
        unsafe {
            std::env::remove_var("PROBE_TOKEN")
        };

        let anonymous = descriptor(&serde_json::json!({ "base_url": "https://a.example" })).unwrap();
        assert!(
            resolve(&anonymous, Some(&store), "probe", None, Some(&permits))
                .unwrap()
                .is_none(),
            "a descriptor with no `auth` never grows a key"
        );
    }

    /// A source no row covers is a typed refusal carrying everything the
    /// ask-once needs — and, the row once written through the same path the
    /// CLI grant verb uses, the next call injects. A row for a *different*
    /// path is not a row for this one.
    #[test]
    fn an_uncovered_source_refuses_until_granted_and_a_different_path_re_asks() {
        let dir = tempfile::tempdir().unwrap();
        let store = harnox::secrets::SecretStore::at(dir.path().join("secrets"));
        store.set("stored_key", "from-store", None, vec![]).unwrap();
        let path = std::env::temp_dir().join(format!("endpoint-permits-2-{}", std::process::id()));
        let permits = Permits::at(path);
        let host = crate::Host::new(std::path::PathBuf::from("/t"));
        permits.attach_checker(std::sync::Arc::new(move |src| {
            crate::permits::check_composed(src, crate::policy::DEFAULT_POLICY, &host)
        }));
        let key = probe_file();
        std::fs::write(&key, "from-file\n").unwrap();
        let def = descriptor(&serde_json::json!({
            "base_url": "https://f.example", "token_file": key.display().to_string(),
            "auth": { "header": "Authorization", "prefix": "Bearer " }
        }))
        .unwrap();

        let err = resolve(&def, Some(&store), "probe", None, Some(&permits)).unwrap_err();
        let need = err.downcast_ref::<GrantRequired>().expect("the typed refusal");
        assert_eq!(need.tool, "probe");
        assert_eq!(need.source, format!("file:{}", key.display()));

        // a row for a different path does not answer this one
        let mut rows = permits.rows();
        rows.grant(GrantRow {
            tool: Some("probe".into()),
            source: "file:/somewhere/else.key".into(),
            granted_by: "noah".into(),
            granted_at: "2026-09-28T00:00:00Z".into(),
        wide_plugin: None,
        });
        permits.write(&rows).unwrap();
        let err = resolve(&def, Some(&store), "probe", None, Some(&permits)).unwrap_err();
        assert!(err.downcast_ref::<GrantRequired>().is_some(), "re-asks");

        // the grant, written the way the ask-once and the CLI both do
        let mut rows = permits.rows();
        rows.grant(GrantRow {
            tool: Some("probe".into()),
            source: format!("file:{}", key.display()),
            granted_by: "noah".into(),
            granted_at: "2026-09-28T00:00:00Z".into(),
        wide_plugin: None,
        });
        permits.write(&rows).unwrap();
        let injected = resolve(&def, Some(&store), "probe", None, Some(&permits))
            .unwrap()
            .unwrap();
        assert_eq!(injected.token.as_str(), "from-file");

        // a removed grant stops the injections again
        let mut rows = permits.rows();
        rows.grant.clear();
        permits.write(&rows).unwrap();
        assert!(resolve(&def, Some(&store), "probe", None, Some(&permits)).is_err());
    }

    /// A store that is not attached grants nothing: the fail-closed
    /// reading for a host assembled without one.
    #[test]
    fn a_host_with_no_store_grants_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = harnox::secrets::SecretStore::at(dir.path().join("secrets"));
        store.set("stored_key", "from-store", None, vec![]).unwrap();
        let def = descriptor(&serde_json::json!({
            "base_url": "https://s.example", "token_secret": "stored_key",
            "auth": { "header": "Authorization", "prefix": "Bearer " }
        }))
        .unwrap();
        let err = resolve(&def, Some(&store), "probe", None, None).unwrap_err();
        assert!(err.downcast_ref::<GrantRequired>().is_some(), "{err}");
    }

    /// End to end, the way a session runs it: one self-contained tool file, a
    /// key in the store, and one request answered by a socket on loopback with
    /// the credential on it and the body back.
    #[tokio::test]
    async fn a_self_contained_script_reaches_its_endpoint_with_the_key() {
        use crate::host::Host;
        use crate::script::ScriptTool;
        use eidolon_core::tool::{CallContext, Tool};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let store = harnox::secrets::SecretStore::at(dir.path().join("secrets"));
        store.set("probe_key", "from-the-store", None, vec![]).unwrap();

        // One HTTP/1.1 request read and one response written: loopback is the
        // only network a build sandbox has. What the test wants is on the
        // request side, so the request comes back from the task.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = "Title: Example Domain";
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
            request
        });

        let host = Host::new(dir.path().to_path_buf());
        host.attach_secrets(Arc::new(store));
        // The grant the door requires before the value moves: the same row
        // `eidolon plugins grant probe secret:probe_key` would write.
        let permits = Permits::at(dir.path().join("policy.permits.rn"));
        {
            let host_for_check = Host::new(dir.path().to_path_buf());
            permits.attach_checker(Arc::new(move |src| {
                crate::permits::check_composed(src, crate::policy::DEFAULT_POLICY, &host_for_check)
            }));
        }
        let mut rows = Rows::default();
        rows.grant(GrantRow {
            tool: Some("probe".into()),
            source: "secret:probe_key".into(),
            granted_by: "noah".into(),
            granted_at: "2026-09-28T00:00:00Z".into(),
        wide_plugin: None,
        });
        permits.write(&rows).unwrap();
        host.attach_permits(Arc::new(permits));

        let src = [
            r#"pub fn manifest() { #{ name: "probe", description: "d", approval: "read_only", input_schema: #{ "type": "object" } } }
               pub async fn call(input) {
                   eidolon::api_request(#{
                       base_url: "http://"#,
            &addr.to_string(),
            r#"",
                       token_secret: "probe_key",
                       auth: #{ header: "Authorization", prefix: "Bearer " },
                   }, "GET", input.url, None).await
               }"#,
        ]
        .concat();
        let tool = ScriptTool::compile("probe", &src, host).unwrap();
        let out = tool
            .call(
                serde_json::json!({ "url": "https://example.com/x" }),
                CallContext {
                    cwd: dir.path().to_path_buf(),
                    cancel: tokio_util::sync::CancellationToken::new(),
                    call_id: "test".into(),
                },
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("Title: Example Domain"),
            "{}",
            out.content
        );
        assert!(out.content.contains("\"status\":200"), "{}", out.content);

        let request = server.await.unwrap();
        // The URL-shaped path is the reader's contract: it goes through as a
        // path on the descriptor's host, not as an authority of its own.
        assert!(
            request.starts_with("GET /https://example.com/x "),
            "{request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer from-the-store"),
            "{request}"
        );
    }

    /// An armed yolo switch answers the credential ask-once, and the row it
    /// writes says `yolo`, not the operator's name. The host here has no
    /// dispatcher at all — nobody to draw a dialog — so a success is proof
    /// the answer came from the switch, and the bug this pins is the ask
    /// (or the refusal) a yolo session used to hit on its first `jina`
    /// request. A disarmed switch leaves the ask standing: no dispatcher,
    /// no answer, fail closed.
    #[tokio::test]
    async fn an_armed_yolo_switch_grants_and_records_yolo_a_disarmed_one_asks() {
        use crate::host::Host;
        use crate::script::ScriptTool;
        use eidolon_core::tool::{CallContext, Tool};
        use eidolon_core::yolo::Switch;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let store = harnox::secrets::SecretStore::at(dir.path().join("secrets"));
        store.set("jina_reader", "from-the-store", None, vec![]).unwrap();

        // One request, answered on loopback: reached only in the armed half.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = "Title: Example Domain";
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
            request
        });

        let src = [
            r#"pub fn manifest() { #{ name: "probe", description: "d", approval: "read_only", input_schema: #{ "type": "object" } } }
               pub async fn call(input) {
                   eidolon::api_request(#{
                       base_url: "http://"#,
            &addr.to_string(),
            r#"",
                       token_secret: "jina_reader",
                       auth: #{ header: "Authorization", prefix: "Bearer " },
                   }, "GET", input.url, None).await
               }"#,
        ]
        .concat();

        let permits = Arc::new(Permits::at(dir.path().join("policy.permits.rn")));
        {
            let host_for_check = Host::new(dir.path().to_path_buf());
            permits.attach_checker(Arc::new(move |src| {
                crate::permits::check_composed(src, crate::policy::DEFAULT_POLICY, &host_for_check)
            }));
        }
        let ctx = |cwd: std::path::PathBuf| CallContext {
            cwd,
            cancel: tokio_util::sync::CancellationToken::new(),
            call_id: "test".into(),
        };

        // Armed: the ask-once is answered by the switch and the grant is
        // written as yolo's, though nobody was here to ask.
        let host = Host::new(dir.path().to_path_buf());
        host.attach_secrets(Arc::new(store));
        host.attach_permits(Arc::clone(&permits));
        host.attach_yolo(Switch::new(true));
        let tool = ScriptTool::compile("probe", &src, Arc::clone(&host)).unwrap();
        let out = tool
            .call(
                serde_json::json!({ "url": "https://example.com/x" }),
                ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(
            server
                .await
                .unwrap()
                .to_ascii_lowercase()
                .contains("authorization: bearer from-the-store"),
            "the request went out with the credential"
        );
        let rows = permits.rows();
        assert_eq!(rows.grant.len(), 1, "{:?}", rows.grant);
        assert_eq!(rows.grant[0].source, "secret:jina_reader");
        assert_eq!(rows.grant[0].tool.as_deref(), Some("probe"));
        assert_eq!(rows.grant[0].granted_by, "yolo", "a waived yes is not a person's");

        // Disarmed: nobody to ask, so the call refuses and nothing is
        // written — the fail-closed reading yolo must not widen. Fresh
        // permits: the row the armed half wrote stands, as a grant should,
        // so this half proves the ask on a source nothing covers yet.
        let permits = Arc::new(Permits::at(dir.path().join("again.permits.rn")));
        {
            let host_for_check = Host::new(dir.path().to_path_buf());
            permits.attach_checker(Arc::new(move |src| {
                crate::permits::check_composed(src, crate::policy::DEFAULT_POLICY, &host_for_check)
            }));
        }
        let host = Host::new(dir.path().to_path_buf());
        let store = harnox::secrets::SecretStore::at(dir.path().join("secrets"));
        store.set("jina_reader", "from-the-store", None, vec![]).unwrap();
        host.attach_secrets(Arc::new(store));
        host.attach_permits(Arc::clone(&permits));
        host.attach_yolo(Switch::new(false));
        let tool = ScriptTool::compile("probe", &src, Arc::clone(&host)).unwrap();
        let out = tool
            .call(
                serde_json::json!({ "url": "https://example.com/x" }),
                ctx(dir.path().to_path_buf()),
            )
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("nobody is here to grant it"), "{}", out.content);
        assert_eq!(permits.rows().grant.len(), 0, "no row was written");
    }
}
