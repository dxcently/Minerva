//! Adoption: a plugin's `tools/*.rn` become tools a session can search and
//! call, namespaced by the plugin's directory and attributed to it. The
//! fixtures are real plugin directories on disk — `plugin.rn` and tool
//! files written the way the loader reads them, because adoption compiles
//! the tools and the point of the tag is what the log says later (vault,
//! *Plugin Packaging (spec)*, slices P1 and P2).

use std::fs;
use std::path::Path;
use std::sync::Arc;

use eidolon_core::tool::{CallContext, ToolRegistry};
use eidolon_rune::{Host, ScriptTool, register_plugins};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

/// The spec's own example manifest, byte for byte — the plugin the design
/// keeps coming back to.
const BROWSER: &str = r#"
pub fn manifest() {
    #{
        name: "browser",
        version: "0.1.0",
        description: "one line, shown by `eidolon plugins`",
        claims: #{ tools: ["open", "screenshot"], secrets: ["browser.token"] },
    }
}
"#;

/// A real tool file, the shape a third party publishes: its manifest
/// declares the *unnamespaced* name, because that is what a file of its
/// own would say. The layout, not the declaration, names the contribution.
const OPEN: &str = r#"
pub fn manifest() {
    #{
        name: "open",
        description: "opens a page",
        approval: "read_only",
        input_schema: #{ "type": "object", "properties": #{ "url": #{ "type": "string" } } },
    }
}

pub async fn call(input) {
    `opened ${input.url}`
}
"#;

/// What the operator's own tool of the same name is like, so a test can
/// tell whose tool kept the name.
const OPERATORS_OWN: &str = r#"
pub fn manifest() {
    #{
        name: "browser_open",
        description: "the operator's own",
        approval: "read_only",
        input_schema: #{ "type": "object" },
    }
}

pub async fn call(input) {
    "the operator's own"
}
"#;

/// A manifest that will not compile, for the broken-file case.
const BROKEN: &str = "pub fn manifest() { #{ name: "; // unterminated

/// Write one plugin directory: `root/<name>/plugin.rn` carrying `src`,
/// plus real tool files under `tools/`. An empty `src` writes no manifest
/// at all — the broken-by-absence shape.
fn plugin(root: &Path, name: &str, src: Option<&str>, tools: &[(&str, &str)]) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    if let Some(src) = src {
        fs::write(dir.join("plugin.rn"), src).unwrap();
    }
    for (file, tool_src) in tools {
        let path = dir.join("tools").join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, tool_src).unwrap();
    }
}

fn host() -> Arc<Host> {
    // The cwd is stored, never read at construction — the policy tests use
    // a bare `/ws` the same way. Nothing here runs a path primitive.
    Host::new(std::path::PathBuf::from("/ws"))
}

async fn call_tool(
    reg: &ToolRegistry,
    name: &str,
    input: serde_json::Value,
) -> eidolon_core::tool::ToolOutput {
    reg.get(name)
        .expect("the tool is registered")
        .call(
            input,
            CallContext {
                cwd: std::env::temp_dir(),
                cancel: CancellationToken::new(),
                call_id: "test".into(),
            },
        )
        .await
        .expect("the call runs")
}

#[tokio::test]
async fn the_layout_names_the_contribution_and_the_tag_rides_it() {
    let root = tempdir().unwrap();
    plugin(root.path(), "browser", Some(BROWSER), &[("open.rn", OPEN)]);
    let host = host();
    let mut reg = ToolRegistry::new();
    let notes = register_plugins(&mut reg, &host, root.path());
    assert!(notes.is_empty(), "a clean plugin says nothing: {notes:?}");

    // The namespaced name is the one the session got — the file's own
    // manifest says `open`, and that is not the name it registers under.
    let manifest = reg.get("browser_open").expect("adopted").manifest();
    assert_eq!(manifest.name, "browser_open");
    assert_eq!(manifest.plugin.as_deref(), Some("browser"));
    assert!(reg.get("open").is_none(), "the declaration did not win");

    // And it runs: the registered tool is the script, callable by the name
    // the plugin's directory gave it.
    let out = call_tool(&reg, "browser_open", serde_json::json!({ "url": "example.com" })).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.content, "opened example.com");
}

#[tokio::test]
async fn the_user_wins_a_collision_and_the_plugin_is_reported() {
    let root = tempdir().unwrap();
    plugin(root.path(), "browser", Some(BROWSER), &[("open.rn", OPEN)]);
    let host = host();
    let mut reg = ToolRegistry::new();
    // The operator's tool, already registered the way `register_dir` does.
    reg.register(Arc::new(
        ScriptTool::compile("browser_open", OPERATORS_OWN, host.clone()).unwrap(),
    ));

    let notes = register_plugins(&mut reg, &host, root.path());
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("browser") && notes[0].contains("browser_open"),
        "the report names the plugin and the name: {}",
        notes[0]
    );
    assert!(
        notes[0].contains("keeps it"),
        "the report says who won: {}",
        notes[0]
    );

    // The name still resolves to the operator's tool — the plugin's file
    // was refused, not swapped in.
    let out = call_tool(&reg, "browser_open", serde_json::json!({})).await;
    assert_eq!(out.content, "the operator's own");
}

#[tokio::test]
async fn a_builtin_name_is_refused_with_both_names_in_the_note() {
    let root = tempdir().unwrap();
    // `ask` + `model` joins to a real built-in's name, and the built-in
    // list is the whole list, not the ones this host happens to enable —
    // this host carries no completion handle, and `ask_model` is still not
    // the plugin's to take.
    plugin(
        root.path(),
        "ask",
        Some(r#"pub fn manifest() { #{ name: "ask" } }"#),
        &[("model.rn", OPEN)],
    );
    let host = host();
    let mut reg = ToolRegistry::new();
    let notes = register_plugins(&mut reg, &host, root.path());
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("plugin `ask`") && notes[0].contains("`ask_model`"),
        "the refusal names the plugin and the name it tried to take: {}",
        notes[0]
    );
    assert!(
        notes[0].contains("never shadowed"),
        "the refusal says why: {}",
        notes[0]
    );
    assert!(reg.get("ask_model").is_none());
}

#[tokio::test]
async fn broken_plugins_are_reported_and_their_neighbors_still_adopt() {
    let root = tempdir().unwrap();
    // A name outside the namespace grammar.
    plugin(
        root.path(),
        "Bad Name",
        Some(r#"pub fn manifest() { #{ name: "x" } }"#),
        &[("x.rn", OPEN)],
    );
    // No manifest at all.
    plugin(root.path(), "ghost", None, &[("open.rn", OPEN)]);
    // A manifest that disagrees with the directory.
    plugin(
        root.path(),
        "mismatch",
        Some(r#"pub fn manifest() { #{ name: "other" } }"#),
        &[("open.rn", OPEN)],
    );
    // And one good plugin beside them.
    plugin(root.path(), "browser", Some(BROWSER), &[("open.rn", OPEN)]);

    let host = host();
    let mut reg = ToolRegistry::new();
    let notes = register_plugins(&mut reg, &host, root.path());
    assert_eq!(notes.len(), 3, "one line per broken plugin: {notes:?}");
    for name in ["Bad Name", "ghost", "mismatch"] {
        let note = notes
            .iter()
            .find(|n| n.contains(name))
            .unwrap_or_else(|| panic!("no line for {name}: {notes:?}"));
        assert!(note.contains("not adopted"), "{note}");
    }
    assert!(reg.get("browser_open").is_some(), "the good one adopted");
    assert!(reg.get("ghost_open").is_none());
}

#[tokio::test]
async fn one_broken_tool_file_skips_itself_not_the_plugin() {
    let root = tempdir().unwrap();
    plugin(
        root.path(),
        "browser",
        Some(BROWSER),
        &[("ok.rn", OPEN), ("broken.rn", BROKEN)],
    );
    let host = host();
    let mut reg = ToolRegistry::new();
    let notes = register_plugins(&mut reg, &host, root.path());
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("broken.rn"), "{}", notes[0]);
    assert!(reg.get("browser_ok").is_some(), "the rest still load");
    assert!(reg.get("browser_broken").is_none());
}

#[test]
fn a_stem_the_namespace_cannot_carry_is_refused() {
    let root = tempdir().unwrap();
    // A leading digit joins into a name no model-facing tool should have:
    // refused with a line, not silently mangled.
    plugin(root.path(), "browser", Some(BROWSER), &[("2fa.rn", OPEN)]);
    let host = host();
    let mut reg = ToolRegistry::new();
    let notes = register_plugins(&mut reg, &host, root.path());
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("`2fa`") && notes[0].contains("`browser_2fa`"),
        "{}",
        notes[0]
    );
    assert!(reg.get("browser_2fa").is_none());
}

#[test]
fn a_missing_plugins_directory_is_nothing_to_say() {
    let host = host();
    let mut reg = ToolRegistry::new();
    let notes = register_plugins(&mut reg, &host, Path::new("/nonexistent/plugins"));
    assert!(notes.is_empty());
    assert!(reg.manifests().is_empty());
}

#[tokio::test]
async fn a_plugin_with_no_tools_payload_contributes_nothing_says_nothing() {
    let root = tempdir().unwrap();
    // Workflows-only plugin: the spec's layout carries payloads the tools
    // adoption does not read.
    let dir = root.path().join("browser");
    fs::create_dir_all(dir.join("workflows")).unwrap();
    fs::write(dir.join("plugin.rn"), BROWSER).unwrap();
    fs::write(dir.join("workflows").join("main.rn"), "pub fn x() { 1 }").unwrap();

    let host = host();
    let mut reg = ToolRegistry::new();
    let notes = register_plugins(&mut reg, &host, root.path());
    assert!(notes.is_empty(), "{notes:?}");
    assert!(reg.manifests().is_empty());
}
