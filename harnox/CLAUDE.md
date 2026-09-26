# CLAUDE.md

`harnox` is the shared Rust foundation under three consumers: **Mneme** (the
vault MCP server, distributed to third parties), **Melete** (the personal-AI-hub
harness), and **Eidolon** (the interactive coding harness). It carries what they
genuinely have in common — an OAuth 2.1 server, the browserless OAuth client, the
custodied secret store, `claude setup-token` automation, the headless `claude`
CLI contract, the command safety classifier's decomposition algebra, and the
model layer (canonical message model, `Provider` trait, Anthropic +
OpenAI-compatible streaming clients) — behind Cargo features.

## Commands

```sh
cargo test --all-features
cargo clippy --all-features --all-targets -- -D warnings   # keep clean
cargo check --no-default-features                            # the core must stand alone
cargo check --no-default-features --features oauth-server    # what Mneme compiles
```

`.github/workflows/ci.yml` runs clippy over every feature combination a
consumer uses — add a new feature to its list.

## Invariants

- **Features are additive.** A feature adds modules; it never changes the
  behaviour of a module another feature compiles. Cargo unifies features across
  a build, so anything else lets one consumer's choice reconfigure another's.
- **The default feature set is empty.** Consumers name what they use. Mneme has
  no LLM code and must never compile a provider layer.
- **Secrets never reach a log.** `OauthToken` redacts itself; the secret store
  has no read path on any tool surface (`external_value` is the one injection
  door, audited as `(name, consumer)`); tokens are stored only as hashes.
- **Errors are `anyhow`** with `.context()` on I/O. No custom error enum.
- **Mneme is distributed**, so a breaking change here reaches third parties
  through it: real semver, tagged releases, consumers pin a tag.
- The consent page's hidden `request_id` markup is a contract between
  `oauth_server::consent_page` and `oauth_client::extract_request_id`.
- **The canonical model is the Anthropic Messages shape**, never a lowest
  common denominator; the OpenAI adapter translates *from* it. A provider
  stream emits exactly one `Start` first and one `Stop` last, and every
  `ToolUseStart` is closed by a `BlockStop` — consumers' assemblers rely on it.
- **`policy` decomposes; the consumer's table decides.** A `LeafTable` is only
  ever asked about something already reduced to a flat `argv`, so it cannot
  grant safety by construction — and a `Verdict`'s `structural` bit separates a
  refusal on the merits from one for want of understanding. Those must not
  collapse into each other: it is the whole reason one classifier can serve an
  unattended run (both refuse) and a person at a keyboard (the second is a fair
  question) without either inheriting the other's posture.
- The `llm` wire clients document *their own subset*, never the protocol —
  a wrapper doc that reads like a spec is how a later reader concludes a
  feature is impossible and ships a degradation.

## Where documentation lives

Module docs (`//!` / `///`) are the technical reference and sit beside the code.
Planning, status, the release procedure, and rationale live in the operator's
Obsidian vault: `wiki/projects/harnox/index.md` is this crate's status page, and
the one-crate-versus-two reasoning is in `wiki/projects/Custom Harness/` (the
"A shared crate" section). Both are reached through Mneme; never write there
from this repo.
