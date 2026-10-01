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
use eidolon_core::event::Event;
use eidolon_core::message::Message;
use eidolon_core::session::RecordKind;
use eidolon_core::tool::{CallOrigin, ToolCall};
use eidolon_remote::Mneme;
use eidolon_tools::web::FetchPolicy;
use harnox::llm::message::ContentBlock;
use harnox::llm::provider::CacheOptions;
use harnox::llm::RetryPolicy;
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
    /// The permission store — what the operator has granted, vouches and
    /// grants alike — read per call by the endpoint's grant check, never at
    /// module-build time. Attached after construction like the rest.
    permits: OnceLock<Arc<crate::permits::Permits>>,
    /// The session's yolo switch — the same shared atomic the gate's `Yolo`
    /// decorator reads, attached here so the credential ask-once can see the
    /// posture too. Attached after construction like the rest and read per
    /// call: `:yolo` thrown mid-session is in force on the next ask.
    yolo: OnceLock<eidolon_core::yolo::Switch>,
    /// The one-shot model call's path: the catalog a completion resolves,
    /// streams and prices through, and the model key this session opened
    /// with. Attached after construction like the swarm — the host exists
    /// before the catalog does (the context warms on another thread while
    /// the catalog loads) — and read per call, so a module built from this
    /// field can never race that warm-up. `None` is a host assembled
    /// without a model reach: tests, and consumers that never resolve a
    /// backend. See [`Completion`].
    completion: OnceLock<Arc<Completion>>,
    /// The one context every script on this host compiles against, and
    /// the runtime they all execute on. Built on first use; the error, if
    /// the standard modules refuse to install, is kept so every caller
    /// sees the same one rather than retrying.
    compiler: OnceLock<Result<Compiler, String>>,
    /// The workflow scope's compiler: the same shape as `compiler`, with a
    /// module that registers the step door and nothing else — a workflow
    /// that reaches for `fs_write` or `shell` fails to *compile*, which is
    /// how the narrow scope refuses what it does not list. Built on the
    /// first workflow compile, never at startup: nothing pays for it until
    /// a plugin ships a workflow.
    wf: OnceLock<Result<Compiler, String>>,
}

/// A compile-time [`Context`] and the [`RuntimeContext`] made from it —
/// the two halves of "a Rune environment", kept together because a unit
/// compiled against one context must run on that context's runtime.
pub struct Compiler {
    pub context: Context,
    pub runtime: Arc<RuntimeContext>,
}

/// What a one-shot model call needs from the session it rides in: the
/// catalog to resolve and bill through, the model this session opened
/// with — the answer to a call that names no `model` — and the output cap
/// the operator configured, which governs this call as it governs an
/// agent turn. The catalog is the providers' front door and nothing
/// narrower, because a completion needs exactly what it holds:
/// resolution, the provider, the rates.
pub struct Completion {
    pub catalog: Arc<eidolon_providers::Catalog>,
    pub default_model: String,
    pub max_tokens: u32,
}

thread_local! {
    /// The cancellation token of the tool call running on this thread, if
    /// one is. Read by the host functions, set by [`with_cancel`].
    static CANCEL: RefCell<Option<CancellationToken>> = const { RefCell::new(None) };
    /// The chain of script tools enclosing whatever is running on this
    /// thread, outermost first: the tool whose script it is, and every
    /// dispatched tool whose script led to it. Read by the primitives that
    /// inject a credential — the last entry names the tool that asked for
    /// the key rather than the harness, and carries its plugin, whose vouch
    /// the audit line writes — and by [`Host::dispatch`], which refuses a
    /// call that would nest deeper than [`MAX_SCRIPT_DISPATCH_DEPTH`]. Set
    /// by [`with_tool`], which inherits the spawning thread's chain: the
    /// one fact a fresh script thread cannot derive, since the thread it
    /// runs on is new. A thread-local for the same reason the token is one:
    /// nothing in the module may close over anything that belongs to a
    /// single call, and a call is the whole of its thread.
    static CALLER: RefCell<Option<Arc<[ChainEntry]>>> = const { RefCell::new(None) };
    /// The dispatch id of the tool call running on this thread, if one
    /// is. Read by the model-call primitive, which journals the call's
    /// spend and pairs it with the `ToolResult` through this id — a
    /// workflow step's deterministic `w<run>-<index>`, a model call's
    /// `tool_use` id, an operator's minted one. Set by [`with_call_id`]
    /// beside [`with_tool`]: a thread-local for the same reason the
    /// others are, because nothing in the module may close over anything
    /// that belongs to a single call.
    static CALL_ID: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// One link of a script's dispatch chain: the tool whose script ran at that
/// level, and the plugin that contributed it, when one did. The plugin rides
/// the entry rather than a second thread-local so a nested call is
/// attributed at its own level — a user tool dispatched from a plugin's
/// script is the user's effect, and the plugin's own script beneath it is
/// the plugin's again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChainEntry {
    pub(crate) name: String,
    pub(crate) plugin: Option<String>,
}

/// Run `f` with `id` as the dispatch id host functions on this thread
/// see. The previous id, if any, is put back afterwards. Set beside
/// [`with_tool`] by the script runner, which is the one place the
/// dispatcher's call id and the thread running the script meet.
pub fn with_call_id<R>(id: &str, f: impl FnOnce() -> R) -> R {
    let prev = CALL_ID.with(|c| c.replace(Some(id.to_string())));
    let out = f();
    CALL_ID.with(|c| *c.borrow_mut() = prev);
    out
}

/// The current call's dispatch id, or empty when no call is running on
/// this thread (a `manifest()` at load, a test).
fn current_call_id() -> String {
    CALL_ID.with(|c| c.borrow().clone()).unwrap_or_default()
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
/// this thread are being called *by*, with `chain` the dispatched scripts
/// that led to it. `plugin` is the tool's own tag, when a plugin contributed
/// it — the fact the audit lines read to say whose effect this thread's
/// calls cause. The thread's chain becomes `chain` plus the entry; the
/// previous one, if any, is put back afterwards. Nested calls do not share
/// one: a script that reaches another tool goes back through the dispatcher,
/// which runs that script on a thread of its own with its own chain.
pub(crate) fn with_tool<R>(
    name: &str,
    plugin: Option<&str>,
    chain: Arc<[ChainEntry]>,
    f: impl FnOnce() -> R,
) -> R {    let entry = ChainEntry {
        name: name.to_string(),
        plugin: plugin.map(str::to_string),
    };
    let mut full = Vec::with_capacity(chain.len() + 1);
    full.extend(chain.iter().cloned());
    full.push(entry);
    let prev = CALLER.with(|c| c.replace(Some(Arc::from(full))));
    let out = f();
    CALLER.with(|c| *c.borrow_mut() = prev);
    out
}

/// The current call's token, or a token nothing will ever cancel when no
/// call is running on this thread (a `manifest()` at load, a test).
pub(crate) fn current_cancel() -> CancellationToken {
    CANCEL.with(|c| c.borrow().clone()).unwrap_or_default()
}

/// Whose effect the host functions on this thread cause: the plugin that
/// contributed the tool whose script is running, as `plugin:<name>` — the
/// vouch the `api_request` audit line writes and spend attribution reads —
/// or the tool's own name when no plugin did. Empty when no script is
/// running on this thread (a `manifest()` at load, a test, a host function
/// called by the harness itself).
/// Whose script is running, as the chain's last entry carries it: the
/// tool's registered name and the plugin that contributed it, when one
/// did — the two halves the grant rows and the audit lines spell.
fn current_caller() -> Option<(String, Option<String>)> {
    CALLER.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|chain| chain.last())
            .map(|e| (e.name.clone(), e.plugin.clone()))
    })
}

fn current_consumer() -> String {
    CALLER.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|ch| ch.last())
            .map(|e| match &e.plugin {
                Some(p) => format!("plugin:{p}"),
                None => e.name.clone(),
            })
            .unwrap_or_default()
    })
}

/// The dispatch chain enclosing this thread's script, empty when none is (a
/// `manifest()` at load, a test, a host function called by the harness
/// itself). Read when a script tool is called, on the thread the dispatcher
/// is polling — which for a script's own nested dispatch is the script's
/// thread, so the fresh thread the call spawns inherits the chain and the
/// depth limit counts the true nesting.
pub(crate) fn current_chain() -> Arc<[ChainEntry]> {
    CALLER.with(|c| c.borrow().clone()).unwrap_or_default()
}

/// How deep script dispatch may nest: the script's own tool is the first
/// chain entry, each dispatch that reached a script adds one, and a dispatch
/// from a script at this depth is refused. Same-plugin workflow nesting
/// (vault, *The Plugin System (design)*, §Workflows) shares this guard —
/// "one mechanism, one guard" — and the number is far above anything a real
/// workflow does (a plugin tool reaching the vault is two) and far below
/// anything that could overflow the stack of rune threads the chain implies.
pub const MAX_SCRIPT_DISPATCH_DEPTH: usize = 8;

/// What a vault rung answers on a session assembled without one: the
/// absent capability, by name, at the call. Both rungs are registered
/// unconditionally so a user script that wraps them compiles everywhere —
/// a box that is simply not wired to a vault is a configuration, not a
/// broken build. An *unknown* primitive remains a compile error, because
/// that is drift and must stay loud.
const NO_VAULT: &str = "no vault is configured for this harness";

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
            permits: OnceLock::new(),
            yolo: OnceLock::new(),
            completion: OnceLock::new(),
            compiler: OnceLock::new(),
            wf: OnceLock::new(),
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

    /// Give this session the permission store, so an endpoint's credential
    /// check and the policy stage read the same rows. Attached after
    /// construction like the rest; read per call, so a vouch or a grant
    /// written mid-session is in force on the next call.
    pub fn attach_permits(&self, permits: Arc<crate::permits::Permits>) {
        let _ = self.permits.set(permits);
    }

    /// The permission store, when the consumer attached one. The one
    /// reader is the primitive that resolves a credential, and what
    /// travels is the fact of a grant, never a value.
    pub fn permits(&self) -> Option<Arc<crate::permits::Permits>> {
        self.permits.get().cloned()
    }

    /// Give this session the yolo switch, so the grant ask-once sees what
    /// the gate's hook sees. `:yolo`'s contract is that the operator answers
    /// nothing, and a credential the endpoint has no grant for is a question
    /// — a switch the host could not see would be one that asked anyway,
    /// which is the bug this exists not to be. Disarmed it changes nothing.
    pub fn attach_yolo(&self, switch: eidolon_core::yolo::Switch) {
        let _ = self.yolo.set(switch);
    }

    /// The switch's present answer, or no when no switch was attached —
    /// a host assembled without one (a test, a no-gate session) asks.
    fn yolo_armed(&self) -> bool {
        self.yolo.get().is_some_and(|s| s.armed())
    }

    /// Give this session the completion path, so a script can make one
    /// tool-less model call. The catalog is finished — providers loaded,
    /// keypool claimed — before it is handed over, and the model key is
    /// the session's resolved launch model, which is what a call without
    /// a `model` of its own answers on. May land after the context has
    /// been built (the primitive reads it per call); what it must precede
    /// is [`crate::register_builtins`], which asks [`Host::has_completion`]
    /// whether the `ask_model` tool exists at all.
    pub fn attach_completion(&self, completion: Completion) {
        let _ = self.completion.set(Arc::new(completion));
    }

    /// Whether this session can make a model call. Read by
    /// [`crate::register_builtins`], exactly as [`Host::has_search`] is.
    pub fn has_completion(&self) -> bool {
        self.completion.get().is_some()
    }

    /// The completion path, when the consumer attached one: the catalog a
    /// run's budget prices its spend at, and the model face `ask_model`
    /// rides. Crate-visible because the interpreter's budget check reads
    /// the catalog the session was assembled with — the rates are the
    /// session's own, not a second source.
    pub(crate) fn completion(&self) -> Option<Arc<Completion>> {
        self.completion.get().cloned()
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

    pub(crate) fn dispatcher(&self) -> Option<Arc<Dispatcher>> {
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

    /// Route a script-originated call through the chokepoint, minting the
    /// call id as it goes.
    async fn dispatch(
        &self,
        name: &str,
        input: serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        self.dispatch_as(name, input, None, cancel).await
    }

    /// The same, with the caller free to name the call.
    ///
    /// A workflow step journals its request under the run's own
    /// deterministic id (`w<run>-<index>`) *before* it dispatches, and a
    /// resumed run dispatches under the id its journal already holds — so
    /// the chokepoint's replay check finds the answer there instead of
    /// firing the side effect again. A minted id would make every resume a
    /// first visit, which is the one thing the log exists to prevent. The
    /// flip side is that naming an id the branch already holds an answer
    /// for *is* the memo: the call does not run. That is wanted from the
    /// workflow scope and a hazard from the tool scope, which is why the
    /// two behaviors are two functions — `tool_call` mints and stays the
    /// ordinary verb; `tool_call_as` is for the caller whose journal is
    /// already written. The caller owns the id whole: what the branch holds
    /// under it is what this returns.
    ///
    /// The depth check comes first because it needs nothing but this
    /// thread's chain — the dispatches whose scripts enclose the script now
    /// calling — and it refuses by *naming the chain* rather than by
    /// letting the nesting overflow: every level is a rune thread and a
    /// dispatcher pass, so the failure has to be an answer, here, before
    /// another level exists.
    pub(crate) async fn dispatch_as(
        &self,
        name: &str,
        input: serde_json::Value,
        id: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let chain = current_chain();
        if chain.len() > MAX_SCRIPT_DISPATCH_DEPTH {
            let names: Vec<&str> = chain.iter().map(|e| e.name.as_str()).collect();
            return Err(format!(
                "script dispatch nested deeper than {MAX_SCRIPT_DISPATCH_DEPTH}: {} → {name}",
                names.join(" → "),
            ));
        }
        let named = match id {
            Some(id) if !id.is_empty() => id.to_string(),
            Some(_) => return Err("tool_call_as needs a non-empty call id".into()),
            None => fresh_id(name),
        };
        let Some(d) = self.dispatcher() else {
            return Err("no dispatcher attached; scripts cannot call tools yet".into());
        };
        let call = ToolCall {
            id: named,
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

    /// One tool-less completion, answering Melete's envelope verbatim:
    /// always a plain `String` carrying `{"ok":true,"reply":…}` or
    /// `{"ok":false,"error":…}` — never sometimes-a-map, and never a
    /// `Result::Err` the script has to catch, because a native-function
    /// argument-type mismatch in Rune is an uncatchable panic and one
    /// shape is the shape a script can parse. Every failure the path can
    /// produce — no handle attached, an unresolvable model, a refused
    /// request, an answer with no text — is the envelope's `ok:false` arm.
    ///
    /// `input` is the call object as the script handed it over:
    /// `{ prompt, model?, history? }`. `history` is honoured as given —
    /// `user`/`assistant` turns of plain text, in order, unmerged —
    /// because building a thread out of remembered steps is the
    /// interpreter's business, not this call's. The model defaults to the
    /// session's launch key and the output cap to the configured one;
    /// both are decisions made where the session is assembled, not here.
    ///
    /// With no completion handle attached — a host assembled without a
    /// model reach — the call says so, loudly, in the envelope: the one
    /// thing it must never do is answer as if nothing were wrong.
    async fn model_call(&self, input: Value, cancel: &CancellationToken) -> String {
        let Some(completion) = self.completion() else {
            return envelope_err(
                "this session has no completion path attached; `ask_model` is unavailable here",
            );
        };
        let json = match rune_to_json(&input) {
            Ok(json) => json,
            Err(e) => return envelope_err(&format!("{e:#}")),
        };
        if !json.is_object() {
            return envelope_err("the model call takes an object: { prompt, model?, history? }");
        }
        let Some(prompt) = json
            .get("prompt")
            .map(|p| p.as_str().map(str::to_string))
            .unwrap_or(None)
            .filter(|p| !p.trim().is_empty())
        else {
            return envelope_err("`prompt` must be a non-empty string");
        };
        let model = match json.get("model").filter(|m| !m.is_null()) {
            None => completion.default_model.clone(),
            Some(m) => match m.as_str() {
                Some(s) if !s.trim().is_empty() => s.to_string(),
                _ => return envelope_err("`model` must be a non-empty string when given"),
            },
        };
        let mut messages = match history_from(&json) {
            Ok(history) => history,
            Err(reason) => return envelope_err(&reason),
        };
        messages.push(Message::user_text(prompt));
        let req = eidolon_providers::CompletionRequest {
            model,
            provider: None,
            system: None,
            messages,
            max_tokens: completion.max_tokens,
            thinking: None,
            temperature: None,
            cache: CacheOptions::off(),
            retries: RetryPolicy::default(),
        };
        match completion.catalog.complete(req, cancel.clone()).await {
            Ok(res) => {
                // The call measured something, so it cost something:
                // journal the spend while the key and the usage are in
                // hand — the envelope the tool returns carries neither —
                // and publish it, so a live consumer's total moves with
                // the log's. Paired by the dispatch id this call ran
                // under; the one arm that can reach here without one (a
                // `manifest()` at load) mints a throwaway rather than
                // pairing the spend with somebody else's call. A failed
                // append is an error line, never a lost turn: the reply
                // is already real.
                let call_id = match current_call_id() {
                    id if !id.is_empty() => id,
                    _ => fresh_id("model_call"),
                };
                self.journal_model_spend(&res.key, &call_id, res.usage)
                    .await;
                match res.text() {
                    Ok(reply) => envelope_ok(&reply),
                    Err(e) => envelope_err(&format!("{e:#}")),
                }
            }
            Err(e) => envelope_err(&format!("{e:#}")),
        }
    }

    /// Write one call's spend to the log and say it on the bus. Best
    /// effort on the journal the way a verdict is: a log that will not
    /// take the record is named on the error channel, and the reply the
    /// caller is owed still goes out.
    async fn journal_model_spend(
        &self,
        key: &str,
        call_id: &str,
        usage: eidolon_core::message::Usage,
    ) {
        let Some(d) = self.dispatcher() else {
            tracing::warn!("a model call's spend could not be journaled: no dispatcher attached");
            return;
        };
        {
            let mut session = d.session().lock().await;
            if let Err(e) = session.append(RecordKind::ModelSpend {
                key: key.to_string(),
                call_id: call_id.to_string(),
                usage,
            }) {
                tracing::error!(error = %e, "failed to journal a model call's spend");
                drop(session);
                d.bus().publish(Event::Error(format!(
                    "failed to journal a model call's spend: {e}"
                )));
                return;
            }
        }
        d.bus().publish(Event::ModelSpend {
            key: key.to_string(),
            call_id: call_id.to_string(),
            usage,
        });
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
                // The permits file's accessor calls the one shared
                // grant-source comparison; every context that compiles the
                // file — this one and the micro-parse — installs it.
                context
                    .install(crate::permits::permits_module())
                    .map_err(|e| format!("installing the permits comparison: {e}"))?;
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

    /// The workflow scope's compiler: the host's one context is the *tool*
    /// scope — every primitive a tool script may name — and this is the
    /// other one. It installs `json` and the workflow module's three names
    /// — the step door, the park, and the model face — and nothing else, so
    /// a workflow that reaches past its scope does not compile. Built on
    /// the first workflow compile and never again; nothing at startup pays
    /// for a scope no session may use.
    pub(crate) fn workflow_compiler(self: &Arc<Self>) -> anyhow::Result<&Compiler> {
        self.wf
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
                    .install(
                        crate::workflow::workflow_module(self)
                            .map_err(|e| format!("eidolon module: {e}"))?,
                    )
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
                      limit: Option<i64>| {
                    let host = live(&weak);
                    async move {
                        let cwd = host?.cwd();
                        let opts = eidolon_tools::search::GrepOptions {
                            pattern: &pattern,
                            path: path.as_deref(),
                            glob: glob.as_deref(),
                            case_insensitive,
                            max: limit.map(|n| n.max(1) as usize),
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
                move |url: String, limit: Option<i64>, timeout_s: Option<i64>, raw: bool| {
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
                            limit.map(|n| n.max(1) as usize),
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
            m.function("web_search", move |query: String, limit: Option<i64>| {
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
                        limit.map(|n| n.max(1) as usize),
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
                    let consumer = current_consumer();
                    let caller = current_caller();
                    async move {
                        let host = host?;
                        let json = rune_to_json(&endpoint).map_err(|e| format!("{e:#}"))?;
                        let def =
                            crate::endpoint::descriptor(&json).map_err(|e| format!("{e:#}"))?;
                        let (tool, plugin) = caller
                            .clone()
                            .map(|c| (c.0, c.1))
                            .unwrap_or((consumer.clone(), None));
                        // The grant check, then — on a source no row covers —
                        // the ask-once. The answer is written as a grant row
                        // through the same door `eidolon plugins grant` uses,
                        // and a failed write is reported and refused rather
                        // than asked about twice. Nobody to ask (headless,
                        // NoUser, no session) is a refusal: fail closed,
                        // unattended.
                        let mut ask_pending = None;
                        let token = match crate::endpoint::resolve(
                            &def,
                            host.store().as_deref(),
                            &tool,
                            plugin.as_deref(),
                            host.permits().as_deref(),
                        ) {
                            Ok(t) => t,
                            Err(e)
                                if e.downcast_ref::<crate::endpoint::GrantRequired>()
                                    .is_some() =>
                            {
                                let need = e
                                    .downcast_ref::<crate::endpoint::GrantRequired>()
                                    .unwrap()
                                    .clone();
                                let permits = host.permits().ok_or_else(|| {
                                    format!("{need}; this session has no permission store to grant from")
                                })?;
                                // An armed yolo switch is the operator's
                                // answer given in advance: it says yes here
                                // the way the gate's hook does, without a
                                // dialog. The row it writes says `yolo`, not
                                // the operator's name — a waived yes is not
                                // a person's, and the permits file is the
                                // ledger that must be able to tell them
                                // apart.
                                let waived = host.yolo_armed();
                                if !waived {
                                    let dispatcher = host.dispatcher().ok_or_else(|| {
                                        format!("{need}; nobody is here to grant it")
                                    })?;
                                    let prompt = format!(
                                        "{} wants a credential from {}, which no grant covers. Grant it? The grant is written to policy.permits.rn and stands until revoked.",
                                        consumer, need.source
                                    );
                                    let yes = dispatcher
                                        .user()
                                        .confirm(&prompt, &cancel)
                                        .await;
                                    // The grant's yes is a dialogs' one; the
                                    // words said beside it have nothing here
                                    // to ride — the row below is the record.
                                    if !yes.given {
                                        return Err(format!("{need}; the grant was not given"));
                                    }
                                }
                                ask_pending = Some((permits, need, waived));
                                None
                            }
                            Err(e) => return Err(format!("{e:#}")),
                        };
                        let token = match (token, ask_pending) {
                            (Some(t), _) => Some(t),
                            (None, Some((permits, need, waived))) => {
                                let mut rows = permits.rows();
                                rows.grant(crate::permits::GrantRow {
                                    tool: Some(need.tool.clone()),
                                    source: need.source.clone(),
                                    wide_plugin: None,
                                    granted_by: if waived {
                                        "yolo".into()
                                    } else {
                                        crate::permits::granted_by()
                                    },
                                    granted_at: crate::permits::granted_at(),
                                });
                                if let Err(e) = permits.write(&rows) {
                                    return Err(format!(
                                        "{need}; the grant could not be written ({e:#}), so the call refuses rather than asking twice"
                                    ));
                                }
                                crate::endpoint::resolve(
                                    &def,
                                    host.store().as_deref(),
                                    &tool,
                                    plugin.as_deref(),
                                    host.permits().as_deref(),
                                )
                                .map_err(|e| format!("{e:#}"))?
                            }
                            // No ask, no token: an endpoint that wants no
                            // credential, and the request goes keyless.
                            (None, None) => None,
                        };
                        // The ledger's sight of a wrapper's request, at
                        // the attempt: a failed request still left the
                        // machine, so the record goes down before the
                        // call, and a log that will not take it stops
                        // the call — an egress nothing counted is the
                        // invisibility this exists to end. No dispatcher
                        // is no session to tell (a test host), and no
                        // ledger misses what it cannot hold.
                        if let Some(d) = host.dispatcher() {
                            d.session()
                                .lock()
                                .await
                                .append(RecordKind::ApiRequest {
                                    consumer: consumer.clone(),
                                    service: crate::endpoint::service(&def),
                                })
                                .map_err(|e| format!("journaling the api_request: {e:#}"))?;
                            d.bus().publish(Event::ApiRequest {
                                consumer: consumer.clone(),
                                service: crate::endpoint::service(&def),
                            });
                        }
                        let response = eidolon_tools::api::request(
                            &def,
                            token.as_ref().map(|inj| inj.token.as_str()),
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
        // The model call. Registered unconditionally and answered at call
        // time, like the search and service primitives above — the host
        // exists before the catalog does, so a module built from the field
        // would race the context warm-up and lose about half the time.
        // What gates on the handle is whether the `ask_model` *tool*
        // exists, decided on the main thread by `register_builtins`. The
        // answer is Melete's envelope, always — see [`Host::model_call`].
        {
            let weak = weak.clone();
            m.function("model_call", move |input: Value| {
                let host = live(&weak);
                let cancel = current_cancel();
                async move {
                    let host = host?;
                    Ok::<String, String>(host.model_call(input, &cancel).await)
                }
            })
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
        // Both are registered **unconditionally** and answer at call time,
        // like the search and swarm rungs: a box that is simply not wired
        // to a vault is a configuration, not a broken build, and a user
        // script wrapping these names has to compile there — the absent
        // capability says so by name when somebody actually calls it. What
        // still gates on the vault is whether the built-in *tool* exists,
        // which is `register_builtins`' decision on the main thread. An
        // *unknown* primitive stays a compile error; that is drift.
        {
            let mneme = self.mneme.clone();
            m.function("mneme_call", move |kind: String, args: Value| {
                let mneme = mneme.clone();
                async move {
                    let Some(mneme) = mneme else {
                        return Err(NO_VAULT.into());
                    };
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
        }
        {
            let weak = weak.clone();
            m.function("mneme_rpc", move |function: String, args: Value| {
                let host = live(&weak);
                let cancel = current_cancel();
                async move {
                    let host = host?;
                    if !host.has_mneme() {
                        return Err(NO_VAULT.into());
                    }
                    let args = serde_json::to_value(&args).map_err(|e| e.to_string())?;
                    host
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
        // The swarm, on the same two-rung shape as the vault above.
        // `swarm_call` is the raw primitive the two built-in scripts wrap,
        // reached only after the dispatcher has said yes; `peers` and
        // `peer_send` are what *other* scripts call, and route back
        // through the chokepoint so a tool messaging a neighbour is a
        // tool call like any other.
        //
        // These are registered **unconditionally** and answer at call time,
        // like every rung above — and for two reasons, not one: a
        // registration *can* arrive late (the context is warmed on a
        // background thread the moment the host exists, so a module built
        // from `self.swarm` races that thread and loses about half the
        // time, leaving the built-ins to fail compilation against a
        // primitive that was there a millisecond later), and an absent
        // swarm is a configuration, which is a call-time fact and not a
        // compile error. What *does* gate on the registration is whether
        // the two tools exist at all, which is decided on the main thread
        // after it (`register_builtins`).
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
                        "answer" => {
                            let to = args
                                .get("to")
                                .and_then(|v| v.as_str())
                                .ok_or("answer needs a `to`")?;
                            let id = args
                                .get("id")
                                .and_then(|v| v.as_str())
                                .ok_or("answer needs the question's `id`")?;
                            let text = args
                                .get("text")
                                .and_then(|v| v.as_str())
                                .ok_or("answer needs a `text`")?;
                            eidolon_swarm::answer(&presence, to, id, text)
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
        // The door the named wrappers above are one spelling of: a script
        // names a tool and hands it input, and what it names is looked up,
        // classified and journaled by the one dispatcher exactly as a
        // model's call would be — origin is the only difference, and it is
        // the script's, never the model's. This is what a plugin workflow
        // is (vault, *The Plugin System (design)*, §Workflows): a script
        // driving dispatch over tools, nothing else. Input reaches the
        // dispatcher as JSON, so the manifest schema check a model's call
        // gets applies to a script's call too.
        {
            let weak = weak.clone();
            m.function("tool_call", move |name: String, input: Value| {
                let host = live(&weak);
                let cancel = current_cancel();
                async move {
                    let input = serde_json::to_value(&input).map_err(|e| e.to_string())?;
                    host?.dispatch(&name, input, &cancel).await
                }
            })
            .build()?;
        }
        // The same door, with the caller naming the call. A workflow step
        // journals its request under the run's own id before it dispatches,
        // and a resumed run dispatches under the id its journal already
        // holds — the chokepoint answers with what the branch holds under
        // that id, which is the memo a resume replays instead of the
        // side effect it would re-fire. Rune functions take a fixed arity,
        // so the optional argument is not one: `None` here would be a
        // spelling every `tool_call` caller pays for and no caller of this
        // wants. Registered beside `tool_call` and contained the same way —
        // the narrow workflow scope decides who reaches it, and an ordinary
        // tool script mints.
        {
            let weak = weak.clone();
            m.function(
                "tool_call_as",
                move |name: String, input: Value, id: String| {
                    let host = live(&weak);
                    let cancel = current_cancel();
                    async move {
                        let input = serde_json::to_value(&input).map_err(|e| e.to_string())?;
                        host?
                            .dispatch_as(&name, input, Some(&id), &cancel)
                            .await
                    }
                },
            )
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

/// The successful arm of the model-call envelope: `{"ok":true,"reply":…}`
/// as JSON *text*, the shape Melete's scripts parse.
fn envelope_ok(reply: &str) -> String {
    serde_json::json!({ "ok": true, "reply": reply }).to_string()
}

/// The failed arm, and the only other shape there is: a call that found
/// no model, named no prompt, or got no answer is `{"ok":false,"error":…}`
/// — a String like the success it stands beside, so a script's parse
/// never meets a second type.
fn envelope_err(error: &str) -> String {
    serde_json::json!({ "ok": false, "error": error }).to_string()
}

/// The `history` a model call rides in: `user`/`assistant` turns of plain
/// text, honoured as given and in order. Absent and null are both "none",
/// because a Rune object's absent field and explicit null are the same
/// read at this boundary and one of them is always a surprise.
fn history_from(json: &serde_json::Value) -> Result<Vec<Message>, String> {
    let Some(list) = json.get("history").filter(|h| !h.is_null()) else {
        return Ok(Vec::new());
    };
    let Some(list) = list.as_array() else {
        return Err("`history` must be a list of { role, content } turns".to_string());
    };
    let mut out = Vec::with_capacity(list.len());
    for turn in list {
        let role = turn
            .get("role")
            .and_then(|r| r.as_str())
            .ok_or("a history turn needs a `role` string")?;
        let content = turn
            .get("content")
            .and_then(|c| c.as_str())
            .ok_or("a history turn needs a `content` string")?;
        out.push(match role {
            "user" => Message::user_text(content),
            "assistant" => Message::assistant(vec![ContentBlock::text(content)]),
            other => {
                return Err(format!(
                    "a history turn's `role` is `user` or `assistant`, not `{other}`"
                ))
            }
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The consumer the audit lines write: a plugin's tool vouches by its
    /// plugin, the operator's by its own name, and nothing on the thread
    /// means no consumer. Nested calls are attributed at their own level —
    /// the plugin rides the chain entry, not the thread.
    #[test]
    fn the_consumer_names_whose_effect_the_calls_cause() {
        assert_eq!(current_consumer(), "");
        let empty: Arc<[ChainEntry]> = Arc::from(Vec::new());
        let outer = with_tool("browser_open", Some("browser"), empty, || {
            assert_eq!(current_consumer(), "plugin:browser");
            let inner_chain = current_chain();
            with_tool("read", None, inner_chain, || {
                assert_eq!(
                    current_consumer(),
                    "read",
                    "a user tool dispatched from a plugin's script is the user's"
                );
            });
            current_consumer()
        });
        assert_eq!(outer, "plugin:browser", "the inner call left no trace");
    }
}
