//! The Rune UI script: compiled once, called per frame (`view`), once at
//! load (`keymap`, and the optional `modes` and `theme`) and per
//! submission (`on_submit`). Sync — no host function here awaits.
//! "Once" is once per load: `reload_ui` compiles the script again and
//! reads all three tables afresh.
//!
//! There is deliberately no `on_key`. Keys go through the modal keymap
//! (see [`crate::keys`]) because a chord, a count and a mode are state,
//! and a [`Vm`] built per call cannot hold state. What the script owns is
//! the *table*, which is the part worth owning.
//!
//! ## Actions `on_submit` may return
//!
//! ```text
//! #{ action: "send", text }             submit a user turn
//! #{ action: "command", name, arg }     a named command (see `command`)
//! #{ action: "dispatch", text }         resolve it on the aimed surface
//! #{ action: "insert", text }           type this into the prompt
//! #{ action: "info", text }             a line in the transcript
//! #{ action: "cancel" }                 cancel the turn in flight
//! #{ action: "quit" }
//! #{ action: "none" }  or  ()           do nothing
//! ```
//!
//! A command the script declares in `commands()` returns the same
//! actions: its function is `fn(state, arg) -> action`, run on the UI
//! thread when the command is bound, typed or picked.
//!
//! `dispatch` is what `ret` means in dispatch mode: the line goes to a
//! classifier instead of to the model (see [`crate::dispatch`]). The
//! script is what decides that, from the `mode` in the snapshot, for the
//! same reason it decides that a `:` line is a command — where a
//! submitted line goes is exactly the question `on_submit` exists to
//! answer.
//!
//! ## The `ui` host module
//!
//! `roman(n)`, `parse_command(text)` (→ `Some(#{ name, arg })` or `None`),
//! `k(n)` (a token count at a glance — `812`, `12.4k`, `1.31M`, which is
//! `usage::human` and not a second convention), `context(tokens, limit)`
//! (`usage::context_line`; a limit of `0` means the model declares no
//! window and gets a bare count), `cogitation(calls)` (the pulse's word
//! after that many tool calls, which the folded thinking line wears
//! too), `agent_icon()` (the agent as two braille cells, standing
//! still), and `commands()` — the whole
//! vocabulary from [`crate::command`] as data, so a script can build its
//! own menu or help out of the same table the harness resolves against.
//! Pure helpers; the script cannot reach the filesystem, the network or
//! the agent from here.
//!
//! `parse_command` takes `:` and only `:`. A leading `/` used to work too;
//! it does not any more, because that prefix is spoken for elsewhere — a
//! line starting with a slash is ordinary text and goes to the model.

use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, bail};
use rune::runtime::RuntimeContext;
use rune::termcolor::Buffer;
use rune::{Context, Diagnostics, Module, Source, Sources, Unit, Value, Vm};
use serde::Deserialize;
use serde::de::IntoDeserializer;

/// A compiled script: the unit and the runtime it runs on, both shared,
/// so a clone is two reference counts and a `Vm` per call is cheap.
#[derive(Clone)]
pub struct UiScript {
    unit: Arc<Unit>,
    runtime: Arc<RuntimeContext>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Send(String),
    Command {
        name: String,
        arg: String,
    },
    /// Resolve this on the aimed dispatch surface and run what it makes.
    Dispatch(String),
    /// Type this into the prompt, where the cursor is.
    Insert(String),
    /// Say this in the transcript.
    Info(String),
    Cancel,
    Quit,
    None,
}

/// The one Rune context UI scripts compile against, and the runtime made
/// from it. Built on first use and shared by every compile after — the
/// default script at startup, a user's `ui.rn` over it, every `reload_ui`
/// — because building the context is 2.5 ms of a 4.5 ms compile and
/// nothing in the `ui` module closes over a script.
struct Compiler {
    context: Context,
    runtime: Arc<RuntimeContext>,
}

static COMPILER: OnceLock<Result<Compiler, String>> = OnceLock::new();

fn compiler() -> anyhow::Result<&'static Compiler> {
    COMPILER
        .get_or_init(|| {
            let mut context =
                Context::with_default_modules().map_err(|e| format!("rune context: {e}"))?;
            context
                .install(
                    rune_modules::json::module(false).map_err(|e| format!("json module: {e}"))?,
                )
                .map_err(|e| format!("installing json: {e}"))?;
            context
                .install(ui_module().map_err(|e| format!("ui module: {e}"))?)
                .map_err(|e| format!("installing ui: {e}"))?;
            let runtime = Arc::new(
                context
                    .runtime()
                    .map_err(|e| format!("rune runtime: {e}"))?,
            );
            Ok(Compiler { context, runtime })
        })
        .as_ref()
        .map_err(|e| anyhow!("{e}"))
}

/// The default script, compiled once per process. It is embedded, so it
/// cannot change underneath a running harness, and it is read far more
/// often than once: the launch compiles it as the layout, then again for
/// the modes a user's script is laid over, again for the theme, and again
/// for the dialog tables a script may leave out — four compiles of one
/// string, 2 ms each, on the way to the first frame. [`UiScript::compile`]
/// hands this out whenever it is given that string.
static DEFAULT: OnceLock<Result<UiScript, String>> = OnceLock::new();

pub fn default_script() -> anyhow::Result<UiScript> {
    DEFAULT
        .get_or_init(|| UiScript::compile_fresh(crate::DEFAULT_UI).map_err(|e| format!("{e:#}")))
        .clone()
        .map_err(|e| anyhow!("{e}"))
}

/// Build the shared context and compile the default script now, on
/// whatever thread this is called from, so the UI thread finds both
/// ready. Startup calls this on a thread of its own while the catalog
/// and the tools load elsewhere; a compile that arrives first waits for
/// it to finish, never builds a second one.
pub fn warm() {
    let _ = default_script();
}

impl UiScript {
    /// Compile the prelude plus `ui_src` into one unit sharing a namespace.
    /// The default script is compiled once per process ([`default_script`]);
    /// any other source is compiled every time it is asked for, which is
    /// what `reload_ui` relies on.
    pub fn compile(ui_src: &str) -> anyhow::Result<Self> {
        if ui_src == crate::DEFAULT_UI {
            return default_script();
        }
        Self::compile_fresh(ui_src)
    }

    fn compile_fresh(ui_src: &str) -> anyhow::Result<Self> {
        let compiler = compiler()?;
        let context = &compiler.context;
        let mut sources = Sources::new();
        for (name, src) in [("prelude", crate::PRELUDE), ("ui", ui_src)] {
            sources
                .insert(Source::new(name, src).map_err(|e| anyhow!("source {name}: {e}"))?)
                .map_err(|e| anyhow!("inserting {name}: {e}"))?;
        }
        let mut diagnostics = Diagnostics::new();
        let result = rune::prepare(&mut sources)
            .with_context(context)
            .with_diagnostics(&mut diagnostics)
            .build();
        let unit = match result {
            Ok(u) => u,
            Err(_) => {
                let mut buf = Buffer::no_color();
                let rendered = match diagnostics.emit(&mut buf, &sources) {
                    Ok(()) => String::from_utf8_lossy(buf.as_slice()).into_owned(),
                    Err(e) => format!("<could not render diagnostics: {e}>"),
                };
                bail!("ui script failed to compile:\n{}", rendered.trim());
            }
        };
        Ok(UiScript {
            unit: Arc::new(unit),
            runtime: compiler.runtime.clone(),
        })
    }

    fn call(&self, name: &str, args: Vec<serde_json::Value>) -> anyhow::Result<serde_json::Value> {
        let mut vm = Vm::new(self.runtime.clone(), self.unit.clone());
        let args: Vec<Value> = args.iter().map(to_rune).collect();
        let exec = match args.len() {
            1 => vm.execute([name], (args[0].clone(),)),
            2 => vm.execute([name], (args[0].clone(), args[1].clone())),
            _ => vm.execute([name], ()),
        };
        let value = exec
            .map_err(|e| anyhow!("ui script has no `{name}`: {e}"))?
            .complete()
            .into_result()
            .map_err(|e| anyhow!("ui `{name}` failed: {e}"))?;
        serde_json::to_value(&value)
            .map_err(|e| anyhow!("ui `{name}` returned a value that is not data: {e}"))
    }

    pub fn view(&self, state: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.call("view", vec![state])
    }

    pub fn on_submit(&self, state: serde_json::Value, text: &str) -> anyhow::Result<Action> {
        Ok(parse_action(self.call(
            "on_submit",
            vec![state, serde_json::Value::String(text.into())],
        )?))
    }

    /// The keymap table, read once. Pure data: mode → key → command or
    /// submap. See [`crate::keys`] for the shape and the notation.
    pub fn keymap(&self) -> anyhow::Result<serde_json::Value> {
        self.call("keymap", vec![])
    }

    /// The mode table, if the script defines one — see [`crate::modes`].
    /// `None` is not an error: a script that says nothing about modes
    /// gets the default script's.
    pub fn modes(&self) -> Option<anyhow::Result<serde_json::Value>> {
        self.has("modes").then(|| self.call("modes", vec![]))
    }

    /// The theme, if the script defines one — see [`crate::theme`].
    pub fn theme(&self) -> Option<anyhow::Result<serde_json::Value>> {
        self.has("theme").then(|| self.call("theme", vec![]))
    }

    /// The label table, if the script defines one — see
    /// [`crate::labels`]. What tools are called on the transcript, as
    /// against their wire names.
    pub fn labels(&self) -> Option<anyhow::Result<serde_json::Value>> {
        self.has("labels").then(|| self.call("labels", vec![]))
    }

    /// The wait-tag table, if the script defines one — see
    /// [`crate::labels`]. What kinds of condition a parked wait's
    /// checklist names, as short words or glyphs in colours of their own.
    pub fn wait_tags(&self) -> Option<anyhow::Result<serde_json::Value>> {
        self.has("wait_tags").then(|| self.call("wait_tags", vec![]))
    }

    /// The script's own commands, if it declares any — see
    /// [`crate::command::Scripted`].
    pub fn commands(&self) -> Option<anyhow::Result<serde_json::Value>> {
        self.has("commands").then(|| self.call("commands", vec![]))
    }

    /// Run one of the script's commands: its function, handed the
    /// snapshot and the argument, answering with an action.
    pub fn run_command(
        &self,
        func: &str,
        state: serde_json::Value,
        arg: &str,
    ) -> anyhow::Result<Action> {
        Ok(parse_action(self.call(
            func,
            vec![state, serde_json::Value::String(arg.into())],
        )?))
    }

    /// Does the script define a function of this name?
    pub fn has(&self, name: &str) -> bool {
        Vm::new(self.runtime.clone(), self.unit.clone())
            .lookup_function([name])
            .is_ok()
    }
}

/// The default script's mode table — the base a user's `modes()` is laid
/// over, and what runs when the user's script says nothing about modes.
/// Falls back to the canonical three if the default script itself will
/// not compile, which the tests forbid.
pub fn default_modes() -> crate::modes::ModeTable {
    let builtin = crate::modes::ModeTable::builtin();
    default_script()
        .ok()
        .and_then(|s| s.modes()?.ok())
        .and_then(|v| crate::modes::ModeTable::from_value(&v, &builtin).ok())
        .unwrap_or(builtin)
}

/// The default script's theme, on the same footing as [`default_modes`].
pub fn default_theme() -> crate::theme::Theme {
    let builtin = crate::theme::Theme::builtin();
    default_script()
        .ok()
        .and_then(|s| s.theme()?.ok())
        .and_then(|v| crate::theme::Theme::from_value(&v, &builtin).ok())
        .map(|(t, _)| t)
        .unwrap_or(builtin)
}

/// The default script's label table, the same shape: the base the user's
/// `labels()` lays over.
pub fn default_labels() -> crate::labels::Labels {
    let builtin = crate::labels::builtin();
    default_script()
        .ok()
        .and_then(|s| s.labels()?.ok())
        .and_then(|v| crate::labels::Labels::from_value(&v, &builtin).ok())
        .unwrap_or(builtin)
}

/// The default script's wait-tag table, the same shape.
pub fn default_wait_tags() -> crate::labels::WaitTags {
    let builtin = crate::labels::builtin_wait_tags();
    default_script()
        .ok()
        .and_then(|s| s.wait_tags()?.ok())
        .and_then(|v| crate::labels::WaitTags::from_value(&v, &builtin).ok())
        .map(|(t, _)| t)
        .unwrap_or(builtin)
}

pub fn to_rune(v: &serde_json::Value) -> Value {
    Value::deserialize(v.clone().into_deserializer()).unwrap_or_else(|_| Value::from(()))
}

fn parse_action(v: serde_json::Value) -> Action {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    match v.get("action").and_then(|a| a.as_str()) {
        Some("send") => Action::Send(s("text")),
        Some("command") => Action::Command {
            name: s("name"),
            arg: s("arg"),
        },
        Some("dispatch") => Action::Dispatch(s("text")),
        Some("insert") => Action::Insert(s("text")),
        Some("info") => Action::Info(s("text")),
        Some("cancel") => Action::Cancel,
        Some("quit") => Action::Quit,
        _ => Action::None,
    }
}

fn ui_module() -> Result<Module, rune::ContextError> {
    let mut m = Module::with_crate("ui")?;
    m.function("roman", |n: i64| roman(n)).build()?;
    // The one wording of a token count, shared with `run` and `chat`
    // through `usage::human`: the status line says `12.4k` where the
    // summary at the end of a headless turn says `12.4k`, because two
    // spellings of one number invite the arithmetic of reconciling them.
    m.function("k", |n: i64| eidolon_core::usage::human(n.max(0) as u64))
        .build()?;
    // The context gauge, in `chat`'s wording: `142.3k/200.0k (71%)` where
    // the window is known and `142.3k` where it is not. A limit of zero is
    // "nobody said" — the convention `usage::context_line` already reads,
    // and the one the snapshot uses, since a Rune script counts in numbers
    // and has no null to hand back.
    //
    // A *negative* token count is that same convention for the other
    // argument: the branch has never recorded a context size, and the
    // gauge draws a dash. Zero could not carry it, because zero is a real
    // answer — it is what a freshly compacted session has.
    m.function("context", |tokens: i64, limit: i64| {
        eidolon_core::usage::context_line(
            (tokens >= 0).then_some(tokens as u64),
            (limit > 0).then_some(limit as u64),
        )
    })
    .build()?;
    // The pulse's word after that many calls, from the table the
    // transcript's folded thinking line reads too.
    m.function("cogitation", |calls: i64| crate::life::word(calls).to_string()).build()?;
    // The agent standing still, for the frame to wear when nothing is.
    m.function("agent_icon", crate::life::icon).build()?;
    m.function("parse_command", |text: String| -> Option<Value> {
        let t = text.trim();
        let body = t.strip_prefix(':')?;
        let (name, arg) = match body.split_once(char::is_whitespace) {
            Some((n, a)) => (n.to_string(), a.trim().to_string()),
            None => (body.to_string(), String::new()),
        };
        Some(to_rune(&serde_json::json!({ "name": name, "arg": arg })))
    })
    .build()?;
    m.function("commands", || -> Value {
        let list: Vec<serde_json::Value> = crate::command::COMMANDS
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "group": c.group,
                    "help": c.help,
                    "usage": crate::command::usage(c),
                    "driver": c.run == crate::command::Run::Driver,
                })
            })
            .collect();
        to_rune(&serde_json::Value::Array(list))
    })
    .build()?;
    Ok(m)
}

/// Roman numerals. 0 → "nulla".
///
/// This was the context gauge once, and is no longer: `ctx XII k` reads
/// as an ornament rather than a number, and a gauge is worth having only
/// while a glance at it answers "how much room is left". It stays in the
/// vocabulary because a script may still want it for something small
/// enough to be legible — a count of forks, a unit number — but the
/// status line counts tokens in digits.
pub fn roman(n: i64) -> String {
    if n <= 0 {
        return "nulla".into();
    }
    let mut n = n;
    let table = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut s = String::new();
    for (v, r) in table {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_script_compiles_and_answers() {
        let s = UiScript::compile(crate::DEFAULT_UI).unwrap();
        let state = serde_json::json!({ "model": "m", "persona": "", "session": "s", "cwd": "/", "working": false, "elapsed_ms": 0,
            "context_tokens": 12000, "context_limit": 200_000, "usage_in": 1, "usage_out": 2, "spend": "", "tools_run": 0, "last_tool": null, "dialog": false,
            "following": true, "detail": "folded", "mode": "insert", "mode_colour": "green", "mode_label": "INS", "mode_sigil": "",
            "mode_kind": "insert", "pending": "", "empty": true, "copied": false,
            "queued": 0, "transcript_len": 0, "fresh": false, "tick": 0, "dispatch": "mneme",
            "tracing": false, "trace_at": 0, "trace_blocks": 0, "trace_kind": "", "trace_open": false,
            "trace_extend": false, "trace_span": 0, "yolo": false });
        let tree = s.view(state.clone()).unwrap();
        assert_eq!(tree["kind"], "column");
        // The context gauge is digits against the window, in the wording
        // `chat` closes a session with. It read `ctx XII k` once — an
        // ornament, at exactly the length of session where the number
        // starts to be worth reading, and against no denominator at all.
        assert!(
            tree.to_string().contains("ctx 12.0k/200.0k (6%)"),
            "status line: {tree}"
        );
        // A model that declares no window gets the count and no fraction.
        let mut unknown = state.clone();
        unknown["context_limit"] = serde_json::json!(0);
        let tree = s.view(unknown).unwrap();
        assert!(
            tree.to_string().contains("ctx 12.0k ·"),
            "status line: {tree}"
        );
        assert_eq!(
            s.on_submit(state.clone(), "hello").unwrap(),
            Action::Send("hello".into())
        );
        assert_eq!(
            s.on_submit(state.clone(), ":model x y").unwrap(),
            Action::Command {
                name: "model".into(),
                arg: "x y".into()
            }
        );
        assert_eq!(
            s.on_submit(state.clone(), ":quit").unwrap(),
            Action::Command {
                name: "quit".into(),
                arg: String::new()
            }
        );
        // `/` is reserved: it is text now, not a command.
        assert_eq!(
            s.on_submit(state.clone(), "/model x y").unwrap(),
            Action::Send("/model x y".into())
        );
        // In dispatch mode the whole line is an utterance, colon and all:
        // the mode was aimed before any of it was typed.
        let mut aimed = state.clone();
        aimed["mode"] = serde_json::Value::String("dispatch".into());
        assert_eq!(
            s.on_submit(aimed.clone(), "read the note Groceries")
                .unwrap(),
            Action::Dispatch("read the note Groceries".into())
        );
        assert_eq!(
            s.on_submit(aimed, ":tree").unwrap(),
            Action::Dispatch(":tree".into())
        );
    }

    #[test]
    fn the_default_keymap_covers_every_mode() {
        let s = UiScript::compile(crate::DEFAULT_UI).unwrap();
        let km = s.keymap().unwrap();
        for mode in ["normal", "insert", "select", "dispatch"] {
            assert!(
                km.get(mode).is_some_and(|m| m.is_object()),
                "no {mode} keymap"
            );
        }
        assert_eq!(km["normal"]["w"]["cmd"], "move_next_word_start");
        // Every name in the table is one the registry knows.
        assert_eq!(
            crate::keys::unknown_commands(&km, &[]),
            Vec::<String>::new()
        );
        // The shift-arrows are cloned into every mode, as in the Helix config.
        for mode in ["normal", "insert", "select"] {
            for key in ["S-up", "S-down", "S-left", "S-right"] {
                assert!(km[mode][key].is_object(), "{mode} is missing {key}");
            }
        }
        assert_eq!(km["insert"]["esc"]["cmd"], "normal_mode");
        // Dispatch mode is insert's table with three keys changed: `tab`
        // picks the surface and `esc` takes the line down, because it is
        // a minibuffer rather than the prompt in another mode.
        // Completion is the command line's own.
        assert_eq!(km["dispatch"]["esc"]["cmd"], "cancel");
        assert_eq!(km["dispatch"]["ret"]["cmd"], "submit");
        assert_eq!(km["dispatch"]["tab"]["cmd"], "dispatch_surface");
        assert_eq!(km["command"]["esc"]["cmd"], "cancel");
        assert_eq!(
            km["command"]["tab"]["cmd"], "complete_next",
            "and the command line completes"
        );
        assert!(
            km["insert"].get("tab").is_none(),
            "insert has no completion to offer"
        );
        // The readline keys, on every surface text is typed into and on
        // none of the modal ones.
        for mode in ["insert", "command", "dispatch", "search", "pick"] {
            assert_eq!(
                km[mode]["C-a"]["cmd"], "goto_line_start",
                "{mode} is missing C-a"
            );
            assert_eq!(
                km[mode]["C-e"]["cmd"], "goto_line_end_newline",
                "{mode} is missing C-e"
            );
            assert_eq!(
                km[mode]["C-u"]["cmd"], "delete_to_line_start",
                "{mode} is missing C-u"
            );
            assert_eq!(
                km[mode]["C-w"]["cmd"], "delete_word_backward",
                "{mode} is missing C-w"
            );
        }
        for mode in ["normal", "select"] {
            assert!(
                km[mode].get("C-u").is_none(),
                "{mode} keeps the readline kills out"
            );
            // The one control chord normal keeps is Helix's, not
            // readline's: increment and decrement, beside the displaced
            // till on `C-t`.
            assert_eq!(km[mode]["C-a"]["cmd"], "increment");
            assert_eq!(km[mode]["C-x"]["cmd"], "decrement");
        }
        // The space menu is a submap, not a leaf.
        assert!(km["normal"]["space"].is_object());
    }

    #[test]
    fn the_default_script_declares_every_mode_the_harness_knows() {
        let s = UiScript::compile(crate::DEFAULT_UI).unwrap();
        let modes = s.modes().expect("default.rn defines modes()").unwrap();
        for m in [
            "normal", "insert", "select", "dispatch", "command", "search",
        ] {
            assert!(modes.get(m).is_some(), "modes() lacks {m}");
        }
        let table = default_modes();
        assert_eq!(table.style("dispatch").sigil, Some('!'));
        assert_eq!(
            table.style("command").kind,
            None,
            "the command line is style-only"
        );
        // The theme names every role the renderer paints, so a reader of
        // default.rn sees the whole vocabulary.
        let theme = s.theme().expect("default.rn defines theme()").unwrap();
        for (role, _) in crate::theme::ROLES {
            assert!(theme.get(role).is_some(), "theme() lacks {role}");
        }
        // And every keymap table is for a row the table defines — an
        // editing mode, or one of the style-only rows that take keys
        // (the command line, the search line, the dialogs).
        let km = s.keymap().unwrap();
        for k in km.as_object().unwrap().keys() {
            assert!(
                table.get(k).is_some(),
                "keymap has a table for `{k}` but modes() has no such row"
            );
        }
        for k in ["command", "search", "confirm", "choose", "pick"] {
            assert!(km.get(k).is_some(), "the default keymap has no `{k}` table");
        }
    }

    #[test]
    fn a_script_without_the_optional_tables_is_not_an_error() {
        let s = UiScript::compile("pub fn view(s) { #{ kind: \"empty\" } }\npub fn keymap() { #{} }\npub fn on_submit(s, t) { send(t) }").unwrap();
        assert!(s.modes().is_none());
        assert!(s.theme().is_none());
    }

    #[test]
    fn roman_numerals() {
        assert_eq!(roman(1994), "MCMXCIV");
        assert_eq!(roman(0), "nulla");
    }
}
