//! The interactive consumer: a terminal UI whose *layout, status line, key
//! bindings and command vocabulary are a Rune script*, drawn by a Rust
//! renderer over a curated widget vocabulary.
//!
//! This is the split the vault fixed (*Extension model*, 2026-09-03). The
//! failure that started the project — fighting a harness's renderer from
//! outside it — cannot recur here because there is no privileged renderer
//! to be outside of: the thing that decides what goes where is
//! `ui/default.rn`, and a user replaces it by dropping their own `ui.rn`
//! in the config directory. What Rust keeps is what Rune would be bad at:
//! the terminal itself, text wrapping, markdown, prompt editing, and the
//! transcript data, exposed as widgets the script places rather than draws.
//!
//! ## The transcript
//!
//! [`render`] lays entries out from the last line backwards and stops once
//! the viewport is covered, so a frame costs what the window costs rather
//! than what the session does. Assistant text goes through [`markdown`].
//!
//! Thinking and tool traffic fold into **clusters**. A cluster is
//! everything the model did between one piece of prose and the next —
//! it thought, it ran three tools, it thought again, it ran two more —
//! and at rest it is *one line* saying what happened: `cogitat (246) ·
//! Read 2 files · Ran 3 shell commands`, with the failures counted onto
//! the end so nothing leaves the screen quietly. A turn that read six
//! files should not push the sentence explaining why out of the window.
//! The cluster at the end of the transcript stays open while the turn is
//! in flight, since that is the work you are watching happen.
//!
//! [`state::Detail`] is the same question asked globally and has three
//! steps, cycled by `space o`: folded, every call with its output
//! capped, every call with all of it. **[`trace`] is the same question
//! asked locally**, and is what `K` does: a cursor on the transcript, so
//! that `l` opens *this* cluster and steps into it rather than opening
//! every fold in the session to get at one. It is Helix's shape —
//! `j`/`k` between blocks, `l`/`h` down and up a level, `v` to extend,
//! `y` to yank — and the search moves the cursor, so a match inside a
//! fold is one key from being read.
//!
//! The wheel scrolls always; `j`/`k` scroll a line in normal mode, and
//! `S-↑`/`S-↓` a page in every mode — `foot` binds the bare page keys to
//! its own scrollback, so the shift-arrows are the ones that arrive.
//! `gb` follows the tail again.
//!
//! ## Finding things
//!
//! [`search`] is Helix's `/ ? n N *` pointed at the transcript, and it is
//! incremental: matches light up as the pattern is typed, the view
//! follows the nearest one, `ret` keeps the search (`n`/`N` then walk it)
//! and `esc` drops it and puts the view back where it was. Patterns are
//! smart-cased regexes; one that does not compile yet is matched as text
//! rather than blanking the results mid-word.
//!
//! The scan runs over the entries' *text* and a hit is an entry, so a
//! keystroke costs a regex pass over the session rather than a layout of
//! it. The **highlight** is computed separately, against the lines
//! actually drawn, which is what keeps it exact through markdown,
//! wrapping and folds. [`search`]'s own docs carry the reasoning.
//!
//! A match can also be somewhere that is *not drawn* — inside folded
//! tool traffic, or below the cap on an output — and a search that
//! counted it while lighting nothing would read as broken. So the
//! renderer asks the question per **block**: one that covers hits but
//! drew none of them says so, with the count written onto the line
//! standing in for it (`Wrote 3 files · 2 matches`) in the highlight's
//! own colour. `reveal_match` opens it — `S-tab` on the search line,
//! without leaving the search — and the line offers that key only while
//! there is something to open. Opening a cluster draws
//! every part of it with all of its output, since it was opened to find
//! something, and puts its start entry in `UiState::opened`: still a
//! view, the same kind of thing as `Detail`, aimed at one cluster
//! instead of all of them. [`trace`]'s `l` is the same act, reached
//! without a search.
//!
//! [`jump`] is the other half, and the faster one: **letter tags**, as in
//! Helix's `gw` — the eye has already found the place, and a tag is the
//! shortest way to say *that one*.
//!
//! `gw` tags **what is on the screen**: every word in the visible
//! conversation and every word in the draft, in one press. It used to
//! tag the prompt's words alone, which is backwards — the prompt is
//! where you are already typing, and the conversation is the part you
//! cannot otherwise reach, so `gw` on an empty prompt had nothing to say
//! while a screenful sat above it. `gW` is the same gesture one unit up:
//! a label per *block*. Two sets and not one because a screenful is a
//! couple of hundred words and about a dozen blocks. `tab` on the search
//! line is the third: a letter on every match, instead of pressing `n`
//! at it six times.
//!
//! Where a tag lands is a property of the tag ([`jump::Spot`]), not of
//! the set, which is what lets one `gw` cover two surfaces. A word in
//! the prompt is a char index and so a *motion* — select mode extends to
//! it. A word in the transcript has no cursor to become, so landing
//! **takes** it: the cells are selected, exactly as a mouse drag selects
//! them, and the word goes to the register and the clipboard, which is
//! what one wants a word out of a transcript for. A block lands the
//! trace cursor, entering trace mode to do it, since a cursor nobody can
//! see has not moved.
//!
//! The two halves of `gw` are even cut differently, and deliberately: in
//! the prompt the class is Helix's word, because a tag there names a
//! place the cursor goes; in the transcript it is the
//! whitespace-delimited run, because a tag there names a thing you are
//! about to copy and `crates/tui/src/edit.rs` is one of those, not four.
//!
//! A tag is written *over* the head of what it points at, so nothing
//! reflows as the tags come up, and every tag in a set is the same
//! width, so none is a prefix of another and a press never has to be
//! waited on.
//!
//! A set of tags owns the keyboard while it is up, because every letter
//! on it is a tag, including the ones that are also commands; it sits in
//! front of the keymap in `handle_key`, next to the dialog that had the
//! same problem first. The search *line* is not a special case at all
//! any more — it is a [`state::Mini`], one of the three one-line modes
//! written on the prompt's bottom border, with its own keymap table and
//! its own ordinary [`edit::Buffer`]. So a hand-written `ui.rn` gets it
//! without knowing it exists, it costs the transcript no rows, and the
//! draft underneath is not merely untouched but **on the screen and
//! searched**: `search::scan_all` scans the message being composed as
//! the entry after the last one.
//!
//! `goto_hit` and `reveal_match` are bound on the search line **and
//! nowhere else**. Both need a live search to mean anything,
//! so binding them in normal mode would put an action in the leader popup
//! that usually cannot run, which is the opposite of what a modal keymap
//! is for. They stay typable, since `:` reaches the whole registry by
//! design. `n`/`N`/`*` are not a counterexample: those are Helix's own
//! normal-mode search keys, and they say so when there is no search.
//!
//! ## The mouse
//!
//! Turning mouse reporting on to get the wheel costs the terminal's own
//! drag-selection — the button press comes to us instead — so the harness
//! draws and copies the selection itself: a drag inverts the cells it
//! covers, releasing puts them on the clipboard ([`clipboard`]), and the
//! highlight retires on the next keystroke or when text moves under it.
//! Shift-drag still reaches the terminal, for anyone who prefers it.
//!
//! ## Threads
//!
//! Two. The **UI thread** owns the terminal, the Rune VM and the UI state;
//! it polls key input, drains a channel of [`app::UiMsg`] from the core
//! (bus events, dialog requests, turn results) and redraws. The **driver**
//! runs on the tokio runtime, owns the agent, forwards the event bus,
//! runs turns as tasks so `Cancel` can land mid-turn, and steers a turn
//! in flight with the message typed during it (pi's `steer`). They talk
//! over
//! two mpsc channels; `choices_user` reaches the UI thread as a
//! [`app::DialogRequest`] carrying a oneshot for the answer, which is what
//! makes the user prompt modal without any shared state.
//!
//! ## Modal keys
//!
//! The prompt is a Helix buffer ([`edit`]): text, one selection, and a
//! mode. Normal is where it rests — `i` starts a message, `esc` comes
//! back, and submitting hands it back in normal. Insert rested there
//! first, on the reasoning that a chat prompt is mostly typing; it cost
//! the keyboard, because `:` and the leader then needed an `esc` in front
//! of them that nothing on screen suggested. Normal mode drives two
//! surfaces at once: the prompt
//! (`w b e`, `f t`, `d c y p`, `x %`, `u U`) and the transcript (`j k`,
//! `gt`/`gb`). There are no control bindings at all; `space` is the leader
//! and its popup says what it can become.
//!
//! The table in `ui/default.rn` is fitted to the operator's own Helix and
//! yazi keymaps rather than to stock Helix — shift-arrows in every mode,
//! `^`/`$` as hard line ends, `space w`/`space q`, a `desc` on every
//! binding. That file says which convention came from where.
//!
//! **A mode is a row in the script's table.** `modes()` in `ui.rn`
//! declares every mode: its name, its badge, its colour, the character
//! it wears at the head of the line, and — for the ones the prompt can
//! be in — its *kind*, which is the only part the buffer needs (see
//! [`modes`] and [`edit::Kind`]). `enter_mode NAME` reaches any row
//! with a kind, from a key or the `:` line, so a fifth mode is a row and
//! a keymap table, with `on_submit` deciding what `ret` means there.
//! Three names are canonical because Helix's verbs reach them — `i`
//! goes to insert, `v` to select, `esc` to normal — and a script may
//! restyle those but not change their kind.
//!
//! **A mode says what it is, the same way in every mode.**
//! [`render::mode_style`] reads that table and is the one answer — the
//! prompt's border, the title on that border and the script's badge
//! are three readers of it, so none can contradict the others; the row
//! reaches the script as `mode_colour`, `mode_label` and `mode_sigil` on
//! the snapshot rather than being written out a second time in Rune.
//! The four modes entered by typing one character wear it at the head
//! of the line in that colour: `:` command, `/` search, `?` search
//! back, `!` dispatch. Two of those are real text (the colon) or a
//! pattern drawn separately; the `!` is neither, so it is *drawn and
//! not stored* — the buffer holds the utterance alone, and dispatch
//! would otherwise be the loudest mode in the harness while looking
//! exactly like insert.
//!
//! **The theme is a table too.** Every colour Rust paints on its own
//! account — the search highlight, the tags, the popup, a dialog, the
//! tool arrows, a heading in the model's markdown — is a named role in
//! [`theme`], and `theme()` in the script overrides any of them. Modes'
//! colours are not roles: they live on the mode's row, beside the label
//! and sigil they belong with.
//!
//! **Commands are a table too.** `commands()` in the script declares
//! commands of its own — a name, a description, the Rune function that
//! runs it as `fn(state, arg) -> action` — and [`command`]'s `*_in`
//! lookups resolve against the built-ins and those together, so a
//! script's command is bound, completed on the `:` line and printed by
//! `:help` exactly as a built-in is. A built-in's
//! name is refused at load. `:help` itself is generated: every mode's
//! bindings from the keymap as loaded, chords spelled out, then the
//! vocabulary — so it cannot drift from what runs.
//!
//! **The one-line modes and the dialogs are keymap tables.** They used
//! to own the keyboard in Rust; now `command`, `search`, `dispatch`,
//! `confirm`, `choose` and `pick` are tables like `normal` is,
//! resolved by the same [`keys`] and drawn in the same which-key popup,
//! with `submit`, `cancel`, `dialog_deny` and the buffer's motion names
//! meaning the obvious thing there. A script that leaves a dialog's
//! table out gets the default's, since an approval box nothing can
//! answer is a hang; one that leaves out a one-line mode's gets
//! insert's, which is the whole of what such a line needs.
//!
//! **The readline keys** — `C-a`/`C-e`, `C-b`/`C-f`, `A-b`/`A-f`, the
//! kills `C-u`/`C-k`/`C-w`/`A-d` and `C-y` to put back what the last one
//! took — are bound in every one of those tables and in insert, and in
//! none of the modal ones. Normal and select keep the Helix keyboard
//! exactly; what justifies the control keys where text is typed is that
//! the hand reaching for `C-a` mid-message is in a text field, not in
//! Helix.
//!
//! `reload_ui` recompiles the script in place — layout, keymap, modes,
//! theme and commands — so editing `ui.rn` is a save and a command, not
//! a restart.
//!
//! ## Dispatch mode
//!
//! A one-line mode ([`dispatch`], on `!` or `space d d`) aims a line at
//! a **classifier** instead of at the model: a line submitted there is
//! resolved by `eidolon_verba` into one typed tool call in milliseconds,
//! with no turn, and then goes through `Dispatcher::dispatch` like any
//! other call. It is a mode and not a picker because what is typed is a
//! sentence, not a row — it wants a buffer, with `ret` meaning something
//! else — and it is the [`state::Mini`] it wants rather than the prompt,
//! so an utterance and a message stop sharing one. Sending leaves the
//! line up, the way sending a message leaves insert, so a run of
//! utterances costs one `!`. The mode is aimed at one *surface* (`mneme`
//! today; `tab` cycles when there are more), because the operator
//! entering it has already said which vocabulary they are about to
//! speak. An abstention is not an error and does not silently fall back
//! to the model: it says what it was close to and leaves the line in the
//! dispatch line's own history.
//!
//! ## The script contract
//!
//! ```rune
//! pub fn view(state) -> node          // the widget tree for this frame
//! pub fn keymap() -> table            // mode → key → command, read once
//! pub fn modes() -> table             // mode → row, read once (optional)
//! pub fn theme() -> table             // role → colour, read once (optional)
//! pub fn commands() -> table          // name → #{ desc, run, arg? }, read once (optional)
//! pub fn on_submit(state, text) -> action
//! ```
//!
//! Nodes and actions are plain objects (see [`render`] for the vocabulary
//! and [`script`] for the actions). `ui/prelude.rn` supplies builders so a
//! script reads as layout, not JSON. If the script fails to compile or a
//! call errors, the renderer falls back to a built-in tree and shows the
//! error in the transcript — a broken script never blanks the screen.
//!
//! There is no `on_key`, deliberately: a modal keymap needs to remember a
//! half-typed chord and a pending count, and a Rune [`script::UiScript`]
//! builds a fresh VM per call. So [`keys`] holds that state in Rust and
//! the script owns the table — which is the half worth owning, since it
//! is where "what does `d` do" is answered.
//!
//! ## One vocabulary
//!
//! What a binding may name is [`command`]: one table of every action the
//! TUI can perform, with where it runs, what argument it takes and what
//! could go in it. `:` opens the command line and resolves against that
//! same table, so a key and a typed command are two ways of saying one
//! thing and never two vocabularies that drift — `space m` *is*
//! `:model`, and `space p` is a second key onto the line itself.
//! `:help` prints the table, so a command cannot exist without being
//! findable.
//!
//! There was a **command palette** on `space p` — the registry as a
//! filterable list, `RunPicked` in [`state::PickAction`]. It is gone.
//! The `:` line completes the same table with the same ranking and shows
//! the command it resolved to *before* running it, where a list ran
//! whatever sat under the highlight; it completes the argument too,
//! which a list of names cannot — the tell was that `RunPicked` needed a
//! rule of its own for commands that must be given something, and that
//! rule was handing off to the command line. What the palette was better
//! at was browsing, and `:help` is better at that still: the whole
//! vocabulary, grouped, on a page that scrolls.
//!
//! **A row says what its argument is made of, and the line completes
//! from that.** [`command::Fill`] is that half of the claim — `PATH`,
//! `[KEY]`, `ID[,ID]` say the *shape*; `Fill` says where the values come
//! from — and [`complete`] reads the `:` line in two spots against it.
//! Before the space, `tab` walks the command names and writes the space
//! itself when the command it lands on takes an argument; after it, the
//! popup is the argument's, filled from the row's source: the
//! filesystem, the catalog's model keys, the modes table, the record ids
//! on the branch, the images staged on the prompt. An argument that is
//! prose gets its shape said and nothing offered, which is the honest
//! answer rather than a guess. The popup and `tab` read one list, so
//! what is drawn is what enter will take.
//!
//! Everything a `Fill` names is answerable from [`state::UiState`],
//! which is a constraint and not a coincidence: the popup is rebuilt on
//! every keystroke, and a completion that had to cross the channel would
//! arrive after the next character. What only the driver knows — the
//! catalog, the session's pins — is handed over once and kept.
//!
//! A message half-written when the line opens is **set aside**, not
//! refused, and comes back when the line goes away — before the command
//! runs, so that a prompt command (`:select_all`) acts on the draft
//! rather than on the line `submit` has just taken. `:` used to do
//! nothing at all on a non-empty prompt, which made reaching a command
//! mid-draft the one thing the palette could do and the command line
//! could not.
//!
//! The pickers that remain — model, fork, sessions, modes — *rank* their
//! filter (what the query prefixes, shortest first, the key before the
//! description) rather than merely hiding, and the highlight follows the
//! best match: the keystrokes that identify a row are the keystrokes
//! that select it.
//!
//! There is no slash **command prefix**: a submitted line beginning with
//! `/` is ordinary prompt text and goes to the model. The `/` *key* is a
//! different question and is bound, in normal mode, to the search — as
//! in Helix, and without ambiguity, because a message is composed in
//! insert mode, where `/` is a character like any other.

// The status-line snapshot is one `json!` literal of every fact a
// script may read, and the macro expands one key at a time. The default
// limit is 128 and the table is past it.
#![recursion_limit = "256"]

pub mod app;
pub mod clipboard;
pub mod command;
pub mod complete;
pub mod dispatch;
pub mod edit;
pub mod editor;
pub mod image;
pub mod jump;
pub mod keys;
pub mod labels;
pub mod launch;
pub mod life;
pub mod markdown;
pub mod vault;
pub mod modes;
pub mod render;
pub mod script;
pub mod search;
pub mod sessions;
pub mod state;
pub mod syntax;
pub mod theme;
pub mod trace;
pub mod usage;
pub mod user;

pub use app::{TuiOptions, run};
pub use user::TuiUser;

pub const PRELUDE: &str = include_str!("../ui/prelude.rn");
pub const DEFAULT_UI: &str = include_str!("../ui/default.rn");
