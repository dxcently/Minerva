//! The provider record, as a script or a TOML entry produces it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use harnox::llm::message::Usage;
pub use harnox::llm::{AuthStyle, Wire};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Cost {
    /// Dollars per million tokens.
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
}

impl Cost {
    /// What `usage` cost at these rates, in dollars.
    ///
    /// Four rates for the four things an endpoint charges for — see
    /// `eidolon_core::usage` for why input is three fields. A cache rate
    /// the definition left at zero is priced as fresh input: those tokens
    /// *were* read, and "nobody said what a cache read costs here" must
    /// not price them as free. For a read that is an upper bound, since a
    /// cache read is a discount everywhere it exists; for a write it is
    /// close (Anthropic bills 1.25×). A definition that knows better says
    /// so, as the built-in DeepSeek one does.
    pub fn price(&self, usage: &Usage) -> f64 {
        let or_input = |rate: f64| if rate > 0.0 { rate } else { self.input };
        let per = |tokens: u64, rate: f64| tokens as f64 * rate / 1_000_000.0;
        per(usage.input_tokens, self.input)
            + per(usage.output_tokens, self.output)
            + per(usage.cache_read_input_tokens, or_input(self.cache_read))
            + per(
                usage.cache_creation_input_tokens,
                or_input(self.cache_write),
            )
    }
}

#[cfg(test)]
mod cost_tests {
    use super::*;

    /// DeepSeek's own rates, and the cache-warm turn that motivated the
    /// caching work: nearly all of the input read from the cache.
    #[test]
    fn a_cache_warm_turn_is_priced_at_the_cache_rate() {
        let c = Cost {
            input: 0.28,
            output: 0.42,
            cache_read: 0.028,
            cache_write: 0.0,
        };
        let u = Usage {
            input_tokens: 1_000,
            output_tokens: 2_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 100_000,
        };
        let fresh = 1_000.0 * 0.28 / 1e6;
        let out = 2_000.0 * 0.42 / 1e6;
        let cached = 100_000.0 * 0.028 / 1e6;
        assert!((c.price(&u) - (fresh + out + cached)).abs() < 1e-12);
        // Priced as if fresh, the same turn would cost ten times more on
        // its input — which is the figure the TUI used to imply.
        assert!(c.price(&u) < fresh + out + 100_000.0 * 0.28 / 1e6);
    }

    #[test]
    fn an_unset_cache_rate_is_the_input_rate_and_never_free() {
        let c = Cost {
            input: 1.0,
            output: 2.0,
            ..Default::default()
        };
        let u = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 500_000,
            cache_read_input_tokens: 500_000,
        };
        assert!(
            (c.price(&u) - 1.0).abs() < 1e-12,
            "a million cached tokens at the input rate"
        );
    }

    #[test]
    fn nothing_read_costs_nothing() {
        assert_eq!(Cost::default().price(&Usage::default()), 0.0);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelDef {
    pub id: String,
    /// Display name; defaults to the id.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub context: Option<u64>,
    #[serde(default)]
    pub reasoning: bool,
    /// Whether this model can be shown an image.
    ///
    /// Three-valued, and absent is the interesting value. `false` is a
    /// definition that positively says this model is blind, and image
    /// blocks are turned into a line of text on the way to the wire rather
    /// than sent to be rejected. Absent means *nobody said* — which is the
    /// ordinary case for a gateway's discovered list — and the image is
    /// sent, because guessing "blind" would silently drop a screenshot the
    /// model could have read, and that looks exactly like a model ignoring
    /// it. Say `vision = false` only where it is known.
    #[serde(default)]
    pub vision: Option<bool>,
    /// Sampling temperature for this model, where the operator names one.
    ///
    /// Absent means *nobody said* and sends no field, leaving the
    /// endpoint's own default — which is 1.0 almost everywhere, and is the
    /// reason this exists. A small model at 1.0 decides between emitting a
    /// tool call and typing the tool's name by a coin flip, and will do
    /// both across two runs of the same prompt; naming 0.2 here is what
    /// makes such a model usable as an agent rather than as a chatbot.
    ///
    /// A model listed here overrides [`ProviderDef::temperature`]. Both are
    /// needed, because a gateway's models are usually *discovered* rather
    /// than listed, and a per-model knob alone would never reach them.
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub cost: Option<Cost>,
}

impl ModelDef {
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

/// Knobs for endpoints that speak the wire imperfectly. Every flag is
/// additive over harnox's default body; a script's `request(body)` hook is
/// the escape hatch for anything not here.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Compat {
    /// Send `max_completion_tokens` instead of `max_tokens` (newer OpenAI
    /// models reject the old name).
    #[serde(default)]
    pub max_completion_tokens: bool,
    /// Include `stream_options: { include_usage }`. Some proxies choke on it.
    #[serde(default = "yes")]
    pub stream_options: bool,
    /// Send `reasoning_effort` for reasoning models. Off for gateways that
    /// reject unknown fields.
    #[serde(default)]
    pub reasoning_effort: bool,
    /// Mark the request's stable prefix with Anthropic-shaped
    /// `cache_control` blocks.
    ///
    /// The chat-completions wire has no prompt caching of its own: an
    /// endpoint that caches does it unasked, on the request prefix, which
    /// is why the harness's side of that bargain is a byte-identical
    /// prefix rather than a field. But a gateway fronting *Anthropic*
    /// models over this wire (OpenRouter and its imitators) passes the
    /// Anthropic field through, and without it those models cache
    /// nothing at all — the one case where the OpenAI wire has to say
    /// something to get what the Anthropic wire gets for free.
    ///
    /// Off by default, because it is a claim about one endpoint and the
    /// endpoints that do not know the field answer `400` rather than
    /// ignoring it.
    #[serde(default)]
    pub cache_control: bool,
    /// Send `prompt_cache_key`, and `prompt_cache_retention` when the
    /// session asks for long retention.
    ///
    /// Where a cache is sharded across machines, this is what routes a
    /// request back to the one holding the entry its predecessor wrote —
    /// a stable prefix still misses whenever the load balancer chooses
    /// differently. Off by default for the same reason as above: it is an
    /// OpenAI field, and a gateway that has never heard of it refuses the
    /// request rather than dropping it.
    #[serde(default)]
    pub prompt_cache_key: bool,
    /// Send each assistant message's thinking back as `reasoning_content`.
    ///
    /// harnox's chat-completions body leaves thinking out — the wire has
    /// no signature to verify and OpenAI's own endpoint has no field for
    /// it — but the endpoints that *emit* `reasoning_content` want it
    /// returned. z.ai's preserved thinking, on by default on the Coding
    /// Plan surface, says the complete, unmodified reasoning must come
    /// back together with the tool results or "performance may degrade";
    /// DeepSeek says the same for any request that carries tools. Without
    /// it every continuation call in a tool loop hands the model a
    /// transcript with its own reasoning cut out.
    ///
    /// Off by default for the reason the other fields are: it is a claim
    /// about one endpoint, and one that has never heard of the field may
    /// refuse the request rather than drop it.
    #[serde(default)]
    pub reasoning_content: bool,
    /// Recover tool calls the endpoint left in the assistant's text.
    ///
    /// litellm's ollama path renders a Mistral-family model's tool calls
    /// with the model's own chat template and then parses only some of them
    /// back out; the rest arrive as content in the shape
    /// `name[ARGS]{json}`, sometimes behind a `[TOOL_CALLS]` marker. Without
    /// this the calls are silently lost — a turn that writes three files
    /// writes two. See `TextToolCalls`.
    #[serde(default)]
    pub tool_calls_in_text: bool,
}

fn yes() -> bool {
    true
}

impl Default for Compat {
    fn default() -> Self {
        Compat {
            max_completion_tokens: false,
            stream_options: true,
            reasoning_effort: false,
            cache_control: false,
            prompt_cache_key: false,
            reasoning_content: false,
            tool_calls_in_text: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProviderDef {
    pub name: String,
    pub wire: Wire,
    pub base_url: String,
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    #[serde(default)]
    pub token_env: Option<String>,
    /// An inline token. For placeholders only (`"ollama"`); a real secret
    /// belongs in the store or a `0600` file.
    #[serde(default)]
    pub token: Option<String>,
    /// The name of an entry in the custodied secret store (`eidolon secret
    /// set NAME`). Preferred for real keys: the value is injected into the
    /// request by Rust and never exists as a string a script can see.
    #[serde(default)]
    pub token_secret: Option<String>,
    /// Several names in the secret store, one *lane* of a token pool.
    ///
    /// For an account whose endpoint issues more than one key and scopes
    /// its caches per key: ollama.com's prefix cache is the measured case,
    /// where two sessions sharing a key evict each other's warm prefixes
    /// and the same two on two keys never contend. Non-empty replaces the
    /// single-credential question ("what is *the* key") with a claim: at
    /// backend build the harness takes the first lane no live session
    /// holds — over the presence registry, an atomic directory per lane —
    /// and holds it for the session's lifetime; see
    /// [`eidolon_swarm::keypool`]. The claimed lane lands in
    /// `token_secret` on the def the backend actually uses, which is why
    /// the ordinary `token_secret` path serves both.
    ///
    /// Nothing to coordinate through (no registry, or a catalog that was
    /// not given a holder — `eidolon models` listing, say) means the
    /// first lane, so a pool degrades to one shared key rather than to an
    /// error. Two trades are accepted out loud: more concurrent sessions
    /// than lanes share the first lane and its eviction regime; and two
    /// sessions with similar large prefixes on different lanes each pay
    /// the cold-start admission the prefix would have shared on one.
    #[serde(default)]
    pub token_secrets: Vec<String>,
    #[serde(default)]
    pub auth: AuthStyle,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub compat: Compat,
    #[serde(default)]
    pub models: Vec<ModelDef>,
    /// Model-id globs this provider also serves (the TOML form's
    /// `models = ["claude-*"]`), for ids not listed explicitly.
    #[serde(default)]
    pub model_globs: Vec<String>,
    /// The sampling temperature for every model of this provider that does
    /// not name its own ([`ModelDef::temperature`]).
    ///
    /// Provider-wide as well as per-model because of how gateways actually
    /// work: `models()` discovers a hundred ids and lists none of them in
    /// the definition, so there is no `[[models]]` row to hang a
    /// temperature on. One line on the provider is what reaches them.
    #[serde(default)]
    pub temperature: Option<f64>,
}

impl ProviderDef {
    /// Read the credential now. `None` for an endpoint with no auth. Order:
    /// secret store, file, env, inline. Store reads are logged as
    /// `(name, consumer)` — never the value.
    ///
    /// A named `token_secret` the store does not hold is not fatal while
    /// another source is declared: the store is the *preferred* home for a
    /// key, not the only one, and a definition that names both (the built-in
    /// `deepseek` names `DEEPSEEK_API_KEY` beside its secret) should reach
    /// whichever the operator actually filled in. With nothing else
    /// declared, the error still names the secret and the command that
    /// creates it.
    ///
    /// A token *pool* rides the same path: the backend built from a
    /// claimed lane carries that lane here as its `token_secret`, and a
    /// pool def nobody claimed (a catalog with no holder, a script hook on
    /// the shared entry) resolves its first lane — one shared key rather
    /// than an error, see [`ProviderDef::token_secrets`].
    pub fn token(
        &self,
        store: Option<&harnox::secrets::SecretStore>,
    ) -> anyhow::Result<Option<zeroize::Zeroizing<String>>> {
        let secret = self.token_secret.as_ref().or_else(|| self.token_secrets.first());
        if let Some(name) = secret
            && let Some(v) = store.and_then(|s| s.external_value(name))
        {
            tracing::info!(secret = %name, consumer = %self.name, "secret injected");
            return Ok(Some(v));
        }
        let read = if let Some(f) = &self.token_file {
            // The Gemini wire's credential is a Google refresh token (a
            // SQLite credentials.db or a JSON file, whichever `eidolon
            // google-login` wrote) that has to be minted into an access
            // token — a plain File read would hand the refresh token to
            // Google as the bearer. A `.db` is that credential whatever
            // the wire, since a bare token file is never a SQLite db.
            //
            // The Codex wire's is the same arrangement for ChatGPT
            // (`eidolon chatgpt-login` writes it): the file holds a
            // refresh token that must be minted, and — the one difference —
            // a rotated refresh token that has to be written back.
            let refresh = self.wire == Wire::Gemini
                || f.extension().is_some_and(|e| e == "db" || e == "sqlite");
            let src = if self.wire == Wire::Codex {
                harnox::llm::TokenSource::CodexRefresh { path: f.clone() }
            } else if refresh {
                harnox::llm::TokenSource::GoogleRefresh {
                    path: f.clone(),
                    client_id: None,
                    client_secret: None,
                }
            } else {
                harnox::llm::TokenSource::File(f.clone())
            };
            src.read()
        } else if let Some(e) = &self.token_env {
            harnox::llm::TokenSource::Env(e.clone()).read()
        } else {
            Ok(self.token.clone())
        };
        // An unset variable or an unreadable file is only a *failed
        // fallback* when a secret was named too — the error worth showing
        // then is the one below, which names both places a key can go.
        let fallback = match (read, secret) {
            (Ok(v), _) => v,
            (Err(_), Some(_)) => None,
            (Err(e), None) => return Err(e),
        };
        match (secret, fallback) {
            (_, Some(v)) => Ok(Some(zeroize::Zeroizing::new(v))),
            // Named a secret, and nothing anywhere held it.
            (Some(name), None) => match store {
                Some(_) => anyhow::bail!(
                    "provider `{}`: no key for secret `{name}`; run `eidolon secret set {name}`{}",
                    self.name,
                    self.or_env()
                ),
                None => anyhow::bail!(
                    "provider `{}` needs secret `{name}` but no secret store is configured{}",
                    self.name,
                    self.or_env()
                ),
            },
            (None, None) => Ok(None),
        }
    }

    /// The other place a key could have gone, for the error above.
    fn or_env(&self) -> String {
        match &self.token_env {
            Some(e) => format!(" (or set {e})"),
            None => String::new(),
        }
    }

    pub fn serves(&self, id: &str) -> bool {
        self.models.iter().any(|m| m.id == id)
            || self
                .model_globs
                .iter()
                .any(|g| harnox::llm::spec::glob_match(g, id))
    }

    /// Lay a [`ProviderPatch`] over this definition, field by field. What
    /// the patch does not mention is kept — which is the whole point: to
    /// move one provider's key to a file you should not have to restate
    /// its wire, its base URL, its models and their prices, and then
    /// watch that copy rot the next time any of them changes.
    ///
    /// The credential is the exception, and deliberately so: a patch that
    /// names *any* token source replaces all of them — the four single
    /// sources and the pool — because "where the key lives" is one
    /// decision. A patch that names none inherits all five.
    pub fn patched(&self, p: ProviderPatch) -> ProviderDef {
        let names_a_token = p.names_a_token();
        let mut def = self.clone();
        if let Some(v) = p.wire {
            def.wire = v;
        }
        if let Some(v) = p.base_url {
            def.base_url = v;
        }
        if names_a_token {
            def.token_file = p.token_file;
            def.token_env = p.token_env;
            def.token = p.token;
            def.token_secret = p.token_secret;
            def.token_secrets = p.token_secrets.unwrap_or_default();
        }
        if let Some(v) = p.auth {
            def.auth = v;
        }
        if p.project.is_some() {
            def.project = p.project.clone();
        }
        if let Some(v) = p.headers {
            def.headers = v;
        }
        if let Some(v) = p.compat {
            def.compat = v;
        }
        // Models replace rather than merge: a list is an answer to "which
        // models does this serve", and half of one is not.
        if let Some((models, globs)) = p.models {
            def.models = models;
            def.model_globs = globs;
        }
        if p.temperature.is_some() {
            def.temperature = p.temperature;
        }
        def
    }

    /// From a `[[providers]]` TOML table (harnox's `ProviderSpec` shape plus
    /// the optional extras). Every field but the name may be absent — an
    /// entry naming a provider the catalog already has is a *patch* on it,
    /// and only a new provider must say enough to be one.
    pub fn from_toml(v: &toml::Value) -> anyhow::Result<Self> {
        let p = ProviderPatch::from_toml(v)?;
        let name = p.name.clone();
        let wire = p.wire.ok_or_else(|| {
            anyhow::anyhow!("provider `{name}`: no `wire`, and no provider of that name to patch")
        })?;
        let base_url = p.base_url.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "provider `{name}`: no `base_url`, and no provider of that name to patch"
            )
        })?;
        Ok(ProviderDef {
            name,
            wire,
            base_url,
            ..Default::default()
        }
        .patched(ProviderPatch {
            wire: None,
            base_url: None,
            ..p
        }))
    }
}

impl Default for ProviderDef {
    fn default() -> Self {
        ProviderDef {
            name: String::new(),
            wire: Wire::OpenAi,
            base_url: String::new(),
            token_file: None,
            token_env: None,
            token: None,
            token_secret: None,
            token_secrets: Vec::new(),
            auth: AuthStyle::default(),
            project: None,
            headers: BTreeMap::new(),
            compat: Compat::default(),
            models: Vec::new(),
            model_globs: Vec::new(),
            temperature: None,
        }
    }
}

/// A `[[providers]]` table read as *changes* rather than as a whole
/// record: everything optional but the name.
///
/// This is what makes a built-in provider adjustable without being
/// restated. A shipped definition is compiled into the binary and cannot
/// be edited in place, and replacing it by name means copying its models,
/// prices and endpoint into a file that then drifts silently from the one
/// it shadowed. Two lines that say only "the key is in this file" cannot
/// drift, because they say nothing that could.
#[derive(Clone, Debug, Default)]
pub struct ProviderPatch {
    pub name: String,
    pub wire: Option<Wire>,
    pub base_url: Option<String>,
    pub token_file: Option<PathBuf>,
    pub token_env: Option<String>,
    pub token: Option<String>,
    pub token_secret: Option<String>,
    /// Replaces [`ProviderDef::token_secrets`] when the patch names any
    /// token source; `None` inherits.
    pub token_secrets: Option<Vec<String>>,
    pub auth: Option<AuthStyle>,
    pub project: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub compat: Option<Compat>,
    /// Explicit models and id globs, when the table lists any.
    pub models: Option<(Vec<ModelDef>, Vec<String>)>,
    pub temperature: Option<f64>,
}

impl ProviderPatch {
    /// Does this patch say where the credential lives?
    pub fn names_a_token(&self) -> bool {
        self.token_file.is_some()
            || self.token_env.is_some()
            || self.token.is_some()
            || self.token_secret.is_some()
            || self.token_secrets.is_some()
    }

    pub fn from_toml(v: &toml::Value) -> anyhow::Result<Self> {
        #[derive(Deserialize)]
        struct T {
            name: String,
            #[serde(default)]
            wire: Option<Wire>,
            #[serde(default)]
            base_url: Option<String>,
            #[serde(default)]
            token_file: Option<PathBuf>,
            #[serde(default)]
            token_env: Option<String>,
            #[serde(default)]
            token: Option<String>,
            #[serde(default)]
            token_secret: Option<String>,
            #[serde(default)]
            token_secrets: Option<Vec<String>>,
            #[serde(default)]
            auth: Option<AuthStyle>,
            #[serde(default)]
            project: Option<String>,
            #[serde(default)]
            headers: Option<BTreeMap<String, String>>,
            #[serde(default)]
            compat: Option<Compat>,
            /// Globs (strings) or full model tables.
            #[serde(default)]
            models: Option<Vec<toml::Value>>,
            #[serde(default)]
            temperature: Option<f64>,
        }
        let t: T = v.clone().try_into()?;
        let models = match t.models {
            None => None,
            Some(list) => {
                let mut models = Vec::new();
                let mut globs = Vec::new();
                for m in list {
                    match m {
                        toml::Value::String(s) => globs.push(s),
                        other => models.push(other.try_into::<ModelDef>()?),
                    }
                }
                Some((models, globs))
            }
        };
        Ok(ProviderPatch {
            name: t.name,
            wire: t.wire,
            base_url: t.base_url,
            token_file: t.token_file,
            token_env: t.token_env,
            token: t.token,
            token_secret: t.token_secret,
            token_secrets: t.token_secrets,
            auth: t.auth,
            project: t.project,
            headers: t.headers,
            compat: t.compat,
            models,
            temperature: t.temperature,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(secret: Option<&str>, env: Option<&str>) -> ProviderDef {
        ProviderDef {
            name: "p".into(),
            wire: Wire::Anthropic,
            base_url: "http://x".into(),
            token_file: None,
            token_env: env.map(String::from),
            token: None,
            token_secret: secret.map(String::from),
            token_secrets: Vec::new(),
            auth: AuthStyle::default(),
            project: None,
            headers: BTreeMap::new(),
            compat: Compat::default(),
            models: vec![],
            model_globs: vec![],
            temperature: None,
        }
    }

    /// A named secret nothing holds falls through to the variable beside it,
    /// and says where a key could have gone when neither is filled in.
    #[test]
    fn secret_falls_back_to_env() {
        let var = "EIDOLON_TEST_KEY_FALLBACK";
        unsafe { std::env::set_var(var, "k") };
        assert_eq!(
            def(Some("p"), Some(var))
                .token(None)
                .unwrap()
                .as_deref()
                .map(|s| s.to_string()),
            Some("k".into())
        );
        unsafe { std::env::remove_var(var) };
        let e = def(Some("p"), Some(var))
            .token(None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("secret `p`") && e.contains(var), "{e}");
        // No secret named: an unset variable is still that provider's error.
        assert!(def(None, Some(var)).token(None).is_err());
    }
}
