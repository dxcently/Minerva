//! The command vocabulary: **one table naming every action the TUI can
//! perform**, and the only place a new one is declared.
//!
//! There used to be four half-lists — the arms of [`crate::app::exec`],
//! the arms of the driver's `command`, the `WANTS_CHAR` const in
//! [`crate::keys`], and a hand-written help paragraph — so a command could
//! exist for a key and not for `:`, or take a char argument the help never
//! mentioned. This is that list, once. `:name arg` resolves against it, a
//! keymap binds names out of it, the `:` line completes it, and `:help`
//! is generated from it, so the three can no longer disagree.
//!
//! A row says three things about a command: where it runs, what it takes
//! after its name ([`Arg`], whose hint gives the *shape* — `PATH`,
//! `[KEY]`, `ID[,ID]`) and where the values for that argument come from
//! ([`Fill`]). The third is what lets the `:` line complete past the
//! space; it lives on the row beside the other two because the
//! alternative is a second list of what commands want, which is the
//! drift this table exists to prevent. [`crate::complete`] is that half
//! read.
//!
//! The split that remains is *where* a command runs, not whether it
//! exists. [`Run::Ui`] commands act on the prompt buffer and the
//! transcript on the UI thread; [`Run::Driver`] commands need the agent,
//! the catalog or the session logs and are forwarded over the channel.
//! Both are named the same way and reached the same way — the caller
//! looks the name up here and the answer says which channel to use.
//!
//! ## Commands the script adds
//!
//! `commands()` in `ui.rn` may declare more: a name, a description, and
//! the Rune function that runs it (on the UI thread, handed the snapshot
//! and the argument, returning an action like `on_submit` does). Those
//! are [`Scripted`], and the `*_in` functions here resolve against the
//! built-ins *and* a slice of them, so a script's command is bound,
//! completed and printed by `:help` exactly as a built-in is. A script cannot redefine a built-in: the name is
//! refused at load, since a key that means one thing in every harness
//! and another in yours is the drift this table exists to prevent.

/// Which side of the two-thread split runs a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run {
    /// On the UI thread, against `UiState` — see [`crate::app::exec`].
    Ui,
    /// On the driver, against the agent — sent as `Cmd::Command`.
    Driver,
}

/// What, if anything, a command takes after its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    /// Nothing.
    None,
    /// The next key press, as a literal character (`f`, `t`, `r`). The
    /// keymap holds the keyboard open for it rather than resolving it.
    Char,
    /// The next two key presses, as literal characters — `surround_replace`'s
    /// from-pair and to-pair ride in together.
    Char2,
    /// Trailing text on the `:` line; the string names it for the help.
    Text(&'static str),
}

/// Where an argument's *values* come from, when they come from anywhere.
///
/// The hint on [`Arg::Text`] says what shape the argument is (`PATH`,
/// `[KEY]`, `ID[,ID]`); this says what the harness could offer if asked.
/// They are two halves of one claim about a command and they live on one
/// row, so the usage line, the `:` line's popup and `:help` cannot come
/// to disagree about what a command wants — which is the whole reason
/// this table exists at all.
///
/// Everything here is answerable from [`crate::state::UiState`] on the
/// UI thread, and that is a constraint rather than a coincidence: the
/// popup is recomputed on every keystroke of the `:` line, and a
/// completion that had to cross the channel and come back would be a
/// completion that arrived after the next character was typed. What the
/// driver knows and the UI does not — the catalog, the pinned files — is
/// handed over once and kept, not asked for per key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// Free text: a note, a shell command, an utterance. There is
    /// nothing to offer, and offering the *shape* is what the hint does.
    Free,
    /// A path on disk, completed against the filesystem.
    Path,
    /// A directory on disk.
    Dir,
    /// A mode from the script's `modes()` table.
    Mode,
    /// A model key from the catalog.
    Model,
    /// A provider that declares a `token_secret`, from the catalog.
    Provider,
    /// A session log in the sessions directory.
    Session,
    /// The name of an image staged on the prompt.
    Attachment,
    /// A record id on the branch.
    Record,
    /// A record id already struck from the context.
    Struck,
    /// A file pinned into every turn.
    Pin,
    /// A persona in the operator's vault, by name.
    Persona,
    /// A dispatch surface.
    Surface,
    /// A fixed set of words this command knows about.
    Words(&'static [&'static str]),
}

impl Fill {
    /// Is the argument a *list* of these, rather than one of them?
    ///
    /// The distinction is what the `:` line splits on. An argument runs
    /// to the end of the line and is not cut at spaces — a model key can
    /// be `fau:Ministral 3 14B` and a filename can have a space in it —
    /// so the comma of `:exclude 12,14` is the only separator inside an
    /// argument, and only these two have one.
    pub fn list(self) -> bool {
        matches!(self, Fill::Record | Fill::Struck)
    }

    /// A script's `fill:` field, by name. `Words` is deliberately absent:
    /// a fixed set belongs in the row that declares it, and a script
    /// declaring one would be declaring it in a string.
    pub fn named(s: &str) -> Option<Fill> {
        Some(match s {
            "free" | "" => Fill::Free,
            "path" => Fill::Path,
            "dir" => Fill::Dir,
            "mode" => Fill::Mode,
            "model" => Fill::Model,
            "provider" => Fill::Provider,
            "session" => Fill::Session,
            "attachment" => Fill::Attachment,
            "record" => Fill::Record,
            "struck" => Fill::Struck,
            "pin" => Fill::Pin,
            "persona" => Fill::Persona,
            "surface" => Fill::Surface,
            _ => return None,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Command {
    pub name: &'static str,
    pub run: Run,
    pub arg: Arg,
    /// What could go in that argument. [`Fill::Free`] whenever the
    /// argument is prose, and whenever there is no argument at all.
    pub fill: Fill,
    /// The heading it files under on the `:` line and in `:help`.
    pub group: &'static str,
    pub help: &'static str,
}

const fn ui(name: &'static str, group: &'static str, help: &'static str) -> Command {
    Command {
        name,
        run: Run::Ui,
        arg: Arg::None,
        fill: Fill::Free,
        group,
        help,
    }
}

const fn ch(name: &'static str, group: &'static str, help: &'static str) -> Command {
    Command {
        name,
        run: Run::Ui,
        arg: Arg::Char,
        fill: Fill::Free,
        group,
        help,
    }
}

const fn ch2(name: &'static str, group: &'static str, help: &'static str) -> Command {
    Command {
        name,
        run: Run::Ui,
        arg: Arg::Char2,
        fill: Fill::Free,
        group,
        help,
    }
}

const fn ui_text(
    name: &'static str,
    group: &'static str,
    hint: &'static str,
    fill: Fill,
    help: &'static str,
) -> Command {
    Command {
        name,
        run: Run::Ui,
        arg: Arg::Text(hint),
        fill,
        group,
        help,
    }
}

const fn driver(name: &'static str, group: &'static str, arg: Arg, help: &'static str) -> Command {
    Command {
        name,
        run: Run::Driver,
        arg,
        fill: Fill::Free,
        group,
        help,
    }
}

const fn driver_text(
    name: &'static str,
    group: &'static str,
    hint: &'static str,
    fill: Fill,
    help: &'static str,
) -> Command {
    Command {
        name,
        run: Run::Driver,
        arg: Arg::Text(hint),
        fill,
        group,
        help,
    }
}

/// Every command, in the order `:help` and a bare `:` list them.
pub const COMMANDS: &[Command] = &[
    // ------------------------------------------------------------ modes
    ui("insert_mode", "mode", "insert before the cursor"),
    ui("append_mode", "mode", "insert after the cursor"),
    ui(
        "insert_at_line_start",
        "mode",
        "insert at the first non-blank",
    ),
    ui(
        "insert_at_line_end",
        "mode",
        "insert past the end of the line",
    ),
    ui("open_below", "mode", "open a line below and insert"),
    ui("open_above", "mode", "open a line above and insert"),
    ui("normal_mode", "mode", "leave insert or select"),
    ui(
        "select_mode",
        "mode",
        "extend the selection with every motion",
    ),
    ui_text(
        "enter_mode",
        "mode",
        "[NAME]",
        Fill::Mode,
        "enter a mode from the script's modes() table, or pick one",
    ),
    // ----------------------------------------------------------- motion
    ui("move_char_left", "motion", "left one character"),
    ui("move_char_right", "motion", "right one character"),
    ui("move_line_up", "motion", "up one line of the prompt"),
    ui("move_line_down", "motion", "down one line of the prompt"),
    ui("move_next_word_start", "motion", "start of the next word"),
    ui("move_next_word_end", "motion", "end of the next word"),
    ui(
        "move_prev_word_start",
        "motion",
        "start of the previous word",
    ),
    ui(
        "move_next_long_word_start",
        "motion",
        "start of the next WORD",
    ),
    ui("move_next_long_word_end", "motion", "end of the next WORD"),
    ui(
        "move_prev_long_word_start",
        "motion",
        "start of the previous WORD",
    ),
    ui("goto_line_start", "motion", "column zero"),
    ui("goto_line_end", "motion", "the last character of the line"),
    ui(
        "goto_line_end_newline",
        "motion",
        "the slot past the last character",
    ),
    ui(
        "goto_first_nonwhitespace",
        "motion",
        "the first non-blank of the line",
    ),
    ui("goto_buffer_start", "motion", "the start of the prompt"),
    ui("goto_buffer_end", "motion", "the end of the prompt"),
    ui(
        "goto_word",
        "motion",
        "tag every word on screen; type a tag to take it, or to go there in the prompt",
    ),
    ui(
        "goto_block",
        "motion",
        "tag every block on screen; type a tag to put the trace cursor on it",
    ),
    ch("find_next_char", "motion", "forward onto the next CHAR"),
    ch("till_next_char", "motion", "forward up to the next CHAR"),
    ch("find_prev_char", "motion", "back onto the previous CHAR"),
    ch("till_prev_char", "motion", "back up to the previous CHAR"),
    ui("goto_line", "motion", "line N, or the last line with no count"),
    ui("goto_column", "motion", "column N of the line, or its start"),
    ui("goto_next_paragraph", "motion", "the next paragraph break"),
    ui("goto_prev_paragraph", "motion", "the previous paragraph break"),
    ui("repeat_last_motion", "motion", "the last find or bracket jump, again"),
    // -------------------------------------------------------- selection
    ui("select_line", "selection", "select the whole line"),
    ui("select_all", "selection", "select the whole prompt"),
    ui("collapse_selection", "selection", "drop to a bare cursor"),
    ui("flip_selections", "selection", "swap the anchor and the cursor"),
    ui("extend_to_line_bounds", "selection", "take the whole lines under the selection"),
    ui("shrink_to_line_bounds", "selection", "snap the selection back to whole lines"),
    ui("match_bracket", "selection", "the bracket matching the one under the cursor"),
    ch("select_object_inner", "selection", "select inside WORD, a quote, or a bracket"),
    ch("select_object_around", "selection", "select around WORD, a quote, or a bracket"),
    ch("surround_add", "selection", "wrap the selection in CHAR"),
    ch2("surround_replace", "selection", "swap the CHAR pair around the selection for another"),
    ch("surround_delete", "selection", "take the CHAR pair around the selection off"),
    // ----------------------------------------------------------- change
    ui("delete_selection", "change", "delete the selection"),
    ui(
        "change_selection",
        "change",
        "delete the selection and insert",
    ),
    ui("yank", "change", "yank the selection"),
    ui("paste_after", "change", "paste after the cursor"),
    ui("paste_before", "change", "paste before the cursor"),
    ch(
        "replace",
        "change",
        "replace every selected character with CHAR",
    ),
    ui("switch_case", "change", "switch the case of the selection"),
    ui("switch_to_lowercase", "change", "fold the selection to lowercase"),
    ui("switch_to_uppercase", "change", "fold the selection to uppercase"),
    ui("replace_with_yanked", "change", "the selection becomes what was yanked"),
    ui(
        "delete_selection_noyank",
        "change",
        "delete the selection, the register left alone",
    ),
    ui(
        "change_selection_noyank",
        "change",
        "delete the selection and insert, the register left alone",
    ),
    ui(
        "change_to_line_end",
        "change",
        "delete from the cursor to the line end and insert",
    ),
    ui("join_selections", "change", "fold the selected lines into one"),
    ui("indent", "change", "two spaces on every selected line"),
    ui("unindent", "change", "two spaces, or a tab, off every selected line"),
    ui("increment", "change", "the number on or after the cursor, up by N"),
    ui("decrement", "change", "the number on or after the cursor, down by N"),
    ui("repeat_last_insert", "change", "what the last spell of typing typed, again"),
    ui("add_newline_below", "change", "a blank line below, cursor staying put"),
    ui("add_newline_above", "change", "a blank line above, cursor staying put"),
    ui("commit_undo_checkpoint", "change", "cut the insert run here, so the rest undoes alone"),
    ui("undo", "change", "undo"),
    ui("redo", "change", "redo"),
    ui(
        "delete_char_backward",
        "change",
        "delete the character behind the cursor",
    ),
    ui(
        "delete_char_forward",
        "change",
        "delete the character under the cursor",
    ),
    ui(
        "delete_to_line_start",
        "change",
        "kill back to the start of the line",
    ),
    ui(
        "delete_to_line_end",
        "change",
        "kill on to the end of the line",
    ),
    ui(
        "delete_word_backward",
        "change",
        "kill the word behind the cursor",
    ),
    ui(
        "delete_word_forward",
        "change",
        "kill the word in front of the cursor",
    ),
    ui(
        "transpose_chars",
        "change",
        "drag the character behind the cursor over the one at it",
    ),
    ui("insert_newline", "change", "break the line"),
    ui("clear_prompt", "change", "empty the prompt"),
    ui("history_prev", "change", "the previous message you sent"),
    ui("history_next", "change", "the next message you sent"),
    // ------------------------------------------------------- transcript
    ui("scroll_line_up", "transcript", "up a line"),
    ui("scroll_line_down", "transcript", "down a line"),
    ui("scroll_half_up", "transcript", "up half a screen"),
    ui("scroll_half_down", "transcript", "down half a screen"),
    ui("scroll_page_up", "transcript", "up a screen"),
    ui("scroll_page_down", "transcript", "down a screen"),
    ui("transcript_top", "transcript", "the top of the session"),
    ui("transcript_bottom", "transcript", "follow the tail again"),
    ui(
        "cycle_detail",
        "transcript",
        "tool detail: folded clusters / every call / uncapped output",
    ),
    // ------------------------------------------------------------ trace
    ui(
        "trace_mode",
        "trace",
        "walk the transcript a block at a time; again to leave",
    ),
    ui_text(
        "trace",
        "trace",
        "[WHAT]",
        Fill::Free,
        "walk only the blocks carrying WHAT — a tool name, a kind, or * for the calls that changed something",
    ),
    ui("link_next", "trace", "select the next vault link in this block or note"),
    ui("link_prev", "trace", "select the previous vault link"),
    ui("link_open", "trace", "open the selected vault link (or the first)"),
    ui("link_back", "trace", "return to the previous vault note"),
    ui("link_close", "trace", "close the vault document and return to the conversation"),
    ui("trace_next", "trace", "the next block"),
    ui("trace_prev", "trace", "the previous block"),
    ui("trace_first", "trace", "the first block of the session"),
    ui("trace_last", "trace", "the last block"),
    ui("trace_open", "trace", "open the fold here and step into it"),
    ui(
        "trace_close",
        "trace",
        "close the fold this block is inside",
    ),
    ui(
        "trace_toggle",
        "trace",
        "open the fold here, or close the one we are in",
    ),
    ui(
        "trace_open_all",
        "trace",
        "open every fold in the transcript",
    ),
    ui("trace_close_all", "trace", "close every fold again"),
    ui(
        "trace_extend",
        "trace",
        "extend the transcript selection with every motion",
    ),
    ui("trace_yank", "trace", "copy the selected blocks"),
    ui("trace_quote", "trace", "stage the selected vault source-line address in the draft"),
    // ----------------------------------------------------------- search
    ui("search", "search", "search the transcript forwards"),
    ui("rsearch", "search", "search the transcript backwards"),
    ui(
        "search_next",
        "search",
        "the next match, the way the search was going",
    ),
    ui("search_prev", "search", "the previous match"),
    ui(
        "search_selection",
        "search",
        "search for what the prompt has selected",
    ),
    ui(
        "goto_hit",
        "search",
        "tag every match on screen; type a tag to go there",
    ),
    ui(
        "reveal_match",
        "search",
        "open whatever is hiding the current match",
    ),
    // ---------------------------------------------------------- eidolon
    ui(
        "submit",
        "eidolon",
        "send the prompt; accept a dialog or a search",
    ),
    ui("dialog_deny", "eidolon", "answer no to an approval"),
    ui(
        "cancel",
        "eidolon",
        "cancel the turn in flight, else drop the selection",
    ),
    ui("quit", "eidolon", "leave the harness"),
    ui(
        "command_line",
        "eidolon",
        "open the `:` line on an empty prompt",
    ),
    ui(
        "complete_next",
        "eidolon",
        "next completion on the `:` line",
    ),
    ui(
        "complete_prev",
        "eidolon",
        "previous completion on the `:` line",
    ),
    ui(
        "reload_ui",
        "eidolon",
        "recompile ui.rn: layout, keymap, modes and theme, without restarting",
    ),
    ui(
        "help",
        "eidolon",
        "the keys, from the live keymap, and this vocabulary",
    ),
    ui(
        "trace_strike",
        "trace",
        "strike the block under the cursor from the context, or put it back",
    ),
    ui(
        "messages",
        "eidolon",
        "every notice said on the status line this session, oldest first",
    ),
    ui(
        "usage",
        "eidolon",
        "what the session has spent, turn by turn: tokens, cache, dollars, tools and time",
    ),
    driver_text(
        "model",
        "eidolon",
        "[KEY]",
        Fill::Model,
        "switch the model, or pick one",
    ),
    driver_text(
        "login",
        "eidolon",
        "[PROVIDER]",
        Fill::Provider,
        "store a provider's API key, or pick a provider",
    ),
    driver_text(
        "fork",
        "eidolon",
        "[ID]",
        Fill::Record,
        "move the head to a record: the trace cursor, or an id",
    ),
    driver(
        "tree",
        "eidolon",
        Arg::None,
        "the session tree, with its forks",
    ),
    driver(
        "compact",
        "eidolon",
        Arg::None,
        "summarise the history and start again from it",
    ),
    driver_text(
        "exclude",
        "eidolon",
        "ID[,ID]",
        Fill::Record,
        "strike records from what the model is sent; they stay on screen",
    ),
    driver_text(
        "restore",
        "eidolon",
        "ID[,ID]",
        Fill::Struck,
        "put struck records back into the context",
    ),
    driver_text(
        "note",
        "eidolon",
        "[TEXT]",
        Fill::Free,
        "a standing note in every turn's system prompt; empty reports it, `-` clears",
    ),
    driver_text(
        "pin",
        "eidolon",
        "[PATH]",
        Fill::Path,
        "re-read a file into every turn; empty lists what is pinned",
    ),
    driver_text("unpin", "eidolon", "PATH", Fill::Pin, "stop pinning a file"),
    driver_text(
        "persona",
        "eidolon",
        "[NAME]",
        Fill::Persona,
        "wear a persona from the vault, or pick one; `-` takes it off",
    ),
    driver(
        "context",
        "eidolon",
        Arg::None,
        "what the next turn would send, block by block; runs no model",
    ),
    driver(
        "fresh",
        "eidolon",
        Arg::None,
        "drop the backend's own session; the next turn replays the branch",
    ),
    driver_text(
        "sessions",
        "eidolon",
        "[all]",
        Fill::Words(&["all"]),
        "the sessions on disk; enter resumes one (`all`: every directory's)",
    ),
    driver_text(
        "resume",
        "eidolon",
        "PATH",
        Fill::Session,
        "continue in another session log",
    ),
    driver(
        "new_session",
        "eidolon",
        Arg::None,
        "start a fresh session here, on this model",
    ),
    driver_text(
        "delete_session",
        "eidolon",
        "[PATH]",
        Fill::Session,
        "delete a session log, or pick one to delete",
    ),
    // On the UI thread: the switch is an atomic shared with the hook in
    // the dispatcher, so throwing it is a store and drawing it is a load,
    // and neither is the driver's business.
    ui_text(
        "yolo",
        "eidolon",
        "[on|off]",
        Fill::Words(&["on", "off"]),
        "answer every question the gate raises with yes; refusals still refuse",
    ),
    // -------------------------------------------------------- images
    // All three run on the UI thread: an attachment is staged on the
    // prompt, and the prompt is the UI thread's. Nothing here reaches the
    // agent — the image only becomes the agent's business when the
    // message it is on is sent.
    ui_text(
        "attach",
        "images",
        "PATH",
        Fill::Path,
        "stage an image for the next message",
    ),
    ui(
        "attach_clipboard",
        "images",
        "stage the image on the clipboard",
    ),
    ui_text(
        "attach_clear",
        "images",
        "[NAME]",
        Fill::Attachment,
        "unstage an image by name, or all of them",
    ),
    // ---------------------------------------------------------- launch
    // Another window, never this one — see [`crate::launch`]. All five
    // run on the UI thread because none of them is the agent's business:
    // a program spawned because the operator pressed `T` is the operator
    // running it, and nothing here reaches the dispatcher.
    //
    // With no argument each takes its aim from the trace cursor when
    // there is one — the file the call under it touched — and from the
    // session's working directory otherwise.
    ui_text(
        "terminal",
        "launch",
        "[DIR]",
        Fill::Dir,
        "a terminal window here",
    ),
    ui_text(
        "files",
        "launch",
        "[PATH]",
        Fill::Path,
        "the file manager here, on the file under the cursor",
    ),
    ui_text(
        "editor",
        "launch",
        "[PATH]",
        Fill::Path,
        "the editor here, on the file under the cursor",
    ),
    ui(
        "edit_prompt",
        "launch",
        "the draft in your editor, in this terminal — readline's C-x C-e",
    ),
    ui_text(
        "harness",
        "launch",
        "[DIR]",
        Fill::Dir,
        "another eidolon session here",
    ),
    ui_text(
        "launch",
        "launch",
        "CMD",
        Fill::Free,
        "run CMD in a new terminal window here",
    ),
    // --------------------------------------------------------- dispatch
    ui(
        "dispatch_mode",
        "dispatch",
        "aim the prompt at a dispatch surface; ret resolves and runs it",
    ),
    ui(
        "dispatch_surface",
        "dispatch",
        "aim at the next dispatch surface",
    ),
    driver_text(
        "dispatch",
        "dispatch",
        "TEXT",
        Fill::Free,
        "resolve TEXT on any surface and run the call it makes",
    ),
];

/// A command declared by the script's `commands()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scripted {
    pub name: String,
    /// The Rune function that runs it: `fn(state, arg) -> action`.
    pub run: String,
    pub help: String,
    pub group: String,
    /// The argument's name in the usage line, if it takes one.
    pub arg: Option<String>,
    /// What fills that argument, from the row's `fill:` field. A name
    /// [`Fill::named`] does not know is a load error rather than a
    /// silent [`Fill::Free`]: a script asking for `paths` and getting
    /// nothing would look exactly like completion being broken.
    pub fill: Fill,
}

impl Scripted {
    /// One row of `commands()`: `#{ desc, run?, group?, arg? }`, with
    /// `run` defaulting to the command's own name.
    pub fn parse(name: &str, v: &serde_json::Value) -> anyhow::Result<Self> {
        let Some(o) = v.as_object() else {
            anyhow::bail!("command `{name}` must be an object")
        };
        let field = |k: &str| {
            o.get(k)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        if name.is_empty() || name.contains(char::is_whitespace) {
            anyhow::bail!("command name `{name}` must be one word");
        }
        let filled = field("fill").unwrap_or_default();
        let fill = Fill::named(&filled)
            .ok_or_else(|| anyhow::anyhow!("command `{name}`: no such fill as `{filled}`"))?;
        Ok(Scripted {
            name: name.to_string(),
            run: field("run").unwrap_or_else(|| name.to_string()),
            help: field("desc").or_else(|| field("help")).unwrap_or_default(),
            group: field("group").unwrap_or_else(|| "script".into()),
            arg: field("arg").filter(|a| !a.is_empty()),
            fill,
        })
    }
}

/// A command from either table, once the lookup has found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry<'a> {
    Builtin(&'static Command),
    Script(&'a Scripted),
}

impl<'a> Entry<'a> {
    pub fn name(&self) -> &str {
        match self {
            Entry::Builtin(c) => c.name,
            Entry::Script(s) => &s.name,
        }
    }

    pub fn help(&self) -> &str {
        match self {
            Entry::Builtin(c) => c.help,
            Entry::Script(s) => &s.help,
        }
    }

    pub fn group(&self) -> &str {
        match self {
            Entry::Builtin(c) => c.group,
            Entry::Script(s) => &s.group,
        }
    }

    /// What could go in the argument.
    pub fn fill(&self) -> Fill {
        match self {
            Entry::Builtin(c) => c.fill,
            Entry::Script(s) => s.fill,
        }
    }

    /// The argument's name in the usage line, when it takes one. A
    /// script's row takes text when it named an argument and nothing
    /// when it did not — there is no way for a script to ask for the
    /// next *key press*, which is a keymap arrangement and not a
    /// command's.
    pub fn hint(&self) -> Option<&str> {
        match self {
            Entry::Builtin(c) => match &c.arg {
                Arg::None => None,
                Arg::Char => Some("CHAR"),
                Arg::Char2 => Some("FROM TO"),
                Arg::Text(h) => Some(h),
            },
            Entry::Script(s) => s.arg.as_deref(),
        }
    }

    /// Does it take an argument at all? What decides whether completing
    /// the *name* leaves the space for it behind.
    pub fn takes_arg(&self) -> bool {
        self.hint().is_some()
    }

    /// A script's command runs on the UI thread: that is where the script
    /// is.
    pub fn run(&self) -> Run {
        match self {
            Entry::Builtin(c) => c.run,
            Entry::Script(_) => Run::Ui,
        }
    }

    pub fn usage(&self) -> String {
        match self {
            Entry::Builtin(c) => usage(c),
            Entry::Script(s) => match &s.arg {
                Some(a) => format!(":{} {a}", s.name),
                None => format!(":{}", s.name),
            },
        }
    }
}

/// The commands a half-typed name could still become, best first: the
/// ones it prefixes, shortest first, then the ones that merely contain it
/// in table order. Shortest-first is what puts `set` above `select_mode`
/// for `se` — the least you could have meant by what you typed — and it
/// ranks an exact match first for free, since nothing shorter can have it
/// as a prefix. An empty query matches everything, which is what makes a
/// bare `:` list the vocabulary rather than nothing.
pub fn matches(query: &str) -> Vec<&'static Command> {
    matches_in(query, &[])
        .into_iter()
        .filter_map(|e| match e {
            Entry::Builtin(c) => Some(c),
            Entry::Script(_) => None,
        })
        .collect()
}

/// [`matches`] over the built-ins and the script's commands together,
/// the script's after the built-ins at equal rank.
pub fn matches_in<'a>(query: &str, extra: &'a [Scripted]) -> Vec<Entry<'a>> {
    let q = query.trim().to_lowercase();
    let all = COMMANDS
        .iter()
        .map(Entry::Builtin)
        .chain(extra.iter().map(Entry::Script));
    let (mut head, mut tail): (Vec<Entry<'a>>, Vec<Entry<'a>>) = (Vec::new(), Vec::new());
    for e in all {
        if e.name().starts_with(&q) {
            head.push(e);
        } else if e.name().contains(&q) || e.help().to_lowercase().contains(&q) {
            tail.push(e);
        }
    }
    // Stable, so equal-length names keep the table's order. An empty query
    // is not ranked at all: a bare `:` is browsing, and the table's own
    // grouping reads better there than a list sorted by name length.
    if !q.is_empty() {
        head.sort_by_key(|e| e.name().len());
    }
    head.extend(tail);
    head
}

pub fn lookup(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// [`lookup`] over both tables. A built-in wins a name, which cannot
/// happen after load — the collision is refused there — but is the right
/// answer if it somehow does.
pub fn lookup_in<'a>(name: &str, extra: &'a [Scripted]) -> Option<Entry<'a>> {
    lookup(name)
        .map(Entry::Builtin)
        .or_else(|| extra.iter().find(|s| s.name == name).map(Entry::Script))
}

/// Does this command swallow the next key press as its argument? The
/// keymap asks before resolving, so that `fd` finds a `d` rather than
/// deleting.
/// How many key presses the command wants as literal characters — zero
/// for the ordinary commands, one for the `f`/`r` family, two for the
/// surround pair. The keymap asks before resolving, so that `fd` finds a
/// `d` rather than deleting.
pub fn char_args(name: &str) -> usize {
    match lookup(name).map(|c| c.arg) {
        Some(Arg::Char) => 1,
        Some(Arg::Char2) => 2,
        _ => 0,
    }
}

pub fn wants_char(name: &str) -> bool {
    char_args(name) > 0
}

/// How the command is written on the `:` line, argument included.
pub fn usage(c: &Command) -> String {
    match c.arg {
        Arg::None => format!(":{}", c.name),
        Arg::Char => format!(":{} CHAR", c.name),
        Arg::Char2 => format!(":{} FROM TO", c.name),
        Arg::Text(hint) => format!(":{} {hint}", c.name),
    }
}

/// The whole vocabulary as text, grouped — what `:help` prints under its
/// paragraph about the keys.
pub fn reference() -> String {
    reference_in(&[])
}

/// [`reference`] with the script's commands after the built-ins, under
/// their own groups.
pub fn reference_in(extra: &[Scripted]) -> String {
    let mut out = String::new();
    let mut group = "";
    for e in COMMANDS
        .iter()
        .map(Entry::Builtin)
        .chain(extra.iter().map(Entry::Script))
    {
        if e.group() != group {
            out.push_str(&format!("\n{}\n", e.group()));
        }
        out.push_str(&format!("  {:<34} {}\n", e.usage(), e.help()));
        // Borrow the group from the entry for the next comparison.
        group = match e {
            Entry::Builtin(c) => c.group,
            Entry::Script(s) => s.group.as_str(),
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_command_is_declared_twice() {
        let mut seen: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        seen.sort_unstable();
        let n = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), n, "a command name is in the table twice");
    }

    #[test]
    fn a_prefix_ranks_above_a_mere_substring() {
        let names = |q| matches(q).iter().map(|c| c.name).collect::<Vec<_>>();
        // `search` prefixes `search_next`; `rsearch` only contains it, so
        // it comes after every one of them.
        let m = names("search");
        assert_eq!(m[0], "search");
        let rsearch = m
            .iter()
            .position(|n| *n == "rsearch")
            .expect("rsearch contains \"search\"");
        assert!(m.iter().position(|n| *n == "search_next").unwrap() < rsearch);
        // Prefix matches rank shortest first — the least you could have
        // meant — and equal lengths keep the table's order.
        // `commit_undo_checkpoint` is a plain prefix; `reload_ui` trails
        // because it is reached through "recompile" in its help, and a
        // substring ranks after every prefix — as does
        // `replace_with_yanked`, through "becomes" in its own.
        assert_eq!(
            names("com"),
            vec![
                "compact",
                "command_line",
                "complete_next",
                "complete_prev",
                "commit_undo_checkpoint",
                "replace_with_yanked",
                "reload_ui"
            ]
        );
        assert_eq!(names("se")[0], "search", "the shortest completion of `se`");
        // Reached through its help rather than its name.
        assert!(names("summarise").contains(&"compact"));
        // A bare `:` is the table itself, in its own order.
        assert_eq!(
            names(""),
            COMMANDS.iter().map(|c| c.name).collect::<Vec<_>>()
        );
        assert!(matches("zzz").is_empty());
    }

    #[test]
    fn a_scripted_command_is_found_completed_and_listed_beside_the_builtins() {
        let extra = vec![
            Scripted::parse(
                "shout",
                &serde_json::json!({ "desc": "send in capitals", "arg": "TEXT" }),
            )
            .unwrap(),
        ];
        assert_eq!(extra[0].run, "shout", "run defaults to the name");
        assert_eq!(extra[0].group, "script");
        assert_eq!(lookup_in("shout", &extra).unwrap().usage(), ":shout TEXT");
        assert_eq!(lookup_in("shout", &extra).unwrap().run(), Run::Ui);
        assert!(lookup_in("model", &extra).is_some_and(|e| matches!(e, Entry::Builtin(_))));
        let names: Vec<String> = matches_in("sho", &extra)
            .iter()
            .map(|e| e.name().to_string())
            .collect();
        assert_eq!(names, ["shout"]);
        assert!(reference_in(&extra).contains(":shout TEXT"));
        assert!(Scripted::parse("two words", &serde_json::json!({})).is_err());
        assert!(Scripted::parse("x", &serde_json::json!("desc")).is_err());
    }

    /// `:login` is a real row in the table — driver-side, an optional
    /// provider argument, completed against the provider fill — the same
    /// three claims `:model` makes about itself.
    #[test]
    fn login_is_registered_as_a_driver_command_with_provider_completion() {
        let c = lookup("login").expect("login must be a command");
        assert_eq!(c.run, Run::Driver);
        assert_eq!(c.fill, Fill::Provider);
        assert_eq!(usage(c), ":login [PROVIDER]");
        assert!(matches("log").iter().any(|c| c.name == "login"));
        assert!(!wants_char("login"), "it takes text, not a bare char");
    }

    #[test]
    fn the_char_takers_are_the_ones_the_keymap_holds_open_for() {
        for n in [
            "find_next_char",
            "till_next_char",
            "find_prev_char",
            "till_prev_char",
            "replace",
            "select_object_inner",
            "select_object_around",
            "surround_add",
            "surround_delete",
        ] {
            assert!(wants_char(n), "{n} should swallow the next key");
        }
        assert!(!wants_char("delete_selection"));
        assert!(!wants_char("no_such_thing"));
        // The surround pair is the one command that wants two.
        assert_eq!(char_args("surround_replace"), 2);
        for n in COMMANDS.iter().filter(|c| c.name != "surround_replace") {
            assert!(
                char_args(n.name) <= 1,
                "{} takes more chars than it should",
                n.name
            );
        }
    }
}
