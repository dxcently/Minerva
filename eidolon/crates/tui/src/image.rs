//! Drawing an attached image where the terminal can draw one.
//!
//! The other half of the feature — reading a file, deciding what it is,
//! turning it into a content block — is `eidolon_tools::image`, and is
//! re-exported here so a consumer of the TUI has one name for both. The
//! split is the layering: what a *model* can do with an image is an engine
//! question the headless `run --image` asks too; what a *terminal* can do
//! with it is this crate's alone.
//!
//! **Every terminal shows the placard** — a line saying what the image is.
//! Only some show the pixels, and they disagree about how: [`Protocol::Kitty`]
//! takes a PNG whole, [`Protocol::Sixel`] wants quantised pixels. Neither
//! is detectable without a round-trip to the terminal, which is not
//! something to spend on a launch, so detection is by environment and the
//! operator can override it (`[images] inline` in `config.toml`). The
//! placard is drawn either way and is never *replaced* by the pixels — it
//! is the caption under them — so a session over ssh into something plain
//! loses the picture and not the fact that there was one.

use std::io::Cursor;

use base64::Engine;

pub use eidolon_tools::image::{Attachment, from_clipboard, human_bytes, sniff};

/// How this terminal takes pixels, if it takes them at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Protocol {
    /// Placard only. The resting answer for anything unrecognised.
    #[default]
    Off,
    /// The kitty graphics protocol: a PNG handed over whole, so nothing
    /// here decodes or quantises anything.
    Kitty,
    /// Sixel: six-pixel bands over an indexed palette, which means this
    /// side does the scaling and the colour reduction.
    Sixel,
}

impl Protocol {
    /// What `[images] inline` says, or what the environment suggests.
    ///
    /// `auto` reads the environment and never asks the terminal. The
    /// honest way to know is a DA1 query (`ESC [ c`, read the reply, look
    /// for `;4`), and that is a *round-trip on a terminal that may not
    /// answer* — a thing to keep off the path to the first frame, and a
    /// hang to explain afterwards. Environment variables are a guess, so
    /// the setting is there to be set when the guess is wrong.
    pub fn resolve(setting: &str) -> Protocol {
        match setting.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" => Protocol::Off,
            "kitty" => Protocol::Kitty,
            "sixel" => Protocol::Sixel,
            _ => Protocol::detect(),
        }
    }

    fn detect() -> Protocol {
        let var = |k: &str| std::env::var(k).unwrap_or_default().to_ascii_lowercase();
        let term = var("TERM");
        let program = var("TERM_PROGRAM");
        // Kitty's own protocol, in the three terminals that speak it well.
        if std::env::var_os("KITTY_WINDOW_ID").is_some()
            || term.contains("kitty")
            || program.contains("ghostty")
            || program.contains("wezterm")
        {
            return Protocol::Kitty;
        }
        // Sixel needs an explicit opt-in. Environment guesses were too
        // eager: foot and xterm often advertise enough to look plausible,
        // but the ordinary expectation is that pasted images stay textual
        // unless the operator asked for pixels.
        if std::env::var_os("EIDOLON_ENABLE_SIXEL").is_some()
            && (term.starts_with("foot")
                || term.contains("sixel")
                || term.contains("mlterm")
                || term.starts_with("contour")
                || term.starts_with("xterm"))
        {
            return Protocol::Sixel;
        }
        Protocol::Off
    }
}

/// One image to be painted over cells the frame has already reserved.
///
/// Collected during the layout walk and emitted *after* ratatui has
/// flushed its diff, because ratatui writes cells and this writes pixels
/// over them: drawn first, the frame's own output would paint straight
/// back over the picture. The cells underneath are marked skip so the next
/// frame does not erase what it did not draw.
#[derive(Clone, PartialEq, Eq)]
pub struct Placement {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
    pub data: String,
    pub media_type: String,
}

/// A placement debugs as where it is and how big, never as its bytes.
///
/// Derived, a failing assertion about one printed several megabytes of
/// base64 into the test output — which is exactly the moment the message
/// most needs to be readable.
impl std::fmt::Debug for Placement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Placement({},{} {}x{} {} {}B)",
            self.col,
            self.row,
            self.cols,
            self.rows,
            self.media_type,
            self.data.len()
        )
    }
}

impl Placement {
    /// This attachment, at a spot the layout reserved for it.
    pub fn of(a: &Attachment, col: u16, row: u16, cols: u16, rows: u16) -> Placement {
        Placement {
            col,
            row,
            cols,
            rows,
            data: a.data.clone(),
            media_type: a.media_type.clone(),
        }
    }

    /// Is this the same picture, in the same place, as `other`?
    ///
    /// Compared by size and not by content: the payloads are megabytes and
    /// this runs once per image per frame. Two *different* images of
    /// exactly equal encoded length landing on exactly the same cells in
    /// consecutive frames is the collision, and its cost is one frame of a
    /// stale thumbnail — cheaper than hashing four megabytes at 30 Hz.
    pub fn same_as(&self, other: &Placement) -> bool {
        (self.col, self.row, self.cols, self.rows) == (other.col, other.row, other.cols, other.rows)
            && self.data.len() == other.data.len()
            && self.media_type == other.media_type
    }
}

/// The escape sequence that draws `placement`, or `None` if this terminal
/// takes none.
///
/// Everything is positioned absolutely and the cursor is put back, because
/// the caller is between ratatui's flush and its next frame and ratatui
/// believes it knows where the cursor is.
pub fn draw(p: &Placement, proto: Protocol, cell: (u16, u16)) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&p.data)
        .ok()?;
    let body = match proto {
        Protocol::Off => return None,
        Protocol::Kitty => kitty(&bytes, &p.media_type, p.cols, p.rows)?,
        Protocol::Sixel => sixel(
            &bytes,
            u32::from(p.cols) * u32::from(cell.0),
            u32::from(p.rows) * u32::from(cell.1),
        )?,
    };
    // `ESC 7` / `ESC 8` save and restore the cursor, so the position
    // ratatui thinks it left behind is the position it finds.
    Some(format!("\x1b7\x1b[{};{}H{body}\x1b8", p.row + 1, p.col + 1))
}

/// The kitty graphics protocol. `f=100` is "these are PNG bytes", so a PNG
/// goes straight over and anything else is transcoded once; `c`/`r` scale
/// it into the cell box the layout reserved. Payloads are chunked at 4096
/// base64 characters because that is the protocol's limit.
fn kitty(bytes: &[u8], media_type: &str, cols: u16, rows: u16) -> Option<String> {
    let png;
    let payload = if media_type == "image/png" {
        bytes
    } else {
        let img = image::load_from_memory(bytes).ok()?;
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .ok()?;
        png = out;
        &png
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
    let mut out = String::with_capacity(b64.len() + 256);
    let chunks: Vec<&str> = b64
        .as_bytes()
        .chunks(4096)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        if i == 0 {
            // `a=T` transmit-and-display, `C=1` leave the cursor alone.
            out.push_str(&format!(
                "\x1b_Ga=T,f=100,C=1,c={cols},r={rows},m={more};{chunk}\x1b\\"
            ));
        } else {
            out.push_str(&format!("\x1b_Gm={more};{chunk}\x1b\\"));
        }
    }
    Some(out)
}

/// Sixel, over a fixed 6×6×6 colour cube.
///
/// A cube rather than a quantiser fitted to the image: 216 colours is
/// coarse for a photograph and entirely adequate for the screenshots and
/// diagrams this exists to show, and it costs no dependency, no second
/// pass over the pixels and no non-determinism. If a photograph ever needs
/// to look right, that is the moment to fit a palette — not before.
///
/// The format: `ESC P q` opens, `"1;1;W;H` declares the size, `#n;2;r;g;b`
/// defines colour *n* in percentages, `#n` selects it, characters `?`..`~`
/// carry six vertical pixels apiece as a bitmask, `!n` repeats the next
/// one, `$` returns to the start of the band and `-` starts the next.
fn sixel(bytes: &[u8], max_w: u32, max_h: u32) -> Option<String> {
    if max_w == 0 || max_h == 0 {
        return None;
    }
    let img = image::load_from_memory(bytes).ok()?;
    let img = img
        .resize(max_w, max_h, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }

    let index = |p: &image::Rgb<u8>| {
        let q = |v: u8| u32::from(v) * 5 / 255;
        q(p.0[0]) * 36 + q(p.0[1]) * 6 + q(p.0[2])
    };

    let mut out = String::with_capacity((w * h / 4) as usize + 4096);
    out.push_str("\x1bPq");
    out.push_str(&format!("\"1;1;{w};{h}"));
    for i in 0..216u32 {
        let (r, g, b) = (i / 36, (i / 6) % 6, i % 6);
        // Sixel colour components are percentages, not bytes.
        out.push_str(&format!("#{i};2;{};{};{}", r * 20, g * 20, b * 20));
    }

    // One band is six pixel rows. Within a band each colour is written as
    // its own pass over the width, and a pass that would set no pixels is
    // skipped — which is what keeps 216 colours from costing 216 empty
    // rows per band.
    let mut used = vec![false; 216];
    for band in 0..h.div_ceil(6) {
        let top = band * 6;
        used.iter_mut().for_each(|u| *u = false);
        for y in top..(top + 6).min(h) {
            for x in 0..w {
                used[index(img.get_pixel(x, y)) as usize] = true;
            }
        }
        let mut first = true;
        for (colour, _) in used.iter().enumerate().filter(|(_, u)| **u) {
            if !first {
                out.push('$');
            }
            first = false;
            out.push_str(&format!("#{colour}"));
            // Run-length encode the band's row for this colour.
            let (mut run, mut last) = (0u32, u8::MAX);
            for x in 0..w {
                let mut bits = 0u8;
                for k in 0..6u32 {
                    let y = top + k;
                    if y < h && index(img.get_pixel(x, y)) as usize == colour {
                        bits |= 1 << k;
                    }
                }
                if bits == last {
                    run += 1;
                } else {
                    push_run(&mut out, last, run);
                    (run, last) = (1, bits);
                }
            }
            push_run(&mut out, last, run);
        }
        out.push('-');
    }
    out.push_str("\x1b\\");
    Some(out)
}

fn push_run(out: &mut String, bits: u8, run: u32) {
    if run == 0 || bits == u8::MAX {
        return;
    }
    let c = char::from(b'?' + bits);
    // `!n` is cheaper than three characters and dearer than two.
    if run > 3 {
        out.push_str(&format!("!{run}{c}"));
    } else {
        for _ in 0..run {
            out.push(c);
        }
    }
}

/// How many rows an image of these dimensions wants, drawn `cols` wide.
///
/// The aspect ratio is in *pixels* and the answer is in *cells*, so the
/// cell's own shape has to come into it — a terminal cell is about twice
/// as tall as it is wide, and an image laid out as though it were square
/// comes out twice as tall as the space it was given.
pub fn rows_for(dims: Option<(u32, u32)>, cols: u16, cell: (u16, u16), max_rows: u16) -> u16 {
    let Some((w, h)) = dims.filter(|(w, h)| *w > 0 && *h > 0) else {
        return 1;
    };
    let (cw, ch) = (f64::from(cell.0.max(1)), f64::from(cell.1.max(1)));
    let rows = f64::from(h) / f64::from(w) * f64::from(cols) * cw / ch;
    (rows.ceil() as u16).clamp(1, max_rows)
}

/// The terminal's cell size in pixels, for the two places that need to
/// turn an aspect ratio into a row count. Falls back to 8×16, which is
/// close enough that a picture is the right shape rather than exact.
pub fn cell_size() -> (u16, u16) {
    match ratatui::crossterm::terminal::window_size() {
        Ok(w) if w.width > 0 && w.height > 0 && w.columns > 0 && w.rows > 0 => {
            (w.width / w.columns, w.height / w.rows)
        }
        _ => (8, 16),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn an_explicit_setting_beats_whatever_the_environment_looks_like() {
        assert_eq!(Protocol::resolve("off"), Protocol::Off);
        assert_eq!(Protocol::resolve("Sixel"), Protocol::Sixel);
        assert_eq!(Protocol::resolve("kitty"), Protocol::Kitty);
    }

    /// A cell is about twice as tall as it is wide, so a square image is
    /// half as many rows as it is columns. Laid out as though the cell
    /// were square it comes out twice as tall as the space it was given.
    #[test]
    fn a_square_image_is_half_as_many_rows_as_columns() {
        assert_eq!(rows_for(Some((100, 100)), 20, (8, 16), 100), 10);
        assert_eq!(rows_for(Some((100, 50)), 20, (8, 16), 100), 5);
    }

    #[test]
    fn an_image_of_unknown_size_still_occupies_a_row() {
        assert_eq!(rows_for(None, 20, (8, 16), 100), 1);
        assert_eq!(rows_for(Some((0, 0)), 20, (8, 16), 100), 1);
    }

    #[test]
    fn a_very_tall_image_is_capped_rather_than_filling_the_screen() {
        assert_eq!(rows_for(Some((10, 4000)), 20, (8, 16), 12), 12);
    }

    #[test]
    fn nothing_is_drawn_where_the_terminal_takes_nothing() {
        let p = Placement {
            col: 0,
            row: 0,
            cols: 10,
            rows: 5,
            data: String::new(),
            media_type: "image/png".into(),
        };
        assert!(draw(&p, Protocol::Off, (8, 16)).is_none());
    }

    fn placement(bytes: &[u8]) -> Placement {
        Placement {
            col: 2,
            row: 3,
            cols: 10,
            rows: 5,
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            media_type: "image/png".into(),
        }
    }

    /// The escape is positioned absolutely and puts the cursor back:
    /// ratatui is between frames and believes it knows where it left it.
    #[test]
    fn a_drawn_image_leaves_the_cursor_where_it_found_it() {
        let s = draw(&placement(&png(8, 8)), Protocol::Kitty, (8, 16)).unwrap();
        assert!(s.starts_with("\x1b7\x1b[4;3H"), "{:?}", &s[..20]);
        assert!(s.ends_with("\x1b8"));
    }

    #[test]
    fn kitty_gets_the_png_whole_and_the_cell_box_to_fit_it_in() {
        let s = draw(&placement(&png(8, 8)), Protocol::Kitty, (8, 16)).unwrap();
        assert!(s.contains("\x1b_Ga=T,f=100,C=1,c=10,r=5,m=0;"), "{s:?}");
    }

    /// A payload over the protocol's 4096-character limit arrives as
    /// several chunks, and only the last says it is the last.
    #[test]
    fn a_large_kitty_payload_is_chunked() {
        let s = draw(&placement(&png(400, 400)), Protocol::Kitty, (8, 16)).unwrap();
        assert!(s.matches("\x1b_G").count() > 1, "one chunk only");
        assert!(s.contains("m=1"), "no continuation chunk");
        assert_eq!(s.matches("m=0;").count(), 1, "more than one final chunk");
    }

    #[test]
    fn sixel_opens_and_closes_and_declares_its_size() {
        let s = draw(&placement(&png(16, 16)), Protocol::Sixel, (8, 16)).unwrap();
        assert!(s.contains("\x1bPq"), "no sixel introducer");
        assert!(s.contains("\"1;1;"), "no raster attributes");
        assert!(s.contains("#0;2;"), "no palette");
        assert!(s.contains("\x1b\\"), "unterminated");
    }

    /// A run is only worth `!n` when it is longer than the characters it
    /// replaces; below that the escape costs more than it saves.
    #[test]
    fn short_runs_are_written_out_and_long_ones_are_counted() {
        let mut s = String::new();
        push_run(&mut s, 0b000001, 3);
        assert_eq!(s, "@@@");
        s.clear();
        push_run(&mut s, 0b000001, 9);
        assert_eq!(s, "!9@");
    }

    /// An empty band writes nothing at all — `bits == u8::MAX` is the
    /// "nothing here yet" sentinel the run loop starts on, and emitting it
    /// would put a character with no meaning into the stream.
    #[test]
    fn the_run_sentinel_never_reaches_the_stream() {
        let mut s = String::new();
        push_run(&mut s, u8::MAX, 5);
        push_run(&mut s, 0b000001, 0);
        assert!(s.is_empty(), "{s:?}");
    }

    #[test]
    fn a_sixel_image_uses_more_than_one_colour_when_it_has_more_than_one() {
        let img = image::RgbImage::from_fn(24, 24, |x, y| {
            image::Rgb([(x * 10) as u8, (y * 10) as u8, 128])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        let s = sixel(&bytes, 200, 200).unwrap();
        // A palette entry is `#n;2;r;g;b`; a *select* is `#n` followed by
        // anything else. Only the selects say what was actually painted.
        let mut selects = std::collections::HashSet::new();
        for part in s.split('#').skip(1) {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() && !part[digits.len()..].starts_with(';') {
                selects.insert(digits);
            }
        }
        assert!(
            selects.len() > 1,
            "a gradient came out {} colour(s)",
            selects.len()
        );
    }
}
