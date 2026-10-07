//! The rules' storage (slice A1b), as the operator ruled it (D10 a): **memory only**, each rule bound to the door that was alive when it
//! was made. There is no file, no disk and no clock here; a rule is in this map, and the map is the perch's.
//!
//! ```text
//!   hello    -> bind    a door's entry: its cwd and its own word on yolo, no rules
//!   watching -> watch   the door's AskView, which is how the routes and the matcher read its asks
//!   closed   -> end     the entry and every rule in it, whatever ended the door
//! ```
//!
//! A rule belongs to a [`DoorKey`] (the process and its port), not to a chat id: anything that ends the door ends its rules. A stop
//! closes the watcher, a model or mode switch is a stop and a start (a new pid and port), a crash ends the watcher, and a perch restart
//! starts with this map empty and has nothing to read a rule back from. So the rules cannot outlive the door they were agreed to for,
//! and nothing here can keep them alive. At most [`RULES_MAX`] per door, and so per chat.
//!
//! This module imports no file API, and a test holds that (`mod.rs`, `the_rules_are_never_written_anywhere`).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::matcher::{Pattern, Shape};
use crate::mode::Mode;
use crate::watch::{AskView, DoorKey};

/// At most this many rules per chat. A page that could make an unbounded number would be a page that could make the list unreadable.
pub const RULES_MAX: usize = 32;

/// How many ask ids a door remembers having handed to an answerer, the smallest forgotten first (ids only go up, and the watcher itself
/// holds 32 asks at a time, so this is room for a long chat).
const CLAIMED_MAX: usize = 1024;

/// One rule: what it allows, for which door, and what it has done.
#[derive(Clone, Debug)]
pub struct Rule {
    /// The perch's own counter, never reused within a run.
    pub rid: u64,
    /// The door it was made under. A rule answers asks from this key and no other.
    pub door: DoorKey,
    /// The mode the chat was in when the rule was made; the rule answers only while the chat is still in it.
    pub mode: Mode,
    /// [`AskView::seq`] when the rule was made: an ask matches only if the watcher first saw it after this.
    pub stamp: u64,
    /// [`AskView::max_ask_id`] when the rule was made: an ask matches only if its id is above this.
    pub max_ask: u64,
    pub pattern: Pattern,
    pub added_ms: u64,
    /// How many asks this rule has answered (confirmed by the door), and when it last did.
    pub uses: u64,
    pub last_ms: Option<u64>,
}

impl Rule {
    /// The rule as the page reads it: `{rid, tool, origin, command?, cwd?, added_ms, uses, last_ms?}`. Nothing else of the rule is
    /// shown (the stamps are the matcher's), and `command` and `cwd` are present only on a command rule.
    pub fn json(&self) -> Value {
        let mut rule = json!({
            "rid": self.rid,
            "tool": self.pattern.tool,
            "origin": self.pattern.origin.as_str(),
            "added_ms": self.added_ms,
            "uses": self.uses,
        });
        if let Shape::Command { command, cwd } = &self.pattern.shape {
            rule["command"] = json!(command);
            rule["cwd"] = json!(cwd.to_string_lossy());
        }
        if let Some(last) = self.last_ms {
            rule["last_ms"] = json!(last);
        }
        rule
    }
}

/// What the store knows of one door.
pub struct Door {
    cwd: PathBuf,
    /// The door's own word in `hello`: `--yolo`.
    yolo: bool,
    view: Option<AskView>,
    rules: Vec<Rule>,
    /// Asks already handed to an answerer, so that an ask told twice (or made a rule from while a rule answers it) is answered once.
    claimed: BTreeSet<u64>,
}

impl Door {
    /// Where the door's bash calls run, as `hello` said it.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn yolo(&self) -> bool {
        self.yolo
    }

    /// The read handle on the door's asks; `None` between `hello` and `watching`, which are told back to back.
    pub fn view(&self) -> Option<&AskView> {
        self.view.as_ref()
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Takes the right to answer `ask_id`: `true` the first time, `false` for as long as it is remembered.
    pub fn claim(&mut self, ask_id: u64) -> bool {
        let first = self.claimed.insert(ask_id);
        while self.claimed.len() > CLAIMED_MAX {
            self.claimed.pop_first();
        }
        first
    }

    /// Gives the right back, for an answer that did not reach the door and left the ask open.
    pub fn release(&mut self, ask_id: u64) {
        self.claimed.remove(&ask_id);
    }
}

/// What [`Store::add`] needs to make a rule.
pub struct New {
    pub mode: Mode,
    pub stamp: u64,
    pub max_ask: u64,
    pub pattern: Pattern,
    pub now_ms: u64,
}

/// A rule that was made, or the one that already said the same.
#[derive(Debug)]
pub enum Added {
    Made(Rule),
    Same(Rule),
}

impl Added {
    pub fn rule(&self) -> &Rule {
        match self {
            Added::Made(rule) | Added::Same(rule) => rule,
        }
    }

    pub fn made(&self) -> bool {
        matches!(self, Added::Made(_))
    }
}

/// Why no rule was added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The door is not (or is no longer) in the store: its watcher has ended.
    NoDoor,
    /// [`RULES_MAX`] already.
    Full,
    /// Another answer for this ask is already on its way.
    Taken,
}

#[derive(Default)]
pub struct Store {
    doors: HashMap<DoorKey, Door>,
    next: u64,
}

impl Store {
    /// A door said `hello`. A key that was already here (a pid the kernel handed out again, whose `closed` was never told) starts over:
    /// its rules were another door's.
    pub fn bind(&mut self, key: DoorKey, cwd: PathBuf, yolo: bool) {
        self.doors.insert(key, Door { cwd, yolo, view: None, rules: Vec::new(), claimed: BTreeSet::new() });
    }

    pub fn watch(&mut self, key: DoorKey, view: AskView) {
        if let Some(door) = self.doors.get_mut(&key) {
            door.view = Some(view);
        }
    }

    /// The door's watcher ended: the entry goes, and its rules with it. How many rules that was.
    pub fn end(&mut self, key: DoorKey) -> usize {
        self.doors.remove(&key).map_or(0, |door| door.rules.len())
    }

    pub fn door(&self, key: DoorKey) -> Option<&Door> {
        self.doors.get(&key)
    }

    pub fn door_mut(&mut self, key: DoorKey) -> Option<&mut Door> {
        self.doors.get_mut(&key)
    }

    /// How many doors the store holds.
    pub fn doors(&self) -> usize {
        self.doors.len()
    }

    /// Makes a rule for the door, and takes the right to answer `ask_id` with it, in one step. A rule that says what one already says
    /// is not a second rule ([`Added::Same`]): the list stays short and the cap stays for different rules. The cap is checked before the
    /// ask is claimed, so an ask a full door refuses stays a person's to answer.
    pub fn add(&mut self, key: DoorKey, new: New, ask_id: u64) -> Result<Added, Refused> {
        let door = self.doors.get_mut(&key).ok_or(Refused::NoDoor)?;
        let same = door.rules.iter().find(|rule| rule.pattern == new.pattern && rule.mode == new.mode).cloned();
        if same.is_none() && door.rules.len() >= RULES_MAX {
            return Err(Refused::Full);
        }
        if !door.claim(ask_id) {
            return Err(Refused::Taken);
        }
        if let Some(rule) = same {
            return Ok(Added::Same(rule));
        }
        self.next += 1;
        let rule = Rule {
            rid: self.next,
            door: key,
            mode: new.mode,
            stamp: new.stamp,
            max_ask: new.max_ask,
            pattern: new.pattern,
            added_ms: new.now_ms,
            uses: 0,
            last_ms: None,
        };
        door.rules.push(rule.clone());
        Ok(Added::Made(rule))
    }

    /// Removes one rule. `false` when this door has no such rule.
    pub fn revoke(&mut self, key: DoorKey, rid: u64) -> bool {
        let Some(door) = self.doors.get_mut(&key) else { return false };
        let before = door.rules.len();
        door.rules.retain(|rule| rule.rid != rid);
        door.rules.len() != before
    }

    /// The rule answered an ask, and the door took the answer.
    pub fn used(&mut self, key: DoorKey, rid: u64, now_ms: u64) {
        if let Some(rule) = self.doors.get_mut(&key).and_then(|door| door.rules.iter_mut().find(|rule| rule.rid == rid)) {
            rule.uses += 1;
            rule.last_ms = Some(now_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::Origin;

    const A: DoorKey = DoorKey { pid: 100, port: 40001 };
    const B: DoorKey = DoorKey { pid: 200, port: 40002 };

    fn pattern(command: &str) -> Pattern {
        Pattern { tool: "bash".into(), origin: Origin::Model, shape: Shape::Command { command: command.into(), cwd: "/w".into() } }
    }

    fn new(command: &str) -> New {
        New { mode: Mode::Auto, stamp: 5, max_ask: 5, pattern: pattern(command), now_ms: 1000 }
    }

    fn store() -> Store {
        let mut store = Store::default();
        store.bind(A, "/w".into(), false);
        store.bind(B, "/w".into(), false);
        store
    }

    #[test]
    fn rules_get_rids_that_are_never_reused() {
        let mut store = store();
        let first = store.add(A, new("one"), 1).unwrap();
        let second = store.add(B, new("two"), 1).unwrap();
        assert_eq!((first.rule().rid, second.rule().rid), (1, 2), "one counter for the whole perch");
        assert!(store.revoke(A, 1));
        let third = store.add(A, new("three"), 2).unwrap();
        assert_eq!(third.rule().rid, 3, "a revoked rid is not handed out again");
    }

    #[test]
    fn a_door_holds_at_most_32_rules_and_a_revoke_makes_room() {
        let mut store = store();
        for n in 0..RULES_MAX {
            assert!(store.add(A, new(&format!("cmd {n}")), n as u64 + 1).unwrap().made());
        }
        assert_eq!(store.door(A).unwrap().rules().len(), 32);
        assert_eq!(store.add(A, new("one too many"), 99).unwrap_err(), Refused::Full);
        assert!(store.door_mut(A).unwrap().claim(99), "a refused ask is not claimed: it stays a person's to answer");
        // Another door has its own 32.
        assert!(store.add(B, new("elsewhere"), 1).unwrap().made());
        // A rule that says what one already says is the same rule, even on a full door.
        assert!(!store.add(A, new("cmd 0"), 100).unwrap().made());
        assert_eq!(store.door(A).unwrap().rules().len(), 32);
        let rid = store.door(A).unwrap().rules()[3].rid;
        assert!(store.revoke(A, rid));
        assert!(store.add(A, new("fits now"), 101).unwrap().made());
    }

    #[test]
    fn the_same_rule_twice_is_one_rule_and_the_second_ask_is_still_claimed() {
        let mut store = store();
        let made = store.add(A, new("ls"), 1).unwrap();
        let same = store.add(A, new("ls"), 2).unwrap();
        assert!(made.made() && !same.made());
        assert_eq!(made.rule().rid, same.rule().rid);
        assert_eq!(store.door(A).unwrap().rules().len(), 1);
        // Another mode is another rule.
        let mut manual = new("ls");
        manual.mode = Mode::Manual;
        assert!(store.add(A, manual, 3).unwrap().made());
    }

    #[test]
    fn an_ask_is_claimed_once() {
        let mut store = store();
        store.add(A, new("ls"), 7).unwrap();
        assert_eq!(store.add(A, new("other"), 7).unwrap_err(), Refused::Taken, "an answer for ask 7 is already on its way");
        assert_eq!(store.door(A).unwrap().rules().len(), 1, "and no rule was made for it");
        let door = store.door_mut(A).unwrap();
        assert!(!door.claim(7));
        door.release(7);
        assert!(door.claim(7), "a failed answer gives the ask back");
        // The memory is bounded, smallest id first.
        for id in 100..100 + CLAIMED_MAX as u64 + 5 {
            door.claim(id);
        }
        assert!(door.claim(7), "an old id has been forgotten");
        assert!(!door.claim(100 + CLAIMED_MAX as u64), "a recent one has not");
    }

    #[test]
    fn a_door_that_ended_takes_its_rules_with_it() {
        let mut store = store();
        store.add(A, new("one"), 1).unwrap();
        store.add(A, new("two"), 2).unwrap();
        store.add(B, new("three"), 1).unwrap();
        assert_eq!(store.end(A), 2);
        assert!(store.door(A).is_none());
        assert_eq!(store.add(A, new("one"), 3).unwrap_err(), Refused::NoDoor, "nothing can be added to a door that ended");
        assert!(!store.revoke(A, 1));
        assert_eq!(store.door(B).unwrap().rules().len(), 1, "another door's rules are not touched");
        assert_eq!(store.end(A), 0, "ending twice is nothing");
        assert_eq!(store.doors(), 1);
    }

    #[test]
    fn a_new_hello_on_a_key_starts_over() {
        let mut store = store();
        store.add(A, new("one"), 1).unwrap();
        store.bind(A, "/other".into(), true);
        let door = store.door(A).unwrap();
        assert!(door.rules().is_empty() && door.yolo() && door.cwd() == Path::new("/other"));
        assert!(door.view().is_none());
    }

    #[test]
    fn a_rule_is_revoked_from_its_own_door_only() {
        let mut store = store();
        let rid = store.add(A, new("one"), 1).unwrap().rule().rid;
        assert!(!store.revoke(B, rid), "B has no such rule");
        assert!(!store.revoke(A, rid + 1));
        assert!(store.revoke(A, rid));
        assert!(!store.revoke(A, rid), "and once");
    }

    #[test]
    fn use_is_counted_and_shown() {
        let mut store = store();
        let rid = store.add(A, new("ls"), 1).unwrap().rule().rid;
        let shown = store.door(A).unwrap().rules()[0].json();
        assert_eq!(shown["uses"], 0);
        assert!(shown.get("last_ms").is_none(), "no last_ms before the first use");
        store.used(A, rid, 2500);
        store.used(A, rid, 3500);
        store.used(B, rid, 9999);
        let shown = store.door(A).unwrap().rules()[0].json();
        assert_eq!((shown["uses"].as_u64(), shown["last_ms"].as_u64()), (Some(2), Some(3500)));
    }

    #[test]
    fn a_rule_is_shown_with_what_the_page_needs_and_nothing_of_the_matcher() {
        let mut store = store();
        store.add(A, new("cargo test"), 1).unwrap();
        let mut name = new("");
        name.pattern = Pattern { tool: "write".into(), origin: Origin::Script, shape: Shape::Name };
        store.add(A, name, 2).unwrap();
        let rules = store.door(A).unwrap().rules();
        assert_eq!(
            rules[0].json(),
            json!({ "rid": 1, "tool": "bash", "origin": "model", "command": "cargo test", "cwd": "/w", "added_ms": 1000, "uses": 0 })
        );
        assert_eq!(rules[1].json(), json!({ "rid": 2, "tool": "write", "origin": "script", "added_ms": 1000, "uses": 0 }));
    }
}
