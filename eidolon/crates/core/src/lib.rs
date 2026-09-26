//! Headless core of the harness: the canonical conversation model, the
//! append-only binary session log, the event bus, the tool/policy contracts,
//! the single async dispatch chokepoint, and the agent loop that ties them
//! together.
//!
//! Nothing in this crate knows about a terminal, a provider's wire format, or
//! Rune. Those are *consumers* and *implementations* of the traits defined
//! here:
//!
//! - [`provider::Provider`] is `harnox::llm::Provider`, implemented there
//!   (Anthropic native wire, OpenAI-compatible adapter). The message model
//!   is harnox's too; this crate re-exports both so consumers name one
//!   crate.
//! - [`tool::Tool`] is implemented by `eidolon-rune` (every tool, built-in or
//!   user-authored, is a Rune script wrapping Rust primitives).
//! - [`policy::PolicyHook`] and [`user::UserIo`] are implemented by whatever
//!   sits at the keyboard (the CLI today, the TUI later) or by nothing at all
//!   in a headless run.
//!
//! The load-bearing invariants, in order:
//!
//! 1. **One chokepoint.** Every tool call of every origin — model-invoked,
//!    script-invoked, user-invoked, `choices_user` — goes through
//!    [`dispatch::Dispatcher::dispatch`]. The policy hook is evaluated there
//!    and nowhere else, and it fails closed.
//! 2. **The log is the truth.** Everything the agent loop does is a record in
//!    the [`session::SessionLog`] before it is an event on the bus. Resume is
//!    replay; a fork is a record whose parent is not the current head.
//! 3. **Turn boundaries are published, not inferred.** The loop emits
//!    [`event::Event::TurnSettled`] when a model response arrives with no
//!    tool calls pending, and journals it as a record so resume can land on a
//!    clean boundary.
//!
//! Design record and rationale: the vault, `wiki/projects/Eidolon/`.

/// The canonical conversation model, from the shared crate.
pub mod message {
    pub use harnox::llm::message::*;
}
/// The provider seam, from the shared crate.
pub mod provider {
    pub use harnox::llm::provider::*;
}

pub mod agent;
pub mod attribution;
pub mod boot;
pub mod commands;
pub mod dispatch;
pub mod escalate;
pub mod event;
pub mod ipc;
pub mod peer;
pub mod persona;
pub mod policy;
pub mod project;
pub mod quiesce;
pub mod schema;
pub mod session;
pub mod testing;
pub mod tool;
pub mod tool_search;
pub mod usage;
pub mod user;
pub mod wait;
pub mod yolo;

pub use agent::{Agent, AgentConfig, Backend, TurnDriver, TurnOutcome};
pub use commands::CommandChannel;
pub use dispatch::Dispatcher;
pub use escalate::Judge;
pub use event::{Event, EventBus};
pub use message::{ContentBlock, Json, Message, Role, StopReason, Usage};
pub use peer::{PeerInbox, PeerMessage};
pub use persona::{PersonaEntry, PersonaSource};
pub use policy::{Approval, PolicyHook, Ruling, Verdict};
pub use provider::{ChatRequest, Provider, StreamEvent, ThinkingConfig, ToolDef};
pub use session::{
    PolicyOutcome, Record, RecordId, RecordKind, Refusal, Session, SessionLog, TreeNode,
};
pub use tool::{CallContext, CallOrigin, Tool, ToolCall, ToolManifest, ToolOutput, ToolRegistry};
pub use tool_search::{Reach, ToolSearch};
pub use usage::UsageExt;
pub use user::{Choice, UserIo};
pub use wait::{Answer, Arm, Condition, Fired, Outcome, Parks, Term, Vantage, Wake, WaitFor};
