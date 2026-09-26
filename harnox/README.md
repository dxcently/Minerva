# harnox

The shared Rust foundation under [Mneme](https://github.com/noah427/mneme),
[Melete](https://github.com/noah427/melete), and the interactive coding harness.

Mneme and Melete each carried their own copy of a ~1,000-line OAuth 2.1
server that differed by 65 lines — an env-var prefix, a scope string, and the
consent-page wording. Melete's OAuth *client* for that server had already been
ported by hand into TypeScript once, and a Rust harness would make a third
consumer. This crate is that extraction: one crate, feature-gated, so each
consumer compiles exactly the seams it uses and nothing it doesn't.

## Features

| Feature | Module | What it is | Consumers |
|---|---|---|---|
| *(core)* | `crypto`, `fs`, `env`, `html`, `time` | OS randomness, SHA-256, base64url/hex; atomic `0600` writes; env lookup with deprecated aliases; one HTML escape; epoch seconds | all |
| `oauth-server` | `oauth_server` | The OAuth 2.1 authorization + resource server that guards an MCP transport for Claude.ai connectors (PKCE S256, DCR, passphrase-gated consent, rotating refresh tokens, RFC 7009 token revocation, optional persisted grants) | Mneme, Melete |
| `oauth-client` | `oauth_client` | The browserless client for that server: register → authorize → consent (passphrase) → token, refresh-token grant exchange, plus an in-memory per-origin token cache | Melete, harness |
| `secrets` | `secrets` | The XChaCha20-Poly1305 custodied secret store with no read path — `external` values injected Rust-side only, `issued` tokens kept as hashes | Melete |
| `provider-auth` | `setup_token` | Driving `claude setup-token` under a PTY to mint a ~1-year Anthropic bearer, with the token reconstructed off a vt100-rendered screen | Melete, harness |
| `llm` | `llm` | The model layer: the canonical Anthropic-shaped conversation model, the `Provider` trait + `StreamEvent` an agent loop consumes, the Anthropic Messages client (native wire; Kimi rides it with a different base URL) and one OpenAI-compatible adapter, an SSE parser, and call-time credential reads from `0600` files | harness (Melete once its own provider client is deleted) |
| `bitcode` | — | `bitcode::Encode`/`Decode` derives on the `llm` message model, for a consumer whose session log is bitcode | harness |
| `claude-cli` | `claude_cli` | Driving the headless `claude` CLI as a model backend on the Claude subscription: the argv contract (one-shot and streaming), the `--output-format json` result, the error taxonomy (`ClaudeErrorClass`) retries and fallbacks hang on, the prompt-over-stdin rule, and a default `tokio::process` runner | Melete (harness, if a setup-token bearer bills differently from the CLI) |
| `full` | — | all of the above | — |

The default feature set is **empty**; a consumer names what it uses:

```toml
[dependencies]
harnox = { git = "https://github.com/noah427/harnox", tag = "v0.1.0", features = ["oauth-server"] }
```

## Invariants

- **Features are additive.** A feature adds modules; it never changes the
  behaviour of a module another feature already compiles. Cargo unifies
  features across a dependency graph, so a behaviour-changing feature would let
  one consumer's choice silently reconfigure another's.
- **Secrets never reach a log or a tool.** `OauthToken`'s `Debug`/`Display`
  are redacted; OAuth tokens are stored only as SHA-256 hashes; the secret store
  has exactly one injection door (`SecretStore::external_value`) and it is the
  consumer's job to audit every use as `(name, consumer)`, never the value.
- **Errors are `anyhow::Result`** with `.context()` on I/O — the convention
  both consumers already hold.

## Wiring

**Server** (an axum app; Mneme and Melete):

```rust
use harnox::oauth_server::{AuthConfig, AuthState, Branding, oauth_router, require_auth};

let branding = Branding {
    scope: "vault.read".into(),
    heading: "Authorize access to your vault".into(),
    grant_description: "(read and write) access to your notes.".into(),
};
// Reads MNEME_PUBLIC_URL / MNEME_AUTH_PASSWORD / MNEME_AUTH_STATE_FILE, with the
// OBSIDIAN_MCP_* names honoured (and warned about) as deprecated aliases.
let auth = AuthState::new(AuthConfig::from_env("MNEME", Some("OBSIDIAN_MCP"), branding)?);

let protected = axum::Router::new()
    .nest_service("/mcp", service)
    .layer(axum::middleware::from_fn_with_state(auth.clone(), require_auth));
let app = axum::Router::new().merge(protected).merge(oauth_router(auth));
```

**Client** (Melete against Mneme; the harness against both):

```rust
use harnox::oauth_client::{ClientIdentity, TokenCache, oauth_base};

static CACHE: LazyLock<TokenCache> = LazyLock::new(TokenCache::new);
let identity = ClientIdentity {
    client_name: "Melete".into(),
    redirect_uri: "http://127.0.0.1/melete-cb".into(),
    scope: "vault.read".into(),
};
let token = CACHE.get_or_mint(&oauth_base(mcp_url), &passphrase, &identity).await?;
// … on a 401 from the server: CACHE.invalidate(&oauth_base(mcp_url)) and mint again.
```

**Credential leases** (a session shipped to another machine — Melete → a fleet box):

```rust
use harnox::oauth_client::{mint_access_token, refresh_access_token};

// The mint's `MintedToken.refresh_token` is the lease: whoever holds it mints
// fresh access tokens without the operator passphrase.
let lease = mint_access_token(&base, &passphrase, &identity).await?;

// On the remote side — the client_id is the one the grant was minted under:
let fresh = refresh_access_token(&base, &lease.refresh_token.unwrap(), &client_id).await?;
// `fresh.refresh_token` is the ROTATED replacement — persist it, retire the old
// (which the server keeps accepting only for a short concurrent-refresh grace
// window). Re-presenting a rotated-away token surfaces `invalid_grant`.

// Ending the lease from home (RFC 7009): POST {base}/oauth/revoke with form
// `token=<refresh_token>` — the grant and its live access bearer die together.
```

**Provider auth** (Melete; the harness):

```rust
use harnox::setup_token;

let started = setup_token::start("claude", &["setup-token".into()]).await?;
// hand `started.authorize_url` to the human; keep `started.session` in memory
let token = started.session.submit_code(&pasted_code_state).await?;
// persist `token.expose()` to a 0600 file — never log it
```

**Model layer** (the harness):

```rust
use harnox::llm::{ChatRequest, ProviderSpec, StreamEvent, build_provider};
use tokio_util::sync::CancellationToken;
use futures_util::StreamExt;

let spec: ProviderSpec = toml::from_str(r#"
    name = "anthropic"
    wire = "anthropic"
    base_url = "https://api.anthropic.com"
    token_file = "~/.config/harness/anthropic.token"   # a `claude setup-token` bearer
    auth = "bearer"
    models = ["claude-*"]
"#)?;
let provider = build_provider(&spec);
let cancel = CancellationToken::new();
let mut events = provider.stream(ChatRequest { /* … */ }, cancel.clone());
while let Some(ev) = events.next().await {
    match ev? {
        StreamEvent::TextDelta(t) => print!("{t}"),
        StreamEvent::Stop { stop_reason, usage } => { /* one per stream, always last */ }
        _ => {}
    }
}
```

**Claude CLI backend** (Melete):

```rust
use harnox::claude_cli::{ClaudeCli, ClaudeErrorClass, TaskOptions};

let cli = ClaudeCli::new("claude", Some("sonnet".into()));
let opts = TaskOptions { mcp_config: Some("/run/mcp.json".into()), allowed_tools: vec!["mcp__mneme__*".into()], ..Default::default() };
match cli.run_task(&huge_prompt, &opts).await {           // prompt goes over stdin, never argv
    Ok(r) => println!("{}", r.result),
    Err(e) => match ClaudeErrorClass::classify(&format!("{e:#}")) {
        ClaudeErrorClass::RateLimit { reset_at } => { /* hold until reset_at */ }
        ClaudeErrorClass::AuthFailure => { /* re-mint with setup_token */ }
        _ => return Err(e),
    },
}
// A consumer with its own process hygiene spawns `cli.binary()` + `cli.build_args(&opts)`
// itself and hands the stdout to `ClaudeCli::parse_result`.
```

## Releasing

Mneme is distributed (a Nix flake, through melete-distributor, to other
people's installs), so a breaking change here reaches third parties. Real
semver, tagged releases, and consumers pinning a tag:

```sh
cargo test --all-features && cargo clippy --all-features --all-targets -- -D warnings
git tag v0.1.0 && git push --tags
```

Then bump the `tag` in each consumer's `Cargo.toml` and `cargo update -p harnox`.
Both consumers build with `--locked` in CI, so the lockfile change ships with the
bump.

The three consumers release on different cadences (Mneme distributed, Melete
auto-deployed on push, the harness local). If the LLM half of this crate ever
churns fast while the auth half stays stable, that divergence is the signal to
split it — by then the seam will be known empirically rather than guessed.

## Layout

```
src/
├── lib.rs           feature map, the additive-features invariant
├── crypto.rs        sha256 · random_bytes · random_token · hex
├── fs.rs            write_atomic_0600 · chmod_600 · upsert_env_line
├── env.rs           env_var(primary, legacy…)
├── html.rs          html_escape
├── time.rs          now_secs · secs_since_epoch
├── oauth_server.rs  [oauth-server]   AuthConfig · Branding · AuthState · oauth_router · require_auth
├── oauth_client.rs  [oauth-client]   ClientIdentity · mint_access_token · refresh_access_token · oauth_base · TokenCache
├── secrets.rs       [secrets]        SecretStore · Kind · Meta · render · validate_secret_key
├── setup_token.rs   [provider-auth]  start · Session · OauthToken · extract_authorize_url · scrape_token
├── claude_cli.rs    [claude-cli]     ClaudeCli · TaskOptions · ClaudeResult · ClaudeErrorClass
└── llm/             [llm]
    ├── message.rs                    Message · Role · ContentBlock · Json · StopReason · Usage   (+bitcode derives under `bitcode`)
    ├── provider.rs                   Provider · ChatRequest · ToolDef · ThinkingConfig · StreamEvent · EventStream
    ├── anthropic.rs                  the native wire (Anthropic, Kimi's /anthropic)
    ├── openai.rs                     the one OpenAI-compatible adapter (Qwen, …)
    ├── sse.rs                        byte stream → SSE events
    ├── credentials.rs                TokenSource — a token read from a 0600 file or env var at call time
    └── spec.rs                       ProviderSpec · Wire · AuthStyle · build_provider · glob_match
```

Technical detail lives in the module docs beside the code. Status, the release
procedure and the consumer table live in the operator's vault at
`wiki/projects/harnox/index.md`; the reasoning behind one crate rather than two
is in `wiki/projects/Custom Harness/` ("A shared crate").
