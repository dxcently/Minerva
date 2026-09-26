//! Operator-only vault browsing. A click reads on the async driver, never in a
//! frame or a model turn. The browser is a view, not context and not a journal.
//! Request generations stop an old reply replacing a newer note or a closed page.

use eidolon_core::persona::PersonaSource;
use ratatui::layout::Rect;
use tokio::sync::mpsc;

use crate::app::{Cmd, UiMsg};
use crate::markdown::{self, Document, Target};
use crate::state::{DialogKind, Entry, UiState};

#[derive(Clone, Debug)]
pub struct Note {
    pub path: String,
    pub body: String,
    pub anchor: Option<String>,
    pub scroll: usize,
    pub focus: Option<usize>,
}

/// One clickable run in the last painted frame. Wrapped labels have several.
#[derive(Clone, Debug)]
pub struct Hit {
    pub rect: Rect,
    pub target: String,
}

/// A document in the reading area, not a modal question or text field.
pub struct Page {
    pub prompt: String,
    pub body: String,
    pub selected: usize,
}

/// A local record document shares the reader's cursor/search, not vault identities.
pub struct Tree {
    pub body: String,
    pub records: std::collections::BTreeMap<usize, eidolon_core::Record>,
}

struct TreeReturn {
    page: Page,
    trace: crate::trace::Trace,
    search: Option<crate::search::Search>,
    record: u64,
}

#[derive(Default)]
pub struct Browser {
    pub active: bool,
    pub tree: Option<Tree>,
    tree_return: Option<TreeReturn>,
    pub page: Option<Page>,
    saved_search: Option<crate::search::Search>,
    pub current: Option<Note>,
    pub history: Vec<Note>,
    pub trace: crate::trace::Trace,
    pub pending: Option<u64>,
    generation: u64,
    pub trace_focus: Option<(usize, usize)>,
    pub hits: Vec<Hit>,
}

impl Browser {
    fn close(&mut self) {
        self.active = false;
        self.tree = None;
        self.tree_return = None;
        self.page = None;
        self.pending = None;
        self.current = None;
        self.history.clear();
        self.hits.clear();
    }
}

/// Resolve exact vault paths first, then a unique path suffix/title. No fuzzy
/// guessing: two folders containing the same title must be disambiguated by the
/// operator. The listing is metadata only; exactly one body is read.
pub async fn read(source: &dyn PersonaSource, target: &str, from: Option<&str>) -> anyhow::Result<Note> {
    let paths = source.list("").await?;
    let stem = |s: &str| s.strip_suffix(".md").unwrap_or(s).to_string();
    let unique = |name: &str| -> anyhow::Result<Option<String>> {
        let wanted = stem(name);
        let mut found: Vec<String> = paths.iter().filter(|p| {
            let p = stem(p);
            p == wanted || p.ends_with(&format!("/{wanted}"))
        }).cloned().collect();
        found.sort();
        found.dedup();
        if found.iter().any(|p| stem(p) == wanted) {
            found.retain(|p| stem(p) == wanted);
        }
        match found.len() {
            0 => Ok(None),
            1 => Ok(Some(found.remove(0))),
            _ => anyhow::bail!("ambiguous vault link '{name}': {}", found.join(", ")),
        }
    };
    // A `#` may separate a heading/block anchor — or be part of the note's own
    // title ("Aoide #3 reply"), which a split-first reading would cut to its
    // first word. The literal title resolves against the listing first; the
    // anchor reading is the fallback, so it only fires when no note carries
    // the whole name.
    if let Some(path) = unique(target.trim())? {
        let body = source.read(&path).await?;
        return Ok(Note { path, body, anchor: None, scroll: 0, focus: None });
    }
    let (name, anchor) = target.split_once('#').map_or((target, None), |(n, a)| (n, Some(a)));
    let name = if name.is_empty() { from.unwrap_or("") } else { name }.trim();
    anyhow::ensure!(!name.is_empty(), "a note name is required for this link");
    anyhow::ensure!(!name.starts_with('/') && !name.contains(':') && !name.split('/').any(|p| p == ".."),
        "not a vault-relative note: {name}");
    let path = unique(name)?.ok_or_else(|| anyhow::anyhow!("no vault note named '{name}'"))?;
    let body = source.read(&path).await?;
    Ok(Note { path, body, anchor: anchor.filter(|a| !a.is_empty()).map(str::to_string), scroll: 0, focus: None })
}

pub fn close(state: &mut UiState) {
    if state.vault.active {
        state.search = state.vault.saved_search.take();
        state.jump = None;
        if state.searching() { state.mini = None; }
    }
    state.vault.close();
}

pub fn layout(state: &UiState) -> (usize, usize) {
    (state.transcript_rect.width.saturating_sub(2).max(1) as usize,
        state.transcript_rect.height.saturating_sub(2).max(1) as usize)
}

pub fn document(state: &UiState) -> Document {
    document_at(state, layout(state).0)
}

pub fn document_at(state: &UiState, width: usize) -> Document {
    let body = state.vault.page.as_ref().map_or("", |d| d.body.as_str());
    if state.vault.tree.is_some() {
        // Plain record text: prose containing Markdown or wikilinks cannot
        // change row identity or turn a record into a vault read.
        let mut doc = Document { lines: vec![], source_lines: vec![], links: vec![], anchors: vec![] };
        for (i, line) in body.lines().enumerate() {
            let rows = crate::render::styled_page_lines(line, width, &state.theme, true);
            doc.source_lines.extend(std::iter::repeat_n(i + 1, rows.len()));
            doc.lines.extend(rows);
        }
        return doc;
    }
    markdown::document(body, width, &markdown::Palette::from(&state.theme))
}

pub fn open_tree(state: &mut UiState, tree: Tree) {
    if state.dialog.as_ref().is_some_and(|d| d.kind != DialogKind::Page) { return; }
    close(state);
    state.vault.saved_search = state.search.take();
    state.vault.active = true;
    state.dialog = None;
    state.jump = None;
    if state.searching() { state.mini = None; }
    state.vault.page = Some(Page { prompt: "tree".into(), body: tree.body.clone(), selected: 0 });
    state.vault.trace = Default::default();
    state.vault.tree = Some(tree);
}

pub fn tree_record(state: &UiState) -> Option<&eidolon_core::Record> {
    let tree = state.vault.tree.as_ref()?;
    if let Some(back) = &state.vault.tree_return {
        return tree.records.values().find(|r| r.id == back.record);
    }
    tree.records.get(&state.vault.trace.at)
}

fn tree_follow(state: &mut UiState) {
    if state.tracing() { trace_sync(state); }
    let Some(record) = tree_record(state) else { state.info("select a record with K, or tab to one"); return; };
    if state.vault.tree_return.is_some() { return; }
    let id = record.id;
    let body = match serde_json::to_string_pretty(record) {
        Ok(body) => format!("── Record #{id} ──\n\n{body}"),
        Err(e) => { state.info(format!("cannot display record: {e}")); return; }
    };
    let Some(page) = state.vault.page.take() else { return; };
    state.vault.tree_return = Some(TreeReturn { page, trace: state.vault.trace, search: state.search.take(), record: id });
    state.vault.page = Some(Page { prompt: format!("tree · record #{id}"), body, selected: 0 });
    state.vault.trace = Default::default();
    state.jump = None;
}

pub fn request(state: &mut UiState, tx: &mpsc::UnboundedSender<Cmd>, target: String) {
    // A model's approval/question owns the screen. Never replace it with a reader.
    if state.dialog.as_ref().is_some_and(|d| d.kind != DialogKind::Page) { return; }
    if state.vault.tree.is_some() { close(state); }
    let from = state.vault.current.as_ref().map(|n| n.path.clone());
    if !state.vault.active {
        state.dialog = None;
        state.vault.page = Some(Page { prompt: "vault".into(), body: format!("Reading {target}…"), selected: 0 });
        state.vault.saved_search = state.search.take();
        if state.searching() { state.mini = None; }
        state.jump = None;
        state.vault.active = true;
    }
    state.vault.generation += 1;
    let id = state.vault.generation;
    state.vault.pending = Some(id);
    if tx.send(Cmd::VaultRead { id, target, from }).is_err() {
        accept(state, id, Err("the vault driver is unavailable".into()));
    }
}

pub fn accept(state: &mut UiState, id: u64, result: Result<Note, String>) {
    if !state.vault.active || state.vault.pending != Some(id) { return; }
    state.vault.pending = None;
    match result {
        Ok(note) => {
            if let Some(mut old) = state.vault.current.take() {
                old.scroll = state.vault.page.as_ref().map_or(0, |d| d.selected);
                state.vault.history.push(old);
            }
            show(state, note);
        }
        Err(e) => {
            if state.vault.current.is_none() && let Some(d) = &mut state.vault.page {
                d.body = format!("Could not open note:\n\n{e}");
            }
            state.info(e);
        }
    }
}

fn show(state: &mut UiState, mut note: Note) {
    state.search = None;
    state.jump = None;
    state.vault.trace = Default::default();
    if state.searching() { state.mini = None; }
    if let Some(d) = &mut state.vault.page {
        d.prompt = format!("vault · {}", note.path);
        d.body = note.body.clone();
        d.selected = note.scroll;
    }
    if let Some(anchor) = note.anchor.take() {
        let doc = document(state);
        // Headings and Obsidian block IDs are matched literally, never inferred
        // from surrounding prose ("item 11" after a citation is not an anchor).
        let found = doc.anchors.iter().find(|(name, _)| name == &anchor).map(|(_, row)| *row);
        if let Some(row) = found {
            let (_, rows) = layout(state);
            if let Some(d) = &mut state.vault.page { d.selected = row.min(doc.lines.len().saturating_sub(rows)); }
        } else { state.info(format!("opened the note; anchor '{anchor}' was not found")); }
    }
    state.vault.current = Some(note);
}

pub fn back(state: &mut UiState) {
    if !state.vault.active { return; }
    state.vault.pending = None;
    if let Some(back) = state.vault.tree_return.take() {
        state.vault.page = Some(back.page);
        state.vault.trace = back.trace;
        state.search = back.search;
        state.jump = None;
        return;
    }
    if let Some(note) = state.vault.history.pop() { show(state, note); }
    else {
        close(state);
    }
}

/// Reader navigation owns the reading area, never the message/minibuffer.
pub fn exec(state: &mut UiState, tx: &mpsc::UnboundedSender<Cmd>, name: &str, count: usize) -> bool {
    let resting = state.mode_name() == "vault";
    match name {
        "link_next" => select(state, false),
        "link_prev" => select(state, true),
        "link_open" => follow(state, tx),
        "link_back" => back(state),
        "link_close" => close(state),
        "trace_quote" if state.tracing() => quote(state),
        name if name.starts_with("trace_") && name != "trace_mode" && state.tracing() => {
            trace_exec(state, tx, name, count);
        }
        "cancel" if resting => {
            if state.search.is_some() { state.search = None; } else { close(state); }
        }
        "yank" if resting => {
            let text = state.vault.page.as_ref().map_or(String::new(), |d| d.body.clone());
            state.prompt.register = text.clone();
            state.copied_at = crate::clipboard::copy(&text).then(std::time::Instant::now);
        }
        _ => {
            let (_, rows) = layout(state);
            let last = document(state).lines.len().saturating_sub(rows);
            let Some(d) = &mut state.vault.page else { return false; };
            let amount = match name {
                "scroll_line_up" | "scroll_line_down" => count.max(1),
                "scroll_half_up" | "scroll_half_down" => {
                    (rows / 2).max(1).saturating_mul(count.max(1))
                }
                "scroll_page_up" | "scroll_page_down" => rows.saturating_mul(count.max(1)),
                "transcript_top" => { d.selected = 0; return true; }
                "transcript_bottom" => { d.selected = last; return true; }
                _ => return false,
            };
            d.selected = if name.ends_with("up") { d.selected.saturating_sub(amount) }
                else { d.selected.saturating_add(amount).min(last) };
        }
    }
    true
}

/// Trace positions in a document are original source lines, so reflow does not
/// change a selection or the address that will be staged in the draft.
pub fn trace_sync(state: &mut UiState) {
    if !state.tracing() { state.vault.trace.live = false; return; }
    if state.vault.trace.live { return; }
    let doc = document(state);
    let row = state.vault.page.as_ref().map_or(0, |d| d.selected);
    let line = if state.vault.tree.is_some() && state.vault.trace.at > 0 { state.vault.trace.at }
        else { doc.source_lines.get(row).copied().unwrap_or(1) };
    state.vault.trace = crate::trace::Trace { at: line, anchor: line, live: true, extend: false };
}

pub fn trace_selection(state: &UiState) -> std::ops::RangeInclusive<usize> {
    let t = &state.vault.trace;
    t.at.min(t.anchor)..=t.at.max(t.anchor)
}

pub fn trace_goto(state: &mut UiState, line: usize) {
    trace_sync(state);
    if state.vault.trace.at != line && let Some(n) = &mut state.vault.current { n.focus = None; }
    state.vault.trace.at = line;
    if !state.vault.trace.extend { state.vault.trace.anchor = line; }
    let doc = document(state);
    let row = doc.source_lines.iter().position(|n| *n == line).unwrap_or(0);
    let (_, rows) = layout(state);
    if let Some(d) = &mut state.vault.page && (row < d.selected || row >= d.selected + rows) {
        d.selected = row.min(doc.lines.len().saturating_sub(rows));
    }
}

pub fn trace_text(state: &UiState) -> String {
    let Some(page) = &state.vault.page else { return String::new(); };
    let range = trace_selection(state);
    page.body.split('\n').enumerate().filter(|(i, _)| range.contains(&(i + 1)))
        .map(|(_, s)| s).collect::<Vec<_>>().join("\n")
}

fn trace_exec(state: &mut UiState, tx: &mpsc::UnboundedSender<Cmd>, name: &str, count: usize) {
    trace_sync(state);
    let doc = document(state);
    let mut lines = doc.source_lines;
    lines.dedup();
    let pos = lines.iter().position(|n| *n == state.vault.trace.at).unwrap_or(0);
    let last = lines.len().saturating_sub(1);
    let next = match name {
        "trace_next" => Some(pos.saturating_add(count.max(1)).min(last)),
        "trace_prev" => Some(pos.saturating_sub(count.max(1))),
        "trace_first" => Some(0),
        "trace_last" => Some(last),
        "trace_extend" => {
            state.vault.trace.extend = !state.vault.trace.extend;
            if !state.vault.trace.extend { state.vault.trace.anchor = state.vault.trace.at; }
            None
        }
        "trace_yank" => {
            let text = trace_text(state);
            state.prompt.register = text.clone();
            state.copied_at = crate::clipboard::copy(&text).then(std::time::Instant::now);
            None
        }
        "trace_open" | "trace_toggle" => { follow(state, tx); None }
        "trace_close" => { back(state); None }
        _ => { state.info("this trace action does not apply to a read-only document"); None }
    };
    if let Some(line) = next.and_then(|i| lines.get(i)) { trace_goto(state, *line); }
}

/// Stage only an address. Source-line ranges are explicit prose beside a real
/// wikilink, not made-up Obsidian anchors; they refer to the version just read.
fn quote(state: &mut UiState) {
    trace_sync(state);
    let Some(note) = &state.vault.current else { state.info("no loaded note to reference"); return; };
    let range = trace_selection(state);
    let address = format!("[[{}]] (source lines {}–{})", note.path, range.start(), range.end());
    state.prompt.goto_buffer_end();
    state.prompt.enter_insert(true);
    if !state.prompt.text().is_empty() && !state.prompt.text().ends_with(char::is_whitespace) { state.prompt.insert(" "); }
    state.prompt.insert(&address);
    state.prompt.insert(" ");
    state.info("reference added to draft — source lines refer to the version read; return to the conversation to send");
}

fn trace_document(state: &UiState) -> Option<Document> {
    if !state.tracing() { return None; }
    let Entry::Assistant { text, .. } = state.transcript.get(state.trace.at)? else { return None; };
    Some(markdown::document(text, state.view_width.max(1), &markdown::Palette::from(&state.theme)))
}

/// Tab traverses occurrences, not destinations; two aliases to the same note
/// remain two places the operator can point at.
pub fn select(state: &mut UiState, backwards: bool) {
    if let Some(tree) = &state.vault.tree {
        if state.vault.tree_return.is_some() { return; }
        let lines: Vec<_> = tree.records.keys().copied().collect();
        let at = state.vault.trace.at;
        let next = if backwards { lines.iter().rev().find(|n| **n < at).or_else(|| lines.last()) }
            else { lines.iter().find(|n| **n > at).or_else(|| lines.first()) };
        if let Some(line) = next { trace_goto(state, *line); }
        return;
    }
    let (doc, focus) = if state.vault.active {
        (document(state), state.vault.current.as_ref().and_then(|n| n.focus))
    } else if state.dialog.is_none() {
        let Some(doc) = trace_document(state) else { state.info("links are navigable in trace mode"); return; };
        (doc, state.vault.trace_focus.filter(|(entry, _)| *entry == state.trace.at).map(|(_, i)| i))
    } else { return; };
    let ids: Vec<_> = doc.links.iter().enumerate().filter_map(|(i, l)| matches!(l.target, Target::Vault(_)).then_some(i)).collect();
    if ids.is_empty() { state.info("no vault links here"); return; }
    let pos = focus.and_then(|f| ids.iter().position(|i| *i == f));
    let next = match (pos, backwards) {
        (Some(p), false) => (p + 1) % ids.len(),
        (Some(p), true) => (p + ids.len() - 1) % ids.len(),
        (None, false) => 0,
        (None, true) => ids.len() - 1,
    };
    let id = ids[next];
    let row = doc.links[id].runs.first().map_or(0, |(r, _)| *r);
    if state.vault.active {
        if state.tracing() && let Some(line) = doc.source_lines.get(row) { trace_goto(state, *line); }
        if let Some(n) = &mut state.vault.current { n.focus = Some(id); }
        let (_, rows) = layout(state);
        if let Some(d) = &mut state.vault.page && (row < d.selected || row >= d.selected + rows) {
            d.selected = row.min(doc.lines.len().saturating_sub(rows));
        }
    } else {
        state.vault.trace_focus = Some((state.trace.at, id));
        let (below, _) = crate::render::block_extent(state, state.view_width.max(1), state.trace.at);
        state.set_scroll(below.saturating_sub(row + state.view_height.max(1)));
    }
    if let Target::Vault(target) = &doc.links[id].target { state.info(format!("{}/{} · {target} · enter to open", next + 1, ids.len())); }
}

pub fn follow(state: &mut UiState, tx: &mpsc::UnboundedSender<Cmd>) {
    if state.vault.tree.is_some() { tree_follow(state); return; }
    let (doc, focus) = if state.vault.active {
        (document(state), state.vault.current.as_ref().and_then(|n| n.focus))
    } else if state.dialog.is_none() {
        let Some(doc) = trace_document(state) else { return; };
        (doc, state.vault.trace_focus.filter(|(entry, _)| *entry == state.trace.at).map(|(_, i)| i))
    } else { return; };
    let link = focus.and_then(|i| doc.links.get(i)).or_else(|| doc.links.iter().find(|l| {
        matches!(l.target, Target::Vault(_)) && (!state.vault.active || !state.tracing()
            || l.runs.iter().any(|(r, _)| doc.source_lines.get(*r) == Some(&state.vault.trace.at)))
    }));
    if let Some(markdown::Link { target: Target::Vault(target), .. }) = link { request(state, tx, target.clone()); }
    else { state.info("no vault link here — use l/h to open or close folds"); }
}

/// The driver's only work here: a bounded read on its own task. No dispatcher
/// call is owed: activation is the operator asking to view a note, not a model
/// choosing a read. It deliberately publishes no bus events or records.
pub async fn fetch_message(source: Option<std::sync::Arc<dyn PersonaSource>>, id: u64, target: String, from: Option<String>) -> UiMsg {
    let result = match source {
        Some(source) => match tokio::time::timeout(std::time::Duration::from_secs(30), read(source.as_ref(), &target, from.as_deref())).await {
            Ok(result) => result.map_err(|e| format!("{e:#}")),
            Err(_) => Err("vault read timed out after 30 seconds".into()),
        },
        None => Err("no vault configured; note links need [mneme] in config.toml".into()),
    };
    UiMsg::VaultNote { id, result }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Fake {
        notes: Vec<(String, String)>,
        reads: Mutex<Vec<String>>,
    }
    impl Fake {
        fn new(paths: &[&str]) -> Self {
            Self { notes: paths.iter().map(|p| (p.to_string(), format!("# {p}"))).collect(), reads: Mutex::new(Vec::new()) }
        }
    }
    #[async_trait::async_trait]
    impl PersonaSource for Fake {
        async fn list(&self, folder: &str) -> anyhow::Result<Vec<String>> {
            assert_eq!(folder, "");
            Ok(self.notes.iter().map(|(p, _)| p.clone()).collect())
        }
        async fn read(&self, path: &str) -> anyhow::Result<String> {
            self.reads.lock().unwrap().push(path.into());
            self.notes.iter().find(|(p, _)| p == path).map(|(_, b)| b.clone()).ok_or_else(|| anyhow::anyhow!("missing"))
        }
    }
    fn state() -> UiState { UiState::new("mock".into(), "session".into(), "/tmp".into()) }
    fn note(path: &str, body: &str) -> Note {
        Note { path: path.into(), body: body.into(), anchor: None, scroll: 0, focus: None }
    }
    fn open(state: &mut UiState, tx: &mpsc::UnboundedSender<Cmd>, path: &str, body: &str) {
        request(state, tx, path.into());
        accept(state, state.vault.pending.unwrap(), Ok(note(path, body)));
    }

    #[tokio::test]
    async fn paths_suffixes_and_titles_resolve_to_one_read_not_a_recursive_fetch() {
        let source = Fake::new(&["wiki/Trackers/Eidolon Bugs Tracker.md", "wiki/Elsewhere.md"]);
        for link in ["wiki/Trackers/Eidolon Bugs Tracker.md", "Trackers/Eidolon Bugs Tracker", "Eidolon Bugs Tracker"] {
            let n = read(&source, link, None).await.unwrap();
            assert_eq!(n.path, "wiki/Trackers/Eidolon Bugs Tracker.md");
        }
        assert_eq!(source.reads.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_hash_in_a_title_resolves_the_whole_title_not_its_first_word() {
        let source = Fake::new(&["Aoide/Aoide #3 reply.md", "Aoide.md"]);
        let n = read(&source, "Aoide #3 reply", None).await.unwrap();
        assert_eq!(n.path, "Aoide/Aoide #3 reply.md");
        assert!(n.anchor.is_none());
        // A real anchor still jumps within the note it names.
        let n = read(&source, "Aoide#Heading", None).await.unwrap();
        assert_eq!(n.path, "Aoide.md");
        assert_eq!(n.anchor.as_deref(), Some("Heading"));
    }

    #[tokio::test]
    async fn ambiguity_and_missing_notes_never_read_an_arbitrary_body() {
        let source = Fake::new(&["a/Note.md", "b/Note.md"]);
        assert!(read(&source, "Note", None).await.unwrap_err().to_string().contains("ambiguous"));
        assert!(read(&source, "Missing", None).await.unwrap_err().to_string().contains("no vault note"));
        assert!(source.reads.lock().unwrap().is_empty());
        assert_eq!(read(&source, "a/Note", None).await.unwrap().path, "a/Note.md");
        let source = Fake::new(&["Note.md", "b/Note.md"]);
        assert_eq!(read(&source, "Note", None).await.unwrap().path, "Note.md");
    }

    #[tokio::test]
    async fn a_fragment_only_link_is_relative_to_the_open_note() {
        let source = Fake::new(&["a/Note.md"]);
        let n = read(&source, "#Heading", Some("a/Note.md")).await.unwrap();
        assert_eq!(n.path, "a/Note.md");
        assert_eq!(n.anchor.as_deref(), Some("Heading"));
        assert!(read(&source, "#Heading", None).await.is_err());
        for bad in ["/etc/passwd", "../Note", "https://example.com"] { assert!(read(&source, bad, None).await.is_err()); }
    }

    #[tokio::test]
    async fn no_vault_is_an_error_reply_not_a_turn() {
        let UiMsg::VaultNote { id, result } = fetch_message(None, 7, "Note".into(), None).await else { panic!() };
        assert_eq!(id, 7);
        assert!(result.unwrap_err().contains("no vault configured"));
        let source: Arc<dyn PersonaSource> = Arc::new(Fake::new(&["Note.md"]));
        let UiMsg::VaultNote { result, .. } = fetch_message(Some(source), 8, "Note".into(), None).await else { panic!() };
        assert_eq!(result.unwrap().path, "Note.md");
    }

    #[test]
    fn back_restores_scroll_and_focus_without_a_second_read_or_touching_the_draft() {
        let mut s = state();
        s.prompt.insert("an unfinished thought");
        let draft = s.prompt.text().to_string();
        let (tx, mut rx) = mpsc::unbounded_channel();
        open(&mut s, &tx, "First", &"[[Second]]\n".repeat(60));
        s.vault.page.as_mut().unwrap().selected = 12;
        s.vault.current.as_mut().unwrap().focus = Some(3);
        open(&mut s, &tx, "Second", "[[Third]]");
        back(&mut s);
        assert_eq!(s.vault.current.as_ref().unwrap().path, "First");
        assert_eq!(s.vault.page.as_ref().unwrap().selected, 12);
        assert_eq!(s.vault.current.as_ref().unwrap().focus, Some(3));
        back(&mut s);
        assert!(s.vault.page.is_none());
        assert_eq!(s.prompt.text(), draft);
        assert!(s.transcript.is_empty());
        assert!(matches!(rx.try_recv().unwrap(), Cmd::VaultRead { .. }));
        assert!(matches!(rx.try_recv().unwrap(), Cmd::VaultRead { .. }));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn late_replies_cannot_reopen_closed_pages_or_replace_newer_reads() {
        let mut s = state();
        let (tx, _rx) = mpsc::unbounded_channel();
        request(&mut s, &tx, "Old".into());
        let old = s.vault.pending.unwrap();
        request(&mut s, &tx, "New".into());
        let new = s.vault.pending.unwrap();
        accept(&mut s, old, Ok(note("Old", "old")));
        assert_eq!(s.vault.pending, Some(new));
        accept(&mut s, new, Ok(note("New", "new")));
        assert_eq!(s.vault.current.as_ref().unwrap().path, "New");
        request(&mut s, &tx, "Late".into());
        let late = s.vault.pending.unwrap();
        back(&mut s);
        accept(&mut s, late, Ok(note("Late", "late")));
        assert!(s.vault.page.is_none());
        request(&mut s, &tx, "Fresh".into());
        assert!(s.vault.pending.unwrap() > late);
        accept(&mut s, late, Err("late failure".into()));
        assert_eq!(s.vault.pending, Some(late + 1));
    }

    #[test]
    fn failed_navigation_keeps_the_previous_note_and_its_history() {
        let mut s = state();
        let (tx, _rx) = mpsc::unbounded_channel();
        open(&mut s, &tx, "First", "hello");
        request(&mut s, &tx, "Missing".into());
        let id = s.vault.pending.unwrap();
        accept(&mut s, id, Err("missing".into()));
        assert_eq!(s.vault.page.as_ref().unwrap().body, "hello");
        assert!(s.vault.history.is_empty());
    }

    #[test]
    fn anchors_are_rendered_heading_rows_not_prose_lookalikes() {
        let mut s = state();
        s.transcript_rect = Rect::new(0, 0, 50, 10);
        let (tx, _rx) = mpsc::unbounded_channel();
        request(&mut s, &tx, "Note#Heading".into());
        let mut n = note("Note", &format!("Heading\n{}\n## Heading\n{}", "before\n".repeat(20), "after\n".repeat(20)));
        n.anchor = Some("Heading".into());
        let id = s.vault.pending.unwrap();
        accept(&mut s, id, Ok(n));
        assert!(s.vault.page.as_ref().unwrap().selected > 20);
        let doc = document(&s);
        assert_eq!(doc.anchors[0].1, s.vault.page.as_ref().unwrap().selected);
    }
}
