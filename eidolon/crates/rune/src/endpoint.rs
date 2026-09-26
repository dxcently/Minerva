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

/// One descriptor's credential, resolved: the custodied store first, then a
/// `token_file`, then the environment.
///
/// The store reader is the caller's (`external_value`, the one door out), which
/// is what keeps the value out of this crate's reach beyond a `Zeroizing` string
/// on its way to a header. The audit line names the secret and the consumer —
/// the tool whose script asked for it — and never the value.
pub fn resolve(
    def: &Endpoint,
    store: Option<&harnox::secrets::SecretStore>,
    consumer: &str,
) -> anyhow::Result<Option<Zeroizing<String>>> {
    if let Some(name) = def.token_secret.as_deref()
        && let Some(value) = store.and_then(|s| s.external_value(name))
    {
        tracing::info!(secret = %name, consumer = %consumer, "secret injected");
        return Ok(Some(value));
    }
    let token = read_token_file(def.token_file.as_deref())
        .or_else(|| {
            def.token_env
                .as_deref()
                .and_then(|var| std::env::var(var).ok())
        })
        .map(Zeroizing::new);
    if def.auth.is_some() && token.is_none() {
        bail!(
            "this endpoint wants a credential and {}",
            def.credential_hint()
        );
    }
    Ok(token)
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
    fn a_credential_resolves_from_the_store_then_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("jina.key");
        std::fs::write(&key, "from-file\n").unwrap();
        let store = harnox::secrets::SecretStore::at(dir.path().join("secrets"));
        store.set("stored_key", "from-store", None, vec![]).unwrap();

        let stored = descriptor(&serde_json::json!({
            "base_url": "https://s.example", "token_secret": "stored_key",
            "auth": { "header": "Authorization", "prefix": "Bearer " }
        }))
        .unwrap();
        assert_eq!(
            resolve(&stored, Some(&store), "probe")
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("from-store")
        );

        let filed = descriptor(&serde_json::json!({
            "base_url": "https://f.example", "token_file": key.display().to_string(),
            "auth": { "header": "Authorization", "prefix": "Bearer " }
        }))
        .unwrap();
        assert_eq!(
            resolve(&filed, Some(&store), "probe")
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("from-file")
        );

        let nowhere = descriptor(&serde_json::json!({
            "base_url": "https://n.example", "token_secret": "missing_key",
            "auth": { "header": "Authorization", "prefix": "Bearer " }
        }))
        .unwrap();
        let e = format!("{:#}", resolve(&nowhere, Some(&store), "probe").unwrap_err());
        assert!(e.contains("eidolon secret set missing_key"), "{e}");

        let anonymous = descriptor(&serde_json::json!({ "base_url": "https://a.example" })).unwrap();
        assert!(
            resolve(&anonymous, Some(&store), "probe").unwrap().is_none(),
            "an endpoint that needs no key must not grow one"
        );
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
}
