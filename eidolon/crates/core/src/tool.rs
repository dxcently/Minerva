//! Tool contracts: the manifest a tool publishes, the call the loop makes,
//! and the registry that maps one to the other.
//!
//! The manifest shape is deliberately MCP's `tools/list` shape plus two
//! harness fields (`approval`, `render`). That is what Mneme's RPC
//! `list`/`schema`, Melete's connector and the pi-extensions engine all
//! already produce, and it is the single most reusable design carried over
//! from `pi-extensions` — keep it whole.
//!
//! A `Tool` is *anything* callable by name with a JSON input. Built-in tools
//! are Rune scripts wrapping Rust primitives; user skills are Rune scripts;
//! `choices_user` is a tool. There is no privileged native path, so nothing can
//! bypass the dispatcher by being "built in".

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::policy::Approval;

/// What a tool tells the model and the harness about itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolManifest {
    pub name: String,
    pub description: String,
    /// JSON Schema for the `input` object.
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub approval: Approval,
    /// Optional text to splice into the system prompt when this tool is
    /// active (usage guidance the description is too short for).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Free-form hints for a renderer (how to draw the call and result).
    /// The core never reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render: Option<serde_json::Value>,
    /// Registered but not offered: the tool is callable by name and
    /// classified like any other, but its definition stays out of the
    /// request until the branch reaches for it — through `tool_search`, or
    /// by calling it. See [`crate::tool_search`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deferred: bool,
}

impl ToolManifest {
    /// A manifest for a tool the harness does not own — a backend's built-in
    /// (`Read`, `Bash`) seen through a hook. Enough for the policy hook to
    /// classify; never shown to a model.
    pub fn synthetic(name: &str, approval: Approval) -> Self {
        ToolManifest {
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({ "type": "object" }),
            approval,
            prompt: None,
            render: None,
            deferred: false,
        }
    }

    /// The subset the model sees. Approval and render hints are harness
    /// business; the provider gets harnox's `ToolDef`.
    pub fn for_model(&self) -> crate::provider::ToolDef {
        crate::provider::ToolDef {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }
}

/// Where a call came from. Policy may treat these differently (a script
/// asking a question is not the model asking to run `rm`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOrigin {
    /// A `tool_use` block in an assistant message.
    Model,
    /// A running Rune script (a skill, a built-in, a panel).
    Script,
    /// The person at the keyboard (slash command, palette).
    User,
}

/// One request to run a tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique per session. Model-originated calls reuse the `tool_use` id
    /// so the result can be paired; other origins mint one.
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    pub origin: CallOrigin,
}

/// What a tool produced. `content` is what goes back to the model verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    /// The tool asked the loop to settle the turn once this result is
    /// journaled, instead of handing it to the model as a reason to carry
    /// on. `choices_user` sets it when the operator exits the dialog
    /// without answering: fed back as an ordinary result, the model's only
    /// move is to ask again, which is the loop the dismissal was closing.
    /// Deliberately **not journaled** — the result's own text tells the
    /// next turn what happened, and a replayed call (a crash before the
    /// settle landed) resumes the interrupted turn rather than re-ending
    /// it. The loop settles on [`StopReason::Dismissed`].
    pub ends_turn: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        ToolOutput {
            content: content.into(),
            is_error: false,
            ends_turn: false,
        }
    }
    pub fn error(content: impl Into<String>) -> Self {
        ToolOutput {
            content: content.into(),
            is_error: true,
            ends_turn: false,
        }
    }
    /// A result that is not a failure but ends the turn — see
    /// [`ToolOutput::ends_turn`].
    pub fn settles(content: impl Into<String>) -> Self {
        ToolOutput {
            content: content.into(),
            is_error: false,
            ends_turn: true,
        }
    }
}

/// Per-call environment handed to a tool. Grows as tools need more: the
/// working directory, the cancellation token, and — since `wait_for` had to
/// name the call that armed a park, so a fire can pair with its
/// registration on the branch — the call's own id.
#[derive(Clone)]
pub struct CallContext {
    pub cwd: std::path::PathBuf,
    pub cancel: CancellationToken,
    /// The `tool_use` id this call arrived with, as the journal knows it.
    /// A tool that writes a record of its own pairs it by this; a tool that
    /// does not never reads it.
    pub call_id: String,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn manifest(&self) -> &ToolManifest;
    async fn call(&self, input: serde_json::Value, ctx: CallContext) -> anyhow::Result<ToolOutput>;
}

/// Name → tool. Insertion order is preserved for the model-facing list so
/// prompts are stable across runs.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    order: Vec<String>,
    /// Guidance that belongs to no tool — see [`ToolRegistry::register_prompt`].
    prompts: Vec<(String, String)>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register, replacing any tool of the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.manifest().name.clone();
        if self.tools.insert(name.clone(), tool).is_none() {
            self.order.push(name);
        }
    }

    /// Register guidance that belongs to **no tool**, spliced into the
    /// system prompt's tool notes under `heading` — for a surface that has
    /// no manifest to hang a `prompt` on.
    ///
    /// A manifest's `prompt` is the usual place for this, and it is the
    /// right one whenever a tool exists: the notes join the prompt when the
    /// tool does and leave when it does. But a surface can be a *channel*
    /// rather than a tool — the model writes something in its prose and the
    /// loop answers it — and then there is nothing to register, nothing to
    /// classify, and nothing in the request's tool list; the teaching still
    /// has to be somewhere. This is that somewhere. Registering the same
    /// heading again replaces it, so what the prompt says cannot accumulate
    /// as a session's registry is rebuilt.
    pub fn register_prompt(&mut self, heading: impl Into<String>, text: impl Into<String>) {
        let heading = heading.into();
        let text = text.into();
        match self.prompts.iter_mut().find(|(h, _)| *h == heading) {
            Some(slot) => slot.1 = text,
            None => self.prompts.push((heading, text)),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    pub fn manifests(&self) -> Vec<ToolManifest> {
        self.order
            .iter()
            .filter_map(|n| self.tools.get(n))
            .map(|t| t.manifest().clone())
            .collect()
    }

    /// The model-facing projection, in registration order.
    pub fn defs(&self) -> Vec<crate::provider::ToolDef> {
        self.order
            .iter()
            .filter_map(|n| self.tools.get(n))
            .map(|t| t.manifest().for_model())
            .collect()
    }

    /// The registered tools' `prompt` sections, in registration order.
    ///
    /// A manifest's `description` has to stay short: it is sent on every
    /// request, once per tool, and a paragraph there is a paragraph the
    /// model re-reads for the whole session. `prompt` is for the guidance
    /// that does not fit — when to reach for the tool, what its failure
    /// modes mean, what to do afterwards — spliced into the system prompt
    /// once, and only while the tool is actually registered.
    ///
    /// `None` when no tool is offered on the request, so a prompt with no
    /// tools gains no trailing section.
    pub fn guidance(&self) -> Option<String> {
        self.guidance_for(&self.order)
    }

    /// [`ToolRegistry::guidance`] for the tools actually offered on a
    /// request — a deferred tool's notes join the prompt when the tool
    /// does, and not before. `names` is in registration order already
    /// when it comes from [`crate::tool_search::offered`], and is walked
    /// in registration order here regardless.
    ///
    /// Guidance registered with [`ToolRegistry::register_prompt`] is not a
    /// tool's and is not filtered: a channel that has no tool at all would
    /// otherwise never reach the model while the request still carries
    /// tools for it to call. It sits after the channel note about calling
    /// tools, which is about every tool at once, and before the tools' own
    /// sections.
    pub fn guidance_for(&self, names: &[String]) -> Option<String> {
        let prompts: Vec<String> = self
            .prompts
            .iter()
            .filter_map(|(heading, text)| {
                let text = text.trim();
                (!text.is_empty()).then(|| format!("### {heading}\n{text}"))
            })
            .collect();
        let sections: Vec<String> = self
            .order
            .iter()
            .filter(|n| names.contains(n))
            .filter_map(|n| self.tools.get(n))
            .filter_map(|t| {
                let m = t.manifest();
                let p = m.prompt.as_deref()?.trim();
                (!p.is_empty()).then(|| format!("### {}\n{p}", m.name))
            })
            .collect();
        if sections.is_empty() && prompts.is_empty() && names.is_empty() {
            return None;
        }
        // The channel note leads, because it is about every tool at once
        // rather than any one of them. Models differ enormously in how
        // much use they make of it — some batch five reads into a reply
        // unprompted, and glm on this machine averaged 1.12 tool calls
        // per message, one tool per iteration, which is the difference
        // between a turn that fits in the iteration budget and one that
        // hits the wall at a quarter of the work. The harness supports
        // parallel calls on every wire; a model that already knows has
        // lost one sentence, and one that does not has gained half a
        // turn.
        const CHANNEL: &str = "## Tool notes\n\n### Calling tools\nSeveral tool calls may \
be made in one message, and all of their results return together before you next speak. \
When calls are independent of each other — several reads, searches, or edits in different \
files — make them together in one message rather than one per message.";
        let rest: Vec<String> = prompts.into_iter().chain(sections).collect();
        Some(if rest.is_empty() {
            CHANNEL.to_string()
        } else {
            format!("{CHANNEL}\n\n{}", rest.join("\n\n"))
        })
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Stub(ToolManifest);

    #[async_trait]
    impl Tool for Stub {
        fn manifest(&self) -> &ToolManifest {
            &self.0
        }
        async fn call(&self, _: serde_json::Value, _: CallContext) -> anyhow::Result<ToolOutput> {
            Ok(ToolOutput::ok(""))
        }
    }

    fn tool(name: &str, prompt: Option<&str>) -> Arc<dyn Tool> {
        Arc::new(Stub(ToolManifest {
            name: name.into(),
            description: String::new(),
            input_schema: json!({ "type": "object" }),
            approval: Approval::ReadOnly,
            prompt: prompt.map(str::to_string),
            render: None,
            deferred: false,
        }))
    }

    #[test]
    fn a_request_with_no_tools_offered_adds_no_section() {
        let mut r = ToolRegistry::new();
        r.register(tool("read", None));
        // Whitespace is not guidance, and no tools offered is no section:
        // the channel note says how to *call* tools, and there is nothing
        // to call.
        r.register(tool("write", Some("   \n ")));
        assert_eq!(r.guidance_for(&[]), None);
        assert_eq!(ToolRegistry::new().guidance(), None);
    }

    #[test]
    fn tools_without_notes_still_get_the_channel_note() {
        let mut r = ToolRegistry::new();
        r.register(tool("read", None));
        r.register(tool("write", Some("   \n ")));
        let g = r.guidance().unwrap();
        assert!(
            g.starts_with("## Tool notes\n\n### Calling tools\n"),
            "the channel note leads: {g}"
        );
        assert!(
            g.contains("in one message"),
            "it says the one thing it exists to say: {g}"
        );
        // Whitespace prompts still contribute no heading of their own.
        assert!(!g.contains("### write"));
    }

    #[test]
    fn guidance_follows_registration_order() {
        let mut r = ToolRegistry::new();
        r.register(tool("read", Some("read first")));
        r.register(tool("bash", None));
        r.register(tool("edit", Some("match exactly")));
        let g = r.guidance().unwrap();
        assert_eq!(
            g,
            "## Tool notes\n\n### Calling tools\nSeveral tool calls may be made in one \
message, and all of their results return together before you next speak. When calls are \
independent of each other — several reads, searches, or edits in different files — make \
them together in one message rather than one per message.\n\n### read\nread first\n\n\
### edit\nmatch exactly"
        );
        // A tool that declares nothing contributes no heading.
        assert!(!g.contains("### bash"));
    }

    #[test]
    fn guidance_tracks_what_is_registered() {
        let mut r = ToolRegistry::new();
        r.register(tool("read", Some("read first")));
        assert!(r.guidance().unwrap().contains("read first"));
        // Replacing a tool replaces its guidance rather than accumulating.
        r.register(tool("read", Some("something else")));
        let g = r.guidance().unwrap();
        assert!(g.contains("something else") && !g.contains("read first"));
    }

    /// A surface with no manifest still has teaching to do — the inline
    /// command channel is the one that does — and it must reach the prompt
    /// whether or not the request carries any tools, because the model's
    /// prose is the whole of how it is reached.
    #[test]
    fn a_prompt_with_no_tool_behind_it_still_reaches_the_prompt() {
        let mut r = ToolRegistry::new();
        r.register_prompt("Vault commands", "write `! read the note X` and stop");
        let alone = r.guidance().unwrap();
        assert_eq!(
            alone,
            "## Tool notes\n\n### Calling tools\nSeveral tool calls may be made in one \
message, and all of their results return together before you next speak. When calls are \
independent of each other — several reads, searches, or edits in different files — make \
them together in one message rather than one per message.\n\n### Vault commands\n\
write `! read the note X` and stop",
            "the channel note leads and the prompt-only section follows it"
        );
        // It is not a tool: the model-facing list and the name lookup are
        // untouched by it, and it is not filtered by what is offered.
        assert!(r.get("Vault commands").is_none());
        assert!(r.defs().is_empty());
        assert_eq!(r.manifests().len(), 0);

        // With tools as well, the prompt-only section comes before the
        // tools' own, and a deferred tool's notes still wait for the tool.
        let mut r = ToolRegistry::new();
        r.register_prompt("Vault commands", "inline");
        r.register(tool("read", Some("read first")));
        let g = r.guidance_for(&["read".into()]).unwrap();
        assert!(
            g.find("### Vault commands").unwrap() < g.find("### read").unwrap(),
            "{g}"
        );
        // Re-registering the same heading replaces it.
        r.register_prompt("Vault commands", "inline, revised");
        let g = r.guidance().unwrap();
        assert!(g.contains("inline, revised") && !g.contains("\ninline\n"), "{g}");
        // Whitespace is not teaching.
        r.register_prompt("Nothing", "   ");
        assert!(!r.guidance_for(&[]).unwrap().contains("### Nothing"));
    }
}
