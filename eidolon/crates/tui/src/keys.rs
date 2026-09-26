//! Key notation and the keymap resolver: the state a modal keymap needs
//! and the Rune script cannot hold.
//!
//! The script's `keymap()` is a *table* — pure data, read once at startup.
//! Everything stateful about pressing keys lives here, because a
//! [`rune::Vm`] is built fresh for every call and so cannot remember that
//! `g` was pressed a moment ago, or that a `3` is waiting to multiply the
//! next motion. So: Rune owns which key means what, Rust owns the
//! half-finished chord.
//!
//! ## Notation
//!
//! A binding key is the string [`name`] produces:
//!
//! ```text
//! a  A  1  /            a printable char (shift is already in the char)
//! space  esc  ret  tab  backspace  del
//! left right up down  home end  pageup pagedown
//! C-a  A-a  C-A-a      control / alt, in that order
//! S-tab  C-left        shift only where it is not already in the char
//! ```
//!
//! ## Table shape
//!
//! ```text
//! #{ normal: #{ … }, select: #{ … }, insert: #{ … } }
//! ```
//!
//! A value is either a command name (`"move_char_left"`), a described
//! command (`#{ cmd: "…", desc: "…" }`, with an optional `arg` for a
//! command that takes text — `#{ cmd: "enter_mode", arg: "review" }`),
//! or a submap — either bare
//! (`#{ h: "…", l: "…" }`) or described (`#{ desc: "goto", keys: #{ … } }`).
//! A submap makes its key a chord prefix: pressing it holds the keymap
//! open and the pending node is what [`Keys::menu`] hands the renderer for
//! the which-key popup.
//!
//! A name is one from [`crate::command`], which is also what `:` resolves
//! against — a binding and a typed command are the same vocabulary, and a
//! key can be bound to `model` or `compact` without a Rust change. This
//! module asks that table only one question: whether the command it just
//! resolved wants the *next* key as its argument.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

/// The spelling of a key press in a keymap table.
pub fn name(k: &KeyEvent) -> String {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    let base = match k.code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Enter => "ret".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "tab".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "del".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}").to_lowercase(),
    };
    // A shifted letter already arrives as the uppercase char, so `S-` is
    // only meaningful on the named keys.
    let named = !matches!(k.code, KeyCode::Char(_));
    let mut out = String::new();
    if ctrl {
        out.push_str("C-");
    }
    if alt {
        out.push_str("A-");
    }
    if shift && named && k.code != KeyCode::BackTab {
        out.push_str("S-");
    }
    if k.code == KeyCode::BackTab {
        out.push_str("S-");
    }
    out.push_str(&base);
    out
}

/// What a key press turned into.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolved {
    /// A chord is open; draw the menu and wait.
    Pending,
    /// Run this, `count` times. The count is **0 when none was typed** —
    /// that is what lets `G` mean "last line" while `3G` means line 3 —
    /// and otherwise the digits as typed, never zero. `arg` is the
    /// character(s) a char-taking command held the keyboard open for, or
    /// the `arg` the binding itself carried.
    Command {
        name: String,
        count: usize,
        arg: Option<String>,
    },
    /// Nothing claimed it, and the mode takes text: type this.
    Literal(String),
    /// Nothing claimed it.
    Unbound,
}

/// The live keymap plus the half-finished input in front of it.
pub struct Keys {
    map: Value,
    /// The chord typed so far, e.g. `["g"]` or `["space"]`.
    pending: Vec<String>,
    /// A count being typed, `0` when there is none.
    count: usize,
    /// A command holding the keyboard open for its char argument(s): the
    /// name, how many it wants, and the ones it has.
    awaiting: Option<(String, usize, String)>,
}

impl Keys {
    pub fn new(map: Value) -> Self {
        Keys {
            map,
            pending: Vec::new(),
            count: 0,
            awaiting: None,
        }
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.count = 0;
        self.awaiting = None;
    }

    /// Does the table have a mode of this name?
    pub fn has(&self, mode: &str) -> bool {
        self.map.get(mode).is_some_and(|m| keys_of(m).is_some())
    }

    /// The chord typed so far, for the status line (`""` when idle).
    pub fn pending_keys(&self) -> String {
        let mut s = String::new();
        if self.count > 0 {
            s.push_str(&self.count.to_string());
        }
        s.push_str(&self.pending.join(" "));
        if self.awaiting.is_some() {
            s.push('…');
        }
        s
    }

    /// The which-key popup: the open chord and what it can still become.
    /// `None` when nothing is pending.
    pub fn menu(&self, mode: &str) -> Option<crate::state::Menu> {
        if self.pending.is_empty() {
            return None;
        }
        let node = self.node(mode)?;
        let obj = keys_of(node)?;
        let mut items: Vec<(String, String)> =
            obj.iter().map(|(k, v)| (k.clone(), label(v))).collect();
        items.sort_by_key(|a| order(&a.0));
        // No `selected`: a chord is a grid of keys, not a list to walk.
        Some(crate::state::Menu {
            title: self.pending.join(" "),
            items,
            selected: None,
        })
    }

    /// Where the pending chord has walked to in the table.
    fn node(&self, mode: &str) -> Option<&Value> {
        let mut node = self.map.get(mode)?;
        for k in &self.pending {
            node = keys_of(node)?.get(k)?;
        }
        Some(node)
    }

    /// Feed a key press. `text_mode` is true in insert mode, where an
    /// unclaimed printable key is text rather than a mistake.
    pub fn press(&mut self, k: &KeyEvent, mode: &str, text_mode: bool) -> Resolved {
        let key = name(k);

        // A pending `f`/`r` owns the next key outright, so that `fd` finds
        // a `d` instead of deleting — and `mr(` holds it for two.
        if let Some((cmd, arity, mut got)) = self.awaiting.take() {
            match k.code {
                KeyCode::Char(c) => {
                    got.push(c);
                    if got.chars().count() >= arity {
                        let count = self.take_count();
                        self.pending.clear();
                        return Resolved::Command {
                            name: cmd,
                            count,
                            arg: Some(got),
                        };
                    }
                    self.awaiting = Some((cmd, arity, got));
                    Resolved::Pending
                }
                _ => {
                    self.pending.clear();
                    self.count = 0;
                    Resolved::Unbound
                }
            }
        } else {
            self.press_open(k, key, mode, text_mode)
        }
    }

    /// The resolver proper, once no char-taking command is mid-sip.
    fn press_open(&mut self, k: &KeyEvent, key: String, mode: &str, text_mode: bool) -> Resolved {
        // Counts, but only outside text mode and never as a leading zero —
        // `0` on its own is a motion.
        if !text_mode
            && self.pending.is_empty()
            && let KeyCode::Char(c) = k.code
            && c.is_ascii_digit()
            && !k.modifiers.contains(KeyModifiers::CONTROL)
            && (c != '0' || self.count > 0)
        {
            self.count = (self.count * 10 + c.to_digit(10).unwrap_or(0) as usize).min(9999);
            return Resolved::Pending;
        }

        let found = match self.node(mode) {
            Some(node) => keys_of(node).and_then(|o| o.get(&key)).cloned(),
            None => None,
        };

        match found {
            Some(v) if keys_of(&v).is_some() => {
                self.pending.push(key);
                Resolved::Pending
            }
            Some(v) => {
                let name = command_of(&v);
                self.pending.clear();
                let arity = crate::command::char_args(&name);
                if arity > 0 {
                    self.awaiting = Some((name, arity, String::new()));
                    return Resolved::Pending;
                }
                let count = self.take_count();
                Resolved::Command {
                    name,
                    count,
                    arg: arg_of(&v),
                }
            }
            None => {
                let had_chord = !self.pending.is_empty();
                self.pending.clear();
                self.count = 0;
                // A dead chord eats the key rather than typing it: `g` then
                // `z` should do nothing, not insert a `z`.
                if had_chord {
                    return Resolved::Unbound;
                }
                match k.code {
                    KeyCode::Char(c)
                        if text_mode
                            && !k.modifiers.contains(KeyModifiers::CONTROL)
                            && !k.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        Resolved::Literal(c.to_string())
                    }
                    _ => Resolved::Unbound,
                }
            }
        }
    }

    /// The count as typed — **0 when none was** — and the counter reset.
    /// Every consumer decides what no count means for it; the multipliers
    /// treat 0 as 1, the gotos treat it as "the other place".
    fn take_count(&mut self) -> usize {
        let n = self.count;
        self.count = 0;
        n
    }
}

/// Every command name a table binds, submaps walked. The registry is the
/// vocabulary, so a name it does not know is a typo in the script rather
/// than an extension point — [`unknown_commands`] is what says so out
/// loud, at startup, instead of on the keystroke that finds it.
pub fn command_names(map: &Value) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match keys_of(v) {
            Some(obj) => obj.values().for_each(|v| walk(v, out)),
            None => {
                let n = command_of(v);
                if !n.is_empty() {
                    out.push(n);
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Some(modes) = map.as_object() {
        modes.values().for_each(|m| walk(m, &mut out));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The names in `map` that neither [`crate::command`] nor the script's
/// own `extra` commands define.
pub fn unknown_commands(map: &Value, extra: &[String]) -> Vec<String> {
    command_names(map)
        .into_iter()
        .filter(|n| crate::command::lookup(n).is_none() && !extra.contains(n))
        .collect()
}

/// One mode's table flattened for a help page: `(keys, label)` per
/// binding, a chord spelled with spaces (`space m`), in menu order.
pub fn bindings(table: &Value) -> Vec<(String, String)> {
    fn walk(prefix: &str, node: &Value, out: &mut Vec<(String, String)>) {
        let Some(obj) = keys_of(node) else { return };
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort_by_key(|k| order(k));
        for k in keys {
            let v = &obj[k];
            let spelled = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix} {k}")
            };
            if keys_of(v).is_some() {
                walk(&spelled, v, out);
            } else {
                out.push((spelled, label(v)));
            }
        }
    }
    let mut out = Vec::new();
    walk("", table, &mut out);
    out
}

/// The submap behind a value, if it is one. A described submap carries its
/// bindings under `keys`; a bare one *is* its bindings, which is what lets
/// a table stay terse.
fn keys_of(v: &Value) -> Option<&serde_json::Map<String, Value>> {
    let o = v.as_object()?;
    if let Some(k) = o.get("keys") {
        return k.as_object();
    }
    if o.contains_key("cmd") {
        return None;
    }
    Some(o)
}

/// The argument a leaf carries, if it does.
fn arg_of(v: &Value) -> Option<String> {
    v.as_object()?.get("arg")?.as_str().map(str::to_string)
}

/// The command a leaf names.
fn command_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => o
            .get("cmd")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// What the which-key popup writes beside a key: the author's `desc` when
/// there is one, else the command name with its underscores opened up.
fn label(v: &Value) -> String {
    let described = v
        .as_object()
        .and_then(|o| o.get("desc"))
        .and_then(Value::as_str);
    if let Some(d) = described {
        return d.to_string();
    }
    if keys_of(v).is_some() {
        return "…".into();
    }
    command_of(v).replace('_', " ")
}

/// Menu order: lowercase before its uppercase, named keys last.
fn order(k: &str) -> (u8, String) {
    let mut cs = k.chars();
    match (cs.next(), cs.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => (
            0,
            format!("{}{}", c.to_ascii_lowercase(), u8::from(c.is_uppercase())),
        ),
        (Some(c), None) => (1, c.to_string()),
        _ => (2, k.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn escape() -> KeyEvent {
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    }

    fn map() -> Value {
        serde_json::json!({
            "normal": {
                "w": "move_next_word_start",
                "d": "delete_selection",
                "f": "find_next_char",
                "g": { "desc": "goto", "keys": { "h": "goto_line_start", "g": "goto_buffer_start" } },
                "space": { "m": { "cmd": "command:model", "desc": "model" } },
            },
            "insert": { "esc": "normal_mode" },
        })
    }

    #[test]
    fn a_chord_stays_pending_until_it_resolves() {
        let mut k = Keys::new(map());
        assert_eq!(k.press(&ev('g'), "normal", false), Resolved::Pending);
        assert!(k.menu("normal").is_some());
        assert_eq!(
            k.press(&ev('h'), "normal", false),
            Resolved::Command {
                name: "goto_line_start".into(),
                count: 0,
                arg: None
            }
        );
        assert!(k.menu("normal").is_none());
    }

    #[test]
    fn a_count_multiplies_the_next_command_and_then_clears() {
        let mut k = Keys::new(map());
        assert_eq!(k.press(&ev('3'), "normal", false), Resolved::Pending);
        assert_eq!(
            k.press(&ev('w'), "normal", false),
            Resolved::Command {
                name: "move_next_word_start".into(),
                count: 3,
                arg: None
            }
        );
        assert_eq!(
            k.press(&ev('w'), "normal", false),
            Resolved::Command {
                name: "move_next_word_start".into(),
                count: 0,
                arg: None
            }
        );
    }

    #[test]
    fn find_swallows_the_next_key_even_when_it_is_bound() {
        let mut k = Keys::new(map());
        assert_eq!(k.press(&ev('f'), "normal", false), Resolved::Pending);
        assert_eq!(
            k.press(&ev('d'), "normal", false),
            Resolved::Command {
                name: "find_next_char".into(),
                count: 0,
                arg: Some("d".into())
            }
        );
    }

    #[test]
    fn a_count_survives_the_first_char_of_a_pair() {
        let mut k = Keys::new(serde_json::json!({
            "normal": { "m": { "r": "surround_replace" } },
        }));
        assert_eq!(k.press(&ev('2'), "normal", false), Resolved::Pending);
        assert_eq!(k.press(&ev('m'), "normal", false), Resolved::Pending);
        assert_eq!(k.press(&ev('r'), "normal", false), Resolved::Pending);
        assert_eq!(
            k.press(&ev('*'), "normal", false),
            Resolved::Pending,
            "one char is not a pair yet"
        );
        assert_eq!(
            k.press(&ev('('), "normal", false),
            Resolved::Command {
                name: "surround_replace".into(),
                count: 2,
                arg: Some("*(".into())
            }
        );
        // And a non-char mid-pair cancels rather than half-running.
        let mut k = Keys::new(serde_json::json!({
            "normal": { "m": { "r": "surround_replace" } },
        }));
        assert_eq!(k.press(&ev('m'), "normal", false), Resolved::Pending);
        assert_eq!(k.press(&ev('r'), "normal", false), Resolved::Pending);
        assert_eq!(k.press(&escape(), "normal", false), Resolved::Unbound);
        assert_eq!(k.press(&ev('m'), "normal", false), Resolved::Pending);
    }

    #[test]
    fn a_binding_may_carry_its_own_argument() {
        let mut k = Keys::new(serde_json::json!({
            "normal": { "R": { "cmd": "enter_mode", "arg": "review", "desc": "review mode" } },
        }));
        assert_eq!(
            k.press(&ev('R'), "normal", false),
            Resolved::Command {
                name: "enter_mode".into(),
                count: 0,
                arg: Some("review".into())
            }
        );
    }

    #[test]
    fn insert_mode_types_what_no_binding_claims() {
        let mut k = Keys::new(map());
        assert_eq!(
            k.press(&ev('d'), "insert", true),
            Resolved::Literal("d".into())
        );
        assert_eq!(
            k.press(
                &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                "insert",
                true
            ),
            Resolved::Command {
                name: "normal_mode".into(),
                count: 0,
                arg: None
            }
        );
    }

    #[test]
    fn a_dead_chord_eats_the_key_rather_than_typing_it() {
        let mut k = Keys::new(map());
        k.press(&ev('g'), "normal", false);
        assert_eq!(k.press(&ev('z'), "normal", false), Resolved::Unbound);
        assert_eq!(k.pending_keys(), "");
    }

    #[test]
    fn a_table_flattens_to_bindings_with_chords_spelled_out() {
        let m = map();
        let b = bindings(&m["normal"]);
        assert!(
            b.contains(&("g h".to_string(), "goto line start".to_string())),
            "{b:?}"
        );
        assert!(
            b.contains(&("space m".to_string(), "model".to_string())),
            "{b:?}"
        );
        assert!(b.contains(&("w".to_string(), "move next word start".to_string())));
        assert_eq!(unknown_commands(&m, &[]), vec!["command:model".to_string()]);
        assert!(unknown_commands(&m, &["command:model".to_string()]).is_empty());
    }

    #[test]
    fn a_bare_submap_needs_no_keys_wrapper() {
        let mut k = Keys::new(map());
        assert_eq!(
            k.press(
                &KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                "normal",
                false
            ),
            Resolved::Pending
        );
        let m = k.menu("normal").unwrap();
        assert_eq!(m.title, "space");
        assert_eq!(m.items, vec![("m".to_string(), "model".to_string())]);
    }

    #[test]
    fn modifiers_spell_themselves_in_a_fixed_order() {
        assert_eq!(
            name(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            "C-a"
        );
        assert_eq!(
            name(&KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            "A"
        );
        assert_eq!(
            name(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            "space"
        );
        assert_eq!(
            name(&KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT)),
            "S-up"
        );
    }
}
