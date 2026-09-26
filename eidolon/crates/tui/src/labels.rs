//! What tools are **called** on the transcript, as against what they are
//! named on the wire. `wait_for` is a wire name; the operator reads
//! `Waiting`. The head of a tool call draws the label, everything else —
//! policy, dispatch, the log, matching — keeps the real name, so a label
//! can never change what a call *is*, only what it says.
//!
//! One table, the way [`crate::modes`] and [`crate::theme`] are: a
//! built-in base, a `labels()` row in `ui.rn` laid over it, validated at
//! load. A label is cosmetic by construction — the fold summary, the
//! search and the trace all read the wire name — so an unknown or missing
//! row is never an error; the wire name draws as it stands.

use serde_json::Value;
use std::collections::BTreeMap;

/// The tools whose wire name is not what a person reads. Everything else
/// draws under its own name, which is right for most calls: `read`,
/// `edit` and `bash` are already words.
pub fn builtin() -> Labels {
    Labels(
        [
            // The wait is not a call that returned; it is the session
            // parking itself, and the head says that rather than the
            // verb the wire happens to use.
            ("wait_for", "Waiting"),
            // The question tool asks the operator to pick, and the box
            // says what the box is, not which primitive filled it.
            ("choices_user", "Options"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect(),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Labels(BTreeMap<String, String>);

impl Labels {
    /// The user's rows laid over the base: a row may rename a tool or
    /// rename it to something else, and nothing else is allowed to
    /// happen — a label is a string and nothing more, so there is
    /// nothing to validate beyond that.
    pub fn from_value(v: &Value, base: &Labels) -> anyhow::Result<Labels> {
        let Some(rows) = v.as_object() else {
            anyhow::bail!("labels() must return an object of tool name → label");
        };
        let mut out = base.0.clone();
        for (name, row) in rows {
            let Some(label) = row.as_str().map(str::trim).filter(|l| !l.is_empty()) else {
                anyhow::bail!("label `{name}` must be a non-empty string");
            };
            out.insert(name.clone(), label.to_string());
        }
        Ok(Labels(out))
    }

    /// What the transcript calls `tool`: its label, or the wire name when
    /// nothing renamed it.
    pub fn label<'a>(&'a self, tool: &'a str) -> &'a str {
        self.0.get(tool).map(String::as_str).unwrap_or(tool)
    }
}

impl Default for Labels {
    fn default() -> Self {
        builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_user_table_renames_and_the_wire_name_falls_through() {
        let base = builtin();
        let t = Labels::from_value(&json!({ "wait_for": "Parked" }), &base).unwrap();
        assert_eq!(t.label("wait_for"), "Parked");
        assert_eq!(t.label("bash"), "bash", "an unlabelled tool draws as itself");
    }

    #[test]
    fn a_row_that_is_not_a_label_is_refused_not_guessed_at() {
        let base = builtin();
        assert!(Labels::from_value(&json!({ "wait_for": 3 }), &base).is_err());
        assert!(Labels::from_value(&json!({ "wait_for": "  " }), &base).is_err());
        assert!(Labels::from_value(&json!([]), &base).is_err());
    }

    #[test]
    fn the_builtin_table_names_the_two_tools_that_need_it() {
        let base = builtin();
        assert_eq!(base.label("wait_for"), "Waiting");
        assert_eq!(base.label("choices_user"), "Options");
    }
}

/// How a wait's checklist names the **kinds** of condition it can watch —
/// the short word in the tag column (`bg`, `peer`, `file`, `log`,
/// `vault`), each in a colour of its own. A hand that would rather have
/// a glyph than a word overrides the word; one that wants its own hue
/// overrides the colour, as a role name or `#rrggbb`. One table like the
/// labels': `wait_tags()` in `ui.rn` over these defaults, and a kind
/// nothing watches is a warning at load rather than a silent row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitTag {
    pub word: String,
    pub colour: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitTags(BTreeMap<String, WaitTag>);

/// The kinds core's conditions name today, each in the `tag` colour the
/// jump tags wear.
pub fn builtin_wait_tags() -> WaitTags {
    WaitTags(
        ["bg", "peer", "file", "log", "vault"]
            .into_iter()
            .map(|k| {
                (
                    k.to_string(),
                    WaitTag {
                        word: k.to_string(),
                        colour: "tag".into(),
                    },
                )
            })
            .collect(),
    )
}

impl WaitTags {
    /// The user's rows laid over the base: a row may be a bare word
    /// (keeping the base colour) or `{ word, colour }`. A kind core does
    /// not watch is reported back, not applied — it would draw nothing.
    pub fn from_value(v: &Value, base: &WaitTags) -> anyhow::Result<(WaitTags, Vec<String>)> {
        let Some(rows) = v.as_object() else {
            anyhow::bail!("wait_tags() must return an object of kind → word or {{ word, colour }}");
        };
        let mut out = base.0.clone();
        let mut unknown = Vec::new();
        for (kind, row) in rows {
            let (word, colour) = match row {
                Value::String(w) => (Some(w.clone()), None),
                Value::Object(o) => (
                    o.get("word").and_then(Value::as_str).map(String::from),
                    o.get("colour")
                        .or_else(|| o.get("color"))
                        .and_then(Value::as_str)
                        .map(String::from),
                ),
                _ => anyhow::bail!("tag `{kind}` must be a word or {{ word, colour }}"),
            };
            if !out.contains_key(kind) {
                unknown.push(kind.clone());
                continue;
            }
            let base_tag = &out[kind];
            out.insert(
                kind.clone(),
                WaitTag {
                    word: word.unwrap_or_else(|| base_tag.word.clone()),
                    colour: colour.unwrap_or_else(|| base_tag.colour.clone()),
                },
            );
        }
        Ok((WaitTags(out), unknown))
    }

    /// The tag for a kind; an unknown kind draws under the kind's own
    /// name, which is the honest fallback for a kind this table
    /// predates.
    pub fn tag(&self, kind: &str) -> WaitTag {
        self.0.get(kind).cloned().unwrap_or(WaitTag {
            word: kind.to_string(),
            colour: "tag".into(),
        })
    }
}

impl Default for WaitTags {
    fn default() -> Self {
        builtin_wait_tags()
    }
}

#[cfg(test)]
mod wait_tag_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_row_may_be_a_word_or_a_word_and_a_colour() {
        let base = builtin_wait_tags();
        let (t, unknown) = WaitTags::from_value(
            &json!({ "peer": { "word": "\u{23fe}", "colour": "#ff8800" }, "bg": "bgr" }),
            &base,
        )
        .unwrap();
        assert!(unknown.is_empty());
        assert_eq!(t.tag("peer").word, "\u{23fe}");
        assert_eq!(t.tag("peer").colour, "#ff8800");
        assert_eq!(t.tag("bg").word, "bgr");
        assert_eq!(t.tag("bg").colour, "tag", "a bare word keeps the base colour");
    }

    #[test]
    fn a_kind_nothing_watches_is_reported_not_applied() {
        let (t, unknown) =
            WaitTags::from_value(&json!({ "spinner": "spin" }), &builtin_wait_tags()).unwrap();
        assert_eq!(unknown, ["spinner"]);
        assert_eq!(t.tag("bg").word, "bg", "the base is untouched");
    }
}
