//! **Trace mode**: a cursor on the transcript, and the folds it opens.
//!
//! The transcript is a flat list of entries drawn as a list of *blocks*
//! — see [`crate::render::block_at`], which is where a fold is decided.
//! At rest a **cluster** (a run of thinking and finished tool calls) is
//! one block and one line, so a turn that thought three times and ran
//! five tools costs a line rather than a screen. That is a good default
//! and a bad ceiling: sooner or later you want the fifth call's output,
//! and the only way in used to be a global knob with three steps that
//! opened *every* fold in the session to get at one.
//!
//! This is the other way in. Trace mode gives the transcript what the
//! prompt has had all along: a cursor, a selection, and motions.
//!
//!  * `j`/`k` walk the blocks — a message, a cluster, a note.
//!  * `l` opens the block under the cursor and steps *into* it: the
//!    cluster becomes its parts, one thinking block and one tool call
//!    at a time, each with all of its output, and the cursor lands on
//!    the first.
//!  * `h` closes it again and puts the cursor back on the fold, so
//!    `l`/`h` is one level down and one level up.
//!  * `v` extends, exactly as in the prompt, and `y` yanks what is
//!    selected — the model's reasoning, or a command's output, without
//!    reaching for the mouse.
//!  * `:trace <what>` filters the walk: `j`/`k` hop only the blocks
//!    that carry the word — a tool's name (`edit`), an entry's kind
//!    (`call`, `reply`), or `*` for the calls that changed something,
//!    which is where "what did this turn touch" lives now that every
//!    settled call folds alike. `:trace` alone clears it, and leaving
//!    the mode clears it too.
//!
//! Leaving the mode puts the cursor away and closes the folds it opened:
//! the transcript comes back to its resting, folded shape.
//!
//! ## What it does not own
//!
//! Nothing here is a second way to decide what a block is: the fold
//! comes from [`crate::render`], the same walk the draw uses, so the
//! cursor cannot be on a block the screen does not have. And nothing
//! here is written to the transcript — [`Trace`] is a *view*, like
//! [`crate::state::Detail`] and [`crate::state::UiState::opened`], so a
//! resumed session opens with a clean one and no bookkeeping.
//!
//! ## Vault links
//!
//! `tab` / `S-tab` select wikilinks in the current assistant block; `ret`
//! opens the selected one (or the first) in the read-only vault page.
//! `l` / `h` and `zz` still operate folds. The page uses the same link
//! keys, with `backspace` for its note history and `q` / `esc` to return
//! to the transcript. [`crate::vault`] owns that view and its async reads;
//! the renderer owns the link runs shared by keyboard focus and clicks.
//!
//! ## And the search
//!
//! The search moves the cursor. A hit inside a fold puts the cursor on
//! the fold — with the count already drawn onto it (`Ran 3 shell
//! commands · 2 matches`) — and `l` opens it. The search is not allowed
//! to open anything on its own: `S-tab` on the search line is the key
//! that says so, and a `n` that silently unfolded three clusters would
//! leave a screen nobody asked for. Going the other way, `*` in trace
//! mode searches for what the *transcript* has selected, the way it
//! searches for what the prompt has selected everywhere else.

use std::ops::Range;

use crate::state::UiState;

/// The transcript's cursor.
///
/// `at` and `anchor` are entry indices, not block indices: blocks come
/// and go as folds open, and an index into a list that is recomputed
/// every keystroke would point somewhere else after every keystroke.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trace {
    /// The entry the cursor is on. Always the *first* entry of its
    /// block, so that "the block under the cursor" needs no search.
    pub at: usize,
    /// The other end of the selection. Equal to `at` unless extending.
    pub anchor: usize,
    /// Motions extend rather than reselect — Helix's select mode, for
    /// the transcript.
    pub extend: bool,
    /// The cursor has been placed for this visit to the mode. Cleared
    /// when the mode is left, which is what makes entering it land on
    /// what you are looking at rather than where you were an hour ago.
    pub live: bool,
}

/// Where the cursor is, for the status line. Computed once per frame,
/// and only while the mode is on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Where {
    /// 1-based, so a script can print it without arithmetic; 0 when the
    /// mode is off or the transcript is empty.
    pub at: usize,
    pub blocks: usize,
    /// What the block is: `cluster` when it stands for several entries,
    /// else the entry's own name (`cogitat`, `call`, `reply`, `you`).
    pub kind: &'static str,
    /// There is a fold here for `l` to open.
    pub open: bool,
    /// How many entries the selection covers.
    pub span: usize,
}

impl Where {
    pub fn of(state: &UiState) -> Where {
        if state.vault.active && state.tracing() {
            let selection = crate::vault::trace_selection(state);
            return Where { at: state.vault.trace.at,
                blocks: state.vault.page.as_ref().map_or(0, |n| n.body.split('\n').count()),
                kind: "source line", open: false, span: selection.end() - selection.start() + 1 };
        }
        if !state.tracing() || state.transcript.is_empty() {
            return Where::default();
        }
        // The count the cursor experiences: with a filter set, `3/7`
        // names the seventh of seven *matching* blocks, and a cursor
        // standing on an excluded block says `0` — off the walk —
        // until a motion brings it back onto one.
        let bs = walkable(state);
        let here = bs.iter().position(|b| b.contains(&state.trace.at));
        let block = here
            .map(|i| bs[i].clone())
            .unwrap_or_else(|| {
                crate::render::block_at(
                    state,
                    state.trace.at.min(state.transcript.len().saturating_sub(1)),
                )
            });
        let sel = selection(state);
        Where {
            at: here.map_or(0, |i| i + 1),
            blocks: bs.len(),
            kind: match (block.len() > 1, state.transcript.get(block.start)) {
                (true, _) => "cluster",
                (false, Some(e)) => e.kind(),
                (false, None) => "",
            },
            open: crate::render::fold_at(state, state.trace.at).is_some(),
            span: sel.len(),
        }
    }
}

/// Every block the transcript draws, top to bottom.
///
/// Walked backwards for the same reason [`crate::render::walk`] is —
/// a block's *start* is only known once its end is — and then reversed,
/// which costs one allocation per keystroke in this mode and nothing at
/// all in any other.
pub fn blocks(state: &UiState) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut end = state.transcript.len();
    while end > 0 {
        let b = crate::render::block_at(state, end - 1);
        end = b.start;
        out.push(b);
    }
    out.reverse();
    out
}

/// Does one entry match a filter word? Kinds first (`call`, `reply`,
/// `note`, …), then a tool's own name — `edit`, `read`, `shell` — so a
/// filter can be as coarse as a kind or as narrow as a tool.
fn entry_matches(e: &crate::state::Entry, f: &str) -> bool {
    if e.kind().contains(f) {
        return true;
    }
    match e {
        crate::state::Entry::Tool { name, .. } => name.to_lowercase().contains(f),
        _ => false,
    }
}

/// Does a block match the cursor's filter? No filter matches everything.
/// `*` is the calls that changed something — the diff is the account of
/// what an edit did, and this is how the cursor finds it now that every
/// settled call folds alike.
fn block_matches(state: &UiState, b: &Range<usize>) -> bool {
    let Some(f) = state
        .trace_filter
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_lowercase)
    else {
        return true;
    };
    state.transcript[b.clone()].iter().any(|e| {
        if f == "*" {
            return match e {
                crate::state::Entry::Tool { name, input, .. } => {
                    crate::render::edit_diff(name, input).is_some()
                }
                _ => false,
            };
        }
        entry_matches(e, &f)
    })
}

/// The blocks the filter leaves: all of them when it is unset.
pub fn walkable(state: &UiState) -> Vec<Range<usize>> {
    let bs = blocks(state);
    if state
        .trace_filter
        .as_deref()
        .is_none_or(|f| f.trim().is_empty())
    {
        return bs;
    }
    bs.into_iter().filter(|b| block_matches(state, b)).collect()
}

/// The entries the cursor covers: both ends grown to whole blocks,
/// because a block is what the screen draws and half of one is not a
/// thing the operator can see they have selected.
pub fn selection(state: &UiState) -> Range<usize> {
    if state.transcript.is_empty() {
        return 0..0;
    }
    let (a, z) = (
        state.trace.at.min(state.trace.anchor),
        state.trace.at.max(state.trace.anchor),
    );
    let lo = crate::render::block_at(state, a.min(state.transcript.len() - 1)).start;
    let hi = crate::render::block_at(state, z.min(state.transcript.len() - 1)).end;
    lo..hi
}

/// Place the cursor on entry into the mode, and keep it in range while
/// the mode is on. Called once per frame, before the snapshot: the mode
/// can be entered by a key, by `:enter_mode trace` or by a script, and
/// this is the one place that has to know about all three.
///
/// Deliberately does **not** scroll. A wheel in trace mode is still a
/// wheel; the view follows the cursor when the cursor moves, and not
/// otherwise.
pub fn sync(state: &mut UiState) {
    if state.vault.active { crate::vault::trace_sync(state); return; }
    if !state.tracing() {
        if state.trace.live {
            // The folds `l` and `zo` opened are part of the mode's view,
            // exactly like the cursor: leaving trace closes them rather
            // than leaving the transcript unrolled behind the prompt.
            state.opened.clear();
        }
        state.trace.live = false;
        // The filter is the mode's too: leaving trace puts the whole
        // transcript back under the cursor, not just the parts some
        // word admitted.
        state.trace_filter = None;
        return;
    }
    if state.transcript.is_empty() {
        state.trace = Trace {
            live: true,
            ..Trace::default()
        };
        return;
    }
    if !state.trace.live {
        state.trace.live = true;
        place(state);
        return;
    }
    let last = state.transcript.len() - 1;
    if state.trace.at > last || state.trace.anchor > last {
        state.trace.at = state.trace.at.min(last);
        state.trace.anchor = state.trace.anchor.min(last);
        snap(state);
    }
}

/// Where the cursor goes when the mode opens: the match you are looking
/// at if a search is live, else the last block of the transcript — the
/// one nearest the prompt line, where the newest work is.
fn place(state: &mut UiState) {
    let last = state.transcript.len() - 1;
    let at = state
        .search
        .as_ref()
        .and_then(crate::search::Search::entry)
        .filter(|e| *e <= last)
        .unwrap_or(last);
    state.trace = Trace {
        at,
        anchor: at,
        extend: false,
        live: true,
    };
    snap(state);
    reveal(state);
}

/// Pull both ends of the cursor onto the head of their blocks.
fn snap(state: &mut UiState) {
    state.trace.at = crate::render::block_at(state, state.trace.at).start;
    state.trace.anchor = crate::render::block_at(state, state.trace.anchor).start;
}

/// Put the cursor on the block holding `entry`, extending if the mode
/// is extending, and scroll it into view.
pub fn goto(state: &mut UiState, entry: usize) {
    if state.transcript.is_empty() {
        return;
    }
    let entry = entry.min(state.transcript.len() - 1);
    state.trace.at = crate::render::block_at(state, entry).start;
    if !state.trace.extend {
        state.trace.anchor = state.trace.at;
    }
    reveal(state);
}

/// `j`/`k`: `delta` blocks along, clamped at both ends.
pub fn step(state: &mut UiState, delta: isize) {
    let bs = walkable(state);
    if bs.is_empty() {
        return;
    }
    // Where the cursor is among the walkable blocks. A block the filter
    // excluded is "before the next match", so the first match past it is
    // still one `j` away — and one `k` finds the match before it, which
    // is what a hand hopping reads naturally off a filtered screen.
    let here = bs.iter().position(|b| b.contains(&state.trace.at));
    let there = match here {
        Some(i) => (i as isize + delta).clamp(0, bs.len() as isize - 1) as usize,
        None => {
            let cur = crate::render::block_at(
                state,
                state.trace.at.min(state.transcript.len().saturating_sub(1)),
            );
            if delta >= 0 {
                bs.iter()
                    .position(|b| b.start >= cur.end)
                    .unwrap_or(bs.len() - 1)
            } else {
                bs.iter()
                    .rposition(|b| b.end <= cur.start)
                    .unwrap_or(0)
            }
        }
    };
    goto(state, bs[there].start);
}

/// `gg` / `ge`.
pub fn edge(state: &mut UiState, last: bool) {
    let bs = walkable(state);
    let Some(b) = (if last { bs.last() } else { bs.first() }) else {
        return;
    };
    goto(state, b.start);
}

/// `l`: open the fold under the cursor and step into it. False when
/// there is nothing here to open.
pub fn open(state: &mut UiState) -> bool {
    let Some(run) = crate::render::fold_at(state, state.trace.at) else {
        return false;
    };
    state.opened.push(run);
    // Into the fold, not merely onto it: the first thing it was hiding
    // is what you opened it for, and it is now a block of its own.
    state.trace.at = run;
    if !state.trace.extend {
        state.trace.anchor = run;
    }
    reveal(state);
    true
}

/// `h`: close the fold the cursor is inside and come back up onto it.
pub fn close(state: &mut UiState) -> bool {
    let Some(run) = crate::render::opened_at(state, state.trace.at) else {
        return false;
    };
    state.opened.retain(|r| *r != run);
    state.trace.at = run;
    if !state.trace.extend {
        state.trace.anchor = run;
    }
    reveal(state);
    true
}

/// Every fold in the transcript, opened or closed at once — `zo` / `zc`.
/// Returns how many changed, so the caller can say nothing happened.
pub fn fold_all(state: &mut UiState, open: bool) -> usize {
    if !open {
        let n = state.opened.len();
        state.opened.clear();
        if state.tracing() {
            snap(state);
        }
        return n;
    }
    let mut n = 0;
    let mut end = state.transcript.len();
    while end > 0 {
        let b = crate::render::block_at(state, end - 1);
        if b.len() > 1 || crate::render::fold_at(state, b.start).is_some() {
            state.opened.push(b.start);
            n += 1;
        }
        end = b.start;
    }
    n
}

/// `v`. Dropping out of extend collapses onto the cursor, as `v` does
/// in the prompt.
pub fn extend(state: &mut UiState) {
    state.trace.extend = !state.trace.extend;
    if !state.trace.extend {
        state.trace.anchor = state.trace.at;
    }
}

/// What `y` copies: the selected blocks, run together the way they read.
pub fn yanked(state: &UiState) -> String {
    let sel = selection(state);
    state.transcript[sel]
        .iter()
        .map(crate::state::Entry::text)
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Scroll the least that puts the cursor's whole block on screen — its
/// top row when the block is taller than the window, since that is
/// where the summary and the arguments are.
pub fn reveal(state: &mut UiState) {
    let (width, height) = (state.view_width.max(1), state.view_height.max(1));
    let (below, tall) = crate::render::block_extent(state, width, state.trace.at);
    // `below` counts the drawn lines from the top of the block to the
    // bottom of the transcript, so the block sits at the top of the
    // view when `scroll_up == below - height` and at the bottom when
    // `scroll_up == below - tall`. Anywhere between is already visible.
    let low = below.saturating_sub(height);
    let high = below.saturating_sub(tall);
    // Through `set_scroll`: this is the cursor's hand on the view, and a
    // scroll anchor left standing from an earlier frame would be
    // re-derived over the top of it.
    state.set_scroll(state.scroll_up.clamp(low.min(high), high.max(low)));
    if high < low {
        state.set_scroll(low);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{Kind, Mode};
    use crate::state::{Detail, Entry};

    fn tool(name: &str, out: &str) -> Entry {
        Entry::Tool {
            id: String::new(),
            name: name.into(),
            input: "{}".into(),
            output: Some((out.into(), false)),
        }
    }

    /// A call whose arguments carry a diff, the shape `edit` answers
    /// with — `*` in a trace filter is defined by exactly this.
    fn edit() -> Entry {
        Entry::Tool {
            id: String::new(),
            name: "edit".into(),
            input: r#"{"path": "a.rs", "old_str": "before", "new_str": "after"}"#.into(),
            output: Some(("edited a.rs (1 replacement)".into(), false)),
        }
    }

    fn thinking(t: &str) -> Entry {
        Entry::Thinking {
            text: t.into(),
            streaming: false,
        }
    }

    /// A session shaped like a real one: a question, two rounds of
    /// thinking-and-calling with nothing between them, then the answer.
    fn state() -> UiState {
        let mut s = UiState::new("m".into(), "s.eid".into(), "/".into());
        s.transcript = vec![
            Entry::User("what is here?".into()),
            thinking("let me look"),
            tool("shell", "a b c"),
            thinking("and again"),
            tool("read", "[package]"),
            tool("read", "fn main"),
            Entry::Assistant {
                text: "It is a Rust crate.".into(),
                streaming: false,
            },
        ];
        s.prompt.mode = Mode::named("trace", Kind::Normal);
        s.view_height = 40;
        s.view_width = 80;
        sync(&mut s);
        s
    }

    #[test]
    fn the_whole_run_of_thinking_and_calls_is_one_block() {
        let s = state();
        assert_eq!(
            blocks(&s),
            vec![0..1, 1..6, 6..7],
            "the question, the cluster, the answer"
        );
    }

    /// `:trace read`, folded and opened: folded, the cluster is the one
    /// block carrying a read; opened, `j`/`k` hop read to read and
    /// nothing else — the whole point of the filter.
    #[test]
    fn a_filter_hops_matching_blocks_only() {
        let mut s = state();
        s.trace_filter = Some("read".into());
        edge(&mut s, false);
        assert_eq!(s.trace.at, 1, "folded, the cluster is the only match");
        assert_eq!(Where::of(&s).blocks, 1, "the count the cursor experiences");
        s.opened.push(1);
        step(&mut s, 1);
        assert_eq!(s.trace.at, 4, "from the cluster's start to the first read");
        step(&mut s, 1);
        assert_eq!(s.trace.at, 5, "read to read");
        step(&mut s, 1);
        assert_eq!(s.trace.at, 5, "clamped at the last match");
        step(&mut s, -1);
        assert_eq!(s.trace.at, 4);
        step(&mut s, -1);
        assert_eq!(s.trace.at, 4, "clamped at the first match");
    }

    /// `*` is the calls that changed something: with the cluster open it
    /// matches the edit and nothing else — the read, the shells and the
    /// prose all stand aside.
    #[test]
    fn star_matches_the_calls_that_changed_something() {
        let mut s = state();
        s.transcript.insert(6, edit());
        s.trace_filter = Some("*".into());
        edge(&mut s, false);
        assert_eq!(s.trace.at, 1, "folded, the cluster carries the change");
        s.opened.push(1);
        edge(&mut s, true);
        assert_eq!(s.trace.at, 6, "opened, the edit is the only match");
        assert_eq!(Where::of(&s).blocks, 1);
        edge(&mut s, false);
        assert_eq!(s.trace.at, 6, "a one-match walk clamps both ways");
    }

    /// Kinds filter too, and as coarsely as a tool name filters narrowly:
    /// `reply` is the answer, `you` the question, `call` any tool at all.
    #[test]
    fn kinds_filter_as_well_as_tools() {
        let mut s = state();
        s.trace_filter = Some("reply".into());
        edge(&mut s, true);
        assert_eq!(s.trace.at, 6);
        s.trace_filter = Some("you".into());
        edge(&mut s, false);
        assert_eq!(s.trace.at, 0);
        s.trace_filter = Some("call".into());
        edge(&mut s, false);
        assert_eq!(s.trace.at, 1, "the cluster holds every call");
    }

    /// The filter is the mode's, not the session's: leaving trace puts
    /// the whole transcript back under the cursor.
    #[test]
    fn leaving_the_mode_clears_the_filter() {
        let mut s = state();
        s.trace_filter = Some("read".into());
        s.opened.push(1);
        s.prompt.enter_normal();
        sync(&mut s);
        assert_eq!(s.trace_filter, None);
        assert!(s.opened.is_empty(), "the folds went with it, as before");
    }

    #[test]
    fn stepping_walks_blocks_and_stops_at_both_ends() {
        let mut s = state();
        edge(&mut s, false);
        assert_eq!(s.trace.at, 0);
        step(&mut s, 1);
        assert_eq!(s.trace.at, 1, "the whole cluster in one press");
        step(&mut s, 1);
        assert_eq!(s.trace.at, 6);
        step(&mut s, 1);
        assert_eq!(s.trace.at, 6, "clamped, not wrapped");
        step(&mut s, -9);
        assert_eq!(s.trace.at, 0);
    }

    #[test]
    fn opening_a_cluster_steps_into_it_and_closing_comes_back_out() {
        let mut s = state();
        edge(&mut s, false);
        step(&mut s, 1);
        assert!(open(&mut s));
        assert_eq!(s.trace.at, 1, "on the first thing the fold was hiding");
        assert_eq!(
            blocks(&s),
            vec![0..1, 1..2, 2..3, 3..4, 4..5, 5..6, 6..7],
            "its parts, one each"
        );
        // …and now j/k walk the calls themselves.
        step(&mut s, 2);
        assert_eq!(s.trace.at, 3);
        assert!(close(&mut s), "closing from inside the fold");
        assert_eq!(s.trace.at, 1, "back onto the fold");
        assert_eq!(blocks(&s), vec![0..1, 1..6, 6..7]);
        assert!(!close(&mut s), "nothing left to close");
    }

    #[test]
    fn there_is_nothing_to_open_on_a_message() {
        let mut s = state();
        edge(&mut s, true);
        assert_eq!(s.trace.at, 6);
        assert!(!open(&mut s));
    }

    #[test]
    fn and_nothing_to_open_once_the_detail_knob_has_opened_everything() {
        let mut s = state();
        s.detail = Detail::Full;
        assert_eq!(blocks(&s).len(), 7, "no folds at all");
        s.trace.at = 1;
        assert!(!open(&mut s));
    }

    #[test]
    fn extending_selects_whole_blocks_and_yanks_them() {
        let mut s = state();
        edge(&mut s, false);
        extend(&mut s);
        step(&mut s, 1);
        assert_eq!(selection(&s), 0..6, "the question and the whole cluster");
        let y = yanked(&s);
        assert!(
            y.contains("what is here?") && y.contains("let me look") && y.contains("[package]"),
            "{y}"
        );
        assert!(
            !y.contains("It is a Rust crate"),
            "the answer was not selected"
        );
        extend(&mut s);
        assert_eq!(
            selection(&s),
            1..6,
            "collapsing keeps the cursor, drops the anchor"
        );
    }

    #[test]
    fn the_cursor_is_placed_on_the_search_hit_when_there_is_one() {
        let mut s = state();
        let mut search = crate::search::Search::new(crate::search::Dir::Forward, 0, 0);
        search.pattern = "package".into();
        search.refresh(&s.transcript, "", 0);
        s.search = Some(search);
        s.trace.live = false;
        sync(&mut s);
        assert_eq!(
            s.trace.at, 1,
            "the match is in entry 4, which is drawn as the cluster at 1"
        );
    }

    #[test]
    fn the_cursor_starts_on_the_last_block_not_the_first() {
        let mut s = state();
        s.trace.live = false;
        // Not the whole transcript: the view is scrolled back. Entering
        // the mode still lands on the newest block, down at the prompt.
        s.view_entries = 0..3;
        sync(&mut s);
        assert_eq!(s.trace.at, 6, "the answer is the last block");
        assert_eq!(s.trace.anchor, 6);
    }

    #[test]
    fn leaving_trace_closes_the_folds_it_opened() {
        let mut s = state();
        edge(&mut s, false);
        step(&mut s, 1);
        assert!(open(&mut s));
        assert_eq!(s.opened, vec![1]);
        s.prompt.mode = Mode::normal();
        sync(&mut s);
        assert!(!s.trace.live, "the cursor is put away");
        assert!(s.opened.is_empty(), "and the fold it opened is shut");
    }

    #[test]
    fn a_cursor_left_pointing_past_the_end_is_pulled_back_rather_than_panicking() {
        let mut s = state();
        s.trace.at = 99;
        s.trace.anchor = 99;
        sync(&mut s);
        assert_eq!(s.trace.at, 6);
        assert_eq!(selection(&s), 6..7);
    }

    #[test]
    fn folding_everything_at_once_is_the_same_view_as_folding_each() {
        let mut s = state();
        assert_eq!(fold_all(&mut s, true), 1, "one cluster in this session");
        assert_eq!(blocks(&s).len(), 7);
        assert_eq!(fold_all(&mut s, false), 1);
        assert_eq!(blocks(&s), vec![0..1, 1..6, 6..7]);
    }
}
