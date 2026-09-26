//! The provider seam: what an agent loop needs from a model, expressed in
//! the canonical (Anthropic-shaped) model. Implementations are [`super::anthropic`]
//! and [`super::openai`]; a loop only consumes the stream.

use std::pin::Pin;

use futures_util::Stream;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::message::{Message, StopReason, Usage};

/// A tool as the model sees it — the MCP `tools/list` shape (name,
/// description, JSON-schema input). A consumer with a richer manifest
/// (approval tiers, render hints, prompt snippets) projects it down to this
/// at request time; the dependency direction is consumer → this crate, never
/// the reverse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Extended-thinking request. `budget_tokens` is the Anthropic knob; the
/// OpenAI adapter maps it to whatever reasoning control the target has, or
/// drops it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThinkingConfig {
    pub budget_tokens: u32,
}

/// Whether the model may answer in prose, or must reach for a tool.
///
/// Two values and not the wire's four, because the two that are missing are
/// decisions a caller has no business making. Naming *one* tool would be the
/// caller choosing the call rather than the model, and forbidding tools
/// outright is already said by offering none.
///
/// [`Auto`] is the default on both wires and sends no field at all, so an
/// endpoint that has never heard of `tool_choice` is not made to hear of it
/// for the ordinary turn. [`Required`] is for the one case that earns it:
/// re-asking a model that answered a tool-shaped question in prose. It
/// forces the *channel* and nothing else — which tool, and with what
/// arguments, stays the model's.
///
/// [`Auto`]: ToolChoice::Auto
/// [`Required`]: ToolChoice::Required
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolChoice {
    /// The model decides whether to call a tool. Nothing is sent.
    #[default]
    Auto,
    /// The model must emit a tool call. Sent only when tools are offered.
    Required,
}

/// How long the endpoint should keep what this request writes to its
/// prompt cache.
///
/// The cache is a prefix cache on every backend that has one, so the saving
/// is decided by two things: that the prefix is byte-identical from one
/// request to the next, and that the entry is still alive when the next
/// request arrives. The first is the consumer's job. This is the second.
///
/// [`Short`] is the endpoint's own default (five minutes on the Anthropic
/// wire) and is right for a turn that is followed by a tool result seconds
/// later. [`Long`] asks for the hour-long entry, which costs more to write
/// (twice the base rate rather than 1.25×) and pays for itself the moment a
/// person thinks for six minutes before answering — the ordinary rhythm of
/// an interactive session, and the case the short entry always loses.
/// [`None`] places no breakpoints at all: for a one-shot request whose
/// prefix nothing will ever share, a write is pure surcharge.
///
/// [`Short`]: CacheRetention::Short
/// [`Long`]: CacheRetention::Long
/// [`None`]: CacheRetention::None
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    /// Write nothing to the cache; read nothing from it.
    None,
    /// The endpoint's default entry lifetime.
    #[default]
    Short,
    /// The long entry, where the endpoint offers one.
    Long,
}

impl CacheRetention {
    pub fn is_off(self) -> bool {
        self == CacheRetention::None
    }
}

/// Prompt-caching preferences for one request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheOptions {
    pub retention: CacheRetention,
    /// A key that is stable for as long as the prefix is — a session id.
    /// Endpoints that shard their cache use it to route a request back to
    /// the machine holding the entry its predecessor wrote; without one, a
    /// perfectly stable prefix still misses whenever the load balancer
    /// picks a different machine. Carried here rather than derived,
    /// because only the consumer knows what "the same conversation" means.
    pub key: Option<String>,
}

impl CacheOptions {
    /// No caching at all — see [`CacheRetention::None`].
    pub fn off() -> Self {
        CacheOptions { retention: CacheRetention::None, key: None }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ChatRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub max_tokens: u32,
    pub thinking: Option<ThinkingConfig>,
    /// See [`CacheOptions`]. `Default` is the endpoint's own short entry.
    pub cache: CacheOptions,
    /// Whether this model can see images — and, in the `None` case,
    /// whether anybody said.
    ///
    /// Three-valued rather than a `bool`, and the third value is the point.
    /// `Some(false)` is a catalog that positively declares no vision, and
    /// image blocks are swapped for text on the way out
    /// ([`downgrade_images`](super::message::downgrade_images)) so that one
    /// attached screenshot does not make every later turn on the branch a
    /// `400`. `None` is *nobody told us*, which is the ordinary case: a
    /// gateway's discovered model list rarely carries the flag, and most
    /// models behind one can see. Sending is the right guess there — an
    /// endpoint that cannot will say so in an error the operator can read,
    /// whereas silently stripping the image looks exactly like a model
    /// that looked at the screenshot and ignored it.
    pub vision: Option<bool>,
    /// Sampling temperature, where a definition names one.
    ///
    /// `None` is *nobody said*, and it sends no field — the endpoint's own
    /// default stands, which is what every request did before this existed.
    /// It is worth naming for small models in particular: at the usual
    /// default of 1.0, a 14B deciding between emitting a tool call and
    /// typing the tool's name is choosing between two nearby samples, and
    /// it will do both across two runs of the same prompt.
    ///
    /// Not sent alongside extended thinking, which on the Anthropic wire
    /// requires the default and answers `400` to anything else.
    pub temperature: Option<f64>,
    /// Whether this request lets the model answer in prose. See
    /// [`ToolChoice`]; [`ToolChoice::Auto`] is the ordinary turn.
    pub tool_choice: ToolChoice,
}

impl ChatRequest {
    /// The messages as the wire should carry them: the branch's own, unless
    /// the model is known not to see, in which case its images are text.
    ///
    /// The substitution happens here and never on the log, so switching
    /// back to a model with vision shows it the picture rather than the
    /// apology written in its place.
    pub fn wire_messages(&self) -> std::borrow::Cow<'_, [Message]> {
        use std::borrow::Cow;
        if self.vision != Some(false) || !self.messages.iter().any(Message::has_images) {
            return Cow::Borrowed(&self.messages);
        }
        let mut m = self.messages.clone();
        super::message::downgrade_images(&mut m);
        Cow::Owned(m)
    }
}

/// Provider-neutral streaming events, already assembled to the granularity
/// a loop cares about. Wire-specific event names (`content_block_delta`,
/// `delta.tool_calls[i]`) are the adapter's problem.
///
/// Contract, on both wires: exactly one [`Start`] first, exactly one
/// [`Stop`] last (unless the stream errors or is cancelled), and every
/// `ToolUseStart` is closed by a `BlockStop` before the next block opens.
///
/// [`Start`]: StreamEvent::Start
/// [`Stop`]: StreamEvent::Stop
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Start { usage: Usage },
    TextDelta(String),
    ThinkingDelta(String),
    /// Signature for the thinking block currently open.
    ThinkingSignature(String),
    RedactedThinking(String),
    ToolUseStart { id: String, name: String },
    /// A fragment of the tool input's JSON text.
    ToolInputDelta(String),
    /// The current content block is complete.
    BlockStop,
    /// Terminal accounting for the message.
    Stop { stop_reason: StopReason, usage: Usage },
}

pub type EventStream<'a> = Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send + 'a>>;

pub trait Provider: Send + Sync {
    /// Human-readable identity, for logs and a status line.
    fn name(&self) -> &str;

    /// Start a streaming completion. Dropping the stream, or cancelling the
    /// token, must abort the request.
    fn stream<'a>(&'a self, req: ChatRequest, cancel: CancellationToken) -> EventStream<'a>;
}
