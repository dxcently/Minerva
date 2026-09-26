//! Explicit live probe: cargo run -p eidolon-providers --example ollama_anthropic_probe
//! Up to three calls per turn, two turns, 180 seconds total; synthetic data only.
//! Uses ollama.com directly, without changing configured providers or logging credentials.
use std::sync::{Arc, Mutex as StdMutex};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::json;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use eidolon_core::{Agent, AgentConfig, Dispatcher, EventBus, Session, Tool, ToolManifest, ToolRegistry};
use eidolon_core::message::{ContentBlock, Message, StopReason};
use eidolon_core::policy::{AllowAll, Approval};
use eidolon_core::provider::{ChatRequest, EventStream, Provider, StreamEvent, ThinkingConfig};
use eidolon_core::session::RecordKind;
use eidolon_core::testing::ScriptedUser;
use eidolon_core::tool::{CallContext, ToolOutput};
use eidolon_providers::{HttpProvider, ProviderScript};

struct ProbeTool { manifest: ToolManifest, receipt: String }
#[async_trait]
impl Tool for ProbeTool {
    fn manifest(&self) -> &ToolManifest { &self.manifest }
    async fn call(&self, input: serde_json::Value, _: CallContext) -> Result<ToolOutput> {
        ensure!(input == json!({"label":"cloud-probe"}), "unexpected probe tool arguments");
        Ok(ToolOutput::ok(self.receipt.clone()))
    }
}

struct Observed {
    inner: HttpProvider,
    requests: StdMutex<Vec<ChatRequest>>,
    events: StdMutex<Vec<Vec<StreamEvent>>>,
}
impl Provider for Observed {
    fn name(&self) -> &str { "ollama-anthropic-probe" }
    fn stream<'a>(&'a self, req: ChatRequest, cancel: CancellationToken) -> EventStream<'a> {
        let index = {
            let mut requests = self.requests.lock().unwrap();
            let index = requests.len();
            requests.push(req.clone());
            self.events.lock().unwrap().push(Vec::new());
            index
        };
        Box::pin(self.inner.stream(req, cancel).inspect(move |event| {
            if let Ok(event) = event { self.events.lock().unwrap()[index].push(event.clone()); }
        }))
    }
}

fn agent(provider: Arc<Observed>, session: Arc<Mutex<Session>>, dir: &std::path::Path, receipt: &str) -> Agent {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ProbeTool {
        manifest: ToolManifest {
            name: "probe_receipt".into(), description: "Return the test receipt; call exactly once with label cloud-probe.".into(),
            input_schema: json!({"type":"object","properties":{"label":{"type":"string","enum":["cloud-probe"]}},
                "required":["label"],"additionalProperties":false}),
            approval: Approval::ReadOnly, prompt: None, render: None, deferred: false,
        },
        receipt: receipt.into(),
    }));
    let dispatcher = Arc::new(Dispatcher::new(registry, Arc::new(AllowAll), ScriptedUser::new(false),
        EventBus::default(), session, dir.to_path_buf()));
    Agent::new(provider, dispatcher, AgentConfig {
        model: "deepseek-v4.1-flash".into(), model_key: "ollama-anthropic-probe:deepseek-v4.1-flash".into(),
        system: Some("This is a small protocol test. Follow the user's instructions precisely; do not perform extra work.".into()),
        max_tokens: 4096, thinking: Some(ThinkingConfig { budget_tokens: 1024 }), max_iterations: 3,
        ..Default::default()
    })
}

fn thinking(messages: &[Message]) -> Vec<ContentBlock> {
    messages.iter().flat_map(|m| &m.content).filter(|b| matches!(b, ContentBlock::Thinking { .. })).cloned().collect()
}

async fn probe(cancel: CancellationToken) -> Result<()> {
    let store = Arc::new(harnox::secrets::SecretStore::at(dirs::config_dir().context("config directory")?.join("eidolon/secrets")));
    let (def, script) = ProviderScript::load_with("ollama", include_str!("../builtin/ollama.rn"), Some(store.clone()))?;
    ensure!(def.wire == harnox::llm::Wire::Anthropic && def.base_url == "https://ollama.com",
        "the shipped Ollama provider must use direct cloud Anthropic");
    let provider = Arc::new(Observed { inner: HttpProvider::with_store(def, Some(script), Some(store)),
        requests: StdMutex::new(Vec::new()), events: StdMutex::new(Vec::new()) });
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cloud-probe.eid");
    let receipt = format!("receipt-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos());
    let session = Arc::new(Mutex::new(Session::create(&path, "ollama-anthropic-probe:deepseek-v4.1-flash", dir.path(), None)?));
    let first_agent = agent(provider.clone(), session.clone(), dir.path(), &receipt);
    let outcome = first_agent.run_turn(vec![ContentBlock::text(
        "Call probe_receipt exactly once with label cloud-probe. Then reply with exactly the receipt returned by that tool. Do not guess it."
    )], cancel.child_token()).await?;
    println!("tool turn: {outcome:?}");
    let messages = {
        let s = session.lock().await;
        ensure!(s.is_settled(), "tool turn did not settle");
        let results: Vec<_> = s.records().iter().filter_map(|r| match &r.kind {
            RecordKind::ToolResult { content, is_error, .. } => Some((content, is_error)), _ => None,
        }).collect();
        ensure!(results.len() == 1 && results[0].0 == &receipt && !results[0].1, "tool was not dispatched exactly once successfully");
        let messages = s.messages();
        ensure!(messages.last().context("missing final message")?.text() == receipt, "final answer did not match tool result");
        ensure!(!thinking(&messages).is_empty(), "no thinking was journaled");
        messages
    };
    {
        let requests = provider.requests.lock().unwrap();
        ensure!(requests.len() == 2, "expected one tool response and one final response, got {} requests", requests.len());
        let replayed = thinking(&requests[1].messages);
        ensure!(replayed.iter().any(|b| matches!(b, ContentBlock::Thinking { thinking, signature } if !thinking.is_empty() && signature.is_empty())),
            "unsigned thinking missing from tool continuation");
        let body = harnox::llm::anthropic::Anthropic::request_body(&requests[1]);
        let wire = body["messages"].as_array().context("wire messages")?;
        ensure!(wire.iter().any(|m| m["content"].as_array().is_some_and(|blocks| blocks.iter().any(|b|
            b["type"] == "thinking" && b["signature"] == ""))), "wire omitted unsigned thinking");
        ensure!(wire.iter().any(|m| m["content"].as_array().is_some_and(|blocks| blocks.iter().any(|b|
            b["type"] == "tool_result" && b["content"] == receipt))), "wire omitted tool result");
        println!("PASS: dispatched tool once; continuation includes unsigned thinking and tool result; final answer matches receipt");
    }
    drop(first_agent);
    drop(session);
    let reopened = Session::open(&path)?;
    ensure!(reopened.messages() == messages, "journal replay changed messages");
    let resumed = Arc::new(Mutex::new(reopened));
    let second_agent = agent(provider.clone(), resumed.clone(), dir.path(), &receipt);
    let outcome = second_agent.run_turn(vec![ContentBlock::text(
        "Without calling any tool, repeat exactly the receipt from the previous turn."
    )], cancel.child_token()).await?;
    println!("resumed turn: {outcome:?}");
    let s = resumed.lock().await;
    ensure!(s.is_settled() && s.messages().last().context("missing resumed answer")?.text() == receipt, "resumed answer failed");
    let requests = provider.requests.lock().unwrap();
    ensure!(requests.len() == 3, "resumed turn unexpectedly used extra calls");
    ensure!(thinking(&requests[2].messages) == thinking(&messages), "resumed request lost thinking");
    let events = provider.events.lock().unwrap();
    for (i, call) in events.iter().enumerate() {
        ensure!(matches!(call.first(), Some(StreamEvent::Start { .. })), "call {} missing Start", i + 1);
        ensure!(call.iter().filter(|e| matches!(e, StreamEvent::Stop { .. })).count() == 1, "call {} Stop count", i + 1);
        ensure!(matches!(call.last(), Some(StreamEvent::Stop { stop_reason, .. }) if *stop_reason == if i == 0 {StopReason::ToolUse} else {StopReason::EndTurn}),
            "call {} unexpected stop", i + 1);
        let thinking_chars: usize = call.iter().filter_map(|e| match e {StreamEvent::ThinkingDelta(t) => Some(t.len()), _ => None}).sum();
        println!("call {}: thinking_bytes={thinking_chars}, usage events={:?}", i + 1,
            call.iter().filter(|e| matches!(e, StreamEvent::Start { .. } | StreamEvent::Stop { .. })).collect::<Vec<_>>());
    }
    println!("PASS: journal reopen preserves exact messages; resumed cloud request accepts history and returns receipt without tools");
    println!("Cache controls used default short retention; cache hits are not guaranteed by this tiny probe.");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cancel = CancellationToken::new();
    tokio::select! {
        result = probe(cancel.clone()) => result,
        _ = tokio::time::sleep(std::time::Duration::from_secs(180)) => {
            cancel.cancel();
            anyhow::bail!("live probe exceeded its 180-second total bound")
        }
    }
}
