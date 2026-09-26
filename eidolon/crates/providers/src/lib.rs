//! Providers and the model catalog.
//!
//! A provider is declared, not coded: a [`ProviderDef`] names a wire
//! (Anthropic or OpenAI-compatible), a base URL, where the token lives,
//! optional headers and compat flags, and the models it serves with their
//! display names, context windows and prices. Definitions come from two
//! places that produce the same record:
//!
//! - a `[[providers]]` entry in `config.toml` (declarative only);
//! - a **Rune script** in the providers directory whose `provider()` returns
//!   the record, and which may also define `request(body)` to rewrite the
//!   outgoing request (rename `max_tokens`, add a gateway field, drop what a
//!   proxy rejects), `models()` to list models dynamically via
//!   `eidolon::http_get`, and `quota()` to read what a subscription has
//!   left ([`quota`]). That is what makes a gateway with quirks a
//!   ten-line file instead of a Rust change. See `examples/providers/`.
//!
//! Three providers ship with the harness: **deepseek**, **zai** and
//! **openrouter**, embedded as `builtin/*.rn` and loaded before either of
//! the above, so a fresh install reaches a real model with a key and no
//! config file. None is
//! privileged — a `[[providers]]` table or a `providers_dir` script under
//! the same name replaces it whole, exactly as a user tool replaces a
//! built-in tool.
//!
//! [`HttpProvider`] turns a definition into a `harnox` `Provider`: it uses
//! harnox's request builders and stream mappers for the wire itself and
//! adds what a definition can vary — headers, auth style, compat, the
//! script hook. The Claude CLI backend is not an HTTP provider; the
//! [`Catalog`] still lists its models so one picker covers everything.
//!
//! Model keys are `provider:id` (`fau:azure_ai/gpt-5.4`,
//! `claude-cli:sonnet`). A bare id resolves through the catalog when it is
//! unambiguous, so `:model sonnet` and `:model gpt-5.4` keep working.
//!
//! [`readiness`] sits beside resolution rather than inside it: it answers
//! *structurally* whether a provider has the local prerequisites to attempt
//! a request — offline, with no mint, no spawn, no network and no token-pool
//! lane taken — and it never says a merely present credential is valid. It is
//! what a picker or a health check may ask on a hot path; what a request
//! finds out is between the request and the endpoint.
//!
//! [`completion`] is the other one: a single **tool-less** provider-backed
//! round trip, whose request type cannot express a tool, resolved and priced
//! here, consuming its stream through the same retry and assembly code the
//! agent loop uses. For a caller that wants an answer rather than an agent —
//! `ask_model`, a summary, a classification.

pub mod catalog;
pub mod completion;
pub mod def;
pub mod http;
pub mod quota;
pub mod readiness;
pub mod script;

pub use catalog::{Catalog, CatalogModel, ClaimedKey, Resolved};
pub use completion::{CompletionEnding, CompletionRequest, CompletionResult, CompletionTiming};
pub use def::{Compat, Cost, ModelDef, ProviderDef};
pub use http::HttpProvider;
pub use quota::{Quota, QuotaWindow};
pub use readiness::{CredentialSource, ModelReadiness, Readiness, UnavailableReason};
pub use script::{Fetch, ProviderScript};
