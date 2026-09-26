//! The user-interaction seam: what a consumer (CLI, TUI) must provide so the
//! core can ask a question and block on the answer.
//!
//! `choices_user` is a *tool* from the core's point of view — dispatched
//! through the same chokepoint as everything else, so the policy hook sees
//! it and the session log journals its answers. On resume, a journaled
//! answer replays instead of re-prompting. This trait is only the last hop:
//! the thing that actually draws a prompt.
//!
//! There is deliberately no free-text question in the vocabulary. A
//! multiple-choice question is worth blocking a turn on — the model cannot
//! guess the operator's pick, and the options are short enough to read in
//! a dialog. A free-text question is not: the operator's answer is the
//! next thing they were going to type anyway, so the model asks it by
//! *ending its turn* with the question in its prose, and the reply arrives
//! as the next prompt. An `ask_user` tool that held the turn open for a
//! paragraph answer existed and was cut for exactly this reason (2026-09).

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::policy::{Ruling, Verdict};
use crate::tool::{ToolCall, ToolManifest};

/// One selectable option in a `choices_user` prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    /// A short paragraph under the label, saying what picking it means.
    /// Shown in the dialog beneath the option; wrapped, not truncated.
    pub description: Option<String>,
}

/// Something that can put a question in front of a person.
#[async_trait]
pub trait UserIo: Send + Sync {
    /// Multiple choice. Returns the chosen label, or `None` on cancel.
    async fn choose(
        &self,
        prompt: &str,
        options: &[Choice],
        cancel: &CancellationToken,
    ) -> Option<String>;

    /// Yes/no gate. Default routes through `choose`.
    async fn confirm(&self, prompt: &str, cancel: &CancellationToken) -> bool {
        let opts = [
            Choice {
                label: "yes".into(),
                description: None,
            },
            Choice {
                label: "no".into(),
                description: None,
            },
        ];
        matches!(
            self.choose(prompt, &opts, cancel).await.as_deref(),
            Some("yes")
        )
    }

    /// A tool call the gate wants a person to approve, with everything
    /// the gate knew about it.
    ///
    /// The default puts the ruling's question through [`UserIo::confirm`],
    /// which is all a consumer with a screen needs — the question already
    /// names the tool and what it would do. A consumer that holds the call
    /// somewhere else wants the *call* and not a sentence about it: Melete
    /// parks one for a Telegram 👍 keyed by its id, writes its own audit
    /// entry from the classifier's `reason`, and denies on a deadline of
    /// its own. That is this method; the dispatcher calls it and nothing
    /// else on the `Ask` path, so overriding it is the whole of what such a
    /// consumer has to do.
    async fn approve(
        &self,
        call: &ToolCall,
        _manifest: &ToolManifest,
        ruling: &Ruling,
        cancel: &CancellationToken,
    ) -> bool {
        let prompt = match &ruling.verdict {
            Verdict::Ask(p) => p.clone(),
            _ => format!("Run `{}`?", call.name),
        };
        self.confirm(&prompt, cancel).await
    }
}

/// A `UserIo` for headless runs: nobody is there, so every question is
/// declined and every confirmation is refused. Fail closed.
pub struct NoUser;

#[async_trait]
impl UserIo for NoUser {
    async fn choose(&self, _: &str, _: &[Choice], _: &CancellationToken) -> Option<String> {
        None
    }
}

/// `choices_user` as a registered tool, so it takes the dispatch path like
/// everything else. Read-only from a policy standpoint: asking a question
/// changes nothing.
pub mod tools {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::{Value, json};

    use super::{Choice, UserIo};
    use crate::policy::Approval;
    use crate::tool::{CallContext, Tool, ToolManifest, ToolOutput};

    pub struct ChoicesUser {
        io: Arc<dyn UserIo>,
        manifest: ToolManifest,
    }

    impl ChoicesUser {
        pub fn new(io: Arc<dyn UserIo>) -> Self {
            ChoicesUser {
                io,
                manifest: ToolManifest {
                    name: "choices_user".into(),
                    description: "Ask the user to pick one of several options and wait for the choice. Returns the chosen label. This is the only tool that asks the user a question: use it when a decision is genuinely theirs and the answers are enumerable, and give each option a one-paragraph description saying what picking it means. If the user exits the dialog without picking, the turn ends there and their next message reaches you. For a question that wants a free-text answer, do not call a tool — end the turn with the question in your reply text, and the user's next message is the answer.".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "prompt": { "type": "string", "description": "The question, as a short paragraph" },
                            "options": {
                                "type": "array",
                                "items": { "oneOf": [
                                    { "type": "string" },
                                    { "type": "object", "properties": { "label": { "type": "string" }, "description": { "type": "string" } }, "required": ["label"] }
                                ] }
                            }
                        },
                        "required": ["prompt", "options"]
                    }),
                    approval: Approval::ReadOnly,
                    prompt: None,
                    render: None,
                    deferred: false,
                },
            }
        }
    }

    #[async_trait]
    impl Tool for ChoicesUser {
        fn manifest(&self) -> &ToolManifest {
            &self.manifest
        }
        async fn call(&self, input: Value, ctx: CallContext) -> anyhow::Result<ToolOutput> {
            let prompt = input.get("prompt").and_then(Value::as_str).unwrap_or("?");
            let options: Vec<Choice> = input
                .get("options")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|o| match o {
                            Value::String(s) => Some(Choice {
                                label: s.clone(),
                                description: None,
                            }),
                            Value::Object(m) => Some(Choice {
                                label: m.get("label")?.as_str()?.to_string(),
                                description: m
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            }),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            if options.is_empty() {
                return Ok(ToolOutput::error("choices_user needs at least one option"));
            }
            Ok(match self.io.choose(prompt, &options, &ctx.cancel).await {
                Some(a) => ToolOutput::ok(a),
                // Exiting the dialog is not a failed call, and it is not an
                // error result either: an error is a reason for the model to
                // try again, and trying again is the box the operator just
                // closed. The result still journals — every call is
                // answered — and it asks the loop to settle the turn, so
                // the next word is the operator's.
                None => ToolOutput::settles(
                    "the question was dismissed without an answer, and the turn ended there. \
                     Wait for the operator's next message rather than asking again.",
                ),
            })
        }
    }
}
