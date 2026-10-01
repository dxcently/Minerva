//! Test doubles: a scripted provider and a scripted user. Public so
//! downstream crates (the CLI's `--provider mock`) can exercise the whole
//! loop — dispatch, policy, journaling, resume — without a network.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::message::{StopReason, Usage};
use crate::policy::{PolicyHook, Ruling};
use crate::provider::{ChatRequest, EventStream, Provider, StreamEvent};
use crate::tool::{ToolCall, ToolManifest};
use crate::user::{Choice, Reply, UserIo};

/// Plays back a fixed sequence of responses, one per model call. Each
/// response is a list of stream events; `Stop` is appended if missing.
/// Records every request it receives.
pub struct ScriptedProvider {
    responses: Mutex<std::collections::VecDeque<Vec<StreamEvent>>>,
    pub requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedProvider {
    pub fn new(responses: Vec<Vec<StreamEvent>>) -> Arc<Self> {
        Arc::new(ScriptedProvider {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// A text-only reply.
    pub fn text(s: &str) -> Vec<StreamEvent> {
        vec![StreamEvent::TextDelta(s.into()), StreamEvent::BlockStop]
    }

    /// A reply that calls one tool.
    pub fn tool(id: &str, name: &str, input: serde_json::Value) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolUseStart {
                id: id.into(),
                name: name.into(),
            },
            StreamEvent::ToolInputDelta(input.to_string()),
            StreamEvent::BlockStop,
            StreamEvent::Stop {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    output_tokens: 1,
                    ..Default::default()
                },
            },
        ]
    }
}

impl Provider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }

    fn stream<'a>(&'a self, req: ChatRequest, _cancel: CancellationToken) -> EventStream<'a> {
        self.requests.lock().unwrap().push(req);
        let mut events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Self::text("(no scripted response left)"));
        if !matches!(events.last(), Some(StreamEvent::Stop { .. })) {
            events.push(StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    output_tokens: 1,
                    ..Default::default()
                },
            });
        }
        events.insert(
            0,
            StreamEvent::Start {
                usage: Usage {
                    input_tokens: 10,
                    ..Default::default()
                },
            },
        );
        Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
    }
}

/// Answers every confirmation with a fixed bool, every multiple choice
/// with its first option; counts how often it was asked.
pub struct ScriptedUser {
    pub confirm: bool,
    pub asked: Mutex<Vec<String>>,
}

impl ScriptedUser {
    pub fn new(confirm: bool) -> Arc<Self> {
        Arc::new(ScriptedUser {
            confirm,
            asked: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl UserIo for ScriptedUser {
    async fn choose(
        &self,
        prompt: &str,
        options: &[Choice],
        _: &CancellationToken,
    ) -> Option<Reply> {
        self.asked.lock().unwrap().push(prompt.to_string());
        if self.confirm {
            options.first().map(|c| Reply::picked(&c.label))
        } else {
            None
        }
    }
}

/// A user whose answers wait until the test lets them go: the operator
/// mid-dialog. `entered` fires when a seam starts blocking, `release`
/// lets every waiter through with the answer the test set — the shape
/// that lets a test stand on the *other* side of a live question and
/// knock.
pub struct HeldUser {
    pub entered: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
    pub approve_with: std::sync::atomic::AtomicBool,
    pub respond_with: Mutex<Option<String>>,
    pub asked: Mutex<Vec<String>>,
}

impl Default for HeldUser {
    fn default() -> Self {
        HeldUser {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            approve_with: std::sync::atomic::AtomicBool::new(true),
            respond_with: Mutex::new(Some("the operator's word".into())),
            asked: Mutex::new(Vec::new()),
        }
    }
}

impl HeldUser {
    pub fn new() -> Arc<Self> {
        Arc::new(HeldUser::default())
    }
}

#[async_trait]
impl UserIo for HeldUser {
    async fn choose(
        &self,
        prompt: &str,
        options: &[Choice],
        _: &CancellationToken,
    ) -> Option<Reply> {
        use std::sync::atomic::Ordering;
        self.entered.notify_one();
        self.release.notified().await;
        self.asked.lock().unwrap().push(prompt.to_string());
        if self.approve_with.load(Ordering::SeqCst) {
            options.first().map(|c| Reply::picked(&c.label))
        } else {
            None
        }
    }

    async fn respond(&self, prompt: &str, _: &CancellationToken) -> Option<String> {
        self.entered.notify_one();
        self.release.notified().await;
        self.asked.lock().unwrap().push(prompt.to_string());
        self.respond_with.lock().unwrap().clone()
    }
}

/// A hook that puts every call in front of the user.
///
/// The harness ships only `AllowAll`, so without this the `Verdict::Ask`
/// path — the one a real policy will use to escalate a dangerous command —
/// would have no test coverage at all.
pub struct AlwaysAsk;

#[async_trait]
impl PolicyHook for AlwaysAsk {
    async fn pre_tool(&self, call: &ToolCall, _: &ToolManifest, _: &std::path::Path) -> Ruling {
        Ruling::ask(format!("Run `{}`?", call.name))
    }
}
