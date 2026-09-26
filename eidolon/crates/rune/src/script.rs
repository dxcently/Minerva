//! Compile a script once, run `call(input)` per dispatch on an isolated
//! thread.
//!
//! Once means once: the unit is compiled against the host's shared
//! [`Context`](rune::Context) at load and every call runs it on the host's
//! shared runtime. The per-call cancellation token, which used to force a
//! rebuild of the context (and so of the unit) for every call, reaches the
//! host functions through [`with_cancel`] instead.

use std::sync::Arc;

use anyhow::{Context as _, anyhow, bail};
use async_trait::async_trait;
use rune::termcolor::Buffer;
use rune::{Diagnostics, Source, Sources, Unit, Value, Vm};
use serde::Deserialize;
use serde::de::IntoDeserializer;

use eidolon_core::tool::{CallContext, Tool, ToolManifest, ToolOutput};

use crate::host::{Host, with_cancel, with_tool};

pub struct ScriptTool {
    manifest: ToolManifest,
    unit: Arc<Unit>,
    name: String,
    host: Arc<Host>,
}

impl ScriptTool {
    /// Compile `src` against the host's context and run its `manifest()`.
    /// The unit is kept and shared by every call.
    pub fn compile(name: &str, src: &str, host: Arc<Host>) -> anyhow::Result<Self> {
        let unit = compile_unit(name, src, &host)?;
        let mut vm = Vm::new(host.compiler()?.runtime.clone(), unit.clone());
        let value = vm
            .execute(["manifest"], ())
            .map_err(|e| anyhow!("script has no `pub fn manifest()`: {e}"))?
            .complete()
            .into_result()
            .map_err(|e| anyhow!("manifest() failed: {e}"))?;
        let json = rune_to_json(&value).context("manifest() must return a plain object")?;
        let mut manifest: ToolManifest =
            serde_json::from_value(json).context("manifest() has the wrong shape")?;
        if manifest.name.is_empty() {
            manifest.name = name.to_string();
        }
        Ok(ScriptTool {
            manifest,
            unit,
            name: name.to_string(),
            host,
        })
    }
}

/// Compile one source against the host's shared context. About 0.1 ms for
/// a built-in; the context it compiles against is the expensive part, and
/// that is built once per host.
fn compile_unit(name: &str, src: &str, host: &Arc<Host>) -> anyhow::Result<Arc<Unit>> {
    let compiler = host.compiler()?;
    let mut sources = Sources::new();
    sources
        .insert(Source::new(name, src).map_err(|e| anyhow!("source: {e}"))?)
        .map_err(|e| anyhow!("inserting source: {e}"))?;
    let mut diagnostics = Diagnostics::new();
    let result = rune::prepare(&mut sources)
        .with_context(&compiler.context)
        .with_diagnostics(&mut diagnostics)
        .build();
    match result {
        Ok(u) => Ok(Arc::new(u)),
        Err(_) => {
            let mut buf = Buffer::no_color();
            let rendered = match diagnostics.emit(&mut buf, &sources) {
                Ok(()) => String::from_utf8_lossy(buf.as_slice()).into_owned(),
                Err(e) => format!("<could not render diagnostics: {e}>"),
            };
            bail!("script `{name}` failed to compile:\n{}", rendered.trim());
        }
    }
}

/// A JSON value as a Rune value. A value that will not convert becomes an
/// empty object rather than a unit: a script's `input.path` on a unit raises
/// `Field \`path\` not available on \`::std::tuple::Tuple\``, which is an
/// interpreter detail with no bearing on what the caller did wrong. An empty
/// object fails the same way every missing field does, and the dispatcher's
/// schema check catches it before this point in the ordinary case.
pub fn json_to_rune(v: &serde_json::Value) -> Value {
    Value::deserialize(v.clone().into_deserializer()).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "a tool input did not convert to a rune value; passing an empty object");
        Value::deserialize(serde_json::Value::Object(Default::default()).into_deserializer()).expect("an empty object always converts")
    })
}

pub fn rune_to_json(v: &Value) -> anyhow::Result<serde_json::Value> {
    serde_json::to_value(v).context("serialising a rune value")
}

/// Interpret whatever `call()` returned.
fn interpret(value: Value) -> ToolOutput {
    // A Rune `Result` (from `?` or an explicit Err) — unwrap it first.
    if let Ok(r) = rune::from_value::<Result<Value, Value>>(value.clone()) {
        return match r {
            Ok(v) => interpret_plain(v),
            Err(e) => ToolOutput::error(display(&e)),
        };
    }
    interpret_plain(value)
}

fn interpret_plain(value: Value) -> ToolOutput {
    match rune_to_json(&value) {
        Ok(serde_json::Value::String(s)) => ToolOutput::ok(s),
        Ok(serde_json::Value::Null) => ToolOutput::ok(""),
        Ok(serde_json::Value::Object(o)) if o.contains_key("content") => ToolOutput {
            content: o
                .get("content")
                .and_then(|c| c.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| o["content"].to_string()),
            is_error: o.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false),
            ends_turn: false,
        },
        Ok(other) => ToolOutput::ok(other.to_string()),
        Err(_) => ToolOutput::ok(format!("{value:?}")),
    }
}

fn display(v: &Value) -> String {
    match rune_to_json(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => format!("{v:?}"),
    }
}

#[async_trait]
impl Tool for ScriptTool {
    fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }

    async fn call(&self, input: serde_json::Value, ctx: CallContext) -> anyhow::Result<ToolOutput> {
        let name = self.name.clone();
        let unit = self.unit.clone();
        let host = self.host.clone();
        let cancel = ctx.cancel.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name(format!("rune-{name}"))
            .spawn(move || {
                // The token is this thread's for the length of the call:
                // the VM is pinned here, so every host function it invokes
                // reads this one and no other.
                let result = with_tool(&name, || with_cancel(cancel, || -> anyhow::Result<ToolOutput> {
                    let runtime = host.compiler()?.runtime.clone();
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    rt.block_on(async move {
                        let mut vm = Vm::new(runtime, unit);
                        let mut exec =
                            vm.execute(["call"], (json_to_rune(&input),)).map_err(|e| {
                                anyhow!("script has no `pub async fn call(input)`: {e}")
                            })?;
                        let value = exec
                            .async_complete()
                            .await
                            .into_result()
                            .map_err(|e| anyhow!("{e}"))?;
                        Ok(interpret(value))
                    })
                }));
                let _ = tx.send(result);
            })
            .context("spawning the rune thread")?;
        rx.await.context("the rune thread ended without a result")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn ctx(dir: &std::path::Path) -> CallContext {
        CallContext {
            cwd: dir.to_path_buf(),
            cancel: CancellationToken::new(),
            call_id: "test".into(),
        }
    }

    #[tokio::test]
    async fn builtin_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.txt"), "hello\nworld\n").unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let tool = ScriptTool::compile("read", crate::BUILTINS[0].1, host).unwrap();
        assert_eq!(tool.manifest().name, "read");
        let out = tool
            .call(serde_json::json!({ "path": "x.txt" }), ctx(dir.path()))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("     2\tworld"));
    }

    #[tokio::test]
    async fn errors_become_error_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let tool = ScriptTool::compile("read", crate::BUILTINS[0].1, host).unwrap();
        let out = tool
            .call(
                serde_json::json!({ "path": "missing.txt" }),
                ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("missing.txt"));
    }

    /// The `fetch` built-in carries the address policy of the host it was
    /// compiled on — the seam a consumer assembles a session through, rather
    /// than a constant in the script. A host assembled strict cannot reach
    /// loopback; the same script on a host assembled permissive reaches the
    /// network as it always has.
    #[tokio::test]
    async fn the_fetch_builtin_carries_its_hosts_address_policy() {
        let dir = tempfile::tempdir().unwrap();
        // A loopback port nothing is listening on, so the permissive half
        // below gets the connection's own complaint rather than a wait.
        let port = {
            let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            free.local_addr().unwrap().port()
        };
        let url = format!("http://127.0.0.1:{port}/");

        let strict = Host::with_fetch_policy(
            dir.path().to_path_buf(),
            eidolon_tools::web::FetchPolicy::PublicOnly,
        );
        let tool = ScriptTool::compile("fetch", crate::BUILTINS[5].1, strict).unwrap();
        let out = tool
            .call(serde_json::json!({ "url": url }), ctx(dir.path()))
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("public internet"), "{}", out.content);

        let permissive = Host::new(dir.path().to_path_buf());
        assert_eq!(
            permissive.fetch_policy(),
            eidolon_tools::web::FetchPolicy::AnyAddress,
            "the host eidolon's own surfaces build has not changed"
        );
        let tool = ScriptTool::compile("fetch", crate::BUILTINS[5].1, permissive).unwrap();
        let out = tool
            .call(serde_json::json!({ "url": url }), ctx(dir.path()))
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.content);
        assert!(
            out.content.contains("127.0.0.1") && !out.content.contains("public internet"),
            "the permissive host dialed it: {}",
            out.content
        );
    }

    /// The `search` primitive is registered on every host and answers a call
    /// on a host with no key by refusing: the *tool* is gated on the key, and
    /// this is the fail-closed half — nothing is sent, because there is no key
    /// to send.
    #[tokio::test]
    async fn a_search_on_a_host_with_no_key_is_refused_at_call_time() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let src = r#"pub fn manifest() { #{ name: "probe", description: "p", approval: "read_only", input_schema: #{ "type": "object" } } }
                     pub async fn call(input) { eidolon::web_search("anything", None).await }"#;
        let tool = ScriptTool::compile("probe", src, host).unwrap();
        let out = tool
            .call(serde_json::json!({}), ctx(dir.path()))
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("no search key"), "{}", out.content);
    }

    /// A script's own command wrapper can take the no-shell primitive: the
    /// argument is one argument whatever it contains, because no command line
    /// was ever assembled for anything to parse.
    #[tokio::test]
    async fn a_script_runs_one_program_without_a_shell() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let src = r#"pub fn manifest() { #{ name: "probe", description: "p", approval: "read_only", input_schema: #{ "type": "object" } } }
                     pub async fn call(input) { eidolon::exec(input.program, input.args, None).await }"#;
        let tool = ScriptTool::compile("probe", src, host).unwrap();
        let out = tool
            .call(
                serde_json::json!({ "program": "printf", "args": ["%s", "a;b $(id) | cat"] }),
                ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "a;b $(id) | cat");
    }

    #[tokio::test]
    async fn all_builtins_compile() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().to_path_buf());
        for (name, src) in crate::BUILTINS {
            let t = ScriptTool::compile(name, src, host.clone()).unwrap();
            assert_eq!(&t.manifest().name, name);
            assert!(t.manifest().input_schema.is_object());
        }
    }

    /// The cancellation token reaches a host function through the call's
    /// thread, not through the compiled unit: a `bash` that would run for
    /// five seconds stops when the token is cancelled.
    #[tokio::test]
    async fn cancellation_reaches_a_running_shell() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let bash = ScriptTool::compile("bash", crate::BUILTINS[3].1, host).unwrap();
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            c.cancel();
        });
        let t = std::time::Instant::now();
        let out = bash
            .call(
                serde_json::json!({ "command": "sleep 5" }),
                CallContext {
                    cwd: dir.path().to_path_buf(),
                    cancel,
                    call_id: "test".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            t.elapsed() < std::time::Duration::from_secs(3),
            "the call ran on after cancellation: {:?}",
            t.elapsed()
        );
        // A cancelled command is killed and says so; that is a result, not
        // a tool error (see `eidolon_tools::shell`).
        assert!(out.content.contains("killed"), "{}", out.content);
    }

    #[tokio::test]
    async fn bash_and_edit() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let bash = ScriptTool::compile("bash", crate::BUILTINS[3].1, host.clone()).unwrap();
        let out = bash
            .call(
                serde_json::json!({ "command": "printf abc > f.txt; cat f.txt" }),
                ctx(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(out.content, "abc");
        let edit = ScriptTool::compile("edit", crate::BUILTINS[2].1, host).unwrap();
        let out = edit
            .call(
                serde_json::json!({ "path": "f.txt", "old_str": "b", "new_str": "B" }),
                ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "aBc"
        );
    }

    /// The `background` parameter detaches through the script: the call
    /// answers at once with a log path, the command writes there after
    /// the call is over, and the foreground path is unchanged when the
    /// flag is false.
    #[tokio::test]
    async fn bash_background_returns_a_log_and_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let bash = ScriptTool::compile("bash", crate::BUILTINS[3].1, host).unwrap();
        let out = bash
            .call(
                serde_json::json!({ "command": "echo detached", "background": true }),
                ctx(dir.path()),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("running in the background"),
            "{}",
            out.content
        );
        let log = out
            .content
            .lines()
            .find_map(|l| l.strip_prefix("log: "))
            .unwrap_or_else(|| panic!("no log path: {}", out.content))
            .trim()
            .to_string();
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(
            std::fs::read_to_string(&log).unwrap().contains("detached"),
            "{}",
            out.content
        );

        // Absent or false, the flag changes nothing about the foreground
        // contract — this is the branch a plain `bash` call must keep
        // taking.
        for (label, input) in [
            ("absent", serde_json::json!({ "command": "echo waited" })),
            (
                "false",
                serde_json::json!({ "command": "echo waited", "background": false }),
            ),
        ] {
            let out = bash.call(input, ctx(dir.path())).await.unwrap();
            // The primitive passes output through verbatim; `echo` ends
            // in a newline.
            assert_eq!(out.content.trim_end(), "waited", "background = {label}");
        }
    }
}

#[cfg(test)]
mod mneme_tests {
    use super::*;
    use eidolon_remote::{Credential, Mneme};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn vault_builtins_register_only_with_a_vault() {
        let dir = tempfile::tempdir().unwrap();
        let plain = Host::new(dir.path().to_path_buf());
        let mut reg = eidolon_core::tool::ToolRegistry::new();
        crate::register_builtins(&mut reg, &plain, dir.path()).unwrap();
        assert!(reg.get("mneme_rpc").is_none());
        assert!(!reg.guidance().unwrap().contains("Mneme is the remote vault"));

        let tf = dir.path().join("tok");
        std::fs::write(&tf, "tok").unwrap();
        let host = Host::with_mneme(
            dir.path().to_path_buf(),
            Mneme::new("http://127.0.0.1:1/mcp", Credential::TokenFile(tf)),
        );
        let mut reg = eidolon_core::tool::ToolRegistry::new();
        crate::register_builtins(&mut reg, &host, dir.path()).unwrap();
        assert!(reg.get("mneme_rpc").is_some());
        // No natural-language tool beside it: `mneme_dispatch` was cut
        // (2026-09-14) once the vault command channel's inline seam covered
        // the same ground, so `mneme_rpc` is the vault's whole model-facing
        // surface and its prompt is the only vault guidance that reaches a
        // request.
        assert!(reg.get("mneme_dispatch").is_none());
        assert!(
            !reg.guidance()
                .unwrap()
                .contains("not just an outer executed disposition")
        );
        let guidance = reg.guidance_for(&["mneme_rpc".into()]).unwrap();
        assert!(guidance.contains("vault command channel"), "{guidance}");
        assert!(!guidance.contains("not just an outer executed disposition"), "{guidance}");
        assert!(guidance.contains("read back important mutations"));
        // The primitive is reachable and reports the connection failure as an error output.
        let tool = reg.get("mneme_rpc").unwrap().clone();
        let out = tool
            .call(
                serde_json::json!({ "function": "list" }),
                CallContext {
                    cwd: dir.path().to_path_buf(),
                    cancel: CancellationToken::new(),
                    call_id: "test".into(),
                },
            )
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.content);
    }
}
