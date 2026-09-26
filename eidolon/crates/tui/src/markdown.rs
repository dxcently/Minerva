//! Markdown for the transcript.
//!
//! A model writes markdown whether or not anyone renders it, so the
//! transcript renders it: headings, emphasis, code, lists, quotes, rules
//! and pipe tables become styled [`Line`]s at a given width. This is a
//! *terminal* renderer, not a conformant CommonMark one — it is
//! line-oriented on purpose, so a half-streamed paragraph or an unclosed
//! fence still draws something sensible on the frame it arrives in.
//!
//! Blocks are recognised per line ([`render`]); inline markup inside a
//! block is scanned by [`inline`] into spans, which [`wrap_spans`] then
//! word-wraps with a per-block prefix (a bullet, a quote bar, a code
//! gutter) that repeats as a hanging indent on continuation lines.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::{borrow::Cow, rc::Rc};
use unicode_segmentation::UnicodeSegmentation;

/// The four colours markdown uses, cut from the theme once per render so
/// the inline scanner is not looking roles up per span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// Inline code, fenced blocks.
    pub code: Color,
    /// Level one and two headings.
    pub heading: Color,
    /// Link labels.
    pub link: Color,
    /// Fences, rules, quote bars, list markers, table rules.
    pub faint: Style,
    /// Behind a fenced block, as a background.
    pub block: Style,
    /// What the pieces of a line of code are painted: the scanner in
    /// [`crate::syntax`] names a run and the theme says what the name
    /// looks like, so a colour scheme never patches the scanner.
    /// Ordered as [`crate::syntax::Role`] lists them.
    pub syntax: [Color; 4],
    /// A fenced block's line numbers.
    pub gutter: Style,
    /// What a call sent: the rail and hole letter of a payload block.
    pub sent: Style,
}

impl Default for Palette {
    fn default() -> Self {
        Palette::from(&crate::theme::Theme::builtin())
    }
}

impl From<&crate::theme::Theme> for Palette {
    fn from(t: &crate::theme::Theme) -> Self {
        Palette {
            code: t.colour("code"),
            heading: t.colour("heading"),
            link: t.colour("link"),
            faint: t.fg("faint"),
            block: t.bg("block"),
            gutter: t.fg("gutter"),
            sent: t.fg("sent"),
            syntax: [
                t.colour("comment"),
                t.colour("string"),
                t.colour("number"),
                t.colour("keyword"),
            ],
        }
    }
}

impl Palette {
    fn dim(&self) -> Style {
        self.faint
    }
}

/// A destination survives rendering; URLs are retained but never launched by the vault reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target { Vault(String), Url(String) }

/// One occurrence, potentially wrapped over several rows. Ranges are UTF-8 byte offsets
/// in the rendered row, not source offsets. Mouse and keyboard consume these same runs.
#[derive(Clone, Debug)]
pub struct Link {
    pub target: Target,
    pub runs: Vec<(usize, std::ops::Range<usize>)>,
}

pub struct Document {
    pub lines: Vec<Line<'static>>,
    pub links: Vec<Link>,
    /// Original Markdown line (1-based) for each rendered row, independent of wrapping.
    pub source_lines: Vec<usize>,
    /// Literal heading text and Obsidian block IDs, with their rendered row.
    pub anchors: Vec<(String, usize)>,
}

#[derive(Clone)]
struct RichSpan<'a> {
    content: Cow<'a, str>,
    style: Style,
    link: Option<Rc<Target>>,
}
impl<'a> RichSpan<'a> {
    fn raw(s: impl Into<Cow<'a, str>>) -> Self { Self::styled(s, Style::default()) }
    fn styled(s: impl Into<Cow<'a, str>>, style: Style) -> Self { Self { content: s.into(), style, link: None } }
    fn fragment(&self, s: impl Into<Cow<'a, str>>) -> Self { Self { content: s.into(), style: self.style, link: self.link.clone() } }
    fn into_span(self) -> Span<'a> { Span::styled(self.content, self.style) }
}
#[derive(Default)]
struct RichLine<'a> { spans: Vec<RichSpan<'a>>, anchor: Option<String>, source: usize }
impl<'a> RichLine<'a> {
    fn from(spans: Vec<RichSpan<'a>>) -> Self { Self { spans, anchor: None, source: 0 } }
    fn styled(s: impl Into<Cow<'a, str>>, style: Style) -> Self { Self::from(vec![RichSpan::styled(s, style)]) }
    fn width(&self) -> usize { self.spans.iter().map(|s| Span::raw(s.content.as_ref()).width()).sum() }
    fn into_line(self) -> Line<'a> { Line::from(self.spans.into_iter().map(RichSpan::into_span).collect::<Vec<_>>()) }
}

/// Render once into styled rows and the destinations attached to them. Code has no links.
pub fn document(text: &str, width: usize, p: &Palette) -> Document {
    let rich = rich_render(text, width, p);
    let mut targets: Vec<Rc<Target>> = Vec::new();
    let mut links: Vec<Link> = Vec::new();
    for (row, line) in rich.iter().enumerate() {
        let mut byte = 0;
        for span in &line.spans {
            let end = byte + span.content.len();
            if let Some(target) = &span.link && end > byte {
                let id = targets.iter().position(|t| Rc::ptr_eq(t, target)).unwrap_or_else(|| {
                    targets.push(target.clone());
                    links.push(Link { target: target.as_ref().clone(), runs: Vec::new() });
                    links.len() - 1
                });
                links[id].runs.push((row, byte..end));
            }
            byte = end;
        }
    }
    let anchors = rich.iter().enumerate().filter_map(|(i, l)| l.anchor.clone().map(|a| (a, i))).collect();
    let source_lines = rich.iter().map(|l| l.source).collect();
    Document { source_lines, lines: rich.into_iter().map(RichLine::into_line).collect(), links, anchors }
}

pub fn render(text: &str, width: usize, p: &Palette) -> Vec<Line<'static>> {
    rich_render(text, width, p).into_iter().map(RichLine::into_line).collect()
}
pub fn inline(s: &str, base: Style, p: &Palette) -> Vec<Span<'static>> {
    rich_inline(s, base, p).into_iter().map(RichSpan::into_span).collect()
}
pub fn wrap_spans(spans: Vec<Span<'static>>, width: usize, first: &[Span<'static>], cont: &[Span<'static>]) -> Vec<Line<'static>> {
    let lift = |s: &Span<'static>| RichSpan::styled(s.content.clone(), s.style);
    rich_wrap(spans.iter().map(lift).collect(), width, &first.iter().map(lift).collect::<Vec<_>>(),
        &cont.iter().map(lift).collect::<Vec<_>>()).into_iter().map(RichLine::into_line).collect()
}

/// Incomplete links stay literal during streaming. Aliases are display text, never markup.
fn wikilink(ch: &[char], i: usize) -> Option<(String, String, usize)> {
    let end = (i + 2..ch.len().saturating_sub(1)).find(|&j| ch[j] == ']' && ch[j + 1] == ']')?;
    let inner: String = ch[i + 2..end].iter().collect();
    if inner.contains(['[', ']', '\n']) { return None; }
    let (target, label) = inner.split_once('|').unwrap_or((&inner, &inner));
    if target.trim().is_empty() || label.trim().is_empty() { return None; }
    Some((target.trim().into(), label.trim().into(), end + 2))
}

/// Render `text` as markdown, wrapped to `width` columns.
fn rich_render(text: &str, width: usize, p: &Palette) -> Vec<RichLine<'static>> {
    let width = width.max(8);
    let src: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<RichLine<'static>> = Vec::new();
    let mut i = 0;

    while i < src.len() {
        let start = out.len();
        let raw = src[i];
        let t = raw.trim_start();
        let indent = raw.chars().take_while(|c| *c == ' ' || *c == '\t').count();

        // Fenced code: everything to the closing fence, or to the end of
        // what has arrived so far (streaming leaves fences open).
        if let Some((fc, n)) = fence(t) {
            let (name, tongue) = split_info(t[n..].trim());
            let lang = crate::syntax::lang(tongue);
            out.extend(block_title(name, width, p));
            for line in &mut out[start..] { line.source = i + 1; }
            i += 1;
            // The block is gathered before it is drawn: the gutter's
            // width is how many lines there turn out to be.
            let first = i;
            let mut body: Vec<&str> = Vec::new();
            while i < src.len() {
                let l = src[i].trim_start();
                if fence(l).is_some_and(|(c, m)| c == fc && m >= n) {
                    i += 1;
                    break;
                }
                body.push(src[i]);
                i += 1;
            }
            // A one-line block is not worth a gutter saying `1`.
            let digits = if body.len() > 1 { body.len().to_string().len() } else { 0 };
            let room = width.saturating_sub(2 + if digits > 0 { digits + 1 } else { 0 });
            for (k, l) in body.iter().enumerate() {
                let at = out.len();
                for (c, chunk) in hard_chunks(l, room).into_iter().enumerate() {
                    let mut spans = Vec::new();
                    if digits > 0 {
                        // A wrapped line's continuation is the same line,
                        // so the number is not said twice.
                        let label = match c {
                            0 => format!("{:>digits$} ", k + 1),
                            _ => " ".repeat(digits + 1),
                        };
                        // The slab is one ground: the numbers take it
                        // like the code, and the `gutter` role alone is
                        // what sets them apart.
                        spans.push(RichSpan::styled(label, p.gutter));
                    }
                    spans.extend(code_spans(&chunk, &lang, p));
                    out.push(block_line(spans, width, p));
                }
                for line in &mut out[at..] { line.source = first + k + 1; }
            }
            continue;
        }

        // A payload fence: the inline vault channel's `!!a … !!`, binding
        // the bytes between the fences to the command's `$a` hole. The
        // fences are protocol rather than prose, so they are not drawn:
        // the block is the slab [`payload_rich`] draws, letter on the
        // rail, bytes inside. Like a code fence it may still be open
        // while streaming, and runs to the end of what has arrived; a
        // `!!`-shaped line the channel would not bind — a wider opener,
        // a closer with no block open — falls through as prose, and a
        // fence inside a code block was had by the branch above.
        if let Some(letter) = payload_opener(raw.strip_suffix('\r').unwrap_or(raw)) {
            let first = i + 2;
            let mut body: Vec<&str> = Vec::new();
            i += 1;
            while i < src.len() {
                let l = src[i].strip_suffix('\r').unwrap_or(src[i]);
                if l == "!!" {
                    i += 1;
                    break;
                }
                body.push(l);
                i += 1;
            }
            let rows = payload_rich(letter, &body.join("\n"), width, p);
            for (line, n) in rows {
                let at = out.len();
                out.push(line);
                for l in &mut out[at..] { l.source = first + n; }
            }
            continue;
        }

        // An indented block — four spaces after a blank line, not under a
        // list — is code: pasted output, a diagram, a snippet nobody
        // fenced. Soft-wrapping folds the shape it was pasted for, so it
        // is ruled and chunked like a fence instead.
        let prev_blank = i == 0 || src[i - 1].trim().is_empty();
        if indent >= 4
            && !t.is_empty()
            && prev_blank
            && list_item(t, indent).is_none()
            && !under_a_list(&src, i)
        {
            while i < src.len() {
                let l = src[i];
                let ind = l.chars().take_while(|c| *c == ' ' || *c == '\t').count();
                let blank = l.trim().is_empty();
                if !blank && ind < 4 {
                    break;
                }
                // A blank line is the block's only if more of it follows.
                if blank
                    && !src[i + 1..]
                        .iter()
                        .find(|n| !n.trim().is_empty())
                        .is_some_and(|n| n.chars().take_while(|c| *c == ' ' || *c == '\t').count() >= 4)
                {
                    break;
                }
                let at = out.len();
                let body: String = l.chars().skip(4).collect();
                for chunk in hard_chunks(&body, width.saturating_sub(2)) {
                    out.push(block_line(
                        vec![RichSpan::styled(chunk, Style::default().fg(p.code))],
                        width,
                        p,
                    ));
                }
                for line in &mut out[at..] {
                    line.source = i + 1;
                }
                i += 1;
            }
            continue;
        }

        // Thematic break.
        if is_rule(t) {
            out.push(RichLine::styled("─".repeat(width), p.dim()));
            for line in &mut out[start..] { line.source = i + 1; }
            i += 1;
            continue;
        }

        // A pipe table, if the row under this one is its separator.
        if let Some((rows, next)) = table_at(&src, i) {
            out.extend(table(&rows, width, p));
            // `table` numbers its lines by the source row each came from,
            // which a wrapped cell makes many-to-one; the rendered line's
            // own index would address the wrong row.
            for line in &mut out[start..] { line.source += i + 1; }
            i = next;
            continue;
        }

        // ATX heading.
        if let Some((level, rest)) = heading(t) {
            let style = match level {
                1 | 2 => Style::default().fg(p.heading).add_modifier(Modifier::BOLD),
                _ => Style::default().add_modifier(Modifier::BOLD),
            };
            if level <= 2 && out.last().is_some_and(|l| l.width() > 0) {
                out.push(RichLine::default());
            }
            let mut lines = rich_wrap(rich_inline(rest, style, p), width, &[], &[]);
            if let Some(first) = lines.first_mut() { first.anchor = Some(plain(rest)); }
            out.extend(lines);
            for line in &mut out[start..] { line.source = i + 1; }
            i += 1;
            continue;
        }

        // Blockquote.
        if let Some(rest) = t.strip_prefix('>') {
            let bar = vec![RichSpan::styled("▏ ", p.dim())];
            let style = p.dim().add_modifier(Modifier::ITALIC);
            out.extend(wrap_block(rest.trim_start(), style, p, width, &bar, &bar));
            for line in &mut out[start..] { line.source = i + 1; }
            i += 1;
            continue;
        }

        // List item, bullet or ordered. Nesting comes from the source
        // indent; the marker's width becomes the hanging indent.
        if let Some((marker, rest)) = list_item(t, indent) {
            let pad = " ".repeat(indent.min(width / 2));
            let first = vec![
                RichSpan::raw(pad.clone()),
                RichSpan::styled(marker.clone(), p.dim()),
            ];
            let cont = vec![RichSpan::raw(format!(
                "{pad}{}",
                " ".repeat(marker.chars().count())
            ))];
            out.extend(wrap_block(rest, Style::default(), p, width, &first, &cont));
            for line in &mut out[start..] { line.source = i + 1; }
            i += 1;
            continue;
        }

        // A plain line. Blank stays blank; otherwise wrap it on its own —
        // a model's single newlines are meant, so they are not reflowed
        // into the previous line the way CommonMark would.
        if t.is_empty() {
            out.push(RichLine::default());
        } else {
            let pad = " ".repeat(indent.min(width / 2));
            let lead = vec![RichSpan::raw(pad)];
            out.extend(wrap_block(t, Style::default(), p, width, &lead, &lead));
        }
        for line in &mut out[start..] { line.source = i + 1; }
        i += 1;
    }
    out
}

/// A block ID belongs to the block, not a substring of arbitrary rendered text.
fn wrap_block(s: &str, style: Style, p: &Palette, width: usize, first: &[RichSpan<'static>], cont: &[RichSpan<'static>])
    -> Vec<RichLine<'static>> {
    let mut lines = rich_wrap(rich_inline(s, style, p), width, first, cont);
    if let Some(id) = s.split_whitespace().last().and_then(|t| t.strip_prefix('^'))
        && !id.is_empty() && id.chars().all(|c| c.is_alphanumeric() || c == '-')
        && let Some(first) = lines.first_mut()
    { first.anchor = Some(format!("^{id}")); }
    lines
}

// ------------------------------------------------------------- block tests

/// A fenced block is a slab: every line of it carries the `block`
/// background, padded to the pane, so the block's extent is a shape
/// rather than a pair of rules across the page. When the fence names a
/// file the slab opens on that name; the language is not said, being
/// legible from the code and from the name itself.
fn block_line(spans: Vec<RichSpan<'static>>, width: usize, p: &Palette) -> RichLine<'static> {
    let used: usize = spans.iter().map(|s| columns(&s.content)).sum();
    let mut spans: Vec<RichSpan<'static>> = spans
        .into_iter()
        .map(|s| RichSpan { style: s.style.patch(p.block), ..s })
        .collect();
    spans.insert(0, RichSpan::styled(" ", p.block));
    spans.push(RichSpan::styled(" ".repeat(width.saturating_sub(used + 1)), p.block));
    RichLine::from(spans)
}

/// Is this line a payload fence opener — `!!` and exactly one letter or
/// digit, nothing else? The same shape the channel binds, and anything
/// else behind `!!` is prose everywhere.
pub fn payload_opener(body: &str) -> Option<char> {
    let rest = body.strip_prefix("!!")?;
    let mut ch = rest.chars();
    match (ch.next(), ch.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c),
        _ => None,
    }
}

/// A payload block as a slab of its own: a rail and the hole letter
/// naming what the bytes are bound to, the bytes verbatim inside the
/// slab, and nothing for the closer — the fences are the channel's
/// syntax, not something a person reads. The rail and the letter take
/// the `sent` role, because this is what a call sent; the bytes keep the
/// code colour, because they are verbatim; and every row sits on the
/// block ground so the whole of it reads as one thing. Hard-chunked, not
/// word-wrapped: the bytes were chosen for the note they go into, and
/// reflowing them for the pane would show a shape the note will not
/// have.
///
/// Returns each row with the payload line it came from (0-based), so a
/// caller rendering a document can give every row the provenance of the
/// source line it stands for.
fn payload_rich(letter: char, payload: &str, width: usize, p: &Palette) -> Vec<(RichLine<'static>, usize)> {
    let width = width.max(8);
    let rail = p.sent;
    let mut out = Vec::new();
    out.push((
        block_line(
            vec![
                RichSpan::styled("\u{258e} ", rail),
                RichSpan::styled(letter.to_string(), rail.add_modifier(Modifier::BOLD)),
            ],
            width,
            p,
        ),
        0,
    ));
    for (n, line) in payload.split('\n').enumerate() {
        for (c, chunk) in hard_chunks(line, width - 2).into_iter().enumerate() {
            let mut spans = vec![RichSpan::styled(
                if c == 0 { "\u{258e} " } else { "  " },
                rail,
            )];
            spans.push(RichSpan::styled(chunk, Style::default().fg(p.code)));
            out.push((block_line(spans, width, p), n));
        }
    }
    out
}

/// [`payload_rich`] as plain lines, for a caller outside a document —
/// the Ask block, which draws the payload where a tool call draws its
/// diff and has no source lines to address.
pub fn payload_lines(letter: char, payload: &str, width: usize, p: &Palette) -> Vec<Line<'static>> {
    payload_rich(letter, payload, width, p)
        .into_iter()
        .map(|(l, _)| l.into_line())
        .collect()
}

/// One line of code as coloured runs: the scanner says what each run
/// is, the palette says what that looks like, and what it does not name
/// keeps the ordinary code colour.
fn code_spans(text: &str, lang: &crate::syntax::Lang, p: &Palette) -> Vec<RichSpan<'static>> {
    let plain = Style::default().fg(p.code);
    let (mut out, mut at) = (Vec::new(), 0);
    for (range, role) in crate::syntax::scan(text, lang) {
        if range.start > at {
            out.push(RichSpan::styled(text[at..range.start].to_string(), plain));
        }
        let style = Style::default().fg(p.syntax[role.index()]);
        out.push(RichSpan::styled(text[range.clone()].to_string(), style));
        at = range.end;
    }
    if at < text.len() {
        out.push(RichSpan::styled(text[at..].to_string(), plain));
    }
    out
}

/// One line of code as ratatui spans, coloured by role — for a diff,
/// which is code with a sign in front of it, and anywhere else outside
/// this module that draws a line of a file.
pub fn code_line(text: &str, lang: &str, p: &Palette) -> Vec<Span<'static>> {
    code_spans(text, &crate::syntax::lang(lang), p)
        .into_iter()
        .map(RichSpan::into_span)
        .collect()
}

/// One line of a file as spans, for a diff of that file to draw: code
/// where the scanner knows the language, markdown where the file is
/// markdown — a heading reads as a heading and `inline code` as code,
/// which is the whole point of the file being markdown — and the text
/// as it stands otherwise.
pub fn file_line(text: &str, lang: &str, p: &Palette) -> Vec<Span<'static>> {
    match lang {
        "md" | "markdown" => {
            // The marker stays on the line: a diff shows what the file
            // says, and `## Coverage` is what that line says.
            let base = match heading(text.trim_start()) {
                Some((1..=2, _)) => Style::default().fg(p.heading).add_modifier(Modifier::BOLD),
                Some(_) => Style::default().add_modifier(Modifier::BOLD),
                None => Style::default(),
            };
            inline(text, base, p)
        }
        _ => code_line(text, lang, p),
    }
}

/// The slab's first line when the fence names a file.
fn block_title(name: &str, width: usize, p: &Palette) -> Option<RichLine<'static>> {
    (!name.is_empty()).then(|| {
        block_line(
            vec![RichSpan::styled(
                name.to_string(),
                Style::default().fg(p.code).add_modifier(Modifier::BOLD),
            )],
            width,
            p,
        )
    })
}

/// A fence's info string as `(name, language)`. A token that carries a
/// dot or a slash is a path; anything else is the language.
fn split_info(info: &str) -> (&str, &str) {
    let (mut name, mut lang) = ("", "");
    for tok in info.split([' ', ':', ',']).filter(|t| !t.is_empty()) {
        let path = tok.contains('/') || (tok.contains('.') && !tok.starts_with('.'));
        match path {
            true if name.is_empty() => name = tok,
            false if lang.is_empty() => lang = tok,
            _ => {}
        }
    }
    (name, lang)
}

/// Whether the last non-blank line before `i` was a list item: an
/// indented run under a list belongs to that list, not to code.
fn under_a_list(src: &[&str], i: usize) -> bool {
    src[..i]
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| {
            let ind = l.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            list_item(l.trim_start(), ind).is_some()
        })
}

/// `(fence char, run length)` if this line opens or closes a fence.
fn fence(t: &str) -> Option<(char, usize)> {
    let c = t.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let n = t.chars().take_while(|x| *x == c).count();
    (n >= 3).then_some((c, n))
}

fn is_rule(t: &str) -> bool {
    let c = match t.chars().next() {
        Some(c @ ('-' | '*' | '_')) => c,
        _ => return false,
    };
    t.chars().filter(|x| *x == c).count() >= 3 && t.chars().all(|x| x == c || x == ' ')
}

fn heading(t: &str) -> Option<(usize, &str)> {
    let n = t.chars().take_while(|c| *c == '#').count();
    if n == 0 || n > 6 {
        return None;
    }
    let rest = &t[n..];
    rest.starts_with(' ').then(|| (n, rest.trim_start()))
}

/// `(marker, content)` for a list item — the marker carries its trailing
/// space so its width is the hanging indent.
fn list_item(t: &str, depth: usize) -> Option<(String, &str)> {
    if let Some(rest) = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))
    {
        let glyph = ["• ", "◦ ", "▪ "][(depth / 2).min(2)];
        return Some((glyph.to_string(), rest.trim_start()));
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && digits <= 9 {
        let rest = &t[digits..];
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((format!("{}. ", &t[..digits]), r.trim_start()));
        }
    }
    None
}

// ------------------------------------------------------------------ tables

/// Rows of a pipe table starting at `i`, and the index just past it. A
/// table needs a separator row (`|---|:--:|`) directly under its header.
fn table_at(src: &[&str], i: usize) -> Option<(Vec<Vec<String>>, usize)> {
    if !src[i].contains('|') || !src.get(i + 1).is_some_and(|l| is_sep_row(l)) {
        return None;
    }
    let mut rows = vec![cells(src[i])];
    let mut j = i + 2;
    while j < src.len() && src[j].contains('|') && !is_sep_row(src[j]) {
        rows.push(cells(src[j]));
        j += 1;
    }
    Some((rows, j))
}

fn is_sep_row(l: &str) -> bool {
    let t = l.trim();
    t.contains('|') && !t.is_empty() && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn cells(l: &str) -> Vec<String> {
    // A wikilink's alias separator is not a table separator. Neither is
    // an escaped pipe or a pipe inside a code span.
    let line = l.trim().trim_start_matches('|').trim_end_matches('|');
    let ch: Vec<char> = line.chars().collect();
    let (mut cells, mut cell) = (Vec::new(), String::new());
    let (mut wiki, mut code, mut escaped) = (false, false, false);
    for (i, &c) in ch.iter().enumerate() {
        if escaped { cell.push(c); escaped = false; continue; }
        if c == '\\' { cell.push(c); escaped = true; continue; }
        if c == '`' { code = !code; }
        if !code && c == '[' && ch.get(i + 1) == Some(&'[') { wiki = true; }
        if !code && c == ']' && i > 0 && ch[i - 1] == ']' { wiki = false; }
        if c == '|' && !wiki && !code { cells.push(cell.trim().to_string()); cell.clear(); }
        else { cell.push(c); }
    }
    cells.push(cell.trim().to_string());
    cells
}

/// Lay the rows out in columns that fit `width`, header row in bold.
/// A cell too wide for its column **wraps**, and the row grows as tall
/// as its tallest cell: a table of `…`-cut cells says less than the same
/// table two rows taller, and the interesting half of a value is rarely
/// the first twelve characters.
fn table(rows: &[Vec<String>], width: usize, p: &Palette) -> Vec<RichLine<'static>> {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }
    let mut w: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| columns(&plain(s)))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    // Shrink the widest column until the row fits. Three cells is the
    // floor; below that a column is noise, and wrapping does the rest.
    // The frame costs a bar either side of every cell and a space of
    // padding inside each: `\u{2502} c \u{2502} c \u{2502}`.
    let gutter = 3 * cols + 1;
    while w.iter().sum::<usize>() + gutter > width {
        let Some(m) = (0..cols).max_by_key(|&c| w[c]) else {
            break;
        };
        if w[m] <= 3 {
            break;
        }
        w[m] -= 1;
    }
    // A column that holds nothing but quantities is right-aligned, so
    // its digits line up and a column of numbers can be read down.
    let right: Vec<bool> = (0..cols)
        .map(|c| {
            let body = || rows.iter().skip(1).filter_map(|r| r.get(c));
            body().any(|s| !s.trim().is_empty())
                && body().all(|s| s.trim().is_empty() || numeric(s))
                // A wrapped cell cannot be right-aligned without reading
                // as two numbers, so a column that wraps stays left.
                && rows.iter().filter_map(|r| r.get(c)).all(|s| columns(&plain(s)) <= w[c])
        })
        .collect();
    // A table whose cells wrap needs a line between its rows: three
    // stacked lines with nothing between them read as one row of three.
    // A table where every cell fits does not, and is quieter without.
    let tall = rows
        .iter()
        .any(|r| (0..cols).any(|c| r.get(c).map_or(0, |s| columns(&plain(s))) > w[c]));
    let mut out = Vec::new();
    for (n, row) in rows.iter().enumerate() {
        let style = if n == 0 {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let cells: Vec<Vec<RichLine<'static>>> = (0..cols)
            .map(|c| {
                let text = row.get(c).map(String::as_str).unwrap_or("");
                match text.trim().is_empty() {
                    true => vec![RichLine::default()],
                    false => rich_wrap(rich_inline(text, style, p), w[c], &[], &[]),
                }
            })
            .collect();
        // The row this line came from, counted off the header: the
        // separator is row 1 of the source and drawn as the header rule,
        // so a body row is its index plus one.
        let src_row = if n == 0 { 0 } else { n + 1 };
        if tall && n > 1 {
            let mut line = RichLine::styled(rule(&w, '\u{251c}', '\u{253c}', '\u{2524}'), p.dim());
            line.source = src_row;
            out.push(line);
        }
        let height = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
        for y in 0..height {
            let mut spans = Vec::new();
            for (c, cw) in w.iter().enumerate() {
                spans.push(RichSpan::styled(
                    if c == 0 { "\u{2502} " } else { " \u{2502} " },
                    p.dim(),
                ));
                let cell = cells[c].get(y).map(|l| l.spans.clone()).unwrap_or_default();
                spans.extend(pad_spans(cell, *cw, style, right[c]));
            }
            spans.push(RichSpan::styled(" \u{2502}", p.dim()));
            let mut line = RichLine::from(spans);
            line.source = src_row;
            out.push(line);
        }
        if n == 0 {
            let mut line = RichLine::styled(rule(&w, '\u{251c}', '\u{253c}', '\u{2524}'), p.dim());
            line.source = 1;
            out.push(line);
        }
    }
    let last = rows.len();
    out.insert(0, RichLine::styled(rule(&w, '\u{250c}', '\u{252c}', '\u{2510}'), p.dim()));
    let mut foot = RichLine::styled(rule(&w, '\u{2514}', '\u{2534}', '\u{2518}'), p.dim());
    foot.source = last;
    out.push(foot);
    out
}

/// One of the table's three horizontal rules, with the corners it wears
/// at the top, the middle and the foot.
fn rule(w: &[usize], left: char, mid: char, right: char) -> String {
    let mut s = String::from(left);
    for (c, n) in w.iter().enumerate() {
        if c > 0 {
            s.push(mid);
        }
        s.extend(std::iter::repeat_n('\u{2500}', n + 2));
    }
    s.push(right);
    s
}

/// A cell that reads as a quantity — `1,234`, `0.0006`, `71%`, `$1.31`,
/// `-3`. It has to parse as one number once its currency and percent are
/// taken off, which is what keeps an address or a version out: nothing
/// is gained by right-aligning `127.0.0.1:8000`.
fn numeric(s: &str) -> bool {
    s.trim()
        .trim_start_matches(['$', '+'])
        .trim_end_matches(['%', 'x'])
        .replace(',', "")
        .parse::<f64>()
        .is_ok()
}

fn pad_spans(spans: Vec<RichSpan<'static>>, width: usize, style: Style, right: bool) -> Vec<RichSpan<'static>> {
    let total: usize = spans.iter().map(|s| columns(&s.content)).sum();
    let room = if total > width { width.saturating_sub(1) } else { width };
    let (mut used, mut out) = (0, Vec::new());
    for span in spans {
        let cut = prefix(&span.content, room.saturating_sub(used));
        used += columns(&span.content[..cut]);
        out.push(span.fragment(span.content[..cut].to_string()));
        if cut < span.content.len() { break; }
    }
    if total > width { out.push(RichSpan::styled("…", style)); used += 1; }
    if used < width {
        let pad = RichSpan::raw(" ".repeat(width - used));
        match right {
            true => out.insert(0, pad),
            false => out.push(pad),
        }
    }
    out
}

fn columns(s: &str) -> usize { Span::raw(s).width() }

/// The byte boundary of the longest grapheme prefix fitting these cells.
fn prefix(s: &str, width: usize) -> usize {
    let (mut used, mut end) = (0, 0);
    for (at, g) in s.grapheme_indices(true) {
        let w = columns(g);
        if used + w > width { break; }
        used += w;
        end = at + g.len();
    }
    end
}

// ------------------------------------------------------------------ inline

/// Scan inline markup into styled spans over `base`. Unclosed markup is
/// left as literal text, which is what a half-streamed line needs.
fn rich_inline(s: &str, base: Style, p: &Palette) -> Vec<RichSpan<'static>> {
    let ch: Vec<char> = s.chars().collect();
    let mut out: Vec<RichSpan<'static>> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    let flush = |buf: &mut String, out: &mut Vec<RichSpan<'static>>| {
        if !buf.is_empty() {
            out.push(RichSpan::styled(std::mem::take(buf), base));
        }
    };
    while i < ch.len() {
        let c = ch[i];
        match c {
            '\\' if i + 1 < ch.len() => {
                buf.push(ch[i + 1]);
                i += 2;
            }
            '`' => {
                let n = run(&ch, i, '`');
                match find(&ch, i + n, '`', n) {
                    Some(end) => {
                        flush(&mut buf, &mut out);
                        let body: String = ch[i + n..end].iter().collect();
                        out.push(RichSpan::styled(body.trim().to_string(), base.fg(p.code)));
                        i = end + n;
                    }
                    None => {
                        buf.push(c);
                        i += 1;
                    }
                }
            }
            '*' | '_' | '~' => {
                let n = run(&ch, i, c);
                // `_` only delimits at a word boundary, so snake_case survives.
                let boundary = c != '_' || i == 0 || !ch[i - 1].is_alphanumeric();
                let close = if boundary {
                    find(&ch, i + n, c, n)
                } else {
                    None
                };
                match close.filter(|e| *e > i + n) {
                    Some(end) if !(c == '~' && n < 2) => {
                        flush(&mut buf, &mut out);
                        let style = match (c, n) {
                            ('~', _) => base.add_modifier(Modifier::CROSSED_OUT),
                            (_, 2) => base.add_modifier(Modifier::BOLD),
                            _ => base.add_modifier(Modifier::ITALIC),
                        };
                        let body: String = ch[i + n..end].iter().collect();
                        out.extend(rich_inline(&body, style, p));
                        i = end + n;
                    }
                    _ => {
                        buf.push(c);
                        i += 1;
                    }
                }
            }
            '[' if ch.get(i + 1) == Some(&'[') => {
                if let Some((target, label, end)) = wikilink(&ch, i) {
                    flush(&mut buf, &mut out);
                    let mut span = RichSpan::styled(label, base.fg(p.link).add_modifier(Modifier::UNDERLINED));
                    span.link = Some(Rc::new(Target::Vault(target)));
                    out.push(span);
                    i = end;
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
            '[' => match link(&ch, i) {
                Some((label, target, end)) => {
                    flush(&mut buf, &mut out);
                    let mut spans = rich_inline(
                        &label,
                        base.fg(p.link).add_modifier(Modifier::UNDERLINED),
                        p,
                    );
                    let target = Rc::new(Target::Url(target));
                    for span in &mut spans { span.link = Some(target.clone()); }
                    out.extend(spans);
                    i = end;
                }
                None => {
                    buf.push(c);
                    i += 1;
                }
            },
            _ => {
                buf.push(c);
                i += 1;
            }
        }
    }
    flush(&mut buf, &mut out);
    out
}

/// Inline markup stripped to its text, for places that must count columns
/// before they style (table cells).
pub fn plain(s: &str) -> String {
    rich_inline(s, Style::default(), &Palette::default())
        .iter()
        .map(|s| s.content.as_ref())
        .collect()
}

fn run(ch: &[char], at: usize, c: char) -> usize {
    ch[at..].iter().take_while(|x| **x == c).count().min(2)
}

/// The next run of exactly `n` `c`s at or after `from`.
fn find(ch: &[char], from: usize, c: char, n: usize) -> Option<usize> {
    let mut i = from;
    while i < ch.len() {
        if ch[i] == '\\' {
            i += 2;
            continue;
        }
        if ch[i] == c {
            let len = ch[i..].iter().take_while(|x| **x == c).count();
            if len >= n {
                return Some(i);
            }
            i += len;
            continue;
        }
        i += 1;
    }
    None
}

/// `[label](url)` → the label and the index just past the link.
fn link(ch: &[char], i: usize) -> Option<(String, String, usize)> {
    let (mut at, mut brackets) = (i + 1, 1);
    while at < ch.len() {
        match ch[at] {
            '\\' => { at += 2; continue; }
            '`' => {
                let n = run(ch, at, '`');
                if let Some(end) = find(ch, at + n, '`', n) { at = end + n; continue; }
            }
            '[' => brackets += 1,
            ']' => { brackets -= 1; if brackets == 0 { break; } }
            _ => {}
        }
        at += 1;
    }
    if brackets != 0 { return None; }
    let close = at;
    if ch.get(close + 1) != Some(&'(') {
        return None;
    }
    let mut depth = 1;
    let mut j = close + 2;
    while j < ch.len() {
        match ch[j] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((ch[i + 1..close].iter().collect(), ch[close + 2..j].iter().collect(), j + 1));
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

// ----------------------------------------------------------------- wrapping

/// Word-wrap styled spans to `width`, prefixing the first line with
/// `first` and every continuation with `cont`.
fn rich_wrap(
    spans: Vec<RichSpan<'static>>,
    width: usize,
    first: &[RichSpan<'static>],
    cont: &[RichSpan<'static>],
) -> Vec<RichLine<'static>> {
    let width = width.max(4);
    let mut out: Vec<RichLine<'static>> = Vec::new();
    let mut line: Vec<RichSpan<'static>> = first.to_vec();
    let mut used: usize = first.iter().map(|s| columns(&s.content)).sum();
    let cont_w: usize = cont.iter().map(|s| columns(&s.content)).sum();
    let mut fresh = true;

    for span in spans {
        for (n, word) in span.content.split(' ').enumerate() {
            let space = n > 0;
            let wl = columns(word);
            if space && !fresh {
                if used + 1 + wl > width && used > cont_w {
                    out.push(RichLine::from(std::mem::replace(&mut line, cont.to_vec())));
                    used = cont_w;
                    fresh = true;
                } else {
                    line.push(span.fragment(" "));
                    used += 1;
                }
            }
            if word.is_empty() {
                continue;
            }
            if used + wl > width && !fresh {
                out.push(RichLine::from(std::mem::replace(&mut line, cont.to_vec())));
                used = cont_w;
                fresh = true;
            }
            // Still too long on a line of its own: hard-break it.
            let mut rest = word;
            while used + columns(rest) > width {
                let mut cut = prefix(rest, width.saturating_sub(used));
                if cut == 0 && used == 0 { cut = rest.graphemes(true).next().unwrap().len(); }
                // A hanging indent can leave less than one wide glyph's room.
                // Drop that indent on this row rather than loop or lose text.
                if cut == 0 && used == cont_w { line.clear(); used = 0; continue; }
                line.push(span.fragment(rest[..cut].to_string()));
                out.push(RichLine::from(std::mem::replace(&mut line, cont.to_vec())));
                used = cont_w;
                rest = &rest[cut..];
            }
            if !rest.is_empty() {
                line.push(span.fragment(rest.to_string()));
                used += columns(rest);
                fresh = false;
            }
        }
    }
    out.push(RichLine::from(line));
    out
}

/// Split a line into `width`-column chunks without word boundaries.
fn hard_chunks(s: &str, width: usize) -> Vec<String> {
    let width = width.max(2);
    if s.is_empty() { return vec![String::new()]; }
    let (mut rest, mut out) = (s, Vec::new());
    while !rest.is_empty() {
        let cut = prefix(rest, width);
        // Terminal graphemes are at most two cells, but make progress even
        // if a future width table gives a cluster a larger width.
        let cut = if cut == 0 { rest.graphemes(true).next().unwrap().len() } else { cut };
        out.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_source_addresses_cover_wrapping_code_tables_and_heading_spacing() {
        let source = "intro\n# Heading\nlong words that will wrap over many rows\n```rs\nlet x = 1;\n```\n| H |\n|---|\n| cell |\n> quoted\n- listed";
        for width in [10, 80] {
            let doc = document(source, width, &Palette::default());
            assert_eq!(doc.lines.len(), doc.source_lines.len());
            assert!(doc.source_lines.iter().all(|n| (1..=11).contains(n)));
            // Neither fence line is drawn: the slab is the block's mark.
            assert!(!doc.source_lines.contains(&4), "opening fence is not rendered");
            assert!(!doc.source_lines.contains(&6), "closing fence is not rendered");
            for (line, source_line) in doc.lines.iter().zip(&doc.source_lines) {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                if text.contains("Heading") { assert_eq!(*source_line, 2); }
                if text.contains("let") { assert_eq!(*source_line, 5); }
                if text.contains("cell") { assert_eq!(*source_line, 9); }
                if text.contains("quoted") { assert_eq!(*source_line, 10); }
                if text.contains("listed") { assert_eq!(*source_line, 11); }
            }
            let mut unique = doc.source_lines.clone();
            unique.dedup();
            assert_eq!(unique, vec![1, 2, 3, 5, 7, 8, 9, 10, 11]);
        }
    }

    #[test]
    fn wikilinks_keep_occurrences_aliases_and_wrapped_byte_runs() {
        let doc = document("- [[Trackers/Eidolon Bugs Tracker|a long tracker label]] then [[other|same]] [[third|same]]", 16, &Palette::default());
        assert_eq!(doc.links.len(), 3);
        assert_eq!(doc.links[0].target, Target::Vault("Trackers/Eidolon Bugs Tracker".into()));
        assert!(doc.links[0].runs.iter().map(|(r, _)| *r).max().unwrap() > 0);
        assert_ne!(doc.links[1].target, doc.links[2].target);
        for link in &doc.links {
            for (row, range) in &link.runs {
                let text: String = doc.lines[*row].spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(!text[range.clone()].contains('['));
            }
        }
    }

    #[test]
    fn code_escapes_and_incomplete_links_are_not_destinations() {
        let doc = document("`[[inline]]` \\[\\[escaped]] [[unfinished\n```md\n[[fenced]]\n```\n[[real]]", 80, &Palette::default());
        assert_eq!(doc.links.len(), 1);
        assert_eq!(doc.links[0].target, Target::Vault("real".into()));
        assert!(text(&doc.lines).join("\n").contains("[[unfinished"));
    }

    #[test]
    fn ordinary_markdown_retains_its_url_and_code_inside_a_label_is_not_a_vault_link() {
        let doc = document("[a **label**](https://example.com/a(b)) [`[[literal]]`](https://x)", 80, &Palette::default());
        assert_eq!(doc.links.len(), 2);
        assert_eq!(doc.links[0].target, Target::Url("https://example.com/a(b)".into()));
        assert!(doc.links.iter().all(|l| matches!(l.target, Target::Url(_))));
    }

    #[test]
    fn tables_keep_alias_pipes_inside_cells_and_keep_links_through_wrapping() {
        let doc = document("| Note | Why |\n|---|---|\n| [[a|a very long label]] | `code|pipe` |", 25, &Palette::default());
        assert_eq!(doc.links.len(), 1);
        assert_eq!(doc.links[0].target, Target::Vault("a".into()));
        assert!(doc.lines.iter().all(|l| l.width() <= 25));
        // The label wraps down the column rather than losing its tail.
        assert!(text(&doc.lines).join("\n").contains("label"));
    }

    #[test]
    fn wide_and_combining_labels_wrap_on_cells_without_splitting_graphemes() {
        let doc = document("界 [[note|界界界界界界ééé]]", 8, &Palette::default());
        assert_eq!(doc.links.len(), 1);
        assert!(doc.lines.iter().all(|l| l.width() <= 8));
        let pieces: String = doc.links[0].runs.iter().map(|(r, range)| {
            let s: String = doc.lines[*r].spans.iter().map(|s| s.content.as_ref()).collect();
            s[range.clone()].to_string()
        }).collect();
        assert_eq!(pieces, "界界界界界界ééé");
    }

    #[test]
    fn anchors_are_headings_and_explicit_block_ids_but_never_code() {
        let doc = document("Heading\n# **Heading**\n- text ^block-id\n```\n# Fake\ntext ^fake\n```", 20, &Palette::default());
        assert_eq!(doc.anchors.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>(), vec!["Heading", "^block-id"]);
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// A payload fence is a slab naming its hole, bytes inside, and no
    /// fence lines anywhere: the protocol is the one thing not drawn.
    #[test]
    fn a_payload_fence_draws_as_a_boxed_slab_and_hides_its_syntax() {
        let out = render(
            "before\n!!a\nthe exact bytes\nwith a \"quote\" in them\n!!\nafter\n",
            40,
            &Palette::default(),
        );
        let t = text(&out);
        assert!(t.contains(&"before".to_string()), "{t:?}");
        assert!(t.contains(&"after".to_string()), "{t:?}");
        assert!(
            !t.iter().any(|l| l.contains("!!")),
            "no fence line survives: {t:?}"
        );
        // The hole letter is named on the slab, the bytes verbatim under it.
        assert!(t.iter().any(|l| l.contains('a') && l.contains('\u{258e}')), "{t:?}");
        assert!(t.iter().any(|l| l.contains("the exact bytes")), "{t:?}");
        assert!(t.iter().any(|l| l.contains("\"quote\"")), "{t:?}");
        // Every row of the slab carries the block ground, so it reads as one box.
        assert!(
            out.iter()
                .filter(|l| l.spans.iter().any(|s| s.content.contains("the exact bytes")))
                .all(|l| l.spans.iter().any(|s| s.style.bg.is_some())),
            "the slab is one ground: {t:?}"
        );
        // Provenance: the first byte row stands for the first payload line.
        let bytes_row = out
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("the exact bytes")))
            .unwrap();
        assert!(bytes_row.spans.iter().any(|s| s.content.contains("the exact bytes")));
    }

    /// A `!!`-shaped line the channel would not bind — a wider opener, a
    /// closer with no block open, two letters — is prose, exactly as the
    /// channel treats it.
    #[test]
    fn an_unbindable_fence_shape_stays_prose() {
        for prose in ["!!ab\nbytes\n!!\n", "!!\nbytes\n!!\n", "bytes\n!!\n"] {
            let out = render(prose, 40, &Palette::default());
            let t = text(&out);
            assert!(
                t.iter().any(|l| l.contains("!!")),
                "{prose} must stay prose: {t:?}"
            );
        }
    }

    /// An unterminated block (streaming, the closer not yet written) draws
    /// what has arrived, in the slab.
    #[test]
    fn an_open_payload_draws_to_the_end_of_what_has_arrived() {
        let out = render("!!a\npartial bytes", 40, &Palette::default());
        let t = text(&out);
        assert!(t.iter().any(|l| l.contains("partial bytes")), "{t:?}");
    }

    /// A fence inside a code block is bytes, and the code branch had it
    /// first: no slab, the lines verbatim in the fence.
    #[test]
    fn a_fence_inside_a_code_block_is_bytes() {
        let out = render(
            "```\n!!a\nbytes\n!!\n```\n",
            40,
            &Palette::default(),
        );
        let t = text(&out);
        assert!(t.iter().any(|l| l.contains("!!a")), "{t:?}");
        assert!(!t.iter().any(|l| l.contains('\u{258e}')), "{t:?}");
    }

    #[test]
    fn headings_lists_and_emphasis() {
        let out = render(
            "## Key Takeaways\n\n- **Commands**: run `cargo build`\n- a [link](http://x) here\n",
            40,
            &Palette::default(),
        );
        let t = text(&out);
        assert!(t.contains(&"Key Takeaways".to_string()));
        assert!(t.contains(&"• Commands: run cargo build".to_string()));
        assert!(t.contains(&"• a link here".to_string()));
        // The bold marker became a modifier, not text.
        assert!(
            out.iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.content == "Commands" && s.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn ordered_items_hang_under_their_number() {
        let out = text(&render(
            "1. alpha beta gamma delta epsilon zeta",
            20,
            &Palette::default(),
        ));
        assert_eq!(out[0], "1. alpha beta gamma");
        assert_eq!(out[1], "   delta epsilon");
    }

    #[test]
    fn nesting_follows_the_source_indent() {
        let out = text(&render(
            "1. one\n   - nested\n     - deeper",
            40,
            &Palette::default(),
        ));
        assert_eq!(out, vec!["1. one", "   ◦ nested", "     ▪ deeper"]);
    }

    #[test]
    fn snake_case_and_unclosed_markup_stay_literal() {
        let out = text(&render(
            "call foo_bar_baz and *this",
            60,
            &Palette::default(),
        ));
        assert_eq!(out[0], "call foo_bar_baz and *this");
    }

    #[test]
    fn fenced_code_keeps_its_lines_and_survives_an_open_fence() {
        let out = text(&render(
            "```rust\nfn a() {}\nfn b() {}",
            40,
            &Palette::default(),
        ));
        // The language is not said: a slab needs no label, and an
        // unnamed fence opens straight on its first line of code.
        let trimmed: Vec<&str> = out.iter().map(|l| l.trim_end()).collect();
        assert_eq!(trimmed, [" 1 fn a() {}", " 2 fn b() {}"]);
        assert!(out.iter().all(|l| l.chars().count() == 40), "the slab is ragged: {out:?}");
    }

    #[test]
    fn a_fence_that_names_a_file_opens_on_the_name_alone() {
        let at = |info: &str| {
            text(&render(&format!("```{info}\nx\n```"), 40, &Palette::default()))[0]
                .trim_end()
                .to_string()
        };
        assert_eq!(at("rust:src/main.rs"), " src/main.rs");
        assert_eq!(at("toml ~/.config/x.toml"), " ~/.config/x.toml");
        assert_eq!(at("crates/tui/src/lib.rs"), " crates/tui/src/lib.rs");
        // A language alone names no file, so there is no title line —
        // and a one-line block is not worth a gutter saying `1`.
        assert_eq!(at("rust"), " x");
    }

    /// The scanner names a run and the palette paints it, so a scheme
    /// is a table of roles: swap the table and the code changes colour
    /// without the scanner knowing.
    /// The gutter counts the block's own lines, is as wide as its
    /// largest number, and says a wrapped line's number once.
    #[test]
    fn a_block_of_many_lines_is_numbered_down_its_side() {
        let src = format!("```txt\n{}\n```", (1..=11).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n"));
        let out = text(&render(&src, 30, &Palette::default()));
        assert_eq!(out[0].trim_end(), "  1 line 1");
        assert_eq!(out[10].trim_end(), " 11 line 11");
        let lone = text(&render("```txt\nshort\n```", 12, &Palette::default()));
        assert_eq!(lone[0].trim_end(), " short", "a lone line takes no gutter");
    }

    #[test]
    fn code_is_painted_by_role_from_the_palette() {
        let p = Palette {
            code: Color::Rgb(9, 9, 9),
            syntax: [Color::Rgb(1, 1, 1), Color::Rgb(2, 2, 2), Color::Rgb(3, 3, 3), Color::Rgb(4, 4, 4)],
            ..Palette::default()
        };
        let lines = render("```rust\nlet x = \"hi\"; // note\n```", 60, &p);
        let painted: Vec<(String, Option<Color>)> = lines[0]
            .spans
            .iter()
            .map(|s| (s.content.to_string(), s.style.fg))
            .collect();
        let of = |needle: &str| painted.iter().find(|(c, _)| c.contains(needle)).and_then(|(_, f)| *f);
        assert_eq!(of("let"), Some(Color::Rgb(4, 4, 4)), "keyword: {painted:?}");
        assert_eq!(of("\"hi\""), Some(Color::Rgb(2, 2, 2)), "string: {painted:?}");
        assert_eq!(of("// note"), Some(Color::Rgb(1, 1, 1)), "comment: {painted:?}");
        assert_eq!(of(" x = "), Some(Color::Rgb(9, 9, 9)), "the rest is ordinary code: {painted:?}");
    }

    #[test]
    fn every_line_of_a_block_carries_the_background() {
        let p = Palette { block: Style::default().bg(Color::Rgb(1, 2, 3)), ..Palette::default() };
        let lines = render("```rs:a.rs\nlet x = 1;\n```\nafter", 30, &p);
        let bg = |l: &Line<'static>| l.spans.iter().all(|s| s.style.bg == Some(Color::Rgb(1, 2, 3)));
        assert!(bg(&lines[0]) && bg(&lines[1]), "the slab is not tinted throughout");
        assert!(!bg(&lines[2]), "the tint outlived the block");
    }

    #[test]
    fn a_pipe_table_becomes_columns() {
        let out = text(&render(
            "| a | bb |\n|---|----|\n| 1 | 2 |",
            40,
            &Palette::default(),
        ));
        assert_eq!(out[0], "┌───┬────┐");
        assert_eq!(out[1], "│ a │ bb │");
        assert_eq!(out[2], "├───┼────┤");
        // A column of quantities is right-aligned; `bb` is two wide.
        assert_eq!(out[3], "│ 1 │  2 │");
        assert_eq!(out[4], "└───┴────┘");
    }

    #[test]
    fn a_cell_too_wide_for_its_column_wraps_instead_of_being_cut() {
        let out = text(&render(
            "| a | bb |\n|---|----|\n| 1 | one two three four five |",
            16,
            &Palette::default(),
        ));
        let body = out[2..].join("\n");
        assert!(!body.contains('…'), "a cell was cut: {out:?}");
        assert!(body.contains("five"), "the tail of the cell was lost: {out:?}");
        assert!(out.iter().all(|l| l.chars().count() <= 16), "{out:?}");
    }

    #[test]
    fn an_indented_block_is_code_and_keeps_its_shape() {
        let art = "a diagram:\n\n    one ────── two\n     │         │\n    three ──── four\n\nafter";
        let out = text(&render(art, 40, &Palette::default()));
        let trimmed: Vec<&str> = out.iter().map(|l| l.trim_end()).collect();
        assert!(trimmed.contains(&" one ────── two"), "{out:?}");
        assert!(trimmed.contains(&"  │         │"), "the shape was folded: {out:?}");
        assert!(out.iter().any(|l| l == "after"), "the block did not end: {out:?}");
    }

    #[test]
    fn an_indented_line_under_a_list_stays_the_list_s() {
        let p = Palette { block: Style::default().bg(Color::Rgb(1, 2, 3)), ..Palette::default() };
        let lines = render("- a point\n\n    its continuation", 40, &p);
        assert!(
            lines.iter().all(|l| l.spans.iter().all(|s| s.style.bg.is_none())),
            "a list continuation became a slab: {:?}",
            text(&lines)
        );
    }

    #[test]
    fn long_words_hard_break() {
        let out = text(&render(&"x".repeat(25), 10, &Palette::default()));
        assert_eq!(out, vec!["xxxxxxxxxx", "xxxxxxxxxx", "xxxxx"]);
    }
}
