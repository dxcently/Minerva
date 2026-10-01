//! The script door — `eidolon::tool_call`; the fork's
//! `eidolon::dispatch(tool, input)` stays the fork's name for it — against
//! the four conditions it was accepted under and the plugin workflows
//! payload inherits whole (vault, *The Plugin System (design)*,
//! §Workflows): origin
//! preserved end to end, no path past the gate, a depth limit that names the
//! chain, and — reported, not tested here; see the report — the parked-
//! question guard that today's API cannot yet decide.

use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use eidolon_core::event::Event;
use eidolon_core::policy::{AllowAll, Ruling, Verdict};
use eidolon_core::session::{PolicyOutcome, RecordKind};
use eidolon_core::user::NoUser;
use eidolon_core::*;

use eidolon_rune::host::MAX_SCRIPT_DISPATCH_DEPTH;
use eidolon_rune::policy::{PolicySettings, ScriptPolicy};
use eidolon_rune::{BUILTINS, Host, ScriptTool};

const TARGET: &str = r#"
pub fn manifest() {
    #{
        name: "target",
        description: "a tool a script may reach",
        approval: "read_only",
        input_schema: #{ "type": "object" },
    }
}

pub async fn call(input) { "target ran" }
"#;

/// The door: a script naming another tool. Runs against a dispatcher with
/// `target` registered; the result is `target`'s answer, not the script's.
const CALLER: &str = r#"
pub fn manifest() {
    #{
        name: "caller",
        description: "dispatches target through the door",
        approval: "read_only",
        input_schema: #{ "type": "object" },
    }
}

pub async fn call(input) { eidolon::tool_call("target", #{}).await }
"#;

/// The same door naming a tool that is not there. The dispatcher's own
/// answer — unknown, with the near-miss list — is what the script gets.
const NOBODY: &str = r#"
pub fn manifest() {
    #{
        name: "nobody",
        description: "dispatches a tool that does not exist",
        approval: "read_only",
        input_schema: #{ "type": "object" },
    }
}

pub async fn call(input) { eidolon::tool_call("no_such_tool", #{}).await }
"#;

/// A gate probe: the shipped `bash`, driven through the door, asking for
/// something the leaf table refuses on sight — the never-on-this-machine
/// arm, before any decomposition question can arise.
const RUN_BASH: &str = r#"
pub fn manifest() {
    #{
        name: "run_bash",
        description: "dispatches bash through the door",
        approval: "read_only",
        input_schema: #{ "type": "object" },
    }
}

pub async fn call(input) {
    eidolon::tool_call("bash", #{ command: "sudo rm -rf /" }).await
}
"#;

/// A chain: every dispatch comes back through the door to the same tool.
/// The depth limit — not the stack — is what stops it.
const RECURSE: &str = r#"
pub fn manifest() {
    #{
        name: "recurse",
        description: "dispatches itself",
        approval: "read_only",
        input_schema: #{ "type": "object" },
    }
}

pub async fn call(input) { eidolon::tool_call("recurse", input).await }
"#;

/// The interpreter's shape: dispatch under the id the journal already
/// holds. Twice below — the first visit runs, the second is the memo.
const NAMED: &str = r#"
pub fn manifest() {
    #{
        name: "named",
        description: "dispatches under an id the journal holds",
        approval: "read_only",
        input_schema: #{ "type": "object" },
    }
}

pub async fn call(input) {
    eidolon::tool_call_as("counted", #{}, "w7-3").await
}
"#;

/// The same door with nothing to name: the id a caller cannot mean.
const EMPTY_ID: &str = r#"
pub fn manifest() {
    #{
        name: "empty_id",
        description: "dispatches under an empty id",
        approval: "read_only",
        input_schema: #{ "type": "object" },
    }
}

pub async fn call(input) {
    eidolon::tool_call_as("counted", #{}, "").await
}
"#;

/// A target that counts its own runs — the testimony a script tool cannot
/// give about itself, and the whole point of the memo test below.
struct Counted(Arc<AtomicUsize>, ToolManifest);

#[async_trait]
impl Tool for Counted {
    fn manifest(&self) -> &ToolManifest {
        &self.1
    }
    async fn call(
        &self,
        _: serde_json::Value,
        _: CallContext,
    ) -> anyhow::Result<ToolOutput> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ToolOutput::ok("the answer"))
    }
}

fn counted() -> (Arc<AtomicUsize>, Arc<dyn Tool>) {
    let runs = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(Counted(
        runs.clone(),
        ToolManifest {
            name: "counted".into(),
            description: "a tool a script names".into(),
            input_schema: json!({"type":"object"}),
            approval: eidolon_core::policy::Approval::ReadOnly,
            prompt: None,
            render: None,
            deferred: false,
            script_only: false,
            plugin: None,
            source_hash: None,
        },
    ));
    (runs, tool as Arc<dyn Tool>)
}

/// The gate's own record of what reached it: each call's origin and name,
/// in order. This is the seam "origin preserved end to end" is proven at —
/// the one place a script's call could have arrived dressed as the model's.
#[derive(Default)]
struct Spy(Mutex<Vec<(CallOrigin, String)>>);

#[async_trait]
impl PolicyHook for Spy {
    async fn pre_tool(&self, call: &ToolCall, _: &ToolManifest, _: &Path) -> Ruling {
        self.0
            .lock()
            .unwrap()
            .push((call.origin, call.name.clone()));
        allow()
    }
}

/// The shipped table, but for `bash` only. The property under test is how
/// `bash` classifies; the table rightly asks about an unclassified *caller*
/// tool, which would refuse the run before the door was ever reached. The
/// bash call itself is delegated untouched — same hook code, same verdict
/// mapping, same audit line a model's call gets.
struct BashGate(ScriptPolicy);

#[async_trait]
impl PolicyHook for BashGate {
    async fn pre_tool(&self, call: &ToolCall, manifest: &ToolManifest, cwd: &Path) -> Ruling {
        if call.name == "bash" {
            return self.0.pre_tool(call, manifest, cwd).await;
        }
        allow()
    }
}

fn allow() -> Ruling {
    Ruling {
        verdict: Verdict::Allow,
        reason: None,
        structural: false,
        judged: None,
        yolo: false,
        vouched: None,
    }
}

/// A session with a real dispatcher, a host attached to it, and script tools
/// compiled against that host — the circularity the door lives in: the
/// dispatcher owns the registry that owns the scripts that own the host, and
/// the host holds the dispatcher that [`Host::attach`] links.
struct Rig {
    _dir: tempfile::TempDir,
    _host: Arc<Host>,
    d: Arc<Dispatcher>,
    session: Arc<AsyncMutex<Session>>,
    bus: eidolon_core::event::EventBus,
}

fn rig(policy: Arc<dyn PolicyHook>, tools: &[(&str, &str)]) -> Rig {
    rig_with(move |_| Ok(policy.clone()), Vec::new(), tools)
}

/// [`rig`], with native tools registered beside the script ones — for a
/// target that can count its own runs, which a script tool cannot testify
/// to any better than the journal already does.
fn rig_with(
    policy: impl FnOnce(&Arc<Host>) -> anyhow::Result<Arc<dyn PolicyHook>>,
    native: Vec<Arc<dyn Tool>>,
    tools: &[(&str, &str)],
) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::new(dir.path().to_path_buf());
    let policy = policy(&host).unwrap();
    let mut reg = ToolRegistry::new();
    for tool in native {
        reg.register(tool);
    }
    for (name, src) in tools {
        reg.register(Arc::new(ScriptTool::compile(name, src, host.clone()).unwrap()));
    }
    let session = Arc::new(AsyncMutex::new(
        Session::create(&dir.path().join("s.eid"), "m", dir.path(), None).unwrap(),
    ));
    let bus = eidolon_core::event::EventBus::default();
    let d = Arc::new(Dispatcher::new(
        reg,
        policy,
        Arc::new(NoUser),
        bus.clone(),
        session.clone(),
        dir.path().to_path_buf(),
    ));
    host.attach(&d);
    Rig {
        _dir: dir,
        _host: host,
        d,
        session,
        bus,
    }
}

fn call(name: &str) -> ToolCall {
    ToolCall {
        id: format!("m-{name}"),
        name: name.into(),
        input: json!({}),
        origin: CallOrigin::Model,
    }
}

/// Every `PolicyVerdict` on the branch — the audit line the gate writes.
async fn audit(rig: &Rig) -> Vec<(String, String, bool, PolicyOutcome, Option<String>)> {
    let session = rig.session.lock().await;
    session
        .branch()
        .into_iter()
        .filter_map(|rec| match &rec.kind {
            RecordKind::PolicyVerdict {
                tool,
                reason,
                structural,
                outcome,
                note,
                ..
            } => Some((
                tool.clone(),
                reason.clone(),
                *structural,
                outcome.clone(),
                note.clone(),
            )),
            _ => None,
        })
        .collect()
}

/// The `ToolCallStarted` events, as a consumer watching the bus saw them.
/// The receiver must have been taken before the calls it should have seen —
/// a broadcast channel has no past.
fn started(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<(CallOrigin, String)> {
    let mut seen = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if let Event::ToolCallStarted(c) = e {
            seen.push((c.origin, c.name));
        }
    }
    seen
}

fn bash_source() -> &'static str {
    BUILTINS
        .iter()
        .find(|(n, _)| *n == "bash")
        .map(|(_, src)| *src)
        .unwrap()
}

// --- the door -------------------------------------------------------------

/// A script names a tool; the tool's answer comes back to the script.
#[tokio::test]
async fn the_door_runs_what_the_script_names() {
    let r = rig(Arc::new(AllowAll), &[("target", TARGET), ("caller", CALLER)]);
    let out = r.d.dispatch(call("caller"), CancellationToken::new()).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.content, "target ran");
}

/// A name that is not registered is the dispatcher's own answer, not a
/// silent nothing.
#[tokio::test]
async fn the_door_does_not_invent_a_tool() {
    let r = rig(Arc::new(AllowAll), &[("nobody", NOBODY)]);
    let out = r.d.dispatch(call("nobody"), CancellationToken::new()).await;
    assert!(out.is_error);
    assert!(
        out.content.contains("unknown tool `no_such_tool`"),
        "{}",
        out.content
    );
}

/// A host with no dispatcher attached refuses by name — the message every
/// dispatcher-shaped function on the module already answers with.
#[tokio::test]
async fn the_door_refuses_cleanly_without_a_dispatcher() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::new(dir.path().to_path_buf());
    let tool = ScriptTool::compile("caller", CALLER, host).unwrap();
    let out = tool
        .call(
            json!({}),
            CallContext {
                cwd: dir.path().to_path_buf(),
                cancel: CancellationToken::new(),
                call_id: "t".into(),
            },
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert_eq!(
        out.content,
        "no dispatcher attached; scripts cannot call tools yet"
    );
}

// --- origin preserved end to end ------------------------------------------

/// A script's call is the script's, everywhere it shows up: the gate saw
/// `CallOrigin::Script`, the bus published it, and the journal holds the
/// result under the script's own id — with no `UserToolCall` (the
/// operator's record) and no assistant message (the model's), because a
/// dispatch fabricates neither. Nothing anywhere records a script's call as
/// a model's.
#[tokio::test]
async fn a_script_call_is_the_scripts_never_the_models() {
    let spy = Arc::new(Spy::default());
    let policy: Arc<dyn PolicyHook> = spy.clone();
    let r = rig(policy, &[("target", TARGET), ("caller", CALLER)]);
    let mut rx = r.bus.subscribe();
    let out = r.d.dispatch(call("caller"), CancellationToken::new()).await;
    assert!(!out.is_error, "{}", out.content);

    let gate_saw = [(CallOrigin::Model, "caller".to_string()), (CallOrigin::Script, "target".to_string())];
    assert_eq!(spy.0.lock().unwrap().as_slice(), &gate_saw);
    assert_eq!(started(&mut rx), gate_saw.to_vec());

    let session = r.session.lock().await;
    let records = session.branch();
    let results: Vec<_> = records
        .iter()
        .filter_map(|rec| match &rec.kind {
            RecordKind::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some((tool_use_id.as_str(), content.as_str(), *is_error)),
            _ => None,
        })
        .collect();
    assert!(
        results
            .iter()
            .any(|(id, c, e)| id.starts_with("script_target_") && *c == "target ran" && !*e),
        "the script's result under its own id: {results:?}"
    );
    assert!(
        results.iter().any(|(id, _, _)| *id == "m-caller"),
        "the model's call paired with its own id: {results:?}"
    );
    assert!(
        !records
            .iter()
            .any(|rec| matches!(rec.kind, RecordKind::UserToolCall { .. })),
        "a script's call is not the operator's"
    );
    assert!(
        !records
            .iter()
            .any(|rec| matches!(rec.kind, RecordKind::AssistantMessage(_))),
        "a dispatch fabricates no assistant message"
    );
}

// --- no path past the gate ------------------------------------------------

/// The same refused command, from the model and from a script through the
/// door: identical verdict, identical audit line — same reason, same
/// structural flag, same outcome, same tool. Origin changes nothing about
/// how `bash` is classified; a step is never laundered past the gate
/// because it arrived inside a script.
#[tokio::test]
async fn a_scripts_bash_is_classified_exactly_like_a_models() {
    let bash = bash_source();
    let model = {
        let settings = PolicySettings::default();
        rig_with(
            move |host| {
                Ok(Arc::new(ScriptPolicy::builtin(host, settings.clone())?) as Arc<dyn PolicyHook>)
            },
            Vec::new(),
            &[("bash", bash)],
        )
    };
    let scripted = {
        let settings = PolicySettings::default();
        rig_with(
            move |host| {
                Ok(Arc::new(BashGate(ScriptPolicy::builtin(host, settings.clone())?))
                    as Arc<dyn PolicyHook>)
            },
            Vec::new(),
            &[("bash", bash), ("run_bash", RUN_BASH)],
        )
    };

    let expected = "denied by policy: privilege escalation";
    let m = model
        .d
        .dispatch(
            ToolCall {
                id: "m1".into(),
                name: "bash".into(),
                input: json!({ "command": "sudo rm -rf /" }),
                origin: CallOrigin::Model,
            },
            CancellationToken::new(),
        )
        .await;
    assert!(m.is_error, "{}", m.content);
    assert_eq!(m.content, expected);

    let s = scripted
        .d
        .dispatch(call("run_bash"), CancellationToken::new())
        .await;
    assert!(s.is_error, "{}", s.content);
    assert_eq!(s.content, expected);

    let line = (
        "bash".to_string(),
        "privilege escalation".to_string(),
        false,
        PolicyOutcome::Refused,
        None,
    );
    assert_eq!(audit(&model).await, vec![line.clone()]);
    assert_eq!(audit(&scripted).await, vec![line]);
}

// --- the depth limit ------------------------------------------------------

/// A chain that grows past the limit is answered, not overflowed: the error
/// names the chain it refused — every script whose dispatch ran, plus the
/// call that did not — and every dispatch that *did* run is journaled, so
/// the log shows the chain unwinding rather than a stack vanishing.
#[tokio::test]
async fn a_chain_too_deep_errors_naming_the_chain() {
    let r = rig(Arc::new(AllowAll), &[("recurse", RECURSE)]);
    let mut rx = r.bus.subscribe();
    let out = r.d.dispatch(call("recurse"), CancellationToken::new()).await;
    assert!(out.is_error, "{}", out.content);
    assert!(
        out.content.contains(&format!(
            "script dispatch nested deeper than {MAX_SCRIPT_DISPATCH_DEPTH}"
        )),
        "{}",
        out.content
    );
    // The chain named: the model's call plus `MAX` script levels that ran,
    // then the refused one.
    assert_eq!(
        out.content.matches("recurse").count(),
        MAX_SCRIPT_DISPATCH_DEPTH + 2,
        "{}",
        out.content
    );
    assert!(
        out.content.contains("recurse → recurse → recurse"),
        "{}",
        out.content
    );

    // The model's dispatch and every script hop that ran: each journaled,
    // each an error carrying the same answer.
    let ran = MAX_SCRIPT_DISPATCH_DEPTH + 1;
    assert_eq!(started(&mut rx).len(), ran);
    let session = r.session.lock().await;
    let records = session.branch();
    let results: Vec<_> = records
        .iter()
        .filter_map(|rec| match &rec.kind {
            RecordKind::ToolResult {
                content,
                is_error,
                ..
            } => Some((content.as_str(), *is_error)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), ran, "{results:?}");
    assert!(
        results.iter().all(|(c, e)| *e && *c == out.content),
        "{results:?}"
    );
}

// --- the caller-named call id ----------------------------------------------

/// A step journaled under the run's own deterministic id, dispatched under
/// that id: the first visit runs the tool and journals the answer under it;
/// the second visit is the memo — the chokepoint's replay check answers
/// from the log and the tool never runs, which is what makes a resume a
/// replay instead of a re-fire. Origin survives the naming too: the gate
/// still sees `Script`, twice.
#[tokio::test]
async fn a_call_dispatched_under_its_journaled_id_memoizes() {
    let spy = Arc::new(Spy::default());
    let policy: Arc<dyn PolicyHook> = spy.clone();
    let (runs, counted) = counted();
    let r = rig_with(
        move |_| Ok(policy.clone()),
        vec![counted],
        &[("named", NAMED)],
    );
    let first = r
        .d
        .dispatch(call("named"), CancellationToken::new())
        .await;
    let second = {
        // A different call id for the *caller* — the replay that matters is
        // keyed on the named id, not the caller's own.
        let mut c = call("named");
        c.id = "m-named-2".into();
        r.d.dispatch(c, CancellationToken::new()).await
    };

    assert!(!first.is_error, "{}", first.content);
    assert_eq!(second.content, first.content, "the memo answers");
    assert_eq!(first.content, "the answer");
    assert_eq!(
        runs.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the tool ran once, not once per resume"
    );
    assert_eq!(
        spy.0.lock().unwrap().as_slice(),
        &[
            (CallOrigin::Model, "named".to_string()),
            (CallOrigin::Script, "counted".to_string()),
            (CallOrigin::Model, "named".to_string()),
            // ... and no second `Script` entry for the memo hit: the
            // chokepoint's replay check answers *before* the gate, because
            // the answer on the branch already carries the verdict that
            // produced it — a resume re-decides nothing.
        ]
    );

    let session = r.session.lock().await;
    let results: Vec<_> = session
        .branch()
        .into_iter()
        .filter_map(|rec| match &rec.kind {
            RecordKind::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some((tool_use_id.as_str(), content.as_str(), *is_error)),
            _ => None,
        })
        .collect();
    let under_id: Vec<_> = results
        .iter()
        .filter(|(id, _, _)| *id == "w7-3")
        .collect();
    assert_eq!(under_id.len(), 1, "one answer, journaled once: {results:?}");
    assert_eq!(under_id[0].1, "the answer");
}

/// An id the caller cannot mean is refused by name, before the dispatcher
/// is consulted — an empty id journals a result nothing can look up.
#[tokio::test]
async fn an_empty_named_id_is_refused_by_name() {
    let (runs, counted) = counted();
    let r = rig_with(
        |_| Ok(Arc::new(AllowAll) as Arc<dyn PolicyHook>),
        vec![counted],
        &[("empty_id", EMPTY_ID)],
    );
    let out = r
        .d
        .dispatch(call("empty_id"), CancellationToken::new())
        .await;
    assert!(out.is_error);
    assert!(
        out.content.contains("tool_call_as needs a non-empty call id"),
        "{}",
        out.content
    );
    assert_eq!(
        runs.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing ran under a name no one can look up"
    );
}
