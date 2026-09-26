//! Verba Volantia in the harness: free text → one typed tool call, in
//! milliseconds, with no model turn — then through the dispatcher like
//! anything else.
//!
//! The vault's decision (*Embedded VV*, 2026-09-03) has two halves and this
//! crate is the first: **resolution moves client-side, execution does not.**
//! A [`Palette`] holds one [`Bundle`] per tool surface — each a trained
//! checkpoint (a directory: `model.safetensors` + `meta.json`) and a rule
//! for turning a verdict into a [`ToolCall`]. `resolve` asks each bundle's
//! classifier in turn and returns the first accepted verdict as a call the
//! caller dispatches. Policy, the approval dialog and the journal all apply
//! exactly as for a model-originated call; the only thing VV replaces is
//! the LLM turn that would have produced the call.
//!
//! **Abstention is the default** (Melete's invariant, kept verbatim): a
//! classifier error, a timeout, a `none`/`UNK` intent, or `accept: false`
//! all mean "not a command", and the caller falls back to sending the text
//! to the model. A dead classifier never breaks the prompt.
//!
//! The classifier runs as Verba Volantia's own binary — `verba-volantia
//! dispatch --out <checkpoint>`, spawn-once-pipe-many: one utterance line in
//! on stdin, one JSON verdict line out — because VV is a binary crate today.
//! In-process inference (VV as a `harnox` feature over candle) is the second
//! half, and it changes nothing above [`Dispatcher`].
//!
//! [`command`] is the same resolution for the *model*: two entry seams into
//! one engine, selected by [`Entry`] — the inline interceptor, where the
//! model writes `! <command>` on a line of its own reply and the loop
//! answers it in the same turn, and the `vv` tool whose only parameter is a
//! line of prose. Both gate the same read-shaped Mneme intents and dispatch
//! through the same chokepoint. [`reconcile`] is the vocabulary mapping
//! every consumer shares — the corpus's slot names are not Mneme's, and
//! neither dispatch mode nor the channel may translate them differently.
//!
//! Targets today: `mneme_rpc` (the intent is a Mneme function, slots are
//! its named args) and `tools` (the intent is a harness tool name, slots
//! its input). Training data for the `tools` surface comes from
//! [`spec::templates_skeleton`], which turns the registered manifests into
//! a `templates.json` for `verba-volantia gen` — the phrasings still need
//! authoring.

pub mod command;
pub mod dispatcher;
pub mod palette;
pub mod reconcile;
pub mod spec;

use serde::{Deserialize, Serialize};

pub use command::{Gate, VaultCommands};
pub use dispatcher::{VerbaDispatcher, Verdict};
pub use palette::{Bundle, Palette, Resolution, Target};

/// Which seam the command channel enters by — the operator's switch, read
/// from the config (`[verba] entry`).
///
/// The channel is one engine with two ways in, and the model must never see
/// both at once: two surfaces taught at once is two conventions to satisfy
/// and two adoption rates measured as one. So this is a choice, declared,
/// rather than a fallback chain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    /// **Inline.** The model writes `! <command>` on a line of its own reply,
    /// the loop intercepts the completed message, and the answers come back
    /// as a journaled record the model reads on one more call. No `vv` tool
    /// is registered; the teaching is spliced into the system prompt. This is
    /// the default, because it is the end-state the channel is aimed at —
    /// no schema in the tool list, no call to compose, and the answers
    /// arriving in the turn that asked rather than the next one.
    ///
    /// A backend that owns its own loop (the Claude CLI) cannot be served
    /// this way — the harness never assembles that turn's request — so such a
    /// session keeps the tool whatever this says. See
    /// [`crate::command`].
    #[default]
    Inline,
    /// **The `vv` tool.** A registered tool whose only parameter is the line
    /// of prose, offered with the rest and dispatched like any call. The
    /// seam the channel had before the interceptor existed, kept because a
    /// surface the operator can fall back to is worth more than one they
    /// cannot.
    Tool,
}
