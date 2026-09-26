//! Every provider and model the harness can reach, and how a model string
//! becomes a backend.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, bail};

use eidolon_core::agent::Backend;
use eidolon_core::provider::Provider;

use crate::def::{Cost, ProviderDef};
use crate::http::HttpProvider;
use crate::script::ProviderScript;

pub const CLAUDE_CLI: &str = "claude-cli";

/// Providers that ship with the harness, on the same contract as one a
/// user drops in `providers_dir`.
pub const BUILTINS: &[(&str, &str)] = &[
    ("deepseek", include_str!("../builtin/deepseek.rn")),
    ("zai", include_str!("../builtin/zai.rn")),
    ("openrouter", include_str!("../builtin/openrouter.rn")),
    ("antigravity", include_str!("../builtin/antigravity.rn")),
    ("chatgpt", include_str!("../builtin/chatgpt.rn")),
    ("ollama", include_str!("../builtin/ollama.rn")),
];

pub struct ProviderEntry {
    pub def: ProviderDef,
    pub provider: Arc<dyn Provider>,
    /// The script that declared it, kept so a patch can be laid over the
    /// definition without losing its `request`/`models` hooks.
    pub script: Option<Arc<ProviderScript>>,
    /// Where it came from, for `eidolon models`.
    pub source: String,
}

/// The Claude CLI as a catalog member.
#[derive(Clone, Debug)]
pub struct ClaudeCliEntry {
    pub binary: Option<String>,
    /// Globs and/or exact ids; exact ids become picker rows.
    pub models: Vec<String>,
    /// Which tools the CLI may use (`own`, `eidolon`, `both`).
    pub tools: eidolon_claude::ToolMode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogModel {
    /// `provider:id`
    pub key: String,
    pub provider: String,
    pub id: String,
    pub name: String,
    pub context: Option<u64>,
    pub reasoning: bool,
    /// Whether the model can be shown an image; `None` is "nobody said".
    /// See [`crate::def::ModelDef::vision`] for why absent is not `false`.
    pub vision: Option<bool>,
    /// All four rates, not the two the picker shows: a consumer pricing a
    /// turn needs the cache rates, and this used to narrow them away on
    /// the way past — which is why they were declared and dead.
    pub cost: Option<Cost>,
    /// The operator's ordering weight — see [`Catalog::set_weights`].
    pub weight: i64,
}

impl CatalogModel {
    /// One line for a picker's description column.
    pub fn describe(&self) -> String {
        let mut parts = vec![self.provider.clone()];
        if let Some(c) = self.context {
            parts.push(format!("{}k ctx", c / 1000));
        }
        if let Some(c) = &self.cost {
            parts.push(format!("${}/{} per M", c.input, c.output));
        }
        if self.reasoning {
            parts.push("reasoning".into());
        }
        // Only the negative is worth a word. Most models see, so "vision"
        // on almost every row would say nothing; "no images" on the few
        // that cannot is what changes whether an operator attaches one.
        if self.vision == Some(false) {
            parts.push("no images".into());
        }
        parts.join(" · ")
    }
}

pub enum Resolved {
    Mock,
    ClaudeCli {
        model: Option<String>,
        binary: Option<String>,
        tools: eidolon_claude::ToolMode,
    },
    Http {
        provider: Arc<dyn Provider>,
        model: String,
        /// The token-pool lane this resolution claimed, when the provider
        /// declares one. Carried out so the caller can journal the claim —
        /// per-session key attribution is a fact about the conversation,
        /// not about the catalog.
        claim: Option<ClaimedKey>,
    },
}

/// One lane of a provider's [`token pool`](crate::def::ProviderDef::token_secrets),
/// taken by this resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimedKey {
    /// The provider whose pool was claimed.
    pub provider: String,
    /// The secret-store name of the lane — the key the backend will send.
    pub secret: String,
    /// Whether this call created the claim. A sticky re-claim (the holder
    /// already held the lane — a model switch back, a judge resolved from
    /// the same process) is not fresh, so a caller journaling attribution
    /// writes one record per provider per session, not one per switch.
    pub fresh: bool,
}

#[derive(Default)]
pub struct Catalog {
    providers: Vec<ProviderEntry>,
    claude: Option<ClaudeCliEntry>,
    store: Option<Arc<harnox::secrets::SecretStore>>,
    /// Where token-pool claims are taken and who is taking them: the
    /// presence registry's root and this session's id in it. `None` is a
    /// catalog that cannot claim — `eidolon models`' listing, or a launch
    /// that did not register — and a pooled provider then serves its
    /// first lane to everyone rather than erroring.
    keypool: Option<(PathBuf, String)>,
    /// How a script's `models()` may reach the network — see
    /// [`Fetch`](crate::script::Fetch). Cached unless a caller says
    /// otherwise, so building a catalog never waits on a gateway.
    fetch: crate::script::Fetch,
    /// The operator's ordering, live: `provider:id` or a glob → weight.
    /// Behind a lock because a weight is *changed from inside a session*
    /// and the catalog is shared — the same reason the dispatcher owns
    /// the working directory rather than every tool holding a copy.
    weights: std::sync::RwLock<std::collections::BTreeMap<String, i64>>,
    pub load_errors: Vec<String>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_claude(mut self, entry: Option<ClaudeCliEntry>) -> Self {
        self.claude = entry;
        self
    }

    /// The custodied secret store providers draw `token_secret` from.
    pub fn with_store(mut self, store: Arc<harnox::secrets::SecretStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// The store this catalog's providers draw their keys from — read by
    /// [`crate::readiness`], which asks the store *presence* and must never
    /// reach for a value: a probe that injected a secret would put the one
    /// injection door on a picker's hot path.
    ///
    /// Crate-visible and deliberately not public: the store's
    /// `external_value` is the single door a secret leaves by, and widening
    /// this to a public accessor would hand every consumer of a catalog a
    /// way to reach it that the audit trail was never designed around.
    pub(crate) fn store(&self) -> Option<&harnox::secrets::SecretStore> {
        self.store.as_deref()
    }

    /// Whether a credential is already stored, without ever reading its
    /// value — for a picker row that says "key set" / "no key" and
    /// nothing more.
    pub fn secret_present(&self, name: &str) -> bool {
        self.store.as_ref().is_some_and(|s| s.has(name))
    }

    /// Write a credential through the catalog's own store — the door
    /// [`store`](Self::store) is deliberately not public about. A `:login`
    /// command is the one caller with a value to write rather than a
    /// presence to ask about, so it gets its own narrow door instead of
    /// the store itself.
    pub fn set_secret(&self, name: &str, value: &str, service: Option<String>) -> anyhow::Result<()> {
        let store = self
            .store
            .as_ref()
            .context("no secret store configured")?;
        store.set(name, value, service, Vec::new())?;
        Ok(())
    }

    /// Let pooled providers claim a lane as `holder` under the presence
    /// registry at `root` — see [`eidolon_swarm::keypool`]. Callers that
    /// pass no holder share the first lane of every pool; a listing that
    /// resolves nothing claims nothing either way.
    pub fn with_keypool(mut self, root: PathBuf, holder: &str) -> Self {
        self.keypool = Some((root, holder.to_string()));
        self
    }

    /// Let scripts' `models()` go to the network and wait for it
    /// ([`Fetch::Live`](crate::script::Fetch::Live)) — for a listing
    /// whose point is the current truth, not for a launch.
    pub fn with_fetch(mut self, fetch: crate::script::Fetch) -> Self {
        self.fetch = fetch;
        self
    }

    pub fn add_def(&mut self, def: ProviderDef, script: Option<Arc<ProviderScript>>, source: &str) {
        let provider: Arc<dyn Provider> = Arc::new(HttpProvider::with_store(
            def.clone(),
            script.clone(),
            self.store.clone(),
        ));
        self.providers.retain(|p| p.def.name != def.name);
        self.providers.push(ProviderEntry {
            def,
            provider,
            script,
            source: source.to_string(),
        });
    }

    /// Take a `[[providers]]` table as *changes* where it names a provider
    /// already loaded, and as a whole definition where it does not.
    ///
    /// Replacing by name is the right answer for a definition someone
    /// wrote; it is the wrong one for a definition that ships inside the
    /// binary. A built-in cannot be edited in place, so moving its key to
    /// a file used to mean copying its wire, its endpoint, its models and
    /// their prices into a file that shadowed it — and then drifted from
    /// it silently, on the next repricing, with the only symptom a cost
    /// estimate quietly going wrong. A patch says the one thing it means
    /// to change and inherits the rest, so there is nothing to drift.
    ///
    /// The hooks survive too: a patched script provider keeps its
    /// `request(body)`, its `quota()` and the models `models()`
    /// discovered, because the script is kept beside the definition it
    /// produced — re-pointed at the definition as patched, so a hook that
    /// reaches the network reaches it with the patch's key. The listing
    /// hook is re-run over the patched definition for the same reason: it
    /// ran at load against the credential being replaced, and a patch that
    /// moves the key would otherwise leave the provider answering with the
    /// load-time list — on a machine where that first fetch could not
    /// authenticate, the declared floor, forever. A patch that lists
    /// models itself is an answer to "which models", and no discovery
    /// runs to un-answer it.
    pub fn patch(&mut self, patch: crate::def::ProviderPatch, source: &str) -> anyhow::Result<()> {
        let Some(i) = self.providers.iter().position(|p| p.def.name == patch.name) else {
            anyhow::bail!("no provider named `{}` to patch", patch.name);
        };
        let lists_models = patch.models.is_some();
        let entry = &self.providers[i];
        let mut def = entry.def.patched(patch);
        // The hooks run over the definition as patched: `http_get` takes
        // its endpoint and its key from the definition the script holds,
        // and a hook reading the account has to read the account the patch
        // pointed at.
        let script = entry.script.as_ref().map(|s| {
            if !lists_models {
                def = s.rediscover_models(def.clone());
            }
            Arc::new(s.with_def(def.clone()))
        });
        let was = entry.source.clone();
        let provider: Arc<dyn Provider> = Arc::new(HttpProvider::with_store(
            def.clone(),
            script.clone(),
            self.store.clone(),
        ));
        self.providers[i] = ProviderEntry {
            def,
            provider,
            script,
            source: format!("{was} + {source}"),
        };
        Ok(())
    }

    pub fn provider_exists(&self, name: &str) -> bool {
        self.providers.iter().any(|p| p.def.name == name)
    }

    /// The built-in provider scripts, embedded so a fresh install reaches a
    /// real model without a config file — only a key. They load first, which
    /// is what makes them replaceable: a `[[providers]]` table or a script in
    /// `providers_dir` under the same name lands on top of one through
    /// [`add_def`](Self::add_def), exactly as a user tool replaces a built-in
    /// tool. A built-in that fails to compile is a `load_errors` line like
    /// any other, never a failed start.
    ///
    /// Unlike the built-in tools, these are *not* seeded into
    /// `providers_dir` for editing in place — a script there loads after
    /// the `[[providers]]` patches and would discard the one naming the
    /// key — only unpacked for reading, under `~/.config/eidolon/system/
    /// providers/` (`eidolon-cli`'s `system` module).
    pub fn load_builtins(&mut self) {
        for (name, src) in BUILTINS {
            match ProviderScript::load_with_fetch(name, src, self.store.clone(), self.fetch) {
                Ok((def, script)) => self.add_def(def, Some(script), "built-in"),
                Err(e) => self
                    .load_errors
                    .push(format!("built-in provider `{name}`: {e:#}")),
            }
        }
    }

    /// Load every `*.rn` in `dir`. A script that fails is recorded in
    /// `load_errors` and skipped, never fatal — one bad gateway file must
    /// not take the others down.
    pub fn load_scripts(&mut self, dir: &Path) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "rn"))
            .collect();
        paths.sort();
        for p in paths {
            let name = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            match std::fs::read_to_string(&p)
                .map_err(anyhow::Error::from)
                .and_then(|src| {
                    ProviderScript::load_with_fetch(&name, &src, self.store.clone(), self.fetch)
                }) {
                Ok((def, script)) => self.add_def(def, Some(script), &p.display().to_string()),
                Err(e) => self.load_errors.push(format!("{}: {e:#}", p.display())),
            }
        }
    }

    pub fn providers(&self) -> &[ProviderEntry] {
        &self.providers
    }

    pub fn has_claude(&self) -> bool {
        self.claude.is_some()
    }

    /// The operator's ordering: `provider:id` (or a glob over it) → a
    /// weight, high first. Everything unnamed is zero, so a table that
    /// promotes three workhorses and buries one gateway's junk is four
    /// lines rather than a full ordering of a hundred models.
    pub fn set_weights(&self, weights: std::collections::BTreeMap<String, i64>) {
        if let Ok(mut w) = self.weights.write() {
            *w = weights;
        }
    }

    /// The weight for one key. An exact entry wins; otherwise the most
    /// specific glob that matches (the longest pattern) does, so
    /// `fau:openai/*` can sink a class and one id inside it can still be
    /// raised back out.
    pub fn weight(&self, key: &str) -> i64 {
        let Ok(w) = self.weights.read() else { return 0 };
        if let Some(n) = w.get(key) {
            return *n;
        }
        w.iter()
            .filter(|(pat, _)| pat.contains('*') && harnox::llm::spec::glob_match(pat, key))
            .max_by_key(|(pat, _)| pat.len())
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }

    /// Every declared context window, by `provider:id`.
    ///
    /// A table rather than a per-key lookup because the consumer that
    /// wants it most — the TUI's status line — lives on a thread with no
    /// catalog, and the model in play can change under it three ways
    /// (the operator picks one, a resume adopts a log's, a replayed
    /// `ModelChanged` moves it mid-branch). Handed the table once, it
    /// answers all three the same way and cannot end up drawing a
    /// fraction against the window of a model it is no longer on.
    ///
    /// A model that declares nothing is absent rather than zero: "this
    /// window is unknown" and "this window is empty" are not the same
    /// claim, and only the first is ever true here.
    pub fn windows(&self) -> std::collections::BTreeMap<String, u64> {
        self.providers
            .iter()
            .flat_map(|p| {
                p.def
                    .models
                    .iter()
                    .filter_map(move |m| m.context.map(|c| (format!("{}:{}", p.def.name, m.id), c)))
            })
            .collect()
    }

    /// The rates a model is billed at, by catalog key.
    ///
    /// `None` is a definition that declares no cost — every Claude CLI row,
    /// which bills as plan usage and reports its own figure, and any
    /// gateway model nobody wrote a `[[models]]` row for. It is not zero:
    /// a turn on such a model is *unpriced*, and a consumer that summed it
    /// in as free would print a session total that was a floor and call it
    /// a bill.
    pub fn cost(&self, key: &str) -> Option<Cost> {
        let (provider, id) = key.split_once(':')?;
        self.providers
            .iter()
            .find(|p| p.def.name == provider)?
            .def
            .models
            .iter()
            .find(|m| m.id == id)?
            .cost
            .clone()
    }

    /// Every priced model's rates, keyed as [`Catalog::models`] keys them.
    /// A table for the same reason [`Catalog::windows`] is one: the status
    /// line that wants it lives on a thread with no catalog.
    pub fn costs(&self) -> std::collections::BTreeMap<String, Cost> {
        self.providers
            .iter()
            .flat_map(|p| {
                p.def.models.iter().filter_map(move |m| {
                    m.cost
                        .clone()
                        .map(|c| (format!("{}:{}", p.def.name, m.id), c))
                })
            })
            .collect()
    }

    /// The script that can read what the subscription behind `key`'s
    /// provider has left, when there is one — see [`crate::quota`].
    ///
    /// By model key and not by provider name because the consumer that
    /// asks holds the key in play, and the quota is the *account's*,
    /// shared by every model on it. Handed back as the script rather
    /// than as the reading, because reading it is a network round trip
    /// and the caller decides which thread pays for that. `None` is the
    /// ordinary answer: a metered provider, a TOML one, the Claude CLI,
    /// the mock — anything whose definition has no `quota()`.
    pub fn quota_source(&self, key: &str) -> Option<Arc<ProviderScript>> {
        let (provider, _) = key.split_once(':')?;
        self.providers
            .iter()
            .find(|p| p.def.name == provider)?
            .script
            .clone()
            .filter(|s| s.has_quota_hook())
    }

    /// Every model, heaviest first; within a weight, Claude CLI first and
    /// then providers in load order. The sort is stable, so the order a
    /// definition was written in survives underneath the operator's.
    pub fn models(&self) -> Vec<CatalogModel> {
        let mut out = Vec::new();
        if let Some(c) = &self.claude {
            let mut ids: Vec<String> = c
                .models
                .iter()
                .filter(|m| !m.contains('*'))
                .cloned()
                .collect();
            if ids.is_empty() {
                ids = ["sonnet", "opus", "haiku"].map(String::from).to_vec();
            }
            for id in ids {
                let key = format!("{CLAUDE_CLI}:{id}");
                let weight = self.weight(&key);
                // The CLI's models are Anthropic's, and every one of them sees.
                out.push(CatalogModel {
                    key,
                    provider: CLAUDE_CLI.into(),
                    id: id.clone(),
                    name: format!("{id} (Claude Code)"),
                    context: None,
                    reasoning: true,
                    vision: Some(true),
                    cost: None,
                    weight,
                });
            }
        }
        for p in &self.providers {
            for m in &p.def.models {
                let key = format!("{}:{}", p.def.name, m.id);
                out.push(CatalogModel {
                    weight: self.weight(&key),
                    key,
                    provider: p.def.name.clone(),
                    id: m.id.clone(),
                    name: m.label().to_string(),
                    context: m.context,
                    reasoning: m.reasoning,
                    vision: m.vision,
                    cost: m.cost.clone(),
                });
            }
        }
        out.sort_by_key(|m| -m.weight);
        out
    }

    /// Resolve a model string: `mock`, `provider:id`, or a bare id.
    ///
    /// A pooled provider resolves onto a *claimed* lane here — see
    /// [`Catalog::resolve_unclaimed`] for the same answer without that side
    /// effect.
    pub fn resolve(&self, spec: &str, provider_override: Option<&str>) -> anyhow::Result<Resolved> {
        self.resolve_inner(spec, provider_override, true)
    }

    /// Which backend a spec names, without taking anything: a pooled provider
    /// comes back as its shared entry (`claim: None`) rather than as a lane
    /// this call would hold for a session's lifetime.
    ///
    /// For a structural probe. A picker drawing its list, a health check, or
    /// a `readiness` answer must not acquire a scarce lane per ask — and a
    /// probe that claimed one would be a bug the operator meets as their
    /// sessions mysteriously spreading over the pool, or as a lane held by a
    /// process that never sends a byte. The two differ in *this* only: which
    /// lane, and whether anything is held afterwards.
    pub fn resolve_unclaimed(
        &self,
        spec: &str,
        provider_override: Option<&str>,
    ) -> anyhow::Result<Resolved> {
        self.resolve_inner(spec, provider_override, false)
    }

    fn resolve_inner(
        &self,
        spec: &str,
        provider_override: Option<&str>,
        claim: bool,
    ) -> anyhow::Result<Resolved> {
        let spec = spec.trim();
        if provider_override == Some("mock") || spec == "mock" {
            return Ok(Resolved::Mock);
        }
        let (provider, id) = match provider_override {
            Some(p) => (Some(p.to_string()), spec.to_string()),
            None => match spec.split_once(':') {
                Some((p, id)) if self.provider_named(p) => (Some(p.to_string()), id.to_string()),
                _ => (None, spec.to_string()),
            },
        };
        match provider.as_deref() {
            Some(CLAUDE_CLI) => {
                let c = self.claude.clone().unwrap_or(ClaudeCliEntry {
                    binary: None,
                    models: vec![],
                    tools: Default::default(),
                });
                Ok(Resolved::ClaudeCli {
                    model: if id.is_empty() { None } else { Some(id) },
                    binary: c.binary,
                    tools: c.tools,
                })
            }
            Some(name) => {
                let p = self
                    .providers
                    .iter()
                    .find(|p| p.def.name == name)
                    .with_context(|| format!("no provider named `{name}`"))?;
                let (provider, holding) = self.lane(p, claim);
                Ok(Resolved::Http {
                    provider,
                    model: id,
                    claim: holding,
                })
            }
            None => {
                if let Some(c) = &self.claude
                    && c.models
                        .iter()
                        .any(|g| harnox::llm::spec::glob_match(g, &id))
                {
                    return Ok(Resolved::ClaudeCli {
                        model: Some(id),
                        binary: c.binary.clone(),
                        tools: c.tools,
                    });
                }
                let exact: Vec<&ProviderEntry> = self
                    .providers
                    .iter()
                    .filter(|p| p.def.models.iter().any(|m| m.id == id))
                    .collect();
                match exact.len() {
                    1 => {
                        let (provider, holding) = self.lane(exact[0], claim);
                        return Ok(Resolved::Http {
                            provider,
                            model: id,
                            claim: holding,
                        });
                    }
                    n if n > 1 => bail!(
                        "`{id}` is served by {} providers; say which: {}",
                        n,
                        exact
                            .iter()
                            .map(|p| format!("{}:{id}", p.def.name))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    _ => {}
                }
                if let Some(p) = self.providers.iter().find(|p| {
                    p.def
                        .model_globs
                        .iter()
                        .any(|g| harnox::llm::spec::glob_match(g, &id))
                }) {
                    let (provider, holding) = self.lane(p, claim);
                    return Ok(Resolved::Http {
                        provider,
                        model: id,
                        claim: holding,
                    });
                }
                bail!("no provider serves `{id}`; run `eidolon models` to see what is configured")
            }
        }
    }

    fn provider_named(&self, name: &str) -> bool {
        name == CLAUDE_CLI || self.providers.iter().any(|p| p.def.name == name)
    }

    /// The provider for a resolution, on a lane of its token pool if it
    /// declares one.
    ///
    /// A pool changes the credential *per session*, and the shared entry
    /// cannot: its provider is one `Arc` for the process. So a pooled
    /// resolution builds its own [`HttpProvider`] over the def as claimed —
    /// `token_secret` set to the lane, the script re-pointed at that def so
    /// a hook reaching the network reaches it with the lane's key, exactly
    /// as a patch re-points one. A provider with no pool resolves to the
    /// shared entry, claim `None`, as ever.
    ///
    /// This is the whole claim point: resolution happens at session start,
    /// at `:model`, and at adoption, and the claim is sticky to the holder,
    /// so all three re-read one lane per session rather than churning.
    /// [`Self::claim_lane`] or the shared entry, depending on whether this
    /// resolution may take a lane at all — see [`Self::resolve_unclaimed`].
    fn lane(&self, entry: &ProviderEntry, claim: bool) -> (Arc<dyn Provider>, Option<ClaimedKey>) {
        if claim {
            self.claim_lane(entry)
        } else {
            (entry.provider.clone(), None)
        }
    }

    fn claim_lane(&self, entry: &ProviderEntry) -> (Arc<dyn Provider>, Option<ClaimedKey>) {
        let lanes = &entry.def.token_secrets;
        if lanes.is_empty() {
            return (entry.provider.clone(), None);
        }
        let (lane, fresh) = match &self.keypool {
            Some((root, holder)) => {
                eidolon_swarm::keypool::claim(root, &entry.def.name, lanes, holder)
            }
            // No holder to claim as: the first lane, shared. A listing
            // with no registry behind it must still resolve.
            None => (0, false),
        };
        let secret = lanes[lane.min(lanes.len() - 1)].clone();
        let mut claimed = entry.def.clone();
        claimed.token_secret = Some(secret.clone());
        let script = entry
            .script
            .as_ref()
            .map(|s| Arc::new(s.with_def(claimed.clone())));
        (
            Arc::new(HttpProvider::with_store(
                claimed,
                script,
                self.store.clone(),
            )) as Arc<dyn Provider>,
            Some(ClaimedKey {
                provider: entry.def.name.clone(),
                secret,
                fresh,
            }),
        )
    }

    /// Build the backend for `spec`, plus the qualified key to journal and
    /// the token-pool lane the resolution claimed, if any — the claim is
    /// the caller's to journal, because the session, not the catalog, is
    /// what a lane is attributed to.
    pub fn backend_for(
        &self,
        spec: &str,
        provider_override: Option<&str>,
        cwd: &Path,
        scratch: &Path,
    ) -> anyhow::Result<(Backend, String, Option<ClaimedKey>)> {
        Ok(match self.resolve(spec, provider_override)? {
            Resolved::Mock => (Backend::Provider(mock()), "mock".into(), None),
            Resolved::ClaudeCli {
                model,
                binary,
                tools,
            } => {
                let key = format!("{CLAUDE_CLI}:{}", model.clone().unwrap_or_default());
                (
                    Backend::Driver(eidolon_claude::driver(
                        binary,
                        model,
                        cwd.to_path_buf(),
                        scratch.to_path_buf(),
                        tools,
                    )?),
                    key,
                    None,
                )
            }
            Resolved::Http {
                provider,
                model,
                claim,
            } => {
                let key = format!("{}:{model}", provider.name());
                (Backend::Provider(provider), key, claim)
            }
        })
    }
}

/// The model id a backend should be told to use, given a journaled key.
pub fn model_id(key: &str) -> &str {
    key.split_once(':').map(|(_, id)| id).unwrap_or(key)
}

/// A provider that lists the working directory with `bash`, greps, then
/// summarises — enough to watch dispatch, policy and the log without a
/// network.
pub fn mock() -> Arc<dyn Provider> {
    use eidolon_core::testing::ScriptedProvider;
    ScriptedProvider::new(vec![
        ScriptedProvider::tool(
            "mock_1",
            "bash",
            serde_json::json!({ "command": "ls -1 | head -5" }),
        ),
        ScriptedProvider::tool(
            "mock_2",
            "grep",
            serde_json::json!({ "pattern": "fn main", "max": 3 }),
        ),
        ScriptedProvider::text(
            "Mock provider: I ran `ls` and a grep through the dispatcher. Pick a real model with :model, or space m.",
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> Catalog {
        let mut c = Catalog::new().with_claude(Some(ClaudeCliEntry {
            binary: None,
            models: vec!["claude-*".into(), "sonnet".into()],
            tools: Default::default(),
        }));
        let src = r#"pub fn provider() { #{ name: "gw", wire: "openai", base_url: "http://x", token: "t", models: [ #{ id: "gpt-5.4", name: "G" }, #{ id: "sonnet", name: "S via gw" } ] } }"#;
        let (def, script) = ProviderScript::load("gw", src).unwrap();
        c.add_def(def, Some(script), "test");
        c
    }

    /// The shipped providers compile, and a user's own file of the same
    /// name lands on top of one rather than beside it.
    #[test]
    fn builtins_load_and_are_replaceable() {
        let mut c = Catalog::new();
        c.load_builtins();
        assert!(c.load_errors.is_empty(), "{:?}", c.load_errors);
        assert!(
            c.providers()
                .iter()
                .any(|p| p.def.name == "deepseek" && p.source == "built-in")
        );
        assert!(
            matches!(c.resolve("deepseek:deepseek-v4-pro", None).unwrap(), Resolved::Http { model, .. } if model == "deepseek-v4-pro")
        );
        assert!(
            c.providers()
                .iter()
                .any(|p| p.def.name == "zai" && p.source == "built-in")
        );
        assert!(
            matches!(c.resolve("zai:glm-5.3", None).unwrap(), Resolved::Http { model, .. } if model == "glm-5.3")
        );
        assert!(
            c.providers()
                .iter()
                .any(|p| p.def.name == "ollama" && p.source == "built-in")
        );
        assert!(
            matches!(c.resolve("ollama:deepseek-v4.1-flash", None).unwrap(), Resolved::Http { model, .. } if model == "deepseek-v4.1-flash"),
            "the cloud rows resolve under their own name"
        );
        // The ids the ollama built-in shares with others name both providers
        // rather than silently picking one account to bill.
        let e = match c.resolve("deepseek-v4-pro", None) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a bare id served by two providers must not resolve"),
        };
        assert!(e.contains("served by 2 providers"), "{e}");
        // And its month is metered in credits whose *share* `/api/usage`
        // reads — the ceiling itself is not published on that wire.
        assert!(c.quota_source("ollama:deepseek-v4.1-flash").is_some());

        // The zai script's `request` hook runs, not just compiles: the
        // vision row's `max_tokens` comes down to what it accepts, and a
        // model with no cap is left alone.
        let zai = c.providers().iter().find(|p| p.def.name == "zai").unwrap();
        let script = zai.script.as_ref().unwrap();
        let clamped = script
            .rewrite(serde_json::json!({ "model": "glm-4.6v", "max_tokens": 64000 }))
            .unwrap();
        assert_eq!(clamped["max_tokens"], 32768);
        let under = script
            .rewrite(serde_json::json!({ "model": "glm-4.6v", "max_tokens": 4096 }))
            .unwrap();
        assert_eq!(under["max_tokens"], 4096);
        let untouched = script
            .rewrite(serde_json::json!({ "model": "glm-5.3", "max_tokens": 64000 }))
            .unwrap();
        assert_eq!(untouched["max_tokens"], 64000);

        let src = r#"pub fn provider() { #{ name: "deepseek", wire: "openai", base_url: "http://mine", token: "t", models: [ #{ id: "mine" } ] } }"#;
        let (def, script) = ProviderScript::load("deepseek", src).unwrap();
        c.add_def(def, Some(script), "test");
        let mine: Vec<&ProviderEntry> = c
            .providers()
            .iter()
            .filter(|p| p.def.name == "deepseek")
            .collect();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].def.base_url, "http://mine");
    }

    /// z.ai's built-in clamps `max_tokens` for the one model whose ceiling
    /// is below the harness's default, and leaves every other model's
    /// alone. Without the clamp `glm-4.6v` is a `400` on the first turn of
    /// any session that did not lower `max_tokens` by hand — a dead row in
    /// the picker wearing the look of a live one, and the kind of failure
    /// nobody meets until they pick the row.
    #[test]
    fn the_zai_hook_clamps_only_the_model_that_needs_it() {
        let (_, script) =
            ProviderScript::load("zai", BUILTINS.iter().find(|(n, _)| *n == "zai").unwrap().1)
                .unwrap();
        let capped = script
            .rewrite(serde_json::json!({ "model": "glm-4.6v", "max_tokens": 64000 }))
            .unwrap();
        assert_eq!(capped["max_tokens"], 32768);
        let untouched = script
            .rewrite(serde_json::json!({ "model": "glm-5.3", "max_tokens": 64000 }))
            .unwrap();
        assert_eq!(untouched["max_tokens"], 64000);
        // A cap is the endpoint's limit, not an opinion about length.
        let modest = script
            .rewrite(serde_json::json!({ "model": "glm-4.6v", "max_tokens": 4096 }))
            .unwrap();
        assert_eq!(modest["max_tokens"], 4096);
    }

    /// The z.ai built-in reads its plan's two windows out of the reply its
    /// endpoint actually sends (captured 2026-09-06), names them from the
    /// `(unit, number)` pair, and turns a reply with no reading in it —
    /// a key with no plan behind it — into the endpoint's own sentence.
    #[test]
    fn the_zai_reading_names_its_windows() {
        let (_, script) =
            ProviderScript::load("zai", BUILTINS.iter().find(|(n, _)| *n == "zai").unwrap().1)
                .unwrap();
        assert!(script.has_quota_hook());
        let read = |body: &str| -> anyhow::Result<crate::quota::Quota> {
            let reply: crate::quota::QuotaReply =
                serde_json::from_value(script.call("read_quota", serde_json::json!(body)).unwrap())
                    .unwrap();
            reply.into_result()
        };
        let body = r#"{"code":200,"msg":"Operation successful","data":{"limits":[
            {"type":"CREDIT_LIMIT","unit":3,"number":5,"usage":2000,"currentValue":158,"remaining":1841,"percentage":7,"nextResetTime":1788693968953},
            {"type":"CREDIT_LIMIT","unit":6,"number":1,"usage":10000,"currentValue":158,"remaining":9841,"percentage":1,"nextResetTime":1789280653997},
            {"type":"TIME_LIMIT","unit":5,"number":1,"usage":100,"currentValue":0,"remaining":100,"percentage":0,"nextResetTime":1791280653997}
        ],"level":"lite"},"success":true}"#;
        let q = read(body).unwrap();
        assert_eq!(q.plan.as_deref(), Some("lite"));
        let rows: Vec<_> = q
            .windows
            .iter()
            .map(|w| (w.label.as_str(), w.percent, w.used, w.limit, w.resets_ms))
            .collect();
        assert_eq!(
            rows,
            [
                ("5h", 7.0, Some(158), Some(2000), Some(1788693968953)),
                ("week", 1.0, Some(158), Some(10000), Some(1789280653997))
            ],
            "the two credit windows, and not the web-search count"
        );

        // Other spellings of a window come out as words too.
        let body = r#"{"data":{"limits":[
            {"type":"TOKENS_LIMIT","unit":4,"number":1,"percentage":50},
            {"type":"CREDIT_LIMIT","unit":6,"number":2,"percentage":12.5},
            {"type":"CREDIT_LIMIT","unit":5,"number":1,"percentage":3},
            {"type":"CREDIT_LIMIT","unit":9,"number":2,"percentage":0}
        ]}}"#;
        let q = read(body).unwrap();
        assert_eq!(q.plan, None);
        assert_eq!(
            q.windows
                .iter()
                .map(|w| (w.label.as_str(), w.percent))
                .collect::<Vec<_>>(),
            [
                ("day", 50.0),
                ("2w", 12.5),
                ("month", 3.0),
                ("2\u{d7}unit9", 0.0)
            ]
        );
        assert_eq!(
            q.windows[0].used, None,
            "a count the endpoint left out is absent"
        );

        // A key with no plan behind it: the sentence the endpoint sends.
        let e =
            read(r#"{"code":500,"msg":"Your account has no active coding plan","success":false}"#)
                .unwrap_err();
        assert_eq!(e.to_string(), "Your account has no active coding plan");
        assert!(read("<html>").unwrap_err().to_string().contains("not JSON"));
        assert!(
            read(r#"{"data":{"level":"lite"}}"#)
                .unwrap_err()
                .to_string()
                .contains("no limits")
        );

        // And the catalog says which key has one to read.
        let mut c = Catalog::new();
        c.load_builtins();
        assert!(c.quota_source("zai:glm-5.3").is_some());
        assert!(
            c.quota_source("deepseek:deepseek-v4-pro").is_none(),
            "metered: nothing to read"
        );
        assert!(c.quota_source("mock").is_none());
        assert!(c.quota_source("claude-cli:sonnet").is_none());
    }

    /// The antigravity built-in reads its agent model list and its binding
    /// quota window out of the reply the native surface actually sends
    /// (`fetchAvailableModels`, captured 2026-09-09): the model rows that
    /// are tab/image/deprecated drop out, the rest map with context and
    /// reasoning, and quota reports the row with the smallest remaining
    /// fraction — the one that would refuse next — as one window.
    #[test]
    fn the_antigravity_reading_names_its_binding_model() {
        // First: does the idiom the script relies on — iterating an
        // object's values — resolve in this script context at all?
        let src = r#"pub fn provider() { #{ name: "objtest", wire: "openai", base_url: "http://x" } }
pub fn probe(body) {
    let reply = match json::from_string(body) { Ok(v) => v, Err(_) => return "no json" };
    let arr = match reply.get("arr") { Some(a) => a, None => return "no arr" };
    let arrstr = 0;
    for row in arr {
        let id_obj = match row.get("model") {
            Some(id) => id,
            None => continue,
        };
        if id_obj is String {
            arrstr += 1;
        }
    }
    return arrstr;
}"#;
        let (_, probe) = ProviderScript::load("objtest", src).unwrap();
        let got = probe
            .call("probe", serde_json::json!(r#"{"arr":[{"model":"a"},{"model":"b"},{}]}"#))
            .unwrap();
        assert_eq!(got, serde_json::json!(2), "Some-unwrapped get + is String works: {got}");
        let (_, script) =
            ProviderScript::load("antigravity", BUILTINS.iter().find(|(n, _)| *n == "antigravity").unwrap().1)
                .unwrap();
        let call = |fn_name: &str, body: &str| -> serde_json::Value {
            script.call(fn_name, serde_json::json!(body)).unwrap()
        };
        let body = r#"{
            "models": {
                "gemini-3.8-flash-high": {"model":"gemini-3.8-flash-high","supportsThinking":true,"maxTokens":1048576,"quotaInfo":{"resetTime":"2026-09-09T09:16:19Z"}},
                "gemini-2.5-flash": {"model":"gemini-2.5-flash","maxTokens":1048576,"quotaInfo":{"resetTime":"2026-09-09T09:16:19Z"}},
                "gemini-3.1-pro-high": {"model":"gemini-3.1-pro-high","supportsThinking":true,"maxTokens":1048576,"quotaInfo":{"resetTime":"2026-09-09T09:16:19Z"}},
                "tab_flash_lite_preview": {"model":"tab_flash_lite_preview","supportsThinking":true,"quotaInfo":{"remainingFraction":1}},
                "some-image-gen": {"model":"some-image-gen","supportsThinking":false,"maxTokens":1,"quotaInfo":{"remainingFraction":1}},
                "claude-sonnet-4-6": {"model":"claude-sonnet-4-6","supportsThinking":true,"maxTokens":250000,"quotaInfo":{"remainingFraction":0.25,"resetTime":"2026-09-09T11:42:28Z"}},
                "gpt-oss-120b-medium": {"model":"gpt-oss-120b-medium","supportsThinking":true,"maxTokens":131072,"quotaInfo":{"remainingFraction":1,"resetTime":"2026-09-09T11:42:28Z"}}
            },
            "tabModelIds": ["tab_flash_lite_preview"],
            "imageGenerationModelIds": ["some-image-gen"],
            "deprecatedModelIds": {"gemini-3.1-pro-high": {"newModelId": "gemini-pro-agent"}}
        }"#;

        let models: Vec<serde_json::Value> =
            serde_json::from_value(call("read_models", body)).unwrap();
        let ids: Vec<&str> = models
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert!(
            ids.contains(&"gemini-3.8-flash-high")
                && ids.contains(&"claude-sonnet-4-6")
                && ids.contains(&"gpt-oss-120b-medium"),
            "agent rows survive: {ids:?}"
        );
        assert!(
            !ids.contains(&"gemini-3.1-pro-high")
                && !ids.contains(&"tab_flash_lite_preview")
                && !ids.contains(&"some-image-gen"),
            "deprecated, tab and image rows drop: {ids:?}"
        );
        let gpt = models
            .iter()
            .find(|m| m["id"] == "gpt-oss-120b-medium")
            .unwrap();
        assert_eq!(gpt["context"], 131072);
        assert_eq!(gpt["reasoning"], true);

        let q: crate::quota::QuotaReply =
            serde_json::from_value(call("read_quota", body)).unwrap();
        let q = q.into_result().unwrap();
        assert_eq!(q.windows.len(), 1, "the binding row, not every row");
        let w = &q.windows[0];
        assert_eq!(w.label, "claude-sonnet-4-6", "the row with the least remaining");
        assert_eq!(w.percent, 75.0, "0.25 remaining is 75% used");
        assert_eq!(w.resets_ms, Some(1788954148000));
    }

    /// The deepseek built-in maps the listing DeepSeek's documented example
    /// shows (captured 2026-09-10) to bare ids — the reply carries no rates
    /// or context, so a row only the listing knows claims nothing it did not
    /// say — while the declared rows price the ids they know, at the peak
    /// rates the file documents for the V4.1 Flash rename.
    #[test]
    fn the_deepseek_listing_names_what_it_serves() {
        let (_, script) = ProviderScript::load(
            "deepseek",
            BUILTINS.iter().find(|(n, _)| *n == "deepseek").unwrap().1,
        )
        .unwrap();
        let body = r#"{"object":"list","data":[
            {"id":"deepseek-flash","object":"model","owned_by":"deepseek"},
            {"id":"deepseek-v4-pro","object":"model","owned_by":"deepseek"}
        ]}"#;
        let rows: Vec<crate::def::ModelDef> = serde_json::from_value(
            script.call("read_models", serde_json::json!(body)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            rows.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["deepseek-flash", "deepseek-v4-pro"]
        );
        assert!(
            rows.iter().all(|m| m.cost.is_none() && m.context.is_none() && m.name.is_none()),
            "a discovered row is the id and nothing else"
        );

        // A down gateway or a changed shape is an empty list, never a
        // failed load — the declared rows stand in.
        for bad in ["", "not json", r#"{"nope":1}"#, r#"{"data":{}}"#, r#"{"data":[{},{"id":1}]}"#] {
            let v = script.call("read_models", serde_json::json!(bad)).unwrap();
            assert_eq!(v, serde_json::json!([]), "`{bad}` must map to no rows");
        }

        // The declared rows are the priced truth the listing cannot supply.
        let mut c = Catalog::new();
        c.load_builtins();
        let flash = c
            .models()
            .into_iter()
            .find(|m| m.key == "deepseek:deepseek-flash")
            .expect("the renamed Flash is a picker row");
        let cost = flash.cost.unwrap();
        assert!(
            (cost.input - 0.3).abs() < 1e-9
                && (cost.output - 1.2).abs() < 1e-9
                && (cost.cache_read - 0.006).abs() < 1e-9
        );
        assert_eq!(flash.context, Some(1000000));
        assert!(
            matches!(c.resolve("deepseek:deepseek-flash", None).unwrap(), Resolved::Http { model, .. } if model == "deepseek-flash"),
            "the new id resolves without touching the file again"
        );
    }

    /// A patch changes what it names and inherits the rest — including
    /// the script's hooks and the models it discovered — so moving a
    /// built-in's key to a file cannot silently freeze its prices.
    #[test]
    fn a_patch_changes_one_field_and_keeps_the_rest() {
        let mut c = Catalog::new();
        c.load_builtins();
        let before = c
            .providers()
            .iter()
            .find(|p| p.def.name == "deepseek")
            .unwrap()
            .def
            .clone();
        assert_eq!(before.token_secret.as_deref(), Some("deepseek"));

        let toml: toml::Value = r#"name = "deepseek"
token_file = "/run/secrets/deepseek""#
            .parse()
            .unwrap();
        let patch = crate::def::ProviderPatch::from_toml(&toml).unwrap();
        c.patch(patch, "config.toml").unwrap();

        let after = c
            .providers()
            .iter()
            .find(|p| p.def.name == "deepseek")
            .unwrap();
        assert_eq!(
            after.def.token_file.as_deref(),
            Some(std::path::Path::new("/run/secrets/deepseek"))
        );
        // Naming one credential replaces all of them: where the key lives
        // is a single decision, not four half-answered ones.
        assert_eq!(after.def.token_secret, None);
        assert_eq!(after.def.token_env, None);
        // And nothing else moved.
        assert_eq!(after.def.wire, before.wire);
        assert_eq!(after.def.base_url, before.base_url);
        assert_eq!(after.def.models, before.models);
        assert!(
            after.script.is_some(),
            "the script's hooks survive the patch"
        );
        assert!(
            after.source.contains("built-in") && after.source.contains("config.toml"),
            "{}",
            after.source
        );

        // And a hook that reaches the network reaches it with the
        // patch's key: `quota()` here reports what `http_get` said, and
        // what it says is that the *patched* token file could not be
        // read — not that the store the built-in named was empty, and
        // not a connection refused on the declared token.
        let src = r#"
pub fn provider() { #{ name: "acct", wire: "openai", base_url: "http://127.0.0.1:1", token: "declared", models: [ #{ id: "m" } ] } }
pub fn quota() { match eidolon::http_get("/quota") { Ok(_) => #{ windows: [] }, Err(e) => #{ error: e } } }
"#;
        let (def, script) = ProviderScript::load("acct", src).unwrap();
        c.add_def(def, Some(script), "test");
        let patch: toml::Value = r#"name = "acct"
token_file = "/nowhere/patched.token""#
            .parse()
            .unwrap();
        c.patch(
            crate::def::ProviderPatch::from_toml(&patch).unwrap(),
            "config.toml",
        )
        .unwrap();
        let e = c
            .quota_source("acct:m")
            .expect("the hook survives")
            .quota()
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("patched.token"),
            "the hook runs on the patched definition: {e}"
        );

        // A table naming nothing loaded is a whole definition, and an
        // incomplete one says which field it is missing.
        let orphan: toml::Value = r#"name = "nope"
token_file = "/x""#
            .parse()
            .unwrap();
        assert!(
            c.patch(
                crate::def::ProviderPatch::from_toml(&orphan).unwrap(),
                "config.toml"
            )
            .is_err()
        );
        let e = ProviderDef::from_toml(&orphan).unwrap_err().to_string();
        assert!(e.contains("no `wire`"), "{e}");
    }

    /// A patch that moves the key re-runs the listing hook under it.
    /// `models()` ran at load against the definition `provider()` returned —
    /// here a secret nothing holds and nothing cached, so the fetch had no
    /// answer and only the declared row survived. The patch points the
    /// script at a token file; the listing is read again with that
    /// credential, from the cache a live fetch would have left behind
    /// (primed between the two, so the test never touches the network). A
    /// patch that lists models itself is an answer, and no discovery runs to
    /// un-answer it.
    #[test]
    fn a_patch_rediscovers_the_models_the_new_credential_serves() {
        let url = "http://127.0.0.1:1/models";

        let token = std::env::temp_dir().join("eidolon-moved-key.token");
        std::fs::write(&token, "a-test-key-with-no-account").unwrap();

        let src = r#"
pub fn provider() {
    #{ name: "moved-key-gw", wire: "openai", base_url: "http://127.0.0.1:1",
       token_secret: "nothing-holds-this", models: [ #{ id: "declared" } ] }
}
pub fn models() {
    match eidolon::http_get("/models") {
        Ok(b) => match json::from_string(b) { Ok(v) => v, Err(_) => [] },
        Err(_) => [],
    }
}
"#;
        let (def, script) = ProviderScript::load("moved-key-gw", src).unwrap();
        assert_eq!(
            def.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["declared"],
            "nothing cached and no credential: the floor stood in"
        );

        // What a live fetch would have left for the launch after it.
        crate::script::test_cache::prime("moved-key-gw", url, r#"[{ "id": "discovered" }]"#);
        let cleanup = crate::script::test_cache::path_of("moved-key-gw", url);

        let mut c = Catalog::new();
        c.add_def(def, Some(script), "test");
        let toml: toml::Value = format!(
            "name = \"moved-key-gw\"\ntoken_file = \"{}\"",
            token.display()
        )
        .parse()
        .unwrap();
        c.patch(crate::def::ProviderPatch::from_toml(&toml).unwrap(), "config.toml")
            .unwrap();
        let ids: Vec<String> = c
            .providers()
            .iter()
            .find(|p| p.def.name == "moved-key-gw")
            .unwrap()
            .def
            .models
            .iter()
            .map(|m| m.id.clone())
            .collect();
        assert_eq!(
            ids,
            ["declared".to_string(), "discovered".to_string()],
            "the listing is read again under the patched credential"
        );

        // A patch that answers "which models" itself is not rediscovered
        // over: replace means replace.
        let toml: toml::Value = r#"name = "moved-key-gw"
models = ["from-patch-*"]"#
            .parse()
            .unwrap();
        c.patch(crate::def::ProviderPatch::from_toml(&toml).unwrap(), "config.toml")
            .unwrap();
        let after = c
            .providers()
            .iter()
            .find(|p| p.def.name == "moved-key-gw")
            .unwrap();
        assert!(after.def.models.is_empty() && after.def.model_globs == ["from-patch-*"]);

        if let Some(p) = cleanup {
            let _ = std::fs::remove_file(p);
        }
        let _ = std::fs::remove_file(&token);
    }

    /// Weights order the listing and nothing else: an exact key beats a
    /// glob, the longest glob beats a shorter one, and models of equal
    /// weight keep the order their definitions gave them.
    #[test]
    fn weights_order_the_listing() {
        let c = cat();
        let plain: Vec<String> = c.models().into_iter().map(|m| m.key).collect();
        assert_eq!(plain, ["claude-cli:sonnet", "gw:gpt-5.4", "gw:sonnet"]);

        c.set_weights([("gw:*".to_string(), -3), ("gw:gpt-5.4".to_string(), 5)].into());
        assert_eq!(c.weight("gw:gpt-5.4"), 5, "an exact entry beats a glob");
        assert_eq!(c.weight("gw:sonnet"), -3);
        assert_eq!(c.weight("claude-cli:sonnet"), 0);
        let sorted: Vec<String> = c.models().into_iter().map(|m| m.key).collect();
        assert_eq!(sorted, ["gw:gpt-5.4", "claude-cli:sonnet", "gw:sonnet"]);

        // A longer glob is the more specific answer.
        c.set_weights([("gw:*".to_string(), -3), ("gw:g*".to_string(), 2)].into());
        assert_eq!(c.weight("gw:gpt-5.4"), 2);
        assert_eq!(c.weight("gw:sonnet"), -3);

        // The whole table is replaced at load and never a row at a time:
        // an ordering is declared, not nudged.
        c.set_weights([("gw:sonnet".to_string(), 4)].into());
        assert_eq!(c.models()[0].key, "gw:sonnet");
        assert_eq!(c.models()[0].weight, 4);
        assert_eq!(
            c.weight("gw:gpt-5.4"),
            0,
            "what the new table does not name is unweighted"
        );
    }

    #[test]
    fn resolution_rules() {
        let c = cat();
        assert!(matches!(c.resolve("mock", None).unwrap(), Resolved::Mock));
        assert!(
            matches!(c.resolve("sonnet", None).unwrap(), Resolved::ClaudeCli { model: Some(m), .. } if m == "sonnet")
        );
        assert!(
            matches!(c.resolve("gw:sonnet", None).unwrap(), Resolved::Http { model, .. } if model == "sonnet")
        );
        assert!(
            matches!(c.resolve("gpt-5.4", None).unwrap(), Resolved::Http { model, .. } if model == "gpt-5.4")
        );
        assert!(matches!(
            c.resolve("claude-cli:", None).unwrap(),
            Resolved::ClaudeCli { model: None, .. }
        ));
        assert!(c.resolve("nope", None).is_err());
        let keys: Vec<String> = c.models().into_iter().map(|m| m.key).collect();
        assert_eq!(keys, ["claude-cli:sonnet", "gw:gpt-5.4", "gw:sonnet"]);
    }

    /// A pooled provider resolves each session onto its own lane: two
    /// catalogs over one registry take two different keys, and each
    /// resolution says which — the fact a session journals for attribution.
    #[test]
    fn a_pool_gives_each_session_its_own_lane() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let src = r#"pub fn provider() { #{ name: "pool-gw", wire: "openai", base_url: "http://x", token: "inline", token_secrets: ["pool-gw", "pool-gw2", "pool-gw3"], models: [ #{ id: "m" } ] } }"#;
        let mut a = Catalog::new();
        let (def, script) = ProviderScript::load("pool-gw", src).unwrap();
        a.add_def(def, Some(script), "test");
        let mut b = Catalog::new();
        let (def, script) = ProviderScript::load("pool-gw", src).unwrap();
        b.add_def(def, Some(script), "test");
        // No registry to claim through: both share the first lane, and it
        // is not an error — a listing resolves the same provider.
        for c in [&a, &b] {
            let Resolved::Http { claim, .. } = c.resolve("pool-gw:m", None).unwrap() else {
                panic!("resolves")
            };
            assert_eq!(
                claim,
                Some(ClaimedKey { provider: "pool-gw".into(), secret: "pool-gw".into(), fresh: false })
            );
        }

        // With holders, the two sessions take two lanes — first free, in
        // declaration order.
        let pa = eidolon_swarm::Presence::register(&root, &tmp.path().join("a.eid"), &root, "mock", "a")
            .unwrap();
        let pb = eidolon_swarm::Presence::register(&root, &tmp.path().join("b.eid"), &root, "mock", "b")
            .unwrap();
        let a = a.with_keypool(root.clone(), pa.id());
        let b = b.with_keypool(root.clone(), pb.id());
        let lane = |c: &Catalog| match c.resolve("pool-gw:m", None).unwrap() {
            Resolved::Http { claim: Some(k), .. } => k.secret,
            _ => panic!("a pooled resolution claims"),
        };
        assert_eq!(lane(&a), "pool-gw");
        assert_eq!(lane(&b), "pool-gw2");
        // Sticky: re-resolving — a model switch back, a judge — reads the
        // same lane and is not fresh, so attribution journals once.
        let Resolved::Http { claim: Some(k), .. } = a.resolve("pool-gw:m", None).unwrap() else {
            panic!("resolves")
        };
        assert_eq!(k.secret, "pool-gw");
        assert!(!k.fresh);
    }

    /// A probe resolves *without* taking a lane. The catalogue answers
    /// "which key would this use" many times per frame in a picker and once
    /// per health check, and a resolution that claimed a lane for each would
    /// spread a session's keys over the pool and hold lanes for processes
    /// that never send a request — the scarce resource this feature exists
    /// to ration.
    #[test]
    fn a_probe_resolves_without_taking_a_lane() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let src = r#"pub fn provider() { #{ name: "pool-gw", wire: "openai", base_url: "http://x", token: "inline", token_secrets: ["pool-gw", "pool-gw2"], models: [ #{ id: "m" } ] } }"#;
        let mut c = Catalog::new();
        let (def, script) = ProviderScript::load("pool-gw", src).unwrap();
        c.add_def(def, Some(script), "test");
        let pa = eidolon_swarm::Presence::register(&root, &tmp.path().join("a.eid"), &root, "mock", "a")
            .unwrap();
        let c = c.with_keypool(root.clone(), pa.id());
        match c.resolve_unclaimed("pool-gw:m", None).unwrap() {
            Resolved::Http { provider, claim, .. } => {
                assert_eq!(claim, None, "a probe claims nothing");
                assert_eq!(provider.name(), "pool-gw", "and still names the provider");
            }
            _ => panic!("resolves"),
        }

        // And it held nothing: a *different* holder resolving afterwards
        // still finds the first lane free. A claim-free resolution that had
        // quietly taken one would leave this on `pool-gw2`.
        let pb = eidolon_swarm::Presence::register(&root, &tmp.path().join("b.eid"), &root, "mock", "b")
            .unwrap();
        let b = Catalog::new();
        let (def, script) = ProviderScript::load("pool-gw", src).unwrap();
        let mut b = b.with_keypool(root.clone(), pb.id());
        b.add_def(def, Some(script), "test");
        let Resolved::Http { claim: Some(k), .. } = b.resolve("pool-gw:m", None).unwrap() else {
            panic!("a real resolution claims")
        };
        assert_eq!(k.secret, "pool-gw", "the first lane was never taken");
    }

    /// A patch can install the pool over a built-in in one line — that is
    /// the entire operator surface for the feature — and it replaces the
    /// single-credential decision wholesale.
    #[test]
    fn a_patch_installs_a_token_pool() {
        let mut c = Catalog::new();
        c.load_builtins();
        let toml: toml::Value = r#"name = "ollama"
token_secrets = ["ollama", "ollama2"]"#
            .parse()
            .unwrap();
        c.patch(crate::def::ProviderPatch::from_toml(&toml).unwrap(), "config.toml")
            .unwrap();
        let def = c
            .providers()
            .iter()
            .find(|p| p.def.name == "ollama")
            .unwrap();
        assert_eq!(
            def.def.token_secrets,
            ["ollama".to_string(), "ollama2".to_string()],
            "the pool arrives"
        );
        assert_eq!(
            def.def.token_secret, None,
            "and replaces the single secret the built-in named"
        );
        assert_eq!(def.def.token_env, None, "and the env fallback with it");
    }
}
