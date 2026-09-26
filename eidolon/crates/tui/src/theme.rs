//! The theme: every colour the Rust renderer paints on its own account,
//! as a table of named roles the script's `theme()` may override.
//!
//! The script has always been able to colour what it draws — a span
//! takes any `fg`, a panel any border. What it could not reach was the
//! colour of the things Rust draws *for* it: the search highlight, the
//! letter tags, the which-key popup, a dialog's frame, the arrows on tool
//! traffic, a heading in the model's markdown. Those were literals, some
//! thirty of them, spread over the renderer. They are now looked up here
//! by role, and the roles are what a theme names:
//!
//! ```rune
//! pub fn theme() {
//!     #{ hit: "yellow", tag: "magenta", menu: "yellow", dialog: "yellow",
//!        ok: "green", error: "red", user: "cyan", tool: "magenta",
//!        faint: "dim", text: "default", cursor: "yellow", focus: "reversed",
//!        code: "cyan", heading: "yellow", link: "blue" }
//! }
//! ```
//!
//! A theme need only say what it changes; the rest comes from the base
//! table underneath, which is the default script's. A colour is a name
//! the renderer knows — the eight, their `light` halves, `gray`,
//! `darkgray` and `white` — or `#rrggbb`, `default` (the terminal's foreground),
//! `dim` (that foreground, dimmed by the terminal) or, for a background,
//! `reversed` (the terminal's inversion). The modes' colours are *not* here:
//! they belong to the rows of `modes()`, beside the label and the sigil
//! they go with.

use std::collections::BTreeMap;

use anyhow::bail;
use ratatui::style::{Color, Style};
use serde_json::Value;

/// Every role, with what it paints. The order is the order a reference
/// lists them in.
pub const ROLES: &[(&str, &str)] = &[
    (
        "hit",
        "a search match, and the count written onto a fold that hides one",
    ),
    (
        "tag",
        "a letter tag, and the offer to open what hides a match",
    ),
    ("menu", "the which-key popup and command completion"),
    ("dialog", "an approval, question or picker"),
    ("ok", "the allow key on an approval"),
    (
        "error",
        "a failed tool, a search with no match, the deny key",
    ),
    ("user", "your own messages in the transcript"),
    (
        "peer",
        "a message from another session working on this project",
    ),
    ("tool", "the arrows and names on tool traffic"),
    (
        "faint",
        "everything said quietly: notes, captions, folded output",
    ),
    (
        "text",
        "secondary text that is not faint",
    ),
    (
        "sent",
        "what a call sent: the command, path or pattern on its line",
    ),
    ("gutter", "a code block's line numbers"),
    ("comment", "a comment inside a code block"),
    ("string", "a quoted string inside a code block"),
    ("number", "a number inside a code block"),
    ("keyword", "a language's reserved word inside a code block"),
    (
        "block",
        "behind a fenced block, as a background — a light theme wants its own",
    ),
    ("added", "behind a line a diff puts in, as a background"),
    ("removed", "behind a line a diff takes out, as a background"),
    ("cursor", "the streaming caret"),
    ("focus", "the block the trace cursor is on, as a background — or `reversed`"),
    ("code", "inline code and fenced blocks"),
    ("heading", "level one and two headings"),
    ("link", "link labels"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    roles: BTreeMap<String, String>,
}

impl Default for Theme {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Theme {
    /// What the renderer paints before a theme says otherwise. The two
    /// quiet roles are `dim` and `default` rather than colours: a named
    /// colour is a palette slot, and the slot for "bright black" sits
    /// on the ground of most dark palettes and off the ground of light
    /// ones. The terminal's own dimming is relative to its own ground.
    pub fn builtin() -> Self {
        let roles = [
            ("hit", "yellow"),
            ("tag", "magenta"),
            ("menu", "yellow"),
            ("dialog", "yellow"),
            ("ok", "green"),
            ("error", "red"),
            ("user", "green"),
            ("peer", "lightgreen"),
            ("tool", "magenta"),
            ("faint", "dim"),
            ("struck", "darkgray"),
            ("text", "default"),
            ("sent", "lightcyan"),
            ("gutter", "dim"),
            ("comment", "darkgray"),
            ("string", "green"),
            ("number", "magenta"),
            ("keyword", "blue"),
            ("block", "#262336"),
            ("added", "#1c3a28"),
            ("removed", "#3a1f26"),
            ("cursor", "yellow"),
            ("focus", "reversed"),
            ("code", "cyan"),
            ("heading", "yellow"),
            ("link", "blue"),
        ];
        Theme {
            roles: roles
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// Lay a script's `theme()` over `base`. Returns the roles it named
    /// that nothing paints, so a typo is a line in the transcript rather
    /// than a colour that never appears.
    pub fn from_value(v: &Value, base: &Theme) -> anyhow::Result<(Self, Vec<String>)> {
        let Some(obj) = v.as_object() else {
            bail!("theme() must return an object of role → colour")
        };
        let mut roles = base.roles.clone();
        let mut unknown = Vec::new();
        for (role, colour) in obj {
            let Some(c) = colour.as_str() else {
                bail!("theme role `{role}` must be a colour name or #rrggbb, not {colour}")
            };
            if !ROLES.iter().any(|(r, _)| r == role) {
                unknown.push(role.clone());
            }
            roles.insert(role.clone(), c.to_string());
        }
        Ok((Theme { roles }, unknown))
    }

    pub fn colour(&self, role: &str) -> Color {
        self.roles
            .get(role)
            .map_or(Color::Reset, |c| crate::render::color(c))
    }

    /// A foreground style in the role's colour; `dim` is the terminal's
    /// foreground with its own dimming, which no palette can misplace.
    pub fn fg(&self, role: &str) -> Style {
        match self.name(role) {
            "dim" => Style::default().add_modifier(ratatui::style::Modifier::DIM),
            _ => Style::default().fg(self.colour(role)),
        }
    }

    /// The role as a background; `reversed` is the terminal's own
    /// inversion, softened, which no palette can put on the wrong side
    /// of its text.
    pub fn bg(&self, role: &str) -> Style {
        match self.name(role) {
            "reversed" => Style::default().add_modifier(ratatui::style::Modifier::REVERSED | ratatui::style::Modifier::DIM),
            _ => Style::default().bg(self.colour(role)),
        }
    }

    /// Black text on the role's colour — a highlight.
    pub fn on(&self, role: &str) -> Style {
        Style::default().fg(Color::Black).bg(self.colour(role))
    }

    /// The colour's name, as the script would spell it.
    pub fn name(&self, role: &str) -> &str {
        self.roles.get(role).map_or("", String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_overrides_what_it_names_and_keeps_the_rest() {
        let (t, unknown) = Theme::from_value(
            &serde_json::json!({ "hit": "#ff0000", "tool": "blue" }),
            &Theme::builtin(),
        )
        .unwrap();
        assert!(unknown.is_empty());
        assert_eq!(t.colour("hit"), Color::Rgb(255, 0, 0));
        assert_eq!(t.colour("tool"), Color::Blue);
        assert!(
            t.fg("faint").add_modifier.contains(ratatui::style::Modifier::DIM),
            "untouched roles come from the base"
        );
    }

    #[test]
    fn a_role_nothing_paints_is_reported_not_swallowed() {
        let (_, unknown) =
            Theme::from_value(&serde_json::json!({ "hits": "red" }), &Theme::builtin()).unwrap();
        assert_eq!(unknown, ["hits"]);
        assert!(Theme::from_value(&serde_json::json!({ "hit": 3 }), &Theme::builtin()).is_err());
        assert!(Theme::from_value(&serde_json::json!("red"), &Theme::builtin()).is_err());
    }

    /// Two roles that meet on the same screen must not be the same
    /// colour, or the screen says less than it looks like it does: the
    /// operator's own words, a path, a link and a heading all sit in
    /// one paragraph of a reply.
    #[test]
    fn the_roles_that_share_a_paragraph_do_not_share_a_colour() {
        let t = Theme::builtin();
        let together = ["user", "peer", "code", "link", "heading", "error", "sent"];
        for (i, a) in together.iter().enumerate() {
            for b in &together[i + 1..] {
                assert_ne!(t.colour(a), t.colour(b), "{a} and {b} are the same colour");
            }
        }
    }

    #[test]
    fn every_documented_role_has_a_built_in_colour() {
        let t = Theme::builtin();
        for (role, _) in ROLES {
            let named = matches!(t.name(role), "dim" | "default" | "reversed");
            assert!(named || t.colour(role) != Color::Reset, "{role} has no default");
        }
    }
}
