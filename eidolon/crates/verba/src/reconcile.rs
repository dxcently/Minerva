//! One reconciliation of VV's vocabulary with Mneme's, for both consumers.
//!
//! The corpus's slot names are the **checkpoint's**; the vault's RPC takes
//! different ones for the intents that differ — `read_note` is bound as `note`
//! and takes `title`, `read_version`'s `version` is `id`, `edit_note`'s
//! `target` is `old_str` — and two arguments are not strings on the wire
//! (`similar_notes.k` is a number, `edit_note.replace_all` a bool). This module
//! is that mapping as one table, and its two callers are the two ways a line
//! gets run:
//!
//! - **dispatch mode**, [`Bundle::call_for`](crate::palette::Bundle::call_for)'s
//!   `mneme_rpc` arm — the TUI's `:do`, `eidolon do` — where a human typed the
//!   line and a human sees what ran;
//! - the **command channel**, [`crate::command`], where the model typed the
//!   line and the loop serialises it.
//!
//! Dispatch mode predates the channel and never knew the two vocabularies
//! diverge: it sent the corpus's slot names verbatim, so every renamed intent —
//! `read note "X"` included — classified correctly and then died at the vault
//! (`failed to deserialize parameters: missing field \`title\``, code -32602),
//! which is exactly the misnaming class the channel exists to remove. One table
//! serves both so the two cannot drift apart.
//!
//! **Interim.** The table is hardcoded. The plan is an `rpc_adapter.json`
//! published beside the weights, carrying the same rows and their types, at
//! which point both consumers read the artifact and this table goes. Until
//! then this is authoritative, and the lead's own ledger
//! (`VerbaVolantia/tools/token_savings.py::RPC_ADAPTER`) mirrors it — keep the
//! copies in step.
//!
//! **Not policy.** What may run is the caller's question: the channel's
//! allowlist refuses writes, dispatch mode is a human who asked for one. This
//! module says what the call looks like once something has decided to make it,
//! and nothing about whether it should.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// The classifier's `TRAIL` pseudo-slot is a span of the utterance, not an
/// argument to anything, so it is never forwarded — it is dropped here rather
/// than by a caller because it is a property of the corpus's verdict, which is
/// the input this module exists to translate.
const TRAIL: &str = "TRAIL";

/// The wire type of one argument, where it is not a string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A stringly number becomes a JSON number (`similar_notes.k`).
    Number,
    /// A stringly boolean becomes a JSON bool (`edit_note.replace_all`).
    Bool,
}

/// One intent whose call is more than its own name over the slots as they
/// arrived.
///
/// There is no function column: the corpus names every intent after the
/// function it maps to, so a row only answers what an argument is *called* and
/// what type it is on the wire. `conventions` is the one intent whose name
/// depends on its own arguments, and it is the one arm written by hand in
/// [`reconcile`].
struct Row {
    intent: &'static str,
    renames: &'static [(&'static str, &'static str)],
    /// Arguments that are not strings on the wire. Everything else is passed
    /// through as it arrived, which is right for every argument that names
    /// something — and *necessary* for `read_version.id`, a string on Mneme's
    /// side that a stringly-number guess would turn into the rejected call this
    /// channel exists to stop making.
    typed: &'static [(&'static str, Kind)],
}

/// Every intent the corpus and the vault spell differently. An intent that is
/// not here is sent as it arrived, which is right for the large majority of
/// them: `search_vault`, `list_notes`, `embed_text` and the rest agree.
const ROWS: &[Row] = &[
    // Reads.
    Row { intent: "read_note", renames: &[("note", "title")], typed: &[] },
    Row { intent: "read_canvas", renames: &[("note", "title")], typed: &[] },
    Row { intent: "read_version", renames: &[("version", "id")], typed: &[] },
    // The name is already right and only the type is not: `k` is a uint32.
    Row { intent: "similar_notes", renames: &[], typed: &[("k", Kind::Number)] },
    // Writes. The three the channel may run carry their payload in a fence and
    // are reconciled like the reads; the rest are refused by the gate before
    // reconciliation is consulted — dispatch mode runs them, and it needs the
    // names as much as the reads do.
    Row { intent: "create_note", renames: &[("note", "title")], typed: &[] },
    Row { intent: "append_to_note", renames: &[("note", "title")], typed: &[] },
    Row {
        intent: "edit_note",
        renames: &[("note", "title"), ("target", "old_str"), ("content", "new_str")],
        typed: &[("replace_all", Kind::Bool)],
    },
    Row {
        intent: "replace_section",
        renames: &[("note", "title"), ("section", "heading"), ("content", "new_content")],
        typed: &[],
    },
    Row { intent: "create_canvas", renames: &[("note", "title")], typed: &[] },
    Row { intent: "delete_note", renames: &[("note", "title")], typed: &[] },
    Row { intent: "move_note", renames: &[("note", "title")], typed: &[] },
    Row { intent: "rename_note", renames: &[("note", "old_title"), ("name", "new_title")], typed: &[] },
    Row { intent: "restore_note", renames: &[("note", "name")], typed: &[] },
    Row { intent: "restore_version", renames: &[("version", "id")], typed: &[] },
];

/// VV's intent and slots → Mneme's function and arguments.
///
/// Total, and deliberately so: an intent this table does not name is sent under
/// its own name with its slots as they arrived ([`crate::command`] has already
/// refused anything it may not run, and a human's dispatch is their own
/// business). Guessing a type for an argument whose schema nobody here has read
/// is the failure this replaces, not a fallback from it.
pub fn reconcile(intent: &str, slots: &BTreeMap<String, Value>) -> (String, Map<String, Value>) {
    // The one intent whose *name* depends on its own arguments: a slug names a
    // single convention, no slug means the whole canon. A blank slug is a slot
    // the classifier bound nothing into, so it is the canon and not a lookup
    // for the empty convention.
    if intent == "conventions" {
        return match slots.get("slug").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            // A slug names one convention, and the corpus's `slug` is the
            // vault's `name`: the same kind of rename the table below carries,
            // applied from here because the *function* turns on it too.
            Some(_) => ("read_convention".to_string(), map_slots(&[("slug", "name")], &[], slots)),
            None => ("get_conventions".to_string(), Map::new()),
        };
    }
    match ROWS.iter().find(|r| r.intent == intent) {
        Some(row) => (intent.to_string(), map_slots(row.renames, row.typed, slots)),
        None => (intent.to_string(), map_slots(&[], &[], slots)),
    }
}

/// The slots, renamed and typed, minus the classifier's own bookkeeping.
///
/// A slot the model bound but the call has no room for is *kept*, here as in
/// the channel: an argument Mneme does not know is ignored on the wire
/// (measured), and dropping it would hide a binding that went nowhere.
fn map_slots(
    renames: &[(&str, &str)],
    typed: &[(&str, Kind)],
    slots: &BTreeMap<String, Value>,
) -> Map<String, Value> {
    let mut args = Map::new();
    for (key, value) in slots {
        if key == TRAIL {
            continue;
        }
        let name = renames
            .iter()
            .find(|(from, _)| *from == key)
            .map(|(_, to)| *to)
            .unwrap_or(key.as_str());
        let value = match typed.iter().find(|(slot, _)| slot == key) {
            Some((_, kind)) => typed_value(value, *kind),
            None => value.clone(),
        };
        args.insert(name.to_string(), value);
    }
    args
}

/// Give one argument its JSON type where the wire has one, and leave it exactly
/// as it arrived where the string is not that type — so the vault says what it
/// made of the value rather than this guessing at it.
fn typed_value(value: &Value, kind: Kind) -> Value {
    match kind {
        Kind::Number => value
            .as_str()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .map(Value::from)
            .unwrap_or_else(|| value.clone()),
        Kind::Bool => match value.as_str() {
            Some("true") => Value::Bool(true),
            Some("false") => Value::Bool(false),
            _ => value.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A slots map as the classifier prints one.
    fn slots(v: Value) -> BTreeMap<String, Value> {
        serde_json::from_value(v).expect("an object of slots")
    }

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().expect("an object").clone()
    }

    /// Every row in the table, both directions: the corpus's name on the left,
    /// Mneme's on the right.
    #[test]
    fn every_divergent_intent_arrives_under_mneme_names() {
        /// intent, corpus slots, expected args.
        type Case = (&'static str, Value, Value);
        let cases: &[Case] = &[
            ("read_note", json!({ "note": "Groceries" }), json!({ "title": "Groceries" })),
            ("read_canvas", json!({ "note": "Roadmap" }), json!({ "title": "Roadmap" })),
            ("read_version", json!({ "note": "Groceries", "version": "7" }), json!({ "note": "Groceries", "id": "7" })),
            ("similar_notes", json!({ "ref": "Groceries", "k": "5" }), json!({ "ref": "Groceries", "k": 5 })),
            ("create_note", json!({ "note": "Foo" }), json!({ "title": "Foo" })),
            ("append_to_note", json!({ "note": "Foo" }), json!({ "title": "Foo" })),
            (
                "edit_note",
                json!({ "note": "Foo", "target": "eggs", "content": "milk" }),
                json!({ "title": "Foo", "old_str": "eggs", "new_str": "milk" }),
            ),
            (
                "replace_section",
                json!({ "note": "Foo", "section": "Store", "content": "milk" }),
                json!({ "title": "Foo", "heading": "Store", "new_content": "milk" }),
            ),
            ("create_canvas", json!({ "note": "Board" }), json!({ "title": "Board" })),
            ("delete_note", json!({ "note": "Foo" }), json!({ "title": "Foo" })),
            ("move_note", json!({ "note": "Foo" }), json!({ "title": "Foo" })),
            (
                "rename_note",
                json!({ "note": "Foo", "name": "Bar" }),
                json!({ "old_title": "Foo", "new_title": "Bar" }),
            ),
            ("restore_note", json!({ "note": "Foo" }), json!({ "name": "Foo" })),
            ("restore_version", json!({ "version": "7" }), json!({ "id": "7" })),
        ];
        // One case per row, and one row per case: a row with no case is a row
        // nothing proved.
        assert_eq!(cases.len(), ROWS.len());
        for (intent, given, expected) in cases {
            let (function, got) = reconcile(intent, &slots(given.clone()));
            assert_eq!(&function, intent, "{intent}");
            assert_eq!(got, args(expected.clone()), "{intent}");
        }
    }

    /// The corpus's name is the function's name for every intent named here —
    /// `conventions` is the exception, and it is the next test.
    #[test]
    fn the_function_is_the_intents_own_name() {
        for row in ROWS {
            let (function, _) = reconcile(row.intent, &slots(json!({})));
            assert_eq!(function, row.intent);
        }
    }

    /// The one intent whose *name* is decided by its own arguments.
    #[test]
    fn conventions_resolves_to_the_canon_or_to_one_convention() {
        let (all, args_all) = reconcile("conventions", &slots(json!({})));
        assert_eq!(all, "get_conventions");
        assert!(args_all.is_empty());
        // An empty slot is not a slug — the model bound nothing, so this is the
        // canon and not a lookup for the empty convention.
        let (blank, args_blank) = reconcile("conventions", &slots(json!({ "slug": "  " })));
        assert_eq!(blank, "get_conventions");
        assert!(args_blank.is_empty());
        let (one, args_one) = reconcile("conventions", &slots(json!({ "slug": "capture" })));
        assert_eq!(one, "read_convention");
        assert_eq!(args_one, args(json!({ "name": "capture" })));
    }

    /// The type of an argument is the wire's, not a coercion's: Mneme's `id` is
    /// a string, so a stringly number must stay one — measured live,
    /// `{"id": 7}` comes back rejected. And this is the whole of the table's
    /// typing: an argument on an intent it does not name is left alone.
    #[test]
    fn only_the_arguments_the_wire_types_are_typed() {
        let (_, version) = reconcile("read_version", &slots(json!({ "version": "7" })));
        assert_eq!(version["id"], json!("7"), "id is a string on Mneme's side");
        let (_, restored) = reconcile("restore_version", &slots(json!({ "version": "7" })));
        assert_eq!(restored["id"], json!("7"), "restore_version's id is a string too");
        let (_, similar) = reconcile("similar_notes", &slots(json!({ "ref": "Groceries", "k": "5" })));
        assert_eq!(similar["k"], json!(5), "k is a uint32 on Mneme's side");
        // A `k` that is not a number is sent as it arrived rather than guessed.
        let (_, odd) = reconcile("similar_notes", &slots(json!({ "k": "many" })));
        assert_eq!(odd["k"], json!("many"));
        let (_, edit) = reconcile("edit_note", &slots(json!({ "replace_all": "true" })));
        assert_eq!(edit["replace_all"], json!(true));
        let (_, edit) = reconcile("edit_note", &slots(json!({ "replace_all": "false" })));
        assert_eq!(edit["replace_all"], json!(false));
        // Not a boolean either: left for the vault to judge.
        let (_, edit) = reconcile("edit_note", &slots(json!({ "replace_all": "yes please" })));
        assert_eq!(edit["replace_all"], json!("yes please"));
        // A number the table does not name — including one on an intent it does
        // not name at all — is not a number because it looks like one.
        let (_, search) = reconcile("search_vault", &slots(json!({ "query": "token savings", "k": "5" })));
        assert_eq!(search["k"], json!("5"));
        let (_, list) = reconcile("list_notes", &slots(json!({ "folder": "wiki", "depth": "2" })));
        assert_eq!(list["depth"], json!("2"));
    }

    /// An intent the table does not name goes as it arrived, and a list-valued
    /// slot is a list — the corpus's `refs` are JSON arrays, and the vault takes
    /// them as arrays.
    #[test]
    fn an_unnamed_intent_passes_through() {
        let (function, got) = reconcile(
            "semantic_centroid",
            &slots(json!({ "refs": ["Token Savings", "Project Envelope"] })),
        );
        assert_eq!(function, "semantic_centroid");
        assert_eq!(got["refs"], json!(["Token Savings", "Project Envelope"]));
        let (_, subtract) = reconcile(
            "semantic_subtract",
            &slots(json!({ "ref": "Groceries", "subtract_refs": ["Melete"] })),
        );
        assert_eq!(subtract["subtract_refs"], json!(["Melete"]));
    }

    /// `TRAIL` is a span of the utterance, not an argument — forwarded, it is
    /// an argument the vault does not know, on every intent there is.
    #[test]
    fn the_classifiers_trail_span_is_never_an_argument() {
        let (_, named) = reconcile("read_note", &slots(json!({ "note": "Groceries", "TRAIL": "and also" })));
        assert_eq!(named, args(json!({ "title": "Groceries" })));
        let (_, unnamed) = reconcile("search_vault", &slots(json!({ "query": "x", "TRAIL": "and also" })));
        assert_eq!(unnamed, args(json!({ "query": "x" })));
    }
}
