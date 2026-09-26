//! A provider declared by a Rune script.
//!
//! ```rune
//! pub fn provider() {
//!     #{ name: "gw", wire: "openai", base_url: "https://…/v1",
//!        token_file: "~/.config/eidolon/gw.token",
//!        compat: #{ max_completion_tokens: true },
//!        models: [ #{ id: "gpt-5.4", name: "GPT-5.4", context: 200000,
//!                     cost: #{ input: 2.5, output: 15.0 } } ] }
//! }
//! // optional: rewrite the outgoing JSON body
//! pub fn request(body) { body.remove("stream_options"); body }
//! // optional: list models dynamically (GET, bearer added for you; a
//! // POST-only surface uses `eidolon::http_post(url, body)` the same way)
//! pub fn models() { json::from_string(eidolon::http_get("/models")).data }
//! // optional: what the subscription has left — see `quota.rs`
//! pub fn quota() { #{ plan: "lite", windows: [ #{ label: "5h", percent: 7 } ] } }
//! ```
//!
//! `provider()` runs once at load; `models()` too, if present, merging into
//! the declared list; `request(body)` runs per call, synchronously, on a
//! fresh VM; `quota()` runs when a consumer asks ([`ProviderScript::quota`])
//! and never at load. The script sees `json`, `eidolon::http_get(path_or_url)`
//! and `eidolon::http_post(path_or_url, body)` — and nothing else: no
//! filesystem, no process.
//!
//! ## The host's fetches answer from a cache
//!
//! A gateway's `models()` is a network round trip, and it ran on the main
//! thread at every launch: the harness could not draw a prompt until a
//! server on another continent had answered — about 80 ms when it was up,
//! and the request's whole timeout when it was not. That is not what a
//! model listing is worth. So *both* host fetches — `http_get`, and the
//! `http_post` a POST-only listing needs — keep the last body they fetched
//! under the cache directory and, by default ([`Fetch::Cached`]), answer
//! from there at once; when the copy is older than [`REFRESH_AFTER`] it is
//! refreshed on a background thread for the *next* launch. A URL never
//! fetched before is fetched in the background too and reported to the
//! script as an error this once, which a `models()` written to survive a
//! gateway being down already handles.
//!
//! The invariant this exists to keep is the harness's: **nothing on the
//! way to the first draw may touch the network.** Under `Fetch::Cached` —
//! how every launch builds its catalog — neither fetch does network work
//! on the calling thread, and neither does the credential, which is
//! resolved on the thread that owns the request rather than on the
//! caller's stack: a bearer is a mint as often as it is a file read (the
//! Gemini wire's access token is an OAuth exchange at Google), and that is
//! a round trip like any other. A cached body is answered *before* the
//! credential is looked at, so a launch that already has yesterday's
//! listing never pays for one. [`Fetch::Live`] is the old behaviour — go
//! to the network, wait, and prime the cache — for a listing command whose
//! point is the current truth, for `quota()` (see below), and for a
//! script's `request(body)`, which runs when a request is already on its
//! way to that gateway.
//!
//! Only the *body* is cached, never the credential: the bearer is added
//! Rust-side on every fetch and the script sees neither.
//!
//! `quota()` is the one hook that runs with [`Fetch::Live`] whatever the
//! catalog was built with. A model listing is worth a stale answer — the
//! gateway had the same models an hour ago — and a quota is not: the
//! question is how much of the window is gone *now*, after the turn that
//! just settled, and a reading from before it is the wrong number wearing
//! the right label. So the hook is never run on the way to a first frame;
//! a consumer asks from a thread of its own, when a turn ends or a panel
//! opens, and is handed the answer with the time it was taken.
//!
//! ## One context
//!
//! Every provider script compiles against one process-wide Rune context
//! and runs on the one runtime made from it — building a context costs
//! 2.5 ms and compiling a script against it a fraction of one, so three
//! providers used to cost eight milliseconds of context-building for one
//! of work. The context can be shared because nothing in the `eidolon`
//! module closes over a script: what `http_get` needs to know — whose
//! `base_url`, whose token, which fetch policy — is put in a thread-local
//! for the duration of each call ([`Scope`]), which is sound because a
//! call runs the VM to completion on the calling thread.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use rune::runtime::RuntimeContext;
use rune::termcolor::Buffer;
use rune::{Context, Diagnostics, Module, Source, Sources, Unit, Value, Vm};
use serde::Deserialize;
use serde::de::IntoDeserializer;

use crate::def::{ModelDef, ProviderDef};

/// A cached body older than this is refreshed in the background on the
/// launch that reads it. Short enough that a gateway's new model lands
/// within the hour, long enough that a burst of restarts is one request.
pub const REFRESH_AFTER: Duration = Duration::from_secs(10 * 60);

/// How `http_get` reaches the network. See the module docs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fetch {
    /// Answer from the cache, refreshing it in the background when stale
    /// or missing. The default: startup must not wait on a gateway.
    #[default]
    Cached,
    /// Go to the network and wait; write what comes back to the cache.
    Live,
}

pub struct ProviderScript {
    unit: Arc<Unit>,
    has_request: bool,
    has_quota: bool,
    /// Whether the script defines `models()`. With [`Self::declared`], what
    /// a patch needs to rebuild the list — see [`Self::rediscover_models`].
    has_models: bool,
    /// The models `provider()` itself declared, before `models()` merged its
    /// discovery in: the floor the listing is merged over, kept so the
    /// merge can be redone against a patched definition without the
    /// load-time answer sticking.
    declared: Vec<ModelDef>,
    /// What `http_get` needs when `request(body)` calls it after load.
    def: ProviderDef,
    store: Option<Arc<harnox::secrets::SecretStore>>,
    fetch: Fetch,
}

/// What the `eidolon` module knows about the script it is serving, for
/// the length of one call. `def` is `None` while `provider()` itself runs,
/// since the base URL and token are what it is about to declare.
#[derive(Clone)]
struct Scope {
    def: Option<ProviderDef>,
    store: Option<Arc<harnox::secrets::SecretStore>>,
    fetch: Fetch,
}

thread_local! {
    static SCOPE: RefCell<Option<Scope>> = const { RefCell::new(None) };
}

struct Compiler {
    context: Context,
    runtime: Arc<RuntimeContext>,
}

static COMPILER: OnceLock<Result<Compiler, String>> = OnceLock::new();

/// The one context provider scripts compile against, built on first use.
fn compiler() -> anyhow::Result<&'static Compiler> {
    COMPILER
        .get_or_init(|| {
            let mut context =
                Context::with_default_modules().map_err(|e| format!("rune context: {e}"))?;
            context
                .install(
                    rune_modules::json::module(false).map_err(|e| format!("json module: {e}"))?,
                )
                .map_err(|e| format!("installing json: {e}"))?;
            context
                .install(host_module().map_err(|e| format!("eidolon module: {e}"))?)
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

/// Build the shared context ahead of the first script, from any thread.
/// A launch calls this beside the other context warm-ups so the three Rune
/// environments the harness needs are built at once rather than in turn.
pub fn warm() {
    let _ = compiler();
}

impl ProviderScript {
    /// Compile, run `provider()`, run `models()` if present.
    pub fn load(name: &str, src: &str) -> anyhow::Result<(ProviderDef, Arc<ProviderScript>)> {
        Self::load_with(name, src, None)
    }

    /// As [`load`](Self::load), with a secret store for `http_get`'s bearer.
    pub fn load_with(
        name: &str,
        src: &str,
        store: Option<Arc<harnox::secrets::SecretStore>>,
    ) -> anyhow::Result<(ProviderDef, Arc<ProviderScript>)> {
        Self::load_with_fetch(name, src, store, Fetch::default())
    }

    /// As [`load_with`](Self::load_with), saying how `http_get` may reach
    /// the network.
    pub fn load_with_fetch(
        name: &str,
        src: &str,
        store: Option<Arc<harnox::secrets::SecretStore>>,
        fetch: Fetch,
    ) -> anyhow::Result<(ProviderDef, Arc<ProviderScript>)> {
        let compiler = compiler()?;
        let mut sources = Sources::new();
        sources
            .insert(Source::new(name, src).map_err(|e| anyhow!("source: {e}"))?)
            .map_err(|e| anyhow!("inserting source: {e}"))?;
        let mut diagnostics = Diagnostics::new();
        let result = rune::prepare(&mut sources)
            .with_context(&compiler.context)
            .with_diagnostics(&mut diagnostics)
            .build();
        let unit = match result {
            Ok(u) => Arc::new(u),
            Err(_) => {
                let mut buf = Buffer::no_color();
                let rendered = match diagnostics.emit(&mut buf, &sources) {
                    Ok(()) => String::from_utf8_lossy(buf.as_slice()).into_owned(),
                    Err(e) => format!("<could not render diagnostics: {e}>"),
                };
                bail!(
                    "provider script `{name}` failed to compile:\n{}",
                    rendered.trim()
                );
            }
        };

        let has = |f: &str| {
            Vm::new(compiler.runtime.clone(), unit.clone())
                .lookup_function([f])
                .is_ok()
        };
        if !has("provider") {
            bail!("provider script `{name}` has no `pub fn provider()`");
        }
        let has_request = has("request");
        let has_models = has("models");
        let has_quota = has("quota");

        let mut scope = Scope {
            def: None,
            store,
            fetch,
        };
        let v = call_sync(&unit, "provider", Vec::new(), &scope)?;
        let mut def: ProviderDef = serde_json::from_value(v)
            .with_context(|| format!("provider script `{name}`: provider() has the wrong shape"))?;
        if def.name.is_empty() {
            def.name = name.to_string();
        }
        scope.def = Some(def.clone());
        let declared = def.models.clone();
        if has_models {
            let v = call_sync(&unit, "models", Vec::new(), &scope)?;
            let extra: Vec<ModelDef> = serde_json::from_value(v).with_context(|| {
                format!("provider script `{name}`: models() has the wrong shape")
            })?;
            merge_models(&mut def.models, extra);
        }
        let script = Arc::new(ProviderScript {
            unit,
            has_request,
            has_quota,
            has_models,
            declared,
            def: def.clone(),
            store: scope.store,
            fetch,
        });
        Ok((def, script))
    }

    pub fn has_request_hook(&self) -> bool {
        self.has_request
    }

    /// This script over a different definition — the one a
    /// `[[providers]]` patch made of the one it declared.
    ///
    /// `http_get` resolves its base URL and its bearer from the definition
    /// the script holds, so a script kept beside a *patched* definition
    /// while holding the original would send `quota()` and `request()` to
    /// the endpoint and the key the operator had just moved away from —
    /// on this machine, a patch naming a `token_file` clears the secret
    /// the built-in named, and the built-in's copy would go on asking a
    /// store that was never filled. The unit is shared; only the
    /// definition changes. A patch must also run
    /// [`rediscover_models`](Self::rediscover_models): `models()` already
    /// ran, at load, against the definition being replaced here.
    pub fn with_def(&self, def: ProviderDef) -> ProviderScript {
        ProviderScript {
            unit: self.unit.clone(),
            has_request: self.has_request,
            has_quota: self.has_quota,
            has_models: self.has_models,
            declared: self.declared.clone(),
            def,
            store: self.store.clone(),
            fetch: self.fetch,
        }
    }

    /// The definition with its model list rebuilt as a patch needs: the
    /// script's own declared rows, plus what `models()` discovers *now*,
    /// against this definition's endpoint and credential.
    ///
    /// `models()` ran once at load, against the definition `provider()`
    /// returned — before any patch moved the key — and on a machine whose
    /// patch does move the key (a `token_file` beside an unfilled secret,
    /// say) that run could not authenticate: the fetch failed, the declared
    /// floor stood in, and retargeting the script afterwards changed
    /// nothing, because the listing is the one hook that had already run.
    /// Without this, a patch would quietly freeze the provider at its
    /// load-time answer forever.
    ///
    /// A failure here is the declared floor, not an error: this runs where
    /// the load-time run did, and a listing is worth a stale answer but
    /// never a failed start. The fetch answers from the cache as
    /// [`Fetch::Cached`] does when the script was so loaded.
    pub fn rediscover_models(&self, mut def: ProviderDef) -> ProviderDef {
        if !self.has_models {
            return def;
        }
        let scope = Scope {
            def: Some(def.clone()),
            store: self.store.clone(),
            fetch: self.fetch,
        };
        let extra = call_sync(&self.unit, "models", Vec::new(), &scope)
            .and_then(|v| {
                serde_json::from_value::<Vec<ModelDef>>(v).with_context(|| {
                    format!("provider script `{}`: models() has the wrong shape", self.def.name)
                })
            })
            .inspect_err(|e| tracing::debug!(provider = %self.def.name, error = %format_args!("{e:#}"), "patch-time model rediscovery failed"));
        def.models = self.declared.clone();
        if let Ok(extra) = extra {
            merge_models(&mut def.models, extra);
        }
        def
    }

    /// Whether the script can say what its subscription has left.
    pub fn has_quota_hook(&self) -> bool {
        self.has_quota
    }

    /// Run `quota()`: what the account behind this provider has left of
    /// its allowances, read from the network now.
    ///
    /// Blocking, for the length of the round trip — up to `http_get`'s
    /// timeout when the endpoint is down — so a consumer with a frame to
    /// draw calls it from a thread of its own. Always live, whatever
    /// `fetch` the script was loaded with: see the module docs for why a
    /// cached quota is the wrong number.
    ///
    /// The script answers with a [`QuotaReply`](crate::quota::QuotaReply):
    /// a reading, or `#{ error: "…" }` with the sentence to show. A script
    /// with no `quota()` is an error here rather than an empty reading,
    /// because the caller is meant to ask [`has_quota_hook`](Self::has_quota_hook)
    /// first and not draw a panel section for a provider that has nothing
    /// to put in it.
    pub fn quota(&self) -> anyhow::Result<crate::quota::Quota> {
        if !self.has_quota {
            bail!("provider `{}` has no `quota()` hook", self.def.name);
        }
        let scope = Scope {
            def: Some(self.def.clone()),
            store: self.store.clone(),
            fetch: Fetch::Live,
        };
        let v = call_sync(&self.unit, "quota", Vec::new(), &scope)?;
        let reply: crate::quota::QuotaReply = serde_json::from_value(v).with_context(|| {
            format!(
                "provider script `{}`: quota() has the wrong shape",
                self.def.name
            )
        })?;
        reply.into_result()
    }

    /// Call any function the script defines with one JSON argument — for
    /// tests of a script's private helpers, which is why it is not `pub`.
    #[cfg(test)]
    pub(crate) fn call(
        &self,
        name: &str,
        arg: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let scope = Scope {
            def: Some(self.def.clone()),
            store: self.store.clone(),
            fetch: self.fetch,
        };
        call_sync(&self.unit, name, vec![arg], &scope)
    }

    /// Apply `request(body)`; identity if the script has none.
    ///
    /// Always [`Fetch::Live`], whatever the catalog was built with: a
    /// request body is rewritten on the way out to a gateway the turn is
    /// already talking to, so a cached answer — or the cache's polite
    /// refusal — would be the wrong reply to a live call. Nothing here is
    /// on the launch's path.
    pub fn rewrite(&self, body: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        if !self.has_request {
            return Ok(body);
        }
        let scope = Scope {
            def: Some(self.def.clone()),
            store: self.store.clone(),
            fetch: Fetch::Live,
        };
        call_sync(&self.unit, "request", vec![body], &scope)
    }
}

/// Run `name(args…)` to completion on this thread, with `scope` visible
/// to the host functions it calls.
fn call_sync(
    unit: &Arc<Unit>,
    name: &str,
    args: Vec<serde_json::Value>,
    scope: &Scope,
) -> anyhow::Result<serde_json::Value> {
    let runtime = compiler()?.runtime.clone();
    let prev = SCOPE.with(|s| s.replace(Some(scope.clone())));
    let out = (|| {
        let mut vm = Vm::new(runtime, unit.clone());
        let args: Vec<Value> = args.iter().map(to_rune).collect();
        let exec = match args.len() {
            1 => vm.execute([name], (args[0].clone(),)),
            _ => vm.execute([name], ()),
        };
        let value = exec
            .map_err(|e| anyhow!("provider script has no `{name}`: {e}"))?
            .complete()
            .into_result()
            .map_err(|e| anyhow!("provider script `{name}()` failed: {e}"))?;
        serde_json::to_value(&value)
            .map_err(|e| anyhow!("provider `{name}()` returned a value that is not data: {e}"))
    })();
    SCOPE.with(|s| *s.borrow_mut() = prev);
    out
}

fn to_rune(v: &serde_json::Value) -> Value {
    Value::deserialize(v.clone().into_deserializer()).unwrap_or_else(|_| Value::from(()))
}

/// Discovered rows onto declared ones: a declared row wins on the same id,
/// because it is the copy that carries what the listing could not say.
fn merge_models(base: &mut Vec<ModelDef>, extra: Vec<ModelDef>) {
    for m in extra {
        if !base.iter().any(|x| x.id == m.id) {
            base.push(m);
        }
    }
}

fn host_module() -> Result<Module, rune::ContextError> {
    let mut m = Module::with_crate("eidolon")?;
    m.function("epoch_ms", |s: String| -> Option<i64> {
        // The reset-time strings the Antigravity surface returns are RFC
        // 3339 ("2026-09-09T09:16:19Z"); Rune has no calendar, so the parse
        // is the host's. Any other shape is None (no reset drawn).
        use time::OffsetDateTime;
        let ok = OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
            .ok()?
            .unix_timestamp();
        Some(ok * 1000)
    })
    .build()?;
    m.function("http_post", |path: String, body: String| -> Result<String, String> {
        let scope = SCOPE
            .with(|s| s.borrow().clone())
            .ok_or("http_post is only available while a provider script runs")?;
        let def = scope
            .def
            .ok_or("http_post is only available after provider() has returned")?;
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path
        } else {
            format!(
                "{}/{}",
                def.base_url.trim_end_matches('/'),
                path.trim_start_matches('/')
            )
        };
        let req = Post {
            provider: def.name.clone(),
            url,
            def: def.clone(),
            store: scope.store.clone(),
            body,
        };
        // A POST is cached under the request it answers — URL *and* body —
        // so the Live pass (`eidolon models`) primes exactly what the next
        // launch's Cached pass reads back. Neither branch resolves the
        // bearer here; see the module docs.
        match scope.fetch {
            Fetch::Live => {
                let out = req.fetch_blocking()?;
                cache::store(&req.provider, &req.key(), &out);
                Ok(out)
            }
            Fetch::Cached => match cache::load(&req.provider, &req.key()) {
                Some((cached, age)) => {
                    if age > REFRESH_AFTER {
                        req.refresh_in_background();
                    }
                    Ok(cached)
                }
                None => {
                    let url = req.url.clone();
                    req.refresh_in_background();
                    Err(format!(
                        "POST {url}: not cached yet; fetching in the background for the next launch"
                    ))
                }
            },
        }
    })
    .build()?;
    m.function("http_get", |path: String| -> Result<String, String> {
        let scope = SCOPE
            .with(|s| s.borrow().clone())
            .ok_or("http_get is only available while a provider script runs")?;
        let def = scope
            .def
            .ok_or("http_get is only available after provider() has returned")?;
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path
        } else {
            format!(
                "{}/{}",
                def.base_url.trim_end_matches('/'),
                path.trim_start_matches('/')
            )
        };
        // The cache is read before the bearer is looked at: a credential is
        // a mint as often as it is a file read, and a launch that has the
        // last listing needs neither.
        let req = Get {
            provider: def.name.clone(),
            url,
            def,
            store: scope.store.clone(),
        };
        match scope.fetch {
            Fetch::Live => {
                let body = req.fetch_blocking()?;
                cache::store(&req.provider, &req.key(), &body);
                Ok(body)
            }
            Fetch::Cached => match cache::load(&req.provider, &req.key()) {
                Some((body, age)) => {
                    if age > REFRESH_AFTER {
                        req.refresh_in_background();
                    }
                    Ok(body)
                }
                None => {
                    let url = req.url.clone();
                    req.refresh_in_background();
                    Err(format!(
                        "GET {url}: not cached yet; fetching in the background for the next launch"
                    ))
                }
            },
        }
    })
    .build()?;
    Ok(m)
}

/// One POST, holding what it needs to run on any thread — the definition
/// and the store rather than a resolved bearer, because the credential is
/// resolved on the thread doing the fetch and never on the caller's.
struct Post {
    provider: String,
    url: String,
    def: ProviderDef,
    store: Option<Arc<harnox::secrets::SecretStore>>,
    body: String,
}

impl Post {
    /// The cache key: the request, not just its URL. A POST's body is part
    /// of what it asks for, so two calls to one endpoint are two entries —
    /// the body's digest in the name is what keeps a prompt out of a
    /// filename and the entries bounded.
    fn key(&self) -> String {
        format!("POST {} {}", self.url, cache::digest(&self.body))
    }

    fn fetch_blocking(&self) -> Result<String, String> {
        std::thread::scope(|s| {
            s.spawn(|| self.fetch_here())
                .join()
                .unwrap_or_else(|_| Err("http_post thread panicked".into()))
        })
    }

    fn fetch_here(&self) -> Result<String, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        rt.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|e| e.to_string())?;
            let mut rb = client
                .post(&self.url)
                .header(reqwest::header::CONTENT_TYPE, "application/json");

            // Rust-side, on this thread: the script never sees the bearer,
            // the cache never holds it, and no launch waits on the mint.
            let token = self
                .def
                .token(self.store.as_deref())
                .map_err(|e| e.to_string())?;
            if let Some(t) = &token {
                rb = rb.bearer_auth(t.as_str());
            }
            if self.def.project.is_some() {
                for (k, v) in harnox::llm::gemini::code_assist_headers() {
                    rb = rb.header(k, v);
                }
            }
            for (k, v) in &self.def.headers {
                rb = rb.header(k, v);
            }
            rb = rb.body(self.body.clone());

            let resp = rb.send().await.map_err(|e| e.to_string())?;
            let status = resp.status();
            let text = resp.text().await.map_err(|e| e.to_string())?;
            if !status.is_success() {
                let trimmed = text.trim();
                let short_err = if trimmed.len() > 200 {
                    format!("{}...", &trimmed[..200])
                } else {
                    trimmed.to_string()
                };
                return Err(format!(
                    "POST {}: HTTP {status}: {short_err}",
                    self.url,
                ));
            }
            Ok(text)
        })
    }

    /// Fetch on a thread of its own — credential and all — and write the
    /// cache for the next launch. Nothing waits on it; a failure is a
    /// debug line, since the launch that spawned it already has an answer.
    fn refresh_in_background(self) {
        let spawned = std::thread::Builder::new().name("provider-refresh".into()).spawn(move || match self.fetch_here() {
            Ok(body) => cache::store(&self.provider, &self.key(), &body),
            Err(e) => tracing::debug!(provider = %self.provider, url = %self.url, error = %e, "background refresh failed"),
        });
        if let Err(e) = spawned {
            tracing::debug!(error = %e, "could not spawn the provider refresh thread");
        }
    }
}

/// One GET, holding what it needs to run on any thread: the definition and
/// the store rather than a resolved bearer, so the credential is resolved
/// on the thread doing the fetch and a cache read never waits on a mint.
struct Get {
    provider: String,
    url: String,
    def: ProviderDef,
    store: Option<Arc<harnox::secrets::SecretStore>>,
}

impl Get {
    /// The cache key: the URL, as it has always been. A GET says everything
    /// it asks for in its URL, so an entry written before `http_post` grew
    /// a cache beside it is still a hit.
    fn key(&self) -> String {
        self.url.clone()
    }

    /// Sync fetch: the request runs on its own thread and runtime, so a
    /// script's `models()` can stay a plain `pub fn` and the caller may
    /// already be inside a runtime.
    fn fetch_blocking(&self) -> Result<String, String> {
        std::thread::scope(|s| {
            s.spawn(|| self.fetch_here())
                .join()
                .unwrap_or_else(|_| Err("http_get thread panicked".into()))
        })
    }

    fn fetch_here(&self) -> Result<String, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        rt.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|e| e.to_string())?;
            let mut rb = client.get(&self.url);
            // Rust-side, on this thread: the script never sees the bearer,
            // the cache never holds it, and no launch waits on the mint.
            let token = self
                .def
                .token(self.store.as_deref())
                .map_err(|e| e.to_string())?;
            if let Some(t) = &token {
                rb = rb.bearer_auth(t.as_str());
            }
            for (k, v) in &self.def.headers {
                rb = rb.header(k, v);
            }
            let resp = rb.send().await.map_err(|e| e.to_string())?;
            let status = resp.status();
            let text = resp.text().await.map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!(
                    "GET {}: HTTP {status}: {}",
                    self.url,
                    text.chars().take(300).collect::<String>()
                ));
            }
            Ok(text)
        })
    }

    /// Fetch on a thread of its own — credential and all — and write the
    /// cache for the next launch. Nothing waits on it; a failure is a debug
    /// line, since the launch that spawned it already has an answer.
    fn refresh_in_background(self) {
        let spawned = std::thread::Builder::new().name("provider-refresh".into()).spawn(move || match self.fetch_here() {
            Ok(body) => cache::store(&self.provider, &self.key(), &body),
            Err(e) => tracing::debug!(provider = %self.provider, url = %self.url, error = %e, "background refresh failed"),
        });
        if let Err(e) = spawned {
            tracing::debug!(error = %e, "could not spawn the provider refresh thread");
        }
    }
}

/// The body cache: `~/.cache/eidolon/http/<provider>/<key>` — one file per
/// request, the provider's name as the directory so one gateway's listing is
/// easy to find and delete by hand. A GET's key is its URL; a POST's is its
/// URL plus a digest of the body, since the body is part of what it asks
/// for. Written whole, through a temp file and a
/// rename, so a reader never sees half a body.
///
/// The root is decided once and falls back to the temp dir when no cache
/// home exists or the one that does cannot be written — a daemon without a
/// home, or a sandboxed build whose `$HOME` is a directory it may not
/// create. Same posture as the swarm registry's root: a degraded location
/// beats silently caching nowhere, which turns "cache miss" into "feature
/// absent" for whoever runs without a home.
mod cache {
    use std::path::PathBuf;
    use std::time::Duration;

    fn root() -> PathBuf {
        static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| {
            let preferred = dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("eidolon")
                .join("http");
            if std::fs::create_dir_all(&preferred).is_ok() {
                preferred
            } else {
                let fallback = std::env::temp_dir().join("eidolon").join("http");
                tracing::debug!(
                    preferred = %preferred.display(),
                    fallback = %fallback.display(),
                    "no writable cache home; caching under the temp dir"
                );
                fallback
            }
        })
        .clone()
    }

    pub(super) fn path(provider: &str, key: &str) -> Option<PathBuf> {
        Some(root().join(safe(provider)).join(safe(key)))
    }

    /// A stable short digest of a request body, for a POST's cache key: the
    /// body itself never reaches the filesystem, and one body always names
    /// one entry. FNV-1a rather than `DefaultHasher`, because a cache
    /// filename that changed with the Rust version would empty the cache on
    /// every rebuild for no reason.
    pub(super) fn digest(body: &str) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in body.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{h:016x}")
    }

    /// A filename out of a URL: anything but `[A-Za-z0-9._-]` becomes `_`,
    /// capped so a long query string cannot exceed a filesystem's limit.
    fn safe(s: &str) -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .take(180)
            .collect()
    }

    /// The cached body and its age.
    pub(super) fn load(provider: &str, key: &str) -> Option<(String, Duration)> {
        let p = path(provider, key)?;
        let body = std::fs::read_to_string(&p).ok()?;
        let age = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .unwrap_or(Duration::MAX);
        Some((body, age))
    }

    pub fn store(provider: &str, key: &str, body: &str) {
        let Some(p) = path(provider, key) else { return };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = p.with_extension(format!("tmp{}", std::process::id()));
            std::fs::write(&tmp, body)?;
            std::fs::rename(&tmp, &p)
        };
        if let Err(e) = write() {
            tracing::debug!(path = %p.display(), error = %e, "could not write the http cache");
        }
    }
}

/// The body cache as a test needs it: prime an entry exactly as a live
/// fetch would, under a provider name the test made up so nothing else
/// reads it, and say where it landed so the test can clean up after itself.
#[cfg(test)]
pub(crate) mod test_cache {
    pub fn prime(provider: &str, url: &str, body: &str) {
        super::cache::store(provider, url, body);
    }

    pub fn path_of(provider: &str, url: &str) -> Option<std::path::PathBuf> {
        super::cache::path(provider, url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_gateway_script_and_rewrites() {
        let src = r#"
pub fn provider() {
    #{ name: "gw", wire: "openai", base_url: "https://gw.example/v1", token: "x",
       compat: #{ max_completion_tokens: true },
       models: [ #{ id: "m1", name: "Model One", context: 1000, cost: #{ input: 1.0, output: 2.0 } } ] }
}
pub fn request(body) {
    body.remove("stream_options");
    body.insert("gateway", "yes");
    body
}
"#;
        let (def, script) = ProviderScript::load("gw", src).unwrap();
        assert_eq!(def.name, "gw");
        assert!(def.compat.max_completion_tokens);
        assert_eq!(def.models[0].label(), "Model One");
        assert!(script.has_request_hook());
        let out = script
            .rewrite(
                serde_json::json!({ "model": "m1", "stream_options": { "include_usage": true } }),
            )
            .unwrap();
        assert!(out.get("stream_options").is_none());
        assert_eq!(out["gateway"], "yes");
    }

    /// The chatgpt builtin's `read_quota`, against captured usage replies:
    /// the live one from the account this was built on — whose *primary*
    /// window is the week — and the documented Plus shape, with a 5h
    /// primary, a week secondary and a code-review day allowance, which
    /// must come back shortest-first with the review window behind the
    /// ordinary ones.
    #[test]
    fn chatgpt_quota_reads_captured_usage_replies() {
        let (_, script) =
            ProviderScript::load("chatgpt", include_str!("../builtin/chatgpt.rn")).unwrap();
        assert!(script.has_quota_hook());

        let read = |body: serde_json::Value| -> crate::quota::Quota {
            let v = script.call("read_quota", body).unwrap();
            let reply: crate::quota::QuotaReply = serde_json::from_value(v).unwrap();
            reply.into_result().unwrap()
        };

        let live = serde_json::json!(r#"{
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": { "used_percent": 0, "limit_window_seconds": 604800, "reset_at": 1789544033 },
                "secondary_window": null
            },
            "code_review_rate_limit": null
        }"#);
        let q = read(live);
        assert_eq!(q.plan.as_deref(), Some("plus"));
        assert_eq!(
            q.windows.iter().map(|w| (w.label.as_str(), w.percent, w.resets_ms)).collect::<Vec<_>>(),
            [("week", 0.0, Some(1789544033000))],
            "the account's only window is the week, whatever slot it sits in"
        );

        let plus = serde_json::json!(r#"{
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": { "used_percent": 82, "limit_window_seconds": 18000, "reset_at": 100 },
                "secondary_window": { "used_percent": 12, "limit_window_seconds": 604800, "reset_at": 200 }
            },
            "code_review_rate_limit": {
                "primary_window": { "used_percent": 5, "limit_window_seconds": 86400, "reset_at": 300 }
            }
        }"#);
        let q = read(plus);
        assert_eq!(q.plan.as_deref(), Some("pro"));
        assert_eq!(
            q.windows.iter().map(|w| (w.label.as_str(), w.percent)).collect::<Vec<_>>(),
            [("5h", 82.0), ("week", 12.0), ("review", 5.0)],
            "shortest first, review behind, whatever order the reply lists"
        );
        assert_eq!(q.windows[0].resets_ms, Some(100000));

        let empty = serde_json::json!(r#"{ "plan_type": "plus", "rate_limit": {} }"#);
        let v = script.call("read_quota", empty).unwrap();
        let reply: crate::quota::QuotaReply = serde_json::from_value(v).unwrap();
        assert_eq!(
            reply.into_result().unwrap_err().to_string(),
            "the usage reply lists no windows"
        );

        let refused = serde_json::json!("not json at all");
        let v = script.call("read_quota", refused).unwrap();
        let reply: crate::quota::QuotaReply = serde_json::from_value(v).unwrap();
        assert!(reply.into_result().is_err(), "a non-JSON reply is a sentence, not a reading");
    }

    /// The ollama builtin's `read_quota`, against the captured live reply
    /// (2026-09-12): `limits.monthly.usage` is a fraction of the month's
    /// included credits — `0.022` while the settings page read $1.31 of
    /// $60 — so the window is a share with no counts and no reset, the
    /// shape chatgpt's percent-only windows already have. An integer
    /// fraction (a fresh month's `0`) must promote into the same percent
    /// arithmetic rather than integer-multiply to zero.
    #[test]
    fn ollama_quota_reads_the_captured_usage_reply() {
        let (_, script) =
            ProviderScript::load("ollama", include_str!("../builtin/ollama.rn")).unwrap();
        assert!(script.has_quota_hook());

        let read = |body: serde_json::Value| -> crate::quota::Quota {
            let v = script.call("read_quota", body).unwrap();
            let reply: crate::quota::QuotaReply = serde_json::from_value(v).unwrap();
            reply.into_result().unwrap()
        };

        let live = serde_json::json!(r#"{
            "activity": { "cost": "0.00000",
                "period": { "type": "last_4_weeks", "starting_at": "2026-08-17T00:00:00Z",
                            "ending_at": "2026-09-12T07:57:30.151682458Z" },
                "models": [] },
            "limits": { "monthly": { "usage": 0.022,
                "models": [ { "name": "deepseek-v4.1-flash", "request_count": 509 },
                            { "name": "deepseek-v4-pro:0813", "request_count": 1 } ] } }
        }"#);
        let q = read(live);
        assert_eq!(q.plan, None, "the plan is not published on this wire");
        assert_eq!(q.windows.len(), 1);
        assert_eq!(q.windows[0].label, "month");
        assert!(
            (q.windows[0].percent - 2.2).abs() < 1e-9,
            "the fraction becomes a percent: 0.022 → 2.2"
        );
        assert_eq!(q.windows[0].used, None, "the endpoint counts a share, not dollars");
        assert_eq!(q.windows[0].resets_ms, None, "the anniversary is not published");

        let fresh = serde_json::json!(r#"{ "limits": { "monthly": { "usage": 0 } } }"#);
        let q = read(fresh);
        assert_eq!(q.windows[0].percent, 0.0, "an integer 0 promotes, not truncates");

        let no_limits = serde_json::json!(r#"{ "activity": { "cost": "0.00000" } }"#);
        let v = script.call("read_quota", no_limits).unwrap();
        let reply: crate::quota::QuotaReply = serde_json::from_value(v).unwrap();
        assert_eq!(
            reply.into_result().unwrap_err().to_string(),
            "the usage reply lists no monthly usage"
        );

        let refused = serde_json::json!("not json at all");
        let v = script.call("read_quota", refused).unwrap();
        let reply: crate::quota::QuotaReply = serde_json::from_value(v).unwrap();
        assert!(reply.into_result().is_err(), "a non-JSON reply is a sentence, not a reading");
    }

    /// Live, over the network, against the account the stored key names —
    /// run with `cargo test -p eidolon-providers ollama_quota_live --
    /// --ignored --nocapture`. Everything the captured-body test above
    /// cannot cover: the bearer the harness injects out of the custodied
    /// store, the absolute URL beside the `/v1` base, the endpoint's
    /// answer.
    #[test]
    #[ignore = "live: reads the account's quota over the network"]
    fn ollama_quota_live() {
        // The default store location `config::secret_store` would compute,
        // without importing the CLI crate to a providers test.
        let config = std::env::var("XDG_CONFIG_HOME")
            .unwrap_or_else(|_| format!("{}/.config", std::env::var("HOME").unwrap()));
        let store = std::sync::Arc::new(harnox::secrets::SecretStore::at(
            std::path::Path::new(&config).join("eidolon").join("secrets"),
        ));
        let (_, script) = ProviderScript::load_with(
            "ollama",
            include_str!("../builtin/ollama.rn"),
            Some(store),
        )
        .unwrap();
        let q = script.quota().unwrap();
        println!("{q:#?}");
        assert!(!q.windows.is_empty(), "a reading with no windows is no reading");
    }

    /// Live, over the network, against the account the credential names —
    /// run with `cargo test -p eidolon-providers chatgpt_quota_live --
    /// --ignored --nocapture` when the login is fresh. Everything the
    /// captured-body test above cannot cover: the masquerade headers on
    /// `http_get`, the minted bearer, the endpoint's answer.
    #[test]
    #[ignore = "live: reads the account's quota over the network"]
    fn chatgpt_quota_live() {
        let (_, script) =
            ProviderScript::load("chatgpt", include_str!("../builtin/chatgpt.rn")).unwrap();
        let q = script.quota().unwrap();
        println!("{q:#?}");
        assert!(!q.windows.is_empty(), "a reading with no windows is no reading");
    }

    #[test]
    fn missing_provider_fn_is_an_error() {
        assert!(ProviderScript::load("x", "pub fn nope() { 1 }").is_err());
    }

    /// The point of the cache: a `models()` that goes to the network does
    /// not make load wait for it. With nothing cached the script hears an
    /// error and keeps its declared list, and the load returns at once
    /// rather than after a connection attempt to a port nothing listens on.
    #[test]
    fn a_cold_models_fetch_does_not_block_load() {
        let src = r#"
pub fn provider() {
    #{ name: "cold-test-gw", wire: "openai", base_url: "http://127.0.0.1:1", token: "x",
       models: [ #{ id: "declared" } ] }
}
pub fn models() {
    match eidolon::http_get("/models") {
        Ok(_) => [ #{ id: "fetched" } ],
        Err(_) => [],
    }
}
"#;
        let t = std::time::Instant::now();
        let (def, _) = ProviderScript::load("cold-test-gw", src).unwrap();
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "load waited on the network: {:?}",
            t.elapsed()
        );
        assert_eq!(def.models.len(), 1);
        assert_eq!(def.models[0].id, "declared");
    }

    /// The same for a POST-only surface — Antigravity's `models()` posts to
    /// Google's Code Assist endpoint — which was the one host fetch with no
    /// cache at all: every launch minted a Google access token and then
    /// waited for the listing, on the way to the first draw.
    #[test]
    fn a_cold_post_models_fetch_does_not_block_load() {
        let src = r#"
pub fn provider() {
    #{ name: "cold-test-post", wire: "openai", base_url: "http://127.0.0.1:1", token: "x",
       models: [ #{ id: "declared" } ] }
}
pub fn models() {
    match eidolon::http_post("/v1internal:fetchAvailableModels", "{}") {
        Ok(_) => [ #{ id: "fetched" } ],
        Err(_) => [],
    }
}
"#;
        let t = std::time::Instant::now();
        let (def, _) = ProviderScript::load("cold-test-post", src).unwrap();
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "load waited on the network: {:?}",
            t.elapsed()
        );
        assert_eq!(
            def.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["declared"],
            "the declared floor stands while the background fetch runs"
        );
    }

    /// A cached body is answered *before* the credential is looked at. This
    /// provider's token file does not exist, so a fetch that resolved it
    /// first would report that instead of the listing the cache holds — and
    /// that is what a launch used to do: a bearer is a mint as often as a
    /// file read (the Gemini wire's is an OAuth exchange at Google), which
    /// is a round trip nothing on the way to the first frame may wait for.
    #[test]
    fn a_cached_listing_is_answered_before_the_credential() {
        let url = "http://127.0.0.1:1/models";
        test_cache::prime("cache-before-credential", url, r#"[{ "id": "from-cache" }]"#);
        let cleanup = test_cache::path_of("cache-before-credential", url);

        let src = r#"
pub fn provider() {
    #{ name: "cache-before-credential", wire: "openai", base_url: "http://127.0.0.1:1",
       token_file: "/nonexistent/eidolon-token", models: [ #{ id: "declared" } ] }
}
pub fn models() {
    match eidolon::http_get("/models") {
        Ok(b) => match json::from_string(b) { Ok(v) => v, Err(_) => [] },
        Err(_) => [],
    }
}
"#;
        let (def, _) = ProviderScript::load("cache-before-credential", src).unwrap();
        assert_eq!(
            def.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["declared", "from-cache"],
            "the cache answers whether or not a credential can be resolved"
        );

        if let Some(p) = cleanup {
            let _ = std::fs::remove_file(p);
        }
    }

    /// And the same for a POST, whose key is the request — URL *and* body —
    /// so a primed entry is answered exactly as a GET's is.
    #[test]
    fn a_cached_post_is_answered_before_the_credential() {
        let provider = "cache-before-credential-post";
        let url = "http://127.0.0.1:1/v1internal:fetchAvailableModels";
        let key = format!("POST {url} {}", cache::digest("{}"));
        test_cache::prime(provider, &key, r#"[{ "id": "from-cache" }]"#);
        let cleanup = test_cache::path_of(provider, &key);

        let src = r#"
pub fn provider() {
    #{ name: "cache-before-credential-post", wire: "openai", base_url: "http://127.0.0.1:1",
       token_file: "/nonexistent/eidolon-token", models: [ #{ id: "declared" } ] }
}
pub fn models() {
    match eidolon::http_post("/v1internal:fetchAvailableModels", "{}") {
        Ok(b) => match json::from_string(b) { Ok(v) => v, Err(_) => [] },
        Err(_) => [],
    }
}
"#;
        let (def, _) = ProviderScript::load(provider, src).unwrap();
        assert_eq!(
            def.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["declared", "from-cache"],
            "a POST answers from the entry its own body names"
        );

        if let Some(p) = cleanup {
            let _ = std::fs::remove_file(p);
        }
    }

    /// `quota()` is a reading or a sentence, on one shape: an object with
    /// an `error` field is the error, and a reading is checked on the way
    /// in. A script without the hook says so rather than answering empty.
    #[test]
    fn quota_is_a_reading_or_a_sentence() {
        let src = r#"
pub fn provider() {
    #{ name: "plan", wire: "openai", base_url: "http://x", token: "t", models: [ #{ id: "m" } ] }
}
pub fn quota() {
    #{ plan: "lite", windows: [ #{ label: "5h", percent: 7, used: 158, limit: 2000, resets_ms: 1788693968953 }, #{ label: "week", percent: 1 } ] }
}
"#;
        let (_, script) = ProviderScript::load("plan", src).unwrap();
        assert!(script.has_quota_hook());
        let q = script.quota().unwrap();
        assert_eq!(q.plan.as_deref(), Some("lite"));
        assert_eq!(
            q.windows
                .iter()
                .map(|w| (w.label.as_str(), w.percent))
                .collect::<Vec<_>>(),
            [("5h", 7.0), ("week", 1.0)]
        );
        assert_eq!(q.windows[0].limit, Some(2000));
        assert_eq!(q.windows[1].limit, None);

        let src = r#"
pub fn provider() { #{ name: "down", wire: "openai", base_url: "http://x", token: "t", models: [ #{ id: "m" } ] } }
pub fn quota() { #{ error: "GET http://x/quota: HTTP 401" } }
"#;
        let (_, script) = ProviderScript::load("down", src).unwrap();
        assert_eq!(
            script.quota().unwrap_err().to_string(),
            "GET http://x/quota: HTTP 401"
        );

        let src = r#"pub fn provider() { #{ name: "metered", wire: "openai", base_url: "http://x", token: "t", models: [ #{ id: "m" } ] } }"#;
        let (_, script) = ProviderScript::load("metered", src).unwrap();
        assert!(!script.has_quota_hook());
        assert!(
            script
                .quota()
                .unwrap_err()
                .to_string()
                .contains("no `quota()` hook")
        );
    }

    /// `http_get` is scoped to the script running: outside a call it says
    /// so instead of guessing at a base URL.
    #[test]
    fn http_get_needs_a_provider() {
        let src = r#"
pub fn provider() {
    let r = eidolon::http_get("/models");
    #{ name: "early", wire: "openai", base_url: "http://x", token: "t", models: [ #{ id: r.is_err() ? "refused" : "answered" } ] }
}
"#;
        // Rune has no ternary; spell it out.
        let src = src.replace(
            r#"r.is_err() ? "refused" : "answered""#,
            r#"if r.is_err() { "refused" } else { "answered" }"#,
        );
        let (def, _) = ProviderScript::load("early", &src).unwrap();
        assert_eq!(def.models[0].id, "refused");
    }
}

