//! The plugin manifest and discovery, against real plugin directories
//! written the way the loader reads them: `plugin.rn` as a file on disk,
//! not a fixture parsed in code (vault, *Plugin Packaging (spec)*).

use std::fs;
use std::path::Path;

use eidolon_rune::plugins::{self, Plugin};
use tempfile::tempdir;

/// The spec's own example manifest, byte for byte.
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

const MINIMAL: &str = r#"pub fn manifest() { #{ name: "minimal" } }"#;

/// Write one plugin directory: `root/<name>/plugin.rn` carrying `src`,
/// plus any payload files it should carry. The payload contents are
/// irrelevant to discovery — it never compiles them — only their
/// existence in the directory matters.
fn plugin(root: &Path, name: &str, src: &str, payloads: &[&str]) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(plugins::PLUGIN_FILE), src).unwrap();
    for rel in payloads {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "pub fn x() { 1 }").unwrap();
    }
}

fn manifest_of<'a>(found: &'a [Plugin], name: &str) -> &'a plugins::PluginManifest {
    match found.iter().find(|p| p.name() == name) {
        Some(Plugin::Installed { manifest, .. }) => manifest,
        other => panic!("expected installed plugin `{name}`, got {other:?}"),
    }
}

fn reason_of<'a>(found: &'a [Plugin], name: &str) -> &'a str {
    match found.iter().find(|p| p.name() == name) {
        Some(Plugin::Broken { reason, .. }) => reason,
        other => panic!("expected broken plugin `{name}`, got {other:?}"),
    }
}

#[test]
fn the_spec_example_is_an_installed_plugin() {
    let root = tempdir().unwrap();
    plugin(
        root.path(),
        "browser",
        BROWSER,
        &["tools/open.rn", "ui/panel.rn"],
    );
    let found = plugins::discover(root.path()).unwrap();
    assert_eq!(found.len(), 1, "exactly one plugin: {found:?}");
    let m = manifest_of(&found, "browser");
    assert_eq!(m.name, "browser");
    assert_eq!(m.version, "0.1.0");
    assert_eq!(m.description, "one line, shown by `eidolon plugins`");
    assert_eq!(m.claims["tools"].join(","), "open,screenshot");
    assert_eq!(m.claims["secrets"].join(","), "browser.token");
    let dir = root.path().join("browser");
    assert_eq!(
        plugins::payloads_present(&dir).join(","),
        "tools,ui",
        "present is read from the directory, in presentation order"
    );
}

#[test]
fn a_service_block_parses_and_its_absence_is_fine() {
    let root = tempdir().unwrap();
    plugin(root.path(), "minimal", MINIMAL, &[]);
    let served = r#"
pub fn manifest() {
    #{
        name: "served",
        command_is_not_a_manifest_field: true,
        service: #{
            command: "eidolon-browser",
            args: [],
            readiness: #{ port: 8090, health: "/health" },
            ready_timeout_s: 5,
        },
    }
}
"#;
    plugin(root.path(), "served", served, &[]);
    let found = plugins::discover(root.path()).unwrap();
    assert!(manifest_of(&found, "minimal").service.is_none(), "no service, no block");
    let service = manifest_of(&found, "served")
        .service
        .as_ref()
        .expect("the block parses");
    assert_eq!(service.command, "eidolon-browser");
    assert!(service.args.is_empty());
    assert_eq!(service.ready_timeout_s, 5, "the default is overridden, not merged");
    let readiness = service.readiness.as_ref().expect("readiness declared");
    assert_eq!(readiness.port, 8090);
    assert_eq!(readiness.health.as_deref(), Some("/health"));
}

#[test]
fn a_service_gets_the_default_ready_timeout() {
    let root = tempdir().unwrap();
    let terse = r#"pub fn manifest() { #{ name: "terse", service: #{ command: "/bin/true" } } }"#;
    plugin(root.path(), "terse", terse, &[]);
    let found = plugins::discover(root.path()).unwrap();
    let service = manifest_of(&found, "terse").service.as_ref().expect("the block parses");
    assert_eq!(service.ready_timeout_s, 30, "a declaration without a timeout still has one");
    assert!(service.readiness.is_none(), "and no readiness is no readiness, not a guess");
}

#[test]
fn manifest_name_must_equal_the_directory() {
    let root = tempdir().unwrap();
    plugin(
        root.path(),
        "browser",
        r#"pub fn manifest() { #{ name: "web" } }"#,
        &[],
    );
    let found = plugins::discover(root.path()).unwrap();
    let reason = reason_of(&found, "browser");
    assert!(reason.contains("`web`") && reason.contains("`browser`"), "{reason}");
}

#[test]
fn bad_directory_names_are_refused() {
    let root = tempdir().unwrap();
    for name in ["Browser", "9lives", "has-dash"] {
        plugin(root.path(), name, MINIMAL, &[]);
    }
    let found = plugins::discover(root.path()).unwrap();
    assert_eq!(found.len(), 3);
    for name in ["Browser", "9lives", "has-dash"] {
        let reason = reason_of(&found, name);
        assert!(reason.contains("is not [a-z]"), "{name}: {reason}");
    }
}

#[test]
fn a_directory_without_a_manifest_is_broken_not_fatal() {
    let root = tempdir().unwrap();
    fs::create_dir_all(root.path().join("empty")).unwrap();
    plugin(root.path(), "minimal", MINIMAL, &[]);
    let found = plugins::discover(root.path()).unwrap();
    assert_eq!(found.len(), 2, "broken does not hide the rest: {found:?}");
    // Sorted by name, and both entries there.
    assert_eq!(found[0].name(), "empty");
    assert!(reason_of(&found, "empty").contains("no plugin.rn"));
    assert_eq!(found[1].name(), "minimal");
}

#[test]
fn a_manifest_that_fails_to_compile_is_one_broken_entry() {
    let root = tempdir().unwrap();
    plugin(root.path(), "bad", "pub fn manifest() { #{ name: ", &[]);
    plugin(root.path(), "nofn", "pub fn other() { 1 }", &[]);
    plugin(root.path(), "minimal", MINIMAL, &[]);
    let found = plugins::discover(root.path()).unwrap();
    assert_eq!(found.len(), 3, "broken entries still list: {found:?}");
    assert!(
        reason_of(&found, "bad").contains("failed to compile"),
        "{}",
        reason_of(&found, "bad")
    );
    assert!(
        reason_of(&found, "nofn").contains("no `pub fn manifest()`"),
        "{}",
        reason_of(&found, "nofn")
    );
    assert_eq!(manifest_of(&found, "minimal").name, "minimal");
}

#[test]
fn a_claim_that_is_not_a_list_of_strings_is_broken() {
    let root = tempdir().unwrap();
    plugin(
        root.path(),
        "weird",
        r#"pub fn manifest() { #{ name: "weird", claims: #{ tools: "open" } } }"#,
        &[],
    );
    let found = plugins::discover(root.path()).unwrap();
    assert!(reason_of(&found, "weird").contains("wrong shape"));
}

#[test]
fn payloads_present_reads_the_directory_not_the_claims() {
    let root = tempdir().unwrap();
    let dir = root.path().join("p");
    plugin(
        root.path(),
        "p",
        r#"pub fn manifest() { #{ name: "p", claims: #{ workflows: ["nope"] } } }"#,
        &["tools/a.rn", "workflows/main.rn", "lib/util.rn"],
    );
    fs::create_dir_all(dir.join("ui")).unwrap(); // empty: not present
    fs::write(dir.join("tools/notes.txt"), "x").unwrap(); // not .rn; tools present anyway
    assert_eq!(
        plugins::payloads_present(&dir).join(","),
        "lib,tools,workflows"
    );
}

#[test]
fn a_missing_plugins_directory_is_no_plugins() {
    let root = tempdir().unwrap();
    let found = plugins::discover(&root.path().join("absent")).unwrap();
    assert!(found.is_empty());
}

#[test]
fn stray_files_are_ignored() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("README.md"), "not a plugin").unwrap();
    plugin(root.path(), "minimal", MINIMAL, &[]);
    let found = plugins::discover(root.path()).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name(), "minimal");
}
