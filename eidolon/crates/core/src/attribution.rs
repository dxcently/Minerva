//! The label the harness stamps onto its own outbound vault calls.
//!
//! Mneme's `RPC` takes an optional `attribution`, recorded verbatim in the
//! vault's audit log beside the change it explains. It is the one field there
//! a *writer* chooses: `client` is the authenticated OAuth client — one name,
//! `eidolon`, shared by every session on this machine — and everything else is
//! what the server observed. A label the model fills is therefore a claim, not
//! a fact, and the model was in practice the only candidate to fill it: no tool
//! teaches the field and no harness code wrote one, which is why the vault's
//! log carried Melete's `chat-00009/meli` on its writes and nothing at all on
//! most of Eidolon's.
//!
//! So the harness stamps it instead — in `Mneme::rpc`, below every caller, so
//! that the model's `mneme_rpc` tool, a resolved vault-channel command and a
//! user script's `mneme_call` all carry one — and a value the model passed is
//! overwritten before the call leaves the process. The model does not get to
//! choose what the audit log says about it, which is the point of doing it
//! here rather than in a prompt.
//!
//! ## The label
//!
//! Named pairs, in a fixed order:
//!
//! ```text
//! session=<id> [persona=<name>] model=<key> cwd=<directory>
//! ```
//!
//! ```text
//! session=eidolon-756d model=zai:glm-5.3 cwd=~/eidolon
//! session=eidolon-756d persona=μέλι model=zai:glm-5.3 cwd=~/eidolon
//! session=eidolon-756d persona="Aoide #3" model=mock cwd="~/a b/eidolon"
//! ```
//!
//! Every field is named because **two of the four are usually absent** — most
//! sessions wear no persona, and a session whose model is not resolved yet
//! names none — and a bare leading token would mean one thing on one line and
//! something else on the next. With pairs, absence is a pair that is not
//! there, and no reader has to hold a positional grammar in its head to know
//! what it is looking at. A value is quoted only when it would otherwise end
//! early: whitespace, a double quote or a backslash. What a session id or a
//! model key can hold is written bare, which is most of what a reader sees.
//!
//! The whole thing is one string on the wire and in the log, because that is
//! what the field is: a human-readable line that a grep or a fifteen-line
//! parser can take apart if it needs to. Melete's own rows read
//! `chat-00009/meli` — a different writer's shape, recorded verbatim, and not
//! one this side tries to match.
//!
//! Session and cwd are fixed for a session's life. The persona and the model
//! are read per call, because both can move under a live session — `:persona`
//! repins and `:model` switches — and a label composed once at launch would go
//! on naming whoever the session was at the start.

use std::sync::{Arc, RwLock};

/// A session's identity, as its outbound vault calls are labelled with it.
///
/// Shared rather than copied: the vault client is built before the session
/// exists and is cloned into every consumer that can reach a vault, so the
/// label has to be one cell all of them read — see `Mneme::set_attribution`.
#[derive(Debug)]
pub struct Attribution {
    /// The session's roster id (`eidolon-756d`), or the log's stem on a
    /// session that never registered with a swarm: the peer id is what a
    /// neighbour can type and what `peers` shows, which is what makes a label
    /// in the audit log findable next to the session that wrote it.
    session: String,
    /// The directory the session's tools run in, in display form
    /// (`~/eidolon`) — the same abbreviation the swarm roster shows, and for
    /// the same reason: "who" is much less useful than "who, and in which
    /// tree".
    cwd: String,
    /// The persona the conversation is wearing, as the *note* names it (the
    /// frontmatter `name`, not the pin, which may be a path). `None` is the
    /// ordinary case — most sessions wear nobody — and empty when a pin
    /// resolved to nothing, which is not a persona called "".
    persona: RwLock<Option<String>>,
    /// The resolved catalog key the session journals as its model
    /// (`zai:glm-5.3`), not the alias a launch was given. `None` before a
    /// model is known.
    model: RwLock<Option<String>>,
}

impl Attribution {
    /// Label a fresh session. `cwd` is the display form of the directory its
    /// tools run in; the caller owns the abbreviation because the roster's
    /// rule for it lives with the roster.
    pub fn new(session: impl Into<String>, cwd: impl Into<String>) -> Arc<Self> {
        Arc::new(Attribution {
            session: session.into(),
            cwd: cwd.into(),
            persona: RwLock::new(None),
            model: RwLock::new(None),
        })
    }

    /// Say which persona the conversation now wears, or `None` when nothing
    /// resolved — a session that unpins stops claiming the character.
    pub fn set_persona(&self, name: Option<String>) {
        *self.persona.write().unwrap() =
            name.filter(|n| !n.trim().is_empty()).map(|n| n.trim().to_string());
    }

    /// Say which model the session now runs, by catalog key.
    pub fn set_model(&self, key: impl Into<String>) {
        let key = key.into();
        let key = key.trim();
        *self.model.write().unwrap() = (!key.is_empty()).then(|| key.to_string());
    }

    /// The label one call is attributed with.
    pub fn label(&self) -> String {
        let mut out = format!("session={}", value(&self.session));
        if let Some(persona) = self.persona.read().unwrap().as_deref() {
            out.push_str(" persona=");
            out.push_str(&value(persona));
        }
        if let Some(model) = self.model.read().unwrap().as_deref() {
            out.push_str(" model=");
            out.push_str(&value(model));
        }
        out.push_str(" cwd=");
        out.push_str(&value(&self.cwd));
        out
    }
}

/// One value, quoted when it would otherwise be read as more than itself.
///
/// A space ends a value and a quote ends the quoting, so those — and a
/// backslash, which is what the escapes are made of — are what force the
/// quotes. Inside them `\"`, `\\`, `\n`, `\t` and `\r` are the escapes, which
/// is enough for the one field a person writes freely (a persona's name) and
/// keeps a label to a single line whatever is in the vault.
fn value(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\\');
    if !needs_quotes {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_is_named_so_absence_needs_no_grammar() {
        let a = Attribution::new("eidolon-756d", "~/eidolon");
        // No persona and no model yet: both pairs are simply not there, and
        // nothing else in the line changes meaning because of it.
        assert_eq!(a.label(), "session=eidolon-756d cwd=~/eidolon");
        a.set_model("zai:glm-5.3");
        assert_eq!(
            a.label(),
            "session=eidolon-756d model=zai:glm-5.3 cwd=~/eidolon"
        );
        a.set_persona(Some("μέλι".into()));
        assert_eq!(
            a.label(),
            "session=eidolon-756d persona=μέλι model=zai:glm-5.3 cwd=~/eidolon"
        );
    }

    #[test]
    fn a_persona_can_be_taken_off_again_and_an_empty_pin_is_not_a_name() {
        let a = Attribution::new("eidolon-756d", "~/eidolon");
        a.set_persona(Some("μέλι".into()));
        assert!(a.label().contains("persona=μέλι"));
        a.set_persona(None);
        assert_eq!(a.label(), "session=eidolon-756d cwd=~/eidolon");
        a.set_persona(Some("  ".into()));
        assert_eq!(a.label(), "session=eidolon-756d cwd=~/eidolon");
    }

    #[test]
    fn a_model_is_dropped_when_unknown_and_replaced_when_it_moves() {
        let a = Attribution::new("1789494170362", "/tmp/scratch");
        assert_eq!(a.label(), "session=1789494170362 cwd=/tmp/scratch");
        a.set_model("claude-sonnet-4-5");
        assert_eq!(
            a.label(),
            "session=1789494170362 model=claude-sonnet-4-5 cwd=/tmp/scratch"
        );
        a.set_model("zai:glm-5.3");
        assert_eq!(
            a.label(),
            "session=1789494170362 model=zai:glm-5.3 cwd=/tmp/scratch"
        );
    }

    #[test]
    fn a_value_that_would_end_early_is_quoted() {
        let a = Attribution::new("eidolon-756d", "~/Application Support/eidolon");
        a.set_persona(Some("Aoide #3".into()));
        assert_eq!(
            a.label(),
            "session=eidolon-756d persona=\"Aoide #3\" cwd=\"~/Application Support/eidolon\""
        );
    }

    #[test]
    fn quotes_and_backslashes_inside_a_value_are_escaped() {
        assert_eq!(value(r#"a"b"#), r#""a\"b""#);
        assert_eq!(value(r"a\b"), r#""a\\b""#);
        // A label is one line whatever the vault holds.
        assert_eq!(value("two\nlines"), r#""two\nlines""#);
        assert_eq!(value(""), r#""""#);
        // The ordinary case is written bare.
        assert_eq!(value("eidolon-756d"), "eidolon-756d");
        assert_eq!(value("~/eidolon"), "~/eidolon");
    }
}
