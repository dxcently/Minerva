//! Model providers (feature `llm`): the canonical conversation model, the
//! provider seam an agent loop consumes, and the wires behind it.
//!
//! The wires, deliberately not a generic abstraction over N:
//!
//! - [`anthropic`] — the native format. Anthropic itself, and any provider
//!   exposing an Anthropic-compatible endpoint (Kimi's `/anthropic`), share
//!   this code path with only a base URL and token changed. Thinking blocks,
//!   signatures, `cache_control` and tool-input deltas all survive intact.
//! - [`openai`] — one adapter for OpenAI-compatible chat-completions
//!   endpoints (Qwen on DashScope, and anything else that speaks it). It
//!   translates *from* the canonical model and back; what it cannot carry
//!   (thinking signatures) it drops.
//! - [`codex`] — the ChatGPT Codex backend: OpenAI's Responses shape with a
//!   Codex flavour, authenticated by a ChatGPT subscription's OAuth token
//!   rather than an API key. Reasoning round-trips through the thinking
//!   block's signature, as `encrypted_content`.
//! - [`gemini`] — Google's Gemini/Antigravity surface.
//!
//! A generic provider abstraction would normalise to a lowest common
//! denominator and cost exactly the things this stack cares about — thinking
//! blocks, `cache_control`, `effort` — so the canonical model *is* the
//! Anthropic Messages shape ([`message`]) and the OpenAI adapter is the one
//! translation.
//!
//! Credentials are never acquired here. [`credentials`] reads a token from a
//! `0600` file (or an environment variable) at call time — and [`probe`]s
//! that source's structure without reading it for use — while minting is
//! somebody else's job, a few times a year, outside the consumer: for
//! Anthropic, the `provider-auth` feature's `setup_token` driver.
//!
//! [`consume`] is what a stream becomes: the event loop, the block assembly,
//! the usage merge, and the retry rules, shared by a long agent loop and a
//! one-shot completion so the two cannot drift into two interpretations of
//! the same wire.
//!
//! [`probe`]: credentials::TokenSource::probe
//!
//! Every message type derives `serde`; with the `bitcode` feature they also
//! derive `bitcode::Encode`/`Decode` for a consumer whose session log is
//! bitcode. That feature only adds trait impls — additive, like every other.

pub mod anthropic;
pub mod consume;
pub mod credentials;
pub mod http;
pub mod message;
pub mod openai;
pub mod provider;
pub mod sse;
pub mod spec;

pub use consume::{
    CallTiming, Collected, MAX_STREAM_ATTEMPTS, QuietSink, RetryPolicy, StreamSink,
    is_auth_failure, is_transient, stream_with_retries,
};
pub use credentials::{CodexCreds, SourceKind, SourceProblem, SourceState, TokenSource, codex_account_from_jwt, CODEX_CLIENT_ID};
pub use message::{ContentBlock, Json, Message, Role, StopReason, Usage};
pub use provider::{CacheOptions, CacheRetention, ChatRequest, EventStream, Provider, StreamEvent, ThinkingConfig, ToolChoice, ToolDef};
pub use spec::{AuthStyle, ProviderSpec, Wire, build_provider};
pub mod codex;
pub mod gemini;
