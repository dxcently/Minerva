//! Incremental search over the transcript: Helix's `/ ? n N *`, on the
//! surface that actually scrolls.
//!
//! ## What it searches, and what it scrolls to
//!
//! The scan runs over the transcript's **text** — an entry at a time,
//! never laid out — because laying the whole session out on every
//! keystroke is exactly the cost [`crate::render`]'s backwards walk exists
//! to avoid, and a search that stutters as you type is not a search you
//! use. So a hit is *an entry that matches*, and `n` scrolls that entry to
//! the top of the view.
//!
//! **And over the draft.** The message being composed is scanned along
//! with the session, as the entry sitting after the last of them, so a
//! `/` typed mid-message finds what you have written as readily as what
//! was said — see [`scan_all`]. It could not have been, once: the search
//! line stood in the prompt's own screen slot, so reaching for it put
//! the draft out of sight. The line lives on the prompt's bottom border
//! now (see [`crate::state::Mini`]), which is what made the question
//! askable.
//!
//! Two consequences worth knowing, both deliberate:
//!
//!  * **One hit per entry**, however many times the pattern occurs in it
//!    (the occurrences are counted, for the status line). The scroll unit
//!    is the entry, so counting occurrences separately would make `n`
//!    press without anything moving, which reads as a broken key.
//!  * **The highlight is painted separately**, by re-running the same
//!    regex over the lines actually on screen. That is a scan of one
//!    window, so it costs nothing, and it is exact where an offset
//!    computed against the raw text would not be: markdown eats its
//!    `**`, wrapping splits a match across two rows, and folded tool
//!    traffic draws a summary instead of the text that matched. What is
//!    highlighted is therefore always what is drawn.
//!
//! The second point has an edge, and it is the interesting one: a match
//! can be inside something that is not drawn — folded tool traffic, or
//! output capped by the detail knob. Hiding such hits would be worse
//! than showing them; the search would silently stop finding things the
//! session plainly contains. So they stay hits, and the renderer says
//! so: a block that covers matches but drew none of them gets the count
//! written onto the line standing in for it — `Wrote 3 files · 2
//! matches` — in the highlight's own colour, because that is where the
//! match went. `reveal_match` (`S-tab`) opens it without leaving the
//! search, and the search line offers that key only while there is
//! something to open. It is bound on the search line and nowhere else:
//! it means nothing without a live search, so no other mode advertises
//! it.
//!
//! ## Case, and half-typed patterns
//!
//! Smart case, as Helix's `search.smart-case` does it by default: a
//! lowercase pattern ignores case, one with any uppercase in it does not.
//!
//! The pattern is a regex, again as in Helix — but an incremental search
//! is compiled from a *half-typed* one on every keystroke, and `foo(` is
//! not a regex. Rather than blank the hits until the parenthesis is
//! closed, an uncompilable pattern is matched **literally** and the line
//! says so, so typing never passes through a state where the search
//! appears to have lost everything.

use regex::{Regex, RegexBuilder};

use crate::state::Entry;

/// Hits past this are not collected. A pattern like `.` matches
/// everywhere, and neither the status line nor `n` has any use for the
/// millionth one; the cap is what keeps a stray keystroke from walking
/// the whole session.
pub const MAX_HITS: usize = 2000;

/// Which way `n` goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Forward,
    Backward,
}

/// A transcript entry that matches, and how many times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub entry: usize,
    pub count: usize,
}

/// A search: the pattern, what it found, and where the view was when it
/// opened.
pub struct Search {
    /// The pattern. It lives here rather than on the line being typed
    /// because the search outlives its line: `n` repeats an accepted
    /// search, and the highlight stays lit, long after the minibuffer
    /// came down. While the line *is* up, every keystroke writes the
    /// minibuffer's text back into this and rescans.
    pub pattern: String,
    pub dir: Dir,
    /// The compiled pattern; `None` while the input is empty.
    pub re: Option<Regex>,
    /// The pattern did not compile and is being matched as plain text.
    pub literal: bool,
    pub hits: Vec<Hit>,
    /// Which hit we are on, as an index into `hits`.
    pub at: Option<usize>,
    /// `scroll_up` when the search opened, so `esc` puts the view back
    /// where it was — Helix restores the view on a cancelled search, and
    /// having scrolled away twenty screens by typing is the reason why.
    pub origin: usize,
    /// The entry the view was showing when it opened. Every incremental
    /// rescan measures "nearest" from here rather than from wherever the
    /// last keystroke scrolled to, or the preview would walk forward
    /// through the session one letter at a time.
    pub anchor: usize,
}

impl Search {
    pub fn new(dir: Dir, origin: usize, anchor: usize) -> Self {
        Search {
            pattern: String::new(),
            dir,
            re: None,
            literal: false,
            hits: Vec::new(),
            at: None,
            origin,
            anchor,
        }
    }

    /// The whole pattern as typed.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Recompile and rescan, then settle on the hit nearest `from` in the
    /// search's own direction. Called on every keystroke: this is the
    /// incremental part.
    ///
    /// `draft` is the message being composed, scanned as one more entry
    /// sitting after the last of them — see [`scan`].
    pub fn refresh(&mut self, entries: &[Entry], draft: &str, from: usize) {
        let compiled = compile(self.pattern());
        self.literal = compiled.as_ref().is_some_and(|c| c.literal);
        self.re = compiled.map(|c| c.re);
        self.hits = match &self.re {
            Some(re) => scan_all(entries, draft, re),
            None => Vec::new(),
        };
        self.at = self.nearest(from);
    }

    /// The hit to land on for a view sitting at entry `from`: the first
    /// at or after it going forward, the last at or before it going back,
    /// wrapping either way.
    fn nearest(&self, from: usize) -> Option<usize> {
        if self.hits.is_empty() {
            return None;
        }
        Some(match self.dir {
            Dir::Forward => self.hits.iter().position(|h| h.entry >= from).unwrap_or(0),
            Dir::Backward => self
                .hits
                .iter()
                .rposition(|h| h.entry <= from)
                .unwrap_or(self.hits.len() - 1),
        })
    }

    /// `n` (`same`) or `N`. Returns the entry to scroll to, wrapping at
    /// either end the way Helix's search does.
    pub fn step(&mut self, same: bool) -> Option<usize> {
        if self.hits.is_empty() {
            return None;
        }
        let forward = (self.dir == Dir::Forward) == same;
        let n = self.hits.len();
        self.at = Some(match self.at {
            None => {
                if forward {
                    0
                } else {
                    n - 1
                }
            }
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
        });
        self.entry()
    }

    /// Land directly on the `i`th hit — what a letter tag does.
    pub fn goto(&mut self, i: usize) -> Option<usize> {
        (i < self.hits.len()).then(|| {
            self.at = Some(i);
            self.hits[i].entry
        })
    }

    /// The entry the current hit is in.
    pub fn entry(&self) -> Option<usize> {
        self.at.map(|i| self.hits[i].entry)
    }

    /// Every occurrence, over every matching entry — the second half of
    /// `hit 3 of 12 · 40 matches`.
    pub fn occurrences(&self) -> usize {
        self.hits.iter().map(|h| h.count).sum()
    }

    /// How the search line reads: the sigil Helix uses for the direction,
    /// then the pattern.
    pub fn sigil(&self) -> char {
        match self.dir {
            Dir::Forward => '/',
            Dir::Backward => '?',
        }
    }
}

/// `text` as a pattern that matches exactly itself. What `*` searches
/// for is a selection, and a selection is text: a `.` in it means a dot.
pub fn as_text(text: &str) -> String {
    regex::escape(text)
}

/// A pattern that compiled, and whether it had to be escaped to do so.
pub struct Compiled {
    pub re: Regex,
    pub literal: bool,
}

/// Compile a pattern, smart-cased. An empty pattern is `None` — not an
/// error, just nothing to look for yet. A pattern that will not parse
/// falls back to itself, escaped, which always parses.
pub fn compile(pattern: &str) -> Option<Compiled> {
    if pattern.is_empty() {
        return None;
    }
    let insensitive = !pattern.chars().any(char::is_uppercase);
    let build = |p: &str| {
        RegexBuilder::new(p)
            .case_insensitive(insensitive)
            .size_limit(1 << 20)
            .build()
            .ok()
    };
    match build(pattern) {
        Some(re) => Some(Compiled { re, literal: false }),
        None => build(&regex::escape(pattern)).map(|re| Compiled { re, literal: true }),
    }
}

/// The transcript **and the draft**, scanned together.
///
/// The message being composed is one more thing on the screen with text
/// in it, so it is one more thing the search finds — and it is scanned
/// as the entry sitting *after* the last one, which is where it is
/// drawn. That is the whole of the encoding: a hit whose `entry` is
/// `entries.len()` is a hit in the prompt, `n` reaches it in the order
/// the eye would, and every consumer that walks the transcript by index
/// (the fold's match count, the letter tags, a block's range) steps over
/// it without being told about it, because no block covers that index.
///
/// Searching what you have not sent yet is not a curiosity: the reason
/// to search a session mid-message is usually to check the thing you
/// are in the middle of writing, and a search that hid the draft while
/// you looked was answering a different question.
pub fn scan_all(entries: &[Entry], draft: &str, re: &Regex) -> Vec<Hit> {
    let mut hits = scan(entries, re);
    if hits.len() >= MAX_HITS {
        return hits;
    }
    let count = re.find_iter(draft).filter(|m| !m.is_empty()).count();
    if count > 0 {
        hits.push(Hit {
            entry: entries.len(),
            count,
        });
    }
    hits
}

/// Which entries match, and how often. Zero-width matches are dropped:
/// `a*` matches between every pair of characters, and neither a highlight
/// nor a jump means anything at a match with no text in it.
pub fn scan(entries: &[Entry], re: &Regex) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let count = fields(e)
            .iter()
            .map(|f| re.find_iter(f).filter(|m| !m.is_empty()).count())
            .sum();
        if count > 0 {
            hits.push(Hit { entry: i, count });
            if hits.len() == MAX_HITS {
                break;
            }
        }
    }
    hits
}

/// The text of an entry, as the pieces it is made of — borrowed, never
/// joined, so that a keystroke's rescan allocates nothing. A tool call's
/// name and arguments are searchable along with its output: `read` finds
/// the call as readily as what it returned.
fn fields(e: &Entry) -> Vec<&str> {
    match e {
        Entry::User(t) | Entry::Info(t) | Entry::Alert(t) => vec![t.as_str()],
        // The settle line is searched like the note it reads as: `/cogitavit`
        // finds the turn in the transcript, star and all, because the star
        // is drawn and only the words are scanned.
        Entry::Settle { text, .. } => vec![text.as_str()],
        // Thinking is scanned like anything else, which it could not be
        // while it was a character count: the reasoning behind a change
        // is often the only place a name appears.
        Entry::Thinking { text, .. } | Entry::Assistant { text, .. } => vec![text.as_str()],
        Entry::Tool {
            name,
            input,
            output,
            ..
        } => match output {
            Some((out, _)) => vec![name.as_str(), input.as_str(), out.as_str()],
            None => vec![name.as_str(), input.as_str()],
        },
        // An image is searchable by its name, and only by its name —
        // which is the only part of it that is text. `/screenshot` finds
        // the picture, and the highlight has a run of characters on
        // screen to land on, because the caption is drawn. Scanning the
        // media type or the base64 would be scanning megabytes per
        // keystroke to match nothing anybody types.
        Entry::Image(a) => vec![a.name.as_str()],
        // The sender is a field of its own rather than part of the text,
        // so `/eidolon-9f2c` finds everything one session said without
        // the scan having to build a joined string per keystroke.
        Entry::Peer { from, text, .. } => vec![from.as_str(), text.as_str()],
        // A wake is found by what it was waiting on or what came of it:
        // `/it fired` lands on the morning the build finished.
        Entry::Woke { condition, outcome } => vec![condition.as_str(), outcome.as_str()],
        // A marked line and the answer it was given, both searchable —
        // `/list notes` finds the command as readily as what came back —
        // and kept apart rather than joined for the same reason a tool's
        // name and arguments are: a rescan per keystroke must not build a
        // string.
        Entry::Ask { commands, answers } => commands
            .iter()
            .flat_map(|m| {
                std::iter::once(m.command.as_str())
                    .chain(m.payloads.iter().map(|(_, bytes)| bytes.as_str()))
            })
            .chain(answers.iter().map(String::as_str))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript() -> Vec<Entry> {
        vec![
            Entry::User("read the Cargo file".into()),
            Entry::Assistant {
                text: "Reading it now.".into(),
                streaming: false,
            },
            Entry::Tool {
                id: String::new(),
                name: "read".into(),
                input: r#"{"path":"Cargo.toml"}"#.into(),
                output: Some(("[package]\nname = \"eidolon\"".into(), false)),
            },
            Entry::Info("compacted 4 messages".into()),
        ]
    }

    fn hits(pattern: &str) -> Vec<usize> {
        let c = compile(pattern).expect("a pattern");
        scan(&transcript(), &c.re)
            .into_iter()
            .map(|h| h.entry)
            .collect()
    }

    #[test]
    fn a_lowercase_pattern_ignores_case_and_a_shouted_one_does_not() {
        assert_eq!(hits("read"), vec![0, 1, 2], "`read` also finds `Reading`");
        assert_eq!(hits("Read"), vec![1], "the capital makes it exact");
    }

    #[test]
    fn a_tool_call_is_searchable_by_its_arguments_and_its_output() {
        assert_eq!(hits("Cargo.toml"), vec![2]);
        assert_eq!(hits("package"), vec![2]);
    }

    #[test]
    fn an_entry_that_matches_twice_is_one_hit_that_counts_twice() {
        let c = compile("a").unwrap();
        let found = scan(&transcript(), &c.re);
        let cargo = found.iter().find(|h| h.entry == 0).unwrap();
        assert_eq!(cargo.count, 2, "`Cargo` and `read the`");
        assert!(found.iter().all(|h| h.count >= 1));
    }

    #[test]
    fn a_half_typed_regex_matches_literally_instead_of_finding_nothing() {
        let c = compile("Cargo.toml)").expect("an unclosed group still compiles as text");
        assert!(c.literal);
        assert_eq!(scan(&transcript(), &c.re).len(), 0);
        let ok = compile("Car(g)o").unwrap();
        assert!(!ok.literal, "a valid regex is used as one");
        assert_eq!(
            scan(&transcript(), &ok.re).len(),
            2,
            "the message and the call's path"
        );
    }

    #[test]
    fn a_pattern_that_matches_nothing_at_all_is_not_a_hit() {
        assert!(hits("zzzz").is_empty());
        assert!(
            compile("").is_none(),
            "an empty pattern is nothing to look for"
        );
    }

    #[test]
    fn the_message_being_written_is_searched_along_with_the_session() {
        let c = compile("read").unwrap();
        let found = scan_all(&transcript(), "read it back to me", &c.re);
        assert_eq!(
            found.last().map(|h| h.entry),
            Some(4),
            "the draft sits after the last entry"
        );
        assert_eq!(found.len(), 4, "the three entries, and the draft");

        let quiet = scan_all(&transcript(), "nothing of the sort", &c.re);
        assert_eq!(quiet.len(), 3, "a draft that does not match is not a hit");
        assert_eq!(
            scan_all(&transcript(), "", &c.re).len(),
            3,
            "nor is an empty one"
        );
    }

    #[test]
    fn n_walks_off_the_end_of_the_session_and_onto_the_draft() {
        let mut s = Search::new(Dir::Forward, 0, 0);
        s.pattern = "read".into();
        s.refresh(&transcript(), "read that again", 0);
        assert_eq!(s.entry(), Some(0));
        assert_eq!(s.step(true), Some(1));
        assert_eq!(s.step(true), Some(2));
        assert_eq!(s.step(true), Some(4), "the prompt is the next hit along");
        assert_eq!(s.step(true), Some(0), "and then it wraps");
        assert_eq!(s.occurrences(), 4);
    }

    #[test]
    fn a_zero_width_match_is_not_a_hit() {
        let c = compile("q*").unwrap();
        assert!(
            scan(&transcript(), &c.re).is_empty(),
            "`q*` matches everywhere and means nothing anywhere"
        );
    }

    #[test]
    fn n_wraps_at_the_end_and_capital_n_goes_the_other_way() {
        let mut s = Search::new(Dir::Forward, 0, 0);
        s.pattern = "read".into();
        s.refresh(&transcript(), "", 0);
        assert_eq!(s.entry(), Some(0));
        assert_eq!(s.step(true), Some(1));
        assert_eq!(s.step(true), Some(2));
        assert_eq!(s.step(true), Some(0), "wraps");
        assert_eq!(s.step(false), Some(2), "N reverses, and wraps back");
    }

    #[test]
    fn the_search_opens_on_the_hit_nearest_the_view() {
        let mut s = Search::new(Dir::Forward, 0, 0);
        s.pattern = "read".into();
        s.refresh(&transcript(), "", 2);
        assert_eq!(s.entry(), Some(2), "forward from entry 2 lands on 2");

        let mut back = Search::new(Dir::Backward, 0, 0);
        back.pattern = "read".into();
        back.refresh(&transcript(), "", 1);
        assert_eq!(back.entry(), Some(1), "backward from 1 lands on 1");
        assert_eq!(
            back.step(true),
            Some(0),
            "n goes backwards in a backward search"
        );
    }

    #[test]
    fn occurrences_are_counted_across_the_entries_that_have_them() {
        let mut s = Search::new(Dir::Forward, 0, 0);
        s.pattern = "read".into();
        s.refresh(&transcript(), "", 0);
        assert_eq!(s.hits.len(), 3);
        assert_eq!(
            s.occurrences(),
            3,
            "once each: the message, `Reading`, and the call's name"
        );
    }
}
