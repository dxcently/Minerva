//! The inline command channel: commands the model writes in its own prose.
//!
//! Some surfaces have no tool call. Verba Volantia's vault channel is one:
//! the model writes a marked line — `! read the note Groceries` — in a reply,
//! and the loop, not the model, serialises it into an ordinary dispatched
//! call. What the core knows about that is **when**: a reply that marks
//! commands is owed their answers exactly as a reply that made a tool call is
//! owed its results, and it is owed them *before the turn settles*, so the
//! model answers from them instead of yielding to the operator with a
//! question the harness has already answered.
//!
//! What the core must not know is the marker, the classifier, the vault, or
//! what an answer looks like. That is why this is a trait — the shape of
//! [`crate::user::UserIo`], [`crate::peer::PeerInbox`] and
//! [`crate::persona::PersonaSource`] — and not a mechanism: the core decides
//! *when* a message becomes part of the conversation, and
//! `eidolon-verba` decides what one is.
//!
//! ## Why the answers are a journaled record and not a tool result
//!
//! Because there is no call. A `tool_result` block is well-formed only
//! against a `tool_use` the assistant actually made, and the whole point of
//! the inline channel is that the model wrote prose instead of a call — so
//! the answers are journaled as [`crate::session::RecordKind::CommandResults`]
//! and the message assembly frames them as the harness's own words. Anything
//! else would tell the model, and every consumer replaying the branch, either
//! that the operator said them or that the model made a call it did not.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::message::Message;

#[async_trait]
pub trait CommandChannel: Send + Sync {
    /// Answer whatever commands `message` marks, or `None` when it marks
    /// none — the ordinary case, and the whole of what the loop needs to
    /// know to go on and settle.
    ///
    /// `Some(lines)` is one entry per command, in the order the message wrote
    /// them: the loop journals them as
    /// [`crate::session::RecordKind::CommandResults`], publishes them, and
    /// makes one more model call so the reply is read as the answer it is.
    /// An empty `Some` is treated as `None`: nothing was answered, so there
    /// is nothing to say and no reason to go round again.
    ///
    /// Called after every complete reply: before the turn settles when the
    /// reply made no calls, and after the results such a reply is owed when
    /// it made them — never in front of them. A truncated tail is not a
    /// complete command, so it is not asked. A reply that marks a command
    /// *and* makes calls is still owed its answers: the first-exposure
    /// hedge — mark the line, call the ordinary tool, see which one
    /// answers — reads silence as the channel being dead, and a model so
    /// taught never marks again.
    async fn answer(&self, message: &Message, cancel: CancellationToken) -> Option<Vec<String>>;
}
