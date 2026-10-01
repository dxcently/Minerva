//! The gated stateless model call: the `ask_model` built-in over the
//! `model_call` primitive.
//!
//! What is proved here is the *shape* the workflow interpreter (slice D)
//! and Melete's ledger will both call against: the tool exists only where
//! a completion path was attached, and its answer is Melete's envelope —
//! always one plain string carrying `{"ok":true,"reply":…}` or
//! `{"ok":false,"error":…}`, whatever went wrong. The happy arm's last
//! link — a stream that produces text — is the providers' completion
//! tests' to prove; over the scripted mock it answers `ok:false` with the
//! no-text reason, which is itself one of the shapes a script must
//! survive.

use std::sync::Arc;

use eidolon_core::tool::{CallContext, Tool, ToolRegistry};
use eidolon_providers::Catalog;
use eidolon_rune::{Host, MODEL_BUILTINS, register_builtins};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn ctx(dir: &std::path::Path) -> CallContext {
    CallContext {
        cwd: dir.to_path_buf(),
        cancel: CancellationToken::new(),
        call_id: "test".into(),
    }
}

fn host_with_completion() -> Arc<Host> {
    let host = Host::new(std::env::temp_dir().join("eidolon-model-call-test"));
    host.attach_completion(eidolon_rune::host::Completion {
        catalog: Arc::new(Catalog::new()),
        default_model: "mock".into(),
        max_tokens: 1024,
    });
    host
}

/// The registered tool, straight off the shipped source.
fn ask_model(host: &Arc<Host>) -> Arc<dyn Tool> {
    let (name, src) = MODEL_BUILTINS[0];
    assert_eq!(name, "ask_model");
    Arc::new(eidolon_rune::ScriptTool::compile(name, src, host.clone()).unwrap())
}

async fn call(tool: &dyn Tool, dir: &std::path::Path, input: Value) -> String {
    tool.call(input, ctx(dir)).await.unwrap().content
}

#[tokio::test]
async fn without_a_completion_path_the_call_says_so_in_the_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::new(dir.path().to_path_buf());
    // The tool does not even register: the capability is the catalog, as
    // the search key is for `search`.
    let mut reg = ToolRegistry::new();
    register_builtins(&mut reg, &host, dir.path()).unwrap();
    assert!(reg.get("ask_model").is_none(), "no catalog, no tool");
    // But the shipped source still answers, loudly, in the envelope —
    // never silently, and never as anything but the one shape.
    let out = call(ask_model(&host).as_ref(), dir.path(), json!({ "prompt": "hi" })).await;
    let envelope: Value = serde_json::from_str(&out).expect("the envelope is JSON text");
    assert_eq!(envelope["ok"], false);
    assert!(
        envelope["error"]
            .as_str()
            .unwrap()
            .contains("no completion path"),
        "{envelope}"
    );
}

#[tokio::test]
async fn with_a_path_the_envelope_is_always_one_string_whatever_the_input() {
    let dir = tempfile::tempdir().unwrap();
    let host = host_with_completion();
    // Missing, empty, and mistyped prompts; a mistyped model; history in
    // the wrong shape — each is the ok:false arm, each a string.
    for (bad, reason) in [
        (json!({}), "prompt"),
        (json!({ "prompt": "   " }), "prompt"),
        (json!({ "prompt": "hi", "model": 42 }), "model"),
        (
            json!({ "prompt": "hi", "history": [{ "role": "system", "content": "x" }] }),
            "role",
        ),
        (json!({ "prompt": "hi", "history": "yesterday" }), "history"),
    ] {
        let out = call(ask_model(&host).as_ref(), dir.path(), bad).await;
        let envelope: Value = serde_json::from_str(&out).expect("the envelope is JSON text");
        assert_eq!(envelope["ok"], false, "{envelope}");
        assert!(
            envelope["error"].as_str().unwrap().contains(reason),
            "`{reason}` expected in {envelope}"
        );
    }
}

#[tokio::test]
async fn a_resolved_call_answers_the_envelope_with_what_the_model_did() {
    let dir = tempfile::tempdir().unwrap();
    let host = host_with_completion();
    // `mock` resolves and streams — and the mock's script answers an
    // agent's first turn, a tool call and no text, so the honest envelope
    // is ok:false naming the ending rather than a reply that says nothing.
    // The ok:true arm's shape is covered by the envelope builder and the
    // providers' completion tests.
    let out = call(
        ask_model(&host).as_ref(),
        dir.path(),
        json!({ "prompt": "hello", "model": "mock" }),
    )
    .await;
    let envelope: Value = serde_json::from_str(&out).expect("the envelope is JSON text");
    assert_eq!(envelope["ok"], false, "{envelope}");
    assert!(
        envelope["error"].as_str().unwrap().contains("no text"),
        "{envelope}"
    );
}

#[test]
fn the_tool_registers_only_where_a_completion_path_exists_and_is_never_offered() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = ToolRegistry::new();
    register_builtins(&mut reg, &host_with_completion(), dir.path()).unwrap();
    let tool = reg.get("ask_model").expect("a catalog, a tool");
    assert!(tool.manifest().script_only, "script-only by declaration");
    // Never in a request's tool list, on any branch — there is nothing to
    // reach it *with*: no search finds it and no call opens it.
    let reach = eidolon_core::tool_search::Reach {
        called: ["ask_model".to_string()].into(),
        queries: vec!["model call".into()],
    };
    assert!(!eidolon_core::tool_search::offered(&reg, &reach).contains(&"ask_model".to_string()));
    let (hits, _) = eidolon_core::tool_search::search(&reg, "ask_model", 8);
    assert!(hits.is_empty(), "{hits:?}");
}

/// The spend record, end to end through the chokepoint: a script-originated
/// `ask_model` dispatch journals one `ModelSpend`, paired with the call id
/// it ran under, and the session's token total sees it. The mock is
/// unpriced, so this is also the unpriced direction: the row is there, the
/// tokens are in the total, and the dollars a reader would look up are
/// honestly *absent* — not zero.
#[tokio::test]
async fn the_call_journals_its_spend_paired_with_its_call_id() {
    use std::sync::Arc as StdArc;

    use eidolon_core::policy::AllowAll;
    use eidolon_core::session::Session;
    use eidolon_core::user::NoUser;
    use tokio::sync::Mutex as AsyncMutex;

    let dir = tempfile::tempdir().unwrap();
    let host = {
        let h = Host::new(dir.path().to_path_buf());
        h.attach_completion(eidolon_rune::host::Completion {
            catalog: StdArc::new(Catalog::new()),
            default_model: "mock".into(),
            max_tokens: 1024,
        });
        h
    };
    let mut reg = ToolRegistry::new();
    reg.register(ask_model(&host));
    let session = StdArc::new(AsyncMutex::new(
        Session::create(&dir.path().join("s.eid"), "mock", dir.path(), None).unwrap(),
    ));
    let d = StdArc::new(eidolon_core::dispatch::Dispatcher::new(
        reg,
        StdArc::new(AllowAll),
        StdArc::new(NoUser),
        eidolon_core::event::EventBus::default(),
        session.clone(),
        dir.path().to_path_buf(),
    ));
    host.attach(&d);

    let out = d
        .dispatch(
            eidolon_core::tool::ToolCall {
                id: "w7-3".into(),
                name: "ask_model".into(),
                input: json!({ "prompt": "hello", "model": "mock" }),
                origin: eidolon_core::tool::CallOrigin::Script,
            },
            CancellationToken::new(),
        )
        .await;
    assert!(!out.is_error);

    let session = session.lock().await;
    let spends: Vec<(&str, &str, eidolon_core::message::Usage)> = session
        .branch()
        .into_iter()
        .filter_map(|r| match &r.kind {
            RecordKind::ModelSpend { key, call_id, usage } => {
                Some((key.as_str(), call_id.as_str(), *usage))
            }
            _ => None,
        })
        .collect();
    assert_eq!(spends.len(), 1, "one call, one spend record");
    let (key, call_id, usage) = spends[0];
    assert_eq!(key, "mock");
    assert_eq!(call_id, "w7-3", "paired with the dispatch id");
    assert!(
        usage.input_tokens > 0 || usage.output_tokens > 0,
        "the stream measured something: {usage:?}"
    );
    // The token total sees the call — the invariant that makes a partial
    // total impossible starts at the session's own arithmetic.
    assert!(session.usage().output_tokens > 0);
    // And the pricing lookup is honestly absent for the mock: the record
    // carries what was measured, and nothing renders it as free.
    assert!(Catalog::new().cost("mock").is_none());
}

use eidolon_core::session::RecordKind;
