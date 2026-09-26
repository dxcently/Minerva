//! Letter tags: put a label on every target on screen, then go to one by
//! typing its letter.
//!
//! This is Helix's `gw` (and flash.nvim's labelled search) rather than a
//! motion: the eye has already found the place, and a tag is the shortest
//! way to say *that one* — `3w` makes you count, `n n n n` makes you
//! walk. [`Jump`] is generic over what a tag points at ([`Spot`]),
//! because four things in the harness want the same machinery and want it
//! to land in four different places.
//!
//! ## What a tag can point at
//!
//! `gw` tags **what is on the screen**, which is the transcript first and
//! the prompt second — it used to tag the prompt's words alone, which is
//! backwards: the prompt is where you are already typing, and the screen
//! is the part you cannot reach. So one press of `gw` puts a label on
//! every word in the visible conversation *and* on every word in the
//! draft, and the label knows which it is: a word in the prompt is a
//! [`Spot::Word`], a char index and therefore a *motion* (select mode
//! extends to it); a word in the transcript is a [`Spot::Cell`], which
//! has no cursor to be — so landing there selects it and takes it,
//! which is the thing one actually wants a word out of a transcript for.
//!
//! The two halves are cut differently on purpose. In the prompt a tag
//! names *a place the cursor goes*, so the word class is Helix's own —
//! `foo` and `(` are two words. In the transcript a tag names *a thing
//! you are about to copy*, so it is the whitespace-delimited run:
//! `crates/tui/src/edit.rs` is one tag and one yank rather than four.
//!
//! `gb` tags **blocks** ([`Spot::Block`]) and lands the trace cursor,
//! which is what the transcript has instead of a caret — the same thing
//! `tab` on the search line does with [`Spot::Hit`], minus the pattern.
//! Words and blocks are two tag sets and not one because they are two
//! granularities: a screenful is a couple of hundred words and about a
//! dozen blocks, and one set of labels cannot be both.
//!
//! ## The alphabet
//!
//! `a`–`z`, which is Helix's `jump-label-alphabet` default and what the
//! operator's config leaves it at (`~/.config/nix/detail/helix/main.nix`
//! overrides plenty, but not that). Handed out in order, so a set of
//! four is `a b c d` and the awkward end of the keyboard is only reached
//! by a screen that earned it.
//!
//! **Every label in a set is the same width**, so no label is a prefix of
//! another. That is the whole reason a two-char set does not mix in the
//! one-char labels it could: with `a` and `ab` both live, pressing `a`
//! cannot be answered without waiting to see whether a `b` follows, and a
//! tag you have to wait after is worth less than the `n` it replaced.

/// The label characters, in the order they are handed out.
pub const ALPHABET: &str = "abcdefghijklmnopqrstuvwxyz";

/// Where one tag goes.
///
/// The variants are the four cursors the harness has, which is why this
/// is an enum rather than an index plus a mode: a set of tags may hold
/// more than one kind at once (`gw` tags the transcript and the prompt
/// in one press), so *where this one lands* is a property of the tag and
/// not of the set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spot {
    /// A char index into the prompt buffer — a motion, so select mode
    /// extends to it rather than jumping.
    Word(usize),
    /// A visible vault link; activating its tag follows rather than copies it.
    Link { x: u16, y: u16, target: String },
    /// A word drawn on the screen: where it starts, how many cells it
    /// covers, and what it says. Landing selects it and takes it.
    Cell {
        x: u16,
        y: u16,
        len: u16,
        text: String,
    },
    /// An index into the live search's hits — `/…` then `tab`.
    Hit(usize),
    /// A transcript entry — `gb`, which lands the trace cursor on it.
    Block(usize),
}

/// What a key press did to an open set of tags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// A prefix of at least one label: more to come.
    Pending,
    /// A label completed; here is the index of the spot it points at.
    Go(usize),
    /// No label can begin with that. The caller closes the tags.
    Miss,
}

/// An open set of tags, and the label chars typed against it so far.
#[derive(Clone, Debug)]
pub struct Jump {
    /// Where each label goes, in label order.
    pub spots: Vec<Spot>,
    pub labels: Vec<String>,
    /// The label chars typed so far.
    pub typed: String,
}

impl Jump {
    /// Tag `spots`, in the order given. More targets than the alphabet
    /// can label are dropped rather than left unlabelled: [`labels`] tops
    /// out at 676, and a screen with that many words on it has no
    /// thirteenth row of tags to spare anyway.
    pub fn new(spots: Vec<Spot>) -> Option<Jump> {
        if spots.is_empty() {
            return None;
        }
        let labels = labels(spots.len());
        let spots = spots.into_iter().take(labels.len()).collect();
        Some(Jump {
            spots,
            labels,
            typed: String::new(),
        })
    }

    /// Feed a key. A label only fires when it is complete, so a two-char
    /// set answers on the second press and never before.
    pub fn press(&mut self, c: char) -> Step {
        self.typed.push(c);
        if let Some(i) = self.labels.iter().position(|l| *l == self.typed) {
            return Step::Go(i);
        }
        if self.labels.iter().any(|l| l.starts_with(&self.typed)) {
            return Step::Pending;
        }
        Step::Miss
    }

    /// The spot a completed label pointed at.
    pub fn spot(&self, i: usize) -> Option<&Spot> {
        self.spots.get(i)
    }

    /// The tags still reachable, as `(where it points, what is left to
    /// type)`. Half-typed sets narrow to their prefix, so the screen
    /// stops offering what pressing a key can no longer reach.
    pub fn live(&self) -> Vec<(&Spot, &str)> {
        self.labels
            .iter()
            .zip(&self.spots)
            .filter_map(|(l, at)| l.strip_prefix(&self.typed).map(|rest| (at, rest)))
            .collect()
    }

    /// Is anything in this set pointing at the transcript? What the
    /// renderer asks before walking the drawn lines looking for cells.
    pub fn on_screen(&self) -> bool {
        self.spots
            .iter()
            .any(|s| matches!(s, Spot::Cell { .. } | Spot::Link { .. } | Spot::Block(_)))
    }
}

/// Labels for `n` targets: single chars while the alphabet lasts, then
/// pairs of them — never a mix, so that no label is a prefix of another.
pub fn labels(n: usize) -> Vec<String> {
    let a: Vec<char> = ALPHABET.chars().collect();
    if n <= a.len() {
        return a.iter().take(n).map(char::to_string).collect();
    }
    let mut out = Vec::with_capacity(n.min(a.len() * a.len()));
    for x in &a {
        for y in &a {
            out.push(format!("{x}{y}"));
            if out.len() == n {
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_char_labels_while_the_alphabet_lasts_then_pairs() {
        assert_eq!(labels(3), ["a", "b", "c"]);
        assert_eq!(labels(26).len(), 26);
        assert_eq!(labels(26).last().unwrap(), "z");
        let many = labels(30);
        assert_eq!(many.len(), 30);
        assert_eq!(many[0], "aa");
        assert_eq!(many[26], "ba");
        // No label is a prefix of another, which is what lets a press
        // resolve without waiting for the next one.
        assert!(many.iter().all(|l| l.chars().count() == 2));
    }

    #[test]
    fn more_targets_than_the_alphabet_can_label_are_dropped() {
        let j = Jump::new((0..2000).map(Spot::Word).collect()).unwrap();
        assert_eq!(j.spots.len(), 26 * 26);
        assert_eq!(j.labels.len(), 26 * 26);
    }

    #[test]
    fn a_single_char_set_fires_on_the_first_press() {
        let mut j = Jump::new(vec![4, 9, 17].into_iter().map(Spot::Word).collect()).unwrap();
        assert_eq!(j.press('b'), Step::Go(1));
        assert_eq!(j.spot(1), Some(&Spot::Word(9)));
    }

    #[test]
    fn a_pair_set_narrows_and_then_fires() {
        let mut j = Jump::new((0..30).map(Spot::Hit).collect()).unwrap();
        assert_eq!(j.press('b'), Step::Pending);
        // Only the `b…` labels are still offered, and they are drawn by
        // what is left to type rather than by the whole label.
        let live = j.live();
        assert_eq!(live.len(), 4, "ba, bb, bc, bd are what 30 targets reach");
        assert_eq!(live[0], (&Spot::Hit(26), "a"));
        assert_eq!(j.press('c'), Step::Go(28));
    }

    #[test]
    fn a_key_no_label_starts_with_is_a_miss() {
        let mut j = Jump::new(vec![Spot::Word(1), Spot::Word(2)]).unwrap();
        assert_eq!(j.press('q'), Step::Miss);
    }

    #[test]
    fn nothing_to_tag_opens_nothing() {
        assert!(Jump::new(Vec::new()).is_none());
    }

    /// One press of `gw` puts labels on two different kinds of thing at
    /// once, and each knows where it lands.
    #[test]
    fn a_set_can_mix_the_screen_and_the_prompt() {
        let j = Jump::new(vec![
            Spot::Cell {
                x: 4,
                y: 2,
                len: 7,
                text: "Cargo.t".into(),
            },
            Spot::Word(3),
        ])
        .unwrap();
        assert!(j.on_screen());
        assert_eq!(j.labels, ["a", "b"]);
        assert!(!Jump::new(vec![Spot::Word(0)]).unwrap().on_screen());
    }
}
