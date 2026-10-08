//! The join snapshot (slice T1c): the bytes that rebuild a terminal's screen for a page that joins once the ring no longer starts at
//! byte 0, from the alacritty copy ([`super::screen`]). Two steps, so the screen's lock is held only for the copy: [`picture`] takes the
//! visible cells, the cursor, the modes and the scroll region under it, and [`encode`] writes them out after it is let go.
//!
//! What is written, in order: `RIS`; `?1049` first when the alternate screen is on; the modes the `Term` holds, each as set or reset
//! (`?1` DECCKM, `?7` wrap, `?25` cursor, the mouse modes `?1000` `?1002` `?1003` `?1006`, `?1004` focus, `?1007` alternate scroll,
//! `?2004` bracketed paste, `ESC =` or `ESC >` for the keypad, and the ANSI modes `20` and `4`); the scroll region (`CSI Pt;Pb r`), when
//! the guard has one (see `screen::Guard`); a clear; the visible rows, each from its column 1, with an SGR wherever the attributes change, the underline's
//! style (`4:n`) and colour (`58`) among them; origin mode (`?6`) when it is on; the cursor.
//!
//! **Why origin mode comes last.** With it on, a row is addressed from the region's top, and setting it homes the cursor. So the rows
//! are drawn with it off, and the cursor is placed after it, from the region's top.
//!
//! **What it cannot rebuild.** The primary screen behind the alternate one and the history above the visible rows are not sent.
//! alacritty keeps one scroll region for both screens, and the page keeps one for each, so a snapshot of the alternate screen takes
//! the last region set on either. A cell holds only what [`shown`] allows, so the bytes here are the program's text and attributes and
//! never one of its escapes: a character that is not shown is written as a space.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::Color;

use super::screen::{Emu, shown};

/// The modes written as DECSET or DECRST, in the order they are written. Origin mode is not among them: it is written last ([`encode`]).
const DECSET: &[(TermMode, u16)] = &[
    (TermMode::APP_CURSOR, 1),
    (TermMode::LINE_WRAP, 7),
    (TermMode::MOUSE_REPORT_CLICK, 1000),
    (TermMode::MOUSE_DRAG, 1002),
    (TermMode::MOUSE_MOTION, 1003),
    (TermMode::SGR_MOUSE, 1006),
    (TermMode::FOCUS_IN_OUT, 1004),
    (TermMode::ALTERNATE_SCROLL, 1007),
    (TermMode::BRACKETED_PASTE, 2004),
    (TermMode::SHOW_CURSOR, 25),
];

/// The modes written as SM or RM (no `?`).
const ANSI_MODES: &[(TermMode, u16)] = &[(TermMode::LINE_FEED_NEW_LINE, 20), (TermMode::INSERT, 4)];

/// The screen as it was when [`picture`] looked at it: the visible cells (row-major), the cursor (row, column, from 0), the modes and the
/// scroll region (from line 0, the line after its bottom; `None` for the whole screen).
pub struct Picture {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    cursor: (usize, usize),
    mode: TermMode,
    region: Option<(usize, usize)>,
}

/// Copy the visible screen: the cells, the cursor, the modes and the region. Cheap enough to run under the screen's lock (a clone of
/// each cell, no formatting), and it is the only part of the snapshot that does.
pub fn picture(emu: &Emu) -> Picture {
    let term = &emu.term;
    let (rows, cols) = (term.screen_lines(), term.columns());
    let grid = term.grid();
    let mut cells = Vec::with_capacity(rows * cols);
    for line in 0..rows {
        for column in 0..cols {
            cells.push(grid[Line(line as i32)][Column(column)].clone());
        }
    }
    let point = grid.cursor.point;
    let cursor = ((point.line.0.max(0) as usize).min(rows - 1), point.column.0.min(cols - 1));
    Picture { cols, rows, cells, cursor, mode: *term.mode(), region: emu.region() }
}

/// The bytes that rebuild `picture` on a blank terminal: what a page writes after a `term-reset` with `snap: true`.
pub fn encode(picture: &Picture) -> Vec<u8> {
    let mode = picture.mode;
    let mut out = Vec::with_capacity(picture.cells.len() + 128);
    out.extend_from_slice(b"\x1bc");
    if mode.contains(TermMode::ALT_SCREEN) {
        out.extend_from_slice(b"\x1b[?1049h");
    }
    for (flag, number) in DECSET {
        let set = if mode.contains(*flag) { 'h' } else { 'l' };
        out.extend_from_slice(format!("\x1b[?{number}{set}").as_bytes());
    }
    for (flag, number) in ANSI_MODES {
        let set = if mode.contains(*flag) { 'h' } else { 'l' };
        out.extend_from_slice(format!("\x1b[{number}{set}").as_bytes());
    }
    out.extend_from_slice(if mode.contains(TermMode::APP_KEYPAD) { b"\x1b=" } else { b"\x1b>" });
    // Origin mode is still off here, so every address below is the screen's own. The region homes the cursor, and the clear does too.
    let top = match picture.region {
        Some((top, bottom)) => {
            out.extend_from_slice(format!("\x1b[{};{}r", top + 1, bottom).as_bytes());
            top
        }
        None => 0,
    };
    out.extend_from_slice(b"\x1b[H\x1b[2J\x1b[0m");
    let plain = Style::plain();
    let mut current = plain;
    for row in 0..picture.rows {
        let cells = &picture.cells[row * picture.cols..(row + 1) * picture.cols];
        draw_row(&mut out, row, cells, plain, &mut current);
    }
    out.extend_from_slice(b"\x1b[0m");
    let origin = mode.contains(TermMode::ORIGIN);
    if origin {
        out.extend_from_slice(b"\x1b[?6h");
    }
    let (row, column) = picture.cursor;
    let row = if origin { row.saturating_sub(top) } else { row };
    out.extend_from_slice(format!("\x1b[{};{}H", row + 1, column + 1).as_bytes());
    out
}

/// One row from its first column. The attributes carried from the last row are `current`, which is what the page has set. Trailing
/// blanks in the plain attributes are not written: a cleared screen is blank there already.
fn draw_row(out: &mut Vec<u8>, row: usize, cells: &[Cell], plain: Style, current: &mut Style) {
    out.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
    let end = cells.iter().rposition(|cell| !blank(cell, plain)).map_or(0, |last| last + 1);
    for cell in &cells[..end] {
        // The second column of a wide character is the character's own: writing the first moves the cursor past both.
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let style = Style::of(cell);
        if style != *current {
            out.extend_from_slice(style.sgr().as_bytes());
            *current = style;
        }
        push_char(out, cell.c);
        for mark in cell.zerowidth().unwrap_or(&[]) {
            if shown(*mark) {
                push_char(out, *mark);
            }
        }
    }
}

fn blank(cell: &Cell, plain: Style) -> bool {
    cell.c == ' ' && Style::of(cell) == plain && cell.zerowidth().is_none() && !cell.flags.intersects(Flags::WIDE_CHAR)
}

fn push_char(out: &mut Vec<u8>, c: char) {
    let c = if shown(c) { c } else { ' ' };
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// The attributes a run of cells shares: colours, the underline's colour, and the flags that change how a character is drawn (not the
/// wide-character bookkeeping).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    fg: Color,
    bg: Color,
    underline: Option<Color>,
    flags: Flags,
}

impl Style {
    fn of(cell: &Cell) -> Style {
        Style { fg: cell.fg, bg: cell.bg, underline: cell.underline_color(), flags: style_flags(cell.flags) }
    }

    fn plain() -> Style {
        Style::of(&Cell::default())
    }

    /// The SGR that sets these attributes from nothing: it begins with a reset, so nothing of the run before is left on.
    fn sgr(&self) -> String {
        let mut parts = vec!["0".to_string()];
        for (flag, code) in [
            (Flags::BOLD, "1"),
            (Flags::DIM, "2"),
            (Flags::ITALIC, "3"),
            (Flags::INVERSE, "7"),
            (Flags::HIDDEN, "8"),
            (Flags::STRIKEOUT, "9"),
        ] {
            if self.flags.contains(flag) {
                parts.push(code.to_string());
            }
        }
        if let Some(style) = underline_style(self.flags) {
            parts.push(style.to_string());
        }
        parts.push(colour(self.fg, 30, 38, 39));
        parts.push(colour(self.bg, 40, 48, 49));
        if let Some(underline) = self.underline {
            parts.push(underline_colour(underline));
        }
        format!("\x1b[{}m", parts.join(";"))
    }
}

fn style_flags(flags: Flags) -> Flags {
    flags & (Flags::INVERSE | Flags::BOLD | Flags::ITALIC | Flags::DIM | Flags::HIDDEN | Flags::STRIKEOUT | Flags::ALL_UNDERLINES)
}

/// The SGR for an underline's style (`4:1` single, `4:2` double, `4:3` curly, `4:4` dotted, `4:5` dashed), if it has one. alacritty 0.26
/// keeps each of these as a flag of the cell.
fn underline_style(flags: Flags) -> Option<&'static str> {
    [
        (Flags::UNDERLINE, "4:1"),
        (Flags::DOUBLE_UNDERLINE, "4:2"),
        (Flags::UNDERCURL, "4:3"),
        (Flags::DOTTED_UNDERLINE, "4:4"),
        (Flags::DASHED_UNDERLINE, "4:5"),
    ]
    .into_iter()
    .find(|(flag, _)| flags.contains(*flag))
    .map(|(_, code)| code)
}

/// The SGR for an underline's colour (`58`): an indexed colour as an index, a truecolour as RGB, and the named colours that have a
/// palette index as that index. Any other name is the default (`59`). alacritty 0.26 gives the cell its underline colour
/// ([`Cell::underline_color`]).
fn underline_colour(color: Color) -> String {
    match color {
        Color::Named(named) if (named as usize) < 16 => format!("58:5:{}", named as usize),
        Color::Named(_) => "59".to_string(),
        Color::Spec(rgb) => format!("58:2::{}:{}:{}", rgb.r, rgb.g, rgb.b),
        Color::Indexed(index) => format!("58:5:{index}"),
    }
}

/// The SGR parameters for one colour: the eight normal and eight bright named colours by number, the rest as truecolour or as an
/// index. A named colour that is neither (the default foreground and background, the cursor's) is the default.
fn colour(color: Color, base: u8, extended: u8, default: u8) -> String {
    match color {
        Color::Named(named) => match named as usize {
            n @ 0..=7 => (base + n as u8).to_string(),
            n @ 8..=15 => (base + 60 + (n as u8 - 8)).to_string(),
            _ => default.to_string(),
        },
        Color::Spec(rgb) => format!("{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b),
        Color::Indexed(index) => format!("{extended};5;{index}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::screen::{Screen, Sink};
    use super::*;

    struct Quiet;

    impl Sink for Quiet {
        fn reply(&self, _: &[u8]) {}
        fn title(&self, _: Option<String>) {}
        fn bell(&self) {}
    }

    fn screen_with(cols: u16, rows: u16, input: &[u8]) -> Screen {
        let screen = Screen::new(cols, rows, Arc::new(Quiet));
        screen.feed(input, || {});
        screen
    }

    type Cell_ = (char, Vec<char>, Color, Color, Flags, Option<Color>);
    type Seen = (Vec<Cell_>, (usize, usize), bool, TermMode, Option<(usize, usize)>);

    /// What a page would see of the screen: each visible cell's character, marks, colours, underline colour and attributes, the cursor,
    /// whether the alternate screen is up, the modes the snapshot writes, and the scroll region.
    fn seen(screen: &Screen) -> Seen {
        let emu = screen.lock();
        let picture = picture(&emu);
        let cells = picture
            .cells
            .iter()
            .map(|c| (c.c, c.zerowidth().unwrap_or(&[]).to_vec(), c.fg, c.bg, style_flags(c.flags), c.underline_color()))
            .collect();
        (cells, picture.cursor, emu.alt_screen(), picture.mode & written(), picture.region)
    }

    /// The modes the snapshot is responsible for: the ones it writes, origin mode and the keypad.
    fn written() -> TermMode {
        let mut modes = TermMode::APP_KEYPAD | TermMode::ORIGIN;
        for (flag, _) in DECSET.iter().chain(ANSI_MODES) {
            modes |= *flag;
        }
        modes
    }

    /// The scroll region the copy has, from line 0 with the line after its bottom, or `None`.
    fn region_of(screen: &Screen) -> Option<(usize, usize)> {
        screen.lock().region()
    }

    /// Feeds `input`, snapshots the screen, feeds the snapshot to a blank screen of the same size, and checks that the two read the
    /// same. Returns the snapshot.
    fn round_trips(cols: u16, rows: u16, input: &[u8]) -> Vec<u8> {
        let original = screen_with(cols, rows, input);
        let bytes = encode(&picture(&original.lock()));
        let copy = screen_with(cols, rows, &bytes);
        assert_eq!(seen(&copy), seen(&original), "the snapshot of {:?} reads the same", String::from_utf8_lossy(input));
        bytes
    }

    fn has(bytes: &[u8], needle: &str) -> bool {
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    #[test]
    fn text_and_the_cursor_round_trip() {
        round_trips(40, 10, b"hello\r\nworld  x\r\n  third");
        round_trips(40, 10, b"\x1b[5;7Hin the middle");
        round_trips(40, 10, b"");
    }

    #[test]
    fn colours_and_attributes_round_trip() {
        round_trips(
            60,
            10,
            b"\x1b[1;31mred\x1b[0m \x1b[2;4;7mdim-under-inverse\x1b[0m \x1b[38;2;1;2;3mrgb\x1b[48;5;200mx\x1b[0m \x1b[91;102mbright\x1b[0m",
        );
    }

    #[test]
    fn wide_characters_and_marks_round_trip() {
        round_trips(20, 5, "漢字e\u{301}x\r\n\u{65e5}本".as_bytes());
    }

    #[test]
    fn the_modes_a_tui_uses_are_written_and_come_back() {
        // The mouse reports are one mode at a time in alacritty (setting `?1002` clears `?1000`), so the one that is set is the one written.
        let tui = b"\x1b[?1049h\x1b[?1h\x1b=\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[?7l\x1b[?25l\x1b[2;3Hpane";
        let bytes = round_trips(40, 10, tui);
        for needle in ["\x1b[?1049h", "\x1b[?1h", "\x1b=", "\x1b[?2004h", "\x1b[?1002h", "\x1b[?1000l", "\x1b[?1006h", "\x1b[?25l", "\x1b[?7l"] {
            assert!(has(&bytes, needle), "the snapshot writes {needle:?}");
        }
        let reset = round_trips(40, 10, b"\x1b[?1h\x1b[?1l\x1b=\x1b>");
        assert!(has(&reset, "\x1b[?1l") && has(&reset, "\x1b>"), "a mode that is off is written off");
    }

    #[test]
    fn the_alternate_screen_comes_back_as_the_alternate_screen() {
        let bytes = round_trips(30, 6, b"primary\x1b[?1049h\x1b[1;1Halternate");
        assert!(has(&bytes, "\x1b[?1049h"));
        assert!(seen(&screen_with(30, 6, &bytes)).2, "the copy is on the alternate screen");
    }

    #[test]
    fn a_snapshot_starts_with_a_reset_and_each_row_at_its_column_one() {
        let bytes = round_trips(20, 5, b"one\r\ntwo");
        assert!(bytes.starts_with(b"\x1bc"), "a snapshot starts with RIS");
        assert!(has(&bytes, "\x1b[1;1Hone") && has(&bytes, "\x1b[2;1Htwo"), "each row begins at its column 1, with its text");
    }

    #[test]
    fn the_snapshot_is_a_few_bytes_for_a_blank_default_screen() {
        let bytes = encode(&picture(&screen_with(120, 32, b"").lock()));
        assert!(bytes.len() < 1024, "a blank screen is the modes and a clear, {} bytes", bytes.len());
    }

    #[test]
    fn a_scroll_region_round_trips() {
        let input = b"top\r\n\x1b[2;4r\x1b[2;1Hone\r\ntwo\r\nthree\r\nfour";
        assert_eq!(region_of(&screen_with(40, 10, input)), Some((1, 4)), "the screen has the region");
        let bytes = round_trips(40, 10, input);
        assert!(has(&bytes, "\x1b[2;4r"), "the snapshot writes the region");
    }

    #[test]
    fn ris_and_a_bare_region_clear_the_region_in_the_snapshot() {
        for (clear, name) in [("\x1bc", "RIS"), ("\x1b[!p", "DECSTR"), ("\x1b[r", "a bare CSI r")] {
            let screen = screen_with(40, 10, format!("\x1b[2;4r{clear}").as_bytes());
            assert_eq!(region_of(&screen), None, "{name} clears the region");
            let bytes = encode(&picture(&screen.lock()));
            assert!(!has(&bytes, "\x1b[2;4r"), "{name}: the snapshot writes no region");
        }
    }

    #[test]
    fn a_region_the_page_ignores_is_not_kept() {
        // A top on the last line with a bottom past it: the page's xterm ignores it, the guard cancels it, so the copy keeps no region.
        let input = b"\x1b[10;12rtop\r\nmid";
        assert_eq!(region_of(&screen_with(40, 10, input)), None, "the page ignores the region, so the copy has none");
        round_trips(40, 10, input);
    }

    #[test]
    fn origin_mode_inside_a_region_round_trips() {
        // The rows are addressed from the region's top while origin mode is on, so the snapshot draws them with it off and sets it last.
        let bytes = round_trips(30, 8, b"above\r\n\x1b[3;6r\x1b[?6h\x1b[2;2Hinside");
        assert!(has(&bytes, "\x1b[?6h"), "the snapshot sets origin mode");
        round_trips(30, 8, b"above\r\n\x1b[3;6r\x1b[?6h\x1b[2;2Hinside\x1b[?6l\x1b[7;1Hbelow");
    }

    #[test]
    fn underline_styles_and_colours_round_trip() {
        let input = b"\x1b[4:2mdouble\x1b[4:3mcurl\x1b[4:4mdot\x1b[4:5mdash\x1b[0m \x1b[4:1msingle\x1b[0m \
            \x1b[4;58:2::1:2:3mrgb\x1b[58:5:196mindexed\x1b[0m \x1b[58:5:9mplain\x1b[0m";
        let bytes = round_trips(80, 4, input);
        for needle in ["4:1", "4:2", "4:3", "4:4", "4:5", "58:2::1:2:3", "58:5:196", "58:5:9"] {
            assert!(has(&bytes, needle), "the snapshot writes {needle:?}");
        }
    }
}
