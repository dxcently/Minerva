//! Bundles: a checkpoint plus the rule that turns its verdict into a call.
//!
//! For the `mneme_rpc` target that rule is a *translation*, not a pass-through:
//! the checkpoint speaks the corpus's slot names and the vault takes its own
//! (`note`→`title`, `version`→`id`, `target`→`old_str`), and two arguments are
//! not strings on the wire. [`crate::reconcile`] holds that mapping — one
//! table, shared with the model's command channel — and this target calls it.
//! Dispatch mode used to send the corpus's names verbatim, which classified
//! correctly and then died at the vault on `-32602`; see that module for why.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use eidolon_core::tool::{CallOrigin, ToolCall};

use crate::dispatcher::{VerbaDispatcher, Verdict};
use crate::reconcile::reconcile;

/// What a bundle's intents name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// Intents are Mneme functions; the call is `mneme_rpc { function, args }`.
    MnemeRpc,
    /// Intents are harness tool names; slots are the tool input.
    Tools,
}

pub struct Bundle {
    pub name: String,
    pub target: Target,
    pub dispatcher: Arc<VerbaDispatcher>,
}

impl Bundle {
    pub fn new(name: impl Into<String>, target: Target, dispatcher: VerbaDispatcher) -> Self {
        Bundle {
            name: name.into(),
            target,
            dispatcher: Arc::new(dispatcher),
        }
    }

    /// The call this bundle makes of an accepted verdict.
    pub fn call_for(&self, v: &Verdict) -> ToolCall {
        match self.target {
            // The corpus's names are not the vault's: reconcile before calling.
            // A `Target::Tools` bundle needs no such step — the harness tool's
            // own schema governs its input, and there is no second vocabulary
            // to translate between.
            Target::MnemeRpc => {
                let (function, args) = reconcile(&v.intent, &v.slots);
                mneme_call(&function, args, CallOrigin::User, &self.name)
            }
            Target::Tools => ToolCall {
                id: call_id(&self.name),
                name: v.intent.clone(),
                input: Value::Object(coerce_slots(&v.slots)),
                origin: CallOrigin::User,
            },
        }
    }
}

/// The id of a call the harness generated itself.
///
/// Unique **within the process**, and not by the clock alone: the dispatcher
/// answers a call whose id already has a `ToolResult` on the branch out of the
/// journal, without running it — that is resume-as-replay, and the id is the
/// only thing it is keyed on. Millisecond resolution is not enough for that
/// key, because the inline channel runs every command one reply marked, back
/// to back: two lines written together are resolved in the same millisecond as
/// a matter of course, and the second call would be handed the first's answer
/// without reaching the vault. The counter is what keeps them apart; the clock
/// stays in the id so a later process does not reuse an earlier one's. The
/// other generators of a locally-made call id — the rune host's `fresh_id` —
/// are this same clock-plus-counter shape.
fn call_id(label: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("vv_{label}_{}_{n}", now_ms())
}

/// `mneme_rpc`'s input: one function and its named arguments, which is the
/// whole wire vocabulary this target needs.
///
/// An `origin` that is not [`CallOrigin::User`] is what a **loop-side**
/// command uses, and it is not cosmetic: the dispatcher journals a
/// `UserToolCall` before running a user-originated call, and the session's
/// message assembly flushes the assistant's open `tool_use` when it meets
/// one — so a nested call made while the model's own `vv` call is still
/// unanswered would come back to it as "interrupted before it produced a
/// result". Harness code running on the model's behalf is a script.
pub fn mneme_call(
    function: &str,
    args: Map<String, Value>,
    origin: CallOrigin,
    label: &str,
) -> ToolCall {
    ToolCall {
        id: call_id(label),
        name: "mneme_rpc".into(),
        input: serde_json::json!({ "function": function, "args": Value::Object(args) }),
        origin,
    }
}

/// A resolved utterance: which bundle accepted it and the call to make.
pub struct Resolution {
    pub bundle: String,
    pub verdict: Verdict,
    pub call: ToolCall,
}

#[derive(Default)]
pub struct Palette {
    bundles: Vec<Bundle>,
}

impl Palette {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, b: Bundle) {
        self.bundles.push(b);
    }

    pub fn is_empty(&self) -> bool {
        self.bundles.is_empty()
    }

    pub fn names(&self) -> Vec<&str> {
        self.bundles.iter().map(|b| b.name.as_str()).collect()
    }

    /// Resolve against **one** named bundle.
    ///
    /// [`resolve`](Self::resolve) is for a caller with a line of text and
    /// no idea which vocabulary it is in. A caller that has already been
    /// told — the TUI's dispatch mode, where the operator entered the
    /// surface before typing — wants this instead: trying the other
    /// bundles there could only turn a mistyped vault command into a
    /// shell one. The verdict comes back either way, accepted or not, so
    /// an abstention can say what it was close to.
    pub async fn resolve_on(
        &self,
        name: &str,
        utterance: &str,
    ) -> anyhow::Result<(Option<Resolution>, Verdict)> {
        let b = self
            .bundles
            .iter()
            .find(|b| b.name == name)
            .with_context(|| format!("no verba bundle named `{name}`"))?;
        let v = b.dispatcher.classify(utterance).await?;
        let call = v.accepted().then(|| b.call_for(&v));
        Ok((
            call.map(|call| Resolution {
                bundle: b.name.clone(),
                verdict: v.clone(),
                call,
            }),
            v,
        ))
    }

    /// First bundle that accepts wins. Errors from a bundle are logged and
    /// treated as abstention; the verdicts of the abstaining bundles come
    /// back so a caller can show what was close.
    pub async fn resolve(&self, utterance: &str) -> (Option<Resolution>, Vec<(String, Verdict)>) {
        let mut abstained = Vec::new();
        for b in &self.bundles {
            match b.dispatcher.classify(utterance).await {
                Ok(v) if v.accepted() => {
                    let call = b.call_for(&v);
                    return (
                        Some(Resolution {
                            bundle: b.name.clone(),
                            verdict: v,
                            call,
                        }),
                        abstained,
                    );
                }
                Ok(v) => abstained.push((b.name.clone(), v)),
                Err(e) => {
                    tracing::warn!(bundle = %b.name, error = %e, "verba bundle failed; treating as abstention")
                }
            }
        }
        (None, abstained)
    }
}

/// Slot values arrive as JSON: mostly strings, but a list-valued slot is
/// already an array. Where one is a string, give the obvious ones their JSON
/// type so a `replace_all: "true"` reaches a harness tool as a bool.
///
/// A guess, and only for the `tools` target, where the tool's own schema is the
/// only thing either side has read. The `mneme_rpc` target does **not** use
/// this: Mneme's `read_version.id` is a *string*, so a stringly number coerced
/// here is a rejected call — that target takes each argument's wire type from
/// [`crate::reconcile`]'s table and coerces only where it says so.
pub fn coerce_slots(slots: &BTreeMap<String, Value>) -> Map<String, Value> {
    slots
        .iter()
        .map(|(k, v)| {
            let val = match v.as_str() {
                Some("true") => Value::Bool(true),
                Some("false") => Value::Bool(false),
                Some(s) => s
                    .parse::<i64>()
                    .map(Value::from)
                    .unwrap_or_else(|_| Value::String(s.to_string())),
                None => v.clone(),
            };
            (k.clone(), val)
        })
        .collect()
}

pub(crate) fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_to_mneme_call() {
        let mut slots = std::collections::BTreeMap::new();
        slots.insert("note".into(), "Groceries".into());
        slots.insert("target".into(), "eggs".into());
        slots.insert("content".into(), "milk".into());
        slots.insert("replace_all".into(), "true".into());
        let v = Verdict {
            utterance: "x".into(),
            delex: "x".into(),
            intent: "edit_note".into(),
            intent_prob: 0.9,
            margin: 0.5,
            threshold: Some(0.3),
            accept: Some(true),
            slots,
            candidates: vec![],
            ..Default::default()
        };
        assert!(v.accepted());
        let b = Bundle::new(
            "mneme",
            Target::MnemeRpc,
            VerbaDispatcher::new("verba-volantia", "/nowhere"),
        );
        let call = b.call_for(&v);
        assert_eq!(call.name, "mneme_rpc");
        assert_eq!(call.input["function"], "edit_note");
        // The corpus's names are not the vault's — this is the translation
        // dispatch mode used to skip, and the reason `eidolon do 'edit note …'`
        // came back with `missing field old_str`.
        assert_eq!(call.input["args"]["title"], "Groceries");
        assert_eq!(call.input["args"]["old_str"], "eggs");
        assert_eq!(call.input["args"]["new_str"], "milk");
        assert_eq!(call.input["args"]["replace_all"], true);
        assert!(call.input["args"].get("note").is_none());
    }

    /// The other target keeps the coercion: a harness tool's schema is what
    /// governs its input, and there is no second vocabulary to translate.
    #[test]
    fn tools_keep_the_obvious_coercion() {
        let mut slots = std::collections::BTreeMap::new();
        slots.insert("note".into(), "Groceries".into());
        slots.insert("replace_all".into(), "true".into());
        slots.insert("k".into(), "5".into());
        let v = Verdict {
            intent: "some_tool".into(),
            accept: Some(true),
            slots,
            ..Default::default()
        };
        let b = Bundle::new("tools", Target::Tools, VerbaDispatcher::new("verba-volantia", "/nowhere"));
        let call = b.call_for(&v);
        assert_eq!(call.name, "some_tool");
        assert_eq!(call.input["replace_all"], true);
        assert_eq!(call.input["k"], 5);
        assert_eq!(call.input["note"], "Groceries");
    }

    /// Ids are the dispatcher's replay key, and a reply's commands are
    /// resolved back to back — so these are generated in a tight loop, the way
    /// the inline channel generates them, and every one of them has to be its
    /// own call. Two sharing an id is the second one being answered out of the
    /// journal without ever reaching the vault.
    #[test]
    fn generated_call_ids_do_not_collide() {
        let ids: Vec<String> = (0..256)
            .map(|_| mneme_call("search_vault", Map::new(), CallOrigin::Script, "mneme").id)
            .collect();
        let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "two commands share an id: {ids:?}");

        // The other call site, `Target::Tools`: same question.
        let b = Bundle::new("tools", Target::Tools, VerbaDispatcher::new("verba-volantia", "/nowhere"));
        let v = Verdict {
            intent: "some_tool".into(),
            accept: Some(true),
            ..Default::default()
        };
        let ids: Vec<String> = (0..256).map(|_| b.call_for(&v).id).collect();
        let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "two dispatches share an id: {ids:?}");
    }
}
