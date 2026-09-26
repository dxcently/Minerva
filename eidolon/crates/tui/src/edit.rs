//! The modal buffer: Helix's selection-first editing model, sized for a
//! prompt rather than a file.
//!
//! A [`Buffer`] is text plus **one** [`Range`] and a [`Mode`]. Following
//! Helix, the range is the unit everything acts on: a motion *reselects*
//! rather than merely moving, and a verb (`d`, `c`, `y`, `~`) consumes
//! whatever the last motion selected. `anchor` is the fixed end and `head`
//! the moving one, both char indices; when `head > anchor` the block
//! cursor sits on the char at `head - 1`, so a forward range of width one
//! covers exactly the char under the cursor.
//!
//! Char indices, not bytes: the arithmetic here is all "one position
//! left/right", which is wrong on bytes and tedious on graphemes. A prompt
//! is short enough that converting to a byte offset for rendering costs
//! nothing. Every public method leaves the buffer in a state where the
//! range is in bounds and, outside the text modes, non-degenerate — see
//! [`Buffer::normalize`].
//!
//! No multiple cursors. Helix's `Selection` is a list of ranges; this is
//! the single-range subset, which is what a three-line prompt wants.

/// How the buffer behaves in a mode — the part of a mode that is the
/// buffer's business. Everything else about a mode (its name on the
/// frame, its colour, its badge, the sigil at the head of the line) is a
/// row in the script's `modes()` table, see [`crate::modes`]; the buffer
/// never reads that table, it is told a [`Mode`] and keeps the kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Motions reselect, letters are commands, the cursor is a block.
    Normal,
    /// Keys are text, the cursor is a caret.
    Insert,
    /// Motions extend the range instead of replacing it.
    Select,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Normal => "normal",
            Kind::Insert => "insert",
            Kind::Select => "select",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "normal" => Some(Kind::Normal),
            "insert" => Some(Kind::Insert),
            "select" => Some(Kind::Select),
            _ => None,
        }
    }

    /// Are keys text here?
    pub fn typing(self) -> bool {
        self == Kind::Insert
    }
}

/// Which keymap is live, and how a motion behaves: a name and a kind.
///
/// The name is what the keymap is indexed by and what the frame says;
/// the kind is what the editing code asks. Two modes may share a kind —
/// dispatch is insert in every respect a buffer can see, and differs only
/// in what `ret` means, which the script decides from the name — so
/// nothing in this module compares names, only kinds.
///
/// Three names are canonical, because the Helix verbs reach them: `i`,
/// `c` and `o` go to [`Mode::insert`], `v` toggles [`Mode::select`], and
/// `esc` returns to [`Mode::normal`]. A script restyles those rows; it
/// cannot change what `i` means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mode {
    pub name: String,
    pub kind: Kind,
}

impl Mode {
    pub fn normal() -> Self {
        Mode::named("normal", Kind::Normal)
    }

    pub fn insert() -> Self {
        Mode::named("insert", Kind::Insert)
    }

    pub fn select() -> Self {
        Mode::named("select", Kind::Select)
    }

    pub fn named(name: &str, kind: Kind) -> Self {
        Mode {
            name: name.to_string(),
            kind,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.name
    }

    /// Are keys text here? Everything about *editing* asks this rather
    /// than naming a mode, because two modes can type — dispatch is
    /// insert to the character — and a user may add a third.
    pub fn typing(&self) -> bool {
        self.kind.typing()
    }

    pub fn is(&self, name: &str) -> bool {
        self.name == name
    }
}

/// A selection: `anchor` stays put, `head` moves. Char indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Range {
    pub anchor: usize,
    pub head: usize,
}

impl Range {
    pub fn point(at: usize) -> Self {
        Range {
            anchor: at,
            head: at,
        }
    }
    /// The lower end, inclusive.
    pub fn from(&self) -> usize {
        self.anchor.min(self.head)
    }
    /// The upper end, exclusive.
    pub fn to(&self) -> usize {
        self.anchor.max(self.head)
    }
    /// Where the block cursor is drawn: on the last selected char when the
    /// range runs forward, on `head` itself when it runs back or is empty.
    pub fn cursor(&self) -> usize {
        if self.head > self.anchor {
            self.head - 1
        } else {
            self.head
        }
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// A character's word class. Two runs of different classes are different
/// words, which is what makes `w` stop between `foo` and `(`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cat {
    Space,
    Word,
    Punct,
}

fn cat(c: char) -> Cat {
    if c.is_whitespace() {
        Cat::Space
    } else if c.is_alphanumeric() || c == '_' {
        Cat::Word
    } else {
        Cat::Punct
    }
}

/// The same split for `W`/`B`/`E`: whitespace, or not.
fn long_cat(c: char) -> Cat {
    if c.is_whitespace() {
        Cat::Space
    } else {
        Cat::Word
    }
}

/// One undo step: the text and where the range was when it was taken.
#[derive(Clone, Debug)]
struct Snapshot {
    text: String,
    range: Range,
}

/// The prompt: text, one selection, a mode, an undo stack and a register.
#[derive(Clone, Debug)]
pub struct Buffer {
    text: String,
    pub range: Range,
    pub mode: Mode,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The unnamed register — what `y`, `d` and `c` fill and `p` pastes.
    pub register: String,
    /// Submitted messages, newest last.
    pub history: Vec<String>,
    /// How far back through `history` we have walked, if at all.
    history_at: Option<usize>,
    /// True while an insert run is in progress, so a whole burst of typing
    /// collapses into the one undo step taken when insert began.
    inserting: bool,
    /// `x` again extends by a line rather than reselecting the same one.
    line_selected: bool,
    /// What the insert run in progress has typed, so leaving the mode can
    /// remember it as [`Buffer::last_insert`].
    run: String,
    /// The text the last completed insert run typed — what `.` replays.
    last_insert: String,
    /// Whether that run began by replacing a selection, which is how `.`
    /// knows to replace rather than type.
    run_replaced: bool,
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer {
            text: String::new(),
            range: Range::default(),
            mode: Mode::normal(),
            undo: Vec::new(),
            redo: Vec::new(),
            register: String::new(),
            history: Vec::new(),
            history_at: None,
            inserting: false,
            line_selected: false,
            run: String::new(),
            last_insert: String::new(),
            run_replaced: false,
        }
    }
}

impl Buffer {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn chars(&self) -> Vec<char> {
        self.text.chars().collect()
    }

    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The byte offset of char index `ci`, for slicing and for the renderer.
    pub fn byte_of(&self, ci: usize) -> usize {
        self.text
            .char_indices()
            .nth(ci)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }

    /// The selected text.
    pub fn selection(&self) -> String {
        self.chars()[self.range.from().min(self.len())..self.range.to().min(self.len())]
            .iter()
            .collect()
    }

    /// Clamp the range into the text and, outside insert mode, widen an
    /// empty one to cover a char — Helix never shows a zero-width cursor.
    fn normalize(&mut self) {
        let n = self.len();
        self.range.anchor = self.range.anchor.min(n);
        self.range.head = self.range.head.min(n);
        if !self.mode.typing() && self.range.is_empty() && self.range.head < n {
            self.range.head += 1;
        }
    }

    /// Take an undo step, unless this is more typing in an insert run.
    fn snapshot(&mut self) {
        if self.inserting {
            return;
        }
        self.undo.push(Snapshot {
            text: self.text.clone(),
            range: self.range,
        });
        self.redo.clear();
        if self.undo.len() > 256 {
            self.undo.remove(0);
        }
    }

    /// Replace the char range `a..b` with `s`, leaving the range on it.
    fn splice(&mut self, a: usize, b: usize, s: &str) {
        let (ba, bb) = (self.byte_of(a), self.byte_of(b));
        self.text.replace_range(ba..bb, s);
        let grown = s.chars().count();
        self.range = Range {
            anchor: a,
            head: a + grown,
        };
        self.line_selected = false;
    }

    // ------------------------------------------------------------- modes

    pub fn enter_insert(&mut self, at_end: bool) {
        self.snapshot();
        let at = if at_end {
            self.range.to()
        } else {
            self.range.from()
        };
        self.mode = Mode::insert();
        self.range = Range::point(at);
        self.inserting = true;
        self.line_selected = false;
        self.run.clear();
    }

    /// Put the buffer in `mode`, whatever its name: the transition is
    /// decided by the *kind*, so a script-defined mode gets the same
    /// cursor discipline as the one it is a kind of. Entering a typing
    /// mode from a block mode drops a caret at the start of the
    /// selection, as `i` does; entering a block mode from a typing one
    /// puts the block on the character behind the caret, as `esc` does;
    /// moving between two modes of one kind changes nothing but the name,
    /// which is what lets `!` on an insert-mode draft keep the caret where
    /// it was.
    pub fn enter(&mut self, mode: Mode) {
        match (self.mode.typing(), mode.typing()) {
            (false, true) => self.enter_insert(false),
            (true, false) => self.enter_normal(),
            _ => {}
        }
        self.mode = mode;
        // A select-kind mode entered from normal keeps the range; one
        // entered from insert has just been given a block by
        // `enter_normal`, which is the right start for extending.
    }

    pub fn enter_normal(&mut self) {
        self.mode = Mode::normal();
        self.inserting = false;
        // The run is over: what it typed is what `.` says again.
        if !self.run.is_empty() {
            self.last_insert = std::mem::take(&mut self.run);
        }
        self.run.clear();
        // Insert leaves a caret between chars; normal wants a block on one.
        self.range = Range::point(
            self.range
                .head
                .saturating_sub(usize::from(self.range.head > 0)),
        );
        self.normalize();
    }

    pub fn toggle_select(&mut self) {
        self.mode = if self.mode.kind == Kind::Select {
            Mode::normal()
        } else {
            Mode::select()
        };
    }

    // ------------------------------------------------------------ motion

    /// Put the cursor at `pos`. In a select-kind mode the anchor stays put
    /// and only the head moves — Helix's `put_cursor(.., extend)`, whose
    /// fiddly part is keeping the anchor on the right side of the char it
    /// was covering when the head crosses it.
    fn put_cursor(&mut self, pos: usize) {
        let n = self.len();
        let r = self.range;
        let anchor = if r.head >= r.anchor && pos < r.anchor {
            (r.anchor + 1).min(n)
        } else if r.head < r.anchor && pos >= r.anchor {
            r.anchor.saturating_sub(1)
        } else {
            r.anchor
        };
        self.range = if anchor <= pos {
            Range {
                anchor,
                head: (pos + 1).min(n),
            }
        } else {
            Range { anchor, head: pos }
        };
    }

    /// A motion that lands on a single position: reselect it, or extend.
    fn set_cursor(&mut self, pos: usize) {
        if self.mode.kind == Kind::Select {
            self.put_cursor(pos);
        } else {
            self.range = Range::point(pos);
        }
        self.line_selected = false;
        self.normalize();
    }

    /// A motion that sweeps a range: take it whole, or extend to its head.
    fn set_range(&mut self, r: Range) {
        if self.mode.kind == Kind::Select {
            self.put_cursor(r.cursor());
        } else {
            self.range = r;
        }
        self.line_selected = false;
        self.normalize();
    }

    pub fn move_char_left(&mut self) {
        let c = self.range.cursor();
        self.set_cursor(c.saturating_sub(1));
    }

    pub fn move_char_right(&mut self) {
        let c = self.range.cursor();
        self.set_cursor((c + 1).min(self.len()));
    }

    pub fn move_line_up(&mut self) {
        let col = self.column();
        let ls = self.line_start(self.range.cursor());
        if ls == 0 {
            return self.set_cursor(0);
        }
        let prev = self.line_start(ls - 1);
        self.set_cursor((prev + col).min(ls - 1));
    }

    pub fn move_line_down(&mut self) {
        let col = self.column();
        let le = self.line_end(self.range.cursor());
        if le >= self.len() {
            return self.set_cursor(self.len());
        }
        let next_end = self.line_end(le + 1);
        self.set_cursor((le + 1 + col).min(next_end));
    }

    pub fn move_next_word_start(&mut self, long: bool) {
        let c = self.chars();
        let r = word_move(&c, self.range, Target::NextStart, long);
        self.set_range(r);
    }

    pub fn move_next_word_end(&mut self, long: bool) {
        let c = self.chars();
        let r = word_move(&c, self.range, Target::NextEnd, long);
        self.set_range(r);
    }

    pub fn move_prev_word_start(&mut self, long: bool) {
        let c = self.chars();
        let r = word_move(&c, self.range, Target::PrevStart, long);
        self.set_range(r);
    }

    pub fn goto_line_start(&mut self) {
        let at = self.line_start(self.range.cursor());
        self.set_cursor(at);
    }

    /// The last char of the line, not the newline past it — `A` is how you
    /// get to the position after it.
    pub fn goto_line_end(&mut self) {
        let c = self.range.cursor();
        let (start, end) = (self.line_start(c), self.line_end(c));
        self.set_cursor(end.saturating_sub(usize::from(end > start)));
    }

    /// Helix's `goto_line_end_newline`: the position *past* the last char,
    /// where an insert would land. `$` and `S-right` want this rather than
    /// [`Buffer::goto_line_end`], so that `$i` types at the end of the line.
    pub fn goto_line_end_newline(&mut self) {
        let at = self.line_end(self.range.cursor());
        self.set_cursor(at);
    }

    pub fn goto_first_nonwhitespace(&mut self) {
        let c = self.chars();
        let mut i = self.line_start(self.range.cursor());
        while i < c.len() && c[i] != '\n' && c[i].is_whitespace() {
            i += 1;
        }
        self.set_cursor(i);
    }

    pub fn goto_buffer_start(&mut self) {
        self.set_cursor(0);
    }

    pub fn goto_buffer_end(&mut self) {
        let n = self.len();
        self.set_cursor(n.saturating_sub(1));
    }

    /// Land the cursor on `pos`, the way a letter tag does. This is a
    /// *motion* and not a teleport: it reselects in normal mode and
    /// extends in select mode, like every other one, so `v` then `gw`
    /// covers the ground between here and the tag rather than throwing
    /// the selection away.
    pub fn goto(&mut self, pos: usize) {
        self.set_cursor(pos.min(self.len()));
    }

    /// Every word start, as char indices — what `gw` puts a tag on.
    ///
    /// Only runs of *word* characters count, as in Helix: three-way
    /// classing would tag every bracket in `foo(bar)` as well, and a
    /// screen of tags you have to read past is the thing tags exist to
    /// avoid.
    pub fn word_starts(&self) -> Vec<usize> {
        let c = self.chars();
        (0..c.len())
            .filter(|&i| cat(c[i]) == Cat::Word && (i == 0 || cat(c[i - 1]) != Cat::Word))
            .collect()
    }

    /// `f`/`t`/`F`/`T`: select from the cursor to `target`, the next or
    /// previous occurrence of `ch` on this line. `till` stops one short.
    pub fn find_char(&mut self, ch: char, forward: bool, till: bool) {
        let c = self.chars();
        let from = self.range.cursor();
        let found = if forward {
            let end = self.line_end(from);
            (from + 1..end)
                .find(|&i| c[i] == ch)
                .map(|i| if till { i } else { i + 1 })
        } else {
            let start = self.line_start(from);
            (start..from)
                .rev()
                .find(|&i| c[i] == ch)
                .map(|i| if till { i + 1 } else { i })
        };
        let Some(head) = found else { return };
        let r = if forward {
            Range { anchor: from, head }
        } else {
            Range {
                anchor: (from + 1).min(self.len()),
                head,
            }
        };
        self.set_range(r);
    }

    // ---------------------------------------------------------- selection

    /// `x`: cover whole lines. Again on an already-line-wide range takes
    /// the next line too, which is how Helix grows a line selection.
    pub fn select_line(&mut self) {
        let n = self.len();
        // The phantom line a trailing newline holds open has one char of
        // its own — the break behind it. `x` there takes the line that
        // break ends, which is what lets x d walk a draft upward to the
        // very top instead of stopping at an empty line.
        if self.range.is_empty() && self.range.head == n && n > 0 && self.chars()[n - 1] == '\n' {
            self.range = Range {
                anchor: self.line_start(n - 1),
                head: n,
            };
            self.line_selected = true;
            return;
        }
        let start = self.line_start(self.range.from());
        let end = if self.line_selected {
            let e = self.range.to();
            if e >= n {
                n
            } else {
                (self.line_end(e) + 1).min(n)
            }
        } else {
            (self.line_end(self.range.to().min(n.saturating_sub(1))) + 1).min(n)
        };
        self.range = Range {
            anchor: start,
            head: end,
        };
        self.line_selected = true;
    }

    pub fn select_all(&mut self) {
        self.range = Range {
            anchor: 0,
            head: self.len(),
        };
        self.line_selected = false;
    }

    /// `;`: throw the selection away, keep the cursor.
    pub fn collapse(&mut self) {
        self.range = Range::point(self.range.cursor());
        self.line_selected = false;
        self.normalize();
    }

    // ------------------------------------------------------------ change

    pub fn delete_selection(&mut self) {
        self.delete_selection_impl(true);
    }

    /// `A-d`: the same deletion, and the register left holding whatever
    /// it held — the delete you do to retype, not the delete you paste.
    pub fn delete_selection_noyank(&mut self) {
        self.delete_selection_impl(false);
    }

    fn delete_selection_impl(&mut self, yank: bool) {
        if self.range.is_empty() {
            // The one place normalize leaves a bare point is the very end
            // of the text — the phantom line a trailing newline holds
            // open. `d` there takes the break behind it: the empty line
            // goes and the cursor lands on the one before. A typing
            // mode's empty range is only the caret, and nothing is
            // selected; a bare point after a real char is no line at all.
            if !self.mode.typing() && !self.is_empty() {
                let at = self.range.cursor();
                if self.chars()[at - 1] == '\n' {
                    self.kill(at - 1, at);
                }
            }
            return;
        }
        self.snapshot();
        if yank {
            self.register = self.selection();
        }
        let (a, b) = (self.range.from(), self.range.to());
        let whole_lines = self.line_selected;
        self.splice(a, b, "");
        // A line-wise delete that ended at the tail of the draft leaves
        // the phantom its break was holding open — an empty line nothing
        // selected and nobody wants. The break behind it goes with the
        // line, and the cursor lands on the line above, which is where
        // x d should put you.
        if whole_lines && a == self.len() && self.text.ends_with('\n') {
            self.splice(a - 1, a, "");
        }
        self.range = Range::point(a);
        self.normalize();
    }

    pub fn change_selection(&mut self) {
        self.change_selection_impl(true);
    }

    /// `A-c`: change, and the register keeps its old contents.
    pub fn change_selection_noyank(&mut self) {
        self.change_selection_impl(false);
    }

    fn change_selection_impl(&mut self, yank: bool) {
        self.snapshot();
        self.inserting = true;
        self.run.clear();
        if !self.range.is_empty() {
            if yank {
                self.register = self.selection();
            }
            let (a, b) = (self.range.from(), self.range.to());
            self.splice(a, b, "");
            self.range = Range::point(a);
        }
        self.mode = Mode::insert();
    }

    pub fn yank(&mut self) {
        self.register = self.selection();
    }

    pub fn paste(&mut self, after: bool) {
        if self.register.is_empty() {
            return;
        }
        self.snapshot();
        let at = if after {
            self.range.to()
        } else {
            self.range.from()
        };
        let reg = self.register.clone();
        self.splice(at, at, &reg);
        self.normalize();
    }

    /// `r`: every char in the selection becomes `ch`, newlines excepted.
    pub fn replace_char(&mut self, ch: char) {
        if self.range.is_empty() {
            return;
        }
        self.snapshot();
        let s: String = self
            .selection()
            .chars()
            .map(|c| if c == '\n' { c } else { ch })
            .collect();
        let (a, b) = (self.range.from(), self.range.to());
        self.splice(a, b, &s);
    }

    pub fn switch_case(&mut self) {
        if self.range.is_empty() {
            return;
        }
        self.snapshot();
        let s: String = self
            .selection()
            .chars()
            .flat_map(|c| {
                if c.is_uppercase() {
                    c.to_lowercase().collect::<Vec<_>>()
                } else {
                    c.to_uppercase().collect::<Vec<_>>()
                }
            })
            .collect();
        let (a, b) = (self.range.from(), self.range.to());
        self.splice(a, b, &s);
    }

    /// `` ` `` and `A-\``: the selection folded to one case, every char
    /// the same way — the two one-sided halves of [`Buffer::switch_case`].
    pub fn switch_case_to(&mut self, upper: bool) {
        if self.range.is_empty() {
            return;
        }
        self.snapshot();
        let s: String = self
            .selection()
            .chars()
            .flat_map(|c| {
                if upper {
                    c.to_uppercase().collect::<Vec<_>>()
                } else {
                    c.to_lowercase().collect::<Vec<_>>()
                }
            })
            .collect();
        let (a, b) = (self.range.from(), self.range.to());
        self.splice(a, b, &s);
    }

    /// `R`: the selection becomes the register, and the register stays
    /// the register — unlike `d`, which fills it with what it took, so
    /// `d` here would feed `R` itself and `R R` would do nothing twice.
    pub fn replace_with_yanked(&mut self) {
        if self.register.is_empty() {
            return;
        }
        self.snapshot();
        let reg = self.register.clone();
        let (a, b) = (self.range.from(), self.range.to());
        self.splice(a, b, &reg);
        self.normalize();
    }

    /// `o` / `O`: a blank line after or before this one, then insert.
    pub fn open_line(&mut self, below: bool) {
        self.snapshot();
        let c = self.range.cursor();
        let at = if below {
            self.line_end(c)
        } else {
            self.line_start(c)
        };
        let (text, caret) = if below { ("\n", at + 1) } else { ("\n", at) };
        self.splice(at, at, text);
        self.mode = Mode::insert();
        self.inserting = true;
        self.range = Range::point(caret);
    }

    pub fn insert(&mut self, s: &str) {
        // What typing adds, the run remembers, so leaving insert mode can
        // hand the whole burst to `.`. Kills and motions inside the run
        // are not recorded — `.` replays what was typed, not the journey.
        if self.mode.typing() {
            if self.inserting {
                self.run.push_str(s);
            } else {
                self.run = s.to_string();
            }
        } else if !self.inserting {
            // A run that opened by replacing a selection replays as a
            // replacement; one that opened at a caret replays as typing.
            self.run_replaced = !self.range.is_empty();
        }
        self.snapshot();
        let at = if self.mode.typing() {
            self.range.head
        } else {
            self.range.from()
        };
        let (a, b) = if self.mode.typing() {
            (at, at)
        } else {
            (self.range.from(), self.range.to())
        };
        self.splice(a, b, s);
        self.range = Range::point(a + s.chars().count());
        self.inserting = true;
    }

    pub fn delete_char_backward(&mut self) {
        let at = self.range.head;
        if at == 0 {
            return;
        }
        self.snapshot();
        self.inserting = true;
        self.splice(at - 1, at, "");
        self.range = Range::point(at - 1);
    }

    pub fn delete_char_forward(&mut self) {
        let at = self.range.head;
        if at >= self.len() {
            return;
        }
        self.snapshot();
        self.inserting = true;
        self.splice(at, at + 1, "");
        self.range = Range::point(at);
    }

    // --------------------------------------------------------- the kills
    //
    // Readline's four, which every unix text field has: `C-u` back to
    // the start of the line, `C-k` on to its end, `C-w` the word behind
    // the caret, `A-d` the one in front of it. They are *changes* like
    // any other and fill the register, so `C-y` puts back what the last
    // one took — readline's kill ring, in the one slot this buffer has.
    //
    // Only `C-w` classes by whitespace alone. That is readline's own
    // split (`unix-word-rubout`), and it is the one worth keeping: a
    // caret after `crates/tui/src/edit.rs` is nearly always somewhere a
    // path was typed wrong, and a `C-w` that stopped at the first slash
    // would be four presses instead of one. The forward kill uses the
    // same three-way classing `w` does, because that is the motion it
    // undoes.

    /// `C-u`: from the start of the line to the caret. A caret already at
    /// the start takes the newline behind it instead — readline's own
    /// behaviour — so a held `C-u` walks up a multi-line draft line by
    /// line rather than stopping dead at the top of one.
    pub fn delete_to_line_start(&mut self) {
        let at = self.range.cursor();
        let start = self.line_start(at);
        if start == at {
            if at > 0 {
                self.kill(at - 1, at);
            }
            return;
        }
        self.kill(start, at);
    }

    /// `C-k`: from the caret to the end of the line. A caret already at
    /// the end takes the newline — again readline's own behaviour, and
    /// what lets `C-k` `C-k` empty a line out of a draft instead of
    /// leaving the break behind as a blank line the next `j` trips on —
    /// and a trailing empty line goes whole, both breaks with it.
    ///
    /// Both kills work from the char the cursor *covers*, not the slot
    /// past it. In a typing mode the two are the same; in a block mode
    /// the slot past it is the next line's first char, and a kill from
    /// there would reach over the very break the cursor is sitting on —
    /// `D` on an empty line would wipe the line below it instead of
    /// taking the empty line away.
    pub fn delete_to_line_end(&mut self) {
        let at = self.range.cursor();
        // A bare point at the very end is the phantom trailing line; the
        // break behind it is its whole content, and `d`'s rule applies.
        if at == self.len() && at > 0 {
            if self.chars()[at - 1] == '\n' {
                self.kill(at - 1, at);
            }
            return;
        }
        let end = self.line_end(at);
        if end == at {
            if self.chars().get(at) == Some(&'\n') {
                if at + 1 == self.len() && at > 0 {
                    self.kill(at - 1, at + 1);
                } else {
                    self.kill(at, at + 1);
                }
            }
            return;
        }
        self.kill(at, end);
    }

    /// `C`: from the block to the end of the line is gone, and insert
    /// begins where it stood — `D`'s twin, the way `c` is `d`'s. The
    /// line's tail is in the register, as the kills fill it.
    pub fn change_to_line_end(&mut self) {
        self.delete_to_line_end();
        self.run.clear();
        self.mode = Mode::insert();
        let at = self.range.cursor();
        self.range = Range::point(at);
        self.normalize();
    }

    /// `C-w`: back over any whitespace, then back over the run of
    /// non-whitespace behind it.
    pub fn delete_word_backward(&mut self) {
        let c = self.chars();
        let at = self.range.head.min(c.len());
        let mut i = at;
        while i > 0 && c[i - 1].is_whitespace() && c[i - 1] != '\n' {
            i -= 1;
        }
        while i > 0 && !c[i - 1].is_whitespace() {
            i -= 1;
        }
        // Nothing but the line break behind the caret: take that, so a
        // held `C-w` walks back through a multi-line draft rather than
        // stopping dead at the top of a line.
        if i == at && i > 0 {
            i -= 1;
        }
        self.kill(i, at);
    }

    /// `A-d`: forward over any whitespace, then over the run of one
    /// class that follows — the ground `w` covers.
    pub fn delete_word_forward(&mut self) {
        let c = self.chars();
        let n = c.len();
        let at = self.range.head.min(n);
        let mut i = at;
        while i < n && c[i].is_whitespace() && c[i] != '\n' {
            i += 1;
        }
        if i < n && c[i] == '\n' {
            i += 1;
        } else {
            let class = if i < n { cat(c[i]) } else { Cat::Space };
            while i < n && cat(c[i]) == class {
                i += 1;
            }
        }
        self.kill(at, i);
    }

    /// Take `a..b` out, into the register. Folded into the insert run in
    /// progress, exactly as a backspace is: a kill in the middle of
    /// typing a line is part of typing that line, and `u` should give
    /// the whole line back rather than replay it a kill at a time.
    fn kill(&mut self, a: usize, b: usize) {
        if a >= b {
            return;
        }
        self.snapshot();
        self.inserting = true;
        self.register = self.chars()[a..b].iter().collect();
        self.splice(a, b, "");
        self.range = Range::point(a);
        self.normalize();
    }

    /// `C-t`: readline's transpose-chars. The character behind the caret
    /// is dragged forward over the one at it, and the caret goes with
    /// them; at the end of the buffer the last two characters simply
    /// swap, which is the same drag seen from the far bank. Nothing
    /// behind the caret means nothing to drag, and the key says nothing
    /// back. Each drag is its own undo step even in the middle of a burst
    /// of typing — a transpose is a thought about the line, not a letter
    /// of it — which is why the run is closed before the step is taken.
    pub fn transpose_chars(&mut self) {
        let n = self.len();
        let at = self.range.head.min(n);
        let (a, b, caret) = if at == n && n >= 2 {
            (n - 2, n - 1, n)
        } else if at > 0 && at < n {
            (at - 1, at, at + 1)
        } else {
            return;
        };
        self.inserting = false;
        self.snapshot();
        let c = self.chars();
        let swapped: String = [c[b], c[a]].iter().collect();
        self.splice(a, b + 1, &swapped);
        self.range = Range::point(caret);
        self.normalize();
    }

    pub fn undo(&mut self) {
        self.inserting = false;
        let Some(s) = self.undo.pop() else { return };
        self.redo.push(Snapshot {
            text: self.text.clone(),
            range: self.range,
        });
        self.text = s.text;
        self.range = s.range;
        self.normalize();
    }

    /// Insert-mode `C-s`: close the run here, so what is typed next undoes
    /// on its own. Nothing happens outside a run — there is no boundary
    /// to cut at.
    pub fn checkpoint(&mut self) {
        self.inserting = false;
    }

    /// `.`: the text the last insert run typed, again — at the caret, the
    /// way `i` would put it there, or over the selection when the run
    /// began by taking one. It is the run's *typing* that repeats: a
    /// motion or a kill made mid-run is not part of the recording.
    pub fn repeat_insert(&mut self) {
        if self.last_insert.is_empty() {
            return;
        }
        self.snapshot();
        let s = self.last_insert.clone();
        let (a, b) = if self.mode.typing() {
            (self.range.head, self.range.head)
        } else if self.run_replaced {
            (self.range.from(), self.range.to())
        } else {
            (self.range.from(), self.range.from())
        };
        self.splice(a, b, &s);
        self.inserting = false;
    }

    pub fn redo(&mut self) {
        self.inserting = false;
        let Some(s) = self.redo.pop() else { return };
        self.undo.push(Snapshot {
            text: self.text.clone(),
            range: self.range,
        });
        self.text = s.text;
        self.range = s.range;
        self.normalize();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.range = Range::default();
        self.inserting = false;
        self.line_selected = false;
        self.run.clear();
    }

    /// Submit: hand back the text, remember it, and leave an empty buffer.
    pub fn take(&mut self) -> String {
        let t = std::mem::take(&mut self.text);
        self.range = Range::default();
        self.undo.clear();
        self.redo.clear();
        self.inserting = false;
        self.line_selected = false;
        self.run.clear();
        if !t.trim().is_empty() {
            self.history.push(t.clone());
        }
        self.history_at = None;
        t
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let at = match self.history_at {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_at = Some(at);
        self.set(self.history[at].clone());
    }

    pub fn history_next(&mut self) {
        let Some(i) = self.history_at else { return };
        if i + 1 >= self.history.len() {
            self.history_at = None;
            self.clear();
        } else {
            self.history_at = Some(i + 1);
            self.set(self.history[i + 1].clone());
        }
    }

    fn set(&mut self, t: String) {
        self.text = t;
        self.range = Range::point(self.len());
        self.inserting = false;
        self.line_selected = false;
        self.run.clear();
        self.normalize();
    }

    /// The whole text, replaced in one undo step, caret at the end. This
    /// is what coming back from the operator's editor does — see
    /// [`crate::editor`] — and nothing else should want it: a buffer is
    /// edited in place, or it is a different buffer.
    pub fn set_text(&mut self, s: &str) {
        self.inserting = false;
        self.snapshot();
        self.text = s.to_string();
        self.range = Range::point(self.len());
        self.line_selected = false;
        self.normalize();
    }

    // ------------------------------------------------------------- lines

    /// Char index of the first char on the line holding `ci`.
    pub fn line_start(&self, ci: usize) -> usize {
        let c = self.chars();
        let mut i = ci.min(c.len());
        while i > 0 && c[i - 1] != '\n' {
            i -= 1;
        }
        i
    }

    /// Char index of the newline ending the line holding `ci`, or the end.
    pub fn line_end(&self, ci: usize) -> usize {
        let c = self.chars();
        let mut i = ci.min(c.len());
        while i < c.len() && c[i] != '\n' {
            i += 1;
        }
        i
    }

    fn column(&self) -> usize {
        let c = self.range.cursor();
        c - self.line_start(c)
    }

    // -------------------------------------------------- goto with a count
    //
    // The raw count reaches these as 0 when no digits were typed, which is
    // what separates `G` (the last line) from `3G` (the third).

    /// `G`: line `n` one-based, clamped; no count is the last line, its
    /// first char. `gg` goes through the same door with its own default.
    pub fn goto_line(&mut self, n: usize) {
        if n == 0 {
            let end = self.line_end(self.len());
            let start = self.line_start(end.saturating_sub(usize::from(end > 0)));
            return self.set_cursor(start);
        }
        let mut at = 0;
        let mut line = 1;
        while line < n {
            let end = self.line_end(at);
            if end >= self.len() {
                break;
            }
            at = end + 1;
            line += 1;
        }
        self.set_cursor(at);
    }

    /// `g|`: column `n` of the current line, clamped to its end; no count
    /// is the start of the line, which is `0`'s job — the count is what
    /// makes this more than a second spelling of it.
    pub fn goto_column(&mut self, n: usize) {
        let c = self.range.cursor();
        let (start, end) = (self.line_start(c), self.line_end(c));
        self.set_cursor((start + n).min(end));
    }

    /// Is the line under `at` blank — empty, or nothing but whitespace?
    fn line_blank(&self, at: usize) -> bool {
        let c = self.chars();
        let mut i = self.line_start(at);
        while i < c.len() && c[i] != '\n' {
            if !c[i].is_whitespace() {
                return false;
            }
            i += 1;
        }
        true
    }

    /// `]p`: the start of the next blank line, or the last char of the
    /// text — the paragraph break is the one unit a draft has that is
    /// bigger than a line.
    pub fn goto_next_paragraph(&mut self) {
        let n = self.len();
        let mut at = self.line_end(self.range.cursor()) + 1;
        while at < n {
            if self.line_blank(at) {
                return self.set_cursor(at);
            }
            at = self.line_end(at) + 1;
        }
        self.set_cursor(n.saturating_sub(1));
    }

    /// `[p`: the start of the previous blank line, or the top of the text.
    pub fn goto_prev_paragraph(&mut self) {
        let mut at = self.line_start(self.range.cursor());
        while at > 0 {
            let prev = self.line_start(at - 1);
            if self.line_blank(prev) {
                return self.set_cursor(prev);
            }
            at = prev;
        }
        self.set_cursor(0);
    }

    /// `]space` / `[space`: a blank line below or above this one, the
    /// cursor staying where it was. Making room is not composing, so
    /// this does not enter insert mode the way `o` does.
    pub fn add_newline(&mut self, below: bool) {
        self.snapshot();
        let c = self.range.cursor();
        let at = if below {
            self.line_end(c)
        } else {
            self.line_start(c)
        };
        self.splice(at, at, "\n");
        self.set_cursor(if below { c } else { c + 1 });
    }

    // ------------------------------------------------- the selection, again

    /// `A-;`: trade the anchor for the cursor, so `;` collapses onto the
    /// end the motion started from rather than the end it finished at.
    pub fn flip_selection(&mut self) {
        std::mem::swap(&mut self.range.anchor, &mut self.range.head);
        self.normalize();
    }

    /// `X`: cover whole lines, both ends, the trailing newline with them.
    /// A selection already wider than a line is snapped out to the lines
    /// it touches, not shrunk to them.
    pub fn extend_to_line_bounds(&mut self) {
        let from = self.range.from();
        let to = self.range.to().max(from + 1);
        let start = self.line_start(from);
        let end = (self.line_end(to.saturating_sub(1)) + 1).min(self.len());
        self.range = Range {
            anchor: start,
            head: end,
        };
        self.line_selected = true;
        self.normalize();
    }

    /// `A-x`: snap a selection to the whole lines it stands on. A range
    /// inside one line is left alone — nothing there to snap to. Across
    /// lines, a partial first line goes and a partial last line is taken
    /// whole.
    pub fn shrink_to_line_bounds(&mut self) {
        let (from, to) = (self.range.from(), self.range.to());
        if to <= from {
            return;
        }
        if self.line_start(from) == self.line_start(to.saturating_sub(1)) {
            return;
        }
        let c = self.chars();
        let n = c.len();
        let start = if from == 0 || c[from - 1] == '\n' {
            from
        } else {
            (self.line_end(from - 1) + 1).min(n)
        };
        let end = if to == n || c[to - 1] == '\n' {
            to
        } else {
            (self.line_end(to - 1) + 1).min(n)
        };
        if start < end {
            self.range = Range {
                anchor: start,
                head: end,
            };
            self.line_selected = true;
            self.normalize();
        }
    }

    // ------------------------------------------------------ line surgery

    /// `J`: fold the lines the selection covers into one, each break
    /// becoming a single space. Whitespace hugging a break goes with the
    /// break; the count is how many breaks to fold, so `3J` on a line
    /// takes the two below it into it.
    pub fn join_selections(&mut self, count: usize) {
        for _ in 0..count.max(1) {
            self.join_once();
        }
    }

    fn join_once(&mut self) {
        self.extend_to_line_bounds();
        let c = self.chars();
        let (from, to) = (self.range.from(), self.range.to());
        let Some(pos) = (from..to).find(|&i| c[i] == '\n') else {
            return;
        };
        // The break, the spaces and tabs before it, and the indentation
        // after it, all become the one space.
        let mut a = pos;
        while a > from && c[a - 1] != '\n' && c[a - 1].is_whitespace() {
            a -= 1;
        }
        let mut b = pos + 1;
        while b < to && (c[b] == ' ' || c[b] == '\t') {
            b += 1;
        }
        self.snapshot();
        self.splice(a, b, " ");
        self.normalize();
    }

    /// `>`: two spaces on every line the selection touches.
    pub fn indent(&mut self) {
        self.shift_lines("  ");
    }

    /// `<`: the same two spaces off them, or a tab, as far as they are
    /// there.
    pub fn unindent(&mut self) {
        self.shift_lines("");
    }

    fn shift_lines(&mut self, add: &str) {
        self.extend_to_line_bounds();
        let (from, to) = (self.range.from(), self.range.to());
        let c = self.chars();
        let mut starts = vec![from];
        let mut i = from;
        while i < to {
            if c[i] == '\n' && i + 1 < to {
                starts.push(i + 1);
            }
            i += 1;
        }
        self.snapshot();
        let mut drift: isize = 0;
        for s in starts {
            let at = (s as isize + drift) as usize;
            if !add.is_empty() {
                self.splice(at, at, add);
                drift += add.chars().count() as isize;
            } else {
                let cc = self.chars();
                let end = self.line_end(at);
                let mut i = at;
                if cc.get(i) == Some(&'\t') {
                    i += 1;
                } else {
                    while i < end && cc[i] == ' ' && i - at < 2 {
                        i += 1;
                    }
                }
                if i > at {
                    self.splice(at, i, "");
                    drift -= (i - at) as isize;
                }
            }
        }
        // The whole shifted region stays selected, not just the line the
        // last shift landed on.
        let end = ((to as isize + drift).max(from as isize)) as usize;
        self.range = Range {
            anchor: from,
            head: end.min(self.len()),
        };
        self.line_selected = true;
        self.normalize();
    }

    /// `C-a`/`C-x`: the number on or after the cursor moves by `by`.
    /// Decimal, on the line the cursor is on, an unattached minus in
    /// front of it included — nothing here pretends to know about
    /// radix prefixes. The cursor ends on the number it made.
    pub fn bump_number(&mut self, by: i64) {
        let c = self.chars();
        let cur = self.range.cursor();
        let line_end = self.line_end(cur);
        let Some(mut start) = (cur..line_end).find(|&i| c[i].is_ascii_digit()) else {
            return;
        };
        // The cursor inside a number is on that number, not on its tail.
        while start > 0 && c[start - 1].is_ascii_digit() {
            start -= 1;
        }
        let mut end = start;
        while end < line_end && c[end].is_ascii_digit() {
            end += 1;
        }
        if start > 0 && c[start - 1] == '-' {
            start -= 1;
        }
        let text: String = c[start..end].iter().collect();
        let Ok(value) = text.parse::<i64>() else {
            return;
        };
        let made = value.saturating_add(by).to_string();
        self.snapshot();
        self.splice(start, end, &made);
        self.normalize();
    }

    // ---------------------------------------------- objects and surrounds
    //
    // Plain scanning. A draft has no tree-sitter to ask, and the pairs it
    // actually holds — brackets around a path, quotes around a phrase —
    // are the ones a bracket scan answers honestly.

    /// `mm`: the bracket matching the one under the cursor, through any
    /// nesting of its own kind on the way. Not on a bracket, nothing
    /// happens.
    pub fn match_bracket(&mut self) {
        const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
        let c = self.chars();
        let at = self.range.cursor();
        let Some(&cur) = c.get(at) else { return };
        let (open, close, back) = if let Some(&(o, cl)) = PAIRS.iter().find(|&&(o, _)| o == cur) {
            (o, cl, false)
        } else if let Some(&(o, cl)) = PAIRS.iter().find(|&&(_, cl)| cl == cur) {
            (o, cl, true)
        } else {
            return;
        };
        let mut depth = 0usize;
        if back {
            for i in (0..at).rev() {
                if c[i] == close {
                    depth += 1;
                } else if c[i] == open {
                    if depth == 0 {
                        return self.set_cursor(i);
                    }
                    depth -= 1;
                }
            }
        } else {
            let mut i = at + 1;
            while i < c.len() {
                if c[i] == open {
                    depth += 1;
                } else if c[i] == close {
                    if depth == 0 {
                        return self.set_cursor(i);
                    }
                    depth -= 1;
                }
                i += 1;
            }
        }
    }

    /// `mi`/`ma`: the object under the cursor. A letter means a word —
    /// lowercase word, uppercase WORD — and anything else means the
    /// nearest quote or bracket pair of that character around the
    /// cursor. Nothing found, nothing selected.
    pub fn select_object(&mut self, inner: bool, ch: char) {
        match ch {
            'w' | 'W' => self.select_word_run(inner, ch == 'W'),
            '"' | '\'' | '`' => self.select_quote(inner, ch),
            _ => self.select_pair(inner, ch),
        }
    }

    /// The run of one word class under the cursor, whitespace included —
    /// a cursor in a gap is on the gap. *Around* adds the whitespace
    /// behind the run, the break excepted: that is what makes `ma w`
    /// twice in a row walk a sentence word by word without merging the
    /// spaces.
    fn select_word_run(&mut self, inner: bool, long: bool) {
        let c = self.chars();
        let n = c.len();
        let at = self.range.cursor();
        let class = |x: char| if long { long_cat(x) } else { cat(x) };
        let Some(&here) = c.get(at) else { return };
        let k = class(here);
        let mut a = at;
        while a > 0 && class(c[a - 1]) == k {
            a -= 1;
        }
        let mut b = at + 1;
        while b < n && class(c[b]) == k {
            b += 1;
        }
        if !inner {
            while b < n && c[b].is_whitespace() && c[b] != '\n' {
                b += 1;
            }
        }
        self.range = Range { anchor: a, head: b };
        self.line_selected = false;
        self.normalize();
    }

    /// The quoted run around the cursor, on the line it is on. On the
    /// quote itself the pair it opens comes first, the pair it closes
    /// second; off it, an odd number of quotes behind means the cursor
    /// is inside one, and an even number means the next pair out is the
    /// one wanted.
    fn select_quote(&mut self, inner: bool, q: char) {
        if let Some((open, close)) = self.quote_pair(q) {
            self.take_region(inner, open, close);
        }
    }

    /// The quote pair around the cursor, on the line it is on — the
    /// heuristic is [`Buffer::select_quote`]'s.
    fn quote_pair(&self, q: char) -> Option<(usize, usize)> {
        let c = self.chars();
        let at = self.range.cursor();
        let line_start = self.line_start(at);
        let line_end = self.line_end(at);
        if c.get(at) == Some(&q) {
            // On the quote: the pair it opens, or the one it closes.
            return match (at + 1..line_end).find(|&i| c[i] == q) {
                Some(close) => Some((at, close)),
                None => (line_start..at).rev().find(|&i| c[i] == q).map(|open| (open, at)),
            };
        }
        let behind = (line_start..at).filter(|&i| c[i] == q).count();
        if behind % 2 == 1 {
            let open = (line_start..at).rev().find(|&i| c[i] == q)?;
            let close = (at + 1..line_end).find(|&i| c[i] == q)?;
            Some((open, close))
        } else {
            let open = (at..line_end).find(|&i| c[i] == q)?;
            let close = (open + 1..line_end).find(|&i| c[i] == q)?;
            Some((open, close))
        }
    }

    /// The nearest pair of one bracket kind around the cursor. On a
    /// delimiter of the kind, that is the pair; off one, the scan walks
    /// out from the cursor and the first opener whose match reaches past
    /// it is the innermost one that encloses.
    fn select_pair(&mut self, inner: bool, ch: char) {
        const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
        let Some(&(open, close)) = PAIRS.iter().find(|&&(o, cl)| o == ch || cl == ch) else {
            return;
        };
        let c = self.chars();
        let at = self.range.cursor();
        if c.get(at) == Some(&open)
            && let Some(e) = self.match_forward(at, open, close)
        {
            return self.take_region(inner, at, e);
        }
        if c.get(at) == Some(&close)
            && let Some(a) = self.match_backward(at, open, close)
        {
            return self.take_region(inner, a, at);
        }
        let mut depth = 0usize;
        let mut i = at;
        while i > 0 {
            i -= 1;
            if c[i] == close {
                depth += 1;
            } else if c[i] == open {
                if depth == 0
                    && let Some(e) = self.match_forward(i, open, close)
                    && e >= at
                {
                    return self.take_region(inner, i, e);
                }
                depth = depth.saturating_sub(1);
            }
        }
    }

    fn take_region(&mut self, inner: bool, open: usize, close: usize) {
        let (a, e) = if inner { (open + 1, close) } else { (open, close + 1) };
        self.range = Range {
            anchor: a,
            head: e.min(self.len()),
        };
        self.line_selected = false;
        self.normalize();
    }

    /// The match of the bracket at `at`, scanning forward through nesting.
    /// A self-paired character (a quote asked as a bracket) matches its
    /// next occurrence.
    fn match_forward(&self, at: usize, open: char, close: char) -> Option<usize> {
        let c = self.chars();
        if open == close {
            return (at + 1..c.len()).find(|&i| c[i] == close);
        }
        let mut depth = 0usize;
        let mut i = at + 1;
        while i < c.len() {
            if c[i] == open {
                depth += 1;
            } else if c[i] == close {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            i += 1;
        }
        None
    }

    /// The match of the bracket at `at`, scanning backward through nesting.
    fn match_backward(&self, at: usize, open: char, close: char) -> Option<usize> {
        let c = self.chars();
        if open == close {
            return (0..at).rev().find(|&i| c[i] == open);
        }
        let mut depth = 0usize;
        for i in (0..at).rev() {
            if c[i] == close {
                depth += 1;
            } else if c[i] == open {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
        }
        None
    }

    /// `ms`: wrap the selection in `ch`. A bracket pairs with its twin;
    /// anything else — a quote, a letter — goes on both sides. The
    /// selection stays on the text it wrapped, so `md` undoes it in one.
    pub fn surround_add(&mut self, ch: char) {
        if self.range.is_empty() {
            return;
        }
        let (open, close) = pair_of(ch);
        self.snapshot();
        let text = self.selection();
        let a = self.range.from();
        self.splice(a, self.range.to(), &format!("{open}{text}{close}"));
        self.range = Range {
            anchor: a + 1,
            head: a + 1 + text.chars().count(),
        };
        self.normalize();
    }

    /// `md`: the nearest pair of that character around the selection,
    /// delimiters and all, gone. The selection stays on what it held.
    pub fn surround_delete(&mut self, ch: char) {
        let Some((a, e)) = self.enclosing_pair(ch) else {
            return;
        };
        self.snapshot();
        // The closer first, so the opener's index survives it.
        self.splice(e, e + 1, "");
        self.splice(a, a + 1, "");
        self.range = Range {
            anchor: a,
            head: e - 1,
        };
        self.normalize();
    }

    /// `mr`: the nearest pair of one character around the selection
    /// becomes a pair of another. The two chars ride in as one argument —
    /// `mr(` says "whatever brackets me, make it parens".
    pub fn surround_replace(&mut self, from: char, to: char) {
        let Some((a, e)) = self.enclosing_pair(from) else {
            return;
        };
        let (open, close) = pair_of(to);
        self.snapshot();
        self.splice(e, e + 1, &close.to_string());
        self.splice(a, a + 1, &open.to_string());
        self.range = Range {
            anchor: a + 1,
            head: e,
        };
        self.normalize();
    }

    /// The innermost pair of one character that encloses the selection:
    /// opener strictly before it, match strictly past it.
    fn enclosing_pair(&self, ch: char) -> Option<(usize, usize)> {
        let (open, close) = pair_of(ch);
        let c = self.chars();
        let at = self.range.from();
        let beyond = self.range.to();
        let mut depth = 0usize;
        let mut i = at;
        while i > 0 {
            i -= 1;
            // Open first: when the pair is one char on both sides, a
            // sighting of it is always an opener here.
            if c[i] == open {
                if depth == 0
                    && let Some(e) = self.match_forward(i, open, close)
                    && e >= beyond
                {
                    return Some((i, e));
                }
                depth = depth.saturating_sub(1);
            } else if c[i] == close {
                depth += 1;
            }
        }
        None
    }
}

/// The two halves of a surround character: brackets pair, everything
/// else is its own twin.
fn pair_of(ch: char) -> (char, char) {
    match ch {
        '(' => ('(', ')'),
        ')' => ('(', ')'),
        '[' => ('[', ']'),
        ']' => ('[', ']'),
        '{' => ('{', '}'),
        '}' => ('{', '}'),
        other => (other, other),
    }
}

// ------------------------------------------------------------- word motion
//
// This is Helix's `range_to_target` scan rather than something equivalent
// -looking. The subtlety it exists for is the anchor: a word motion drags
// the anchor forward past a boundary hit on the very first step, which is
// why `w` twice on "hello brave" selects "brave " and not " brave".

#[derive(Clone, Copy, PartialEq)]
enum Target {
    NextStart,
    NextEnd,
    PrevStart,
}

fn class(c: char, long: bool) -> Cat {
    if long { long_cat(c) } else { cat(c) }
}

fn boundary(a: char, b: char, long: bool) -> bool {
    class(a, long) != class(b, long)
}

/// Whether the step from `prev` to `next` is the target we are hunting.
/// The two shapes differ in which side is allowed to be whitespace, which
/// is what separates "start of a word" from "end of one".
fn reached(target: Target, prev: char, next: char, long: bool) -> bool {
    match target {
        Target::NextStart => boundary(prev, next, long) && (next == '\n' || !next.is_whitespace()),
        // Travelling backwards swaps which of the pair leads, so the
        // condition is the mirror of NextStart's, not a copy of it.
        Target::NextEnd | Target::PrevStart => {
            boundary(prev, next, long) && (next == '\n' || !prev.is_whitespace())
        }
    }
}

fn word_move(c: &[char], range: Range, target: Target, long: bool) -> Range {
    let n = c.len();
    let back = target == Target::PrevStart;
    if (!back && range.head >= n) || (back && range.head == 0) {
        return range;
    }
    // Put the anchor on the char the block cursor covers and the head one
    // step ahead of it, in the direction of travel: the incoming anchor is
    // irrelevant to where a motion lands, only to where it starts.
    let start = if back {
        if range.anchor < range.head {
            Range {
                anchor: range.head,
                head: range.head - 1,
            }
        } else {
            Range {
                anchor: (range.head + 1).min(n),
                head: range.head,
            }
        }
    } else if range.anchor < range.head {
        Range {
            anchor: range.head - 1,
            head: range.head,
        }
    } else {
        Range {
            anchor: range.head,
            head: (range.head + 1).min(n),
        }
    };

    let (mut anchor, mut head) = (start.anchor, start.head);
    let head_start = head;
    let mut prev = if back {
        if head < n { c[head] } else { ' ' }
    } else if head > 0 {
        c[head - 1]
    } else {
        ' '
    };
    loop {
        let next = if back {
            if head == 0 {
                break;
            }
            c[head - 1]
        } else {
            if head >= n {
                break;
            }
            c[head]
        };
        if reached(target, prev, next, long) {
            if head == head_start {
                // A boundary right under the cursor moves the anchor
                // rather than ending the motion.
                anchor = head;
            } else {
                break;
            }
        }
        prev = next;
        if back {
            head -= 1;
        } else {
            head += 1;
        }
    }
    Range { anchor, head }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str) -> Buffer {
        let mut b = Buffer::default();
        b.insert(text);
        b.enter_normal();
        b.goto_buffer_start();
        b
    }

    /// A typing buffer with the caret at the end, which is where the
    /// readline keys are pressed from.
    fn typing(text: &str) -> Buffer {
        let mut b = Buffer::default();
        b.enter(Mode::insert());
        b.insert(text);
        b
    }

    // -------------------------------------------------------- the kills

    #[test]
    fn c_u_takes_the_line_back_to_its_start_and_c_k_takes_it_on_to_the_end() {
        let mut b = typing("git commit -m wip");
        b.delete_to_line_start();
        assert_eq!(b.text(), "");
        assert_eq!(
            b.register, "git commit -m wip",
            "and what it took is pasteable"
        );
        b.paste(false);
        assert_eq!(b.text(), "git commit -m wip", "C-y puts it back");

        let mut b = typing("one two");
        b.goto_line_start();
        b.delete_to_line_end();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn a_kill_that_ran_out_of_line_takes_the_break_with_it() {
        let mut b = typing("first\nsecond");
        b.goto_buffer_start();
        b.delete_to_line_start();
        assert_eq!(
            b.text(),
            "first\nsecond",
            "at the very top there is nothing behind the caret"
        );
        b.delete_to_line_end();
        assert_eq!(b.text(), "\nsecond");
        // The line is gone; the second `C-k` has run out of line, so the
        // break goes with it — the chaining is the point.
        b.delete_to_line_end();
        assert_eq!(b.text(), "second", "C-k C-k empties the line out");

        // And the mirror: `C-u` at the top of a line climbs into the one
        // above rather than stopping dead.
        b.goto_buffer_start();
        b.delete_to_line_start();
        assert_eq!(b.text(), "second", "nothing above yet, and nothing taken");
        b.insert("third\n");
        assert_eq!(b.text(), "third\nsecond");
        // The caret sits at the start of the second line now: the kill
        // takes the break behind it and the lines join.
        b.delete_to_line_start();
        assert_eq!(b.text(), "thirdsecond", "the break went with the kill");
    }

    /// A block cursor on an empty line covers the newline itself. The
    /// kills work from the char *under* the block — from the slot past it
    /// they would reach over the break and take the line below instead,
    /// which is what `D` on an empty line was doing.
    #[test]
    fn a_kill_on_an_empty_line_takes_the_break_the_block_sits_on() {
        let mut b = buf("abc\n\ndef");
        b.goto_line(2);
        b.delete_to_line_end();
        assert_eq!(
            b.text(),
            "abc\ndef",
            "the empty line is gone and the line below came up"
        );
        assert_eq!(b.range.cursor(), 4, "on the `d` that joined");

        // With nothing below, the trailing empty line goes whole.
        let mut b = buf("abc\n\n");
        b.goto_line(2);
        b.delete_to_line_end();
        assert_eq!(b.text(), "abc", "both breaks with it");
        let mut b = buf("abc\n\ndef");
        b.goto_line(2);
        b.goto_column(1);
        b.delete_to_line_start();
        assert_eq!(b.text(), "abc\ndef", "the mirror kills back, from the block");
    }

    #[test]
    fn change_to_line_end_takes_the_line_from_the_block_and_types() {
        let mut b = buf("hello\n\nworld");
        b.goto_line(1);
        b.goto_column(2);
        b.change_to_line_end();
        assert_eq!(b.text(), "he\n\nworld");
        assert!(b.mode.typing(), "insert is on where the line was cut");
        b.insert("X");
        b.enter_normal();
        assert_eq!(b.text(), "heX\n\nworld");
        // And on the empty line: the break under the block, then insert.
        b.goto_line(2);
        b.change_to_line_end();
        assert_eq!(b.text(), "heX\nworld");
        assert!(b.mode.typing());
        b.insert("-> ");
        assert_eq!(b.text(), "heX\n-> world");
    }

    /// The trailing newline holds a phantom line open past the text — the
    /// one place a block range stays bare. `d` there takes the break
    /// behind it: the empty line goes, and the cursor lands on the line
    /// that was before it. Held, it empties trailing blank lines one at
    /// a time; after a full line there is no line to take.
    #[test]
    fn d_on_the_trailing_empty_line_takes_the_break_behind_it() {
        let mut b = buf("abc\n");
        b.goto_line(2);
        assert!(b.range.is_empty(), "the phantom line is the one bare point");
        b.delete_selection();
        assert_eq!(b.text(), "abc");
        assert_eq!(
            b.range.cursor(),
            3,
            "the slot after the `c` — where typing would land"
        );

        let mut b = buf("abc\n\n");
        b.goto_line(3);
        b.delete_selection();
        assert_eq!(b.text(), "abc\n");
        b.delete_selection();
        assert_eq!(b.text(), "abc");

        let mut b = buf("abc");
        b.move_char_right();
        b.move_char_right();
        b.move_char_right();
        b.delete_selection();
        assert_eq!(b.text(), "abc", "no break behind the point: nothing to take");

        let mut b = buf("abc\n");
        b.goto_line(2);
        b.delete_to_line_end();
        assert_eq!(b.text(), "abc", "and D reads the phantom the same way");
    }

    /// Deleting the last line of a draft — the one that ends with the
    /// trailing newline — used to leave the cursor on the phantom line
    /// that newline was holding open, so walking a draft upward cost
    /// x d d x d d. The phantom goes with the line, and x on it takes
    /// the line behind: x d all the way up.
    #[test]
    fn x_d_on_the_last_line_lands_on_the_previous_one() {
        let mut b = buf("abc\ndef\n");
        b.goto_line(2);
        b.select_line();
        assert_eq!(b.selection(), "def\n");
        b.delete_selection();
        assert_eq!(b.text(), "abc", "the phantom went with the line");
        assert_eq!(b.range.cursor(), 3, "the end of the line above");
        b.select_line();
        assert_eq!(b.selection(), "abc", "and x still takes the line behind");
        b.delete_selection();
        assert_eq!(b.text(), "");

        // A trailing blank line goes whole, and the walk continues.
        let mut b = buf("abc\n\n");
        b.goto_line(2);
        b.select_line();
        b.delete_selection();
        assert_eq!(b.text(), "abc");

        // The phantom met cold, with no delete before it: x takes the
        // line the break behind it ends.
        let mut b = buf("abc\n");
        b.goto_line(2);
        b.select_line();
        assert_eq!(b.selection(), "abc\n");
        b.delete_selection();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn c_w_takes_a_whitespace_delimited_word_the_way_readline_does() {
        let mut b = typing("open crates/tui/src/edit.rs");
        b.delete_word_backward();
        assert_eq!(
            b.text(),
            "open ",
            "a path is one word, which is the whole reason for the split"
        );
        b.delete_word_backward();
        assert_eq!(
            b.text(),
            "",
            "the trailing space goes with the word behind it"
        );

        let mut b = typing("a\nb");
        b.delete_word_backward();
        assert_eq!(b.text(), "a\n");
        b.delete_word_backward();
        assert_eq!(
            b.text(),
            "a",
            "and it walks back over the break rather than stopping dead"
        );
    }

    #[test]
    fn a_d_takes_the_word_in_front_by_the_same_classing_w_uses() {
        let mut b = typing("hello brave world");
        b.goto_buffer_start();
        b.delete_word_forward();
        assert_eq!(b.text(), " brave world");
        b.delete_word_forward();
        assert_eq!(
            b.text(),
            " world",
            "the space in front goes with the word after it"
        );
    }

    #[test]
    fn a_kill_at_the_edge_takes_nothing_and_leaves_the_register_alone() {
        let mut b = typing("abc");
        b.register = "kept".into();
        b.delete_to_line_end();
        assert_eq!(b.text(), "abc");
        assert_eq!(b.register, "kept", "an empty kill is not a kill");
        b.goto_line_start();
        b.delete_to_line_start();
        assert_eq!(b.text(), "abc");
        assert_eq!(b.register, "kept");
    }

    #[test]
    fn a_forward_range_of_one_puts_the_cursor_on_the_char_it_covers() {
        let b = buf("abc");
        assert_eq!(b.range, Range { anchor: 0, head: 1 });
        assert_eq!(b.range.cursor(), 0);
        assert_eq!(b.selection(), "a");
    }

    #[test]
    fn w_selects_the_word_it_crosses_and_d_eats_exactly_that() {
        let mut b = buf("hello brave world");
        b.move_next_word_start(false);
        assert_eq!(b.selection(), "hello ");
        b.delete_selection();
        assert_eq!(b.text(), "brave world");
        assert_eq!(b.register, "hello ");
    }

    #[test]
    fn e_lands_on_the_end_of_the_word_and_b_walks_back() {
        let mut b = buf("hello brave world");
        b.move_next_word_end(false);
        assert_eq!(b.selection(), "hello");
        b.move_next_word_start(false);
        b.move_prev_word_start(false);
        assert_eq!(b.range.cursor(), 0);
    }

    #[test]
    fn punctuation_is_its_own_word_unless_the_motion_is_long() {
        let mut b = buf("foo(bar)");
        b.move_next_word_start(false);
        assert_eq!(b.selection(), "foo");
        let mut b = buf("foo(bar) baz");
        b.move_next_word_start(true);
        assert_eq!(b.selection(), "foo(bar) ");
    }

    #[test]
    fn select_mode_extends_where_normal_mode_reselects() {
        let mut b = buf("hello brave world");
        b.move_next_word_start(false);
        b.toggle_select();
        b.move_next_word_start(false);
        assert_eq!(b.selection(), "hello brave ");
        b.toggle_select();
        b.move_next_word_start(false);
        assert_eq!(b.selection(), "world");
    }

    #[test]
    fn find_till_stops_one_short_of_the_char() {
        let mut b = buf("path/to/file");
        b.find_char('/', true, false);
        assert_eq!(b.selection(), "path/");
        let mut b = buf("path/to/file");
        b.find_char('/', true, true);
        assert_eq!(b.selection(), "path");
    }

    #[test]
    fn line_end_and_line_end_newline_differ_by_the_char_you_can_type_on() {
        let mut b = buf("  hello\nnext");
        b.goto_line_end();
        assert_eq!(b.range.cursor(), 6); // the "o"
        b.goto_line_end_newline();
        assert_eq!(b.range.cursor(), 7); // the newline, where `i` types
        // `^` is the hard start of the line; first-non-blank is its own move.
        b.goto_line_start();
        assert_eq!(b.range.cursor(), 0);
        b.goto_first_nonwhitespace();
        assert_eq!(b.range.cursor(), 2);
    }

    #[test]
    fn x_takes_the_line_then_grows_by_one_more() {
        let mut b = buf("one\ntwo\nthree");
        b.select_line();
        assert_eq!(b.selection(), "one\n");
        b.select_line();
        assert_eq!(b.selection(), "one\ntwo\n");
    }

    #[test]
    fn a_burst_of_typing_undoes_as_one_step() {
        let mut b = buf("start");
        b.enter_insert(false);
        b.insert("a");
        b.insert("b");
        b.insert("c");
        assert_eq!(b.text(), "abcstart");
        b.enter_normal();
        b.undo();
        assert_eq!(b.text(), "start");
        b.redo();
        assert_eq!(b.text(), "abcstart");
    }

    #[test]
    fn c_t_drags_the_char_behind_the_caret_forward() {
        // Mid-line: the caret goes with the drag, as readline's does —
        // `a|bcd` becomes `ba|cd`.
        let mut b = typing("abcd");
        b.goto(1);
        b.transpose_chars();
        assert_eq!(b.text(), "bacd");
        assert_eq!(b.range.head, 2, "the caret went with the drag");
        // At the end there is nothing to drag over, so the last two
        // simply swap and the caret stays where it was.
        let mut b = typing("abdc");
        b.transpose_chars();
        assert_eq!(b.text(), "abcd");
        assert_eq!(b.range.head, 4);
        // Nothing behind the caret is nothing to drag, and a one-character
        // buffer has nothing to drag into place either.
        let mut b = typing("abcd");
        b.goto(0);
        b.transpose_chars();
        assert_eq!(b.text(), "abcd");
        let mut b = typing("x");
        b.transpose_chars();
        assert_eq!(b.text(), "x");
        // One keypress, one undo step — like a paste, not like typing.
        let mut b = typing("abcd");
        b.goto(2);
        b.transpose_chars();
        assert_eq!(b.text(), "acbd");
        b.transpose_chars();
        assert_eq!(b.text(), "acdb");
        b.undo();
        assert_eq!(b.text(), "acbd");
        b.undo();
        assert_eq!(b.text(), "abcd");
    }

    #[test]
    fn set_text_replaces_whole_and_undoes_whole() {
        let mut b = typing("the draft so far");
        b.set_text("edited in helix");
        assert_eq!(b.text(), "edited in helix");
        assert_eq!(b.range.head, b.len(), "the caret went to the end");
        b.undo();
        assert_eq!(b.text(), "the draft so far");
        b.redo();
        assert_eq!(b.text(), "edited in helix");
        // The way back from an editor that was handed an empty save.
        let mut b = typing("the draft so far");
        b.set_text("");
        assert!(b.is_empty());
        assert_eq!(b.range.head, 0);
        b.undo();
        assert_eq!(b.text(), "the draft so far");
    }

    #[test]
    fn yank_and_paste_move_text_without_the_system_clipboard() {
        let mut b = buf("hello world");
        b.move_next_word_start(false);
        b.yank();
        b.goto_buffer_end();
        b.paste(true);
        assert_eq!(b.text(), "hello worldhello ");
    }

    #[test]
    fn replace_and_switch_case_act_on_the_whole_selection() {
        let mut b = buf("abc def");
        b.move_next_word_start(false);
        b.switch_case();
        assert_eq!(b.text(), "ABC def");
        // `r` replaces every char the selection covers, the trailing space
        // included — the selection is the unit, not the word inside it.
        b.replace_char('-');
        assert_eq!(b.text(), "----def");
    }

    #[test]
    fn insert_replaces_the_selection_rather_than_growing_beside_it() {
        let mut b = buf("hello world");
        b.select_all();
        b.change_selection();
        assert_eq!(b.text(), "");
        assert_eq!(b.mode, Mode::insert());
    }

    #[test]
    fn the_range_never_escapes_the_text() {
        let mut b = buf("ab");
        for _ in 0..10 {
            b.move_char_right();
        }
        assert!(b.range.to() <= b.len());
        for _ in 0..10 {
            b.move_char_left();
        }
        assert_eq!(b.range.cursor(), 0);
    }

    #[test]
    fn multibyte_text_moves_by_character_not_byte() {
        let mut b = buf("héllo wörld");
        b.move_next_word_start(false);
        assert_eq!(b.selection(), "héllo ");
        b.delete_selection();
        assert_eq!(b.text(), "wörld");
    }

    #[test]
    fn a_typing_mode_of_another_name_edits_exactly_as_insert_does() {
        let mut b = Buffer::default();
        b.enter(Mode::named("dispatch", Kind::Insert));
        assert!(b.mode.is("dispatch"));
        assert!(b.mode.typing(), "keys are text here, as in insert");
        b.insert("read the note");
        assert_eq!(b.text(), "read the note");
        // A caret, not a block on a character — the insert rule, which is
        // `typing()`'s whole job.
        assert!(b.range.is_empty());
        b.delete_char_backward();
        assert_eq!(b.text(), "read the not");
        // And it leaves the way insert does, with the text kept.
        b.enter_normal();
        assert_eq!(b.mode, Mode::normal());
        assert_eq!(b.text(), "read the not");
        assert!(
            !b.range.is_empty(),
            "normal never shows a zero-width cursor"
        );
    }

    #[test]
    fn entering_a_mode_is_decided_by_its_kind_not_its_name() {
        // Typing → typing keeps the caret where it was.
        let mut b = Buffer::default();
        b.enter_insert(false);
        b.insert("abc");
        b.enter(Mode::named("shout", Kind::Insert));
        assert_eq!(b.range, Range::point(3));
        // Typing → block puts a block on the char behind the caret.
        b.enter(Mode::named("review", Kind::Normal));
        assert!(b.mode.is("review"));
        assert_eq!(b.range.cursor(), 2);
        assert!(!b.range.is_empty());
        // Block → select-kind keeps the range and extends from it.
        b.goto_buffer_start();
        b.enter(Mode::named("pick", Kind::Select));
        b.move_char_right();
        assert_eq!(b.selection(), "ab");
        // Block → typing drops a caret at the start of the selection.
        b.enter(Mode::named("dispatch", Kind::Insert));
        assert_eq!(b.range, Range::point(0));
    }

    #[test]
    fn only_the_typing_kind_types() {
        assert!(Kind::Insert.typing());
        assert!(!Kind::Normal.typing());
        assert!(!Kind::Select.typing());
        assert!(Mode::named("dispatch", Kind::Insert).typing());
        assert_eq!(Mode::named("dispatch", Kind::Insert).as_str(), "dispatch");
        assert_eq!(Kind::parse("select"), Some(Kind::Select));
        assert_eq!(Kind::parse("command"), None);
    }

    // ------------------------------------------------- the Helix additions

    #[test]
    fn goto_line_takes_a_one_based_count_and_g_without_one_takes_the_last() {
        let mut b = buf("one\ntwo\nthree");
        b.goto_line(2);
        assert_eq!(b.range.cursor(), 4, "the `t` of two");
        b.goto_line(99);
        assert_eq!(b.range.cursor(), 8, "past the end clamps to the last line");
        b.goto_line(0);
        assert_eq!(b.range.cursor(), 8, "no count is the last line");
        b.goto_line(1);
        assert_eq!(b.range.cursor(), 0);
    }

    #[test]
    fn goto_column_clamps_to_the_line_it_is_on() {
        let mut b = buf("ab\ncd");
        b.goto_line(2);
        b.goto_column(1);
        assert_eq!(b.range.cursor(), 4);
        b.goto_column(99);
        assert_eq!(
            b.range.cursor(),
            5,
            "past the end lands on the slot past the last char, as `$` does"
        );
        b.goto_column(0);
        assert_eq!(b.range.cursor(), 3, "no count is the line start");
    }

    #[test]
    fn paragraphs_land_on_blank_lines_and_stop_at_the_ends() {
        let mut b = buf("one\n\nthree\nfour\n\nsix");
        b.goto_buffer_start();
        b.goto_next_paragraph();
        assert_eq!(b.range.cursor(), 4, "the blank line between one and three");
        b.goto_next_paragraph();
        assert_eq!(b.range.cursor(), 16, "the blank between four and six");
        b.goto_next_paragraph();
        assert_eq!(b.range.cursor(), 19, "no more breaks: the last char");
        b.goto_prev_paragraph();
        assert_eq!(b.range.cursor(), 16);
        b.goto_prev_paragraph();
        assert_eq!(b.range.cursor(), 4);
        b.goto_prev_paragraph();
        assert_eq!(b.range.cursor(), 0, "no break above: the top");
        // A whitespace-only line is a break too.
        let mut b = buf("a\n   \nb");
        b.goto_next_paragraph();
        assert_eq!(b.range.cursor(), 2);
    }

    #[test]
    fn add_newline_makes_room_and_keeps_the_cursor() {
        let mut b = buf("one\ntwo");
        b.goto_line(2);
        b.add_newline(false);
        assert_eq!(b.text(), "one\n\ntwo", "a blank line above");
        assert_eq!(b.range.cursor(), 5, "still on the line that asked");
        let mut b = buf("one\ntwo");
        b.goto_buffer_start();
        b.add_newline(true);
        assert_eq!(b.text(), "one\n\ntwo", "a blank line below");
        assert_eq!(b.range.cursor(), 0);
    }

    #[test]
    fn flip_swaps_which_end_the_cursor_stands_on() {
        let mut b = buf("hello world");
        b.move_next_word_start(false);
        assert_eq!(b.range.cursor(), 5, "the space the word ran into");
        b.flip_selection();
        assert_eq!(b.range.cursor(), 0, "now the `h`");
        assert_eq!(b.selection(), "hello ", "the selection itself unchanged");
    }

    #[test]
    fn line_bounds_extend_and_shrink_snap_to_lines() {
        let mut b = buf("one\ntwo\nthree");
        b.goto_line(2);
        b.move_char_right();
        b.extend_to_line_bounds();
        assert_eq!(b.selection(), "two\n", "the whole line, break with it");
        b.shrink_to_line_bounds();
        assert_eq!(b.selection(), "two\n", "already whole: unchanged");
        let mut b = buf("one\ntwo\nthree");
        b.range = Range {
            anchor: 1,
            head: 6,
        };
        assert_eq!(b.selection(), "ne\ntw", "one line and a bit of the next");
        b.shrink_to_line_bounds();
        assert_eq!(
            b.selection(),
            "two\n",
            "the partial first line went, the last was taken whole"
        );
        let mut b = buf("one\ntwo");
        b.goto_line(2);
        b.move_char_right();
        b.shrink_to_line_bounds();
        assert_eq!(b.selection(), "w", "inside one line: nothing to snap to");
    }

    #[test]
    fn case_folds_one_way_and_replace_keeps_the_register() {
        let mut b = buf("Mixed Case");
        b.select_all();
        b.switch_case_to(true);
        assert_eq!(b.text(), "MIXED CASE");
        b.switch_case_to(false);
        assert_eq!(b.text(), "mixed case");

        let mut b = buf("keep this\ngone");
        b.goto_line(2);
        b.select_line();
        b.delete_selection();
        assert_eq!(b.register, "gone", "d fills the register as always");
        assert_eq!(
            b.text(),
            "keep this",
            "and the phantom the trailing break held goes with the line"
        );
        b.goto_column(5);
        b.select_object(true, 'w');
        b.replace_with_yanked();
        assert_eq!(b.text(), "keep gone", "the word became the register");
        assert_eq!(
            b.register, "gone",
            "and R did not feed itself with what it replaced"
        );
    }

    #[test]
    fn noyank_deletes_leave_the_register_alone() {
        let mut b = buf("hello world");
        b.goto_line_start();
        b.select_line();
        b.delete_selection_noyank();
        assert_eq!(b.text(), "");
        assert_eq!(b.register, "", "nothing was taken into it");

        let mut b = buf("hello world");
        b.move_next_word_start(false);
        b.yank();
        assert_eq!(b.register, "hello ");
        b.select_all();
        b.delete_selection_noyank();
        assert_eq!(b.text(), "");
        assert_eq!(b.register, "hello ", "an older yank survived the delete");
        b.paste(false);
        assert_eq!(b.text(), "hello ", "and C-y still puts it back");
    }

    #[test]
    fn join_folds_breaks_into_spaces_and_takes_the_indent_with_them() {
        let mut b = buf("first\n    second\nthird");
        b.select_all();
        b.join_selections(1);
        assert_eq!(b.text(), "first second\nthird");
        b.join_selections(1);
        assert_eq!(b.text(), "first second third");
        let mut b = buf("tail  \nend");
        b.select_all();
        b.join_selections(0);
        assert_eq!(b.text(), "tail end", "no count is one join, and trailing space goes");
        let mut b = buf("a\nb");
        b.select_all();
        b.join_selections(1);
        assert_eq!(b.text(), "a b", "a break with nothing around it is the space");
    }

    #[test]
    fn indent_and_unindent_move_the_lines_they_touch() {
        let mut b = buf("one\ntwo\nthree");
        b.goto_line(2);
        b.indent();
        assert_eq!(b.text(), "one\n  two\nthree");
        b.indent();
        assert_eq!(b.text(), "one\n    two\nthree", "counts stack");
        b.unindent();
        b.unindent();
        b.unindent();
        assert_eq!(b.text(), "one\ntwo\nthree", "unindent stops at the margin");
        let mut b = buf("one\ntwo\nthree");
        b.select_all();
        b.indent();
        assert_eq!(b.text(), "  one\n  two\n  three", "every line in the range");
        b.unindent();
        assert_eq!(b.text(), "one\ntwo\nthree");
        let mut b = buf("\ta");
        b.select_all();
        b.unindent();
        assert_eq!(b.text(), "a", "a tab goes as one stop");
    }

    #[test]
    fn bump_moves_the_number_on_or_after_the_cursor() {
        let mut b = buf("retry 9 times");
        b.goto_buffer_start();
        b.bump_number(1);
        assert_eq!(b.text(), "retry 10 times", "the next number on the line");
        b.bump_number(-3);
        assert_eq!(b.text(), "retry 7 times");
        let mut b = buf("v-2");
        b.goto_buffer_start();
        b.bump_number(5);
        assert_eq!(b.text(), "v3", "a loose minus rides with the number");
        let mut b = buf("a1b");
        b.goto_buffer_start();
        b.bump_number(10);
        assert_eq!(b.text(), "a11b");
        let mut b = buf("no digits");
        b.goto_buffer_start();
        b.bump_number(1);
        assert_eq!(b.text(), "no digits", "nothing to take: nothing moves");
    }

    #[test]
    fn match_bracket_walks_to_the_twin_through_nesting() {
        let mut b = buf("f(a(b)c)");
        b.goto_column(1);
        b.match_bracket();
        assert_eq!(b.range.cursor(), 7);
        b.match_bracket();
        assert_eq!(b.range.cursor(), 1, "and back again");
        b.goto_column(3);
        b.match_bracket();
        assert_eq!(b.range.cursor(), 5, "the inner pair, not the outer");
        b.goto_column(0);
        b.match_bracket();
        assert_eq!(b.range.cursor(), 0, "not on a bracket: nothing moves");
    }

    #[test]
    fn word_objects_select_the_run_and_around_adds_the_space() {
        let mut b = buf("say hello world");
        b.goto_column(4);
        b.select_object(true, 'w');
        assert_eq!(b.selection(), "hello");
        b.select_object(false, 'w');
        assert_eq!(b.selection(), "hello ", "around takes the space behind");
        let mut b = buf("foo(bar) baz");
        b.goto_column(0);
        b.select_object(true, 'W');
        assert_eq!(
            b.selection(),
            "foo(bar)",
            "WORD runs through the punctuation a word stops at"
        );
        let mut b = buf("two  gaps");
        b.goto_column(3);
        b.select_object(true, 'w');
        assert_eq!(b.selection(), "  ", "a cursor in a gap is on the gap");
    }

    #[test]
    fn quote_objects_find_the_pair_the_cursor_is_inside() {
        let mut b = buf("say \"hello there\" ok");
        b.goto_column(6);
        b.select_object(true, '"');
        assert_eq!(b.selection(), "hello there");
        b.select_object(false, '`');
        assert_eq!(
            b.selection(),
            "hello there",
            "no pair of that kind: the selection stays"
        );
        b.goto_column(4);
        b.select_object(false, '"');
        assert_eq!(
            b.selection(),
            "\"hello there\"",
            "on the quote itself: the pair it opens"
        );
        let mut b = buf("a 'b' c");
        b.goto_column(4);
        b.select_object(true, '\'');
        assert_eq!(b.selection(), "b", "on the closing quote: the pair it closes");
    }

    #[test]
    fn bracket_objects_take_the_innermost_pair_enclosing_the_cursor() {
        let mut b = buf("f(a(b)c)");
        b.goto_column(4);
        b.select_object(true, '(');
        assert_eq!(b.selection(), "b", "innermost first");
        b.goto_column(4);
        b.select_object(false, '(');
        assert_eq!(b.selection(), "(b)", "around the innermost, either half names it");
        b.goto_column(6);
        b.select_object(true, '(');
        assert_eq!(b.selection(), "a(b)c", "outside the inner one: the outer pair");
        b.select_object(true, '[');
        assert_eq!(
            b.selection(),
            "a(b)c",
            "no brackets of that kind: the selection stays"
        );
    }

    #[test]
    fn surround_wraps_and_unwraps_the_selection() {
        let mut b = buf("hello");
        b.select_object(true, 'w');
        b.surround_add('(');
        assert_eq!(b.text(), "(hello)");
        b.surround_delete(')');
        assert_eq!(b.text(), "hello", "and the selection stayed on the word");
        b.surround_add('*');
        assert_eq!(b.text(), "*hello*", "a non-bracket goes on both sides");
        b.surround_replace('*', '"');
        assert_eq!(b.text(), "\"hello\"");
        b.surround_replace('"', '(');
        assert_eq!(b.text(), "(hello)");
        let mut b = buf("f(a b)");
        b.goto_column(2);
        b.select_object(true, '(');
        b.surround_delete('(');
        assert_eq!(
            b.text(),
            "fa b",
            "the pair asked for comes off, the text it held stays"
        );
    }

    #[test]
    fn repeat_insert_replays_the_last_run_of_typing() {
        let mut b = buf("");
        b.enter_insert(false);
        b.insert("y");
        b.insert("o");
        b.enter_normal();
        b.move_char_right();
        b.repeat_insert();
        assert_eq!(b.text(), "yoyo", "the run, typed again at the caret");
        let mut b = buf("a\nb\n");
        b.enter_insert(false);
        b.insert("- ");
        b.enter_normal();
        assert_eq!(b.text(), "- a\nb\n", "the run really did type");
        b.goto_line(2);
        b.repeat_insert();
        assert_eq!(b.text(), "- a\n- b\n");
        b.goto_line(3);
        b.repeat_insert();
        assert_eq!(b.text(), "- a\n- b\n- ", "and on the empty last line");
        let mut b = buf("plain");
        b.repeat_insert();
        assert_eq!(b.text(), "plain", "no run yet, nothing happens");
    }

    #[test]
    fn checkpoint_cuts_the_undo_run_where_it_stands() {
        let mut b = typing("");
        b.insert("one");
        b.checkpoint();
        b.insert("two");
        assert_eq!(b.text(), "onetwo");
        b.undo();
        assert_eq!(b.text(), "one", "the checkpoint is where undo stops");
        b.undo();
        assert_eq!(b.text(), "");
    }

    /// The operator's own chain: `x` `d` takes the line and its break, so
    /// the next `x` takes the *next* line and a held `xd` empties the
    /// draft from the top.
    #[test]
    fn x_then_d_takes_the_break_so_the_chain_walks_on() {
        let mut b = buf("one\ntwo\nthree");
        b.select_line();
        assert_eq!(b.selection(), "one\n");
        b.delete_selection();
        assert_eq!(b.text(), "two\nthree", "no blank line left behind");
        b.select_line();
        assert_eq!(b.selection(), "two\n", "the next line, not the ghost of one");
        b.delete_selection();
        b.select_line();
        b.delete_selection();
        assert_eq!(b.text(), "", "three lines, three xds");
    }
}
