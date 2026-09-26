//! The mode table: what the script's `modes()` declares, and the one
//! place the harness asks what a mode is called, what colour it wears
//! and what character it puts at the head of the line.
//!
//! A mode used to be a Rust enum with its name, colour and sigil written
//! into `render::mode_style` — so a user who wanted dispatch mode in
//! orange, or a fifth mode of their own, was editing Rust. Now a mode is
//! a **row in a table the script owns**, and the buffer keeps only what
//! it genuinely needs to edit: the [`Kind`], which says whether keys are
//! text, whether motions extend, and whether the cursor is a caret or a
//! block. Everything else about a mode is presentation, and presentation
//! is the script's.
//!
//! ## The table
//!
//! ```rune
//! pub fn modes() {
//!     #{
//!         "normal":   mode("normal", "NOR", "blue"),
//!         "insert":   mode("insert", "INS", "green"),
//!         "select":   mode("select", "SEL", "magenta"),
//!         "dispatch": sigil(mode("insert", "DISP", "red"), "!"),
//!         "command":  style("CMD", "cyan"),
//!         "search":   style("FIND", "yellow"),
//!     }
//! }
//! ```
//!
//! A row with a `kind` is an **editing mode**: the prompt can be in it,
//! `enter_mode NAME` reaches it, and the keymap may have a table for it.
//! A row without one is **style only** — the command line and the search
//! line are modes to the hands without being modes on the buffer, and
//! they take their colour and badge from here like the others.
//!
//! ## What stays fixed
//!
//! Three names are canonical: `normal`, `insert` and `select`. Helix's
//! own verbs reach them by name — `i` and `c` and `o` go to insert, `v`
//! to select, `esc` to normal — so a script may restyle them freely but
//! may not change their kind, and a table that omits one gets it from
//! the base table underneath. Everything else, dispatch included, is an
//! ordinary row that happens to ship in `ui/default.rn`.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail};
use serde_json::Value;

use crate::edit::{Kind, Mode};

/// One row of the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeDef {
    /// The id: what the keymap, the snapshot and `enter_mode` call it.
    pub name: String,
    /// How the buffer behaves here; `None` for a style-only row.
    pub kind: Option<Kind>,
    /// The badge — short, for a status line.
    pub label: String,
    /// The frame's colour, in the renderer's colour vocabulary.
    pub colour: String,
    /// A character drawn at the head of the line while the mode is on,
    /// never stored in the buffer. Dispatch's `!`.
    pub sigil: Option<char>,
}

impl ModeDef {
    /// The buffer mode this row names, if it is an editing mode.
    pub fn mode(&self) -> Option<Mode> {
        self.kind.map(|k| Mode::named(&self.name, k))
    }
}

/// The three rows every table has, in the shape the buffer's own verbs
/// assume. The fallback of last resort — the default script carries the
/// real styling, and a user's table is laid over that.
fn canonical() -> Vec<ModeDef> {
    let row = |name: &str, kind: Kind, label: &str, colour: &str| ModeDef {
        name: name.into(),
        kind: Some(kind),
        label: label.into(),
        colour: colour.into(),
        sigil: None,
    };
    vec![
        row("normal", Kind::Normal, "NOR", "blue"),
        row("insert", Kind::Insert, "INS", "green"),
        row("select", Kind::Select, "SEL", "magenta"),
    ]
}

#[derive(Clone, Debug)]
pub struct ModeTable {
    rows: BTreeMap<String, ModeDef>,
}

impl Default for ModeTable {
    fn default() -> Self {
        Self::builtin()
    }
}

impl ModeTable {
    /// The canonical three and nothing else. What the harness runs on if
    /// even the built-in script cannot be read.
    pub fn builtin() -> Self {
        ModeTable {
            rows: canonical()
                .into_iter()
                .map(|d| (d.name.clone(), d))
                .collect(),
        }
    }

    /// Lay a script's `modes()` over `base`. A row in `v` replaces the
    /// row of the same name; a row missing from `v` survives from `base`,
    /// so a user's table need only say what it changes.
    ///
    /// Fails on a row that cannot mean anything — an unknown kind, a
    /// sigil longer than one character, a canonical mode given the wrong
    /// kind — because a mode that is half-defined is worse than the
    /// table it would have replaced.
    pub fn from_value(v: &Value, base: &ModeTable) -> anyhow::Result<Self> {
        let Some(obj) = v.as_object() else {
            bail!("modes() must return an object of mode → row")
        };
        let mut rows = base.rows.clone();
        for (name, row) in obj {
            let def = parse_row(name, row)?;
            if let Some(c) = canonical().iter().find(|c| c.name == *name)
                && def.kind != c.kind
            {
                bail!(
                    "mode `{name}` must keep kind `{}`: the prompt's own verbs reach it by name",
                    c.kind.map_or("?", Kind::as_str)
                );
            }
            rows.insert(name.clone(), def);
        }
        Ok(ModeTable { rows })
    }

    pub fn get(&self, name: &str) -> Option<&ModeDef> {
        self.rows.get(name)
    }

    /// How a mode looks, by name, for the frame and the badge. A name the
    /// table does not know — the prompt is in a mode a reloaded script no
    /// longer defines — gets a plain row rather than a panic, so the
    /// screen keeps saying *something* until the next mode change.
    pub fn style(&self, name: &str) -> ModeDef {
        self.rows.get(name).cloned().unwrap_or_else(|| ModeDef {
            name: name.to_string(),
            kind: None,
            label: name.to_uppercase().chars().take(4).collect(),
            colour: "white".into(),
            sigil: None,
        })
    }

    /// The buffer mode a name stands for, or why it cannot be entered.
    pub fn mode(&self, name: &str) -> anyhow::Result<Mode> {
        match self.rows.get(name) {
            Some(d) => d.mode().ok_or_else(|| {
                anyhow!("`{name}` is a style-only mode; the prompt cannot be put in it")
            }),
            None => Err(anyhow!(
                "no mode `{name}`; modes() defines {}",
                self.editing_names().join(", ")
            )),
        }
    }

    /// The editing modes, by name, in table order.
    pub fn editing_names(&self) -> Vec<String> {
        self.rows
            .values()
            .filter(|d| d.kind.is_some())
            .map(|d| d.name.clone())
            .collect()
    }

    pub fn rows(&self) -> impl Iterator<Item = &ModeDef> {
        self.rows.values()
    }
}

fn parse_row(name: &str, row: &Value) -> anyhow::Result<ModeDef> {
    let Some(o) = row.as_object() else {
        bail!("mode `{name}` must be an object")
    };
    let kind = match o.get("kind") {
        None | Some(Value::Null) => None,
        Some(Value::String(k)) => Some(Kind::parse(k).ok_or_else(|| {
            anyhow!("mode `{name}`: unknown kind `{k}` (normal, insert or select)")
        })?),
        Some(other) => bail!("mode `{name}`: kind must be a string, not {other}"),
    };
    let label = match o.get("label").and_then(Value::as_str) {
        Some(l) if !l.is_empty() => l.to_string(),
        _ => name.to_uppercase().chars().take(4).collect(),
    };
    let colour = o
        .get("colour")
        .or_else(|| o.get("color"))
        .and_then(Value::as_str)
        .unwrap_or("white")
        .to_string();
    let sigil = match o.get("sigil").and_then(Value::as_str).unwrap_or("") {
        "" => None,
        s if s.chars().count() == 1 => s.chars().next(),
        s => bail!("mode `{name}`: sigil `{s}` must be a single character"),
    };
    Ok(ModeDef {
        name: name.to_string(),
        kind,
        label,
        colour,
        sigil,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(v: serde_json::Value) -> anyhow::Result<ModeTable> {
        ModeTable::from_value(&v, &ModeTable::builtin())
    }

    #[test]
    fn a_user_table_is_laid_over_the_base_and_need_only_say_what_changes() {
        let t = table(serde_json::json!({ "dispatch": { "kind": "insert", "label": "DISP", "colour": "red", "sigil": "!" } })).unwrap();
        assert_eq!(t.style("dispatch").sigil, Some('!'));
        assert_eq!(
            t.mode("dispatch").unwrap(),
            Mode::named("dispatch", Kind::Insert)
        );
        // The canonical three came through from the base untouched.
        assert_eq!(t.style("normal").colour, "blue");
        assert_eq!(
            t.editing_names(),
            ["dispatch", "insert", "normal", "select"]
        );
    }

    #[test]
    fn a_canonical_mode_may_be_restyled_but_not_rekinded() {
        let t = table(serde_json::json!({ "insert": { "kind": "insert", "colour": "#ff8800", "label": "TYPE" } })).unwrap();
        assert_eq!(t.style("insert").colour, "#ff8800");
        assert_eq!(t.style("insert").label, "TYPE");
        let err = table(serde_json::json!({ "insert": { "kind": "normal" } })).unwrap_err();
        assert!(err.to_string().contains("must keep kind"), "{err}");
    }

    #[test]
    fn a_style_only_row_cannot_be_entered_but_still_has_a_look() {
        let t = table(serde_json::json!({ "search": { "label": "FIND", "colour": "yellow" } }))
            .unwrap();
        assert_eq!(t.style("search").colour, "yellow");
        let err = t.mode("search").unwrap_err().to_string();
        assert!(err.contains("style-only"), "{err}");
        let err = t.mode("nope").unwrap_err().to_string();
        assert!(err.contains("no mode `nope`"), "{err}");
    }

    #[test]
    fn a_half_defined_row_is_refused_rather_than_guessed_at() {
        assert!(table(serde_json::json!({ "x": { "kind": "weird" } })).is_err());
        assert!(table(serde_json::json!({ "x": { "kind": "insert", "sigil": "!!" } })).is_err());
        assert!(table(serde_json::json!({ "x": "insert" })).is_err());
        assert!(table(serde_json::json!([])).is_err());
    }

    #[test]
    fn an_unknown_name_still_draws_as_something() {
        let t = ModeTable::builtin();
        let s = t.style("review");
        assert_eq!(s.name, "review");
        assert_eq!(s.label, "REVI");
        assert!(s.kind.is_none());
    }
}
