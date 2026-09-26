//! Spill: tool outputs too long for context overflow to a file the model
//! can page, instead of flooding the transcript.
//!
//! A tool can return more text than belongs in context. What travels
//! inline is capped ([`MAX_INLINE_CHARS`]); anything longer is written
//! whole to a spill file, and what is journaled and handed back is the
//! head plus the path. The model pages the rest with the `read` tool it
//! already has. The journal holds the bounded text the model saw, so a
//! resume replays the pointer rather than the flood.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use eidolon_core::dispatch::{MAX_INLINE_CHARS, SPILL_HEAD_CHARS};
use eidolon_core::policy::{AllowAll, Approval};
use eidolon_core::session::RecordKind;
use eidolon_core::tool::{CallContext, CallOrigin, Tool, ToolCall, ToolManifest, ToolOutput};
use eidolon_core::*;

struct Flood(ToolManifest);

#[async_trait]
impl Tool for Flood {
    fn manifest(&self) -> &ToolManifest {
        &self.0
    }
    async fn call(&self, _: serde_json::Value, _: CallContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::ok("x".repeat(MAX_INLINE_CHARS + 1_000)))
    }
}

struct Drip(ToolManifest);

#[async_trait]
impl Tool for Drip {
    fn manifest(&self) -> &ToolManifest {
        &self.0
    }
    async fn call(&self, _: serde_json::Value, _: CallContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::ok("short"))
    }
}

struct Boom(ToolManifest);

#[async_trait]
impl Tool for Boom {
    fn manifest(&self) -> &ToolManifest {
        &self.0
    }
    async fn call(&self, _: serde_json::Value, _: CallContext) -> anyhow::Result<ToolOutput> {
        // Error outputs are the model's debugging; they stay whole so a
        // denial or a traceback is never half a sentence.
        Ok(ToolOutput::error("e".repeat(MAX_INLINE_CHARS + 1_000)))
    }
}

fn manifest(name: &str) -> ToolManifest {
    ToolManifest {
        name: name.into(),
        description: "test tool".into(),
        input_schema: json!({"type":"object"}),
        approval: Approval::ReadOnly,
        prompt: None,
        render: None,
        deferred: false,
    }
}

fn dispatcher(dir: &std::path::Path) -> (Arc<Dispatcher>, Arc<Mutex<Session>>) {
    let log = dir.join("s.eid");
    let session = Arc::new(Mutex::new(
        Session::create(&log, "m", dir, None).unwrap(),
    ));
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(Flood(manifest("flood"))));
    reg.register(Arc::new(Drip(manifest("drip"))));
    reg.register(Arc::new(Boom(manifest("boom"))));
    let d = Arc::new(Dispatcher::new(
        reg,
        Arc::new(AllowAll),
        eidolon_core::testing::ScriptedUser::new(false),
        EventBus::default(),
        session.clone(),
        dir.to_path_buf(),
    ));
    d.set_spill_dir(Some(Dispatcher::spill_dir_for(&log)));
    (d, session)
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        input: json!({}),
        origin: CallOrigin::Model,
    }
}

/// A short output travels inline and writes no file.
#[tokio::test]
async fn a_short_output_is_not_spilled() {
    let dir = tempfile::tempdir().unwrap();
    let (d, session) = dispatcher(dir.path());
    let out = d.dispatch(call("short", "drip"), CancellationToken::new()).await;
    assert!(!out.is_error);
    assert_eq!(out.content, "short");
    assert!(
        !dir.path().join("s.spills").exists(),
        "no spill file was written"
    );
    let s = session.lock().await;
    let last = s
        .branch()
        .into_iter()
        .rev()
        .find_map(|r| match &r.kind {
            RecordKind::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(last, "short");
}

/// A long output spills: the file holds it whole, the model gets the head
/// plus the path, and the journal holds the bounded text.
#[tokio::test]
async fn a_long_output_spills_to_a_file_beside_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let (d, session) = dispatcher(dir.path());
    let out = d.dispatch(call("spill-flood", "flood"), CancellationToken::new()).await;
    assert!(!out.is_error, "spilling is not a failure: {}", out.content);
    assert!(
        out.content.chars().count() < MAX_INLINE_CHARS,
        "the receipt is bounded: {} chars",
        out.content.chars().count()
    );
    assert!(
        out.content.contains("spill-flood-flood.txt"),
        "the receipt names the spill file: {}",
        out.content
    );
    // The head is the output's own start.
    assert!(
        out.content.starts_with(&"x".repeat(SPILL_HEAD_CHARS)),
        "the head stays inline"
    );
    // The file holds the whole output.
    let spill = dir.path().join("s.spills").join("spill-flood-flood.txt");
    let body = std::fs::read_to_string(&spill).unwrap();
    assert_eq!(body.len(), MAX_INLINE_CHARS + 1_000);
    // The journal holds the bounded receipt, so a resume replays the
    // pointer rather than the flood.
    let s = session.lock().await;
    let last = s
        .branch()
        .into_iter()
        .rev()
        .find_map(|r| match &r.kind {
            RecordKind::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(last, out.content);
}

/// Error outputs are never spilled, however long.
#[tokio::test]
async fn an_error_output_is_never_spilled() {
    let dir = tempfile::tempdir().unwrap();
    let (d, _) = dispatcher(dir.path());
    let out = d.dispatch(call("spill-boom", "boom"), CancellationToken::new()).await;
    assert!(out.is_error);
    assert_eq!(out.content.len(), MAX_INLINE_CHARS + 1_000);
    assert!(
        !dir.path().join("s.spills").exists(),
        "no spill file was written"
    );
}

/// Without a spill directory the overflow lands beside the working
/// directory instead — the fallback for sessions with no log path.
#[tokio::test]
async fn without_a_spill_dir_the_overflow_lands_beside_the_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let session = Arc::new(Mutex::new(
        Session::create(&dir.path().join("s.eid"), "m", dir.path(), None).unwrap(),
    ));
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(Flood(manifest("flood"))));
    let d = Arc::new(Dispatcher::new(
        reg,
        Arc::new(AllowAll),
        eidolon_core::testing::ScriptedUser::new(false),
        EventBus::default(),
        session,
        dir.path().to_path_buf(),
    ));
    let out = d.dispatch(call("abc", "flood"), CancellationToken::new()).await;
    assert!(!out.is_error);
    let body = std::fs::read_to_string(dir.path().join("abc-flood.txt")).unwrap();
    assert_eq!(body.len(), MAX_INLINE_CHARS + 1_000);
}
