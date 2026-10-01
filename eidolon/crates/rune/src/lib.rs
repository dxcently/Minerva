//! The Rune host: how a `.rn` script becomes a [`Tool`], and the primitive
//! vocabulary those scripts call.
//!
//! Everything the user touches is Rune (vault, *Extension model*,
//! 2026-09-03). The built-in tools under `builtin/` are scripts on the
//! same contract as a user-authored tool; the only thing that
//! distinguishes them is that they ship inside the binary. Six of them
//! are on every session; `search` arrives with a search key, the way the
//! vault's arrive with a vault. There is no
//! privileged native tool path to keep in sync, and nothing to shim around.
//! A user's own tools are `*.rn` files in a directory
//! ([`register_dir`], `~/.config/eidolon/tools` by default), compiled by
//! the same [`ScriptTool`].
//!
//! ## The built-ins live in that directory too
//!
//! The harness seeds every built-in into the tools directory on first
//! launch (`eidolon-cli`'s `system` module), so the file the operator
//! edits is the file the model runs. [`register_builtins`] therefore
//! reads a built-in's source from `<dir>/<name>.rn` when that file exists
//! and from the binary when it does not — a file named like a built-in
//! *is* that built-in's source, loaded in the built-in's slot and on the
//! built-in's terms (`peers.rn` exists only while the session has peers,
//! whoever wrote it). A copy that will not compile is a line on stderr and
//! the shipped source takes its place, which is what already happened
//! when a user's `read.rn` was broken: the shipped `read` was registered
//! underneath it. [`register_dir`] skips those names, so a seeded copy is
//! compiled once and never reported as replacing itself.
//!
//! ## The script contract
//!
//! ```rune
//! pub fn manifest() { #{ name: "…", description: "…", approval: "read_only", input_schema: #{…} } }
//! pub async fn call(input) { … }   // returns a String, or #{ content, is_error }
//! ```
//!
//! `manifest()` runs once at load and is converted through serde into a
//! [`ToolManifest`]. `call(input)` runs per dispatch with the model's JSON
//! input as a Rune object. A `Result::Err` returned from `call` (via `?`)
//! becomes an error output.
//!
//! ## The host vocabulary (`eidolon::*`)
//!
//! Registered in [`host`]: `fs_read`, `fs_write`, `fs_edit`, `shell`,
//! `exec`, `grep`, `web_fetch`, `web_search`, `api_request`, `cwd`, `ask_user`,
//! `choices_user`, and `mneme_rpc` (plus `mneme_call`, the raw primitive the
//! vault built-in wraps — both registered unconditionally and answered at
//! call time, so a session with no vault says so when one is called rather
//! than failing a script's compile; what gates on the vault is only whether
//! the built-in tool is registered). The first
//! six are the Rust engine primitives from `eidolon-tools`. `ask_user`,
//! `choices_user` and `mneme_rpc` route back through the
//! dispatcher, so a script asking a question or reaching the vault is a tool
//! call like any other — policy sees it and the log journals it.
//!
//! `tool_call(name, input)` is the generic form of those, and the door a
//! plugin workflow drives (vault, *The Plugin System (design)*,
//! §Workflows): a script names any registered tool and its input, and the
//! call is classified and journaled with `CallOrigin::Script` — the same
//! gate, the same log, never a second execution path. Nesting is bounded by
//! [`MAX_SCRIPT_DISPATCH_DEPTH`], which names the chain in its error.
//! `tool_call_as(name, input, id)` is the same door with the caller naming
//! the call: a workflow step journals its request under `w<run>-<index>`
//! before dispatching, and the resumed run dispatches under that id, so the
//! chokepoint's replay check answers from the log instead of re-firing.
//!
//! `web_search` is registered on every host and answers a *call* at call time:
//! the key is the session's, and a session assembled without one is refused
//! rather than searched. What the key gates is whether the `search` tool
//! exists at all, decided where the tools are registered — the same split the
//! swarm's two use, and for the same reason (the context may be warmed on
//! another thread before the capability lands).
//!
//! When the session is registered among its peers, the same two rungs
//! appear for the swarm: `swarm_call` is the raw primitive the `peers`
//! and `send` built-ins wrap, and `peers` / `peer_send` are what other
//! scripts call, routed back through the chokepoint.
//!
//! `api_request(endpoint, method, path, body)` is the general one, and it is what
//! makes a tool for a new API a script-only job. The endpoint is a descriptor the
//! script carries — base URL, the name of the secret to use, which header the
//! credential rides in ([`endpoint`]) — so a tool file is self-contained and
//! shareable, and what it names is a key rather than holding one: Rust resolves
//! the name at call time and puts the value in a header, where no script, tool
//! result or log can reach it. There is no registry: the file that makes the
//! call is the file that says what it reaches.
//!
//! ## Isolation
//!
//! A Rune `Vm` holds `!Send` state across `.await`, so a script cannot be
//! polled on a tokio worker thread. Each call runs on a dedicated OS thread
//! with its own current-thread runtime, the same arrangement Melete uses
//! (`jobs/skill_task.rs::run_scope_isolated`). Host futures are ordinary
//! futures and run fine on that runtime; cancellation reaches them through
//! the token in [`CallContext`], handed to the call's thread by
//! [`host::with_cancel`] rather than compiled into the script.
//!
//! ## One context
//!
//! Every script on a [`Host`] compiles against the host's one Rune
//! context and runs on the one runtime made from it ([`Host::compiler`]).
//! Building a context is the expensive part of loading a script — 2.5 ms
//! against 0.1 ms for the compile — so the built-ins cost about 3 ms at
//! startup rather than 25, and a call no longer rebuilds anything.

pub mod host;
pub mod permits;
pub mod plugins;
pub mod policy;
pub mod script;
pub mod store;
pub mod workflow;
pub mod endpoint;

use std::sync::Arc;

use eidolon_core::tool::{Tool, ToolRegistry};

pub use host::Host;
pub use script::ScriptTool;
pub use store::Store;
pub use workflow::{RunOutcome, Workflow};

/// The six built-in scripts, embedded so they ship in lockstep with the
/// primitives they wrap.
pub const BUILTINS: &[(&str, &str)] = &[
    ("read", include_str!("../builtin/read.rn")),
    ("write", include_str!("../builtin/write.rn")),
    ("edit", include_str!("../builtin/edit.rn")),
    ("bash", include_str!("../builtin/bash.rn")),
    ("grep", include_str!("../builtin/grep.rn")),
    ("fetch", include_str!("../builtin/fetch.rn")),
];

/// The vault built-in: Mneme's whole surface as one tool instead of 27
/// registered functions (vault decision 2026-09-03). Registered only when
/// the host has a configured vault; the primitives beneath it
/// (`eidolon::mneme_call`/`mneme_rpc`) are registered everywhere and answer
/// the absence at call time.
///
/// It was two until 2026-09-14: `mneme_dispatch`, the natural-language
/// sibling, was first deferred and then cut outright once the vault command
/// channel's inline seam covered the same ground without a second
/// natural-language surface in the tool list (see the design note *Vault
/// Command Channel*). What remains of it is the primitive beneath —
/// `eidolon::mneme_call("dispatch", …)`, which a script can still call; no
/// tool in the binary wraps it.
pub const MNEME_BUILTINS: &[(&str, &str)] = &[("mneme_rpc", include_str!("../builtin/mneme_rpc.rn"))];

/// The swarm built-ins: seeing the other sessions, speaking to one, and
/// answering a question one has asked. Registered only when this session
/// is registered among its peers — a lone harness would otherwise carry
/// tools whose only honest answer is that there is nobody there, and pay
/// for them in the system prompt.
pub const SWARM_BUILTINS: &[(&str, &str)] = &[
    ("peers", include_str!("../builtin/peers.rn")),
    ("send", include_str!("../builtin/send.rn")),
    ("answer", include_str!("../builtin/answer.rn")),
];

/// The search built-in, registered only on a session that was assembled with
/// a key for it ([`Host::has_search`]) — the key *is* the capability, and a
/// session without one has nothing to search with. The endpoint and the
/// address policy are not the script's to name: the primitive takes neither
/// from a script.
pub const SEARCH_BUILTINS: &[(&str, &str)] = &[("search", include_str!("../builtin/search.rn"))];

/// The model-call built-in, registered only on a session that was assembled
/// with a completion path ([`Host::has_completion`]) — the catalog *is* the
/// capability, and a session with none has nothing to ask. The tool is
/// `script_only`: a workflow drives it, and a model is never offered it.
/// The endpoint, the key and the output cap are not the script's to name —
/// the primitive takes none of them from a script.
pub const MODEL_BUILTINS: &[(&str, &str)] =
    &[("ask_model", include_str!("../builtin/ask_model.rn"))];

/// Whether `name` is one of the tools that ship in the binary — any of the
/// three lists, enabled or not. A file of that name in the tools directory
/// is the built-in's own source and is [`register_builtins`]'s to load.
pub fn is_builtin(name: &str) -> bool {
    BUILTINS
        .iter()
        .chain(MNEME_BUILTINS)
        .chain(SWARM_BUILTINS)
        .chain(SEARCH_BUILTINS)
        .chain(MODEL_BUILTINS)
        .any(|(n, _)| *n == name)
}

/// The source a built-in compiles from: `<dir>/<name>.rn` when the operator
/// has one, else `shipped`. A file that is there but cannot be read is
/// reported and the shipped source is used.
pub fn builtin_source(
    dir: &std::path::Path,
    name: &str,
    shipped: &'static str,
) -> (std::borrow::Cow<'static, str>, Option<String>) {
    let path = dir.join(format!("{name}.rn"));
    match std::fs::read_to_string(&path) {
        Ok(src) => (src.into(), None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (shipped.into(), None),
        Err(e) => (
            shipped.into(),
            Some(format!(
                "{}: {e}; the shipped `{name}` is in force",
                path.display()
            )),
        ),
    }
}

/// Compile every `*.rn` in `dir` that is not a built-in and register it,
/// in name order. Returns one line per thing worth saying — a script that
/// failed to compile (skipped, the rest still load), or a tool that
/// replaced one already registered — rather than failing, because one
/// broken user tool should not take the harness down with it. A missing
/// directory is nothing to say. A file named like a built-in is that
/// built-in's source and was already loaded by [`register_builtins`].
pub fn register_dir(
    registry: &mut ToolRegistry,
    host: &Arc<Host>,
    dir: &std::path::Path,
) -> Vec<String> {
    let mut notes = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return notes;
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rn"))
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if is_builtin(&name) {
            continue;
        }
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                notes.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        match ScriptTool::compile(&name, &src, host.clone()) {
            Ok(tool) => {
                let registered = tool.manifest().name.clone();
                if registry.get(&registered).is_some() {
                    notes.push(format!(
                        "{}: tool `{registered}` replaces the one already registered",
                        path.display()
                    ));
                }
                registry.register(Arc::new(tool));
            }
            Err(e) => notes.push(format!("{}: {e:#}", path.display())),
        }
    }
    notes
}

/// Adopt a directory of installed plugins' tools: every installed
/// plugin's `tools/*.rn` becomes a tool called `<name>_<stem>`, attributed
/// to `<name>`. Enumeration rides [`crate::plugins::discover`], so the
/// same rules that list a plugin rule its adoption: a directory outside
/// the namespace grammar, without a compiling `plugin.rn`, or with a
/// manifest that disagrees with its directory is one broken entry —
/// reported, and its tools are not adopted.
///
/// The layout is authoritative for *what exists* — a plugin with no
/// `tools/` simply contributes none — and the tool files themselves are
/// read the same way [`register_dir`] reads them, at the same point in
/// session build, after the operator's own tools (**user wins** on a
/// collision) and never over a built-in, whose name is not shadowable at
/// all. The manifest's `approval` and `claims` gate nothing here, by
/// ruling: adoption believes the directory's contents, and an effect is
/// gated by the one table like any other caller's.
///
/// Returns one line per thing worth saying, as [`register_dir`] does: a
/// broken plugin and why, a tool this session refuses (a built-in's name,
/// a name already taken, a stem the namespace cannot carry), a script that
/// failed to compile. One refused tool skips that tool, not the plugin,
/// and one broken plugin skips nothing else — the operator still reads why
/// on stderr. A missing plugins directory is nothing to say.
pub fn register_plugins(
    registry: &mut ToolRegistry,
    host: &Arc<Host>,
    plugins_dir: &std::path::Path,
) -> Vec<String> {
    let mut notes = Vec::new();
    let plugins = match crate::plugins::discover(plugins_dir) {
        Ok(plugins) => plugins,
        // A directory that is there and cannot be read is said out loud —
        // it is the one case where "no plugins" would be a lie.
        Err(e) => return vec![format!("{}: {e:#}", plugins_dir.display())],
    };
    // The record consult, before anything is compiled: a plugin the
    // operator disabled is present and listed but not adopted — its
    // tools join no session, and nothing of it is revoked, so
    // re-enabling asks nobody again. The read fails open (an unreadable
    // record enables), and the skip is silent by ruling — the listing is
    // where the decision is visible, not a stderr line every session
    // would repeat. (crate::store, *Plugin Packaging (spec)*.)
    let record = crate::store::Store::load(plugins_dir).ok();
    let enabled = |name: &str| record.as_ref().is_none_or(|s| s.enabled(name));
    for entry in plugins {
        let dir = match entry {
            crate::plugins::Plugin::Installed { dir, ref manifest }
                if enabled(&manifest.name) =>
            {
                dir
            }
            crate::plugins::Plugin::Installed { .. } => continue,
            crate::plugins::Plugin::Broken { dir, reason } => {
                notes.push(format!("{}: not adopted — {reason}", dir.display()));
                continue;
            }
        };
        let plugin = dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tools = dir.join("tools");
        let Ok(files) = std::fs::read_dir(&tools) else {
            // A plugin with no tools payload — workflows only, say — is
            // ordinary and not worth a line.
            continue;
        };
        let mut paths: Vec<std::path::PathBuf> = files
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "rn"))
            .collect();
        paths.sort();
        for path in paths {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = format!("{plugin}_{stem}");
            // Both halves of the name answer to the same grammar the
            // plugin's own does — the joined name is a tool a model calls
            // and a log line greps.
            if !crate::plugins::valid_name(&stem) {
                notes.push(format!(
                    "{}: `{stem}` cannot carry the plugin's namespace; `{name}` is not adopted",
                    path.display()
                ));
                continue;
            }
            // A built-in's source *is* the file at its name, which is why
            // the user's directory skips those stems rather than refusing:
            // a plugin's file does not have that excuse, so its
            // contribution is refused and the refusal names both the plugin
            // and the name it tried to take.
            if is_builtin(&name) {
                notes.push(format!(
                    "{}: plugin `{plugin}` may not take `{name}` — a built-in's name is never shadowed",
                    path.display()
                ));
                continue;
            }
            let src = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    notes.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            // Compiled under its namespaced name, so a diagnostic names the
            // tool the session would have had, and attributed to the plugin
            // the directory names — the tag the gate's question and the
            // audit lines carry.
            match ScriptTool::compile(&name, &src, host.clone()) {
                Ok(mut tool) => {
                    tool.attribute(&name, &plugin);
                    if registry.get(&name).is_some() {
                        notes.push(format!(
                            "{}: plugin `{plugin}` may not take `{name}` — the operator's tool (or an earlier plugin's) keeps it",
                            path.display()
                        ));
                        continue;
                    }
                    registry.register(Arc::new(tool));
                }
                Err(e) => notes.push(format!("{}: {e:#}", path.display())),
            }
        }
    }
    notes
}

/// Compile every built-in the host enables and register it, each from its
/// copy in `dir` when there is one and from the binary otherwise. Returns
/// a line per copy that could not be used — unreadable, or a compile
/// error — each saying that the shipped source is in force instead; a
/// shipped source that fails is a bug and fails the call.
pub fn register_builtins(
    registry: &mut ToolRegistry,
    host: &Arc<Host>,
    dir: &std::path::Path,
) -> anyhow::Result<Vec<String>> {
    let mut all: Vec<&(&str, &str)> = BUILTINS.iter().collect();
    if host.has_mneme() {
        all.extend(MNEME_BUILTINS.iter());
    }
    if host.has_swarm() {
        all.extend(SWARM_BUILTINS.iter());
    }
    if host.has_search() {
        all.extend(SEARCH_BUILTINS.iter());
    }
    if host.has_completion() {
        all.extend(MODEL_BUILTINS.iter());
    }
    let mut notes = Vec::new();
    for (name, shipped) in all {
        let (src, note) = builtin_source(dir, name, shipped);
        notes.extend(note);
        let shipped_tool = || {
            ScriptTool::compile(name, shipped, host.clone())
                .map_err(|e| anyhow::anyhow!("built-in `{name}`: {e:#}"))
        };
        let tool = match &src {
            std::borrow::Cow::Borrowed(_) => shipped_tool()?,
            std::borrow::Cow::Owned(live) => match ScriptTool::compile(name, live, host.clone()) {
                Ok(tool) => tool,
                Err(e) => {
                    notes.push(format!(
                        "{}: {e:#}; the shipped `{name}` is in force",
                        dir.join(format!("{name}.rn")).display()
                    ));
                    shipped_tool()?
                }
            },
        };
        registry.register(Arc::new(tool));
    }
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tools_directory_loads_what_compiles_and_says_what_did_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("hello.rn"),
            r#"pub fn manifest() { #{ name: "hello", description: "hi", approval: "read_only", input_schema: #{ "type": "object" } } }
               pub async fn call(input) { "hello" }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("broken.rn"),
            "pub fn manifest() { #{ name: \"broken\" }",
        )
        .unwrap();
        // A `read.rn` in the directory is the built-in's own source: it is
        // what `read` compiles from, in the built-in's slot, and nothing
        // says "replaced" — it was never two tools.
        std::fs::write(
            dir.path().join("read.rn"),
            r#"pub fn manifest() { #{ name: "read", description: "mine", approval: "read_only", input_schema: #{ "type": "object" } } }
               pub async fn call(input) { "mine" }"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a script").unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let mut reg = ToolRegistry::new();
        assert!(
            register_builtins(&mut reg, &host, dir.path())
                .unwrap()
                .is_empty()
        );
        assert_eq!(reg.get("read").unwrap().manifest().description, "mine");
        let notes = register_dir(&mut reg, &host, dir.path());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("broken.rn") && notes[0].contains("failed to compile"),
            "{notes:?}"
        );
        assert!(reg.get("hello").is_some());
        assert_eq!(
            reg.get("read").unwrap().manifest().description,
            "mine",
            "register_dir left the built-in alone"
        );
        // A directory that is not there is nothing to say.
        assert!(register_dir(&mut reg, &host, &dir.path().join("nope")).is_empty());
    }

    /// The seeded copy of a built-in is edited in place, so the one way it
    /// can go wrong is an edit that does not compile. That must not cost
    /// the model the tool: the shipped source is registered instead and
    /// the note says so, which is what already happened when a user's
    /// `read.rn` was broken and the built-in stood underneath it.
    #[test]
    fn a_broken_copy_of_a_built_in_is_reported_and_the_shipped_one_stands_in() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("write.rn"),
            "pub fn manifest() { #{ name: \"write\" }",
        )
        .unwrap();
        // The swarm built-ins exist on the built-in's terms whoever wrote
        // the file: with no registration this is not compiled — it could
        // not be, the primitive it calls is not in the context — and it
        // is not a user tool either.
        std::fs::write(dir.path().join("peers.rn"), SWARM_BUILTINS[0].1).unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let mut reg = ToolRegistry::new();
        let notes = register_builtins(&mut reg, &host, dir.path()).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("write.rn") && notes[0].contains("shipped `write` is in force"),
            "{notes:?}"
        );
        assert!(
            reg.get("write")
                .unwrap()
                .manifest()
                .description
                .contains("Write content")
        );
        assert!(register_dir(&mut reg, &host, dir.path()).is_empty());
        assert!(reg.get("peers").is_none());
        assert!(is_builtin("peers") && is_builtin("mneme_rpc") && !is_builtin("hello"));
    }

    /// One word for how much comes back. The four built-ins whose output the
    /// caller can bound all name the knob `limit` (standardized 2026-09-29:
    /// they had shipped `limit`, `max`, `max` and `count` — four words and
    /// three units for one idea, which is also why the dispatcher's spill
    /// exemption has to read the older spellings). This pin is about the
    /// surface rather than the rule: the exemption would keep working if a
    /// name drifted back, and what would come back with it is a model
    /// relearning the same knob three times.
    #[test]
    fn the_output_bounding_built_ins_name_the_knob_limit() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let _ = host.compiler();
        host.attach_search_key("test-key");
        let mut reg = ToolRegistry::new();
        register_builtins(&mut reg, &host, dir.path()).unwrap();
        for name in ["read", "grep", "fetch", "search"] {
            let manifest = reg.get(name).unwrap_or_else(|| panic!("{name}")).manifest().clone();
            let props = &manifest.input_schema["properties"];
            assert_eq!(
                props["limit"]["type"], "integer",
                "`{name}` bounds its output with `limit`: {}",
                manifest.input_schema
            );
            for older in ["max", "count"] {
                assert!(
                    props.get(older).is_none(),
                    "`{name}` kept the older spelling `{older}`: {}",
                    manifest.input_schema
                );
            }
        }
    }

    /// The dispatcher's spill exemption reads the call's own `limit`
    /// (`eidolon_core::dispatch::named_read_length`), so the built-in
    /// `read` has to keep naming its read length exactly that. A rename
    /// here is a silent return of the spill chain: the receipt tells the
    /// model to page with `read` (offset/limit), the page is spilled
    /// again, and the answer is a pointer to a second file.
    #[test]
    fn the_built_in_read_declares_the_limit_the_spill_exemption_reads() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let mut reg = ToolRegistry::new();
        register_builtins(&mut reg, &host, dir.path()).unwrap();
        let manifest = reg.get("read").unwrap().manifest().clone();
        let limit = &manifest.input_schema["properties"]["limit"];
        assert_eq!(
            limit["type"], "integer",
            "`read` still takes a top-level `limit`: {}",
            manifest.input_schema
        );
    }

    /// The swarm tools exist exactly when the session is registered among
    /// its peers. A lone harness should not carry two tools whose only
    /// honest answer is that there is nobody there — and it pays for a
    /// registered tool twice, in the manifest list and in the system
    /// prompt's stable prefix.
    #[test]
    fn the_swarm_built_ins_arrive_with_a_registration_and_not_before() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let mut reg = ToolRegistry::new();
        register_builtins(&mut reg, &host, dir.path()).unwrap();
        assert!(reg.get("peers").is_none(), "no registration, no peers tool");
        assert!(reg.get("send").is_none());

        let presence = eidolon_swarm::Presence::register(
            &dir.path().join("run"),
            &dir.path().join("s.eid"),
            dir.path(),
            "mock",
            "",
        )
        .unwrap();
        let host = Host::new(dir.path().to_path_buf());
        // The context is warmed on a background thread at startup, so it
        // is routinely built *before* the registration lands. A module
        // that decided from the field rather than reading it per call
        // failed here exactly as it failed on a real launch: `peers.rn`
        // would not compile against a primitive that existed a
        // millisecond later.
        let _ = host.compiler();
        host.attach_swarm(presence);
        let mut reg = ToolRegistry::new();
        register_builtins(&mut reg, &host, dir.path()).unwrap();
        let peers = reg.get("peers").expect("registered").manifest().clone();
        let send = reg.get("send").expect("registered").manifest().clone();
        assert_eq!(
            peers.approval,
            eidolon_core::Approval::ReadOnly,
            "looking costs nothing"
        );
        assert_eq!(
            send.approval,
            eidolon_core::Approval::Mutating,
            "reaching another session does not"
        );
        assert!(
            send.input_schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "to")
        );
        assert!(
            send.input_schema["properties"].get("operator").is_none(),
            "the send tool must not grow an `operator` parameter"
        );
    }

    /// The override is not a tool's to claim: the send tool's schema has
    /// no `operator`, and a model-fabricated one in the arguments is
    /// ignored rather than honoured — the envelope that lands says
    /// nothing of the operator. This is the pin that keeps a level-0
    /// park deaf to peers only: the operator's flag is their CLI act's
    /// to set, and nobody else reaches it.
    #[tokio::test]
    async fn a_fabricated_operator_field_in_the_send_tool_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        let host = Host::new(dir.path().to_path_buf());
        // Two sessions: the sender holds the tool, the recipient holds
        // the inbox — a session's own roster is every OTHER session by
        // construction, so nobody sends to themselves here.
        let sender = eidolon_swarm::Presence::register(
            &run,
            &dir.path().join("sender.eid"),
            dir.path(),
            "mock",
            "sender",
        )
        .unwrap();
        let recipient = eidolon_swarm::Presence::register(
            &run,
            &dir.path().join("receiver.eid"),
            dir.path(),
            "mock",
            "receiver",
        )
        .unwrap();
        let to = recipient.id().to_string();
        let inbox = recipient.inbox_dir();
        let _ = host.compiler();
        host.attach_swarm(sender);
        let mut reg = ToolRegistry::new();
        register_builtins(&mut reg, &host, dir.path()).unwrap();
        let send = reg.get("send").expect("registered").clone();
        let out = send
            .call(
                serde_json::json!({
                    "to": to,
                    "text": "fabricated override",
                    "operator": true,
                }),
                eidolon_core::tool::CallContext {
                    cwd: dir.path().to_path_buf(),
                    cancel: tokio_util::sync::CancellationToken::new(),
                    call_id: "test".into(),
                },
            )
            .await
            .expect("the tool call itself succeeds");
        assert!(!out.is_error, "{}", out.content);
        let mail = eidolon_swarm::inbox::drain(&inbox);
        assert_eq!(mail.len(), 1);
        assert!(
            !mail[0].operator,
            "a fabricated operator field must arrive unmarked: {mail:?}"
        );
        assert!(mail[0].wake, "and the ordinary direct-message defaults hold");
    }

    /// The search tool exists exactly when the session was assembled with a
    /// key: the key *is* the capability, and a session without one has nothing
    /// to search with — the same shape as the swarm's two. The value never
    /// reaches the manifest, and the endpoint the key goes to is not something
    /// a model names.
    #[test]
    fn the_search_built_in_arrives_with_a_key_and_not_before() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().to_path_buf());
        let mut reg = ToolRegistry::new();
        register_builtins(&mut reg, &host, dir.path()).unwrap();
        assert!(reg.get("search").is_none(), "no key, no search tool");
        assert!(is_builtin("search"), "but the name is the built-in's");

        // The context is warmed on a background thread at startup, so it is
        // routinely built *before* the key lands — which is why the decision
        // is read where the registration happens rather than at module build.
        let host = Host::new(dir.path().to_path_buf());
        let _ = host.compiler();
        host.attach_search_key("test-key");
        let mut reg = ToolRegistry::new();
        register_builtins(&mut reg, &host, dir.path()).unwrap();
        let search = reg.get("search").expect("registered").manifest().clone();
        assert_eq!(
            search.approval,
            eidolon_core::Approval::ReadOnly,
            "searching changes nothing here"
        );
        assert!(
            search.input_schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "query")
        );
        assert!(
            search.input_schema["properties"].get("endpoint").is_none(),
            "the endpoint this key goes to is not the model's to choose"
        );
        assert!(
            !serde_json::to_string(&search.input_schema)
                .unwrap()
                .contains("test-key"),
            "and the key is not in the schema"
        );
    }
}
