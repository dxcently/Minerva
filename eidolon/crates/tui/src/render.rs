//! Draw a widget tree (plain JSON from the script) with ratatui.
//!
//! ## The vocabulary
//!
//! ```text
//! #{ kind: "column" | "row", children: [ #{ size, child } … ] }
//!     size: "fill" (default) | N (rows/cols) | "min:N" | "pct:N"
//! #{ kind: "panel", title: str | spans, child, fg? }
//! #{ kind: "transcript" }                      the conversation (Rust data)
//! #{ kind: "prompt", title? }                  the input box (Rust editing)
//! #{ kind: "menu" }                            the which-key popup (overlay)
//! #{ kind: "status", left: spans, right: spans }
//! #{ kind: "text", spans | content, align?: "left"|"center"|"right", wrap?: bool }
//! #{ kind: "dialog" }                          overlay when a dialog is pending
//! #{ kind: "empty" }
//! spans: str | [ #{ text, fg?, bg?, bold?, dim?, italic? } … ]
//! ```
//!
//! A `dialog` node takes no space in its parent; it is drawn last, over
//! everything. If the tree has none and a dialog is pending, one is drawn
//! anyway, so a script can never make an approval prompt invisible.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use serde_json::Value;

use std::ops::Range;

use crate::modes::ModeDef;
use crate::state::{Anchor, Detail, DialogKind, Entry, Fold, Level, Selection, UiState};

use crate::theme::Theme;

/// How many rows of a `question`'s answer are on screen at once. The
/// field scrolls past this rather than growing without bound: the box is
/// centred over the transcript, and one that grew to the screen would
/// stop being a dialog.
/// The most rows one option's description may occupy in a `choose` dialog.
/// A paragraph longer than this is cut with an ellipsis: the option still
/// says what it is, and one verbose description cannot starve the others
/// of the screen.
const DESC_ROWS: usize = 4;

pub struct Renderer<'a> {
    pub state: &'a UiState,
    pub link_hits: Vec<crate::vault::Hit>,
    overlays: Vec<Value>,
    dialog_drawn: bool,
    menu_drawn: bool,
    /// Where the caret is, if the prompt was drawn.
    pub cursor: Option<(u16, u16)>,
    /// Whether that caret is a block (normal/select) or a bar.
    pub block_cursor: bool,
    /// Visible transcript lines, for scroll clamping.
    pub transcript_lines: usize,
    /// The bottom visible row's place in its block. Reported every frame
    /// the transcript drew — following or not; [`crate::app`] keeps it
    /// only for a frame that drew scrolled up, and that held copy is
    /// what keeps the stream from moving the view under the reader.
    pub scroll_anchor: Option<Anchor>,
    pub transcript_height: usize,
    /// The width it was wrapped to, which the search needs to work out
    /// where a scroll has to land.
    pub transcript_width: usize,
    /// Where the transcript was drawn, so `gw` can tag the cells in it.
    pub transcript_rect: Rect,
    /// The transcript entries this frame drew, for [`crate::jump`]: the
    /// hit tags are handed out over what is on screen, so the set has to
    /// come back from the draw that decided it.
    pub view_entries: std::ops::Range<usize>,
    /// Every rectangle an overlay took ownership of this frame — each is a
    /// `Clear` site. An image whose box one of these covers is dropped
    /// rather than drawn: see [`Renderer::settle_images`].
    covered: Vec<Rect>,
    /// Where this frame wants pixels painted, in screen coordinates.
    ///
    /// Read by [`crate::app::run`] *after* `terminal.draw` returns, which
    /// is the only moment it can be acted on: ratatui writes cells and a
    /// graphics escape writes pixels over cells, so drawing before the
    /// flush means the flush paints over the picture. Everything the
    /// layout could do about it has been done — the rows are reserved and,
    /// where the picture has not moved, marked skip so the diff leaves
    /// them alone.
    pub images: Vec<crate::image::Placement>,
}

impl<'a> Renderer<'a> {
    pub fn new(state: &'a UiState) -> Self {
        Renderer {
            state,
            link_hits: Vec::new(),
            overlays: Vec::new(),
            dialog_drawn: false,
            menu_drawn: false,
            cursor: None,
            block_cursor: false,
            covered: Vec::new(),
            images: Vec::new(),
            transcript_lines: 0,
            scroll_anchor: None,
            transcript_height: 0,
            transcript_width: 0,
            transcript_rect: Rect::default(),
            view_entries: 0..0,
        }
    }

    pub fn draw(&mut self, f: &mut Frame, tree: &Value) {
        let area = f.area();
        self.node(f, area, tree);
        let overlays = std::mem::take(&mut self.overlays);
        for o in &overlays {
            self.node(f, area, o);
        }
        if self.state.dialog.is_some() && !self.dialog_drawn {
            self.dialog(f, area);
        }
        if self.state.menu.is_some() && !self.menu_drawn {
            self.menu(f, area);
        }
        if let Some(sel) = self.state.selection.filter(|s| !s.is_empty()) {
            invert(f, sel);
        }
        self.settle_images(f);
        if let Some((x, y)) = self.cursor
            && self.state.dialog.is_none()
        {
            self.paint_caret(f, x, y);
        }
    }

    /// Painted into the frame's cells, so a frame can move it no further than it moves any other
    /// cell. Normal modes reverse the cell; typing writes `▏` at the caret column.
    fn paint_caret(&mut self, f: &mut Frame, x: u16, y: u16) {
        let Some(c) = f.buffer_mut().cell_mut((x, y)) else {
            return;
        };
        if self.block_cursor {
            // The block keeps the cell's own colours: reversed, they are
            // what the native block drew.
            let style = c.style().add_modifier(Modifier::REVERSED);
            c.set_style(style);
        } else {
            c.set_symbol("\u{258f}");
            c.set_style(c.style().fg(self.state.theme.colour("cursor")));
        }
    }

    /// Decide, once the whole frame is laid out, which candidate images
    /// are actually drawn and whose cells may be kept out of the diff.
    ///
    /// This cannot happen during the transcript's own pass, and getting
    /// that wrong is visible: the transcript is drawn first and every
    /// overlay after it, so marking a cell `Skip` while laying out the
    /// transcript suppresses whatever a dialog later writes into the same
    /// cell. The model picker came up with its left edge missing and its
    /// rows sitting on the pixels underneath, because those cells were
    /// flagged before the picker existed and the diff then declined to
    /// send them.
    ///
    /// So an image covered by an overlay is **dropped**, not merely
    /// unskipped: its pixels would sit on top of the dialog. Dropping it
    /// also erases it, because a placement that is gone makes the previous
    /// frame's copy stale — the cells are repainted by the frame (sixel)
    /// and the terminal's own copy is deleted (kitty).
    fn settle_images(&mut self, f: &mut Frame) {
        let covered = std::mem::take(&mut self.covered);
        self.images.retain(|p| {
            let (r, b) = (p.col + p.cols, p.row + p.rows);
            !covered
                .iter()
                .any(|c| p.col < c.x + c.width && c.x < r && p.row < c.y + c.height && c.y < b)
        });
        let area = f.area();
        let buf = f.buffer_mut();
        for p in &self.images {
            // A picture that has not moved is still on the screen, so its
            // cells are left out of the diff and it is not painted again —
            // which is what stops it flickering under a streaming reply.
            // One that *has* moved is not skipped: the frame repainting
            // those cells is what rubs out the pixels that were there.
            if !self.state.drawn_images.iter().any(|d| d.same_as(p)) {
                continue;
            }
            for y in p.row..(p.row + p.rows).min(area.y + area.height) {
                for x in p.col..(p.col + p.cols).min(area.x + area.width) {
                    if let Some(c) = buf.cell_mut((x, y)) {
                        c.set_diff_option(ratatui::buffer::CellDiffOption::Skip);
                    }
                }
            }
        }
    }

    fn node(&mut self, f: &mut Frame, area: Rect, n: &Value) {
        match n.get("kind").and_then(Value::as_str).unwrap_or("empty") {
            "column" => self.stack(f, area, n, Direction::Vertical),
            "row" => self.stack(f, area, n, Direction::Horizontal),
            "panel" => {
                let mut block = Block::default().borders(Borders::ALL);
                if let Some(t) = n.get("title") {
                    block = block.title(spans_of(t));
                }
                if let Some(fg) = n.get("fg").and_then(Value::as_str) {
                    block = block.border_style(Style::default().fg(color(fg)));
                }
                let inner = block.inner(area);
                f.render_widget(block, area);
                if let Some(c) = n.get("child") {
                    self.node(f, inner, c);
                }
            }
            "transcript" => self.transcript(f, area),
            "prompt" => self.prompt(f, area, n),
            "status" => {
                // A notice borrows the left half — the whole of it, since
                // a sentence squeezed in beside the badge, the model and
                // the session name is a sentence with no room. The right
                // half stays: it is the gauge, and the notice is not
                // about it. Rust draws it rather than the script, for the
                // reason the search line borrows the prompt's slot: a
                // script that never heard of notices must not lose them.
                // A line break in a notice becomes a dot, since the slot
                // is one row and `:messages` has the whole of it.
                let left = match &self.state.notice {
                    Some(nt) => {
                        let style = match nt.level {
                            Level::Info => {
                                self.state.theme.fg("text").add_modifier(Modifier::ITALIC)
                            }
                            Level::Alert => self.state.theme.fg("error"),
                        };
                        Line::from(Span::styled(
                            format!(" {}", nt.text.replace('\n', " \u{b7} ")),
                            style,
                        ))
                    }
                    None => n.get("left").map(spans_of).unwrap_or_default(),
                };
                // The left half — badge, model, notice — takes its width;
                // the gauge gives way on a narrow row.
                let right = n.get("right").map(spans_of).unwrap_or_default();
                let lw = (left.width() as u16 + 1).min(area.width);
                let [l, r] =
                    Layout::horizontal([Constraint::Length(lw), Constraint::Fill(1)]).areas(area);
                f.render_widget(Paragraph::new(left), l);
                f.render_widget(Paragraph::new(right).alignment(Alignment::Right), r);
            }
            "text" => {
                let line = match n.get("spans") {
                    Some(s) => spans_of(s),
                    None => spans_of(n.get("content").unwrap_or(&Value::Null)),
                };
                let mut p = Paragraph::new(line);
                p = match n.get("align").and_then(Value::as_str) {
                    Some("center") => p.alignment(Alignment::Center),
                    Some("right") => p.alignment(Alignment::Right),
                    _ => p,
                };
                if n.get("wrap").and_then(Value::as_bool).unwrap_or(false) {
                    p = p.wrap(ratatui::widgets::Wrap { trim: false });
                }
                f.render_widget(p, area);
            }
            "menu" if self.state.menu.is_some() => self.menu(f, area),
            "dialog" if self.state.dialog.is_some() => self.dialog(f, area),
            _ => {}
        }
    }

    fn stack(&mut self, f: &mut Frame, area: Rect, n: &Value, dir: Direction) {
        let empty = Vec::new();
        let children = n
            .get("children")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let mut constraints = Vec::new();
        let mut nodes = Vec::new();
        for c in children {
            let (size, child) = match c.get("child") {
                Some(ch) => (c.get("size"), ch),
                None => (None, c),
            };
            // Overlays float over the finished frame rather than taking a
            // row of it. They must be lifted out *before* the constraints
            // are built: left in, each one claims a `Fill(1)` share and
            // silently halves the transcript.
            if matches!(
                child.get("kind").and_then(Value::as_str),
                Some("dialog" | "menu")
            ) {
                self.overlays.push(child.clone());
                continue;
            }
            // `grow` is the one size the layout has to *measure* rather
            // than read. The prompt's height is the operator's to decide
            // — by typing — so the box follows the text between a floor
            // and a ceiling instead of pinning a message to one row.
            // Only a column can answer it, because only there is a
            // child's width the parent's, and the width is what the wrap
            // depends on; in a row it falls back to `fill`.
            let cons = match (dir, grow_bounds(size)) {
                (Direction::Vertical, Some((lo, hi))) => {
                    Constraint::Length(rows_wanted(self.state, child, area.width).clamp(lo, hi))
                }
                _ => constraint(size),
            };
            constraints.push(cons);
            nodes.push(child);
        }
        let areas = Layout::default()
            .direction(dir)
            .constraints(constraints)
            .split(area);
        for (a, c) in areas.iter().zip(nodes) {
            self.node(f, *a, c);
        }
    }

    /// The conversation, anchored to its last line. Blocks are laid out
    /// from the end backwards and the walk stops as soon as enough lines
    /// exist to fill the viewport at the current scroll, so the cost of a
    /// frame follows the window rather than the length of the session.
    ///
    /// A block is usually one entry. The exception is a *run* of tool
    /// calls, which folds to a single summary line ([`group_lines`]) —
    /// which is why the walk takes blocks rather than entries, and why it
    /// runs backwards over runs it has to find the front of.
    fn transcript(&mut self, f: &mut Frame, area: Rect) {
        if self.state.vault.active { self.vault_page(f, area); return; }
        let (visible, owner, total, anchor, offsets) = visible_rows(self.state, area);
        self.transcript_lines = total;
        self.scroll_anchor = anchor;
        self.transcript_height = area.height as usize;
        self.transcript_width = area.width.max(1) as usize;
        self.transcript_rect = area;
        self.view_entries = match (owner.first(), owner.last()) {
            (Some(a), Some(z)) => a.start..z.end,
            _ => 0..0,
        };
        let mut visible = self.focused(self.lit(visible, &owner), &owner);
        if self.state.dialog.is_none() && self.state.jump.is_none() {
            let mut docs = std::collections::HashMap::new();
            for (y, line) in visible.iter_mut().enumerate() {
                let entry = owner[y].start;
                if let Entry::Assistant { text, .. } = &self.state.transcript[entry] {
                    let doc = docs.entry(entry).or_insert_with(|| {
                        let mut doc = crate::markdown::document(text, area.width as usize,
                            &crate::markdown::Palette::from(&self.state.theme));
                        if self.state.record_at(entry).is_some_and(|r| self.state.struck.contains(&r)) {
                            for link in &mut doc.links {
                                for (row, range) in &mut link.runs {
                                    let prefix = if *row == 0 { "⊘ " } else { "  " }.len();
                                    range.start += prefix;
                                    range.end += prefix;
                                }
                            }
                        }
                        doc
                    });
                    let focus = self.state.vault.trace_focus
                        .filter(|(e, _)| self.state.tracing() && *e == entry && self.state.trace.at == entry).map(|(_, i)| i);
                    link_row(line, &doc.links, offsets[y], focus,
                        Rect::new(area.x, area.y + y as u16, area.width, 1), &mut self.link_hits);
                }
            }
        }
        let lines = self.tagged(
            visible,
            &owner,
            area,
        );
        f.render_widget(Paragraph::new(lines), area);
        self.place_images(area, &owner);
    }

    /// The screen tags — `gw`'s words and `gb`'s blocks — written over
    /// the lines they point at.
    ///
    /// *Over*, as everywhere else a tag is drawn: a label that pushed the
    /// text along would reflow the screen the moment the tags came up,
    /// and the whole trick is that the place you were looking at is still
    /// where you were looking. A cell tag is placed by its column, a
    /// block tag at the head of the block's first drawn line — where the
    /// tool arrow is, which is where a tag always goes.
    fn tagged(
        &self,
        lines: Vec<Line<'static>>,
        owner: &[Range<usize>],
        area: Rect,
    ) -> Vec<Line<'static>> {
        let Some(j) = &self.state.jump else {
            return lines;
        };
        if !j.on_screen() {
            return lines;
        }
        let style = self.state.theme.on("tag").add_modifier(Modifier::BOLD);
        // Per row, right to left: a tag is written over the characters it
        // covers, so laying one down can move the byte offsets of
        // everything after it but never of what is before.
        let mut per_row: Vec<Vec<(usize, String)>> = vec![Vec::new(); lines.len()];
        for (spot, rest) in j.live() {
            match spot {
                crate::jump::Spot::Cell { x, y, .. } | crate::jump::Spot::Link { x, y, .. } => {
                    let (Some(row), Some(col)) = (y.checked_sub(area.y), x.checked_sub(area.x))
                    else {
                        continue;
                    };
                    if let Some(slot) = per_row.get_mut(row as usize) {
                        slot.push((col as usize, rest.to_string()));
                    }
                }
                crate::jump::Spot::Block(entry) => {
                    if let Some(row) = owner.iter().position(|o| o.contains(entry)) {
                        per_row[row].push((0, rest.to_string()));
                    }
                }
                _ => {}
            }
        }
        lines
            .into_iter()
            .zip(per_row)
            .map(|(l, mut tags)| {
                tags.sort_unstable_by_key(|(c, _)| std::cmp::Reverse(*c));
                let text = plain(&l);
                tags.into_iter().fold(l, |l, (col, label)| {
                    let byte = text.char_indices().nth(col).map_or(text.len(), |(i, _)| i);
                    overwrite(&l, byte, &label, style)
                })
            })
            .collect()
    }

    /// The pictures on screen, found from the lines that were drawn.
    ///
    /// Derived rather than recorded: `owner` already says which entry
    /// every visible line came from, and an entry's lines are contiguous,
    /// so the run of lines belonging to an image entry *is* the box the
    /// layout reserved for it. Nothing has to be threaded down through the
    /// walk and back, and the box cannot drift from the lines that were
    /// actually drawn — which is the same reason `block_at` is the one
    /// place a block boundary is decided.
    ///
    /// A run shorter than the entry's full height has been clipped by the
    /// top or bottom of the viewport, and is skipped: a graphics protocol
    /// draws from the top-left of the box it is given, so a half-scrolled
    /// image would be drawn whole, over whatever is above it.
    fn place_images(&mut self, area: Rect, owner: &[Range<usize>]) {
        let Some(cell) = self.state.pixel_cell() else {
            return;
        };
        let mut i = 0;
        while i < owner.len() {
            let run = owner[i..].iter().take_while(|o| **o == owner[i]).count();
            let (start, this) = (i, owner[i].clone());
            i += run;
            if this.len() != 1 {
                continue;
            }
            let Some(Entry::Image(a)) = self.state.transcript.get(this.start) else {
                continue;
            };
            let (cols, rows) = image_box(a, area.width.max(1) as usize, cell);
            // The reserved rows, the caption, and the blank the block ends
            // on. Anything less and the entry is only partly on screen.
            if run != rows as usize + 2 {
                continue;
            }
            // Only a *candidate* here. Whether it is drawn, and whether
            // its cells are kept out of the diff, cannot be decided until
            // the whole frame is laid out — an overlay drawn after the
            // transcript may land on top of it. See `settle_images`.
            self.images.push(crate::image::Placement::of(
                a,
                area.x + 2,
                area.y + start as u16,
                cols,
                rows,
            ));
        }
    }

    /// The trace cursor, painted over the blocks it covers.
    ///
    /// A background and not a reverse: what is under the cursor is
    /// still text to be read — a reply is still markdown, a fold still
    /// says which of its calls failed, a match is still lit — and every
    /// one of those is a foreground colour a reversed line would throw
    /// away. The blank line a block ends on carries no spans and so
    /// takes no paint, which is what leaves a gap between one selected
    /// block and the next.
    fn focused(&self, lines: Vec<Line<'static>>, owner: &[Range<usize>]) -> Vec<Line<'static>> {
        if !self.state.tracing() {
            return lines;
        }
        let sel = crate::trace::selection(self.state);
        let bg = self.state.theme.bg("focus");
        lines
            .into_iter()
            .zip(owner)
            .map(|(mut l, o)| {
                if o.start < sel.end && sel.start < o.end {
                    l.style = l.style.patch(bg);
                }
                l
            })
            .collect()
    }

    /// The visible lines with the live search painted onto them: every
    /// match highlighted, and the first match of a tagged entry wearing
    /// its letter.
    ///
    /// The highlight is computed here, against the drawn text, rather
    /// than carried down from [`crate::search`]'s scan of the raw text —
    /// see that module for why. It costs one regex pass over one window.
    /// A match that is drawn is highlighted; a match that is *not* drawn
    /// is counted onto the line standing in for it. The second case is
    /// the one that reads as a bug without this: a fold draws
    /// `Wrote 3 files` instead of the text that matched, so a search
    /// saying `1 of 2` would light nothing on screen and look broken.
    ///
    /// Which is why the question is asked per **block** and not per
    /// line. A block knows the entries it stands for, so "this block
    /// covers hits but drew none of them" is exactly the condition, and
    /// it catches capped tool output for the same reason it catches a
    /// fold, without either being special-cased.
    fn lit(
        &self,
        lines: Vec<Line<'static>>,
        owner: &[std::ops::Range<usize>],
    ) -> Vec<Line<'static>> {
        let Some(search) = self.state.search.as_ref() else {
            return lines;
        };
        let Some(re) = search.re.as_ref() else {
            return lines;
        };
        let tags = self.hit_tags();
        let hit_style = self.state.theme.on("hit");
        let tag_style = self.state.theme.on("tag").add_modifier(Modifier::BOLD);

        let found: Vec<Vec<(usize, usize)>> = lines
            .iter()
            .map(|l| {
                let text = plain(l);
                re.find_iter(&text)
                    .filter(|m| !m.is_empty())
                    .map(|m| (m.start(), m.end()))
                    .collect()
            })
            .collect();

        // Decided a block at a time, then applied a line at a time.
        let mut note: Vec<Option<String>> = vec![None; lines.len()];
        let mut label: Vec<Option<(usize, String)>> = vec![None; lines.len()];
        let mut i = 0;
        while i < lines.len() {
            let block = owner[i].clone();
            let end = (i..lines.len())
                .find(|k| owner[*k] != block)
                .unwrap_or(lines.len());
            let drawn: usize = found[i..end].iter().map(Vec::len).sum();
            let tag = block.clone().find_map(|e| tags.get(&e).cloned());
            if drawn > 0 {
                // The tag rides the first match of the block you can see.
                if let (Some(t), Some(k)) = (tag, (i..end).find(|k| !found[*k].is_empty())) {
                    label[k] = Some((found[k][0].0, t));
                }
            } else {
                let hidden: usize = search
                    .hits
                    .iter()
                    .filter(|h| block.contains(&h.entry))
                    .map(|h| h.count)
                    .sum();
                if hidden > 0 {
                    note[i] = Some(format!(
                        " \u{b7} {hidden} match{}",
                        if hidden == 1 { "" } else { "es" }
                    ));
                    // Nothing of the match is on screen to wear the tag,
                    // so it goes at the head of the line standing in for
                    // it — over the arrow, where a tag always goes.
                    label[i] = tag.map(|t| (0, t));
                }
            }
            i = end;
        }

        lines
            .into_iter()
            .enumerate()
            .map(|(k, l)| {
                let l = if found[k].is_empty() {
                    l
                } else {
                    restyle(&l, &found[k], hit_style)
                };
                // The count wears the match's own colour: it is not a
                // remark about the line, it is where the match went.
                let l = match &note[k] {
                    Some(n) => append(&l, Span::styled(n.clone(), hit_style)),
                    None => l,
                };
                match &label[k] {
                    Some((at, t)) => overwrite(&l, *at, t, tag_style),
                    None => l,
                }
            })
            .collect()
    }

    /// The letter each tagged entry is wearing, keyed by entry — what is
    /// left of the label, so a half-typed pair narrows on screen.
    fn hit_tags(&self) -> std::collections::HashMap<usize, String> {
        let (Some(j), Some(s)) = (&self.state.jump, &self.state.search) else {
            return Default::default();
        };
        j.live()
            .into_iter()
            .filter_map(|(spot, rest)| match spot {
                crate::jump::Spot::Hit(i) => s.hits.get(*i).map(|h| (h.entry, rest.to_string())),
                _ => None,
            })
            .collect()
    }

    /// The prompt, drawn from the modal buffer: the selection reversed and
    /// the cursor a block on its head, or a bare caret in insert mode.
    ///
    /// The **body is always the message being composed**. That is the
    /// whole of what changed here: the search line used to stand in this
    /// slot and the `:` line used to *be* this buffer, so reaching for
    /// either put a half-written message out of sight. Both are now the
    /// [minibuffer](crate::state::Mini), written along the bottom border
    /// — see [`Renderer::mini_line`] — and the draft stays on the screen
    /// underneath, lit by the search like anything else.
    ///
    /// The wrapping here has to agree with [`cursor_rc`] to the character,
    /// because the selection is painted by *position* — a span split at the
    /// wrong column would highlight the wrong text rather than merely look
    /// untidy.
    fn prompt(&mut self, f: &mut Frame, area: Rect, n: &Value) {
        let b = &self.state.prompt;
        let m = mode_style(self.state);
        let hue = m.colour.as_str();
        // Red the moment a pattern stops matching, so the keystroke that
        // lost the search is the one that says so; otherwise the mode's
        // own colour, like every other prompt frame.
        let lost = self.state.searching()
            && self
                .state
                .search
                .as_ref()
                .is_some_and(|s| s.hits.is_empty() && !s.pattern.is_empty());
        let colour = if lost {
            self.state.theme.colour("error")
        } else {
            color(hue)
        };
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colour));
        // The mode names itself on the frame, in the frame's own colour.
        // While a one-line mode is up that is *its* name — the frame
        // says where the keyboard is going, and the keyboard is going to
        // the border. A script that wants its own title still overrides.
        block = match n.get("title").filter(|t| !t.is_null()) {
            Some(t) => block.title(spans_of(t)),
            None => block.title(Span::styled(
                format!(" {} ", self.frame_name()),
                Style::default().fg(colour).add_modifier(Modifier::BOLD),
            )),
        };
        let inner = block.inner(area);
        let width = inner.width.max(1) as usize;
        // The bottom border carries one of two things, and the line
        // being typed wins: an attachment strip is a standing fact about
        // the next message and comes back the moment the line is gone.
        let bottom = match self.state.mini.is_some() {
            true => self.mini_line(area, inner, colour),
            false => {
                if !self.state.attachments.is_empty() || self.state.attaching > 0 {
                    // What is staged for the next message, along the
                    // bottom of the frame it will be sent from. On the
                    // prompt and not in the transcript because it has
                    // not been said yet — an attachment is part of the
                    // message being composed, and putting it above the
                    // prompt would claim a turn had happened.
                    vec![(
                        Line::from(Span::styled(
                            format!(
                                " {} ",
                                attachment_strip(&self.state.attachments, self.state.attaching)
                            ),
                            Style::default().fg(colour),
                        )),
                        false,
                    )]
                } else {
                    Vec::new()
                }
            }
        };
        for (line, right) in bottom {
            block = match right {
                true => block.title_bottom(line.right_aligned()),
                false => block.title_bottom(line),
            };
        }
        f.render_widget(block, area);

        let text = b.text().to_string();
        let cursor = b.range.cursor();
        let (row, col) = cursor_rc(
            &text[..b.byte_of(if b.mode.typing() {
                b.range.head
            } else {
                cursor
            })],
            width,
        );
        // Only paint a selection worth seeing: in a text mode the range is
        // a caret, and a one-char normal-mode range is the cursor itself.
        let sel = (!b.mode.typing() && b.range.to() - b.range.from() > 1)
            .then(|| (b.range.from(), b.range.to()));

        let tags = self.word_tags();
        let hit = self.state.search.as_ref().and_then(|s| s.re.as_ref());
        let hit_style = self.state.theme.on("hit");
        let lines: Vec<Line> = wrap_rows(&text, width)
            .into_iter()
            .map(|(at, row)| {
                let base = match sel {
                    Some((a, z)) => Line::from(split_selected(&row, at, a, z)),
                    None => Line::raw(row.clone()),
                };
                // The draft is searched along with the session (see
                // [`crate::search::scan_all`]) and so it is lit along
                // with it — by the same rule the transcript's highlight
                // follows, run over the row actually drawn.
                let base = match hit {
                    Some(re) => {
                        let spans: Vec<(usize, usize)> = re
                            .find_iter(&row)
                            .filter(|m| !m.is_empty())
                            .map(|m| (m.start(), m.end()))
                            .collect();
                        if spans.is_empty() {
                            base
                        } else {
                            restyle(&base, &spans, hit_style)
                        }
                    }
                    None => base,
                };
                // Right to left: a tag is written over the characters it
                // covers, so laying one down can move the byte offsets of
                // everything after it but never of what is before.
                tags.iter()
                    .rev()
                    .filter(|(ci, _)| (at..at + row.chars().count()).contains(ci))
                    .fold(base, |l, (ci, label)| {
                        let byte = row
                            .char_indices()
                            .nth(ci - at)
                            .map_or(row.len(), |(i, _)| i);
                        overwrite(
                            &l,
                            byte,
                            label,
                            self.state.theme.on("tag").add_modifier(Modifier::BOLD),
                        )
                    })
            })
            .collect();
        let height = inner.height.max(1) as usize;
        let first = row.saturating_sub(height - 1);
        let shown: Vec<Line> = lines.into_iter().skip(first).take(height).collect();
        f.render_widget(Paragraph::new(shown), inner);
        // The caret belongs to whichever buffer is taking keys, and
        // `mini_line` has already claimed it if one is up.
        if self.state.mini.is_none() {
            self.cursor = Some((inner.x + col as u16, inner.y + (row - first) as u16));
            self.block_cursor = !b.mode.typing();
        }
    }

    /// What the frame calls the mode it is round. The mode's own name,
    /// except that a backward search says so in words: `/` and `?` differ
    /// by a shift and by one glyph, but they land on different matches
    /// and send `n` opposite ways, and a difference that real should not
    /// rest on a character you have to already know.
    fn frame_name(&self) -> String {
        match self.state.search_dir() {
            Some(crate::search::Dir::Backward) => "search back".into(),
            _ => mode_style(self.state).name.clone(),
        }
    }

    /// **The minibuffer**, written along the prompt's bottom border: the
    /// sigil the mode wears, the line being typed, and — right-aligned —
    /// whatever that line has to say about itself.
    ///
    /// A title and not a node of its own, for the reason the attachment
    /// strip is one: it works under a hand-written `ui.rn` that never
    /// heard of it, and it costs the transcript no rows at all. The old
    /// search line took three, by taking the prompt's whole box.
    ///
    /// Returns the border's titles, `(line, right_aligned)`, and claims
    /// the caret — which is on the border row, because that is where the
    /// text being typed is.
    fn mini_line(&mut self, area: Rect, inner: Rect, colour: Color) -> Vec<(Line<'static>, bool)> {
        let Some(mini) = self.state.mini.as_ref() else {
            return Vec::new();
        };
        let info = self.mini_info(mini);
        let info_w: usize = info.iter().map(|s| s.content.chars().count()).sum();
        let width = inner.width as usize;
        // Four columns is the floor: a border with no room for the line
        // is not a place to have put it, but a terminal that narrow has
        // worse problems and the caret still has to land somewhere.
        let room = width.saturating_sub(info_w + 1).max(4);
        // The character at the head of the line is the mode's row's, as
        // it has always been for dispatch's `!`: a script that drops it
        // draws none, and a script that changes it changes this. Which
        // *way* a search is going picks between two, because a direction
        // is not a mode — see [`Renderer::frame_name`], which says the
        // same thing in words on the frame.
        let sigil = match (mode_style(self.state).sigil, mini.kind) {
            (Some(_), crate::state::MiniKind::Search(crate::search::Dir::Backward)) => Some('?'),
            (s, _) => s,
        };
        let text = match sigil {
            Some(c) => format!("{c}{}", mini.input.text()),
            None => mini.input.text().to_string(),
        };
        let shift = usize::from(sigil.is_some());
        let (shown, at, from) = line_window(&text, shift + mini.input.range.head, room);
        let lead = Style::default().fg(colour).add_modifier(Modifier::BOLD);
        let body = Line::raw(shown);
        // The sigil is bold and coloured, when the line has not scrolled
        // out from under it.
        let body = match (from, sigil) {
            (0, Some(c)) => restyle(&body, &[(0, c.len_utf8())], lead),
            _ => body,
        };
        self.cursor = Some((inner.x + at as u16, area.y + area.height.saturating_sub(1)));
        self.block_cursor = false;
        let mut out = vec![(body, false)];
        if !info.is_empty() {
            out.push((Line::from(info), true));
        }
        out
    }

    /// What the line being typed has to say about itself, right-aligned
    /// on the border: how the search is going, or which surface a
    /// dispatch is aimed at. The `:` line says nothing here — the
    /// completion popup above it is already saying what it resolved to,
    /// and saying it twice on one frame is one of them being ignored.
    fn mini_info(&self, mini: &crate::state::Mini) -> Vec<Span<'static>> {
        let faint = self.state.theme.fg("faint");
        match mini.kind {
            crate::state::MiniKind::Dispatch => match self.state.dispatch.surface() {
                Some(s) => vec![Span::styled(format!("· {s} "), faint)],
                None => Vec::new(),
            },
            crate::state::MiniKind::Command => Vec::new(),
            crate::state::MiniKind::Search(_) => {
                let Some(s) = self.state.search.as_ref() else {
                    return Vec::new();
                };
                if s.pattern.is_empty() {
                    return Vec::new();
                }
                let mut out = Vec::new();
                let count = match s.hits.is_empty() {
                    true => "no match".to_string(),
                    false => {
                        let n = s.occurrences();
                        format!(
                            "{} of {} · {n} match{}",
                            s.at.map_or(0, |i| i + 1),
                            s.hits.len(),
                            if n == 1 { "" } else { "es" }
                        )
                    }
                };
                out.push(Span::styled(format!("· {count} "), faint));
                if s.literal {
                    out.push(Span::styled(
                        "· as text ",
                        faint.add_modifier(Modifier::ITALIC),
                    ));
                }
                // The affordance appears exactly when it is worth
                // something: the match you are on is inside a box that is
                // drawing a summary instead of it, and one key opens that
                // box without leaving the search. Nothing to open,
                // nothing said.
                if !self.state.vault.active && s.entry().and_then(|e| fold_at(self.state, e)).is_some() {
                    out.push(Span::styled(
                        "· S-tab opens ",
                        self.state.theme.fg("tag").add_modifier(Modifier::BOLD),
                    ));
                }
                out
            }
        }
    }

    /// Where `gw`'s tags sit in the prompt: `(char index, what is left of
    /// the label)`, ascending.
    fn word_tags(&self) -> Vec<(usize, String)> {
        match &self.state.jump {
            Some(j) => j
                .live()
                .into_iter()
                .filter_map(|(spot, rest)| match spot {
                    crate::jump::Spot::Word(at) => Some((*at, rest.to_string())),
                    _ => None,
                })
                .collect(),
            None => Vec::new(),
        }
    }

    /// The which-key popup: the open chord and what it can still become,
    /// sat above the prompt so it never covers what is being typed.
    fn menu(&mut self, f: &mut Frame, area: Rect) {
        let Some(menu) = &self.state.menu else { return };
        self.menu_drawn = true;
        let (chord, items) = (&menu.title, &menu.items);
        // A list to pick from rather than a grid of keys: one column, the
        // highlighted row inverted, scrolled to keep it on screen.
        if let Some(sel) = menu.selected {
            return self.menu_list(f, area, chord, items, sel);
        }
        let widest = items
            .iter()
            .map(|(k, d)| k.chars().count() + d.chars().count() + 4)
            .max()
            .unwrap_or(10);
        let cols = ((area.width as usize).saturating_sub(4) / widest.max(1)).clamp(1, 4);
        // The popup is *every* binding under the open chord — it is a
        // promise about what this key can become, so it does not choose
        // among them. On a short terminal it may not fit, and the one
        // thing it must not do then is drop the rest in silence: a row
        // goes to saying how many are missing and where the whole
        // vocabulary lives, the same way a folded tool output does.
        let room = (area.height.saturating_sub(6)).max(1) as usize;
        let want = items.len().div_ceil(cols);
        let (rows, hidden) = match want <= room {
            true => (want, 0),
            false => {
                let r = room.saturating_sub(1).max(1);
                (r, items.len().saturating_sub(r * cols))
            }
        };
        let width = ((widest * cols) as u16 + 4).min(area.width);
        let height = (rows as u16 + 2 + u16::from(hidden > 0)).min(area.height);
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.height.saturating_sub(height + 4).max(area.y),
            width,
            height,
        };
        let menu_fg = self.state.theme.fg("menu");
        let body: Vec<Line> = (0..rows)
            .map(|r| {
                let mut spans = Vec::new();
                for c in 0..cols {
                    let Some((k, d)) = items.get(c * rows + r) else {
                        continue;
                    };
                    spans.push(Span::styled(
                        format!("{k:>3} "),
                        menu_fg.add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::styled(
                        format!("{d:<w$}", w = widest.saturating_sub(k.chars().count() + 4)),
                        Style::default(),
                    ));
                }
                Line::from(spans)
            })
            .collect();
        let mut body = body;
        if hidden > 0 {
            body.push(Line::styled(
                format!("  … +{hidden} more — space p for all"),
                self.state.theme.fg("faint").add_modifier(Modifier::ITALIC),
            ));
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(menu_fg)
            .title(Span::styled(
                format!(" {chord} "),
                menu_fg.add_modifier(Modifier::BOLD),
            ));
        self.link_hits.retain(|h| !h.rect.intersects(rect));
        self.covered.push(rect);
        f.render_widget(Clear, rect);
        f.render_widget(Paragraph::new(body).block(block), rect);
    }

    /// The popup as a single-column list — command completion. Capped to a
    /// third of the screen and scrolled so the selection is always drawn.
    fn menu_list(
        &mut self,
        f: &mut Frame,
        area: Rect,
        title: &str,
        items: &[(String, String)],
        sel: usize,
    ) {
        // The names share a column, so the descriptions line up under each
        // other rather than stepping in and out with each name's length.
        let keyw = items
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(1);
        let widest = keyw
            + items
                .iter()
                .map(|(_, d)| d.chars().count())
                .max()
                .unwrap_or(1)
            + 3;
        // Twenty columns is a floor, not a demand: `clamp(20, area.width)`
        // panics outright on a terminal narrower than the floor, which is
        // a crash rather than a cramped popup. Widen to the content, cap
        // at what there is, and let a very narrow terminal have a very
        // narrow menu.
        let width = ((widest as u16) + 4).max(20).min(area.width);
        let visible = (items.len() as u16)
            .min(area.height.saturating_sub(6))
            .clamp(1, 12);
        let height = visible + 2;
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.height.saturating_sub(height + 4).max(area.y),
            width,
            height,
        };
        // Keep the selection in view without scrolling past the end.
        let first = sel
            .saturating_sub(visible as usize - 1)
            .min(items.len().saturating_sub(visible as usize));
        let theme = &self.state.theme;
        let body: Vec<Line> = items
            .iter()
            .enumerate()
            .skip(first)
            .take(visible as usize)
            .map(|(i, (k, d))| {
                let on = i == sel;
                let key = Style::default()
                    .fg(if on {
                        Color::Black
                    } else {
                        theme.colour("menu")
                    })
                    .add_modifier(Modifier::BOLD);
                let rest = Style::default().fg(if on {
                    Color::Black
                } else {
                    theme.colour("text")
                });
                let line = Line::from(vec![
                    Span::styled(format!(" {k:<keyw$} "), key),
                    Span::styled(
                        format!("{d:<w$} ", w = (width as usize).saturating_sub(keyw + 5)),
                        rest,
                    ),
                ]);
                if on {
                    line.style(Style::default().bg(theme.colour("menu")))
                } else {
                    line
                }
            })
            .collect();
        let more = if items.len() > visible as usize {
            format!(" {}/{} ", sel + 1, items.len())
        } else {
            String::new()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.fg("menu"))
            .title(Span::styled(
                format!(" {title} "),
                theme.fg("menu").add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(more, theme.fg("faint")));
        self.link_hits.retain(|h| !h.rect.intersects(rect));
        self.covered.push(rect);
        f.render_widget(Clear, rect);
        f.render_widget(Paragraph::new(body).block(block), rect);
    }

    fn dialog(&mut self, f: &mut Frame, area: Rect) {
        let Some(d) = &self.state.dialog else { return };
        if d.kind == DialogKind::Page {
            return self.page(f, area);
        }
        self.dialog_drawn = true;
        let theme = &self.state.theme;
        let width = (area.width * 3 / 4).clamp(20, 80);
        let body: Vec<Line> = match d.kind {
            DialogKind::Confirm => {
                let mut v: Vec<Line> = wrap(&d.prompt, width as usize - 4)
                    .into_iter()
                    .map(Line::raw)
                    .collect();
                v.push(Line::default());
                v.push(Line::from(vec![
                    Span::styled("[y]", theme.fg("ok").add_modifier(Modifier::BOLD)),
                    Span::raw(" allow   "),
                    Span::styled("[n]", theme.fg("error").add_modifier(Modifier::BOLD)),
                    Span::raw(" deny"),
                ]));
                v
            }
            // A multiple-choice question. The question and every option's
            // description are paragraphs — wrapped, never truncated on a
            // row (a description the operator cannot read all of is a
            // choice they cannot make) — with the description capped at
            // [`DESC_ROWS`] rows so one runaway paragraph cannot eat the
            // screen. The label wears its number because a digit selects
            // it outright, which was true before it was visible.
            DialogKind::Choose => {
                let inner = width as usize - 4;
                let mut v: Vec<Line> = wrap(&d.prompt, inner).into_iter().map(Line::raw).collect();
                v.push(Line::default());
                // One block per option: the label row plus the wrapped
                // description rows, so the scroll below budgets in what
                // is actually drawn.
                let block = |i: usize| -> Vec<Line> {
                    let (label, desc) = &d.options[i];
                    let sel = i == d.selected;
                    let head = format!("{}{}. ", if sel { "▶ " } else { "  " }, i + 1);
                    let mut rows = vec![Line::from(vec![
                        Span::styled(head, theme.fg(if sel { "dialog" } else { "faint" })),
                        Span::styled(
                            label.clone(),
                            if sel {
                                theme.fg("dialog").add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            },
                        ),
                    ])];
                    if let Some(text) = desc {
                        let mut lines = wrap(text, inner.saturating_sub(4));
                        if lines.len() > DESC_ROWS {
                            lines.truncate(DESC_ROWS);
                            let last = lines.pop().unwrap();
                            lines.push(format!("{last} …"));
                        }
                        rows.extend(lines.into_iter().map(|l| {
                            Line::from(Span::styled(format!("    {l}"), theme.fg("faint")))
                        }));
                    }
                    rows
                };
                let heights: Vec<usize> = (0..d.options.len())
                    .map(|i| {
                        1 + d.options[i]
                            .1
                            .as_deref()
                            .map(|t| wrap(t, inner.saturating_sub(4)).len().min(DESC_ROWS))
                            .unwrap_or(0)
                    })
                    .collect();
                // The window that keeps the selected block whole and fills
                // the rest of the budget from whichever side has room:
                // start as far above the selection as fits, then draw
                // forward until the budget is spent.
                let budget = (area.height as usize).saturating_sub(8).max(3);
                let mut start = d.selected.min(heights.len().saturating_sub(1));
                let mut used = heights[start];
                while start > 0 && used + heights[start - 1] <= budget {
                    start -= 1;
                    used += heights[start];
                }
                let mut drawn = 0;
                for (i, h) in heights.iter().enumerate().skip(start) {
                    if drawn + h > budget {
                        break;
                    }
                    drawn += h;
                    v.extend(block(i));
                }
                v.push(Line::default());
                // The legend is wrapped like everything else in the box: a
                // hint clipped off the edge of a narrow terminal is a
                // promise the dialog did not make.
                for l in wrap(
                    "ret takes the arrowed option · a digit takes its number · esc dismisses",
                    inner,
                ) {
                    v.push(Line::from(Span::styled(l, theme.fg("faint"))));
                }
                v
            }
            // A harness picker: one line per row, the field is a filter.
            DialogKind::Pick => {
                let mut v: Vec<Line> = wrap(&d.prompt, width as usize - 4)
                    .into_iter()
                    .map(Line::raw)
                    .collect();
                v.push(Line::from(vec![
                    Span::styled("filter: ", theme.fg("faint")),
                    Span::raw(d.input.text().to_string()),
                    Span::styled("▌", theme.fg("dialog")),
                ]));
                v.push(Line::default());
                let visible = d.visible();
                let max_rows = (area.height as usize).saturating_sub(8).max(3);
                let sel_pos = visible.iter().position(|&i| i == d.selected).unwrap_or(0);
                let first = sel_pos.saturating_sub(max_rows - 1);
                for &i in visible.iter().skip(first).take(max_rows) {
                    let (label, desc) = &d.options[i];
                    let sel = i == d.selected;
                    let mark = if sel { "▶ " } else { "  " };
                    let style = if sel {
                        theme.fg("dialog").add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    let mut spans = vec![Span::styled(format!("{mark}{}", label), style)];
                    if let Some(d) = desc {
                        spans.push(Span::styled(format!("  {d}"), theme.fg("faint")));
                    }
                    // A weighted picker wears the number, drawn from the
                    // dialog rather than baked into the description, so a
                    // bump shows on the keystroke that made it. Zero is
                    // blank: the ordering is only worth reading where the
                    // operator has said something.
                    if let Some(&w) = d.weights.get(i)
                        && w != 0
                    {
                        spans.push(Span::styled(
                            format!("  {w:+}"),
                            theme.fg(if w > 0 { "dialog" } else { "faint" }),
                        ));
                    }
                    v.push(Line::from(spans));
                }
                v
            }
            // One field, masked when the dialog says to hide it — a
            // credential is drawn as `•` per character and never as the
            // text itself.
            DialogKind::Ask => {
                let shown = if d.masked {
                    "•".repeat(d.input.text().chars().count())
                } else {
                    d.input.text().to_string()
                };
                // Bold, in the dialog's colour: faint dots read as an empty field.
                let style = if d.masked { theme.fg("dialog").add_modifier(Modifier::BOLD) } else { Style::default() };
                vec![
                    Line::from(vec![
                        Span::styled(shown, style),
                        Span::styled("▌", theme.fg("dialog")),
                    ]),
                    Line::default(),
                    Line::from(Span::styled(
                        "ret stores it · esc cancels",
                        theme.fg("faint"),
                    )),
                ]
            }
            // Drawn by `page` above; here so the match stays whole.
            DialogKind::Page => Vec::new(),
        };
        let height = (body.len() as u16 + 2).min(area.height);
        let rect = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        self.link_hits.retain(|h| !h.rect.intersects(rect));
        self.covered.push(rect);
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.fg("dialog"))
            .title(match d.kind {
                DialogKind::Confirm => " approve? ".to_string(),
                // The question box says what the box is, and the word is
                // the label table's — `choices_user` is the wire name of
                // the tool that filled it, which is not a word a person
                // reads.
                DialogKind::Choose => {
                    format!(" {} ", self.state.labels.label("choices_user"))
                }
                DialogKind::Pick => " pick ".to_string(),
                DialogKind::Page => " page ".to_string(),
                // The prompt names what is being asked for — `login`
                // hands it " provider · API key ", so the box says whose
                // key it is without the value ever needing to.
                DialogKind::Ask => d.prompt.clone(),
            });
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        f.render_widget(Paragraph::new(body), inner);
    }

    /// A page of text: `:help`, `:context`, `:tree`, `:messages`. Most
    /// of the screen, because it is a document and not a question, and
    /// scrolled from `selected` in the lines actually drawn — which is
    /// why [`page_layout`] is shared with the key handler rather than
    /// computed here alone.
    fn vault_page(&mut self, f: &mut Frame, area: Rect) {
        let Some(d) = &self.state.vault.page else { return; };
        let inner = area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });
        let doc = crate::vault::document_at(self.state, inner.width.max(1) as usize);
        let top = d.selected.min(doc.lines.len().saturating_sub(inner.height as usize));
        self.transcript_rect = area;
        self.transcript_height = inner.height as usize;
        self.transcript_width = inner.width.max(1) as usize;
        self.transcript_lines = doc.lines.len();
        self.view_entries = top..(top + inner.height as usize).min(doc.lines.len());
        let owner: Vec<_> = self.view_entries.clone().map(|row| row..row + 1).collect();
        let mut lines: Vec<_> = doc.lines.iter().skip(top).take(inner.height as usize).cloned().collect();
        let focus = self.state.vault.current.as_ref().and_then(|n| n.focus);
        for (y, line) in lines.iter_mut().enumerate() {
            link_row(line, &doc.links, top + y, focus,
                Rect::new(inner.x, inner.y + y as u16, inner.width, 1), &mut self.link_hits);
        }
        if self.state.tracing() || self.state.vault.tree.is_some() {
            let selection = crate::vault::trace_selection(self.state);
            for (offset, line) in lines.iter_mut().enumerate() {
                if selection.contains(&doc.source_lines[top + offset]) {
                    *line = line.clone().style(self.state.theme.on("hit"));
                }
            }
        }
        let lines = self.tagged(self.lit(lines, &owner), &owner, inner);
        let loading = if self.state.vault.pending.is_some() { "reading… · " } else { "" };
        let hint = if self.state.vault.tree.is_some() {
            "K trace · / search · tab record · enter inspect · space f fork · backspace back · q close"
        } else { "K trace · v select / Q reference in trace · gw jump · backspace back · i compose (no send)" };
        let block = Block::default().borders(Borders::ALL).border_style(self.state.theme.fg("dialog"))
            .title(format!(" {} ", d.prompt))
            .title_bottom(format!(" {loading}{hint} "));
        f.render_widget(block, area);
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn page(&mut self, f: &mut Frame, area: Rect) {
        let Some(d) = &self.state.dialog else { return };
        self.dialog_drawn = true;
        let theme = &self.state.theme;
        let rect = page_rect(area);
        let (width, rows) = page_layout((area.width, area.height));
        let structured = matches!(d.prompt.as_str(), "help" | "context" | "tree");
        let lines = styled_page_lines(&d.body, width, theme, structured);
        let top = d.selected.min(lines.len().saturating_sub(rows));
        let body: Vec<Line> = lines.iter().skip(top).take(rows).map(|l| {
            let mut l = l.clone();
            l.spans.insert(0, Span::raw(" "));
            l
        }).collect();
        // Where in the page you are, and only when there is more of it
        // than the frame shows: a page that fits has nowhere else to be.
        let place = match lines.len() > rows {
            true => format!(
                " {}\u{2013}{} of {} ",
                top + 1,
                (top + rows).min(lines.len()),
                lines.len()
            ),
            false => String::new(),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.fg("dialog"))
            .title(Span::styled(
                format!(" {} ", d.prompt),
                theme.fg("dialog").add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Line::from(Span::styled(place, theme.fg("faint"))).right_aligned());
        self.link_hits.retain(|h| !h.rect.intersects(rect));
        self.covered.push(rect);
        f.render_widget(Clear, rect);
        f.render_widget(Paragraph::new(body).block(block), rect);
    }
}

/// Project a link's rendered byte runs into terminal cells, after wrapping and
/// scrolling. Label text is never searched: duplicates, aliases and wide glyphs
/// all point at exactly the occurrence that was painted.
fn link_row(line: &mut Line<'static>, links: &[crate::markdown::Link], row: usize, focus: Option<usize>,
    rect: Rect, hits: &mut Vec<crate::vault::Hit>) {
    let text = plain(line);
    let mut selected = Vec::new();
    for (id, link) in links.iter().enumerate() {
        let crate::markdown::Target::Vault(target) = &link.target else { continue; };
        for (_, range) in link.runs.iter().filter(|(r, _)| *r == row) {
            let x = Span::raw(&text[..range.start]).width().min(rect.width as usize) as u16;
            let end = Span::raw(&text[..range.end]).width().min(rect.width as usize) as u16;
            if end > x {
                hits.push(crate::vault::Hit { rect: Rect::new(rect.x + x, rect.y, end - x, 1), target: target.clone() });
            }
            if focus == Some(id) { selected.push((range.start, range.end)); }
        }
    }
    // Underlined and bold, not reversed: the block it sits in may already be
    // reversed as the trace cursor's, and a mark that is the same mark vanishes.
    *line = restyle(line, &selected, Style::default().add_modifier(Modifier::UNDERLINED | Modifier::BOLD));
}

/// The widest a page is drawn. Prose past a hundred columns is hard to
/// follow back to the start of the next line, and a help page is prose.
const PAGE_WIDTH: u16 = 110;

/// Where a page sits: most of the screen, centred, capped at
/// [`PAGE_WIDTH`] — and never wider or taller than the screen itself, so
/// a tiny terminal gets a tiny page rather than a panic.
pub fn page_rect(area: Rect) -> Rect {
    let width = (area.width * 9 / 10).clamp(20.min(area.width), PAGE_WIDTH.min(area.width));
    let height = area.height.saturating_sub(2).max(3.min(area.height));
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The text a page holds on a `screen` of that size: `(columns per line,
/// lines per frame)`. The key handler scrolls in these units, so `j`
/// moves by exactly the line the frame drew and the last page down
/// lands on the last line rather than past it.
pub fn page_layout(screen: (u16, u16)) -> (usize, usize) {
    let r = page_rect(Rect {
        x: 0,
        y: 0,
        width: screen.0,
        height: screen.1,
    });
    (
        r.width.saturating_sub(4).max(1) as usize,
        r.height.saturating_sub(2).max(1) as usize,
    )
}

/// A page's text as drawn lines. A line that fits is kept as it is,
/// indentation and all — `:context` lines up columns with spaces — and
/// only one that does not is wrapped.
pub fn page_lines(body: &str, width: usize) -> Vec<String> {
    body.lines()
        .flat_map(|l| {
            if l.chars().count() <= width {
                vec![l.to_string()]
            } else {
                wrap(l, width)
            }
        })
        .collect()
}

/// Styling never changes page geometry: scrolling and copying still use the
/// same plain document. Other pages keep their existing presentation.
pub(crate) fn styled_page_lines(body: &str, width: usize, theme: &crate::theme::Theme, structured: bool) -> Vec<Line<'static>> {
    page_lines(body, width).into_iter().map(|text| {
        if !structured { return Line::raw(text); }
        if text.starts_with("── ") {
            return Line::styled(text, theme.fg("dialog").add_modifier(Modifier::BOLD));
        }
        if text.contains("← HEAD") {
            return Line::styled(text, theme.fg("dialog").add_modifier(Modifier::BOLD));
        }
        if let Some((label, value)) = text.split_once(" │ ") {
            return Line::from(vec![
                Span::styled(label.to_string(), theme.fg("dialog")),
                Span::styled(" │ ", theme.fg("faint")),
                Span::raw(value.to_string()),
            ]);
        }
        Line::raw(text)
    }).collect()
}

/// Mark the selection on the drawn frame. Inverting cells that are
/// already painted keeps it independent of the widget tree — a script can
/// lay the screen out however it likes and the drag still highlights.
fn invert(f: &mut Frame, sel: Selection) {
    let area = f.area();
    let ((x0, y0), (x1, y1)) = sel.ordered();
    let last = area.width.saturating_sub(1);
    let buf = f.buffer_mut();
    for y in y0..=y1.min(area.height.saturating_sub(1)) {
        let from = if y == y0 { x0 } else { 0 };
        let to = if y == y1 { x1.min(last) } else { last };
        for x in from..=to {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
            }
        }
    }
}

/// The transcript entry drawn on screen row `y`, if any — what a click
/// at that row is pointing at.
pub fn entry_at(state: &UiState, area: Rect, y: u16) -> Option<usize> {
    let (_, owner, _, _) = visible_transcript(state, area);
    owner.get(y.checked_sub(area.y)? as usize).map(|o| o.start)
}

/// The drawn frame as per-column cell symbols. Keeping a cell per column
/// (a wide glyph's second cell is empty) is what lets a selection be cut
/// out by column rather than by character.
pub fn screen_rows(buf: &ratatui::buffer::Buffer) -> Vec<Vec<String>> {
    let a = buf.area;
    (0..a.height)
        .map(|y| {
            (0..a.width)
                .map(|x| {
                    buf.cell((a.x + x, a.y + y))
                        .map(|c| c.symbol().to_string())
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect()
}

/// The text under `sel`, cut from rows captured by [`screen_rows`]. Each
/// row is trimmed at its right edge, so the padding a frame is made of
/// does not travel to the clipboard.
pub fn selected_text(rows: &[Vec<String>], sel: Selection) -> String {
    let ((x0, y0), (x1, y1)) = sel.ordered();
    let mut out: Vec<String> = Vec::new();
    for y in y0..=y1 {
        let Some(row) = rows.get(y as usize) else {
            continue;
        };
        let from = if y == y0 { x0 as usize } else { 0 };
        let to = if y == y1 {
            (x1 as usize + 1).min(row.len())
        } else {
            row.len()
        };
        out.push(if from < to {
            row[from..to].concat().trim_end().to_string()
        } else {
            String::new()
        });
    }
    out.join("\n")
}

/// The slice of a one-line field to draw in `width` columns, the column
/// the caret falls in, and the char index the slice starts at.
///
/// A minibuffer is one row of a border, and a line longer than the
/// border still has to be typed into, so the window follows the caret:
/// once past the right edge it sits on it and the text scrolls under it.
/// Stateless, and deliberately — a scroll offset kept between frames is
/// one more thing to put back when the line is cleared, undone or filled
/// in by a completion, and it buys nothing a reader would notice.
pub fn line_window(text: &str, caret: usize, width: usize) -> (String, usize, usize) {
    let n = text.chars().count();
    let caret = caret.min(n);
    let from = if caret < width { 0 } else { caret + 1 - width };
    let shown: String = text.chars().skip(from).take(width).collect();
    (shown, caret - from, from)
}

/// The transcript as it will be drawn in `area`: the visible lines, the
/// entries each came from, how many lines the session has in total, and
/// where the bottom visible row sits — its block's first entry and the
/// row's offset within that block, the anchor a scrolled-up view is held
/// to while the live end grows (see [`UiState::scroll_anchor`]).
///
/// One answer, two readers — the draw and the tag set `gw` builds — for
/// the reason [`block_at`] is one place: a label computed against a
/// layout the frame did not use would be drawn over the wrong word, and
/// nothing on the screen would say so.
pub fn visible_transcript(
    state: &UiState,
    area: Rect,
) -> (Vec<Line<'static>>, Vec<Range<usize>>, usize, Option<Anchor>) {
    let (lines, owner, total, anchor, _) = visible_rows(state, area);
    (lines, owner, total, anchor)
}

type VisibleRows = (Vec<Line<'static>>, Vec<Range<usize>>, usize, Option<Anchor>, Vec<usize>);

/// Also carry each row's offset in its block, so rendered link runs can be
/// projected through scrolling without matching labels or reparsing screen text.
fn visible_rows(state: &UiState, area: Rect) -> VisibleRows {
    let width = area.width.max(1) as usize;
    let height = area.height as usize;
    let (blocks, total) = walk(state, width, height + state.scroll_up, None);
    // `total >= need` whenever the walk stopped early, so this only
    // clamps once the top of the transcript is actually in hand.
    let up = state.scroll_up.min(total.saturating_sub(height));
    let start = total.saturating_sub(height + up);
    // Flattened into draw order, each line remembering which entry it
    // came from — which is what the search highlight and the tags are
    // painted against, and what `view_entries` reports back.
    let (mut lines, mut owner, mut offsets) = (Vec::new(), Vec::new(), Vec::new());
    // The anchor is named while flattening, because the walk's blocks are
    // the only thing that knows where one ends and the next begins.
    let shown = height.min(total.saturating_sub(start));
    let bottom = shown.checked_sub(1).map(|n| start + n);
    let (mut idx, mut anchor) = (0, None);
    for (entries, b) in blocks.into_iter().rev() {
        for (k, l) in b.into_iter().enumerate() {
            if Some(idx) == bottom {
                anchor = Some((entries.start, k));
            }
            lines.push(l);
            offsets.push(k);
            owner.push(entries.clone());
            idx += 1;
        }
    }
    let visible: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
    let owner: Vec<Range<usize>> = owner.into_iter().skip(start).take(visible.len()).collect();
    let offsets = offsets.into_iter().skip(start).take(visible.len()).collect();
    (visible, owner, total, anchor, offsets)
}

/// Every word on the screen worth a tag, in reading order.
///
/// The class is the **whitespace-delimited run**, not Helix's word: a tag
/// here names a thing you are about to take, and
/// `crates/tui/src/edit.rs` is one such thing rather than four. A run
/// with no letter or digit in it — a rule, a bullet, the tool arrow — is
/// not a word and gets no label, or a screen of drawn furniture would
/// spend the alphabet before reaching the text.
pub fn screen_words(state: &UiState, area: Rect) -> Vec<crate::jump::Spot> {
    let (lines, area, links, top) = if state.vault.active {
        let doc = crate::vault::document(state);
        let inner = area.inner(ratatui::layout::Margin { horizontal: 1, vertical: 1 });
        let top = state.vault.page.as_ref().map_or(0, |d| d.selected)
            .min(doc.lines.len().saturating_sub(inner.height as usize));
        (doc.lines.into_iter().skip(top).take(inner.height as usize).collect(), inner, doc.links, top)
    } else {
        let (lines, _, _, _) = visible_transcript(state, area);
        (lines, area, Vec::new(), 0)
    };
    let mut out = Vec::new();
    let mut tagged_links = std::collections::HashSet::new();
    for (row, line) in lines.iter().enumerate() {
        let text = plain(line);
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_whitespace() {
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if !word.chars().any(char::is_alphanumeric) {
                continue;
            }
            let prefix: String = chars[..start].iter().collect();
            let col = Span::raw(prefix.as_str()).width();
            if col >= area.width as usize { continue; }
            if let Some((id, link)) = links.iter().enumerate().find(|(_, l)| l.runs.iter().any(|(r, range)| *r == top + row
                && range.start < prefix.len() + word.len() && range.end > prefix.len()))
                && let crate::markdown::Target::Vault(target) = &link.target {
                if tagged_links.insert(id) {
                    let start = link.runs.iter().find(|(r, _)| *r == top + row).unwrap().1.start;
                    let col = Span::raw(&text[..start]).width();
                    out.push(crate::jump::Spot::Link { x: area.x + col as u16, y: area.y + row as u16, target: target.clone() });
                }
                continue;
            }
            let len = Span::raw(word.as_str()).width().min(area.width as usize - col);
            out.push(crate::jump::Spot::Cell {
                x: area.x + col as u16,
                y: area.y + row as u16,
                len: len as u16,
                text: word,
            });
        }
    }
    out
}

/// The first entry of every block drawn in `area`, in reading order —
/// what `gb` tags. One label per block and not per line: a block is the
/// trace cursor's unit, so two labels on one block would be two ways of
/// saying the same landing.
pub fn screen_blocks(state: &UiState, area: Rect) -> Vec<crate::jump::Spot> {
    if state.vault.active { return Vec::new(); }
    let (_, owner, _, _) = visible_transcript(state, area);
    let mut out: Vec<crate::jump::Spot> = Vec::new();
    let mut seen: Option<Range<usize>> = None;
    for o in owner {
        if seen.as_ref() != Some(&o) {
            out.push(crate::jump::Spot::Block(o.start));
            seen = Some(o);
        }
    }
    out
}

/// The bounds of a `grow:MIN:MAX` size, or `None` for every other size.
fn grow_bounds(size: Option<&Value>) -> Option<(u16, u16)> {
    let (lo, hi) = size?.as_str()?.strip_prefix("grow:")?.split_once(':')?;
    let (lo, hi) = (lo.parse().ok()?, hi.parse().ok()?);
    Some((lo, u16::max(lo, hi)))
}

/// How tall a node would like to be at `width`, borders included. Only
/// the prompt has an answer: every other node is content the layout
/// sizes, rather than content that sizes the layout.
fn rows_wanted(state: &UiState, n: &Value, width: u16) -> u16 {
    match n.get("kind").and_then(Value::as_str) {
        Some("prompt") => prompt_rows(state, width),
        _ => 1,
    }
}

/// The rows the prompt's text needs at `width`, borders included —
/// measured the same way [`Renderer::prompt`] draws it, so the box
/// cannot be a row short of its own content.
///
/// The one-line modes are not measured here and never were the prompt's
/// height: they are written on the border the box already has, so a `/`
/// or a `:` costs the transcript nothing. The search line used to stand
/// in this slot and force it to three rows.
pub fn prompt_rows(state: &UiState, width: u16) -> u16 {
    let inner = width.saturating_sub(2).max(1) as usize;
    wrap_rows(state.prompt.text(), inner).len().max(1) as u16 + 2
}

fn constraint(size: Option<&Value>) -> Constraint {
    match size {
        Some(Value::Number(n)) => Constraint::Length(n.as_u64().unwrap_or(1) as u16),
        Some(Value::String(s)) => {
            if let Some(n) = s.strip_prefix("min:") {
                Constraint::Min(n.parse().unwrap_or(1))
            } else if let Some(n) = s.strip_prefix("pct:") {
                Constraint::Percentage(n.parse().unwrap_or(50))
            } else {
                Constraint::Fill(1)
            }
        }
        _ => Constraint::Fill(1),
    }
}

fn spans_of(v: &Value) -> Line<'static> {
    match v {
        Value::String(s) => Line::raw(s.clone()),
        Value::Array(items) => Line::from(items.iter().map(span_of).collect::<Vec<_>>()),
        Value::Object(_) => Line::from(vec![span_of(v)]),
        Value::Null => Line::default(),
        other => Line::raw(other.to_string()),
    }
}

fn span_of(v: &Value) -> Span<'static> {
    match v {
        Value::String(s) => Span::raw(s.clone()),
        Value::Object(o) => {
            let mut style = Style::default();
            if let Some(fg) = o.get("fg").and_then(Value::as_str) {
                style = style.fg(color(fg));
            }
            if let Some(bg) = o.get("bg").and_then(Value::as_str) {
                style = style.bg(color(bg));
            }
            for (k, m) in [
                ("bold", Modifier::BOLD),
                ("dim", Modifier::DIM),
                ("italic", Modifier::ITALIC),
                ("underline", Modifier::UNDERLINED),
            ] {
                if o.get(k).and_then(Value::as_bool).unwrap_or(false) {
                    style = style.add_modifier(m);
                }
            }
            Span::styled(
                o.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                style,
            )
        }
        other => Span::raw(other.to_string()),
    }
}

/// A colour as the script spells it: a name, or `#rrggbb`.
pub(crate) fn color(name: &str) -> Color {
    match name {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "white" => Color::White,
        // The bright half of the palette, so a scheme has two tones of
        // each hue to tell two roles apart with.
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "default" => Color::Reset,
        s if s.starts_with('#') && s.len() == 7 => {
            let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0);
            Color::Rgb(p(1), p(3), p(5))
        }
        _ => Color::Reset,
    }
}

/// Word-wrap to `width` columns (chars, not cells — good enough for a
/// first slice), preserving explicit newlines.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        let mut len = 0;
        for word in para.split(' ') {
            let wl = word.chars().count();
            if len > 0 && len + 1 + wl > width {
                out.push(std::mem::take(&mut line));
                len = 0;
            }
            if wl > width {
                // Hard-break an overlong word.
                for ch in word.chars() {
                    if len >= width {
                        out.push(std::mem::take(&mut line));
                        len = 0;
                    }
                    line.push(ch);
                    len += 1;
                }
                continue;
            }
            if len > 0 {
                line.push(' ');
                len += 1;
            }
            line.push_str(word);
            len += wl;
        }
        out.push(line);
    }
    out
}

/// Character-exact wrapping for the prompt, each row tagged with the char
/// index it starts at. The index is what lets the selection be painted by
/// position; the wrapping itself must stay in step with [`cursor_rc`].
fn wrap_rows(text: &str, width: usize) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut at = 0;
    for (i, para) in text.split('\n').enumerate() {
        if i > 0 {
            at += 1; // the newline itself
        }
        let chars: Vec<char> = para.chars().collect();
        if chars.is_empty() {
            out.push((at, String::new()));
            continue;
        }
        for chunk in chars.chunks(width.max(1)) {
            out.push((at, chunk.iter().collect()));
            at += chunk.len();
        }
    }
    out
}

/// One drawn row cut into at most three spans, so the part of it inside
/// `sel_from..sel_to` (char indices into the whole buffer) is reversed.
fn split_selected(row: &str, at: usize, sel_from: usize, sel_to: usize) -> Vec<Span<'static>> {
    let chars: Vec<char> = row.chars().collect();
    let a = sel_from.saturating_sub(at).min(chars.len());
    let z = sel_to.saturating_sub(at).min(chars.len());
    let take = |r: std::ops::Range<usize>| -> String { chars[r].iter().collect() };
    if a >= z {
        return vec![Span::raw(take(0..chars.len()))];
    }
    let mut out = Vec::new();
    if a > 0 {
        out.push(Span::raw(take(0..a)));
    }
    out.push(Span::styled(
        take(a..z),
        Style::default().add_modifier(Modifier::REVERSED),
    ));
    if z < chars.len() {
        out.push(Span::raw(take(z..chars.len())));
    }
    out
}

// ------------------------------------------------------ painting on a line
//
// A drawn line is already spans — markdown has styled it, a tool block has
// coloured its arrow — so the search highlight and the letter tags cannot
// simply build a new one. They cut the spans that exist at byte offsets
// and put the pieces back, so what was underneath keeps its own style
// wherever the paint does not cover it.

/// A line's text, spans joined. Byte offsets into this are what the
/// helpers below cut at, and what `Regex` hands back.
fn plain(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// `line`'s spans cut at every offset in `cuts` (sorted), each piece
/// labelled with where it starts. Cuts fall on char boundaries because
/// they come from match ends, so no piece can split a character.
fn pieces(line: &Line<'static>, cuts: &[usize]) -> Vec<(usize, Span<'static>)> {
    let mut out = Vec::new();
    let mut pos = 0;
    for span in &line.spans {
        let s = span.content.as_ref();
        let end = pos + s.len();
        let mut at: Vec<usize> = std::iter::once(0)
            .chain(
                cuts.iter()
                    .filter(|p| **p > pos && **p < end)
                    .map(|p| p - pos),
            )
            .collect();
        at.push(s.len());
        at.dedup();
        for w in at.windows(2) {
            if w[0] < w[1] {
                out.push((
                    pos + w[0],
                    Span::styled(s[w[0]..w[1]].to_string(), span.style),
                ));
            }
        }
        pos = end;
    }
    out
}

/// Lay `style` over every byte range in `ranges`, keeping what is under
/// it. This is [`Style::patch`], so a match inside bold text stays bold.
fn restyle(line: &Line<'static>, ranges: &[(usize, usize)], style: Style) -> Line<'static> {
    if ranges.is_empty() {
        return line.clone();
    }
    let mut cuts: Vec<usize> = ranges.iter().flat_map(|(a, z)| [*a, *z]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let spans: Vec<Span<'static>> = pieces(line, &cuts)
        .into_iter()
        .map(
            |(at, sp)| match ranges.iter().any(|(a, z)| at >= *a && at < *z) {
                true => Span::styled(sp.content, sp.style.patch(style)),
                false => sp,
            },
        )
        .collect();
    keep(line, spans)
}

/// Write `with` *over* the line at byte offset `from`, covering as many
/// characters as it is wide. A tag replaces the head of the match it
/// points at rather than pushing it sideways: the text under a tag is
/// the one text on screen you do not need to read, and a line that
/// reflows as the tags come up is a line you have to find again.
fn overwrite(line: &Line<'static>, from: usize, with: &str, style: Style) -> Line<'static> {
    let text = plain(line);
    let from = from.min(text.len());
    let to = text[from..]
        .char_indices()
        .nth(with.chars().count())
        .map_or(text.len(), |(i, _)| from + i);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut placed = false;
    for (at, sp) in pieces(line, &[from, to]) {
        if !placed && at >= from {
            spans.push(Span::styled(with.to_string(), style));
            placed = true;
        }
        if at < from || at >= to {
            spans.push(sp);
        }
    }
    if !placed {
        spans.push(Span::styled(with.to_string(), style));
    }
    keep(line, spans)
}

/// `line` with one more span on the end.
fn append(line: &Line<'static>, span: Span<'static>) -> Line<'static> {
    let mut spans = line.spans.clone();
    spans.push(span);
    keep(line, spans)
}

/// A rebuilt line with the original's own style and alignment — which
/// live on the line, not on its spans, and would otherwise be dropped.
fn keep(line: &Line<'static>, spans: Vec<Span<'static>>) -> Line<'static> {
    let mut out = Line::from(spans);
    out.style = line.style;
    out.alignment = line.alignment;
    out
}

/// Row and column of the cursor given the text before it, under the same
/// hard wrapping `wrap_rows` applies.
fn cursor_rc(before: &str, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut row = 0;
    let mut col = 0;
    for (i, para) in before.split('\n').enumerate() {
        if i > 0 {
            row += 1;
        }
        let n = para.chars().count();
        row += n / width;
        col = n % width;
    }
    (row, col)
}

fn one_line(s: &str, width: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let width = width.max(8);
    if flat.chars().count() > width {
        let cut: String = flat.chars().take(width - 1).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

/// What the staged attachments read as on the prompt's frame.
///
/// One is named in full, because the operator wants to know they attached
/// the file they meant to. Several are counted and listed by name alone —
/// the sizes would not fit and are not the question at that point.
fn attachment_strip(a: &[crate::image::Attachment], reading: usize) -> String {
    let staged = match a {
        [] => String::new(),
        [one] => format!("\u{29c9} {}", one.label()),
        many => format!(
            "\u{29c9} {} images: {}",
            many.len(),
            many.iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    // A read in flight is said out loud. It costs a fifth of a second on a
    // large screenshot, and a fifth of a second in which the prompt looks
    // exactly as it did before reads as a dropped keystroke.
    match (staged.is_empty(), reading) {
        (_, 0) => staged,
        (true, 1) => "\u{29c9} reading\u{2026}".into(),
        (true, n) => format!("\u{29c9} reading {n}\u{2026}"),
        (false, n) => format!("{staged} · reading {n}\u{2026}"),
    }
}

/// The cell box an image reserves: as wide as the transcript allows up to
/// a limit, as tall as that width and the image's own shape imply.
///
/// Both caps exist for the same reason: a picture is a thing *in* a
/// conversation, and one that fills the terminal has pushed the
/// conversation off the screen. When the height cap bites it is the
/// *width* that gives — shrinking the box in one axis alone would hand the
/// picture a box of the wrong shape, and a protocol asked to fill a box of
/// the wrong shape stretches to fit it.
fn image_box(a: &crate::image::Attachment, width: usize, cell: (u16, u16)) -> (u16, u16) {
    const INDENT: usize = 2;
    const MAX_COLS: u16 = 64;
    const MAX_ROWS: u16 = 20;
    let mut cols = (width.saturating_sub(INDENT + 1) as u16).clamp(1, MAX_COLS);
    let mut rows = crate::image::rows_for(a.dims, cols, cell, u16::MAX);
    if rows > MAX_ROWS {
        cols = (u32::from(cols) * u32::from(MAX_ROWS) / u32::from(rows)).max(1) as u16;
        rows = MAX_ROWS;
    }
    (cols, rows)
}

/// A block the operator has struck from the context, drawn as struck.
///
/// It stays on screen at full size — the point of striking rather than
/// deleting is that you can still read it, and still put it back — but it
/// must not *look* like something the model can see. Dim plus crossed-out
/// plus a mark in the gutter, because the three degrade differently:
/// crossed-out is patchy across terminals, dim is nearly universal, and
/// the mark survives both being ignored.
fn struck(lines: Vec<Line<'static>>, theme: &Theme) -> Vec<Line<'static>> {
    let style = theme
        .fg("struck")
        .add_modifier(Modifier::DIM | Modifier::CROSSED_OUT);
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let mark = Span::styled(if i == 0 { "\u{2298} " } else { "  " }, theme.fg("struck"));
            let spans: Vec<Span<'static>> = std::iter::once(mark)
                .chain(l.spans.into_iter().map(|s| Span::styled(s.content, style)))
                .collect();
            Line::from(spans)
        })
        .collect()
}

/// One transcript entry as lines. Blocks end with a blank line so the
/// conversation breathes; the streaming caret rides the last line drawn.
fn entry_lines(
    state: &UiState,
    e: &Entry,
    width: usize,
    detail: Detail,
    pixels: Option<(u16, u16)>,
    word: &str,
) -> Vec<Line<'static>> {
    let (theme, labels, waits) = (&state.theme, &state.labels, &state.waits);
    let mut lines: Vec<Line<'static>> = Vec::new();
    match e {
        Entry::User(t) => {
            for (i, l) in wrap(t, width.saturating_sub(2)).into_iter().enumerate() {
                let prefix = if i == 0 { "\u{203a} " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(prefix, theme.fg("user").add_modifier(Modifier::BOLD)),
                    Span::styled(l, theme.fg("user")),
                ]));
            }
            lines.push(Line::default());
        }
        // A neighbour's message. Drawn like the operator's own but in
        // its own colour and under a name, because the one thing that
        // must never be ambiguous in this transcript is who said a
        // thing — and a swarm's whole failure mode is a model acting on
        // a colleague's note as though its operator had written it.
        Entry::Peer {
            from,
            channel,
            text,
        } => {
            let style = theme.fg("peer");
            let who = if *channel {
                format!("{from} \u{2192} project")
            } else {
                from.clone()
            };
            lines.push(Line::from(vec![
                Span::styled("\u{25c2} ", style.add_modifier(Modifier::BOLD)),
                Span::styled(who, style.add_modifier(Modifier::BOLD)),
            ]));
            for l in wrap(text, width.saturating_sub(2)) {
                lines.push(Line::from(vec![Span::raw("  "), Span::styled(l, style)]));
            }
            lines.push(Line::default());
        }
        // An image: the rows its pixels will be painted over, then the
        // line naming it.
        //
        // The blank rows are reserved and not drawn — the picture is
        // painted onto them after the frame is flushed, because ratatui
        // writes cells and a graphics escape writes pixels over cells, and
        // in the other order the frame paints back over the picture. On a
        // terminal that takes no pixels none are reserved: a hole where an
        // image would have been says less than the caption does, and says
        // it while taking a fifth of the screen.
        Entry::Image(a) => {
            if let Some(cell) = pixels {
                let (_, rows) = image_box(a, width, cell);
                lines.extend(std::iter::repeat_n(Line::default(), rows as usize));
            }
            let faint = theme.fg("faint");
            lines.push(Line::from(vec![
                Span::styled("\u{29c9} ", theme.fg("user")),
                Span::styled(a.label(), faint),
            ]));
            lines.push(Line::default());
        }
        // Thinking says how much of it there is, and — once something
        // has opened it — what it said. The header stays either way, so
        // an opened block is still identifiable as thinking and still
        // says how long it ran.
        Entry::Thinking { text, streaming } => {
            let faint = theme.fg("faint").add_modifier(Modifier::ITALIC);
            lines.push(Line::styled(
                format!("{BULLET} {} ({})", word.to_lowercase(), text.chars().count()),
                faint,
            ));
            if detail == Detail::Full && !text.is_empty() {
                for l in wrap(text, width.saturating_sub(2)) {
                    lines.push(Line::styled(format!("  {l}"), faint));
                }
            }
            if *streaming {
                if let Some(last) = lines.last_mut() {
                    last.push_span(Span::styled("\u{258c}", theme.fg("cursor")));
                }
            } else {
                lines.push(Line::default());
            }
        }
        Entry::Assistant { text, streaming } => {
            if !text.is_empty() {
                lines.extend(crate::markdown::render(
                    text,
                    width,
                    &crate::markdown::Palette::from(theme),
                ));
            }
            if *streaming {
                match lines.last_mut() {
                    Some(last) => last.push_span(Span::styled("\u{258c}", theme.fg("cursor"))),
                    None => lines.push(Line::styled("\u{258c}", theme.fg("cursor"))),
                }
            } else if !text.is_empty() {
                lines.push(Line::default());
            }
        }
        // A parked wait is not a call that returned — it is the session
        // holding still while the world moves, so it draws as the
        // checklist it is, not as a name and a JSON blob. See
        // [`wait_block`].
        Entry::Tool {
            id,
            name,
            input,
            output,
            ..
        } if name == "wait_for" => lines.extend(wait_block(
            state,
            input,
            output.as_ref().map(|(out, _)| out.as_str()),
            waits.get(id),
            width,
        )),
        Entry::Tool {
            name,
            input,
            output,
            ..
        } => lines.extend(tool_block(
            name,
            labels.label(name),
            input,
            output.as_ref().map(|(out, err)| (out.as_str(), *err)),
            width,
            detail == Detail::Full,
            theme,
        )),
        // One marked line the harness ran, drawn as the call it is — see
        // [`ask_block`]. Before this it was prose with the answer arriving
        // as a quiet note nothing tied to it.
        Entry::Ask { commands, answers } => {
            for (i, answer) in answers.iter().enumerate() {
                lines.extend(ask_block(
                    commands.get(i),
                    answer,
                    width,
                    detail == Detail::Full,
                    theme,
                ));
            }
        }
        // A park resolving: the mark says *harness*, the words say what
        // came of the wait. The tool colour, not the faint italic of a
        // note — this is the third way a turn starts, and it reads at a
        // glance or the morning's transcript has a hole in it.
        Entry::Woke { condition, outcome } => {
            let mark = theme.fg("tool");
            let head = vec![
                Span::styled("\u{263e} ", mark.add_modifier(Modifier::BOLD)),
                Span::styled("woke ", mark.add_modifier(Modifier::BOLD)),
                Span::styled(format!("{outcome} — waiting on {condition}"), mark),
            ];
            lines.extend(crate::markdown::wrap_spans(head, width, &[], &[Span::raw("  ")]));
            lines.push(Line::default());
        }
        Entry::Info(t) => {
            for (i, l) in wrap(t, width.saturating_sub(2)).into_iter().enumerate() {
                let prefix = if i == 0 { format!("{BULLET} ") } else { "  ".to_string() };
                lines.push(Line::styled(
                    format!("{prefix}{l}"),
                    theme.fg("faint").add_modifier(Modifier::ITALIC),
                ));
            }
        }
        Entry::Settle { turn, text } => {
            // The star where every other line of harness talk wears the
            // bullet: the same faint italic line, with the turn's own mark
            // in front of it, so a finished turn reads at a glance and no
            // two read alike.
            let mark = crate::life::star(*turn);
            for (i, l) in wrap(text, width.saturating_sub(2)).into_iter().enumerate() {
                let prefix = if i == 0 { format!("{mark} ") } else { "  ".to_string() };
                lines.push(Line::styled(
                    format!("{prefix}{l}"),
                    theme.fg("faint").add_modifier(Modifier::ITALIC),
                ));
            }
        }
        Entry::Alert(t) => {
            for (i, l) in wrap(t, width.saturating_sub(2)).into_iter().enumerate() {
                let prefix = if i == 0 { "\u{26a0} " } else { "  " };
                lines.push(Line::styled(format!("{prefix}{l}"), theme.fg("error")));
            }
        }
    }
    lines
}

// ---------------------------------------------------------------- tool calls

/// Keys worth showing first: a tool's arguments usually have one that *is*
/// the call — `read AGENTS.md` — and the rest are modifiers on it.
const PRIMARY: [&str; 8] = [
    "path",
    "file_path",
    "file",
    "command",
    "cmd",
    "pattern",
    "query",
    "url",
];

/// Output lines a collapsed tool result shows.
const TOOL_LINES: usize = 6;

/// The bullet a line of the transcript leads with when it has only words
/// to say — the model thinking, the harness talking about itself.
///
/// The **full** bullet (U+2022), not the middle dot it used to be: a
/// middle dot is a separator's character, and at the head of a line it
/// read as a speck of dirt rather than as a mark. It is the same glyph a
/// list in a reply leads with, which is no accident — this is a bullet
/// point, and it should look like the one on a page. A finished turn
/// wears its star instead, and a folded run its disclosure.
const BULLET: &str = "\u{2022}";

/// The start of the run of *settled* tool calls ending at `end`, or `end`
/// itself when the entry before it is not one. A call still waiting on its
/// output is not part of a run: it is drawn as itself, so a turn cancelled
/// mid-call keeps saying so rather than being counted as work that
/// happened.
/// What mode the prompt is in, as the row of the script's table that
/// describes it — **one answer, asked by three things**: the border
/// round the prompt, the title on it, and the badge the script draws on
/// the status line. Those were three separate literals, so the badge
/// could say one colour while the frame said another, and only the
/// search line bothered with a title at all. The literals then moved
/// here, and from here into `modes()` in `ui.rn`: this reads the table,
/// and the snapshot reads this, so the script sees the very row the
/// frame was drawn from.
///
/// Two of these are not modes on the buffer. The command line is the
/// prompt in insert with a `:` in front of it and the search line is not
/// the prompt at all, but both are modes to the hands, so both name
/// themselves here — which is the same judgement [`UiState::snapshot`]
/// already makes when it reports `mode`.
pub fn mode_style(state: &UiState) -> ModeDef {
    state.modes.style(state.mode_name())
}

/// One laid-out block: the transcript entries it stands for, and the
/// lines it draws as. A block is usually one entry; a folded run is
/// several, which is why the range and not an index — "does this block
/// hide a match" is a question about everything it stands for.
type BlockLines = (std::ops::Range<usize>, Vec<Line<'static>>);

/// Walk the transcript backwards from its end, laying out one block at a
/// time, and stop once `need` lines are in hand — or, when `down_to` is
/// set, once the walk has reached that entry. Returns the blocks in
/// bottom-up order, each with the entry it starts at, and the line total.
///
/// Both callers go through here so they cannot disagree about what a
/// line is: the draw needs a window's worth of lines, and
/// [`lines_below`] needs the distance from an entry to the bottom, and a
/// search that scrolled to a line the renderer counts differently would
/// land near the match rather than on it.
fn walk(
    state: &UiState,
    width: usize,
    need: usize,
    down_to: Option<usize>,
) -> (Vec<BlockLines>, usize) {
    let t = &state.transcript;
    let mut blocks = Vec::new();
    let mut total = 0;
    let mut end = t.len();
    while end > 0 && (total < need || down_to.is_some_and(|i| end > i)) {
        // Where the block boundaries are is decided in one place, so a
        // cursor and a draw cannot disagree about what a block is.
        let b = block_at(state, end - 1);
        let (start, lines) = match fold_at(state, end - 1) {
            Some(_) => (b.start, group_lines(&t[b.clone()], width, &state.theme, crate::life::word(state.tools_run as i64), false)),
            None => {
                // An opened cluster draws every part with all of its
                // output. That is what "open this box" has to mean: it
                // was opened to find something in it, and a capped
                // output would hide the find one level further down.
                let detail = if opened_at(state, end - 1).is_some() {
                    Detail::Full
                } else {
                    state.detail
                };
                let mut lines = entry_lines(state, &t[end - 1], width, detail, state.pixel_cell(), crate::life::word(state.tools_run as i64));
                // The run's own line stays above what it opened onto, so
                // the thing that was clicked is still there to click back.
                if opened_at(state, end - 1) == Some(end - 1)
                    && let Some(run) = cluster_of(state, end - 1)
                    && let Some(head) = group_lines(&t[run], width, &state.theme, crate::life::word(state.tools_run as i64), true).into_iter().next()
                {
                    lines.insert(0, head);
                }
                (end - 1, lines)
            }
        };
        // Struck when every record the block covers has been struck. A
        // cluster that is only partly struck is left alone rather than
        // drawn as though all of it were gone — the operator opens it
        // (`l`) to see which part, and the parts draw themselves.
        let covered: Vec<_> = (start..end).filter_map(|i| state.record_at(i)).collect();
        let lines = if !covered.is_empty() && covered.iter().all(|r| state.struck.contains(r)) {
            struck(lines, &state.theme)
        } else {
            lines
        };
        total += lines.len();
        blocks.push((start..end, lines));
        end = start;
    }
    (blocks, total)
}

/// The **cluster** `entry` belongs to — the run of thinking and finished
/// tool calls the view would draw as one line — whether or not it is
/// currently folded. `None` when the entry is not that kind of thing, or
/// when the view is not folding at all.
///
/// This asks about the *view*, not the data. Three things stop a cluster
/// being one: the detail knob is off its resting step, the entry is
/// prose or a call still running, or — while the turn is in flight — the
/// [`Fold`] setting says the turn holds itself open. Under
/// [`Fold::Settle`] that is the whole of it, since the operator's
/// message: folding each call as it finished made a summary line that
/// popped with every call, so the turn folds once, when it settles.
/// Under [`Fold::Live`] only the tail run is held — a finished run folds
/// the moment the next call begins, as it did before the settle rule.
fn cluster_of(state: &UiState, entry: usize) -> Option<Range<usize>> {
    let t = &state.transcript;
    if state.detail != Detail::Folded || !t.get(entry).is_some_and(Entry::clustered) {
        return None;
    }
    if state.working && entry >= match state.fold {
        Fold::Settle => turn_start(t),
        Fold::Live => cluster_start(state, t.len()),
    } {
        return None;
    }
    // A parked wait is the one settled call that stays drawn: the turn
    // ended but the wait has not, and the checklist ticking in place is
    // the whole point of drawing it. It folds like any call once the
    // fire has landed.
    if parked_wait(state, entry) {
        return None;
    }
    Some(cluster_start(state, entry + 1)..cluster_end(state, entry))
}

/// Is the entry a `wait_for` still armed — output in, fire not yet? The
/// liveness lives in the view's waits map, not the entry: a resumed
/// session has no tick state and folds its old waits like any call.
fn parked_wait(state: &UiState, i: usize) -> bool {
    match state.transcript.get(i) {
        Some(Entry::Tool { id, name, .. }) if name == "wait_for" => {
            state.waits.get(id).is_some_and(|w| w.settled.is_none())
        }
        _ => false,
    }
}

/// The fold `entry` is currently drawn inside, as that cluster's first
/// entry — or `None` when it is drawn as itself and there is nothing to
/// open.
pub fn fold_at(state: &UiState, entry: usize) -> Option<usize> {
    cluster_of(state, entry)
        .map(|c| c.start)
        .filter(|start| !state.opened.contains(start))
}

/// The cluster `entry` is in that the operator has already opened, as
/// its first entry — the other half of [`fold_at`], and what says that
/// this entry is drawn with all of its output.
pub fn opened_at(state: &UiState, entry: usize) -> Option<usize> {
    cluster_of(state, entry)
        .map(|c| c.start)
        .filter(|start| state.opened.contains(start))
}

/// The block `entry` is drawn as part of: one entry, unless it is inside
/// a fold, in which case the whole cluster. The one answer [`walk`] and
/// [`crate::trace`] both take theirs from.
pub fn block_at(state: &UiState, entry: usize) -> Range<usize> {
    match fold_at(state, entry) {
        Some(_) => cluster_of(state, entry).unwrap_or(entry..entry + 1),
        None => entry..entry + 1,
    }
}

/// Drawn lines from the top of the block holding `entry` to the bottom
/// of the transcript, and that block's own height — the two numbers a
/// scroll needs to put a block on screen without moving more than it
/// has to. See [`crate::trace::reveal`].
pub fn block_extent(state: &UiState, width: usize, entry: usize) -> (usize, usize) {
    let (blocks, total) = walk(state, width.max(1), 0, Some(entry));
    (
        total,
        blocks.last().map_or(1, |(_, lines)| lines.len().max(1)),
    )
}

/// Drawn lines from the top of entry `i` to the bottom of the
/// transcript — which is exactly the `scroll_up` that puts `i` on the
/// first row, once the window height is taken off it.
///
/// An entry inside a folded run answers for the whole run, because that
/// is the block it is drawn as. Scrolling to it therefore puts the fold
/// summary on the first row, which is where the match is, as far as the
/// screen is concerned.
pub fn lines_below(state: &UiState, width: usize, i: usize) -> usize {
    walk(state, width.max(1), 0, Some(i)).1
}

/// The first entry of the cluster ending at `end`, or `end` itself when
/// the entry before it does not belong to one.
/// Where the turn in flight began: the entry after the last message the
/// operator sent, or the top when there is none.
fn turn_start(t: &[Entry]) -> usize {
    t.iter().rposition(|e| matches!(e, Entry::User(_))).map_or(0, |i| i + 1)
}

fn cluster_start(state: &UiState, end: usize) -> usize {
    let t = &state.transcript;
    let mut i = end;
    while i > 0 && t[i - 1].clustered() && !parked_wait(state, i - 1) {
        i -= 1;
    }
    i
}

/// One past the last entry of the cluster holding `entry`.
fn cluster_end(state: &UiState, entry: usize) -> usize {
    let t = &state.transcript;
    let mut i = entry + 1;
    while i < t.len() && t[i].clustered() && !parked_wait(state, i) {
        i += 1;
    }
    i
}

/// Presentation only: how a run of one tool reads in prose. A tool not on
/// this list — a user's skill, a tool served over MCP — falls back to its
/// own name (`Ran 3 mneme_rpc calls`), which is why this can stay short
/// and why nothing breaks by being left off it. Matched case-insensitively,
/// because a backend that runs its own tools spells them `Bash` and `Read`.
const PROSE: [(&str, &str, &str, &str); 12] = [
    ("read", "Read", "file", "files"),
    ("write", "Wrote", "file", "files"),
    ("edit", "Edited", "file", "files"),
    ("bash", "Ran", "shell command", "shell commands"),
    ("shell", "Ran", "shell command", "shell commands"),
    ("grep", "Ran", "search", "searches"),
    ("glob", "Ran", "search", "searches"),
    ("search", "Ran", "search", "searches"),
    ("fetch", "Fetched", "URL", "URLs"),
    ("webfetch", "Fetched", "URL", "URLs"),
    ("websearch", "Ran", "web search", "web searches"),
    ("wait_for", "Waited on", "condition", "conditions"),
];

/// How many kinds of call a summary names before it gives up and counts
/// the rest. A cluster spanning several rounds can touch a lot of tools,
/// and a line the width cuts off mid-phrase says less than a short one.
const SUMMARY_KINDS: usize = 3;

/// The name a cluster's marked lines are counted under. Not a tool: what
/// the model wrote was prose, so there is no tool to name it after — the
/// channel it reached is the vault.
const ASK_KIND: &str = "vault";

/// `n` calls of one tool, in prose: `Read 2 files`, `Ran 1 shell
/// command`, `Ran 3 mneme_rpc calls` — and, for the lines the model marked
/// in its own prose, `Asked the vault 2 questions`, which is the one phrase
/// that is not a tool at all.
fn phrase(name: &str, n: usize) -> String {
    if name == ASK_KIND {
        return format!("Asked the vault {n} question{}", if n == 1 { "" } else { "s" });
    }
    let (verb, one, many) = match PROSE.iter().find(|(k, ..)| *k == name) {
        Some((_, verb, one, many)) => (*verb, (*one).to_string(), (*many).to_string()),
        None => ("Ran", format!("{name} call"), format!("{name} calls")),
    };
    format!("{verb} {n} {}", if n == 1 { one } else { many })
}

/// What a folded cluster's work says it did, one phrase per kind in the
/// order the model first reached for it — `Read 2 files · Ran 3 shell
/// commands`.
///
/// It used to be a single phrase, and a run that changed tools halfway
/// was reported as `Ran 5 tools`, on the grounds that naming only the
/// first would be a lie in the shape of the truth. That was right when
/// a fold was one round of calls and mixed runs were rare. A cluster
/// spans every round of a turn, so mixed is now the *usual* case, and
/// "5 tools" is the answer to a question nobody asked.
///
/// The marked lines are counted here beside the calls because they are
/// the same kind of thing: work between two pieces of prose. One entry is
/// one reply's marks and its answers, so it contributes as many as it was
/// given lines.
fn summary(calls: &[&Entry]) -> String {
    let mut kinds: Vec<(String, usize)> = Vec::new();
    let mut note = |name: String, n: usize| match kinds.iter_mut().find(|(k, _)| *k == name) {
        Some((_, count)) => *count += n,
        None => kinds.push((name, n)),
    };
    for e in calls {
        match e {
            Entry::Tool { name, .. } => note(name.to_lowercase(), 1),
            Entry::Ask { answers, .. } => note(ASK_KIND.to_string(), answers.len()),
            _ => {}
        }
    }
    let rest: usize = kinds.iter().skip(SUMMARY_KINDS).map(|(_, n)| n).sum();
    let mut out: Vec<String> = kinds
        .iter()
        .take(SUMMARY_KINDS)
        .map(|(k, n)| phrase(k, *n))
        .collect();
    if rest > 0 {
        out.push(format!("{rest} more"));
    }
    out.join(" \u{b7} ")
}

/// A settled **cluster** as one line — everything the model did between
/// one piece of prose and the next: `cogitat (246) · Read 2 files · Ran
/// 3 shell commands`.
///
/// The thinking leads because it came first, and it is counted rather
/// than quoted: a summary that spent four lines on the reasoning would
/// be the wall of text this exists to replace. The failures are counted
/// onto the end, because folding must not be a way for a tool that
/// failed to leave the screen quietly, and the whole of it is one `l`
/// away in trace mode.
fn group_lines(run: &[Entry], width: usize, theme: &Theme, word: &str, open: bool) -> Vec<Line<'static>> {
    let faint = theme.fg("faint");
    // What the run *did*: the tools it called and the lines it marked for
    // the harness. Marked lines count as work for the same reason they are
    // in a cluster at all — a call the chokepoint ran between two pieces of
    // prose — so a run of nothing but vault asks wears the work arrow and
    // not the quiet mark thinking alone wears.
    let calls: Vec<&Entry> = run
        .iter()
        .filter(|e| matches!(e, Entry::Tool { .. } | Entry::Ask { .. }))
        .collect();
    let thought: usize = run
        .iter()
        .filter_map(|e| match e {
            Entry::Thinking { text, .. } => Some(text.chars().count()),
            _ => None,
        })
        .sum();
    // The arrow is the tool traffic's; a cluster that only thought
    // keeps the quieter mark it wears when it is drawn as itself, so
    // folding never turns thinking into work that was done.
    // A fold wears a disclosure and not the tool arrow: the arrow means
    // a call went out, and this line stands for several. It points down
    // once the run is open, which is the one thing that says a click did
    // anything and that a second click puts it back.
    let mut spans = match calls.is_empty() {
        true => vec![Span::styled(format!("{BULLET} "), faint)],
        false => vec![Span::styled(if open { "▾ " } else { "▸ " }, theme.fg("tool"))],
    };
    if thought > 0 {
        spans.push(Span::styled(
            format!("{} ({thought})", word.to_lowercase()),
            faint.add_modifier(Modifier::ITALIC),
        ));
        if !calls.is_empty() {
            spans.push(Span::styled(" \u{b7} ", faint));
        }
    }
    if !calls.is_empty() {
        spans.push(Span::styled(summary(&calls), faint));
    }
    let failed = run
        .iter()
        .filter(|e| {
            matches!(
                e,
                Entry::Tool {
                    output: Some((_, true)),
                    ..
                }
            )
        })
        .count();
    if failed > 0 {
        spans.push(Span::styled(
            format!(" \u{b7} {failed} failed"),
            theme.fg("error"),
        ));
    }
    vec![clip(spans, width, theme), Line::default()]
}

/// One marked line as the call it became: `→ ! read the note X`, then the
/// harness's answer under `↳`.
///
/// A tool call's own shape, arrows and all, because that is what this is: a
/// call the harness made on the model's behalf and what came back from it.
/// It goes through [`tool_block`] so the cap, the wrapping and the `space o`
/// that reveals the rest are one implementation for both — the channel used
/// to clip an answer to a couple of hundred characters, which made the fold
/// tidy and the model reach for `mneme_rpc`, and it would be a bug for this
/// to be the one place a long one was swallowed.
///
/// The marker is **drawn and never stored**: it was the syntax of the line
/// the model wrote, `UiState::answered` took it out of the prose, and what
/// is left is the command itself — so the `!` beside the arrow is the same
/// drawn sigil the dispatch minibuffer wears, not text.
/// One marked line the harness ran, drawn as the call it is: the command
/// under the call arrow, each payload block boxed beneath it the way the
/// reply's markdown draws one — the arguments a tool call would carry —
/// then the answer under the return arrow. See [`Entry::Ask`].
fn ask_block(
    marked: Option<&crate::state::Marked>,
    answer: &str,
    width: usize,
    expanded: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let tool = theme.fg("tool");
    let palette = crate::markdown::Palette::from(theme);
    let mut head = vec![Span::styled("→ ! ", tool)];
    if let Some(marked) = marked {
        head.push(Span::styled(
            marked.command.clone(),
            theme.fg("sent").add_modifier(Modifier::BOLD),
        ));
    }
    let mut lines = vec![clip(head, width, theme)];
    if let Some(marked) = marked {
        for (letter, bytes) in &marked.payloads {
            let rows = crate::markdown::payload_lines(*letter, bytes, width, &palette);
            if expanded || rows.len() <= TOOL_LINES {
                lines.extend(rows);
            } else {
                lines.extend(rows[..TOOL_LINES].iter().cloned());
                lines.push(Line::styled(
                    format!(
                        "    … +{} more line{} — space o",
                        rows.len() - TOOL_LINES,
                        if rows.len() - TOOL_LINES == 1 { "" } else { "s" }
                    ),
                    theme.fg("faint").add_modifier(Modifier::ITALIC),
                ));
            }
        }
    }
    lines.extend(output_block(answer, false, width, expanded, theme));
    lines
}

/// One tool call: the arguments read as arguments rather than as the JSON
/// the model sent, then the output, capped unless the transcript is
/// expanded (`space o`).
/// The change an `edit` call is about to make, read off the call's own
/// arguments. The tool answers `edited <path> (1 replacement)` and never
/// a diff, so this is the only place the change can be seen — and it is
/// seen while the call is still running, which is when it matters.
///
/// The lines the two sides share at each end are trimmed away: a diff of
/// the whole quoted block buries the one line that changed.
pub(crate) fn edit_diff(name: &str, input: &str) -> Option<Vec<(char, String)>> {
    if !matches!(name, "edit" | "write") {
        return None;
    }
    let v: Value = serde_json::from_str(input.trim()).ok()?;
    // A file written is a file whose every line is new, which is how a
    // diff has always shown one. The cap keeps a long one from taking
    // the screen; `space o` has the rest.
    if name == "write" {
        let content = v.get("content").and_then(Value::as_str)?;
        let lines: Vec<(char, String)> = content.lines().map(|l| ('+', l.to_string())).collect();
        return (!lines.is_empty()).then_some(lines);
    }
    let old = v.get("old_str").and_then(Value::as_str)?;
    let new = v.get("new_str").and_then(Value::as_str)?;
    let (o, n): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let head = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let tail = o[head..]
        .iter()
        .rev()
        .zip(n[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    // One line of what did not change on either side, so the change is
    // read in the place it happens rather than on its own.
    let mut out: Vec<(char, String)> = o[head.saturating_sub(CONTEXT)..head]
        .iter()
        .map(|l| (' ', (*l).to_string()))
        .collect();
    out.extend(o[head..o.len() - tail].iter().map(|l| ('-', (*l).to_string())));
    out.extend(n[head..n.len() - tail].iter().map(|l| ('+', (*l).to_string())));
    let after = o.len() - tail;
    out.extend(o[after..(after + CONTEXT).min(o.len())].iter().map(|l| (' ', (*l).to_string())));
    out.iter().any(|(s, _)| *s != ' ').then_some(out)
}

/// Lines of the unchanged kept either side of a change, so it is read
/// in the place it happens. Only what the call itself quoted: the
/// harness has the fragment the tool was given and not the file, so a
/// change the model quoted tightly has little around it to show.
const CONTEXT: usize = 3;

/// What language a call's diff is in, from the extension of the path it
/// names. Nothing when the path has none, which the scanner reads as
/// plain and paints only for strings and numbers.
fn diff_lang(input: &str) -> String {
    let v: Option<Value> = serde_json::from_str(input.trim()).ok();
    let path = v
        .as_ref()
        .and_then(|v| v.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let name = path.rsplit('/').next().unwrap_or("");
    name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default()
}

/// Whether a tool's output is a unified diff — a `git diff` through the
/// shell, a patch — and so should be painted by sign rather than read as
/// prose with some unlucky leading hyphens.
fn is_unified_diff(out: &str) -> bool {
    out.lines().any(|l| l.starts_with("@@ ") && l.contains(" @@"))
        || (out.starts_with("--- ") && out.contains("\n+++ "))
}

/// A diff under the call that is making it: a bar down the left, and a
/// line that changed tinted across the pane — green for what goes in,
/// red for what comes out, nothing for what stays. The tint is a
/// background and the text keeps its own colour, because a whole line
/// painted green reads as a warning rather than as an addition.
fn diff_lines(diff: &[(char, String)], lang: &str, width: usize, expanded: bool, theme: &Theme) -> Vec<Line<'static>> {
    let cap = if expanded { diff.len() } else { TOOL_LINES.min(diff.len()) };
    let bar = Span::styled("\u{258e} ", theme.fg("tool"));
    let palette = crate::markdown::Palette::from(theme);
    // Numbered by the side that survives — the lines the file will have
    // — so a removed line takes a blank and the numbers stay a sequence.
    // For a write those are the file's own; for an edit they are the
    // fragment's, which is what there is to count.
    let kept = diff.iter().filter(|(s, _)| *s != '-').count();
    let digits = if kept > 1 { kept.to_string().len() } else { 0 };
    let mut n = 0;
    let mut lines: Vec<Line<'static>> = diff[..cap]
        .iter()
        .map(|(sign, text)| {
            // Every line of the block sits on a ground, so the block is
            // one shape: green for what goes in, red for what comes
            // out, and the slab's own colour for what only stands there.
            let tint = match sign {
                '+' => theme.bg("added"),
                '-' => theme.bg("removed"),
                _ => palette.block,
            };
            if *sign != '-' {
                n += 1;
            }
            let mut spans = vec![bar.clone()];
            if digits > 0 {
                let label = match sign {
                    '-' => " ".repeat(digits + 1),
                    _ => format!("{n:>digits$} "),
                };
                // The line is one ground, numbers included: the `gutter`
                // role alone is what sets them apart from the code.
                spans.push(Span::styled(label, palette.gutter.patch(tint)));
            }
            spans.push(Span::styled(format!("{sign} "), tint));
            spans.extend(
                crate::markdown::file_line(&text.replace('\t', "    "), lang, &palette)
                    .into_iter()
                    .map(|s| Span::styled(s.content, s.style.patch(tint))),
            );
            let used: usize = spans.iter().map(|s| Span::raw(s.content.as_ref()).width()).sum();
            spans.push(Span::styled(" ".repeat(width.saturating_sub(used)), tint));
            clip(spans, width, theme)
        })
        .collect();
    if diff.len() > cap {
        let n = diff.len() - cap;
        lines.push(Line::styled(
            format!("    \u{2026} +{n} more line{} \u{2014} space o", if n == 1 { "" } else { "s" }),
            theme.fg("faint").add_modifier(Modifier::ITALIC),
        ));
    }
    lines
}

/// One row of the wait's tree: a composite's label, or a leaf with the
/// index of its sample — the position its answer takes in the condition's
/// own `terms()` order, which the ticks repeat.
enum WaitNode {
    Label(&'static str),
    /// The leaf as [`Term::parts`] says it — kind for the tag column,
    /// subject and predicate for the row — with the index of its sample
    /// in the condition's `terms()` order, the order the ticks repeat.
    Leaf {
        parts: eidolon_core::wait::TermParts,
        at: usize,
    },
}

/// The condition as rows: composites become quiet labels (`all of`,
/// `or`, `not`), leaves become checklist rows in the words the model was
/// given — [`Term::describe`], not a re-derivation. The leaf walk is the
/// exact mirror of the registry's own leaf collection, dedup included,
/// because the ticks are indexed by that order.
fn wait_tree(
    c: &eidolon_core::wait::Condition,
    depth: usize,
    out: &mut Vec<(usize, WaitNode)>,
    seen: &mut Vec<eidolon_core::wait::Term>,
) {
    use eidolon_core::wait::Condition;
    match c {
        Condition::All(cs) => {
            if cs.len() > 1 {
                out.push((depth, WaitNode::Label("all of")));
            }
            for c in cs {
                wait_tree(c, depth + 1, out, seen);
            }
        }
        Condition::Any(cs) => {
            if cs.len() > 1 {
                out.push((depth, WaitNode::Label("or")));
            }
            for c in cs {
                wait_tree(c, depth + 1, out, seen);
            }
        }
        Condition::Not(inner) => match inner.as_ref() {
            // A negated leaf is one row: `not (session x is idle)` reads
            // whole, and a label over one row is a fence around nothing.
            Condition::Term(t) => {
                let at = seen_index(seen, t);
                let mut parts = t.parts();
                parts.predicate = format!("not ({})", parts.predicate);
                out.push((depth, WaitNode::Leaf { parts, at }));
            }
            other => {
                out.push((depth, WaitNode::Label("not")));
                wait_tree(other, depth + 1, out, seen);
            }
        },
        Condition::Term(t) => {
            let at = seen_index(seen, t);
            out.push((depth, WaitNode::Leaf { parts: t.parts(), at }));
        }
        Condition::Never => out.push((depth, WaitNode::Label("nothing but the deadline"))),
    }
}

/// The position of a leaf in the registry's deduped order, registering it
/// when it is new.
fn seen_index(seen: &mut Vec<eidolon_core::wait::Term>, t: &eidolon_core::wait::Term) -> usize {
    if let Some(at) = seen.iter().position(|s| s == t) {
        at
    } else {
        seen.push(t.clone());
        seen.len() - 1
    }
}

/// A duration a person reads: `6m 02s`, `1h 05m`.
fn fmt_wait(s: u64) -> String {
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// A deadline a person reads — the same, but a whole minutes count is
/// said whole: `15m`, not `15m 00s`.
fn fmt_deadline(s: u64) -> String {
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else if s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else {
        format!("{}m {:02}s", s / 60, s % 60)
    }
}

/// The braille frames a pending row turns through: waiting is not a
/// full stop, it is the watcher's pulse made visible.
const SPINNER: [char; 10] = [
    '\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}',
    '\u{2827}', '\u{2807}', '\u{280f}',
];

/// A parked wait, drawn as the checklist it is.
///
/// The head is the label and nothing else — `→ Waiting` — because what
/// the call waits on is the block, and the JSON it sent is not a thing a
/// person reads. Under it, one row per leaf of the condition, in the
/// condition's own words, ticked by the watcher's live samples; a footer
/// that says parked, how long is left, and what mail may interrupt. When
/// the fire arrives the rows keep their last state — the one that fired
/// stays checked — and the footer says what happened and how long it
/// took. With no live state at all — a resumed session, whose record
/// cannot say which leaf fired — the block collapses to the head and one
/// quiet line, because half a checklist would be a guess.
fn wait_block(
    state: &UiState,
    input: &str,
    output: Option<&str>,
    view: Option<&crate::state::WaitView>,
    width: usize,
) -> Vec<Line<'static>> {
    let theme = &state.theme;
    let label = state.labels.label("wait_for");
    let state_tick = state.tick;
    let tags = &state.wait_tags;
    let tool = theme.fg("tool");
    let faint = theme.fg("faint");
    let quiet = theme.fg("faint").add_modifier(Modifier::ITALIC);
    let mut lines = vec![Line::from(vec![
        Span::styled("→ ", tool),
        Span::styled(
            label.to_string(),
            tool_colour("wait_for").add_modifier(Modifier::BOLD),
        ),
    ])];

    // What it waits on, from its own input — the condition the model
    // wrote, not the JSON it was serialised to.
    let v: Value = serde_json::from_str(input.trim()).unwrap_or(Value::Null);
    let note = v.get("note").and_then(Value::as_str);
    let wake = v.get("wake").and_then(Value::as_u64).unwrap_or(1);
    let parsed = v
        .get("condition")
        .and_then(|c| eidolon_core::wait::Condition::parse(c).ok());
    let tree = match &parsed {
        Some(c) => {
            let mut out = Vec::new();
            wait_tree(c, 0, &mut out, &mut Vec::new());
            out
        }
        None => Vec::new(),
    };

    match view {
        // Live: the checklist, ticked by the watcher's own samples.
        Some(view) => {
            // One tag column: every kind word padded to the widest, so
            // the subjects read down a clean edge.
            let tag_width = tree
                .iter()
                .filter_map(|(_, n)| match n {
                    WaitNode::Leaf { parts, .. } => {
                        Some(ratatui::text::Span::raw(tags.tag(parts.kind).word.clone()).width())
                    }
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            for (depth, node) in &tree {
                // Children hang off a faint rail, so a group reads as one
                // group and two `or` branches never blur into a paragraph.
                let prefix = if *depth > 0 {
                    format!("  \u{2506}  {}", "  ".repeat((*depth - 1) * 2))
                } else {
                    "  ".to_string()
                };
                match node {
                    WaitNode::Label(word) => {
                        lines.push(Line::styled(format!("{prefix}\u{25b8} {word}"), quiet))
                    }
                    WaitNode::Leaf { parts, at } => {
                        let tag = tags.tag(parts.kind);
                        let tag_style = if theme.name(&tag.colour).is_empty() {
                            Style::default().fg(crate::render::color(&tag.colour))
                        } else {
                            theme.fg(&tag.colour)
                        };
                        // Pending is not a dot but a spinner, turning on
                        // the tick the countdown already runs — the row
                        // is being watched right now, and the block says
                        // so. Rows stagger off the tick, so they do not
                        // all nod in unison.
                        let glyph = match view.rows.get(*at) {
                            Some(row) if row.met => Span::styled("\u{2713} ", theme.fg("ok")),
                            Some(row) if row.gone.is_some() => {
                                Span::styled("\u{2717} ", theme.fg("error"))
                            }
                            _ => Span::styled(
                                SPINNER[(state_tick as usize).wrapping_add(*at) % SPINNER.len()]
                                    .to_string(),
                                faint,
                            ),
                        };
                        let mut head = vec![
                            glyph,
                            Span::raw("  "),
                            Span::styled(
                                format!("{:<w$}", tag.word, w = tag_width),
                                tag_style,
                            ),
                            Span::raw("  "),
                            Span::styled(
                                parts.subject.clone(),
                                theme.fg("sent").add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(" \u{b7} ", faint),
                            Span::styled(parts.predicate.clone(), theme.fg("text")),
                        ];
                        for q in &parts.qualifiers {
                            head.push(Span::styled(
                                format!("  \u{b7} {q}"),
                                faint,
                            ));
                        }
                        let hang =
                            ratatui::text::Span::raw(prefix.clone()).width() + 2 + tag_width + 2;
                        lines.extend(crate::markdown::wrap_spans(
                            head,
                            width,
                            &[Span::raw(prefix.clone())],
                            &[Span::raw(" ".repeat(hang))],
                        ));
                    }
                }
            }
            lines.push(Line::default());
            match &view.settled {
                Some(outcome) => {
                    let words = match outcome.waited_s {
                        Some(s) => format!("{} after {}", outcome.words, fmt_wait(s)),
                        None => outcome.words.clone(),
                    };
                    lines.push(Line::from(vec![
                        Span::styled("  \u{21b3} ", tool),
                        Span::styled(words, theme.fg("ok")),
                    ]));
                }
                None => {
                    lines.push(Line::from(vec![
                        Span::styled("  \u{21b3} ", tool),
                        Span::styled("parked", faint.add_modifier(Modifier::ITALIC)),
                        Span::styled(
                            match view.deadline_s {
                                0 => String::new(),
                                s => {
                                    let left =
                                        s.saturating_sub(view.armed_at.elapsed().as_secs());
                                    format!(" \u{b7} {} left", fmt_deadline(left))
                                }
                            },
                            faint.add_modifier(Modifier::ITALIC),
                        ),
                        Span::styled(
                            format!(" \u{b7} wake {wake}"),
                            faint.add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                }
            }
        }
        // No live view — a resume, mostly: the block collapses, because
        // the record cannot say which row fired and a guessed tick would
        // be a lie shaped like the truth. The result's own first line
        // says what the park came to.
        None => {
            if let Some(out) = output {
                let first = out.lines().next().unwrap_or_default();
                let rest = first
                    .strip_prefix("[armed #")
                    .map(|r| match r.split_once(']') {
                        Some((n, _)) => format!("armed #{n}"),
                        None => first.to_string(),
                    })
                    .or_else(|| first.split_once("] ").map(|(_, what)| what.to_string()));
                let body = rest.unwrap_or_else(|| first.to_string());
                let deadline = out
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("deadline: ")
                            .and_then(|s| s.strip_suffix('s'))
                            .and_then(|s| s.parse::<u64>().ok())
                    })
                    .map(fmt_deadline);
                lines.push(Line::from(vec![
                    Span::styled("  \u{21b3} ", tool),
                    Span::styled(
                        match deadline {
                            Some(d) => format!("{body} \u{b7} deadline {d}"),
                            None => body,
                        },
                        theme.fg("text"),
                    ),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled("  \u{21b3} ", tool),
                    Span::styled("arming\u{2026}", faint.add_modifier(Modifier::ITALIC)),
                ]));
            }
        }
    }
    // The model's own reason, in its own words — the one artifact in
    // the block written for a human reader rather than a watcher, so
    // it draws in both paths: live, and resumed with no view at all.
    // Wrapped under the lead like the leaves above, because a
    // paragraph-length note is exactly what a narrow pane cuts.
    if let Some(note) = note {
        // ASCII lead, so bytes are columns.
        let lead = "    because: ";
        let hang = " ".repeat(lead.len());
        lines.extend(crate::markdown::wrap_spans(
            vec![Span::styled(note.to_string(), quiet)],
            width,
            &[Span::styled(lead.to_string(), quiet)],
            &[Span::styled(hang, quiet)],
        ));
    }
    lines.push(Line::default());
    lines
}

fn tool_block(
    name: &str,
    label: &str,
    input: &str,
    output: Option<(&str, bool)>,
    width: usize,
    expanded: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let tool = theme.fg("tool");
    let faint = theme.fg("faint").add_modifier(Modifier::ITALIC);
    let mut head = vec![
        Span::styled("→ ", tool),
        // The wire name keeps the colour hash and every lookup; the label
        // is only what the eye reads.
        Span::styled(label.to_string(), tool_colour(name).add_modifier(Modifier::BOLD)),
    ];
    // What the diff below is about to show is not also said on the head
    // line: an edit's head is the path it touches.
    let diff = edit_diff(name, input);
    let hide: &[&str] = match diff {
        Some(_) => &["old_str", "new_str", "content"],
        None => &[],
    };
    head.extend(args_spans(input, theme, hide));
    let mut lines = vec![clip(head, width, theme)];
    if let Some(diff) = diff {
        lines.extend(diff_lines(&diff, &diff_lang(input), width, expanded, theme));
    }

    match output {
        None => lines.push(Line::from(vec![
            Span::styled("  ↳ ", tool),
            Span::styled("running…", faint),
        ])),
        Some((out, is_err)) => lines.extend(output_block(out, is_err, width, expanded, theme)),
    }
    lines.push(Line::default());
    lines
}

/// A tool's output, as drawn under the return arrow: read, not skimmed —
/// `text`, not `faint` — capped at [`TOOL_LINES`] lines unless the
/// transcript is expanded, the cap saying what it hid. A unified diff is
/// painted by its signs. Shared by every caller that draws a result, so
/// the cap and the painting cannot drift between tool calls and asks.
fn output_block(
    out: &str,
    is_err: bool,
    width: usize,
    expanded: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let tool = theme.fg("tool");
    let faint = theme.fg("faint").add_modifier(Modifier::ITALIC);
    let mut lines = Vec::new();
    // Output is read, not skimmed: `text`, not `faint`.
    let style = if is_err {
        theme.fg("error")
    } else {
        theme.fg("text")
    };
    let diffy = is_unified_diff(out);
    let body: Vec<&str> = out.trim_end().lines().collect();
    if body.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  ↳ ", tool),
            Span::styled("(no output)", faint),
        ]));
    }
    let shown = if expanded {
        body.len()
    } else {
        TOOL_LINES.min(body.len())
    };
    for (i, l) in body.iter().take(shown).enumerate() {
        let arrow = if i == 0 {
            Span::styled("  ↳ ", tool)
        } else {
            Span::raw("    ")
        };
        // A diff is read by its signs, so it is painted by them.
        let style = match diffy {
            true => match l.as_bytes().first() {
                Some(b'+') => style.patch(theme.bg("added")),
                Some(b'-') => style.patch(theme.bg("removed")),
                Some(b'@') => theme.fg("heading"),
                _ => style,
            },
            false => style,
        };
        let cell = Span::styled(l.replace('\t', "    "), style);
        if expanded {
            lines.extend(crate::markdown::wrap_spans(
                vec![cell],
                width,
                &[arrow],
                &[Span::raw("    ")],
            ));
        } else {
            lines.push(clip(vec![arrow, cell], width, theme));
        }
    }
    if body.len() > shown {
        let n = body.len() - shown;
        lines.push(Line::styled(
            format!(
                "    … +{n} more line{} — space o",
                if n == 1 { "" } else { "s" }
            ),
            faint,
        ));
    }
    lines
}

/// A tool's input as arguments a person reads. Input that does not parse —
/// it is still streaming in, or the tool takes something other than an
/// object — falls back to its flattened text.
/// A tool's name in a hue of its own, hashed from the name so it is the
/// same everywhere; the arrows keep the theme's `tool` role.
fn tool_colour(name: &str) -> Style {
    const PALETTE: [Color; 6] = [Color::Magenta, Color::Cyan, Color::Green, Color::Yellow, Color::Blue, Color::LightRed];
    let h = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    Style::default().fg(PALETTE[(h % PALETTE.len() as u32) as usize])
}

fn args_spans(input: &str, theme: &Theme, hide: &[&str]) -> Vec<Span<'static>> {
    let t = input.trim();
    if t.is_empty() {
        return Vec::new();
    }
    let faint = theme.fg("faint");
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(t) else {
        return vec![Span::styled(format!(" {}", one_line(t, 80)), faint)];
    };
    let primary = PRIMARY
        .iter()
        .find(|k| map.contains_key(**k))
        .map(|k| (*k).to_string())
        .or_else(|| (map.len() == 1).then(|| map.keys().next().cloned().unwrap_or_default()));
    let mut out = Vec::new();
    // The main argument is what was sent: the `sent` role, bold.
    if let Some(k) = primary.as_deref().and_then(|k| map.get(k)) {
        out.push(Span::styled(
            format!(" {}", scalar(k, 72)),
            theme.fg("sent").add_modifier(Modifier::BOLD),
        ));
    }
    for (k, v) in map
        .iter()
        .filter(|(k, _)| primary.as_deref() != Some(k.as_str()) && !hide.contains(&k.as_str()))
    {
        out.push(Span::styled(format!("  {k}={}", scalar(v, 32)), faint));
    }
    out
}

/// A JSON value the way a person reads it: strings unquoted, whitespace
/// folded, anything structured left as JSON. Clipped to `max` columns.
fn scalar(v: &Value, max: usize) -> String {
    match v {
        Value::String(s) => one_line(s, max),
        other => one_line(&other.to_string(), max),
    }
}

/// Truncate a span list to `width` columns, marking the cut.
fn clip(spans: Vec<Span<'static>>, width: usize, theme: &Theme) -> Line<'static> {
    let mut used = 0usize;
    let mut out = Vec::new();
    for s in spans {
        let n = s.content.chars().count();
        if used + n <= width {
            used += n;
            out.push(s);
            continue;
        }
        let room = width.saturating_sub(used + 1);
        if room > 0 {
            let cut: String = s.content.chars().take(room).collect();
            out.push(Span::styled(cut, s.style));
        }
        out.push(Span::styled("…", theme.fg("faint")));
        break;
    }
    Line::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::state::{Selection, UiState};

    fn painted_links(state: &UiState, width: u16, height: u16) -> (ratatui::buffer::Buffer, Vec<crate::vault::Hit>) {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        let mut renderer = Renderer::new(state);
        terminal.draw(|f| renderer.draw(f, &serde_json::json!({"kind": "transcript"}))).unwrap();
        (terminal.backend().buffer().clone(), renderer.link_hits)
    }

    #[test]
    fn clicks_use_painted_alias_cells_including_wide_prefixes_scroll_and_resize() {
        let mut s = UiState::new("m".into(), "log".into(), "/".into());
        s.transcript.push(Entry::Assistant { text: "界 [[First|alpha]] [[Second|beta]]".into(), streaming: false });
        for width in [12, 30] {
            let (buf, hits) = painted_links(&s, width, 10);
            for target in ["First", "Second"] {
                let h = hits.iter().find(|h| h.target == target).unwrap();
                assert_eq!(buf[(h.rect.x, h.rect.y)].symbol(), if target == "First" { "a" } else { "b" });
            }
            assert!(!hits.iter().any(|h| h.rect.contains((0, 0).into())));
        }
        s.transcript[0] = Entry::Assistant {
            text: (0..40).map(|i| format!("[[Note{i}|label{i}]]")).collect::<Vec<_>>().join("\n"), streaming: false,
        };
        s.scroll_up = 10;
        let (buf, hits) = painted_links(&s, 30, 8);
        assert!(!hits.is_empty());
        for h in hits {
            let expected = format!("label{}", h.target.strip_prefix("Note").unwrap());
            let painted: String = (h.rect.x..h.rect.right()).map(|x| buf[(x, h.rect.y)].symbol()).collect();
            assert_eq!(painted, expected);
        }
    }

    #[test]
    fn struck_gutters_shift_link_ranges_and_link_focus_marks_only_the_alias() {
        let mut s = UiState::new("m".into(), "log".into(), "/".into());
        s.transcript.push(Entry::Assistant { text: "Source: [[First|alpha]]".into(), streaming: false });
        s.records.insert(0, 7);
        s.struck.insert(7);
        s.prompt.mode = crate::edit::Mode::named("trace", crate::edit::Kind::Normal);
        s.trace.live = true;
        s.vault.trace_focus = Some((0, 0));
        let (buf, hits) = painted_links(&s, 40, 8);
        let h = &hits[0];
        assert_eq!(buf[(h.rect.x, h.rect.y)].symbol(), "a");
        assert!(buf[(h.rect.x, h.rect.y)].modifier.contains(Modifier::UNDERLINED));
        assert!(!buf[(h.rect.x - 1, h.rect.y)].modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn a_vault_page_renders_markdown_and_exposes_only_its_own_links() {
        let mut s = UiState::new("m".into(), "log".into(), "/".into());
        s.screen = (60, 20);
        s.transcript.push(Entry::Assistant { text: "[[behind]]".into(), streaming: false });
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        crate::vault::request(&mut s, &tx, "Note".into());
        let id = s.vault.pending.unwrap();
        crate::vault::accept(&mut s, id, Ok(crate::vault::Note {
            path: "Note.md".into(), body: "# Heading\n[[Child|read me]]".into(), anchor: None, scroll: 0, focus: Some(0),
        }));
        let (buf, hits) = painted_links(&s, 60, 20);
        assert!(!hits.is_empty());
        for h in &hits { assert_eq!(h.target, "Child"); }
        let h = &hits[0];
        assert_eq!(buf[(h.rect.x, h.rect.y)].symbol(), "r");
        assert!(buf[(h.rect.x, h.rect.y)].modifier.contains(Modifier::UNDERLINED));
        assert_eq!(buf[(1, 1)].symbol(), "H");
        assert!(buf[(1, 1)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn vault_tags_share_drawn_cells_for_wide_aliases_wrapping_scroll_and_resize() {
        let mut s = UiState::new("m".into(), "log".into(), "/".into());
        s.transcript.push(Entry::Assistant { text: "hidden transcript".into(), streaming: false });
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        crate::vault::request(&mut s, &tx, "Note".into());
        let id = s.vault.pending.unwrap();
        crate::vault::accept(&mut s, id, Ok(crate::vault::Note {
            path: "Note.md".into(), body: "界 [[Child|wide child label]] ordinary\n\n[[Child|second occurrence]]".into(),
            anchor: None, scroll: 0, focus: None,
        }));
        for width in [18, 50] {
            s.transcript_rect = Rect::new(0, 0, width, 8);
            for top in [0, 1] {
                s.vault.page.as_mut().unwrap().selected = top;
                let (before, hits) = painted_links(&s, width, 8);
                let spots = screen_words(&s, s.transcript_rect);
                let links = spots.iter().filter(|s| matches!(s, crate::jump::Spot::Link { .. })).count();
                assert_eq!(links, 2, "one tag per visible occurrence, not per word or destination");
                assert!(!spots.iter().any(|s| matches!(s, crate::jump::Spot::Cell { text, .. } if text == "hidden")));
                s.jump = crate::jump::Jump::new(spots);
                let (tagged, _) = painted_links(&s, width, 8);
                for (spot, label) in s.jump.as_ref().unwrap().spots.iter().zip(&s.jump.as_ref().unwrap().labels) {
                    let (x, y) = match spot {
                        crate::jump::Spot::Link { x, y, .. } => {
                            assert!(hits.iter().any(|h| h.rect.contains((*x, *y).into())));
                            (*x, *y)
                        }
                        crate::jump::Spot::Cell { x, y, .. } => (*x, *y),
                        _ => panic!(),
                    };
                    assert_ne!(before[(x, y)].symbol(), " ");
                    assert_eq!(tagged[(x, y)].symbol(), &label[..1]);
                }
                s.jump = None;
            }
        }
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// The transcript alone, drawn into a `w` x `h` terminal.
    fn rows(state: &UiState, w: u16, h: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        let mut r = Renderer::new(state);
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "transcript" })))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// Draw an arbitrary tree and hand back both the glyphs and, per cell,
    /// whether it came out reversed — the selection is a style, not a
    /// character, so glyphs alone cannot show whether it landed.
    fn draw(
        state: &UiState,
        tree: serde_json::Value,
        w: u16,
        h: u16,
    ) -> (Vec<String>, Vec<String>) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        let mut r = Renderer::new(state);
        terminal.draw(|f| r.draw(f, &tree)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let glyphs = (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        let marks = (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| {
                        if buf[(x, y)].modifier.contains(Modifier::REVERSED) {
                            '^'
                        } else {
                            ' '
                        }
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        (glyphs, marks)
    }

    /// Per cell, `#` where the background came out `c` — the search
    /// highlight is a colour, so glyphs alone cannot show it landed.
    fn painted(state: &UiState, tree: serde_json::Value, w: u16, h: u16, c: Color) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        let mut r = Renderer::new(state);
        terminal.draw(|f| r.draw(f, &tree)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| if buf[(x, y)].bg == c { '#' } else { ' ' })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// One row's foreground colours, for what glyphs cannot show — a ramp.
    /// `painted` is the same read for a background.
    fn foregrounds(state: &UiState, tree: serde_json::Value, w: u16, h: u16, y: u16) -> Vec<Color> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        let mut r = Renderer::new(state);
        terminal.draw(|f| r.draw(f, &tree)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..w).map(|x| buf[(x, y)].fg).collect()
    }

    /// A terminal narrower than the popup's own minimum used to panic the
    /// UI thread outright — `clamp(20, area.width)` with `area.width < 20`
    /// is `min > max`. Found by driving the harness under a pty with no
    /// size; a cramped popup is the right answer, a crash is not.
    #[test]
    fn a_terminal_narrower_than_the_popup_does_not_take_the_ui_thread_down() {
        for w in [0u16, 1, 8, 19, 20, 60] {
            let mut st = with_prompt(":mod");
            st.prompt.enter_insert(true);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(w.max(1), 8)).unwrap();
            let mut r = Renderer::new(&st);
            terminal
                .draw(|f| r.draw(f, &serde_json::json!({ "kind": "prompt" })))
                .unwrap();
        }
    }

    // ------------------------------------------------------------ the caret

    #[test]
    fn the_prompt_caret_is_painted_a_block_in_normal_and_a_bar_in_typing() {
        let tree = serde_json::json!({ "kind": "prompt" });
        let st = with_prompt("hi");
        let (glyphs, marks) = draw(&st, tree.clone(), 24, 3);
        // Per cell, not per byte: the border glyph is three bytes wide.
        let cells: Vec<char> = glyphs[1].chars().collect();
        let marks: Vec<char> = marks[1].chars().collect();
        assert_eq!(&cells[1..3], &['h', 'i'], "the block keeps the character");
        assert_eq!(marks[1], '^', "the block is the reversed cell");

        // Read off the buffer: the `draw` helper shows colour and glyph only through marks.
        let mut typing = with_prompt("hi");
        typing.prompt.enter_insert(false);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(24, 3)).unwrap();
        let mut r = Renderer::new(&typing);
        terminal.draw(|f| r.draw(f, &tree)).unwrap();
        let cell = terminal.backend().buffer()[(1, 1)].clone();
        assert_eq!(cell.symbol(), "\u{258f}", "the bar");
        assert_eq!(cell.fg, st.theme.colour("cursor"), "the bar's colour");
        assert!(!cell.modifier.contains(Modifier::REVERSED), "not a block");
    }

    /// A byte-capturing sink.
    #[derive(Clone, Default)]
    struct Bytes(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl std::io::Write for Bytes {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn no_frame_ever_shows_the_terminal_caret() {
        let sink = Bytes::default();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(sink.clone())).unwrap();
        let st = with_prompt("hi");
        let mut r = Renderer::new(&st);
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "prompt" })))
            .unwrap();
        let bytes = sink.0.borrow().clone();
        assert!(
            !bytes.windows(6).any(|w| w == b"\x1b[?25h"),
            "a frame showed the terminal's own caret: {:?}",
            bytes
        );
        assert!(
            bytes.ends_with(b"\x1b[?25l"),
            "a frame left the native caret visible: {:?}",
            bytes
        );
    }

    // ------------------------------------------------------------ images

    fn shot(name: &str, w: u32, h: u32) -> crate::image::Attachment {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        crate::image::Attachment::from_bytes(name.into(), bytes).unwrap()
    }

    fn with_image(proto: crate::image::Protocol, a: crate::image::Attachment) -> UiState {
        let mut st = with_prompt("");
        st.image_protocol = proto;
        st.cell_size = (8, 16);
        st.transcript.push(Entry::Image(a));
        st
    }

    /// A terminal that cannot draw pixels reserves no rows for them. A
    /// blank hole where a picture would have been says less than the
    /// caption does, and says it while taking a fifth of the screen.
    /// A struck block stays legible and stops looking like context.
    ///
    /// All three markings, because they degrade differently: crossed-out is
    /// patchy across terminals, dim is nearly universal, and the gutter
    /// mark survives both being ignored. A row the model cannot see must
    /// never look like a row it can.
    #[test]
    fn a_struck_block_is_dimmed_crossed_out_and_marked() {
        let theme = Theme::builtin();
        let plain = vec![Line::from(vec![Span::raw("cargo build output")])];
        let out = struck(plain, &theme);
        assert_eq!(out.len(), 1);
        let spans = &out[0].spans;
        assert_eq!(
            spans[0].content, "\u{2298} ",
            "the gutter says so without any colour at all"
        );
        let style = spans[1].style;
        assert!(style.add_modifier.contains(Modifier::DIM));
        assert!(style.add_modifier.contains(Modifier::CROSSED_OUT));
        // Still there to read, and still there to put back.
        assert_eq!(spans[1].content, "cargo build output");
    }

    #[test]
    fn a_terminal_that_draws_nothing_reserves_nothing() {
        let st = with_image(crate::image::Protocol::Off, shot("layout.png", 100, 100));
        let lines = entry_lines(
            &st,
            &st.transcript[0],
            40,
            Detail::Folded,
            st.pixel_cell(),
            "COGITAT",
        );
        // The caption and the blank the block ends on, and nothing else.
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].to_string().contains("layout.png"),
            "{:?}",
            lines[0]
        );
    }

    /// One that can gets the caption *and* a box of the right shape. A
    /// cell is about twice as tall as it is wide, so a square picture is
    /// half as many rows as it is columns.
    #[test]
    fn a_terminal_that_draws_pixels_reserves_a_box_of_the_right_shape() {
        let st = with_image(crate::image::Protocol::Sixel, shot("layout.png", 100, 100));
        let (cols, rows) = image_box(&shot("layout.png", 100, 100), 40, (8, 16));
        assert_eq!((cols, rows), (37, 19));
        let lines = entry_lines(
            &st,
            &st.transcript[0],
            40,
            Detail::Folded,
            st.pixel_cell(),
            "COGITAT",
        );
        assert_eq!(lines.len(), rows as usize + 2);
    }

    /// When the height cap bites it is the *width* that gives. Shrinking
    /// one axis alone would hand the picture a box of the wrong shape, and
    /// a protocol asked to fill a box of the wrong shape stretches to it.
    #[test]
    fn a_very_tall_image_is_capped_in_both_axes_and_keeps_its_shape() {
        let (cols, rows) = image_box(&shot("tall.png", 10, 200), 120, (8, 16));
        assert_eq!(rows, 20);
        assert!(
            cols < 20,
            "the box stayed wide while the height was capped: {cols}\u{d7}{rows}"
        );
    }

    /// The caption is always drawn, on every terminal. It is what a
    /// session over ssh into something plain has instead of the picture,
    /// and what the search has to land on.
    #[test]
    fn the_caption_names_the_file_and_says_how_big_it_is() {
        for proto in [
            crate::image::Protocol::Off,
            crate::image::Protocol::Sixel,
            crate::image::Protocol::Kitty,
        ] {
            let st = with_image(proto, shot("layout.png", 20, 10));
            let lines = entry_lines(
                &st,
                &st.transcript[0],
                60,
                Detail::Folded,
                st.pixel_cell(),
            "COGITAT",
            );
            let caption = lines
                .iter()
                .map(ToString::to_string)
                .find(|l| l.contains("layout.png"))
                .unwrap_or_default();
            assert!(caption.contains("20\u{d7}10"), "{proto:?}: {caption}");
        }
    }

    /// The placement comes back in screen coordinates, over the rows the
    /// layout reserved — derived from the lines that were actually drawn,
    /// so the box cannot drift from them.
    #[test]
    fn a_drawn_image_reports_where_its_pixels_go() {
        let mut st = with_image(crate::image::Protocol::Sixel, shot("layout.png", 40, 20));
        st.view_height = 20;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 20)).unwrap();
        let mut r = Renderer::new(&st);
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "transcript" })))
            .unwrap();
        assert_eq!(r.images.len(), 1, "{:?}", r.images);
        let p = &r.images[0];
        assert_eq!(p.col, 2, "the box is indented like the messages around it");
        assert_eq!(p.media_type, "image/png");
        assert!(p.rows > 0 && p.cols > 0);
    }

    /// An image only partly on screen is not drawn at all: a graphics
    /// protocol draws from the top-left of the box it is given, so a
    /// half-scrolled picture would be drawn whole, over what is above it.
    #[test]
    fn a_clipped_image_is_left_to_its_caption() {
        let mut st = with_image(crate::image::Protocol::Sixel, shot("layout.png", 40, 40));
        st.view_height = 4;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 4)).unwrap();
        let mut r = Renderer::new(&st);
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "transcript" })))
            .unwrap();
        assert!(
            r.images.is_empty(),
            "a clipped image was drawn anyway: {:?}",
            r.images
        );
    }

    /// The bug Noah hit: open the model picker over a transcript holding
    /// an image and the picker came up with its left edge missing and its
    /// rows sitting on the pixels underneath.
    ///
    /// The transcript is drawn first and every overlay after it, so a cell
    /// flagged `Skip` while laying out the transcript suppresses whatever
    /// the picker later writes into it — the diff simply declines to send
    /// those cells. An image an overlay covers must therefore be dropped,
    /// not merely left undrawn: its pixels would sit on top of the dialog,
    /// and its cells have to reach the terminal so the dialog is whole.
    #[test]
    fn a_dialog_over_an_image_is_drawn_whole_and_the_image_is_dropped() {
        let mut st = with_image(crate::image::Protocol::Sixel, shot("layout.png", 40, 20));
        st.view_height = 20;
        // The picture is on screen and settled, which is the state that
        // would mark its cells skip on the next frame.
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 20)).unwrap();
        let mut r = Renderer::new(&st);
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "transcript" })))
            .unwrap();
        assert_eq!(
            r.images.len(),
            1,
            "the image should be placed with nothing over it"
        );
        st.drawn_images = r.images.clone();

        // Now the model picker, over the same transcript.
        st.dialog = Some(crate::state::Dialog {
            kind: crate::state::DialogKind::Pick,
            prompt: "filter:".into(),
            options: (0..8)
                .map(|i| (format!("model-{i}"), Some("a provider".into())))
                .collect(),
            keys: (0..8).map(|i| format!("k{i}")).collect(),
            weights: Vec::new(),
            selected: 0,
            input: crate::state::Dialog::field(),
            reply: None,
            pick: Some(crate::state::PickAction::Run("model".into())),
            body: String::new(),
            masked: false,
        });
        let mut r = Renderer::new(&st);
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "transcript" })))
            .unwrap();
        assert!(
            r.images.is_empty(),
            "an image under a dialog must be dropped, not drawn over it: {:?}",
            r.images
        );

        // And not one cell the dialog owns may be held back from the diff.
        let buf = terminal.backend().buffer().clone();
        let skipped: Vec<(u16, u16)> = (0..20u16)
            .flat_map(|y| (0..60u16).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].diff_option == ratatui::buffer::CellDiffOption::Skip)
            .collect();
        assert!(
            skipped.is_empty(),
            "cells withheld from the diff while a dialog is up: {skipped:?}"
        );
    }

    /// Put a live, accepted search on `st` — one whose line has come
    /// down, which is what a `Search` with no minibuffer beside it is.
    fn searching(st: &mut UiState, pattern: &str) {
        let mut s = crate::search::Search::new(crate::search::Dir::Forward, 0, 0);
        s.pattern = pattern.to_string();
        s.refresh(&st.transcript, st.prompt.text(), 0);
        st.search = Some(s);
        st.mini = None;
    }

    /// Put a one-line mode in front of the prompt, with `text` on it.
    fn mini(st: &mut UiState, kind: crate::state::MiniKind, text: &str) {
        let mut m = crate::state::Mini::new(kind);
        m.input.insert(text);
        st.mini = Some(m);
    }

    /// The same search with its line still up.
    fn search_line(st: &mut UiState, pattern: &str) {
        searching(st, pattern);
        mini(
            st,
            crate::state::MiniKind::Search(crate::search::Dir::Forward),
            pattern,
        );
    }

    fn with_prompt(text: &str) -> UiState {
        let mut st = UiState::new("m".into(), "/tmp/s.eid".into(), "/".into());
        // The default script's tables, as a running harness has them.
        st.modes = crate::script::default_modes();
        st.theme = crate::script::default_theme();
        st.prompt.insert(text);
        st.prompt.enter_normal();
        st.prompt.goto_buffer_start();
        st
    }

    /// The regression that put the prompt in the middle of the screen: an
    /// overlay left in the constraint list is invisible but not free.
    #[test]
    fn an_overlay_in_a_column_costs_no_rows() {
        let mut st = with_prompt("hi");
        st.transcript.push(Entry::Info("first".into()));
        st.transcript.push(Entry::Info("last".into()));
        let tree = serde_json::json!({ "kind": "column", "children": [
            { "size": "fill", "child": { "kind": "transcript" } },
            { "size": 1, "child": { "kind": "status", "left": "s", "right": "" } },
            { "kind": "menu" },
            { "kind": "dialog" },
        ]});
        let (glyphs, _) = draw(&st, tree, 20, 6);
        // The status line is on the last row and the transcript owns every
        // row above it. With the overlays left in the constraint list they
        // take a `Fill(1)` share each and the status line lands mid-screen.
        assert_eq!(glyphs[5], "s");
        assert_eq!(glyphs[0], "\u{2022} first");
        assert_eq!(glyphs[1], "\u{2022} last");
    }

    /// The one that would actually have caught it: draw the tree the real
    /// `ui/default.rn` returns, not a hand-built stand-in.
    #[test]
    fn the_default_layout_puts_the_prompt_at_the_bottom_of_the_screen() {
        let st = with_prompt("");
        let script = crate::script::UiScript::compile(crate::DEFAULT_UI).unwrap();
        let tree = script.view(st.snapshot()).unwrap();
        // 80 columns: the sprite rides past the wordmark, on screen.
        let (glyphs, _) = draw(&st, tree, 80, 24);
        // 1 spacer + transcript + 3 prompt + 1 status, bottom-anchored.
        assert!(
            glyphs[23].starts_with(" NOR"),
            "status line off the bottom row: {:?}",
            glyphs[23]
        );
        assert!(
            glyphs[20].starts_with('┌'),
            "prompt box not just above it: {:?}",
            glyphs[20]
        );
        assert!(
            glyphs[22].starts_with('└'),
            "prompt box not 3 rows tall: {:?}",
            glyphs[22]
        );
        // With nothing said yet the start screen stands where the transcript would.
        assert!(
            glyphs[2..8]
                .iter()
                .all(|r| r.contains('█') || r.contains('▄') || r.contains('▀')),
            "start screen art not on the first rows: {glyphs:?}"
        );
        assert_eq!(glyphs[8], "", "the wordmark is six rows: {glyphs:?}");
        assert!(
            glyphs[4].ends_with("██████"),
            "no sprite beside the wordmark: {:?}",
            glyphs[4]
        );
        assert!(
            glyphs[6].ends_with("██  ██"),
            "no feet under the sprite: {:?}",
            glyphs[6]
        );
        // On a short screen the last key row sits right on the box.
        assert!(!glyphs[19].contains('┌'), "start screen runs into the prompt: {:?}", glyphs[19]);
    }

    /// While a turn runs the pulse takes the prompt frame's title, in
    /// place of the mode's name: the transcript gives up no row for it,
    /// the badge below still says the mode, and the status line keeps
    /// saying what the session is.
    #[test]
    fn a_running_turn_puts_the_pulse_on_the_prompt_frame() {
        let mut st = with_prompt("");
        st.working = true;
        st.tick = 1;
        let script = crate::script::UiScript::compile(crate::DEFAULT_UI).unwrap();
        let tree = script.view(st.snapshot()).unwrap();
        let (glyphs, _) = draw(&st, tree, 80, 24);
        let frame = &glyphs[20];
        assert!(frame.starts_with('┌'), "the prompt box moved: {frame:?}");
        assert!(!frame.contains("insert"), "the mode's name was not replaced: {frame:?}");
        // At rest the same figure stands beside the mode's name.
        let mut idle = with_prompt("");
        idle.working = false;
        let tree = script.view(idle.snapshot()).unwrap();
        let (rest, _) = draw(&idle, tree, 80, 24);
        assert!(rest[20].contains(&crate::life::icon()), "no agent at rest: {:?}", rest[20]);
        assert!(rest[20].contains("normal"), "the mode lost its name at rest: {:?}", rest[20]);
        assert!(frame.contains("COGITAT"), "the word is not on the frame: {frame:?}");
        assert!(
            frame.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)),
            "no life on the frame: {frame:?}"
        );
        assert!(glyphs[19].trim().is_empty(), "the pulse still took a row: {:?}", glyphs[19]);
        assert!(glyphs[23].contains('m'), "the status line lost the model: {:?}", glyphs[23]);
        // The strip is shaded along its length, so its two ends are not
        // the same colour.
        let tree = script.view(st.snapshot()).unwrap();
        let ramp: Vec<Color> = foregrounds(&st, tree, 80, 24, 20)
            .into_iter()
            .filter(|c| matches!(c, Color::Rgb(..)))
            .collect();
        assert!(ramp.len() > 2, "the strip is not painted per cell: {ramp:?}");
        assert_ne!(ramp.first(), ramp.last(), "the strip is one flat colour");
    }

    /// The prompt box follows what is in it. It was pinned at three rows
    /// — one of text — so a message with a second line showed only the
    /// line the caret was on, and a paste was a box you could not read.
    #[test]
    fn the_prompt_box_grows_with_the_message() {
        let st = with_prompt("one\ntwo\nthree");
        let script = crate::script::UiScript::compile(crate::DEFAULT_UI).unwrap();
        let tree = script.view(st.snapshot()).unwrap();
        let (glyphs, _) = draw(&st, tree, 40, 24);
        assert!(
            glyphs[23].starts_with(" NOR"),
            "status line moved: {:?}",
            glyphs[23]
        );
        // Three text rows plus two borders, sitting on the status line.
        assert!(
            glyphs[18].starts_with('\u{250c}'),
            "box does not start at 18: {glyphs:?}"
        );
        assert!(glyphs[19].contains("one"));
        assert!(glyphs[20].contains("two"));
        assert!(glyphs[21].contains("three"));
        assert!(
            glyphs[22].starts_with('\u{2514}'),
            "box does not end at 22: {glyphs:?}"
        );
    }

    /// And stops at its ceiling rather than eating the transcript: past
    /// the cap the field scrolls, which is what the drawn rows already
    /// did inside a fixed box.
    #[test]
    fn a_long_message_stops_at_the_boxs_ceiling() {
        let st = with_prompt(
            &(1..=30)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let script = crate::script::UiScript::compile(crate::DEFAULT_UI).unwrap();
        let tree = script.view(st.snapshot()).unwrap();
        let (glyphs, _) = draw(&st, tree, 40, 24);
        assert!(
            glyphs[11].starts_with('\u{250c}'),
            "the ceiling is 12 rows: {glyphs:?}"
        );
        assert!(
            glyphs[22].starts_with('\u{2514}'),
            "the ceiling is 12 rows: {glyphs:?}"
        );
        assert!(
            glyphs[23].starts_with(" NOR"),
            "the status line is still on the bottom row"
        );
    }

    /// A wrapped single line counts too: the measurement is the same wrap
    /// the draw does, sigil included, so the box cannot be a row short of
    /// its own text.
    #[test]
    fn a_wrapped_line_is_measured_as_the_rows_it_takes() {
        let st = with_prompt("aaaa bbbb cccc");
        assert_eq!(
            prompt_rows(&st, 8),
            5,
            "6 usable columns is three rows of text"
        );
        assert_eq!(prompt_rows(&st, 40), 3);
    }

    /// A dialog row read inside its borders: the dialog is centered, so
    /// every row carries padding and a border, and indentation is only
    /// meaningful once those are off.
    fn inner(r: &str) -> &str {
        r.trim()
            .trim_start_matches('│')
            .trim_end_matches('│')
            .trim_end()
    }

    /// A question's options are paragraphs: the question wraps, each
    /// description wraps indented under its label, and a description too
    /// long for [`DESC_ROWS`] rows is cut with an ellipsis rather than
    /// pushing the other options off the screen.
    #[test]
    fn a_choice_wraps_its_paragraphs_and_caps_them() {
        let mut st = with_prompt("");
        st.dialog = Some(crate::state::Dialog {
            kind: DialogKind::Choose,
            prompt: "The deploy pipeline found three viable shapes for the \
                     migration window; which risk do you want to carry?"
                .into(),
            options: vec![
                (
                    "staging first".into(),
                    Some(
                        "Ship to staging tonight, soak it for a day, promote Thursday. \
                          Slowest, and the only option where a bad index is caught by \
                          someone other than a customer."
                            .into(),
                    ),
                ),
                ("prod now".into(), Some("Flip the flag in one go.".into())),
                ("hold".into(), None),
            ],
            keys: Vec::new(),
            weights: Vec::new(),
            selected: 0,
            input: crate::state::Dialog::field(),
            reply: None,
            pick: None,
            body: String::new(),
            masked: false,
        });
        // 30 rows: the box holds every block plus the legend without the
        // frame's height cap clipping the tail.
        let (glyphs, _) = draw(&st, serde_json::json!({ "kind": "dialog" }), 40, 30);
        let text = glyphs.join("\n");
        // The rows are centered with padding and bordered, so read each
        // one inside its borders before matching on indentation.
        // The question is a wrapped paragraph; its phrases are intact on
        // the rows the wrap made of them.
        assert!(
            text.contains("migration window;") && text.contains("which risk do you want"),
            "{text}"
        );
        // The selected option wears the arrow and its number; the others
        // keep their numbers without the arrow.
        assert!(text.contains("▶ 1. staging first"), "{text}");
        assert!(
            text.contains("2. prod now") && !text.contains("▶ 2. prod now"),
            "{text}"
        );
        // The first option's description is an indented paragraph that
        // wrapped and was capped: exactly DESC_ROWS rows, the last one
        // saying it was cut.
        let one = glyphs
            .iter()
            .position(|r| r.contains("1. staging first"))
            .unwrap();
        let two = glyphs
            .iter()
            .position(|r| r.contains("2. prod now"))
            .unwrap();
        let para: Vec<&str> = glyphs[one + 1..two]
            .iter()
            .map(|r| inner(r))
            .filter(|r| r.starts_with("    "))
            .collect();
        assert!(para.len() >= 2, "the description wrapped: {para:?}");
        assert_eq!(para.len(), super::DESC_ROWS, "the cap held: {para:?}");
        assert!(
            para.last().unwrap().ends_with('…'),
            "the cut says so: {para:?}"
        );
        // A description that fits is not cut.
        let three = glyphs.iter().position(|r| r.contains("3. hold")).unwrap();
        let short: Vec<&str> = glyphs[two + 1..three]
            .iter()
            .map(|r| inner(r))
            .filter(|r| r.starts_with("    "))
            .collect();
        assert!(!short.last().unwrap().ends_with('…'), "{short:?}");
        // The digit hint is on the dialog, because a digit selects.
        assert!(
            text.contains("digit") && text.contains("esc dismisses"),
            "{text}"
        );
    }

    /// The scroll window budgets in whole blocks, so the selected option's
    /// paragraph is never half-drawn to fit the frame.
    #[test]
    fn a_choice_keeps_the_selected_block_whole() {
        let mut st = with_prompt("");
        st.dialog = Some(crate::state::Dialog {
            kind: DialogKind::Choose,
            prompt: "which?".into(),
            options: (0..8)
                .map(|i| (format!("option {i}"), Some("word ".repeat(60))))
                .collect(),
            keys: Vec::new(),
            weights: Vec::new(),
            selected: 7,
            input: crate::state::Dialog::field(),
            reply: None,
            pick: None,
            body: String::new(),
            masked: false,
        });
        let (glyphs, _) = draw(&st, serde_json::json!({ "kind": "dialog" }), 40, 16);
        let text = glyphs.join("\n");
        assert!(
            text.contains("▶ 8. option 7"),
            "the selection is drawn: {text}"
        );
        // The selected block's description is whole: all DESC_ROWS rows of
        // it, cut by its own cap (with the ellipsis) and not by the window.
        let para: Vec<&str> = glyphs
            .iter()
            .map(|r| inner(r))
            .filter(|r| r.starts_with("    "))
            .collect();
        assert_eq!(
            para.len(),
            super::DESC_ROWS,
            "the whole block was drawn: {para:?}"
        );
        assert!(para.last().unwrap().ends_with('…'), "{para:?}");
        assert!(
            text.contains("esc dismisses"),
            "the legend wrapped rather than clipping: {text}"
        );
    }

    #[test]
    fn the_prompt_paints_the_selection_a_motion_made() {
        let mut st = with_prompt("hello brave world");
        st.prompt.move_next_word_start(false);
        let (glyphs, marks) = draw(&st, serde_json::json!({ "kind": "prompt" }), 24, 3);
        assert_eq!(glyphs[1], "│hello brave world     │");
        // Exactly "hello " — the trailing space included, and nothing past it.
        assert_eq!(marks[1], " ^^^^^^");
    }

    #[test]
    fn a_selection_that_wraps_is_painted_on_both_rows() {
        let mut st = with_prompt("aaaa bbbb cccc");
        st.prompt.select_all();
        // 8 columns of border leaves 6 usable, so the text takes three rows.
        let (glyphs, marks) = draw(&st, serde_json::json!({ "kind": "prompt" }), 8, 5);
        assert_eq!(glyphs[1], "│aaaa b│");
        assert_eq!(marks[1], " ^^^^^^");
        assert_eq!(marks[2], " ^^^^^^");
    }

    #[test]
    fn an_insert_mode_caret_paints_nothing() {
        let mut st = with_prompt("hello");
        st.prompt.select_all();
        st.prompt.enter_insert(false);
        let (_, marks) = draw(&st, serde_json::json!({ "kind": "prompt" }), 24, 3);
        assert_eq!(marks[1], "");
    }

    #[test]
    fn the_which_key_popup_lists_what_the_chord_can_become() {
        let mut st = with_prompt("x");
        st.menu = Some(crate::state::Menu {
            title: "space".into(),
            items: vec![
                ("m".into(), "model picker".into()),
                ("q".into(), "quit".into()),
            ],
            selected: None,
        });
        let (glyphs, _) = draw(&st, serde_json::json!({ "kind": "menu" }), 40, 10);
        let all = glyphs.join("\n");
        assert!(
            all.contains("space"),
            "the popup should be titled with the open chord:\n{all}"
        );
        assert!(all.contains("m model picker"), "{all}");
        assert!(all.contains("q quit"), "{all}");
    }

    #[test]
    fn the_view_is_anchored_to_the_last_line_and_scrolls_by_lines() {
        let mut st = UiState::new("m".into(), "s".into(), "/".into());
        for n in 1..=500 {
            st.transcript.push(Entry::Info(format!("info {n}")));
        }
        assert_eq!(
            rows(&st, 20, 3),
            vec!["\u{2022} info 498", "\u{2022} info 499", "\u{2022} info 500"]
        );
        st.scroll_up = 2;
        assert_eq!(
            rows(&st, 20, 3),
            vec!["\u{2022} info 496", "\u{2022} info 497", "\u{2022} info 498"]
        );
        // The walk stops once the window is covered rather than laying out
        // all 500 entries.
        let mut r = Renderer::new(&st);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 3)).unwrap();
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "transcript" })))
            .unwrap();
        assert_eq!(r.transcript_lines, 5);
        // The same frame names the row a held scroll anchors to: the
        // window is 496..=498, so the bottom visible row is entry 497's
        // one and only one.
        assert_eq!(r.scroll_anchor, Some((497, 0)));
        // Following, there is nothing to hold — the anchor is still named
        // (the caller drops it; the renderer only reports), and the last
        // row of the transcript is entry 499's.
        st.scroll_up = 0;
        let mut r = Renderer::new(&st);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 3)).unwrap();
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "transcript" })))
            .unwrap();
        assert_eq!(r.scroll_anchor, Some((499, 0)));
    }

    #[test]
    fn a_drag_cuts_the_text_it_covers_out_of_the_frame() {
        let rows: Vec<Vec<String>> = ["hello world  ", "second line  ", "third        "]
            .iter()
            .map(|r| r.chars().map(|c| c.to_string()).collect())
            .collect();
        // Within one row, inclusive of the cell under the pointer.
        let sel = Selection {
            from: (0, 0),
            to: (4, 0),
            dragging: false,
        };
        assert_eq!(selected_text(&rows, sel), "hello");
        // Across rows: to the end of the first, all of the middle, and the
        // head of the last — with the frame's right-hand padding dropped.
        let sel = Selection {
            from: (6, 0),
            to: (5, 2),
            dragging: false,
        };
        assert_eq!(selected_text(&rows, sel), "world\nsecond line\nthird");
        // Dragging backwards selects the same span.
        let back = Selection {
            from: (5, 2),
            to: (6, 0),
            dragging: false,
        };
        assert_eq!(selected_text(&rows, back), selected_text(&rows, sel));
    }

    /// A finished call, with output nobody should see once it is folded.
    fn call(name: &str, ok: bool) -> Entry {
        Entry::Tool {
            id: String::new(),
            name: name.into(),
            input: "{}".into(),
            output: Some(("SECRETUM".into(), !ok)),
        }
    }

    fn with_calls(entries: Vec<Entry>) -> UiState {
        let mut st = UiState::new("m".into(), "/tmp/s.eid".into(), "/".into());
        st.transcript = entries;
        st
    }

    /// A marked line and the answer it was given: what
    /// `UiState::answered` puts on the transcript where the prose and the
    /// answer used to sit apart.
    fn ask(command: &str, answer: &str) -> Entry {
        Entry::Ask {
            commands: vec![crate::state::Marked {
                command: command.into(),
                payloads: Vec::new(),
            }],
            answers: vec![answer.into()],
        }
    }

    /// A payload is an argument, so it draws on the call: the command,
    /// the boxed bytes under it, and only then the answer — the order a
    /// tool call draws its input and its output, and never the payload
    /// stranded in the reply above the command that consumed it.
    #[test]
    fn a_payload_draws_on_the_call_between_command_and_answer() {
        let mut st = with_calls(vec![Entry::Ask {
            commands: vec![crate::state::Marked {
                command: "append $a to the note \"Build Log\"".into(),
                payloads: vec![('a', "the exact bytes\nwith a \"quote\" in them".into())],
            }],
            answers: vec!["[vv] append → ok: appended".into()],
        }]);
        st.detail = Detail::Full;
        let out = rows(&st, 70, 12);
        let command_row = out
            .iter()
            .position(|r| r.contains("→ ! append $a"))
            .expect("the command draws under the call arrow: {out:?}");
        let bytes_row = out
            .iter()
            .position(|r| r.contains("the exact bytes"))
            .expect("the bytes draw boxed under the command: {out:?}");
        let answer_row = out
            .iter()
            .position(|r| r.contains("↳ [vv] append"))
            .expect("the answer draws under the return arrow: {out:?}");
        assert!(
            command_row < bytes_row && bytes_row < answer_row,
            "command, payload, answer, in that order: {out:?}"
        );
        // The hole letter names the box, on its rail.
        assert!(
            out[bytes_row - 1].contains('\u{258e}') && out[bytes_row - 1].contains('a'),
            "the box opens on the hole letter: {out:?}"
        );
        assert!(
            !out.iter().any(|r| r.contains("!!")),
            "no fence line survives the drawing: {out:?}"
        );
    }

    /// A long payload is capped the way an output is — the box says what
    /// it hid, and `space o` has the rest.
    #[test]
    fn a_long_payload_draws_capped_the_way_an_output_does() {
        let bytes = (0..20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let mut st = with_calls(vec![Entry::Ask {
            commands: vec![crate::state::Marked {
                command: "append $a to X".into(),
                payloads: vec![('a', bytes)],
            }],
            answers: vec!["[vv] append → ok: appended".into()],
        }]);
        st.detail = Detail::Calls;
        let out = rows(&st, 70, 30);
        assert!(out.iter().any(|r| r.contains("line 0")), "{out:?}");
        assert!(
            out.iter().any(|r| r.contains("… +") && r.contains("space o")),
            "the cap says what it hid: {out:?}"
        );
        assert!(!out.iter().any(|r| r.contains("line 19")), "the cap is real: {out:?}");
        st.detail = Detail::Full;
        let out = rows(&st, 70, 40);
        assert!(out.iter().any(|r| r.contains("line 19")), "full detail has it all: {out:?}");
    }

    /// A parked wait draws as a `wait_for` call, an armed wait draws as
    /// the checklist it is: head says Waiting, the leaves say what they
    /// wait on in the condition's own words, ticked rows, a footer.
    #[test]
    fn an_armed_wait_draws_as_a_ticked_checklist() {
        let input = r#"{"condition": {"any": [
            {"all": [{"command_finished": "/tmp/bg-1.log"}, {"peer_idle": "eidolon-b26d"}]},
            {"file_exists": "/tmp/ready"}
        ]}, "timeout_s": 900}"#;
        let mut st = with_calls(vec![Entry::Tool {
            id: "w1".into(),
            name: "wait_for".into(),
            input: input.into(),
            output: Some(("[armed #3] waiting for …".into(), false)),
        }]);
        st.detail = Detail::Full;
        st.waits.insert(
            "w1".into(),
            crate::state::WaitView {
                rows: vec![
                    eidolon_core::wait::ParkRow { met: true, gone: None },
                    eidolon_core::wait::ParkRow { met: false, gone: None },
                    eidolon_core::wait::ParkRow { met: false, gone: None },
                ],
                armed_at: std::time::Instant::now(),
                deadline_s: 900,
                settled: None,
            },
        );
        let out = rows(&st, 78, 20);
        assert!(
            out.iter().any(|r| r.contains("→ Waiting")),
            "the head is the label, not the wire name: {out:?}"
        );
        // The compound shape reads as a tree: `all of` under `or`, one row
        // per leaf, the fired one checked.
        assert!(out.iter().any(|r| r.contains("or")), "{out:?}");
        assert!(out.iter().any(|r| r.contains("all of")), "{out:?}");
        assert!(
            out.iter()
                .any(|r| r.contains("\u{2713}") && r.contains("bg-1 \u{b7} has finished running")),
            "the met leaf is checked: {out:?}"
        );
        assert!(
            out.iter().any(|r| r.contains("eidolon-b26d \u{b7} is idle")),
            "leaves speak the condition's own words: {out:?}"
        );
        assert!(
            out.iter().any(|r| r.contains("/tmp/ready \u{b7} exists")),
            "{out:?}"
        );
        assert!(
            out.iter().any(|r| r.contains("parked") && r.contains("left")),
            "the footer says parked with a countdown: {out:?}"
        );
        // And no raw JSON anywhere.
        assert!(!out.iter().any(|r| r.contains("{\"")), "{out:?}");
    }

    /// A wait with no live state — a resumed session — collapses: the head
    /// and what the result said, never a half-drawn checklist.
    #[test]
    fn a_resumed_wait_collapses_instead_of_guessing_its_rows() {
        let mut st = with_calls(vec![Entry::Tool {
            id: "w1".into(),
            name: "wait_for".into(),
            input: r#"{"condition": {"peer_idle": "eidolon-b26d"}, "timeout_s": 900}"#.into(),
            output: Some(
                (
                    "[armed #3] waiting for session eidolon-b26d is idle\nwake: 1 (…)\ndeadline: 900s\n…"
                        .into(),
                    false,
                ),
            ),
        }]);
        st.detail = Detail::Full;
        let out = rows(&st, 70, 10);
        assert!(out.iter().any(|r| r.contains("→ Waiting")), "{out:?}");
        assert!(
            out.iter().any(|r| r.contains("armed #3")),
            "{out:?}"
        );
        assert!(
            out.iter().any(|r| r.contains("15m")),
            "the deadline is a time a person reads: {out:?}"
        );
        assert!(
            !out.iter().any(|r| r.contains("\u{2713}")),
            "no tick may be invented: {out:?}"
        );
        assert!(!out.iter().any(|r| r.contains("parked")), "{out:?}");
    }

    /// The note is the one artifact in a wait block written for a human
    /// reader rather than a watcher, so it survives the resume: the
    /// collapsed path draws it too, wrapped under its lead, and a long
    /// note wraps rather than truncating at the pane edge.
    #[test]
    fn a_resumed_wait_still_says_why_and_wraps_it() {
        let mut st = with_calls(vec![Entry::Tool {
            id: "w1".into(),
            name: "wait_for".into(),
            input: r#"{"condition": {"peer_idle": "eidolon-b26d"}, "timeout_s": 900, "note": "the rebase onto master pastes commit 2422fb4 and its tests take minutes; waiting here keeps the operator free for the review that needs a person"}"#.into(),
            output: Some(
                (
                    "[armed #3] waiting for session eidolon-b26d is idle\nwake: 1 (…)\ndeadline: 900s\n…"
                        .into(),
                    false,
                ),
            ),
        }]);
        st.detail = Detail::Full;
        let out = rows(&st, 44, 12);
        assert!(
            out.iter().any(|r| r.contains("because: the rebase")),
            "the note draws in the resumed path: {out:?}"
        );
        // No truncation: the note's end arrives, wrapped, below its start.
        let joined = out.join("\n");
        assert!(
            joined.contains("needs a person"),
            "the note is drawn whole: {joined:?}"
        );
        // Wrapped to the pane: nothing drawn runs past the width.
        assert!(
            out.iter().all(|r| r.chars().count() <= 44),
            "the wrapped note stays inside the pane: {out:?}"
        );
    }

    /// Moving the note out of the live arm must not drop it there: a
    /// parked wait still ends with why it waits.
    #[test]
    fn a_parked_wait_still_says_why() {
        let input = r#"{"condition": {"file_exists": "/tmp/ready"}, "timeout_s": 900, "note": "waiting for the fixture the review bot drops"}"#;
        let mut st = with_calls(vec![Entry::Tool {
            id: "w1".into(),
            name: "wait_for".into(),
            input: input.into(),
            output: Some(("[armed #1] waiting for …".into(), false)),
        }]);
        st.detail = Detail::Full;
        st.waits.insert(
            "w1".into(),
            crate::state::WaitView {
                rows: vec![eidolon_core::wait::ParkRow {
                    met: false,
                    gone: None,
                }],
                armed_at: std::time::Instant::now(),
                deadline_s: 900,
                settled: None,
            },
        );
        let out = rows(&st, 60, 12);
        assert!(
            out.iter()
                .any(|r| r.contains("because: waiting for the fixture")),
            "the live block ends with the note: {out:?}"
        );
    }

    /// A wait that came to rest keeps its rows and says how long it took.
    #[test]
    fn a_settled_wait_keeps_its_rows_and_says_what_happened() {
        let mut st = with_calls(vec![Entry::Tool {
            id: "w1".into(),
            name: "wait_for".into(),
            input: r#"{"condition": {"file_exists": "/tmp/ready"}}"#.into(),
            output: Some(("[armed #1] waiting for …".into(), false)),
        }]);
        st.detail = Detail::Full;
        st.waits.insert(
            "w1".into(),
            crate::state::WaitView {
                rows: vec![eidolon_core::wait::ParkRow { met: true, gone: None }],
                armed_at: std::time::Instant::now() - std::time::Duration::from_secs(362),
                deadline_s: 900,
                settled: Some(crate::state::WaitOutcome {
                    words: "it fired".into(),
                    waited_s: Some(362),
                }),
            },
        );
        let out = rows(&st, 70, 12);
        assert!(
            out.iter()
                .any(|r| r.contains("\u{2713}") && r.contains("/tmp/ready \u{b7} exists")),
            "the row that fired stays checked: {out:?}"
        );
        assert!(
            out.iter().any(|r| r.contains("it fired after 6m 02s")),
            "{out:?}"
        );
        assert!(!out.iter().any(|r| r.contains("parked")), "{out:?}");
    }

    /// A parked wait is the one settled call that does not fold: the
    /// checklist ticking in place is the point, and it folds like any
    /// call only once the fire has landed.
    #[test]
    fn a_parked_wait_stands_open_until_it_settles() {
        let mut st = with_calls(vec![
            Entry::User("start the build".into()),
            Entry::Tool {
                id: "w1".into(),
                name: "wait_for".into(),
                input: r#"{"condition": {"file_exists": "/tmp/ready"}, "timeout_s": 900}"#.into(),
                output: Some(("[armed #1] waiting for …".into(), false)),
            },
            Entry::Settle {
                turn: 0,
                text: "settled".into(),
            },
        ]);
        st.detail = Detail::Folded;
        st.waits.insert(
            "w1".into(),
            crate::state::WaitView {
                rows: vec![eidolon_core::wait::ParkRow {
                    met: false,
                    gone: None,
                }],
                armed_at: std::time::Instant::now(),
                deadline_s: 900,
                settled: None,
            },
        );
        let out = rows(&st, 70, 12);
        assert!(
            out.iter().any(|r| r.contains("/tmp/ready")),
            "the row is drawn: {out:?}"
        );
        assert!(
            out.iter().any(|r| r.contains("parked")),
            "the block stands open while armed: {out:?}"
        );
        assert!(
            !out.iter().any(|r| r.contains("Waited on")),
            "a parked wait must not fold into a summary: {out:?}"
        );
        // The fire lands: the block folds like any call, rows and all.
        st.wait_settled("w1", "it fired");
        let out = rows(&st, 70, 12);
        assert!(
            out.iter().any(|r| r.contains("Waited on 1 condition")),
            "settled, it folds: {out:?}"
        );
        assert!(!out.iter().any(|r| r.contains("parked")), "{out:?}");
    }

    /// The exception holds in `fold = "live"` too — the mode that folds
    /// finished runs before the turn ends: a run from the park's own
    /// turn folds beside the parked wait, never over it, and the block
    /// stays drawn while a new turn runs on.
    #[test]
    fn live_folding_folds_around_a_parked_wait_never_over_it() {
        let mut st = with_calls(vec![
            Entry::User("build it".into()),
            Entry::Thinking {
                text: "building".into(),
                streaming: false,
            },
            Entry::Tool {
                id: "b1".into(),
                name: "bash".into(),
                input: "{}".into(),
                output: Some(("wrote /tmp/x".into(), false)),
            },
            Entry::Tool {
                id: "w1".into(),
                name: "wait_for".into(),
                input: r#"{"condition": {"file_exists": "/tmp/ready"}, "timeout_s": 900}"#.into(),
                output: Some(("[armed #1] waiting for …".into(), false)),
            },
            Entry::Settle {
                turn: 0,
                text: "settled".into(),
            },
            Entry::User("status?".into()),
            Entry::Tool {
                id: "r1".into(),
                name: "read".into(),
                input: r#"{"path": "notes/x"}"#.into(),
                output: None,
            },
        ]);
        st.detail = Detail::Folded;
        st.fold = Fold::Live;
        st.working = true;
        st.waits.insert(
            "w1".into(),
            crate::state::WaitView {
                rows: vec![eidolon_core::wait::ParkRow {
                    met: false,
                    gone: None,
                }],
                armed_at: std::time::Instant::now(),
                deadline_s: 900,
                settled: None,
            },
        );
        let out = rows(&st, 70, 20);
        // The neighbour run folds, live mode doing what it is for…
        assert!(
            out.iter()
                .any(|r| r.contains("Ran 1 shell command")),
            "{out:?}"
        );
        // …but the parked wait is drawn, not folded into it or away.
        assert!(
            out.iter().any(|r| r.contains("parked")),
            "the parked wait stands open in live mode: {out:?}"
        );
        assert!(
            out.iter().any(|r| r.contains("/tmp/ready")),
            "{out:?}"
        );
        assert!(
            !out.iter().any(|r| r.contains("Waited on")),
            "a parked wait must not fold into a summary: {out:?}"
        );
    }

    /// The wake is its own line, in the tool's colour and with its own
    /// mark — not the operator's green, not a faint note.
    #[test]
    fn a_wake_draws_as_its_own_kind_of_line() {
        let st = with_calls(vec![Entry::Woke {
            condition: "background bg-1 finished".into(),
            outcome: "it fired".into(),
        }]);
        let out = rows(&st, 70, 4);
        assert!(
            out.iter().any(|r| r.contains("\u{263e}") && r.contains("woke it fired — waiting on background bg-1 finished")),
            "{out:?}"
        );
    }

    /// A marked line is a call, so it is drawn as one — the command under
    /// the call arrow and the answer under the return arrow, which is the
    /// one thing the prose-then-note pair could not say.
    #[test]
    fn a_marked_line_draws_as_the_call_it_became() {
        let mut st = with_calls(vec![ask("list notes", "[vv] list_notes → ok: 42 notes")]);
        st.detail = Detail::Full;
        let out = rows(&st, 60, 3);
        assert!(out[0].starts_with("→ ! list notes"), "{out:?}");
        assert!(
            out[1].starts_with("  ↳ [vv] list_notes → ok: 42 notes"),
            "{out:?}"
        );
    }

    /// An answer whose command the transcript could not pair is still the
    /// harness's words: drawn bare rather than not at all.
    #[test]
    fn an_answer_with_no_command_still_draws() {
        let mut st = with_calls(vec![Entry::Ask {
            commands: Vec::new(),
            answers: vec!["[vv] read_note title=\"X\" → ok: it".into()],
        }]);
        st.detail = Detail::Full;
        let out = rows(&st, 60, 3);
        assert!(out[0].starts_with("→ !"), "{out:?}");
        assert!(out[1].contains("[vv] read_note"), "{out:?}");
    }

    /// The channel used to hand the model a clipped answer — a couple of
    /// hundred characters and `… (+N more chars)` — so an ask drew one short
    /// line. It now hands back whatever the vault said, and that draws the way
    /// a tool's output does: a capped few lines, the rest one `space o` away,
    /// and a count saying how much is behind the cap. Never swallowed, and
    /// never the whole of a long note in the frame either.
    #[test]
    fn a_long_answer_draws_capped_the_way_an_output_does() {
        let body = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut st = with_calls(vec![ask(
            "read the note X",
            &format!("[vv] read_note title=\"X\" → ok: {body}"),
        )]);
        st.detail = Detail::Calls;
        let out = rows(&st, 60, 20);
        assert!(out.iter().any(|r| r.contains("→ ok: line 0")), "{out:?}");
        assert!(
            out.iter().any(|r| r.contains("\u{2026} +") && r.contains("space o")),
            "the cap says what it hid: {out:?}"
        );
        assert!(
            !out.iter().any(|r| r.contains("line 19")),
            "and the cap is real: {out:?}"
        );
        st.detail = Detail::Full;
        let out = rows(&st, 60, 40);
        assert!(out.iter().any(|r| r.contains("line 19")), "{out:?}");
    }

    /// The marked lines are work between two pieces of prose like a call
    /// is, so they count into the fold — and a run of nothing but asks is
    /// work that happened, not the quiet mark thinking alone wears.
    #[test]
    fn a_folded_run_says_the_vault_was_asked() {
        let st = with_calls(vec![
            Entry::User("go".into()),
            ask("list notes", "[vv] list_notes → ok: 42 notes"),
            ask("read the note Groceries", "[vv] read_note title=\"Groceries\" → ok: milk"),
        ]);
        let out = rows(&st, 60, 6);
        assert!(
            out.iter()
                .any(|r| r.contains("▸ Asked the vault 2 questions")),
            "{out:?}"
        );
        assert!(
            !out.iter().any(|r| r.contains("42 notes")),
            "a folded run draws no answers: {out:?}"
        );
        // Asked beside the tools it ran with, in one line.
        let st = with_calls(vec![
            call("bash", true),
            ask("list notes", "[vv] list_notes → ok: 42 notes"),
        ]);
        let out = rows(&st, 60, 6);
        assert!(
            out.iter()
                .any(|r| r.contains("Ran 1 shell command \u{b7} Asked the vault 1 question")),
            "{out:?}"
        );
    }

    /// An alert is an info line the operator must not skim past, so it is
    /// drawn in the `error` role rather than faint — the whole point of
    /// having a second variant.
    #[test]
    fn an_alert_is_drawn_where_it_cannot_be_missed() {
        let st = with_calls(vec![
            Entry::Info("compacted 4 messages".into()),
            Entry::Alert("response cut off".into()),
        ]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 6)).unwrap();
        let mut r = Renderer::new(&st);
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "transcript" })))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let colour_of = |needle: &str| {
            (0..6)
                .find(|y| {
                    (0..40)
                        .map(|x| buf[(x, *y)].symbol())
                        .collect::<String>()
                        .contains(needle)
                })
                .map(|y| buf[(2, y)].style().fg)
                .unwrap()
        };
        let theme = Theme::builtin();
        assert_eq!(
            colour_of("cut off"),
            theme.fg("error").fg,
            "an alert is painted in the error role"
        );
        let dim_of = |needle: &str| {
            (0..buf.area.height)
                .find(|y| (0..buf.area.width).map(|x| buf[(x, *y)].symbol()).collect::<String>().contains(needle))
                .map(|y| buf[(2, y)].style().add_modifier.contains(Modifier::DIM))
                .unwrap()
        };
        assert!(
            dim_of("compacted") == theme.fg("faint").add_modifier.contains(Modifier::DIM),
            "an ordinary info line is still faint"
        );
    }

    #[test]
    fn a_settled_run_of_calls_folds_to_one_line_and_unfolds_on_demand() {
        let mut st = with_calls(vec![
            Entry::User("go".into()),
            call("bash", true),
            call("bash", true),
            call("bash", true),
        ]);
        let out = rows(&st, 44, 10);
        assert!(
            out.iter().any(|r| r.contains("Ran 3 shell commands")),
            "{out:?}"
        );
        assert!(
            !out.iter().any(|r| r.contains("SECRETUM")),
            "a folded run should not draw its output: {out:?}"
        );
        // The next step of the cycle is every call, capped as before.
        st.detail = Detail::Calls;
        let out = rows(&st, 44, 16);
        assert_eq!(
            out.iter()
                .filter(|r| r.starts_with("→ bash"))
                .count(),
            3,
            "{out:?}"
        );
        assert_eq!(
            out.iter().filter(|r| r.contains("SECRETUM")).count(),
            3,
            "{out:?}"
        );
    }

    /// The case the tail-only rule missed: two calls done and a third
    /// running. The two used to fold the moment the third began, and
    /// the summary line rewrote itself under the operator call by call.
    #[test]
    fn finished_calls_stay_open_while_a_later_call_is_still_running() {
        let mut st = with_calls(vec![
            Entry::User("go".into()),
            call("bash", true),
            call("bash", true),
            Entry::Tool { id: String::new(), name: "grep".into(), input: "{}".into(), output: None },
        ]);
        st.working = true;
        let out = rows(&st, 44, 16);
        assert!(!out.iter().any(|r| r.contains("Ran 2")), "folded mid-turn: {out:?}");
        assert_eq!(out.iter().filter(|r| r.contains("SECRETUM")).count(), 2, "{out:?}");
        assert!(out.iter().any(|r| r.contains("running…")), "{out:?}");
    }

    #[test]
    fn the_run_in_flight_stays_open_until_the_turn_lets_go_of_it() {
        let mut st = with_calls(vec![
            Entry::User("go".into()),
            call("bash", true),
            call("bash", true),
        ]);
        st.working = true;
        let out = rows(&st, 44, 16);
        assert!(
            !out.iter().any(|r| r.contains("Ran 2")),
            "the live run folded under the operator: {out:?}"
        );
        assert_eq!(
            out.iter().filter(|r| r.contains("SECRETUM")).count(),
            2,
            "{out:?}"
        );
        // Settling is what folds it; nothing else about the state moved.
        st.working = false;
        assert!(
            rows(&st, 44, 16)
                .iter()
                .any(|r| r.contains("Ran 2 shell commands"))
        );
    }

    /// `[transcript] fold = "live"` is the older rhythm, back: the
    /// finished calls fold the moment the next call begins, and only the
    /// call in flight — never part of a run — stays open.
    #[test]
    fn live_folding_folds_finished_calls_while_the_turn_runs() {
        let mut st = with_calls(vec![
            Entry::User("go".into()),
            call("bash", true),
            call("bash", true),
            Entry::Tool { id: String::new(), name: "grep".into(), input: "{}".into(), output: None },
        ]);
        st.fold = Fold::Live;
        st.working = true;
        let out = rows(&st, 44, 16);
        assert!(
            out.iter().any(|r| r.contains("Ran 2 shell commands")),
            "the finished calls did not fold mid-turn: {out:?}"
        );
        // Folded output is hidden, and the running call has none to show.
        assert!(
            !out.iter().any(|r| r.contains("SECRETUM")),
            "folded output leaked: {out:?}"
        );
        assert!(out.iter().any(|r| r.contains("running…")), "{out:?}");
        // And the settle still folds, as it always did: the same state at
        // rest draws the same summary.
        st.working = false;
        assert!(
            rows(&st, 44, 16)
                .iter()
                .any(|r| r.contains("Ran 2 shell commands"))
        );
    }

    #[test]
    fn folding_counts_the_failures_rather_than_hiding_them() {
        let st = with_calls(vec![call("bash", true), call("bash", false)]);
        let out = rows(&st, 44, 8);
        assert!(
            out.iter()
                .any(|r| r.contains("Ran 2 shell commands \u{b7} 1 failed")),
            "{out:?}"
        );
    }

    /// The trace cursor is a background, and it covers the block it is
    /// on and nothing else — a highlight that bled into the neighbours
    /// would say the selection is bigger than it is.
    #[test]
    fn the_trace_cursor_paints_the_block_it_is_on() {
        let mut st = with_calls(vec![
            Entry::User("what is here?".into()),
            Entry::Thinking {
                text: "look first".into(),
                streaming: false,
            },
            call("bash", true),
            Entry::Assistant {
                text: "a crate".into(),
                streaming: false,
            },
        ]);
        st.prompt.mode = crate::edit::Mode::named("trace", crate::edit::Kind::Normal);
        st.view_width = 40;
        st.view_height = 10;
        crate::trace::sync(&mut st);
        crate::trace::edge(&mut st, false);
        crate::trace::step(&mut st, 1);
        assert_eq!(st.trace.at, 1, "on the cluster");

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 10)).unwrap();
        let mut r = Renderer::new(&st);
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "transcript" })))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let painted = |needle: &str| {
            (0..10)
                .find(|y| {
                    (0..40)
                        .map(|x| buf[(x, *y)].symbol())
                        .collect::<String>()
                        .contains(needle)
                })
                .map(|y| buf[(0, y)].style().add_modifier.contains(Modifier::REVERSED))
        };
        assert_eq!(painted("cogitat"), Some(true), "the cursor's own block");
        assert_eq!(
            painted("what is here?"),
            Some(false),
            "the block above is untouched"
        );
        assert_eq!(painted("a crate"), Some(false), "and so is the one below");
    }

    #[test]
    fn a_cluster_that_changed_tools_names_each_of_them() {
        let st = with_calls(vec![call("read", true), call("bash", true)]);
        let out = rows(&st, 60, 8);
        assert!(
            out.iter()
                .any(|r| r.contains("Read 1 file \u{b7} Ran 1 shell command")),
            "{out:?}"
        );
        // One tool the phrasebook has never heard of still reads as prose.
        let st = with_calls(vec![call("mneme_rpc", true), call("mneme_rpc", true)]);
        assert!(
            rows(&st, 60, 8)
                .iter()
                .any(|r| r.contains("Ran 2 mneme_rpc calls"))
        );
        // Singular, for the run of one a settled turn often ends on.
        let st = with_calls(vec![call("read", true)]);
        assert!(rows(&st, 60, 8).iter().any(|r| r.contains("Read 1 file")));
    }

    /// Past three kinds the line would be cut off mid-phrase by the
    /// width, which says less than a short line does.
    #[test]
    fn a_cluster_that_touched_everything_counts_the_tail_instead() {
        let st = with_calls(vec![
            call("read", true),
            call("bash", true),
            call("edit", true),
            call("grep", true),
            call("write", true),
        ]);
        let out = rows(&st, 78, 8);
        assert!(
            out.iter().any(|r| r.contains(
                "Read 1 file \u{b7} Ran 1 shell command \u{b7} Edited 1 file \u{b7} 2 more"
            )),
            "{out:?}"
        );
    }

    /// The cluster is the point: thinking and calls, however many
    /// rounds of them, are one line and say so.
    #[test]
    fn thinking_and_the_calls_it_led_to_fold_together() {
        let st = with_calls(vec![
            Entry::Thinking {
                text: "a".repeat(38),
                streaming: false,
            },
            call("bash", true),
            Entry::Thinking {
                text: "b".repeat(82),
                streaming: false,
            },
            call("read", true),
            call("read", true),
        ]);
        let out = rows(&st, 60, 8);
        assert!(out.iter().any(|r| r.contains("cogitat (120) \u{b7} Ran 1 shell command \u{b7} Read 2 files")), "{out:?}");
        assert_eq!(
            out.iter().filter(|r| r.contains("cogitat")).count(),
            1,
            "one line for the lot: {out:?}"
        );
    }

    /// …and thinking on its own keeps the mark it wears when it is drawn
    /// as itself — the bullet and not the tool arrow — rather than being
    /// reported as work done.
    #[test]
    fn a_cluster_of_pure_thought_is_not_dressed_up_as_tool_traffic() {
        let st = with_calls(vec![Entry::Thinking {
            text: "mm".into(),
            streaming: false,
        }]);
        let out = rows(&st, 60, 8);
        assert!(
            out.iter().any(|r| r.trim_end() == "\u{2022} cogitat (2)"),
            "{out:?}"
        );
        assert!(
            !out.iter().any(|r| r.starts_with('\u{b7}')),
            "a thought line wore a separator's middle dot: {out:?}"
        );
    }

    /// Opening a cluster reads out what the thinking actually said —
    /// the text is kept now, not counted.
    #[test]
    fn an_opened_cluster_reads_out_the_thinking() {
        let mut st = with_calls(vec![
            Entry::Thinking {
                text: "the plan is to look first".into(),
                streaming: false,
            },
            call("bash", true),
        ]);
        assert!(
            !rows(&st, 60, 12).iter().any(|r| r.contains("the plan")),
            "folded, it is a count"
        );
        st.opened.push(0);
        assert!(
            rows(&st, 60, 12)
                .iter()
                .any(|r| r.contains("the plan is to look first")),
            "opened, it is the text"
        );
    }

    #[test]
    fn a_call_abandoned_mid_flight_is_not_counted_as_work_that_happened() {
        let st = with_calls(vec![
            call("bash", true),
            Entry::Tool {
                id: String::new(),
                name: "bash".into(),
                input: "{}".into(),
                output: None,
            },
            Entry::Info("cancelled".into()),
        ]);
        let out = rows(&st, 44, 10);
        assert!(out.iter().any(|r| r.contains("running\u{2026}")), "{out:?}");
        assert!(
            out.iter().any(|r| r.contains("Ran 1 shell command")),
            "{out:?}"
        );
        assert!(!out.iter().any(|r| r.contains("Ran 2")), "{out:?}");
    }

    /// An edit shows what it is about to do, from its own arguments,
    /// while it is still running — the tool's answer is a count.
    #[test]
    fn an_edit_shows_its_change_while_it_runs() {
        let input = r#"{"path": "a.rs", "old_str": "keep\nbefore\nkeep", "new_str": "keep\nafter\nkeep"}"#;
        let lines = tool_block("edit", "edit", input, None, 60, false, &Theme::builtin());
        let out = text(&lines);
        assert_eq!(out[0], "→ edit a.rs");
        let body: Vec<&str> = out[1..5].iter().map(|l| l.trim_end()).collect();
        // One line of context either side, then the change itself.
        // Numbered by the side that survives, so the removed line takes
        // a blank and the numbers stay a sequence.
        assert_eq!(body, ["▎ 1   keep", "▎   - before", "▎ 2 + after", "▎ 3   keep"]);
        // An unchanged line still sits on a ground, just not a coloured one.
        let ground = |needle: &str| {
            out.iter()
                .position(|l| l.contains(needle))
                .and_then(|i| lines[i].spans.iter().find_map(|s| s.style.bg))
        };
        let p = crate::markdown::Palette::from(&Theme::builtin());
        assert_eq!(ground("keep"), p.block.bg, "context has no ground");
        assert_eq!(ground("- before"), Theme::builtin().bg("removed").bg);
        assert!(out.iter().any(|l| l.contains("running…")), "{out:?}");
    }

    /// The fold is for traffic. An edit's diff is the only account of
    /// what changed, so it is still there when the turn has settled.
    /// The fold's own line stays above what it opened onto, pointing
    /// down: the thing that was clicked is still there to click back.
    #[test]
    fn an_opened_run_keeps_its_line_and_turns_the_marker_over() {
        let mut st = with_calls(vec![
            Entry::User("go".into()),
            call("bash", true),
            call("bash", true),
        ]);
        let folded = rows(&st, 44, 12);
        assert!(folded.iter().any(|r| r.starts_with("▸ Ran 2 shell commands")), "{folded:?}");
        st.opened.push(1);
        let open = rows(&st, 44, 12);
        assert!(open.iter().any(|r| r.starts_with("▾ Ran 2 shell commands")), "{open:?}");
        assert_eq!(open.iter().filter(|r| r.starts_with("→ bash")).count(), 2, "{open:?}");
    }

    #[test]
    fn a_settled_edit_folds_like_the_rest_and_opens_on_its_diff() {
        let mut st = with_calls(vec![
            Entry::User("go".into()),
            Entry::Tool {
                id: String::new(),
                name: "edit".into(),
                input: r#"{"path": "a.rs", "old_str": "before", "new_str": "after"}"#.into(),
                output: Some(("edited a.rs (1 replacement)".into(), false)),
            },
            call("bash", true),
            call("bash", true),
        ]);
        let out = rows(&st, 44, 16);
        assert!(
            !out.iter().any(|r| r.contains("- before")),
            "no call stands unfolded to hold a diff open — finding edits is trace's job: {out:?}"
        );
        st.opened.push(1);
        let open = rows(&st, 44, 16);
        assert!(open.iter().any(|r| r.contains("- before")), "the diff opens with its call: {open:?}");
        assert!(open.iter().any(|r| r.contains("+ after")), "{open:?}");
    }

    /// A file written is shown the way a diff shows a new file: every
    /// line of it an addition.
    #[test]
    fn a_write_shows_what_it_wrote() {
        let input = r#"{"path": "poem.txt", "content": "one\ntwo"}"#;
        let out = text(&tool_block("write", "write", input, None, 60, false, &Theme::builtin()));
        assert_eq!(out[0], "→ write poem.txt", "the content is not repeated on the head: {out:?}");
        let body: Vec<&str> = out[1..3].iter().map(|l| l.trim_end()).collect();
        assert_eq!(body, ["▎ 1 + one", "▎ 2 + two"]);
    }

    /// A diff is code, so it is coloured like code — the language read
    /// off the extension of the path the call names.
    #[test]
    fn a_diff_is_coloured_by_the_language_of_the_file_it_touches() {
        let t = Theme::builtin();
        let p = crate::markdown::Palette::from(&t);
        let input = r#"{"path": "src/main.rs", "old_str": "let a = 1;", "new_str": "let b = 2;"}"#;
        let lines = tool_block("edit", "edit", input, None, 60, false, &t);
        let spans: Vec<(String, Option<Color>)> = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| (s.content.to_string(), s.style.fg)))
            .collect();
        let keyword = p.syntax[crate::syntax::Role::Keyword.index()];
        assert!(spans.iter().any(|(c, f)| c == "let" && *f == Some(keyword)), "{spans:?}");
        // A path with no extension is plain, not guessed at.
        assert_eq!(diff_lang(r#"{"path": "/tmp/notes"}"#), "");
        assert_eq!(diff_lang(input), "rs");
    }

    /// A markdown file's diff reads as markdown: a heading is a
    /// heading and inline code is code, which is what the file is for.
    #[test]
    fn a_markdown_file_s_diff_is_rendered_as_markdown() {
        let t = Theme::builtin();
        let p = crate::markdown::Palette::from(&t);
        let input = r##"{"path": "notes.md", "old_str": "# Old", "new_str": "# New with `code`"}"##;
        let lines = tool_block("edit", "edit", input, None, 60, false, &t);
        let spans: Vec<(String, Option<Color>)> = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| (s.content.to_string(), s.style.fg)))
            .collect();
        assert!(spans.iter().any(|(c, f)| c.contains("New") && *f == Some(p.heading)), "{spans:?}");
        assert!(spans.iter().any(|(c, f)| c == "code" && *f == Some(p.code)), "{spans:?}");
    }

    #[test]
    fn a_diff_is_painted_by_its_signs_and_other_output_is_not() {
        let paint = |body: &str| {
            let lines = tool_block("bash", "bash", "{}", Some((body, false)), 60, true, &Theme::builtin());
            lines
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| (s.content.to_string(), s.style.bg)))
                .collect::<Vec<_>>()
        };
        let t = Theme::builtin();
        let diff = paint("@@ -1,2 +1,2 @@\n-gone\n+here");
        assert!(diff.iter().any(|(c, b)| c.contains("gone") && *b == t.bg("removed").bg), "{diff:?}");
        assert!(diff.iter().any(|(c, b)| c.contains("here") && *b == t.bg("added").bg), "{diff:?}");
        // A list with leading hyphens is prose, not a diff.
        let plain = paint("- one\n- two");
        assert!(!plain.iter().any(|(c, b)| c.contains("one") && b.is_some()), "{plain:?}");
    }

    #[test]
    fn a_call_reads_as_arguments_not_json() {
        let out = text(&tool_block(
            "read",
            "read",
            r#"{"path": "AGENTS.md"}"#,
            None,
            60,
            false,
            &Theme::builtin(),
        ));
        assert_eq!(out[0], "→ read AGENTS.md");
        assert_eq!(out[1], "  ↳ running…");
    }

    #[test]
    fn secondary_arguments_follow_the_primary_one() {
        let out = text(&tool_block(
            "grep",
            "grep",
            r#"{"pattern": "fn main", "glob": "*.rs"}"#,
            None,
            60,
            false,
            &Theme::builtin(),
        ));
        assert_eq!(out[0], "→ grep fn main  glob=*.rs");
    }

    #[test]
    fn half_streamed_input_still_draws() {
        let out = text(&tool_block(
            "read",
            "read",
            r#"{"path": "CLA"#,
            None,
            60,
            false,
            &Theme::builtin(),
        ));
        assert_eq!(out[0], r#"→ read {"path": "CLA"#);
    }

    #[test]
    fn output_collapses_to_a_few_lines_and_says_how_many_are_left() {
        let body = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = text(&tool_block(
            "read",
            "read",
            "{}",
            Some((body.as_str(), false)),
            60,
            false,
            &Theme::builtin(),
        ));
        assert_eq!(out[1], "  ↳ line 1");
        assert_eq!(out[TOOL_LINES], "    line 6");
        assert_eq!(out[TOOL_LINES + 1], "    … +14 more lines — space o");
        // Expanded shows every line, so there is nothing left to report.
        let all = text(&tool_block(
            "read",
            "read",
            "{}",
            Some((body.as_str(), false)),
            60,
            true,
            &Theme::builtin(),
        ));
        assert_eq!(all.len(), 22);
        assert_eq!(all[20], "    line 20");
    }

    #[test]
    fn a_long_line_is_clipped_to_the_width() {
        let long = "x".repeat(200);
        let out = text(&tool_block(
            "read",
            "read",
            "{}",
            Some((long.as_str(), false)),
            20,
            false,
            &Theme::builtin(),
        ));
        assert_eq!(out[1].chars().count(), 20);
        assert!(out[1].ends_with('…'));
    }

    // ------------------------------------------------------- the modes

    #[test]
    fn every_mode_names_itself_on_the_frame_not_only_the_search_one() {
        let seen = |st: &UiState| {
            let (glyphs, _) = draw(st, serde_json::json!({ "kind": "prompt" }), 40, 3);
            glyphs[0].clone()
        };
        let mut st = with_prompt("hello");
        assert!(seen(&st).contains("normal"), "{}", seen(&st));
        st.prompt.enter_insert(false);
        assert!(seen(&st).contains("insert"), "{}", seen(&st));
        st.prompt.enter_normal();
        st.prompt.toggle_select();
        assert!(seen(&st).contains("select"), "{}", seen(&st));

        // The one-line modes are modes to the hands without being
        // `Mode`s on the buffer, and they say so on the same frame —
        // which is round the message they are *not*.
        let mut cmd = with_prompt("hello");
        mini(&mut cmd, crate::state::MiniKind::Command, "model");
        assert!(seen(&cmd).contains("command"), "{}", seen(&cmd));
        mini(&mut cmd, crate::state::MiniKind::Dispatch, "read it");
        assert!(seen(&cmd).contains("dispatch"), "{}", seen(&cmd));
    }

    /// The foreground colour of one cell, and where the cursor landed.
    fn cell(state: &UiState, at: (u16, u16)) -> (String, Color, Option<(u16, u16)>) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 3)).unwrap();
        let mut r = Renderer::new(state);
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "prompt" })))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        (buf[at].symbol().to_string(), buf[at].fg, r.cursor)
    }

    /// `:` `/` `?` `!` are the four ways into a mode by typing one
    /// character, and all four wear that character at the head of their
    /// line — which is on the prompt's **bottom border**, with the
    /// message still in the box above it.
    #[test]
    fn a_one_line_mode_wears_the_key_that_opened_it_in_its_own_colour() {
        let bottom = 2; // a three-row prompt: top border, text, bottom border
        for (kind, sigil, hue) in [
            (crate::state::MiniKind::Command, ":", Color::Cyan),
            (crate::state::MiniKind::Dispatch, "!", Color::Red),
            (
                crate::state::MiniKind::Search(crate::search::Dir::Forward),
                "/",
                Color::Yellow,
            ),
            (
                crate::state::MiniKind::Search(crate::search::Dir::Backward),
                "?",
                Color::Yellow,
            ),
        ] {
            let mut st = with_prompt("hi");
            mini(&mut st, kind, "");
            let (glyph, painted, cursor) = cell(&st, (1, bottom));
            assert_eq!(glyph, sigil);
            assert_eq!(painted, hue, "{sigil}");
            assert_eq!(
                st.prompt.text(),
                "hi",
                "the sigil is drawn, never stored — and neither is the message"
            );
            // The caret is on the line's own first character, which the
            // sigil has pushed one cell right: it must not land on the
            // sigil, and it must not be up in the message.
            assert_eq!(cursor, Some((2, bottom)), "{sigil}");
        }
        // With no line up the caret is in the message, where it was.
        let mut bare = with_prompt("hi");
        bare.prompt.enter_insert(false);
        assert_eq!(cell(&bare, (1, 1)).2, Some((1, 1)));
    }

    #[test]
    fn no_two_modes_on_screen_at_once_share_a_colour() {
        // Command and search were both yellow, which made the one thing
        // the colour is for — telling them apart — impossible.
        let mut st = with_prompt("");
        let mut seen: Vec<(String, String)> = Vec::new();
        for m in ["normal", "insert", "select"] {
            st.prompt.mode = st.modes.mode(m).unwrap();
            let d = mode_style(&st);
            seen.push((d.name, d.colour));
        }
        for kind in [
            crate::state::MiniKind::Command,
            crate::state::MiniKind::Dispatch,
            crate::state::MiniKind::Search(crate::search::Dir::Forward),
        ] {
            let mut one = with_prompt("");
            mini(&mut one, kind, "x");
            let d = mode_style(&one);
            seen.push((d.name, d.colour));
        }

        let mut hues: Vec<&str> = seen.iter().map(|(_, h)| h.as_str()).collect();
        hues.sort_unstable();
        let n = hues.len();
        hues.dedup();
        assert_eq!(hues.len(), n, "two modes share a colour: {seen:?}");
    }

    #[test]
    fn a_script_that_wants_its_own_title_still_gets_it() {
        let st = with_prompt("hello");
        let (glyphs, _) = draw(
            &st,
            serde_json::json!({ "kind": "prompt", "title": "mine" }),
            40,
            3,
        );
        assert!(glyphs[0].contains("mine"), "{}", glyphs[0]);
        assert!(!glyphs[0].contains("normal"));
    }

    #[test]
    fn the_frame_and_the_badge_are_told_the_same_colour() {
        let mut st = with_prompt("hello");
        // One answer, three readers: border, title and the script's
        // badge. The snapshot hands the script the very same row.
        let d = mode_style(&st);
        assert_eq!((d.name.as_str(), d.colour.as_str()), ("normal", "blue"));
        assert_eq!(st.snapshot()["mode_colour"], "blue");
        assert_eq!(st.snapshot()["mode_label"], "NOR");
        st.prompt.enter_insert(false);
        let d = mode_style(&st);
        assert_eq!((d.name.as_str(), d.colour.as_str()), ("insert", "green"));
        assert_eq!(st.snapshot()["mode_colour"], "green");
        assert_eq!(st.snapshot()["mode"], "insert");
        assert_eq!(st.snapshot()["mode_kind"], "insert");
    }

    #[test]
    fn a_restyled_mode_changes_the_frame_the_sigil_and_the_badge_together() {
        // A user's `modes()` row, laid over the default table: dispatch in
        // a colour of their own with a different character in front.
        let mut st = with_prompt("hi");
        st.modes = crate::modes::ModeTable::from_value(
            &serde_json::json!({ "dispatch": { "label": "GO", "colour": "#00ff00", "sigil": ">" } }),
            &st.modes,
        )
        .unwrap();
        mini(&mut st, crate::state::MiniKind::Dispatch, "");
        let (glyph, hue, _) = cell(&st, (1, 2));
        assert_eq!(glyph, ">");
        assert_eq!(hue, Color::Rgb(0, 255, 0));
        let snap = st.snapshot();
        assert_eq!(snap["mode_colour"], "#00ff00");
        assert_eq!(snap["mode_label"], "GO");
        assert_eq!(snap["mode_sigil"], ">");
        // And a mode of the user's own, with none of the harness's names.
        st.modes = crate::modes::ModeTable::from_value(
            &serde_json::json!({ "review": { "kind": "normal", "label": "REV", "colour": "cyan" } }),
            &st.modes,
        )
        .unwrap();
        st.mini = None;
        st.prompt.enter(st.modes.mode("review").unwrap());
        let (glyphs, _) = draw(&st, serde_json::json!({ "kind": "prompt" }), 40, 3);
        assert!(glyphs[0].contains("review"), "{}", glyphs[0]);
        assert_eq!(st.snapshot()["mode_kind"], "normal");
    }

    #[test]
    fn the_theme_recolours_what_rust_draws() {
        let mut st = with_prompt("");
        st.transcript.push(Entry::User("hello".into()));
        st.transcript.push(Entry::Tool {
            id: "1".into(),
            name: "read".into(),
            input: "{}".into(),
            output: Some(("x".into(), true)),
        });
        st.detail = Detail::Calls;
        let (t, _) = Theme::from_value(
            &serde_json::json!({ "user": "#123456", "error": "#654321" }),
            &st.theme,
        )
        .unwrap();
        st.theme = t;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 8)).unwrap();
        let mut r = Renderer::new(&st);
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "transcript" })))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let fgs: Vec<Color> = (0..8)
            .flat_map(|y| (0..30).map(move |x| (x, y)))
            .map(|at| buf[at].fg)
            .collect();
        assert!(
            fgs.contains(&Color::Rgb(0x12, 0x34, 0x56)),
            "the user line took the theme's colour"
        );
        assert!(
            fgs.contains(&Color::Rgb(0x65, 0x43, 0x21)),
            "the failed output took the theme's colour"
        );
    }

    #[test]
    fn a_backward_search_says_so_in_words_and_not_only_in_its_sigil() {
        let mut st = with_prompt("");
        st.transcript.push(Entry::User("the needle".into()));
        search_line(&mut st, "needle");
        let title =
            |st: &UiState| draw(st, serde_json::json!({ "kind": "prompt" }), 60, 3).0[0].clone();
        assert!(title(&st).contains("search"), "{}", title(&st));
        assert!(!title(&st).contains("back"));
        st.mini = Some({
            let mut m = crate::state::Mini::new(crate::state::MiniKind::Search(
                crate::search::Dir::Backward,
            ));
            m.input.insert("needle");
            m
        });
        // `/` and `?` differ by a shift and one glyph but land on
        // different matches; the frame should not make the reader know
        // that already.
        assert!(title(&st).contains("search back"), "{}", title(&st));
    }

    // ----------------------------------------------------------- notices

    /// A notice takes the status line's left half whole and leaves the
    /// right, and when it is taken down the script's `left` is back.
    #[test]
    fn a_notice_borrows_the_left_of_the_status_line_and_gives_it_back() {
        let mut st = UiState::new("m".into(), "/tmp/s.eid".into(), "/".into());
        let tree =
            serde_json::json!({ "kind": "status", "left": "NOR · model", "right": "ctx 1k" });
        let (rows, _) = draw(&st, tree.clone(), 40, 1);
        assert!(rows[0].starts_with("NOR · model"), "{rows:?}");
        assert!(rows[0].ends_with("ctx 1k"), "{rows:?}");
        st.info("no matches\nsecond line");
        let (rows, _) = draw(&st, tree.clone(), 40, 1);
        assert!(
            rows[0].starts_with(" no matches · second line"),
            "one row, so a break is a dot: {rows:?}"
        );
        assert!(
            !rows[0].contains("NOR"),
            "the notice takes the whole left: {rows:?}"
        );
        assert!(rows[0].ends_with("ctx 1k"), "the gauge stays: {rows:?}");
        st.notice = None;
        let (rows, _) = draw(&st, tree, 40, 1);
        assert!(rows[0].starts_with("NOR · model"), "{rows:?}");
    }

    #[test]
    fn structured_page_styles_preserve_rows_at_every_width() {
        let theme = crate::theme::Theme::default();
        let body = "── Overview ──\n\n  system     │ ━━━··· 120c\n  ● #5 current record ← HEAD\n  a long description with words to wrap";
        for width in [1, 12, 40, 100] {
            let lines = styled_page_lines(body, width, &theme, true);
            assert_eq!(lines.iter().map(plain).collect::<Vec<_>>(), page_lines(body, width));
        }
        let styled = styled_page_lines(body, 100, &theme, true);
        assert!(styled[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(styled[3].style.add_modifier.contains(Modifier::BOLD));
        let plain = styled_page_lines(body, 100, &theme, false);
        assert_eq!(plain[0].style, Style::default());
    }

    /// A page is most of the screen, drawn from its scroll, keeps a line
    /// that fits as it is — indentation and all — and says where in it
    /// you are.
    #[test]
    fn a_page_is_drawn_from_its_scroll_and_says_where_it_is() {
        let mut st = UiState::new("m".into(), "/tmp/s.eid".into(), "/".into());
        let body: String = (1..=30)
            .map(|i| format!("  line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        st.dialog = Some(crate::state::Dialog {
            kind: DialogKind::Page,
            prompt: "help".into(),
            options: Vec::new(),
            keys: Vec::new(),
            weights: Vec::new(),
            selected: 5,
            input: crate::state::Dialog::field(),
            reply: None,
            pick: None,
            body,
            masked: false,
        });
        let (rows, _) = draw(&st, serde_json::json!({ "kind": "transcript" }), 40, 12);
        let all = rows.join("\n");
        assert!(all.contains(" help "), "titled: {all}");
        assert!(
            all.contains("   line 6 "),
            "drawn from the scroll, indentation kept: {all}"
        );
        assert!(
            !all.contains(" line 5 "),
            "what is scrolled past is not drawn: {all}"
        );
        let (_, per_frame) = page_layout((40, 12));
        assert!(
            all.contains(&format!("6\u{2013}{} of 30", 5 + per_frame)),
            "{all}"
        );
    }

    // ------------------------------------------------- the which-key popup

    fn chord(items: usize, h: u16) -> Vec<String> {
        let mut st = with_prompt("");
        st.menu = Some(crate::state::Menu {
            title: "space".into(),
            items: (0..items)
                .map(|i| (format!("{i}"), format!("does thing {i}")))
                .collect(),
            selected: None,
        });
        draw(&st, serde_json::json!({ "kind": "menu" }), 80, h).0
    }

    #[test]
    fn the_popup_is_every_binding_under_the_chord_and_chooses_among_none() {
        let out = chord(8, 30).join("\n");
        for i in 0..8 {
            assert!(
                out.contains(&format!("does thing {i}")),
                "{i} is missing from {out}"
            );
        }
        assert!(!out.contains("more"), "nothing was held back");
    }

    #[test]
    fn a_popup_that_cannot_fit_says_how_many_it_kept_back() {
        // The one thing it must not do is drop them in silence.
        let out = chord(40, 12).join("\n");
        assert!(out.contains("more — space p for all"), "{out}");
        let hidden: usize = out
            .split("… +")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .and_then(|n| n.parse().ok())
            .expect("a count");
        let shown = (0..40)
            .filter(|i| out.contains(&format!("does thing {i}")))
            .count();
        assert_eq!(
            shown + hidden,
            40,
            "every binding is either drawn or counted"
        );
    }

    // ------------------------------------------------- search and tags

    #[test]
    fn every_match_on_screen_is_highlighted_where_it_is_drawn() {
        let mut st = with_prompt("");
        st.transcript
            .push(Entry::User("find the needle here".into()));
        searching(&mut st, "needle");
        // `› ` then `find the ` puts the match at column 11.
        let lit = painted(
            &st,
            serde_json::json!({ "kind": "transcript" }),
            30,
            2,
            Color::Yellow,
        );
        assert_eq!(lit[0], "           ######");
    }

    #[test]
    fn nothing_is_highlighted_once_the_search_is_put_away() {
        let mut st = with_prompt("");
        st.transcript
            .push(Entry::User("find the needle here".into()));
        searching(&mut st, "needle");
        st.search = None;
        let lit = painted(
            &st,
            serde_json::json!({ "kind": "transcript" }),
            30,
            2,
            Color::Yellow,
        );
        assert_eq!(lit[0], "");
    }

    #[test]
    fn a_tag_is_written_over_the_head_of_the_match_it_points_at() {
        let mut st = with_prompt("");
        st.transcript.push(Entry::User("the needle".into()));
        st.transcript.push(Entry::User("more needle".into()));
        searching(&mut st, "needle");
        st.jump =
            crate::jump::Jump::new(vec![0, 1].into_iter().map(crate::jump::Spot::Hit).collect());
        let out = rows(&st, 30, 5);
        // The tag replaces the match's first character rather than being
        // inserted beside it: the line must not reflow under the tags.
        assert_eq!(out[0], "› the aeedle");
        assert_eq!(out[2], "› more beedle");
    }

    #[test]
    fn a_half_typed_pair_of_tags_narrows_to_what_can_still_be_reached() {
        let mut st = with_prompt("");
        for i in 0..30 {
            st.transcript.push(Entry::User(format!("needle {i}")));
        }
        searching(&mut st, "needle");
        let mut j = crate::jump::Jump::new((0..30).map(crate::jump::Spot::Hit).collect()).unwrap();
        // 30 targets means two-char labels; `b` leaves only `ba`…`bd`.
        assert_eq!(j.press('b'), crate::jump::Step::Pending);
        st.jump = Some(j);
        let out = rows(&st, 30, 62);
        assert_eq!(
            out[52], "› aeedle 26",
            "the 27th match wears what is left of `ba`"
        );
        assert!(
            out[0].starts_with("› needle 0"),
            "the ones `b` cannot reach lost their tags"
        );
    }

    /// The search line goes on the prompt's **bottom border**, and the
    /// draft stays in the box above it — which is the whole point of the
    /// minibuffer. It used to stand in the prompt's own slot, so
    /// reaching for a search put a half-written message off the screen.
    #[test]
    fn the_search_line_sits_on_the_border_and_leaves_the_draft_on_screen() {
        let mut st = with_prompt("a draft I have not sent");
        st.transcript.push(Entry::User("the needle".into()));
        search_line(&mut st, "needle");
        let (glyphs, _) = draw(&st, serde_json::json!({ "kind": "prompt" }), 44, 3);
        assert!(glyphs[0].contains("search"), "{:?}", glyphs[0]);
        assert!(
            glyphs[1].contains("a draft I have not sent"),
            "the draft is where it was: {:?}",
            glyphs[1]
        );
        assert!(
            glyphs[2].contains("/needle"),
            "and the pattern is on the border: {:?}",
            glyphs[2]
        );
        assert!(
            glyphs[2].contains("1 of 1"),
            "with the count beside it: {:?}",
            glyphs[2]
        );
    }

    /// And the draft is searched along with the session, so a match in
    /// the message being written is one of the hits and is lit where it
    /// is drawn.
    #[test]
    fn a_match_in_the_draft_is_a_hit_and_is_lit_in_the_prompt() {
        let mut st = with_prompt("the needle I am writing about");
        st.transcript.push(Entry::User("the needle".into()));
        search_line(&mut st, "needle");
        let (glyphs, _) = draw(&st, serde_json::json!({ "kind": "prompt" }), 44, 3);
        assert!(
            glyphs[2].contains("of 2"),
            "the entry and the draft are two hits: {:?}",
            glyphs[2]
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(44, 3)).unwrap();
        let mut r = Renderer::new(&st);
        terminal
            .draw(|f| r.draw(f, &serde_json::json!({ "kind": "prompt" })))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        // `needle` starts at column 5 of the prompt's own row.
        assert_eq!(buf[(5, 1)].symbol(), "n");
        assert_eq!(
            buf[(5, 1)].bg,
            st.theme.colour("hit"),
            "the match in the draft wears the highlight"
        );
        assert_eq!(buf[(1, 1)].bg, Color::Reset, "and nothing else does");
    }

    #[test]
    fn a_pattern_that_will_not_compile_says_it_is_being_read_as_text() {
        let mut st = with_prompt("");
        st.transcript.push(Entry::User("a (paren".into()));
        search_line(&mut st, "(paren");
        assert!(st.search.as_ref().unwrap().literal);
        let (glyphs, _) = draw(&st, serde_json::json!({ "kind": "prompt" }), 44, 3);
        assert!(glyphs[2].contains("as text"), "{:?}", glyphs[2]);
    }

    #[test]
    fn gw_tags_are_drawn_over_the_prompts_own_words() {
        let mut st = with_prompt("delete the cargo");
        st.jump = crate::jump::Jump::new(
            st.prompt
                .word_starts()
                .into_iter()
                .map(crate::jump::Spot::Word)
                .collect(),
        );
        let (glyphs, _) = draw(&st, serde_json::json!({ "kind": "prompt" }), 24, 3);
        assert!(
            glyphs[1].starts_with("│aelete bhe cargo"),
            "{:?}",
            glyphs[1]
        );
    }

    #[test]
    fn the_lines_below_an_entry_are_what_scrolls_it_to_the_top() {
        let mut st = with_prompt("");
        for i in 0..5 {
            st.transcript.push(Entry::Info(format!("line {i}")));
        }
        // Info entries are one drawn line each, so the arithmetic is
        // visible: entries 3 and 4 sit below the top of entry 3.
        assert_eq!(lines_below(&st, 40, 3), 2);
        assert_eq!(lines_below(&st, 40, 0), 5);
        // And that is the scroll: with a two-row window, putting entry 3
        // on the first row means scrolling up by nothing at all.
        st.view_height = 2;
        assert_eq!(lines_below(&st, 40, 3).saturating_sub(st.view_height), 0);
        assert_eq!(lines_below(&st, 40, 0).saturating_sub(st.view_height), 3);
    }

    /// A settled turn's line wears the star of the turn it was — where
    /// every other note in the transcript wears the quiet dot — and the
    /// next turn's is a different one.
    #[test]
    fn a_settled_turn_leads_with_its_own_star() {
        let drawn = |turn: usize| {
            let mut st = with_calls(vec![
                Entry::Assistant { text: "done".into(), streaming: false },
                Entry::Settle { turn, text: crate::state::settle_line(0, None, 87) },
                Entry::Info("a note".into()),
            ]);
            st.modes = crate::script::default_modes();
            st.theme = crate::script::default_theme();
            rows(&st, 60, 5)
        };
        let first = drawn(0);
        let second = drawn(1);
        let line = |out: &[String]| out.iter().find(|r| r.contains("insiliit")).unwrap().clone();
        assert_eq!(line(&first), format!("{} insiliit · ⇣ 87", crate::life::star(0)));
        assert_eq!(line(&second), format!("{} insiliit · ⇣ 87", crate::life::star(1)));
        assert_ne!(crate::life::star(0), crate::life::star(1), "one star for two turns");
        // The note above it keeps the dot, and the star takes no more
        // room than the dot did: the line is still flush at the pane and
        // still wraps at the same column.
        assert!(
            first.iter().any(|r| r == "\u{2022} a note"),
            "a note stopped wearing its dot: {first:?}"
        );
    }

    /// A transcript whose only match is inside a settled tool call —
    /// which folds, so the match is nowhere on screen.
    fn folded_match() -> UiState {
        let mut st = with_prompt("");
        st.transcript.push(Entry::User("write three files".into()));
        st.transcript.push(Entry::Tool {
            id: String::new(),
            name: "write".into(),
            input: r#"{"path":"random_char_3.txt"}"#.into(),
            output: Some(("wrote random_char_3.txt: @".into(), false)),
        });
        searching(&mut st, "@");
        st
    }

    #[test]
    fn a_match_hidden_inside_a_fold_is_counted_onto_the_line_that_hid_it() {
        let st = folded_match();
        assert_eq!(
            st.search.as_ref().unwrap().hits.len(),
            1,
            "the search found it"
        );
        let out = rows(&st, 40, 6);
        // Without this the fold would say `Wrote 1 file` and nothing on
        // screen would be lit, while the search line said `1 of 1`.
        assert_eq!(out[2], "▸ Wrote 1 file \u{b7} 1 match");
    }

    #[test]
    fn the_count_is_painted_as_the_match_because_that_is_where_it_went() {
        let st = folded_match();
        let lit = painted(
            &st,
            serde_json::json!({ "kind": "transcript" }),
            40,
            6,
            Color::Yellow,
        );
        assert_eq!(
            lit[2], "              ##########",
            "` · 1 match` wears the highlight"
        );
    }

    #[test]
    fn several_hidden_matches_are_counted_together_and_pluralised() {
        let mut st = with_prompt("");
        for i in 0..3 {
            st.transcript.push(Entry::Tool {
                id: String::new(),
                name: "write".into(),
                input: format!(r#"{{"path":"random_char_{i}.txt"}}"#),
                output: Some((format!("wrote random_char_{i}.txt: @"), false)),
            });
        }
        searching(&mut st, "@");
        let out = rows(&st, 40, 4);
        assert_eq!(out[0], "▸ Wrote 3 files \u{b7} 3 matches");
    }

    #[test]
    fn opening_the_fold_draws_the_call_and_lights_the_match_in_it() {
        let mut st = folded_match();
        // The run starts at entry 1, which is what `opened` names.
        assert_eq!(fold_at(&st, 1), Some(1));
        st.opened.push(1);
        assert_eq!(fold_at(&st, 1), None, "nothing left to open");
        let out = rows(&st, 40, 8);
        assert!(
            out.iter().any(|r| r.contains("wrote random_char_3.txt: @")),
            "{out:?}"
        );
        // And now it is a real highlight rather than a count.
        let lit = painted(
            &st,
            serde_json::json!({ "kind": "transcript" }),
            40,
            8,
            Color::Yellow,
        );
        assert!(lit.iter().any(|r| r.contains('#')));
        assert!(
            !out.iter().any(|r| r.contains("1 match")),
            "the count gave way to the thing itself"
        );
    }

    #[test]
    fn an_opened_fold_shows_all_of_its_output_not_a_capped_slice() {
        let mut st = with_prompt("");
        let body = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        st.transcript.push(Entry::Tool {
            id: String::new(),
            name: "read".into(),
            input: "{}".into(),
            output: Some((body, false)),
        });
        searching(&mut st, "line 19");
        st.opened.push(0);
        let out = rows(&st, 40, 30);
        // Opened means opened: a capped slice would hide the match one
        // level further down and the key would look like it did nothing.
        assert!(out.iter().any(|r| r.contains("line 19")), "{out:?}");
    }

    #[test]
    fn the_search_line_offers_to_open_a_fold_only_when_there_is_one() {
        let mut st = folded_match();
        mini(
            &mut st,
            crate::state::MiniKind::Search(crate::search::Dir::Forward),
            "needle",
        );
        let (glyphs, _) = draw(&st, serde_json::json!({ "kind": "prompt" }), 60, 3);
        assert!(glyphs[2].contains("S-tab opens"), "{:?}", glyphs[2]);
        // Opened, the offer retires rather than lying about what is left.
        st.opened.push(1);
        let (after, _) = draw(&st, serde_json::json!({ "kind": "prompt" }), 60, 3);
        assert!(!after[2].contains("S-tab opens"), "{:?}", after[2]);
    }

    #[test]
    fn a_visible_match_is_never_reported_as_hidden() {
        let mut st = with_prompt("");
        st.transcript.push(Entry::User("the needle".into()));
        searching(&mut st, "needle");
        let out = rows(&st, 40, 4);
        assert_eq!(
            out[0], "\u{203a} the needle",
            "no count where the text is right there"
        );
    }

    #[test]
    fn a_tag_for_a_hidden_match_rides_the_line_that_stands_for_it() {
        let mut st = folded_match();
        st.jump = crate::jump::Jump::new(vec![crate::jump::Spot::Hit(0)]);
        let out = rows(&st, 40, 6);
        // The tag goes at the head of the fold line — over the arrow,
        // where a tag always goes — because no part of the match is on
        // screen to carry it.
        assert_eq!(out[2], "a Wrote 1 file \u{b7} 1 match");
    }

    #[test]
    fn an_entry_inside_a_folded_run_answers_for_the_whole_fold() {
        let mut st = with_prompt("");
        st.transcript.push(Entry::User("go".into()));
        for _ in 0..3 {
            st.transcript.push(Entry::Tool {
                id: String::new(),
                name: "read".into(),
                input: "{}".into(),
                output: Some(("ok".into(), false)),
            });
        }
        // Folded, the three calls are one block, so the middle one is not
        // separately reachable — it scrolls to the summary, which is
        // where it is drawn.
        assert_eq!(st.detail, Detail::Folded);
        assert_eq!(lines_below(&st, 40, 2), lines_below(&st, 40, 1));
    }

    /// A masked `Ask` dialog paints one `•` per character typed and never
    /// the character itself — the one guarantee that matters for a
    /// credential prompt, checked against the actual painted cells rather
    /// than trusting the code that produces them.
    #[test]
    fn a_masked_ask_dialog_paints_bullets_not_the_secret() {
        let mut s = UiState::new("m".into(), "log".into(), "/".into());
        s.dialog = Some(crate::state::Dialog {
            kind: crate::state::DialogKind::Ask,
            prompt: " ollama · API key ".into(),
            options: Vec::new(),
            keys: vec!["ollama".into()],
            weights: Vec::new(),
            selected: 0,
            input: {
                let mut b = crate::state::Dialog::field();
                b.insert("sk-secret");
                b
            },
            reply: None,
            pick: Some(crate::state::PickAction::Run("login".into())),
            body: String::new(),
            masked: true,
        });
        let (buf, _) = painted_links(&s, 40, 12);
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(!text.contains("sk-secret"), "the secret must never be painted");
        assert!(text.contains("•••••••••"), "one bullet per character typed");
        // And the title names the provider, so the operator can tell
        // which key they are about to overwrite without seeing it.
        assert!(text.contains("ollama"), "the title names the provider");
    }

    /// An unmasked dialog — nothing in the harness opens one today, but
    /// the `masked` flag is what a future free-text `Ask` would flip off,
    /// and the render must still show the text plainly in that case.
    #[test]
    fn an_unmasked_ask_dialog_paints_the_text_itself() {
        let mut s = UiState::new("m".into(), "log".into(), "/".into());
        s.dialog = Some(crate::state::Dialog {
            kind: crate::state::DialogKind::Ask,
            prompt: " note ".into(),
            options: Vec::new(),
            keys: Vec::new(),
            weights: Vec::new(),
            selected: 0,
            input: {
                let mut b = crate::state::Dialog::field();
                b.insert("plain text");
                b
            },
            reply: None,
            pick: None,
            body: String::new(),
            masked: false,
        });
        let (buf, _) = painted_links(&s, 40, 12);
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("plain text"));
    }
}
