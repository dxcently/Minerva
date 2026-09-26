//! The `eidolon` module exposed to scripts, and the [`Host`] state its
//! functions close over.
//!
//! Every function is registered as a closure (`module.function(name,
//! closure).build()`), capturing a `Weak<Host>`; async ones return an
//! `async move` block, which Rune awaits like any other future. Strings and
//! integers cross the boundary natively; structured results cross as JSON
//! text a script parses with `json::from_string`, which keeps the surface
//! free of `Any` types that would need per-method registration.
//!
//! `ask_user`/`choices_user` need the dispatcher, and the dispatcher owns
//! the registry that owns the scripts that own this host — so the link is
//! a `Weak` set after construction ([`Host::attach`]). A script that asks
//! before attachment gets an error result, never a hang.
//!
//! ## One context, built once
//!
//! A Rune [`Context`] with the standard modules costs about 2.5 ms to
//! build; compiling one of the built-in tools against it costs 0.08 ms.
//! Building a context per script — and, worse, per *call* — was where a
//! six-tool startup spent 20 ms and every tool call spent 3 ms before it
//! did anything. So the host builds its context exactly once
//! ([`Host::compiler`]) and every script compiles against it and runs on
//! the one [`RuntimeContext`] derived from it.
//!
//! What made the per-call rebuild look necessary was the cancellation
//! token: it is per call, and the module's closures used to capture it.
//! It now travels through a thread-local instead ([`with_cancel`]) — sound
//! because every call already runs on a thread of its own with the VM
//! pinned to it, so the token the closures read is always the token of the
//! call that invoked them. The working directory is read from the
//! dispatcher at invocation time for the same reason: nothing in the
//! module may close over anything that belongs to one call.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};

use anyhow::anyhow;
use rune::runtime::RuntimeContext;
use rune::{Context, ContextError, Module, Value};
use tokio_util::sync::CancellationToken;

use eidolon_core::dispatch::Dispatcher;
use eidolon_core::tool::{CallOrigin, ToolCall};
use eidolon_remote::Mneme;
use eidolon_tools::web::FetchPolicy;
use zeroize::Zeroizing;

use crate::script::rune_to_json;

pub struct Host {
    /// The directory the host was built with — the fallback for a host
    /// with no dispatcher attached. Read [`Host::cwd`] instead.
    cwd: PathBuf,
    dispatcher: OnceLock<Weak<Dispatcher>>,
    mneme: Option<Arc<Mneme>>,
    /// What this host's `fetch` tool will connect to. A constructor argument
    /// rather than a late-attached field like the dispatcher and the swarm:
    /// it is a security decision, so the choice is made where the host is
    /// built instead of being left to a default that a missing attach step
    /// would quietly stand in for.
    fetch: FetchPolicy,
    /// This session's registration among its peers, when it has one. Set
    /// after construction like the dispatcher, and read *per call* rather
    /// than at module-build time — the context is warmed on a background
    /// thread the moment the host exists, so anything the module decided
    /// from this field would be racing that thread.
    swarm: OnceLock<Arc<eidolon_swarm::Presence>>,
    /// The key this session's `search` tool runs with, when the consumer that
    /// assembled the session gave it one. Set after construction like the
    /// swarm and read *per call*; the value goes into a header inside the
    /// primitive and never anywhere a script, a tool result or a log can
    /// reach it, which is why there is no public accessor for it. Wrapped so
    /// it wipes itself when the session is done with it.
    search_key: OnceLock<Zeroizing<String>>,
    /// The store a script's endpoint resolves its `token_secret` against — the
    /// one door a secret leaves by, held here so the resolution happens at call
    /// time, on the side of the boundary that has no script. Set after
    /// construction like the rest.
    store: OnceLock<Arc<harnox::secrets::SecretStore>>,
    /// The one context every script on this host compiles against, and
    /// the runtime they all execute on. Built on first use; the error, if
    /// the standard modules refuse to install, is kept so every caller
    /// sees the same one rather than retrying.
    compiler: OnceLock<Result<Compiler, String>>,
}

/// A compile-time [`Context`] and the [`RuntimeContext`] made from it —
/// the two halves of "a Rune environment", kept together because a unit
/// compiled against one context must run on that context's runtime.
pub struct Compiler {
    pub context: Context,
    pub runtime: Arc<RuntimeContext>,
}

thread_local! {
    /// The cancellation token of the tool call running on this thread, if
    /// one is. Read by the host functions, set by [`with_cancel`].
    static CANCEL: RefCell<Option<CancellationToken>> = const { RefCell::new(None) };
    /// The name of the tool whose script is running on this thread, if one
    /// is. Read by the primitives that inject a credential, so the audit line
    /// names the tool that asked for the key rather than the harness, and set
    /// by [`with_tool`]. A thread-local for the same reason the token is one:
    /// nothing in the module may close over anything that belongs to a single
    /// call, and a call is the whole of its thread.
    static CALLER: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Run `f` with `cancel` as the token host functions on this thread see.
/// The previous token, if any, is put back afterwards.
pub fn with_cancel<R>(cancel: CancellationToken, f: impl FnOnce() -> R) -> R {
    let prev = CANCEL.with(|c| c.replace(Some(cancel)));
    let out = f();
    CANCEL.with(|c| *c.borrow_mut() = prev);
    out
}

/// Run `f` as the tool named `name`, whose script is what host functions on
/// this thread are being called *by*. The previous name, if any, is put back
/// afterwards. Nested calls do not share one: a script that reaches another
/// tool goes back through the dispatcher, which runs that script on a thread of
/// its own with its own name.
pub fn with_tool<R>(name: &str, f: impl FnOnce() -> R) -> R {
    let prev = CALLER.with(|c| c.replace(Some(name.to_string())));
    let out = f();
    CALLER.with(|c| *c.borrow_mut() = prev);
    out
}

/// The current call's token, or a token nothing will ever cancel when no
/// call is running on this thread (a `manifest()` at load, a test).
fn current_cancel() -> CancellationToken {
    CANCEL.with(|c| c.borrow().clone()).unwrap_or_default()
}

/// The tool whose script is running on this thread, or empty when none is (a
/// `manifest()` at load, a test, a host function called by the harness itself).
fn current_tool() -> String {
    CALLER.with(|c| c.borrow().clone()).unwrap_or_default()
}

impl Host {
    pub fn new(cwd: PathBuf) -> Arc<Self> {
        Self::build(cwd, None, FetchPolicy::AnyAddress)
    }

    /// A host with a configured vault. The `mneme_*` primitives exist only
    /// on such a host, and the `mneme_*` built-ins are registered only then.
    pub fn with_mneme(cwd: PathBuf, mneme: Mneme) -> Arc<Self> {
        Self::build(cwd, Some(mneme), FetchPolicy::AnyAddress)
    }

    /// A host whose `fetch` tool will not connect to anything but a public
    /// address — the seam the sessions that nobody reads over are assembled
    /// with. See [`FetchPolicy`]; the two constructors above are this one with
    /// the policy eidolon's own surfaces keep.
    pub fn with_fetch_policy(cwd: PathBuf, fetch: FetchPolicy) -> Arc<Self> {
        Self::build(cwd, None, fetch)
    }

    /// The same, with a vault as well.
    pub fn with_mneme_and_fetch_policy(
        cwd: PathBuf,
        mneme: Mneme,
        fetch: FetchPolicy,
    ) -> Arc<Self> {
        Self::build(cwd, Some(mneme), fetch)
    }

    fn build(cwd: PathBuf, mneme: Option<Mneme>, fetch: FetchPolicy) -> Arc<Self> {
        Arc::new(Host {
            cwd,
            dispatcher: OnceLock::new(),
            mneme: mneme.map(Arc::new),
            fetch,
            swarm: OnceLock::new(),
            search_key: OnceLock::new(),
            store: OnceLock::new(),
            compiler: OnceLock::new(),
        })
    }

    /// Give this session a search key — the capability itself, since the
    /// `search` tool exists only on a host that has one. May land after the
    /// context has been built (the primitive reads it per call), but it must
    /// precede [`crate::register_builtins`], which is what decides whether the
    /// tool exists at all.
    pub fn attach_search_key(&self, key: impl Into<String>) {
        let _ = self.search_key.set(Zeroizing::new(key.into()));
    }

    /// Whether this session may search the web. Read by
    /// [`crate::register_builtins`], exactly as [`Host::has_swarm`] is.
    pub fn has_search(&self) -> bool {
        self.search_key.get().is_some()
    }

    /// The key [`Host::has_search`] says is there. Private on purpose: the one
    /// reader is the primitive that puts it in a header, and no accessor hands
    /// it to a caller, a script or a log.
    fn search_key(&self) -> Option<&str> {
        self.search_key.get().map(|key| key.as_str())
    }

    /// Give this session the custodied store, so a script's own endpoint can
    /// name a secret (`token_secret`) instead of carrying one. The value is
    /// read per call, here, and put in a header by the primitive — the same
    /// arrangement the search key has, with the difference that the *name* is
    /// the script's and the value never is. May land after the context has been
    /// built: the primitive reads it per call, so what it must precede is the
    /// call rather than the registration.
    pub fn attach_secrets(&self, store: Arc<harnox::secrets::SecretStore>) {
        let _ = self.store.set(store);
    }

    fn store(&self) -> Option<Arc<harnox::secrets::SecretStore>> {
        self.store.get().cloned()
    }

    /// What this host's `fetch` tool will connect to.
    pub fn fetch_policy(&self) -> FetchPolicy {
        self.fetch
    }

    pub fn has_mneme(&self) -> bool {
        self.mneme.is_some()
    }

    /// Link this session's registration, so scripts can see and reach the
    /// other sessions. May land after the context has been built; what it
    /// must precede is [`crate::register_builtins`], which asks
    /// [`Host::has_swarm`] whether the two swarm tools exist.
    pub fn attach_swarm(&self, presence: Arc<eidolon_swarm::Presence>) {
        let _ = self.swarm.set(presence);
    }

    pub fn has_swarm(&self) -> bool {
        self.swarm.get().is_some()
    }

    fn swarm(&self) -> Option<Arc<eidolon_swarm::Presence>> {
        self.swarm.get().cloned()
    }

    /// Link the dispatcher so scripts can route calls back through it.
    pub fn attach(&self, d: &Arc<Dispatcher>) {
        let _ = self.dispatcher.set(Arc::downgrade(d));
    }

    fn dispatcher(&self) -> Option<Arc<Dispatcher>> {
        self.dispatcher.get().and_then(Weak::upgrade)
    }

    /// Where the primitives run. The dispatcher owns the live value — it
    /// moves when the consumer adopts a session started somewhere else —
    /// and this is read per call, so a script never closes over a stale
    /// directory. The host's own `cwd` is the answer only before
    /// [`Host::attach`], which is to say in tests.
    fn cwd(&self) -> PathBuf {
        self.dispatcher()
            .map(|d| d.cwd())
            .unwrap_or_else(|| self.cwd.clone())
    }

    /// Route a script-originated call through the chokepoint.
    async fn dispatch(
        &self,
        name: &str,
        input: serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let Some(d) = self.dispatcher() else {
            return Err("no dispatcher attached; scripts cannot call tools yet".into());
        };
        let call = ToolCall {
            id: fresh_id(name),
            name: name.to_string(),
            input,
            origin: CallOrigin::Script,
        };
        let out = d.dispatch(call, cancel.child_token()).await;
        if out.is_error {
            Err(out.content)
        } else {
            Ok(out.content)
        }
    }

    /// The context and runtime every script on this host shares. Built on
    /// the first call and never again; see the module docs for why that
    /// is safe and what it saves.
    pub fn compiler(self: &Arc<Self>) -> anyhow::Result<&Compiler> {
        self.compiler
            .get_or_init(|| {
                let mut context =
                    Context::with_default_modules().map_err(|e| format!("rune context: {e}"))?;
                context
                    .install(
                        rune_modules::json::module(false)
                            .map_err(|e| format!("json module: {e}"))?,
                    )
                    .map_err(|e| format!("installing json: {e}"))?;
                context
                    .install(self.module().map_err(|e| format!("eidolon module: {e}"))?)
                    .map_err(|e| format!("installing eidolon: {e}"))?;
                let runtime = Arc::new(
                    context
                        .runtime()
                        .map_err(|e| format!("rune runtime: {e}"))?,
                );
                Ok(Compiler { context, runtime })
            })
            .as_ref()
            .map_err(|e| anyhow!("{e}"))
    }

    /// Build the `eidolon` module. Its closures hold a `Weak<Host>` — not
    /// an `Arc`, since the host owns the context the module is installed
    /// in — and read everything that can change (the working directory,
    /// the call's cancellation token) when they are invoked, never when
    /// they are built.
    pub fn module(self: &Arc<Self>) -> Result<Module, ContextError> {
        let mut m = Module::with_crate("eidolon")?;
        let weak = Arc::downgrade(self);
        // The host, or the one error a script sees if it somehow outlived it.
        fn live(weak: &Weak<Host>) -> Result<Arc<Host>, String> {
            weak.upgrade()
                .ok_or_else(|| "the script host is gone".to_string())
        }

        {
            let weak = weak.clone();
            m.function("cwd", move || -> Result<String, String> {
                Ok(live(&weak)?.cwd().display().to_string())
            })
            .build()?;
        }
        {
            let weak = weak.clone();
            m.function(
                "fs_read",
                move |path: String, offset: Option<i64>, limit: Option<i64>| {
                    let host = live(&weak);
                    async move {
                        let cwd = host?.cwd();
                        eidolon_tools::fs::read(
                            &cwd,
                            &path,
                            offset.map(|n| n.max(0) as usize),
                            limit.map(|n| n.max(0) as usize),
                        )
                        .map_err(|e| format!("{e:#}"))
                    }
                },
            )
            .build()?;
        }
        {
            let weak = weak.clone();
            m.function("fs_write", move |path: String, content: String| {
                let host = live(&weak);
                async move {
                    eidolon_tools::fs::write(&host?.cwd(), &path, &content)
                        .map_err(|e| format!("{e:#}"))
                }
            })
            .build()?;
        }
        {
            let weak = weak.clone();
            m.function(
                "fs_edit",
                move |path: String, old: String, new: String, replace_all: bool| {
                    let host = live(&weak);
                    async move {
                        eidolon_tools::fs::edit(&host?.cwd(), &path, &old, &new, replace_all)
                            .map_err(|e| format!("{e:#}"))
                    }
                },
            )
            .build()?;
        }
        {
            let weak = weak.clone();
            m.function("shell", move |command: String, timeout_s: Option<i64>| {
                let host = live(&weak);
                let cancel = current_cancel();
                async move {
                    let cwd = host?.cwd();
                    let timeout =
                        timeout_s.map(|s| std::time::Duration::from_secs(s.max(1) as u64));
                    eidolon_tools::shell::run(&cwd, &command, timeout, &cancel)
                        .await
                        .map(|e| e.render())
                        .map_err(|e| format!("{e:#}"))
                }
            })
            .build()?;
        }
        // The no-shell twin: one program and its argv. A tool whose arguments
        // come from a model wants this — there is no command line for a `;` or
        // a `$(…)` in an argument to be read as syntax, because none was ever
        // parsed. Same process group, timeout, cancellation and rendering.
        {
            let weak = weak.clone();
            m.function(
                "exec",
                move |program: String, args: Value, timeout_s: Option<i64>| {
                    let host = live(&weak);
                    let cancel = current_cancel();
                    async move {
                        let cwd = host?.cwd();
                        let args = match rune_to_json(&args).map_err(|e| format!("{e:#}"))? {
                            serde_json::Value::Array(items) => items
                                .into_iter()
                                .map(|item| match item {
                                    serde_json::Value::String(s) => Ok(s),
                                    other => {
                                        Err(format!("an argument must be a string, not {other}"))
                                    }
                                })
                                .collect::<Result<Vec<String>, String>>()?,
                            serde_json::Value::Null => Vec::new(),
                            _ => return Err("exec takes a list of arguments".to_string()),
                        };
                        let timeout =
                            timeout_s.map(|s| std::time::Duration::from_secs(s.max(1) as u64));
                        eidolon_tools::shell::exec(&cwd, &program, &args, timeout, &cancel)
                            .await
                            .map(|e| e.render())
                            .map_err(|e| format!("{e:#}"))
                    }
                },
            )
            .build()?;
        }
        {
            let weak = weak.clone();
            m.function("shell_background", move |command: String| {
                let host = live(&weak);
                async move {
                    // Nothing to await — the start answers the moment the
                    // child is spawned; the async wrapper is for the
                    // script's `.await`, like `shell`'s.
                    let cwd = host?.cwd();
                    eidolon_tools::shell::run_background(&cwd, &command)
                        .map(|b| b.render())
                        .map_err(|e| format!("{e:#}"))
                }
            })
            .build()?;
        }
        {
            let weak = weak.clone();
            m.function(
                "grep",
                move |pattern: String,
                      path: Option<String>,
                      glob: Option<String>,
                      case_insensitive: bool,
                      max: Option<i64>| {
                    let host = live(&weak);
                    async move {
                        let cwd = host?.cwd();
                        let opts = eidolon_tools::search::GrepOptions {
                            pattern: &pattern,
                            path: path.as_deref(),
                            glob: glob.as_deref(),
                            case_insensitive,
                            max: max.map(|n| n.max(1) as usize),
                        };
                        eidolon_tools::search::grep(&cwd, opts)
                            .map(|(hits, truncated)| {
                                eidolon_tools::search::render(&hits, truncated)
                            })
                            .map_err(|e| format!("{e:#}"))
                    }
                },
            )
            .build()?;
        }
        {
            let weak = weak.clone();
            m.function(
                "web_fetch",
                move |url: String, max: Option<i64>, timeout_s: Option<i64>, raw: bool| {
                    // The host is needed for its fetch policy — the address
                    // rule this session was assembled with — and the upgrade
                    // is kept besides, so a script outliving its host is told
                    // so rather than silently reaching the network anyway.
                    let host = live(&weak);
                    let cancel = current_cancel();
                    async move {
                        let policy = host?.fetch_policy();
                        let timeout =
                            timeout_s.map(|s| std::time::Duration::from_secs(s.max(1) as u64));
                        eidolon_tools::web::fetch(
                            &url,
                            policy,
                            max.map(|n| n.max(1) as usize),
                            timeout,
                            raw,
                            &cancel,
                        )
                        .await
                        .map(|p| p.render())
                        .map_err(|e| format!("{e:#}"))
                    }
                },
            )
            .build()?;
        }
        // The search primitive. Registered unconditionally and answered at
        // call time — like the swarm's — because whether the *tool* exists is
        // decided by `register_builtins` from `Host::has_search` on the main
        // thread, after the context may already have been warmed on another.
        {
            let weak = weak.clone();
            m.function("web_search", move |query: String, count: Option<i64>| {
                let host = live(&weak);
                let cancel = current_cancel();
                async move {
                    let host = host?;
                    // The session's key, read here and nowhere else: it goes
                    // into a header inside the call below and is never a value
                    // a script, a tool result or a log can see.
                    let key = host
                        .search_key()
                        .ok_or("this session has no search key")?;
                    eidolon_tools::web::search(
                        &query,
                        count.map(|n| n.max(1) as usize),
                        // No endpoint from a script, and not the session's
                        // fetch policy either: the caller cannot name an
                        // address here, and this is the one request in the
                        // harness that carries a credential.
                        None,
                        key,
                        FetchPolicy::PublicOnly,
                        &cancel,
                    )
                    .await
                    .map(|hits| eidolon_tools::web::render(&hits))
                    .map_err(|e| format!("{e:#}"))
                }
            })
            .build()?;
        }
        // The service primitive: one request to an endpoint the *script*
        // carries, with the credential it names put in a header here.
        // Registered unconditionally and answered at call time, like the two
        // above: a session with no store should say so by name rather than
        // pretend the primitive is not there.
        {
            let weak = weak.clone();
            m.function(
                "api_request",
                move |endpoint: Value, method: String, path: String, body: Option<String>| {
                    let host = live(&weak);
                    let cancel = current_cancel();
                    let consumer = current_tool();
                    async move {
                        let host = host?;
                        let json = rune_to_json(&endpoint).map_err(|e| format!("{e:#}"))?;
                        let def =
                            crate::endpoint::descriptor(&json).map_err(|e| format!("{e:#}"))?;
                        let token =
                            crate::endpoint::resolve(&def, host.store().as_deref(), &consumer)
                                .map_err(|e| format!("{e:#}"))?;
                        let response = eidolon_tools::api::request(
                            &def,
                            token.as_ref().map(|t| t.as_str()),
                            &eidolon_tools::api::Call {
                                method: &method,
                                path: &path,
                                body: body.as_deref(),
                                policy: host.fetch_policy(),
                            },
                            &cancel,
                        )
                        .await
                        .map_err(|e| format!("{e:#}"))?;
                        serde_json::to_string(&response).map_err(|e| e.to_string())
                    }
                },
            )
            .build()?;
        }
        // The vault, as two rungs. `mneme_call` is the raw primitive the
        // `mneme_rpc` built-in wraps (the dispatcher has already approved by
        // then), and it keeps both of Mneme's call kinds: the `dispatch` one
        // has had no tool over it since `mneme_dispatch` was cut
        // (2026-09-14 — the vault command channel's inline seam replaced it),
        // so it is a script's door only. `mneme_rpc` is what *other* scripts
        // call, and it routes back through the dispatcher like `ask_user`
        // does.
        if let Some(mneme) = self.mneme.clone() {
            m.function("mneme_call", move |kind: String, args: Value| {
                let mneme = mneme.clone();
                async move {
                    let args = serde_json::to_value(&args).map_err(|e| e.to_string())?;
                    match kind.as_str() {
                        "rpc" => {
                            let function = args
                                .get("function")
                                .and_then(|v| v.as_str())
                                .ok_or("rpc needs a `function`")?
                                .to_string();
                            let fargs = args.get("args").cloned().unwrap_or(serde_json::json!({}));
                            mneme
                                .rpc(&function, fargs)
                                .await
                                .map_err(|e| format!("{e:#}"))
                        }
                        "dispatch" => {
                            let utterance = args
                                .get("utterance")
                                .and_then(|v| v.as_str())
                                .ok_or("dispatch needs an `utterance`")?
                                .to_string();
                            let bindings = args.get("bindings").cloned().filter(|b| !b.is_null());
                            mneme
                                .dispatch(&utterance, bindings)
                                .await
                                .map_err(|e| format!("{e:#}"))
                        }
                        other => Err(format!("unknown mneme call kind `{other}`")),
                    }
                }
            })
            .build()?;
            {
                let weak = weak.clone();
                m.function("mneme_rpc", move |function: String, args: Value| {
                    let host = live(&weak);
                    let cancel = current_cancel();
                    async move {
                        let args = serde_json::to_value(&args).map_err(|e| e.to_string())?;
                        host?
                            .dispatch(
                                "mneme_rpc",
                                serde_json::json!({ "function": function, "args": args }),
                                &cancel,
                            )
                            .await
                    }
                })
                .build()?;
            }
        }
        // The swarm, on the same two-rung shape as the vault above.
        // `swarm_call` is the raw primitive the two built-in scripts wrap,
        // reached only after the dispatcher has said yes; `peers` and
        // `peer_send` are what *other* scripts call, and route back
        // through the chokepoint so a skill messaging a neighbour is a
        // tool call like any other.
        //
        // Unlike the vault's, these are registered **unconditionally** and
        // answer at call time. The vault can gate on a field because
        // `mneme` is a constructor argument and cannot arrive late; a
        // registration can, and the context is warmed on a background
        // thread the moment the host exists — so a module built from
        // `self.swarm` races that thread and loses about half the time,
        // leaving the built-ins to fail compilation against a primitive
        // that was there a millisecond later. What *does* gate on the
        // registration is whether the two tools exist at all, which is
        // decided on the main thread after it (`register_builtins`).
        {
            let weak = weak.clone();
            m.function("swarm_call", move |kind: String, args: Value| {
                let host = live(&weak);
                async move {
                    let presence = host?
                        .swarm()
                        .ok_or("this session is not registered among its peers")?;
                    let args = serde_json::to_value(&args).map_err(|e| e.to_string())?;
                    match kind.as_str() {
                        "peers" => Ok(eidolon_swarm::roster(&presence)),
                        "send" => {
                            let to = args
                                .get("to")
                                .and_then(|v| v.as_str())
                                .ok_or("send needs a `to`")?;
                            let text = args
                                .get("text")
                                .and_then(|v| v.as_str())
                                .ok_or("send needs a `text`")?;
                            // Absent, not false: `eidolon_swarm::send`
                            // decides the default from who is addressed
                            // — a direct message wakes, a fan-out does
                            // not — and a `false` here would erase that.
                            let wake = args.get("wake").and_then(|v| v.as_bool());
                            eidolon_swarm::send(&presence, to, text, wake)
                                .map_err(|e| format!("{e:#}"))
                        }
                        other => Err(format!("unknown swarm call kind `{other}`")),
                    }
                }
            })
            .build()?;
        }
        {
            let weak = weak.clone();
            m.function("peers", move || {
                let host = live(&weak);
                let cancel = current_cancel();
                async move {
                    host?
                        .dispatch("peers", serde_json::json!({}), &cancel)
                        .await
                }
            })
            .build()?;
        }
        {
            let weak = weak.clone();
            m.function("peer_send", move |to: String, text: String, wake: bool| {
                let host = live(&weak);
                let cancel = current_cancel();
                async move {
                    host?
                        .dispatch(
                            "send",
                            serde_json::json!({ "to": to, "text": text, "wake": wake }),
                            &cancel,
                        )
                        .await
                }
            })
            .build()?;
        }
        {
            let weak = weak.clone();
            m.function("ask_user", move |prompt: String| {
                let host = live(&weak);
                let cancel = current_cancel();
                async move {
                    host?
                        .dispatch("ask_user", serde_json::json!({ "prompt": prompt }), &cancel)
                        .await
                }
            })
            .build()?;
        }
        {
            let weak = weak.clone();
            m.function("choices_user", move |prompt: String, options: Value| {
                let host = live(&weak);
                let cancel = current_cancel();
                async move {
                    let options = serde_json::to_value(&options).map_err(|e| e.to_string())?;
                    host?
                        .dispatch(
                            "choices_user",
                            serde_json::json!({ "prompt": prompt, "options": options }),
                            &cancel,
                        )
                        .await
                }
            })
            .build()?;
        }
        Ok(m)
    }
}

fn fresh_id(name: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("script_{name}_{t}_{n}")
}
