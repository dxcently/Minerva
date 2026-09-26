//! Dispatch mode: the prompt aimed at a **classifier** instead of at the
//! model.
//!
//! A dispatch is Verba Volantia's bargain (see `eidolon_verba`): free text
//! resolved *client-side* into one typed [`ToolCall`] in milliseconds, with
//! no model turn — and then handed to `Dispatcher::dispatch` like anything
//! else, so the policy hook, the journal, the bus and the transcript are
//! the same ones a model-originated call gets. Nothing about a dispatch is
//! privileged; the only thing it replaces is the LLM turn that would
//! otherwise have written the call.
//!
//! What this module adds is the **mode**. `eidolon do …` and `chat`'s
//! `:do` already had the mechanism; what they did not have was a place to
//! stand. Typing `:do read the note Groceries` costs four characters of
//! ceremony every time, and the whole point of the classifier is that it
//! answers in milliseconds — a surface that fast should be a mode you
//! enter, fire from repeatedly, and leave, not a command you re-type.
//!
//! ## Why a mode and not a picker
//!
//! Every other "the operator is choosing something" surface here is a
//! [`crate::state::Dialog`] with [`crate::state::PickAction`] — the model
//! picker, the persona picker, the palette. This is not one of those, because
//! what is being typed is a *sentence*, not a row: there is no list to
//! filter, and the arguments come out of the sentence as slots. What it
//! wants is the prompt buffer — wrapping, editing, history, `esc` — with
//! `ret` meaning something else. That is precisely a mode, so a mode is
//! what it is — a row named `dispatch` in the script's `modes()` table,
//! of kind insert: insert in every respect a buffer can see, differing
//! only in where a submitted line goes, which `on_submit` decides from
//! the name.
//!
//! ## Surfaces, and the submodes to come
//!
//! A `Palette` holds one bundle per tool *surface* — `mneme` (the vault's
//! functions), `tools` (the harness's own registry) — and dispatch mode is
//! aimed at exactly one of them at a time. Aimed, and not "the palette,
//! first bundle that accepts wins": the operator entering the mode has
//! already said which vocabulary they are about to speak, and trying the
//! others on top of that could only turn a mistyped vault command into a
//! shell one. So the driver resolves with `Palette::resolve_on`.
//!
//! Today the configured surface is `mneme` and there is nothing to choose
//! between. The aim is kept as an index anyway, because the shape of the
//! next step is already known: one submode per surface, entered from the
//! same key with a second letter, or picked from the list when there are
//! enough of them to be worth a picker. [`Dispatch::next`] is the cycle
//! that a `space d d` would bind to; until a second bundle exists it is a
//! no-op with a test on it rather than dead code with a comment.
//!
//! ## Abstention is not an error
//!
//! Melete's invariant, kept verbatim through `eidolon_verba`: a classifier
//! error, a timeout, a `none`/`UNK` intent or `accept: false` all mean
//! *not a command*, and the harness falls back to the model. In this mode
//! the fallback cannot be automatic — the operator aimed at a surface on
//! purpose, and silently sending a half-remembered vault command to a
//! model as a prompt would be a surprising way to spend a turn. So an
//! abstention says what it was close to and stops. The line itself is in
//! the dispatch line's own history, so `↑` has it back to edit and fire
//! again — a history of its own, and not the messages you have sent,
//! because an utterance is not one.

use eidolon_verba::Verdict;

/// Which dispatch surface the prompt is aimed at.
///
/// Sticky across mode changes: `esc` leaves dispatch mode but does not
/// forget where it was pointed, so re-entering comes back to the same
/// surface. The surface list is the driver's palette, in the order it
/// would try the bundles, handed over at startup — the UI thread cannot
/// ask the palette anything, since resolving is async and lives on the
/// other side of the channel.
#[derive(Clone, Debug, Default)]
pub struct Dispatch {
    surfaces: Vec<String>,
    at: usize,
}

impl Dispatch {
    pub fn new(surfaces: Vec<String>) -> Self {
        Dispatch { surfaces, at: 0 }
    }

    /// No bundle is configured, so the mode has nothing to resolve
    /// against and refuses to open.
    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    pub fn surfaces(&self) -> &[String] {
        &self.surfaces
    }

    /// The surface the prompt is aimed at, or `None` when none is
    /// configured.
    pub fn surface(&self) -> Option<&str> {
        self.surfaces.get(self.at).map(String::as_str)
    }

    /// Aim at a named surface. False — and no change — when there is no
    /// such bundle, which is what lets `:dispatch_mode nonsense` say so
    /// rather than open a mode pointed at nothing.
    pub fn aim(&mut self, name: &str) -> bool {
        match self.surfaces.iter().position(|s| s == name) {
            Some(i) => {
                self.at = i;
                true
            }
            None => false,
        }
    }

    /// The next surface round: the submode cycle, once there is more than
    /// one bundle to cycle between.
    pub fn next(&mut self) {
        if !self.surfaces.is_empty() {
            self.at = (self.at + 1) % self.surfaces.len();
        }
    }
}

/// What the transcript says when a bundle accepted: the call it made, and
/// the confidence behind it. The slots are the arguments, so they are
/// worth printing in full — a dispatch that read the wrong note should be
/// visible as such without unfolding the tool traffic.
pub fn resolved_line(bundle: &str, v: &Verdict) -> String {
    let slots: Vec<String> = v
        .slots
        .iter()
        .map(|(k, val)| format!("{k}={val}"))
        .collect();
    let args = if slots.is_empty() {
        String::new()
    } else {
        format!(" {}", slots.join(" "))
    };
    format!("{bundle} → {}{args} · p {:.2}", v.intent, v.intent_prob)
}

/// What it says when the bundle abstained. Abstention is the default and
/// not a failure, so this reports what was closest rather than an error,
/// and says how to get the line back.
pub fn abstained_line(bundle: &str, v: &Verdict) -> String {
    let close = match v.intent.as_str() {
        "none" | "UNK" => "nothing close".to_string(),
        intent => format!(
            "closest {intent}, p {:.2}, margin {:.2}",
            v.intent_prob, v.margin
        ),
    };
    format!("{bundle} abstained: {close} — esc i ↑ brings the line back to send to the model")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(intent: &str, accept: bool) -> Verdict {
        Verdict {
            utterance: "read the note Groceries".into(),
            delex: "read the note $a".into(),
            intent: intent.into(),
            intent_prob: 0.94,
            margin: 0.42,
            threshold: Some(0.3),
            accept: Some(accept),
            slots: [("note".to_string(), serde_json::Value::from("Groceries"))]
                .into_iter()
                .collect(),
            candidates: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn one_surface_is_the_one_it_rests_on() {
        let d = Dispatch::new(vec!["mneme".into()]);
        assert!(!d.is_empty());
        assert_eq!(d.surface(), Some("mneme"));
    }

    #[test]
    fn no_bundles_means_no_surface_to_aim_at() {
        let d = Dispatch::default();
        assert!(d.is_empty());
        assert_eq!(d.surface(), None);
    }

    #[test]
    fn aiming_takes_a_name_the_palette_knows_and_refuses_the_rest() {
        let mut d = Dispatch::new(vec!["mneme".into(), "tools".into()]);
        assert!(d.aim("tools"));
        assert_eq!(d.surface(), Some("tools"));
        assert!(!d.aim("nonsense"));
        assert_eq!(
            d.surface(),
            Some("tools"),
            "a refused aim leaves the surface alone"
        );
    }

    #[test]
    fn the_submode_cycle_wraps_and_is_harmless_with_one_surface() {
        let mut d = Dispatch::new(vec!["mneme".into(), "tools".into()]);
        d.next();
        assert_eq!(d.surface(), Some("tools"));
        d.next();
        assert_eq!(d.surface(), Some("mneme"));

        let mut one = Dispatch::new(vec!["mneme".into()]);
        one.next();
        assert_eq!(one.surface(), Some("mneme"));
        Dispatch::default().next(); // and does not panic on nothing
    }

    #[test]
    fn an_accepted_verdict_prints_its_call_and_its_slots() {
        let line = resolved_line("mneme", &verdict("read_note", true));
        assert_eq!(line, "mneme → read_note note=\"Groceries\" · p 0.94");
    }

    #[test]
    fn an_abstention_says_what_was_close_rather_than_erroring() {
        assert!(
            abstained_line("mneme", &verdict("read_note", false)).contains("closest read_note")
        );
        assert!(abstained_line("mneme", &verdict("none", false)).contains("nothing close"));
    }
}
