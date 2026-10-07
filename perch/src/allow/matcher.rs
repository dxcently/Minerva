//! The matcher (slice A1b): a pure function from the rules, one pending ask and the door to the rule that answers it, or none.
//! No I/O and no clock, so every clause of "when a rule matches" is a unit test, and [`pattern_of`] (what a rule is made from) lives
//! beside [`find`] (what a rule answers) so that the two cannot drift: a rule can be made from an ask exactly when the same ask, asked
//! again, would match it.
//!
//! **A rule matches an ask only when every one of these holds.** Anything unclear is a no: the person is asked, which is what an ask
//! is anyway.
//!
//! ```text
//!   the rule is this door's             the DoorKey the rule was made under is the door the ask came from
//!   the chat is auto or manual          yolo never (the door answers itself), and the mode now is the mode at creation
//!   the ask is an approval              a question is never answered
//!   the ask was first seen after it     ask.seen_seq > rule.stamp (and seen_seq != 0, which is "pending before the watcher saw it"),
//!                                       and ask.ask_id > the highest ask id the door had shown when the rule was made
//!   the origin is known and the same    a model rule never answers a script's nested call; `None` (unknown, disputed) never matches
//!   the tool name is byte-exact         "Bash", "bash " and a lookalike are other tools
//!   bash:  input.command is a String equal byte for byte (no trim, no CRLF folding, no Unicode folding), input.background is absent or
//!          `false` (null, "true", 1 all fail), no field but command, timeout_s and background, and the door's hello cwd is the rule's
//!   other: ask.structural == false      eidolon asks about the call's shape (outside the working directory), not its name
//! ```
//!
//! The ask is always the watcher's own copy, re-read at the moment of deciding ([`crate::watch::AskView::ask`]), never the note the sink
//! was told: the origin of a call can become known after its ask opened, and can be disputed after.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::store::Rule;
use crate::mode::Mode;
use crate::watch::{AskKind, DoorKey, OpenAsk, Origin};

/// The one tool whose rule is an exact command. A user tool registered under this name has this tool's rules (XS states it).
pub const BASH: &str = "bash";

/// The only fields a bash call may carry for a rule to cover it: the builtin tool's whole schema (`eidolon/crates/rune/builtin/bash.rn`).
/// A field beyond these is a field no rule was shown, so the call is a person's to judge.
const BASH_FIELDS: [&str; 3] = ["command", "timeout_s", "background"];

/// What a rule allows, fixed when the rule was made from an ask and never changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    pub tool: String,
    /// The origin of the call the rule was made from; a rule answers only that origin.
    pub origin: Origin,
    pub shape: Shape,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Every call of the tool whose ask is not about the call's shape.
    Name,
    /// `bash` with exactly this command, in the foreground, in the working directory the door said in `hello`.
    Command { command: String, cwd: PathBuf },
}

/// What the matcher needs to know of the door an ask came from. The mode is the effective one: `Yolo` when the door's own `hello` said
/// so, else the registry's.
#[derive(Clone, Copy, Debug)]
pub struct DoorFacts<'a> {
    pub key: DoorKey,
    pub mode: Mode,
    /// The cwd the door said in `hello`: where its bash calls run.
    pub cwd: &'a Path,
}

/// Why a rule cannot be made from an ask. [`Refusal::status`] is the route's answer: 409 for what is not the ask's shape (it is not an
/// approval), 422 for an approval a rule must not cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A question, not an approval.
    NotAnApproval,
    /// An approval that names no tool, or a bash call whose command is not a string.
    Unreadable,
    /// Who started the call is not known (its frame was never read, or two frames disagreed).
    UnknownOrigin,
    /// Eidolon asks about this call's shape rather than its name, so no rule by name.
    Structural,
    /// A bash call that is not plainly in the foreground (`background` true, or anything but absent and `false`).
    Background,
    /// A bash call with a field beyond `command`, `timeout_s` and `background`.
    Fields,
}

impl Refusal {
    pub fn status(self) -> u16 {
        match self {
            Refusal::NotAnApproval => 409,
            _ => 422,
        }
    }

    /// A sentence for the person: it names the rule, never the call.
    pub fn why(self) -> &'static str {
        match self {
            Refusal::NotAnApproval => "that ask is a question, not an approval",
            Refusal::Unreadable => "that ask does not carry a tool and a command a rule can be made from",
            Refusal::UnknownOrigin => "who started that call is not known, so no rule can be made from it",
            Refusal::Structural => "eidolon asks about this call's shape, not its name",
            Refusal::Background => "a call that runs in the background is never allowed by a rule",
            Refusal::Fields => "that call has fields a command rule does not cover",
        }
    }
}

/// Whether a bash call's `background` is plainly off: absent, or the boolean `false`. Anything else (`true`, `"true"`, `null`, `0`) is not.
fn foreground(input: &Value) -> bool {
    matches!(input.get("background"), None | Some(Value::Bool(false)))
}

/// Whether the call carries only the fields the builtin bash tool has.
fn known_fields(input: &Value) -> bool {
    input.as_object().is_some_and(|fields| fields.keys().all(|key| BASH_FIELDS.contains(&key.as_str())))
}

/// The pattern a rule made from `ask` would have, or why none can be. `cwd` is the door's `hello` cwd. The order of the checks is the
/// order of [`Refusal`]'s variants, so the first reason is the one reported.
pub fn pattern_of(ask: &OpenAsk, cwd: &Path) -> Result<Pattern, Refusal> {
    if ask.kind != AskKind::Approval {
        return Err(Refusal::NotAnApproval);
    }
    let Some(tool) = ask.tool.as_deref().filter(|tool| !tool.is_empty()) else { return Err(Refusal::Unreadable) };
    let Some(origin) = ask.origin else { return Err(Refusal::UnknownOrigin) };
    let shape = if tool == BASH {
        let input = ask.input();
        let Some(command) = input.get("command").and_then(Value::as_str) else { return Err(Refusal::Unreadable) };
        if !foreground(&input) {
            return Err(Refusal::Background);
        }
        if !known_fields(&input) {
            return Err(Refusal::Fields);
        }
        Shape::Command { command: command.to_string(), cwd: cwd.to_path_buf() }
    } else {
        if ask.structural {
            return Err(Refusal::Structural);
        }
        Shape::Name
    };
    Ok(Pattern { tool: tool.to_string(), origin, shape })
}

/// The clauses that need nothing of the ask's input.
fn cheap(rule: &Rule, ask: &OpenAsk, door: &DoorFacts<'_>) -> bool {
    rule.door == door.key
        && matches!(door.mode, Mode::Auto | Mode::Manual)
        && rule.mode == door.mode
        && ask.kind == AskKind::Approval
        // `seen_seq == 0` is an ask that was pending before the watcher first saw it: it may predate any rule, so no stamp is low enough.
        && ask.seen_seq != 0
        && ask.seen_seq > rule.stamp
        && ask.ask_id > rule.max_ask
        // `None` (unknown or disputed) is not `Some(anything)`.
        && ask.origin == Some(rule.pattern.origin)
        && ask.tool.as_deref() == Some(rule.pattern.tool.as_str())
}

/// The input-dependent clauses, for a rule that passed [`cheap`]. `input` is the ask's parsed input, parsed once for all the rules.
fn shape_matches(rule: &Rule, ask: &OpenAsk, door: &DoorFacts<'_>, input: &OnceCell<Value>) -> bool {
    match &rule.pattern.shape {
        Shape::Name => rule.pattern.tool != BASH && !ask.structural,
        Shape::Command { command, cwd } => {
            // The folder is compared as the bytes the door said, not as a path: a trailing slash or a `.` is not the same word.
            if rule.pattern.tool != BASH || door.cwd.as_os_str() != cwd.as_os_str() {
                return false;
            }
            let input = input.get_or_init(|| ask.input());
            input.get("command").and_then(Value::as_str) == Some(command.as_str()) && foreground(input) && known_fields(input)
        }
    }
}

/// The rule that answers `ask`, as its `rid`; the oldest when several do. `None` is "ask the person".
pub fn find(rules: &[Rule], ask: &OpenAsk, door: &DoorFacts<'_>) -> Option<u64> {
    let input = OnceCell::new();
    rules.iter().find(|rule| cheap(rule, ask, door) && shape_matches(rule, ask, door, &input)).map(|rule| rule.rid)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::watch::AskKind;

    const KEY: DoorKey = DoorKey { pid: 4242, port: 40000 };
    const CWD: &str = "/work/chat";

    fn bash(command: &str) -> OpenAsk {
        ask("bash", json!({ "command": command }))
    }

    /// A model-origin approval on a non-structural call, the ninth ask the watcher saw (id 9).
    fn ask(tool: &str, input: Value) -> OpenAsk {
        OpenAsk {
            ask_id: 9,
            kind: AskKind::Approval,
            call_id: Some("c9".into()),
            tool: Some(tool.into()),
            input_json: Arc::from(input.to_string()),
            structural: false,
            reason: Some("outside-cwd".into()),
            origin: Some(Origin::Model),
            seen_seq: 9,
        }
    }

    /// A rule made at stamp 3 (when the watcher had opened three asks and the door had shown id 3), in auto mode, for `pattern`.
    fn rule(pattern: Pattern) -> Rule {
        Rule { rid: 1, door: KEY, mode: Mode::Auto, stamp: 3, max_ask: 3, pattern, added_ms: 0, uses: 0, last_ms: None }
    }

    fn command_rule(command: &str) -> Rule {
        rule(Pattern { tool: "bash".into(), origin: Origin::Model, shape: Shape::Command { command: command.into(), cwd: CWD.into() } })
    }

    fn name_rule(tool: &str) -> Rule {
        rule(Pattern { tool: tool.into(), origin: Origin::Model, shape: Shape::Name })
    }

    fn door(mode: Mode) -> DoorFacts<'static> {
        DoorFacts { key: KEY, mode, cwd: Path::new(CWD) }
    }

    fn matched(rule: &Rule, ask: &OpenAsk) -> bool {
        find(std::slice::from_ref(rule), ask, &door(Mode::Auto)) == Some(rule.rid)
    }

    #[test]
    fn the_exact_command_in_the_same_place_from_the_same_origin_matches() {
        let rule = command_rule("cargo test");
        assert!(matched(&rule, &bash("cargo test")));
        // `timeout_s` is ignored, whatever it holds; `background: false` is the foreground.
        assert!(matched(&rule, &ask("bash", json!({ "command": "cargo test", "timeout_s": 5 }))));
        assert!(matched(&rule, &ask("bash", json!({ "command": "cargo test", "timeout_s": "soon", "background": false }))));
    }

    #[test]
    fn a_command_that_is_not_the_same_bytes_never_matches() {
        let rule = command_rule("cargo test");
        for other in ["cargo test ", " cargo test", "cargo test\n", "cargo test\r\n", "cargo  test", "Cargo test", "cargo test;", "cargo tes\u{0442}", "cargo\u{a0}test", "cargo t\u{0435}st"] {
            assert!(!matched(&rule, &bash(other)), "{other:?}");
        }
        // Not a string at all.
        for input in [json!({ "command": ["cargo", "test"] }), json!({ "command": 7 }), json!({ "command": null }), json!({}), json!("cargo test"), json!(null)] {
            assert!(!matched(&rule, &ask("bash", input.clone())), "{input}");
        }
    }

    #[test]
    fn a_different_working_directory_never_matches() {
        let rule = command_rule("cargo test");
        let elsewhere = DoorFacts { key: KEY, mode: Mode::Auto, cwd: Path::new("/work/other") };
        assert_eq!(find(std::slice::from_ref(&rule), &bash("cargo test"), &elsewhere), None);
        // Bytes, not components: a trailing slash or a `.` is another word, so it is a person's to judge.
        for spelled in ["/work/chat/", "/work/./chat", "/work//chat", "work/chat", ""] {
            let facts = DoorFacts { key: KEY, mode: Mode::Auto, cwd: Path::new(spelled) };
            assert_eq!(find(std::slice::from_ref(&rule), &bash("cargo test"), &facts), None, "{spelled:?}");
        }
        let sibling = DoorFacts { key: KEY, mode: Mode::Auto, cwd: Path::new("/work/chat2") };
        assert_eq!(find(std::slice::from_ref(&rule), &bash("cargo test"), &sibling), None);
    }

    #[test]
    fn a_background_call_never_matches_and_neither_does_anything_but_absent_or_false() {
        let rule = command_rule("cargo test");
        for background in [json!(true), json!("true"), json!("false"), json!(null), json!(1), json!(0), json!([]), json!({})] {
            let call = ask("bash", json!({ "command": "cargo test", "background": background.clone() }));
            assert!(!matched(&rule, &call), "background: {background}");
        }
    }

    #[test]
    fn a_bash_call_with_a_field_the_rule_never_saw_never_matches() {
        let rule = command_rule("cargo test");
        for extra in ["cwd", "workdir", "env", "sudo", "description", "Command", "command "] {
            let call = ask("bash", json!({ "command": "cargo test", extra: "x" }));
            assert!(!matched(&rule, &call), "{extra}");
        }
    }

    #[test]
    fn a_question_is_never_answered() {
        let rule = command_rule("cargo test");
        let mut question = bash("cargo test");
        question.kind = AskKind::Question;
        assert!(!matched(&rule, &question));
        let name = name_rule("write");
        let mut asked = ask("write", json!({}));
        asked.kind = AskKind::Question;
        assert!(!matched(&name, &asked));
    }

    #[test]
    fn yolo_never_matches_and_the_mode_must_still_be_the_mode_the_rule_was_made_in() {
        let rule = command_rule("cargo test");
        assert_eq!(find(std::slice::from_ref(&rule), &bash("cargo test"), &door(Mode::Yolo)), None);
        // Made in auto, asked in manual: another mode's decision.
        assert_eq!(find(std::slice::from_ref(&rule), &bash("cargo test"), &door(Mode::Manual)), None);
        let mut manual = command_rule("cargo test");
        manual.mode = Mode::Manual;
        assert_eq!(find(std::slice::from_ref(&manual), &bash("cargo test"), &door(Mode::Manual)), Some(1));
        // A rule that says yolo (nothing makes one) answers nothing.
        let mut loose = command_rule("cargo test");
        loose.mode = Mode::Yolo;
        assert_eq!(find(std::slice::from_ref(&loose), &bash("cargo test"), &door(Mode::Yolo)), None);
    }

    #[test]
    fn a_structural_ask_is_never_answered_by_a_name_rule() {
        let rule = name_rule("write");
        let mut call = ask("write", json!({ "path": "/etc/passwd" }));
        assert!(matched(&rule, &call), "a name rule answers a call whose ask is not about its shape, whatever the input holds");
        call.structural = true;
        assert!(!matched(&rule, &call));
    }

    #[test]
    fn a_name_rule_for_bash_answers_nothing() {
        // Nothing makes one (`pattern_of` makes a command rule for bash), and if one existed it would be a rule for every command.
        let rule = name_rule("bash");
        assert!(!matched(&rule, &bash("rm -rf /")));
    }

    #[test]
    fn the_tool_name_is_byte_exact() {
        let rule = name_rule("write");
        for other in ["Write", "write ", " write", "writ", "write2", "wr\u{0131}te", "", "bash"] {
            assert!(!matched(&rule, &ask(other, json!({}))), "{other:?}");
        }
        let command = command_rule("ls");
        for other in ["Bash", "bash ", "BASH", "sh"] {
            assert!(!matched(&command, &ask(other, json!({ "command": "ls" }))), "{other:?}");
        }
        let mut nameless = ask("write", json!({}));
        nameless.tool = None;
        assert!(!matched(&rule, &nameless));
    }

    #[test]
    fn an_ask_pending_before_the_rule_never_matches() {
        let rule = command_rule("cargo test");
        for seen in [0, 1, 2, 3] {
            let mut call = bash("cargo test");
            call.seen_seq = seen;
            assert!(!matched(&rule, &call), "seen_seq {seen}: not after the stamp");
        }
        let mut after = bash("cargo test");
        after.seen_seq = 4;
        after.ask_id = 4;
        assert!(matched(&rule, &after), "the first ask seen after the rule's stamp");
        // `0` is "pending before the watcher saw it": no stamp is low enough, not even a rule made before the first ask.
        let mut fresh = command_rule("cargo test");
        fresh.stamp = 0;
        fresh.max_ask = 0;
        let mut replayed = bash("cargo test");
        replayed.seen_seq = 0;
        assert!(!matched(&fresh, &replayed));
        // The door's ids only go up: an id at or below the one the door had shown when the rule was made was asked before it, whether or
        // not the watcher held it.
        let mut old_id = bash("cargo test");
        old_id.ask_id = 3;
        old_id.seen_seq = 10;
        assert!(!matched(&rule, &old_id));
    }

    #[test]
    fn a_script_origin_ask_is_not_answered_by_a_model_rule_and_an_unknown_origin_is_answered_by_none() {
        let rule = command_rule("cargo test");
        for origin in [Some(Origin::Script), Some(Origin::User), None] {
            let mut call = bash("cargo test");
            call.origin = origin;
            assert!(!matched(&rule, &call), "{origin:?}");
        }
        // And the other way round: a rule made on a script's call answers script calls, not the model's.
        let mut script_rule = command_rule("cargo test");
        script_rule.pattern.origin = Origin::Script;
        assert!(!matched(&script_rule, &bash("cargo test")));
        let mut script_call = bash("cargo test");
        script_call.origin = Some(Origin::Script);
        assert!(matched(&script_rule, &script_call));
    }

    #[test]
    fn another_doors_rule_never_answers() {
        let rule = command_rule("cargo test");
        for other in [DoorKey { pid: 4243, port: 40000 }, DoorKey { pid: 4242, port: 40001 }] {
            let facts = DoorFacts { key: other, mode: Mode::Auto, cwd: Path::new(CWD) };
            assert_eq!(find(std::slice::from_ref(&rule), &bash("cargo test"), &facts), None, "{other:?}");
        }
    }

    #[test]
    fn the_oldest_matching_rule_is_the_one() {
        let mut first = command_rule("cargo test");
        first.rid = 7;
        let mut second = command_rule("cargo test");
        second.rid = 8;
        let mut other = command_rule("ls");
        other.rid = 6;
        assert_eq!(find(&[other, first, second], &bash("cargo test"), &door(Mode::Auto)), Some(7));
    }

    #[test]
    fn a_rule_is_made_from_an_ask_exactly_when_the_ask_would_match_it() {
        let cwd = Path::new(CWD);
        let cases = [
            bash("cargo test"),
            ask("bash", json!({ "command": "cargo test", "timeout_s": 9 })),
            ask("bash", json!({ "command": "cargo test", "background": false })),
            ask("write", json!({ "path": "a" })),
            ask("fetch", json!({})),
        ];
        for call in cases {
            let pattern = pattern_of(&call, cwd).unwrap_or_else(|why| panic!("{call:?}: {why:?}"));
            let rule = rule(pattern);
            let mut again = call.clone();
            again.ask_id = 10;
            again.seen_seq = 10;
            assert!(matched(&rule, &again), "{call:?}");
        }
    }

    #[test]
    fn what_a_rule_cannot_be_made_from() {
        let cwd = Path::new(CWD);
        let refused = |call: &OpenAsk| pattern_of(call, cwd).unwrap_err();
        let mut question = bash("ls");
        question.kind = AskKind::Question;
        assert_eq!(refused(&question), Refusal::NotAnApproval);
        assert_eq!(Refusal::NotAnApproval.status(), 409);

        let mut unknown = bash("ls");
        unknown.origin = None;
        assert_eq!(refused(&unknown), Refusal::UnknownOrigin);

        let mut nameless = bash("ls");
        nameless.tool = None;
        assert_eq!(refused(&nameless), Refusal::Unreadable);
        let mut empty = bash("ls");
        empty.tool = Some(String::new());
        assert_eq!(refused(&empty), Refusal::Unreadable);
        assert_eq!(refused(&ask("bash", json!({ "command": 3 }))), Refusal::Unreadable);
        assert_eq!(refused(&ask("bash", json!({}))), Refusal::Unreadable);

        for background in [json!(true), json!("true"), json!(null), json!(1)] {
            assert_eq!(refused(&ask("bash", json!({ "command": "ls", "background": background }))), Refusal::Background);
        }
        assert_eq!(refused(&ask("bash", json!({ "command": "ls", "workdir": "/" }))), Refusal::Fields);

        let mut structural = ask("write", json!({}));
        structural.structural = true;
        assert_eq!(refused(&structural), Refusal::Structural);
        for why in [Refusal::Unreadable, Refusal::UnknownOrigin, Refusal::Structural, Refusal::Background, Refusal::Fields] {
            assert_eq!(why.status(), 422);
            assert!(why.why().is_ascii() && !why.why().is_empty());
        }
    }

    #[test]
    fn a_structural_bash_ask_can_be_a_command_rule_and_a_structural_name_cannot() {
        // The exact command, in the exact place, is what a person approved; the shape that made eidolon ask is part of that command.
        let mut compound = bash("cd /tmp && ls");
        compound.structural = true;
        let pattern = pattern_of(&compound, Path::new(CWD)).unwrap();
        assert_eq!(pattern.shape, Shape::Command { command: "cd /tmp && ls".into(), cwd: CWD.into() });
    }
}
