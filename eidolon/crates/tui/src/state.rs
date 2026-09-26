//! UI-side state: the transcript as drawn, the prompt buffer, the pending
//! dialog, and the small snapshot the Rune script sees each frame.

use eidolon_core::UsageExt;
use std::time::{Duration, Instant};

use eidolon_core::session::{Record, RecordId, RecordKind};

/// How much of the transcript's tool traffic is drawn.
///
/// One knob with three steps rather than two toggles, because the two
/// questions an operator actually asks — *how many calls do I see* and
/// *how much of each output* — are the same question asked louder. The
/// resting step is [`Detail::Folded`]: a settled run of tool calls is one
/// line saying what it did, so a turn that read six files does not push
/// the sentence explaining why off the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Detail {
    /// A settled run of tool calls collapses to a summary line.
    #[default]
    Folded,
    /// Every call, with its arguments and a capped slice of its output.
    Calls,
    /// Every call, with the whole of its output.
    Full,
}

impl Detail {
    /// The next step of the cycle — what `space o` does.
    pub fn next(self) -> Detail {
        match self {
            Detail::Folded => Detail::Calls,
            Detail::Calls => Detail::Full,
            Detail::Full => Detail::Folded,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Detail::Folded => "folded",
            Detail::Calls => "calls",
            Detail::Full => "full",
        }
    }
}

/// When a run of *finished* calls folds into its summary line. This is a
/// when, not a how-much — [`Detail`] above is still the knob that decides
/// whether anything folds at all.
///
/// The default is [`Fold::Settle`]: nothing since the operator's message
/// folds while the turn runs, so the summary line does not rewrite itself
/// under the operator call by call, and the whole turn collapses once,
/// when it settles. [`Fold::Live`] is the older behaviour — each run
/// folds the moment the next call begins, trading the rewriting summary
/// for a transcript that keeps only the current call on screen. Both are
/// reasonable; `[transcript] fold` decides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fold {
    /// Each run folds as it finishes, even mid-turn.
    Live,
    /// The turn in flight stays open; one fold, at the settle.
    #[default]
    Settle,
}

/// A finished attachment read, or why it failed. One channel, because the
/// operator does the same thing about a missing file and a file that is
/// not an image: read the line and try again.
pub type Attached = Result<crate::image::Attachment, String>;
/// The end a reader thread holds.
pub type Attaching = std::sync::mpsc::Sender<Attached>;
/// The end the UI loop drains.
pub type Arrived = std::sync::mpsc::Receiver<Attached>;

#[derive(Clone, Debug)]
pub enum Entry {
    User(String),
    /// The model's extended thinking, kept in full.
    ///
    /// Its own entry rather than a character count on the message it
    /// preceded, which is what it used to be. Thinking is a *block*: it
    /// folds into a cluster, the search scans it, and the trace cursor
    /// lands on it and opens it — and a counter on a neighbouring entry
    /// could be none of those. It also disappeared the moment the reply
    /// began, since the count was only drawn while the text was empty.
    Thinking {
        text: String,
        streaming: bool,
    },
    Assistant {
        text: String,
        streaming: bool,
    },
    /// One tool call. `id` is the `tool_use` id it was made under, and is
    /// what a result is paired to: a turn with parallel calls journals
    /// three results against three open entries, and "the last one still
    /// empty" picks the wrong one every time.
    Tool {
        id: String,
        name: String,
        input: String,
        output: Option<(String, bool)>,
    },
    /// The harness saying what happened to the *conversation*: it was
    /// compacted, cancelled, moved to another model, resumed. Every one
    /// of these is a fact the log replays, which is the test for being
    /// an entry at all — `seed` draws the same line from the record, so
    /// a resumed session looks like the live one did. What happened to
    /// the last *keystroke* ("no matches", "yanked 2 blocks") is a
    /// [`Notice`] and never an entry: it used to be, and it made the
    /// transcript a block the trace cursor stopped on, a hit the search
    /// found, and a line resume could not reproduce.
    Info(String),
    /// An [`Info`](Entry::Info) the operator must not miss — a turn that did
    /// not finish, an error. Same line, painted in the `error` role instead
    /// of faint, because a notice nobody reads is not a notice.
    Alert(String),
    /// The line a **settled** turn leaves under its reply —
    /// `✶ cogitavit 22.4s · ⇣ 2.4k`. An [`Info`](Entry::Info) in
    /// everything but its mark: a note leads with the quiet dot every
    /// other line of harness talk wears, and this leads with the star of
    /// [`crate::life::STARS`], which turns over once per settled turn so no two
    /// replies finish under the same one. Its own entry rather than a
    /// flag, because the star a *resumed* session draws under turn three
    /// has to be the star turn three wore, and an entry that carries the
    /// turn it was drawn at is what makes replay and the live session
    /// agree without a counter living outside the transcript.
    Settle {
        /// Which turn this was, counted from zero over the transcript —
        /// what [`crate::life::star`] reads the mark off.
        turn: usize,
        text: String,
    },
    /// A message from another harness session.
    ///
    /// Not an [`Entry::User`], though it is user-role in the conversation:
    /// what the operator typed and what a colleague's agent sent are
    /// different things to look at, and drawing them the same way would
    /// leave the operator reading their own words in a voice they did not
    /// use. Not an [`Entry::Info`] either — an info line is the harness
    /// talking about itself, and this is a third party.
    Peer {
        from: String,
        channel: bool,
        text: String,
    },
    /// A park resolving: this session was woken by a condition it asked
    /// `wait_for` to watch, and this is the harness saying so — neither
    /// the operator speaking nor a colleague, and not a quiet note about
    /// the conversation either. It is the third way a turn starts, beside
    /// the operator and a peer's message, so like a peer's message it is
    /// drawn as its own kind and seeded from its own record; a resumed
    /// session sees the wake that started the turn it resumes into.
    Woke {
        /// The condition in words, as the model wrote it.
        condition: String,
        /// What became of it — `it fired`, `the deadline arrived first`.
        outcome: String,
    },
    /// One line the model **marked** in its own prose, as the call it
    /// became, with the harness's answer as its output.
    ///
    /// The inline command channel means the model writes `! read the note X`
    /// on a line of its reply and the loop — not the model — serialises it
    /// into a dispatched call, journaling the answers as
    /// [`RecordKind::CommandResults`]. So a marked line *is* a call, and it
    /// used to be drawn as ordinary prose with the answers arriving later as
    /// quiet [`Info`](Entry::Info) notes that nothing tied to it.
    /// [`UiState::answered`] takes the marked lines out of the reply when
    /// their answers are journaled and draws them here, where a call is
    /// drawn: the command, then what came back.
    ///
    /// One entry per record rather than per command, because that is what
    /// the log has — one record, one line per marked line, in the order the
    /// reply wrote them — and it is what makes the whole exchange one block
    /// for the trace cursor and one thing to strike.
    ///
    /// `commands` is empty when the transcript could not pair the answers
    /// with the lines that asked for them: the answers are the harness's
    /// words and must still be drawn, and an answer drawn under a command
    /// that did not ask for it would be worse than one drawn bare. See
    /// [`UiState::answered`].
    Ask {
        commands: Vec<Marked>,
        answers: Vec<String>,
    },
    /// An image on the message beside it.
    ///
    /// Its own entry rather than a decoration on [`Entry::User`], for the
    /// reason thinking is its own entry: it is a *block*. It occupies real
    /// rows, the trace cursor lands on it, and — crucially — the rows it
    /// occupies are rows nothing else may draw into, because a terminal
    /// that can show pixels shows them by painting over the cells the
    /// layout reserved. A count on a neighbouring entry could reserve
    /// nothing.
    ///
    /// It carries the attachment whole, including the bytes, because the
    /// transcript is the only thing that survives a resume: the log stores
    /// the image on the user message, and a session reopened tomorrow has
    /// to be able to draw it without going back to a file that may be
    /// gone.
    Image(crate::image::Attachment),
}

/// What the wrap-up nudge says on screen — one wording, drawn by the live
/// `Event::TurnBudget` arm and by `seed`'s record arm, so a resumed session
/// shows the same line the live one did.
///
/// Not the model's sentence ([`eidolon_core::session::budget_frame`]):
/// that one is a paragraph telling the model what to do with its remaining
/// calls, and this one is a line telling the operator what the harness
/// just said.
pub(crate) fn budget_note(calls_left: u32) -> String {
    let call = if calls_left == 1 { "call" } else { "calls" };
    format!("harness: {calls_left} model {call} left — the model has been told to wrap up")
}

impl Entry {
    /// What the entry says, as one string — what a yank copies and what
    /// a cluster summary counts. A tool call reads as the call and then
    /// its output, which is how it is drawn.
    pub fn text(&self) -> String {
        match self {
            Entry::User(t) | Entry::Info(t) | Entry::Alert(t) => t.clone(),
            Entry::Settle { text, .. } => text.clone(),
            // The sender is part of what a peer message says: a search
            // for a session id should find what it sent, and a yank that
            // dropped the name would paste an unattributed instruction.
            Entry::Peer { from, text, .. } => format!("{from}: {text}"),
            // The command and then its answer, as a tool call reads as the
            // call and then its output — and because that is how it is
            // drawn, so a yank pastes what the eye just read.
            Entry::Ask { commands, answers } => {
                let mut out = String::new();
                for (i, answer) in answers.iter().enumerate() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    if let Some(marked) = commands.get(i) {
                        out.push_str("! ");
                        out.push_str(&marked.command);
                        // The payload bytes are what the command sent, so
                        // they paste as they were written — what the eye
                        // just read in the box.
                        for (_, bytes) in &marked.payloads {
                            out.push('\n');
                            out.push_str(bytes);
                        }
                        out.push('\n');
                    }
                    out.push_str(answer);
                }
                out
            }
            // The same line the transcript draws: a wake is a fact about
            // this conversation's turn order, and a yank of it should
            // read as one.
            Entry::Woke { condition, outcome } => {
                format!("woke: {outcome} — waiting on {condition}")
            }
            // The label and not the bytes: a search for `screenshot`
            // should find the picture, and a yank of it should paste
            // something a person can read rather than four megabytes of
            // base64.
            Entry::Image(a) => a.label(),
            Entry::Thinking { text, .. } | Entry::Assistant { text, .. } => text.clone(),
            Entry::Tool {
                name,
                input,
                output,
                ..
            } => match output {
                Some((out, _)) => format!("{name} {input}\n{out}"),
                None => format!("{name} {input}"),
            },
        }
    }

    /// What the trace cursor calls it, for a status line.
    pub fn kind(&self) -> &'static str {
        match self {
            Entry::User(_) => "you",
            Entry::Thinking { .. } => "cogitat",
            Entry::Assistant { .. } => "reply",
            Entry::Tool { .. } => "call",
            Entry::Info(_) => "note",
            // A settle line is a note in the trace's eyes too: the cursor
            // label and the block kinds stay what they were, and the star
            // is a thing the draw does.
            Entry::Settle { .. } => "note",
            Entry::Alert(_) => "alert",
            Entry::Peer { .. } => "peer",
            Entry::Woke { .. } => "wake",
            Entry::Ask { .. } => "ask",
            Entry::Image(_) => "image",
        }
    }

    /// Does this entry belong in a **cluster** — the run of thinking and
    /// finished tool calls that folds to one line?
    ///
    /// Everything the model does between one piece of prose and the next:
    /// it thought, it called three tools, it thought again, it marked a
    /// line of its prose and the harness ran it. A call still waiting on
    /// its output is not part of one — it is drawn as itself, so a turn
    /// cancelled mid-call keeps saying so rather than being counted as
    /// work that happened — and neither is thinking still arriving. An
    /// [`Ask`](Entry::Ask) has no waiting state to exclude: it is built
    /// from the answers, so it exists only once the call has come back.
    pub fn clustered(&self) -> bool {
        // Every settled call folds the same way — reads, searches, shells,
        // and edits alike — and `space o` opens them again. Finding the
        // calls that changed something is trace's job, not a fold rule:
        // an exclusion nobody could see read as randomness.
        matches!(
            self,
            Entry::Thinking {
                streaming: false,
                ..
            } | Entry::Tool {
                output: Some(_),
                ..
            } | Entry::Ask { .. }
        )
    }
}

/// **The minibuffer**: the one-line modes, written on the prompt's own
/// bottom border.
///
/// Three of the harness's modes are not modes the *message* is in — they
/// are a second, shorter thing being typed while the message waits: the
/// `:` command line, the `/` search, the `!` dispatch utterance. Each of
/// them used to take the prompt away. The command line *was* the prompt
/// (with the draft stashed in a `draft` field and handed back afterwards
/// by a rule that had to be run after every keystroke), dispatch was a
/// mode the prompt buffer was put into, and the search line stood in the
/// prompt's screen slot with its own one-line field. Three mechanisms,
/// one consequence: a half-written message went off the screen the
/// moment you reached for any of them, and searching the transcript for
/// the very thing you were about to say meant losing what you had
/// written of it.
///
/// So they are one mechanism now, and it is a *second buffer* rather
/// than a borrowed one. The prompt body always holds the message; the
/// minibuffer is a row of the frame around it. Nothing is stashed,
/// nothing is given back, and `restore_draft` — which existed only
/// because the `:` line was the prompt — is gone with the field it
/// restored.
///
/// It is a [`crate::edit::Buffer`] and not a one-line field of its own,
/// which is the other half of the change: a `:` line, a pattern and an
/// utterance now select, yank, undo and take the readline keys exactly
/// as the prompt does, because they *are* the same code. It is held in
/// insert kind for its whole life, like a dialog's field — there is no
/// `i` to press to type a pattern.
#[derive(Clone, Debug)]
pub struct Mini {
    pub kind: MiniKind,
    pub input: crate::edit::Buffer,
}

/// Which one-line mode the minibuffer is: the row it wears in the
/// script's `modes()`, the sigil at the head of the line, and what
/// `ret` means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MiniKind {
    /// `:` — a command, resolved against [`crate::command`].
    Command,
    /// `/` or `?` — a pattern over the transcript and the draft.
    Search(crate::search::Dir),
    /// `!` — an utterance for the classifier.
    Dispatch,
}

impl MiniKind {
    /// The mode name, which is the row in `modes()` and the keymap table
    /// this kind resolves keys against.
    pub fn as_str(self) -> &'static str {
        match self {
            MiniKind::Command => "command",
            MiniKind::Search(_) => "search",
            MiniKind::Dispatch => "dispatch",
        }
    }

    /// The name back again, for `:enter_mode command` and for the mode
    /// picker — a one-line mode is entered by opening it, not by putting
    /// the prompt into it, so [`crate::modes`] cannot do this lookup.
    pub fn parse(name: &str) -> Option<MiniKind> {
        match name {
            "command" => Some(MiniKind::Command),
            "search" => Some(MiniKind::Search(crate::search::Dir::Forward)),
            "dispatch" => Some(MiniKind::Dispatch),
            _ => None,
        }
    }

    /// The character the line wears at its head. It is the mode's
    /// `sigil` in the script's table that is *drawn*; this is the same
    /// character as text, for the places a line has to be handed on
    /// whole — `on_submit`, which parses `:name arg`, and the history.
    pub fn sigil(self) -> char {
        match self {
            MiniKind::Command => ':',
            MiniKind::Search(crate::search::Dir::Forward) => '/',
            MiniKind::Search(crate::search::Dir::Backward) => '?',
            MiniKind::Dispatch => '!',
        }
    }
}

impl Mini {
    /// A minibuffer of `kind`, empty and ready to type into.
    pub fn new(kind: MiniKind) -> Self {
        Mini {
            kind,
            input: Dialog::field(),
        }
    }

    /// The text on the line, without its sigil.
    pub fn text(&self) -> &str {
        self.input.text()
    }

    /// The line as it would be typed whole, sigil included.
    pub fn line(&self) -> String {
        format!("{}{}", self.kind.sigil(), self.input.text())
    }

    /// The line as `on_submit` is handed it, which is **as it was typed
    /// before the minibuffer existed** — because a script reads it and
    /// the point of the change is that no script has to notice.
    ///
    /// The `:` line carries its colon: `parse_command` takes `:name arg`
    /// and always has, from when the command line really was the prompt
    /// with a colon in it. The other two carry what was typed and
    /// nothing else, which is also what they always carried — dispatch's
    /// `!` was drawn rather than stored even then, since the whole line
    /// is the utterance.
    pub fn submitted(&self) -> String {
        match self.kind {
            MiniKind::Command => self.line(),
            _ => self.input.text().to_string(),
        }
    }

    pub fn is(&self, kind: MiniKind) -> bool {
        self.kind == kind
    }
}

/// Where a row of the transcript sits regardless of the scroll: the
/// entry its block starts at, and the row's offset within that block.
/// What a scrolled-up view is held to while the live end grows — see
/// [`UiState::scroll_anchor`].
pub type Anchor = (usize, usize);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogKind {
    Confirm,
    Choose,
    /// A harness picker: type to filter, enter to choose. The choice
    /// feeds a command rather than answering a `UserIo` request — see
    /// [`PickAction`].
    Pick,
    /// A page of text to read and close: `:help`, `:context`, `:tree`,
    /// `:messages`. A dialog and not an [`Entry`] because it is neither
    /// the conversation nor a word about a keystroke; it is a document
    /// the operator asked for, and a document on the transcript was a
    /// screenful the search found, the cursor walked and nothing could
    /// clear. `body` is the text and `selected` the first line drawn.
    Page,
    /// A single line of free text, answered on `submit` and thrown away
    /// on `cancel` — a credential, where [`Dialog::masked`] hides what is
    /// typed. Not a [`Pick`](Self::Pick): there is nothing to choose
    /// among, only something to type once and never see again.
    Ask,
}

/// What enter does to the key behind the highlighted row. Every picker in
/// the harness is one of these two, which is why they are a closed set
/// rather than a callback.
///
/// There was a third — `RunPicked`, where the chosen key *was* a command
/// name — and it was the command palette, which is gone: the `:` line
/// completes the same registry with the same ranking, and shows you the
/// resolved command before you commit to it. The tell was that the
/// variant needed a rule of its own about arguments (a command that must
/// be given something cannot be run off a list of names, so the row came
/// back as a half-written `:` line) — which was the palette admitting it
/// could not do the job and handing off to the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickAction {
    /// Run this command with the chosen key as its argument — the model
    /// picker (`model gpt-5`), the persona picker (`persona PATH`), a
    /// resume (`resume /logs/1.eid`). `fork` is deliberately not among
    /// them any more: its rows are the transcript's rows, and trace mode
    /// is the cursor that picks one.
    Run(String),
    /// Put `:<command> <key> ` in the prompt and hand it back in insert,
    /// for the operator to finish. The session-delete picker, where the
    /// row names the log but pressing enter on the line is the
    /// confirmation.
    Edit(String),
}

pub struct Dialog {
    pub kind: DialogKind,
    pub prompt: String,
    /// (label, description)
    pub options: Vec<(String, Option<String>)>,
    /// For `Pick`: the key behind each option, same order as `options`.
    pub keys: Vec<String>,
    /// For `Pick`: each option's ordering weight, same order again, when
    /// the picker has any. Empty in a picker that does not weight. The
    /// model picker draws it: a weight is not editable from here, but
    /// seeing it is what explains why the list is ordered as it is.
    pub weights: Vec<i64>,
    pub selected: usize,
    /// The field: a filter for `Pick`, unused by the rest. A
    /// [`crate::edit::Buffer`] and not a one-line field of its
    /// own because the filter is typed like any other line. It is held in
    /// insert kind for its whole life: the dialog has no keymap of
    /// modes, so the field is a plain typing field that happens to know
    /// where its lines are. The same reasoning made the [`Mini`] one,
    /// and there is now no other kind of field in the harness.
    pub input: crate::edit::Buffer,
    pub reply: Option<tokio::sync::oneshot::Sender<Option<String>>>,
    /// For `Pick`: what enter does with the chosen key.
    pub pick: Option<PickAction>,
    /// For `Page`: the text. Wrapped when drawn, so a page reads the
    /// same at any width and the scroll is over drawn lines.
    pub body: String,
    /// For `Ask`: draw `input` as `•` per character rather than the text
    /// itself. `false` everywhere else — a filter or a page has nothing
    /// to hide.
    pub masked: bool,
}

impl Dialog {
    /// A dialog's field, ready to type into. Insert kind from the start
    /// and for good: a dialog resolves keys against its own table, so
    /// there is no `i` to press and nothing that would ever leave it.
    pub fn field() -> crate::edit::Buffer {
        let mut b = crate::edit::Buffer::default();
        b.enter(crate::edit::Mode::insert());
        b
    }

    /// Option indices matching the filter text, best first — all of them
    /// in the list's own order when the filter is empty.
    ///
    /// Ranked rather than merely filtered, and ranked the way
    /// [`crate::command::matches_in`] ranks the `:` line's candidates:
    /// what the query *prefixes* first, shortest first, then what merely
    /// contains it. Unranked, typing `mod` into the palette left `model`
    /// somewhere below every command whose description happens to say
    /// "model", which is a filter that makes the list longer to read
    /// rather than shorter — and the same three keystrokes in the model
    /// picker put `sonnet` under whatever else mentions it.
    ///
    /// The key is what leads, because the key is what enter takes: a row
    /// found by its description is still a row, but it is not what was
    /// meant by typing a name. One answer for the renderer and the key
    /// handler both, so the row under the highlight is the row that gets
    /// picked.
    pub fn visible(&self) -> Vec<usize> {
        let q = self.input.text().trim().to_lowercase();
        if q.is_empty() {
            return (0..self.options.len()).collect();
        }
        let key = |i: usize| {
            self.keys
                .get(i)
                .map(|k| k.to_lowercase())
                .unwrap_or_default()
        };
        let label = |i: usize| self.options[i].0.to_lowercase();
        let desc = |i: usize| self.options[i].1.as_deref().unwrap_or("").to_lowercase();
        // Four tiers, and the sort is stable, so within one the list
        // keeps whatever order it was built in — the model picker's
        // weights, the sessions' recency.
        let tier = |i: usize| {
            if key(i).starts_with(&q) || label(i).starts_with(&q) {
                Some(0)
            } else if key(i).contains(&q) || label(i).contains(&q) {
                Some(1)
            } else if desc(i).contains(&q) {
                Some(2)
            } else {
                None
            }
        };
        let mut found: Vec<(usize, usize, usize)> = (0..self.options.len())
            .filter_map(|i| tier(i).map(|t| (t, key(i).chars().count(), i)))
            .collect();
        found.sort_by_key(|&(t, len, _)| (t, len));
        found.into_iter().map(|(_, _, i)| i).collect()
    }
}

/// How loudly a [`Notice`] is said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Alert,
}

/// A passing word on the status line: what the last keystroke or the
/// last command came to — "no matches", "yanked 2 blocks", "no such
/// command". It is about the keystroke and not the conversation, so it
/// is not an [`Entry`]: it borrows the status line's left half for
/// [`NOTICE_TTL`] and goes, and the transcript stays what the log
/// replays. Every notice is also kept in [`UiState::notices`], which is
/// what `:messages` pages through — a word that went by while you were
/// typing can be read back.
#[derive(Clone, Debug)]
pub struct Notice {
    pub text: String,
    pub level: Level,
    pub at: Instant,
}

/// What a settled turn says: `cogitavit 22.4s · ⇣ 2.4k`. The word is
/// the pulse's table in the perfect tense, by the session's tool calls;
/// the clock is the turn's wall time when the pace record carries one.
pub fn settle_line(calls: u64, timing: Option<eidolon_core::usage::TurnTiming>, out: u64) -> String {
    let word = crate::life::done(calls as i64).to_lowercase();
    let clock = timing.map(|t| {
        let secs = t.total_ms / 1000;
        if secs >= 60 { format!(" {}m{}s", secs / 60, secs % 60) } else { format!(" {secs}.{}s", (t.total_ms / 100) % 10) }
    });
    format!("{word}{} \u{b7} \u{21e3} {}", clock.unwrap_or_default(), eidolon_core::usage::human(out))
}

/// How long a notice stays on the status line. Long enough to read a
/// sentence, short enough that the mode badge and the gauge come back
/// before they are missed.
pub const NOTICE_TTL: Duration = Duration::from_secs(5);

/// How many notices `:messages` keeps. A bound rather than the session,
/// because a notice is a byte or two per keystroke for as long as the
/// harness runs.
pub const NOTICES_KEPT: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu {
    /// The popup's title: the chord so far, or what is being completed.
    pub title: String,
    /// `(key or candidate, description)`.
    pub items: Vec<(String, String)>,
    /// Set when the popup is a list to pick from rather than a grid of
    /// keys; the index into `items` that is highlighted.
    pub selected: Option<usize>,
}

/// A drag over the drawn frame, in screen cells. The harness draws and
/// copies this itself: once mouse reporting is on, the terminal hands the
/// button press to us instead of starting a selection of its own, so a
/// plain drag can only keep working if we do the work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub from: (u16, u16),
    pub to: (u16, u16),
    /// The button is still down.
    pub dragging: bool,
}

impl Selection {
    /// The two ends in reading order: `(x, y)` sorted by row, then column.
    pub fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let (a, b) = (self.from, self.to);
        if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        }
    }
    /// A click that never moved selects nothing.
    pub fn is_empty(&self) -> bool {
        self.from == self.to
    }
}

/// The marker the inline command channel teaches the model: bang and space,
/// at the start of a line.
///
/// The channel is `eidolon-verba`'s — that crate's `command::MARKER` and
/// its scanner are the authority, and the core deliberately knows nothing
/// about the marker. This consumer reads the lines back out of the reply
/// because the answers are journaled as a record and the *command* the
/// model wrote is not: the branch has the prose and the answers, so the
/// only way to draw the two together is to re-read the reply the way the
/// channel did. Deliberately the same rule and no more of it: a line that
/// begins with the marker and has anything behind it, after whitespace.
/// `![alt](url)` has no space after the bang, an indented line is prose,
/// and a bare marker asks nothing — and, being nothing, is taken out of the
/// reply rather than drawn as a lone bang. See [`bare_marks`].
///
/// A reply made of several text blocks is the one place the two readings
/// can differ — the channel scans block by block, the transcript has the
/// blocks already joined — so a count that disagrees is handled by drawing
/// the answers bare rather than by pairing them with the wrong lines. See
/// [`UiState::answered`].
const MARKER: &str = "! ";

/// The lines of one reply that ask for a command, as the byte range each
/// occupies in `text` and the line with the marker still on it.
///
/// The range covers the line *and its newline*, so removing the marks from
/// the prose cannot leave the blank line where one used to be.
fn marker_lines(text: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        let body = line.strip_suffix('\n').unwrap_or(line);
        let body = body.strip_suffix('\r').unwrap_or(body);
        if body.starts_with(MARKER) {
            out.push((start..end, body));
        }
        start = end;
    }
    out
}

/// What is behind the marker on a line: the command, or `Some("")` for the
/// bare marker. `None` is a line the marker does not own — the two are told
/// apart by the caller that needs to, because the channel tells them apart.
fn mark_of(body: &str) -> Option<&str> {
    body.strip_prefix(MARKER).map(str::trim)
}

fn bare_marks(text: &str) -> Vec<std::ops::Range<usize>> {
    marker_lines(text)
        .into_iter()
        .filter(|(_, body)| mark_of(body) == Some(""))
        .map(|(range, _)| range)
        .collect()
}

/// One marked line and what it carried: the command, and — when the reply
/// supplied them — the payload blocks bound to its holes, in the order they
/// were written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marked {
    pub command: String,
    /// Each `(hole, bytes)` a `!!<hole> … !!` fence under this command
    /// bound, fenced at column zero as the channel requires. A block the
    /// channel itself would refuse — no command above it, a second opener
    /// before the closer — is not here; the prose keeps it.
    pub payloads: Vec<(char, String)>,
}

/// The marked lines of `text`, each with the payload fences bound to it and
/// the ranges drawn for all of it — the command's line plus every fence
/// line, whole with their newlines, so [`UiState::answered`] can lift a
/// command and its arguments out of the reply in one pass.
///
/// The TUI keeps its own copy of the channel's rule, as it does for
/// [`MARKER`]: a line `!!<letter>` at column zero opens a block for the
/// marked line above it, a line exactly `!!` closes it, and what lies
/// between is the value verbatim — including lines that look like commands
/// or fences. A fence under no marked line binds to nothing and stays
/// prose, which is what the channel does with it too; so does an
/// unterminated one, which is what streaming leaves behind until its
/// closer arrives.
/// One marked command and every range drawn for it — its own line plus
/// each fence line, whole with their newlines.
type MarkedWithRanges = (Marked, Vec<std::ops::Range<usize>>);

/// A payload block being read: the command it binds to, the hole it
/// names, the lines between the fences so far, and the fence ranges.
type OpenBlock = (usize, char, Vec<String>, Vec<std::ops::Range<usize>>);

fn marked_commands(text: &str) -> Vec<MarkedWithRanges> {
    let mut out: Vec<MarkedWithRanges> = Vec::new();
    // The block being read: the command it binds to, the hole it names, the
    // lines between the fences, and the fence ranges read so far.
    let mut open: Option<OpenBlock> = None;
    let mut at = 0usize;
    for line in text.split_inclusive('\n') {
        let end = at + line.len();
        let body = line.strip_suffix('\n').unwrap_or(line);
        let body = body.strip_suffix('\r').unwrap_or(body);
        if let Some((owner, letter, mut lines, mut ranges)) = open.take() {
            if body == "!!" {
                if let Some((marked, spans)) = out.get_mut(owner) {
                    marked.payloads.push((letter, lines.join("\n")));
                    ranges.push(at..end);
                    spans.extend(ranges);
                }
            } else {
                lines.push(body.to_string());
                ranges.push(at..end);
                open = Some((owner, letter, lines, ranges));
            }
        } else if let Some(command) = mark_of(body).filter(|c| !c.is_empty()) {
            out.push((
                Marked {
                    command: command.to_string(),
                    payloads: Vec::new(),
                },
                std::iter::once(at..end).collect(),
            ));
        } else if let Some(letter) = crate::markdown::payload_opener(body)
            && !out.is_empty()
        {
            // The opener's own line is drawn for the block it starts, so
            // its range is the first the block collects.
            open = Some((
                out.len() - 1,
                letter,
                Vec::new(),
                std::iter::once(at..end).collect(),
            ));
        }
        at = end;
    }
    out
}

/// `text` without the lines `drops` names — each a whole line with its
/// newline, so nothing is left blank in their place.
fn drop_ranges<'a>(
    text: &str,
    drops: impl IntoIterator<Item = &'a std::ops::Range<usize>>,
) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut at = 0;
    for range in drops {
        kept.push_str(&text[at..range.start]);
        at = range.end;
    }
    kept.push_str(&text[at..]);
    kept
}

/// What a reply's [`Entry::Assistant`] carries: the message's own text with
/// the bare markers taken out ([`bare_marks`]).
///
/// One function because the live arm and `seed` must agree about it, exactly
/// as [`UiState::answered`] is one method for both. A marked line *with a
/// command* is deliberately not taken out here: it leaves when the record that
/// answered it arrives, which is the only thing that can say a command ran.
pub fn reply_text(m: &eidolon_core::message::Message) -> String {
    let text = m.text();
    drop_ranges(&text, &bare_marks(&text))
}

/// The transcript entry a result belongs to: the call with that id, or — for
/// a backend that reports tool traffic without ids we can match — the
/// *oldest* still-open call, which is the order results arrive in.
///
/// It used to be the newest still-open one, which reads the same on a
/// serial turn and exactly backwards on a parallel one: three writes in
/// one assistant message drew file 1's output under file 3's call.
///
/// Order is the answer only when there is **nothing to match on**: a result
/// that names no id, or an open call that names none. Two *named* things
/// that disagree are not the same call, and pairing them by order writes one
/// call's output under another's — which is what a nested script call's
/// result did to the call that made it (the channel's resolved `mneme_rpc`
/// landing under the `vv` line that asked for it, and the line's own result
/// then dropped for want of an open entry to land on).
pub(crate) fn find_call<'a>(t: &'a mut [Entry], id: &str) -> Option<&'a mut Entry> {
    if let Some(i) = t
        .iter()
        .position(|e| matches!(e, Entry::Tool { id: i, output: None, .. } if i == id))
    {
        return t.get_mut(i);
    }
    let i = t
        .iter()
        .position(|e| matches!(e, Entry::Tool { output: None, .. }))?;
    let open_names_one = matches!(&t[i], Entry::Tool { id, .. } if !id.is_empty());
    if open_names_one && !id.is_empty() {
        return None;
    }
    t.get_mut(i)
}

/// Every `Thinking` block of a message, run together — the text a
/// [`Entry::Thinking`] carries. `RedactedThinking` is opaque and says so
/// rather than being counted as thought the operator could read.
pub fn thinking_of(m: &eidolon_core::message::Message) -> String {
    use eidolon_core::message::ContentBlock;
    let mut out = String::new();
    for b in &m.content {
        match b {
            ContentBlock::Thinking { thinking, .. } => out.push_str(thinking),
            ContentBlock::RedactedThinking { .. } => out.push_str("(redacted)"),
            _ => {}
        }
    }
    out
}

/// How old a quota reading may be before opening the usage panel asks
/// the driver for a fresh one.
pub const QUOTA_STALE_MS: u64 = 60_000;

/// The live state of one `wait_for` — the checklist the transcript draws
/// while a park is armed. **View state, deliberately journaled nowhere**:
/// the log keeps the arm (the call and its result) and the fire (the
/// `TriggerFired` record), and the samples between are the polling made
/// visible, not facts the conversation replays. So this lives only here,
/// and a resumed session draws the same wait *collapsed* — the block
/// without its live rows — because the record cannot say which leaf
/// fired, and inventing one would be a lie shaped like the truth.
pub struct WaitView {
    /// Every leaf, in the condition's own order, as the watcher last saw
    /// it. Empty until the first tick lands.
    pub rows: Vec<eidolon_core::wait::ParkRow>,
    /// When the park armed: what the countdown counts down from, and what
    /// `fired after …` counts up.
    pub armed_at: std::time::Instant,
    /// The deadline the result stated, in seconds. `0` is unknown — the
    /// footer then says `parked` without a time.
    pub deadline_s: u64,
    /// Set when the fire arrives: the outcome's own words, and how long
    /// the wait had been armed when it did.
    pub settled: Option<WaitOutcome>,
}

/// How a wait came to rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitOutcome {
    pub words: String,
    pub waited_s: Option<u64>,
}

pub struct UiState {
    pub model: String,
    pub session_path: String,
    pub cwd: String,
    pub transcript: Vec<Entry>,
    /// Which record each transcript entry came from, where one is known.
    ///
    /// Keyed by index rather than carried on the [`Entry`] itself, because
    /// the transcript's indices are *already* load-bearing and stable:
    /// `opened` and the trace cursor hold them, and the streaming path
    /// fills entries in rather than inserting precisely so nothing shifts.
    /// A map keyed on the same thing costs a lookup and leaves every
    /// `Entry` construction site alone.
    ///
    /// Sparse on purpose. A streaming entry has no record until its
    /// message is journaled, and some entries never get one — an `Info`
    /// line about a compaction is drawn from a record but is not a thing
    /// you can strike. Absent means "nothing to act on here", which is the
    /// honest answer rather than a guess.
    pub records: std::collections::HashMap<usize, RecordId>,
    /// Records struck from the replay, so the drawing can say so and a
    /// second `x` can put one back.
    pub struck: std::collections::HashSet<RecordId>,
    /// Where the operator's messages began, oldest first, until each is
    /// journaled and [`Event::UserMessage`](eidolon_core::event::Event::UserMessage)
    /// says which record it became.
    ///
    /// A queue and not a slot because a message typed during a turn waits
    /// to be journalled at the steer point, and two of them typed in a
    /// row used to leave the first with the second's index — the record
    /// then landed on the wrong entry. Entries are matched by their text
    /// on the way out, so a message the UI never drew (steered in by
    /// something other than the prompt) is drawn when journalled rather
    /// than taking a waiting entry's place.
    pub pending_user: std::collections::VecDeque<usize>,
    pub prompt: crate::edit::Buffer,
    /// The last find-char motion or bracket jump, as `(command, chars)` —
    /// what `repeat_last_motion` runs again. The chars ride in the string
    /// because a bracket jump has none.
    pub last_motion: Option<(String, String)>,
    pub dialog: Option<Dialog>,
    pub vault: crate::vault::Browser,
    pub working: bool,
    pub working_since: Option<Instant>,
    /// How full the context is, as of the last turn — `None` when
    /// nothing on the branch has said.
    ///
    /// Fed by `RecordKind`/`Event::ContextSize` and never by the settle's
    /// usage, which is the *turn's* total across every model call it made.
    /// Reading the settle here is what drew `ctx 12.92M/1.00M (1292%)` on
    /// a resumed session: one turn had run a hundred and forty records of
    /// tool loop, and the sum of its calls' inputs is not a context.
    pub context_tokens: Option<u64>,
    /// The context window each model declares, by catalog key, read once
    /// at startup.
    ///
    /// This thread has no catalog — the driver holds it — and the model
    /// under it moves three ways: the operator picks one, a resume adopts
    /// the log's, and a replayed `ModelChanged` moves it in the middle of
    /// a branch. A table answers all three in one place. Carrying the
    /// number alongside each of the three messages instead would leave
    /// the third with nothing to ask, and a gauge that keeps drawing a
    /// percentage against the *previous* model's denominator is worse
    /// than one that draws none: nothing on the line says which model the
    /// fraction is about.
    pub windows: std::collections::BTreeMap<String, u64>,
    /// The rates of every priced model, on the arrangement `windows` is
    /// on and for the same reason: the turn that settles is priced at the
    /// rates of the model it ran on, which the status line has to know
    /// without a catalog to ask.
    pub costs: std::collections::BTreeMap<String, eidolon_providers::Cost>,
    pub usage_in: u64,
    pub usage_out: u64,
    /// Characters streamed this turn, for the pulse's live output estimate.
    pub streamed: u64,
    /// Dollars, summed over the priced turns on the branch. A turn on a
    /// model whose definition declares no rates adds nothing and is not
    /// pretended to be free: the figure is drawn only once it is nonzero,
    /// and the headless summary says how many turns it leaves out.
    pub spend: f64,
    /// Every settled turn, for the usage panel: fed from the same arms
    /// the three totals above are, live and on resume, so `:usage` and
    /// the status line cannot disagree. See [`crate::usage`].
    pub ledger: crate::usage::Ledger,
    /// What the subscription behind the model in play has left, as the
    /// driver last read it — see [`crate::usage::QuotaReading`]. Not in
    /// the log and not derivable from it: the account's number, not the
    /// session's. Kept across a `:model` and across [`UiState::adopt`],
    /// and drawn only while the model in play is the provider it was
    /// read from.
    pub quota: Option<crate::usage::QuotaReading>,
    pub tools_run: u64,
    pub last_tool: Option<String>,
    pub queued: usize,
    /// Images staged for the next message, in the order they were
    /// attached.
    ///
    /// They live on the state and not in the prompt buffer because the
    /// buffer is text and these are not — a filename typed into the line
    /// would be a filename, and the whole point is that the model gets the
    /// pixels. Cleared when the message goes, and only then: a turn that
    /// fails to start leaves the attachment where the operator put it.
    pub attachments: Vec<crate::image::Attachment>,
    /// Attachments being read, and the channel they come back on.
    ///
    /// Reading one is not cheap and is not work the frame loop may do:
    /// a screenshot off a large display costs two `wl-paste` spawns, a
    /// decode, a downscale and a re-encode — about a fifth of a second,
    /// measured, and nearly all of it the resize. Done in `exec` on the UI
    /// thread that is a fifth of a second in which nothing repaints and no
    /// key is read, which is exactly what it felt like.
    ///
    /// So the work goes to a thread and the result comes back here, drained
    /// once per loop pass beside the driver's messages. The channel lives on
    /// the state rather than being plumbed through `exec` because every
    /// command already has the state and none of them has the UI channel.
    pub arrivals: (Attaching, Arrived),
    /// How many reads are in flight, so the prompt can say so.
    pub attaching: usize,
    /// How this terminal draws pixels, if it does — resolved once at
    /// startup from `[images] inline` and the environment.
    pub image_protocol: crate::image::Protocol,
    /// The terminal's cell size in pixels, for turning an image's aspect
    /// ratio into a number of rows. Read once; a terminal that is resized
    /// keeps its cell size, which is the thing this is for.
    pub cell_size: (u16, u16),
    /// The images painted on the last frame, so the next one can tell
    /// which of them have not moved.
    ///
    /// Pixels are painted *over* cells, so they survive only as long as
    /// ratatui leaves those cells alone. An image in the same place with
    /// the same content is left alone (the cells are marked skip and the
    /// picture is not redrawn, which is what stops it flickering under a
    /// streaming reply); anything else is repainted by the frame and then
    /// drawn again. See [`crate::render::Renderer::images`].
    pub drawn_images: Vec<crate::image::Placement>,
    /// Lines scrolled up from the bottom; 0 = following.
    pub scroll_up: usize,
    /// Where the bottom row of the last-drawn viewport sits in the
    /// transcript. Recorded by every frame that drew scrolled up, so
    /// the next frame can hold the **content** still while the live
    /// end grows under it — `scroll_up` counts rows from the bottom
    /// *edge*, and an edge that moves takes the window with it. `None`
    /// while following, where every new row is meant to arrive.
    pub scroll_anchor: Option<Anchor>,
    /// The transcript viewport as last drawn, for the half- and full-page
    /// scrolls — the script places the widget, so only the renderer knows.
    pub view_height: usize,
    /// Its width, which the search needs for the same reason: the lines
    /// an entry takes depend on what it was wrapped to.
    pub view_width: usize,
    /// How much of the tool traffic the transcript draws (`space o`).
    pub detail: Detail,
    /// When a finished run folds into its summary line — mid-turn, or
    /// once at the settle. `[transcript] fold`; see [`Fold`].
    pub fold: Fold,
    /// The transcript's own cursor, live while the prompt is in trace
    /// mode. It is kept across visits rather than rebuilt, so leaving
    /// the mode and coming back lands where you were.
    pub trace: crate::trace::Trace,
    /// The trace cursor's filter, set by `:trace [WHAT]` and cleared the
    /// same way or when the mode is left. `None` walks every block; a
    /// word walks only the blocks that carry it — a tool's name
    /// (`edit`), an entry's kind (`call`, `reply`), or `*` for the
    /// calls that changed something.
    pub trace_filter: Option<String>,
    /// Where the assistant message in flight began in the transcript.
    /// The thinking of a message arrives as deltas and again, in full,
    /// on the settled message; this is how the second finds the entry
    /// the first opened, without shifting an index anything else holds.
    pub msg_start: usize,
    /// Which dispatch surface the prompt is aimed at while it is in
    /// dispatch mode. Sticky, and empty when no
    /// `[[verba.bundles]]` is configured — the mode then refuses to open
    /// rather than opening onto nothing. Seeded in [`crate::app::run`],
    /// which is where the palette is known.
    pub dispatch: crate::dispatch::Dispatch,
    /// The chord typed so far, for the status line ("" when idle).
    pub pending: String,
    /// Tab-completion in progress on the command line: the stem that was
    /// typed, and which candidate is showing. The stem is kept because the
    /// line is rewritten as you cycle — completing `:c` to `:cancel` would
    /// otherwise narrow the candidates to one and strand you there.
    pub completing: Option<(String, usize)>,
    /// The popup over the prompt. Two things want it and they are the same
    /// thing — "here is what can come next, with descriptions" — so they
    /// share it: an open chord (no `selected`, drawn as a grid) and command
    /// completion (a `selected` row, drawn as a list). They are never both
    /// live, and sharing means a hand-written `ui.rn` that places `menu()`
    /// gets completion without knowing it exists.
    pub menu: Option<Menu>,
    /// The live search, if there is one. It outlives its line: the line
    /// comes down on `ret`, and what stays is what `n` repeats and what
    /// the transcript keeps highlighting until `esc` puts it away.
    pub search: Option<crate::search::Search>,
    /// An open set of letter tags. It owns the keyboard while it is up —
    /// a tag is one or two keys and then it is gone, so there is no mode
    /// to be stuck in.
    pub jump: Option<crate::jump::Jump>,
    /// The transcript entries the last frame actually drew. The hit tags
    /// are built from this: a tag you cannot see is not worth a letter.
    pub view_entries: std::ops::Range<usize>,
    /// Folded runs the operator has opened, by the entry each run starts
    /// at. Still a *view* and not bookkeeping on the transcript — the
    /// same kind of thing as `detail`, just aimed at one run rather than
    /// all of them — which is what a search needs when the match it
    /// found is inside a box that draws a summary instead of the text.
    pub opened: Vec<usize>,
    /// The drag in progress, or the last one, until something moves under it.
    pub selection: Option<Selection>,
    /// When the selection last reached the clipboard, for the status line.
    pub copied_at: Option<Instant>,
    /// The word on the status line, while there is one. See [`Notice`].
    pub notice: Option<Notice>,
    /// Every notice said so far, oldest first, for `:messages`.
    pub notices: Vec<(Level, String)>,
    /// The terminal, as of the last frame. A page dialog scrolls in
    /// drawn lines and the drawn lines depend on the width, so the key
    /// handler needs the same answer the renderer had.
    pub screen: (u16, u16),
    pub tick: u64,
    /// The pulse's Life strip: stepped on the tick, poked by tool calls.
    pub life: crate::life::World,
    pub quit: bool,
    pub script_error: Option<String>,
    /// The script's `modes()`: what each mode is called, what it wears,
    /// and — for the editing ones — what kind of mode it is. Read by
    /// [`crate::render::mode_style`] and by `enter_mode`.
    pub modes: crate::modes::ModeTable,
    /// The script's `theme()`: the colours Rust paints on its own account.
    pub theme: crate::theme::Theme,
    /// The script's `labels()`: what tools are called on the transcript,
    /// as against their wire names. Cosmetic by construction — see
    /// [`crate::labels`].
    pub labels: crate::labels::Labels,
    /// The `wait_for` calls whose parks are drawn live, keyed by the
    /// call's `tool_use` id — the id the entry carries, the fire names,
    /// and the ticks repeat. See [`WaitView`].
    pub waits: std::collections::HashMap<String, WaitView>,
    /// The script's `wait_tags()`: the short words (or glyphs) and
    /// colours a parked wait's checklist names its condition kinds by.
    pub wait_tags: crate::labels::WaitTags,
    /// `reload_ui` was asked for. The UI loop owns the script and the
    /// keymap, so it is the one that acts on this; a command can only ask.
    pub reload: bool,
    /// `edit_prompt` was asked for, for the same reason `reload` is a
    /// flag: the terminal and its modes are the UI loop's, and an editor
    /// has to be handed the whole screen — [`crate::editor`] does the
    /// standing down and standing up, from the loop, with the terminal
    /// in hand. A command can only ask.
    pub edit_draft: bool,
    /// The script's `commands()`, validated: bound, completed, listed and
    /// run beside the built-ins.
    pub script_commands: Vec<crate::command::Scripted>,
    /// The keymap as loaded, for `help` — the resolver owns the live copy.
    pub keymap: serde_json::Value,
    /// `[terminal]`: what the `T` chords open another window with.
    ///
    /// Here for the reason [`image_protocol`](Self::image_protocol) is —
    /// `crate::app::exec` sees the state and not the options — and empty
    /// by default, which means *work it out on the first press* rather
    /// than *disabled*. See [`crate::launch`].
    pub launcher: crate::launch::Launcher,
    /// The blanket-yes switch, shared with the policy hook running in the
    /// dispatcher — so `:yolo` is a store to an atomic and the badge is a
    /// load from it, with no round trip to the driver in either
    /// direction. `None` when the classifier is off: there is nothing to
    /// answer for, and the command says so.
    ///
    /// A *posture*, like [`Detail`] and the trace cursor, and like them it
    /// is not journaled — a session reopened tomorrow comes back gated.
    pub yolo: Option<eidolon_core::yolo::Switch>,
    /// Every model the catalog knows, as `(key, name)`.
    ///
    /// Here rather than asked for, because the `:` line's popup is
    /// rebuilt on every keystroke and the catalog is on the other side of
    /// the channel — see [`crate::command::Fill`]. It is the same
    /// arrangement [`windows`](Self::windows) is on, and taken from the
    /// same walk over definitions already in memory, so it costs the
    /// first frame nothing.
    pub model_keys: Vec<(String, String)>,
    /// Every provider that declares a `token_secret`, by name — for
    /// completing `:login`. [`model_keys`](Self::model_keys)' arrangement,
    /// for the same reason.
    pub provider_keys: Vec<String>,
    /// The vault's personas as `(name, path)`, for completing `:persona`.
    ///
    /// Here for [`model_keys`](Self::model_keys)' reason and one more: this
    /// list is behind a *network* call, not just behind the driver's lock,
    /// so it arrives when the driver's warm task has it and is empty until
    /// then. Empty is a fine resting state — `:persona` with no argument
    /// asks the vault itself and does not consult this at all.
    pub persona_keys: Vec<(String, String)>,

    /// The persona this branch is wearing, as pinned — what the status
    /// line draws. Derived in [`UiState::seed`] from the branch's
    /// `PersonaPinned` records, like [`model`](Self::model), so resume and
    /// fork need no separate message.
    pub persona: Option<String>,
    /// Where the session logs live, for completing `:resume` and
    /// `:delete_session` off their names.
    pub sessions_dir: std::path::PathBuf,
    /// The one-line mode in front of the prompt, if any: the `:`
    /// command line, the `/` search, the `!` dispatch utterance. See
    /// [`Mini`] — it is a second buffer drawn on the prompt's bottom
    /// border, so opening one no longer costs the operator the message
    /// they were halfway through writing.
    /// Where the transcript was drawn last frame, so `gw` and `gb` can
    /// tag what is on the screen. Recorded by the draw and read by the
    /// key that opens a tag set — the same way `view_entries` is.
    pub transcript_rect: ratatui::layout::Rect,
    pub mini: Option<Mini>,
    /// What has been typed on each one-line mode, oldest first, keyed by
    /// [`MiniKind::as_str`].
    ///
    /// It lives here rather than in the [`Mini`] because the buffer is
    /// built fresh every time a line opens and a history that died with
    /// the line would make `up` mean nothing on the surface that most
    /// wants it — a dispatch utterance the classifier abstained on is
    /// worth getting back with one key. Three histories and not one:
    /// commands, patterns and utterances are three vocabularies, and the
    /// old arrangement had all of them in the *message* history, because
    /// the `:` line was the prompt.
    pub mini_history: std::collections::HashMap<&'static str, Vec<String>>,
    /// The files pinned into every turn, for completing `:unpin`.
    ///
    /// Session state, so it is the driver's to know; it is kept here the
    /// same way and for the same reason, refreshed by [`UiMsg::Pins`]
    /// whenever a pin is added or dropped.
    ///
    /// [`UiMsg::Pins`]: crate::app::UiMsg::Pins
    pub pins: Vec<String>,
}

impl UiState {
    /// The command line's contents — the `:` line's text, without the
    /// colon — when the minibuffer is one.
    ///
    /// It used to be a question about the *prompt's* text ("a single
    /// line beginning with `:`"), because the command line was the
    /// prompt; it is now a question about which minibuffer is up, which
    /// is the same question asked where the answer lives. What that
    /// buys is the thing the old spelling could not have: a `:` typed
    /// into a message is a colon, always, with no rule about which
    /// typing modes are exempt.
    pub fn command_line(&self) -> Option<&str> {
        self.mini
            .as_ref()
            .filter(|m| m.is(MiniKind::Command))
            .map(Mini::text)
    }

    /// The dispatch utterance being typed, if one is.
    pub fn dispatch_line(&self) -> Option<&str> {
        self.mini
            .as_ref()
            .filter(|m| m.is(MiniKind::Dispatch))
            .map(Mini::text)
    }

    /// The direction of the search line, while it is up.
    pub fn search_dir(&self) -> Option<crate::search::Dir> {
        match self.mini.as_ref().map(|m| m.kind) {
            Some(MiniKind::Search(d)) => Some(d),
            _ => None,
        }
    }

    /// The prompt is in trace mode, so the transcript has a cursor. The
    /// mode is the single source of truth for that: the cursor itself is
    /// always present, and always stale until the mode places it.
    pub fn tracing(&self) -> bool {
        self.prompt.mode.is("trace")
    }

    /// The search line is up and taking keys. A search that has been
    /// accepted is still *live* — it highlights, and `n` repeats it — but
    /// its line is down, which is the difference this asks about. The
    /// line being up *is* the minibuffer being a search, so there is no
    /// second flag to keep in step with it.
    pub fn searching(&self) -> bool {
        self.search_dir().is_some()
    }

    /// The mode to the hands: what the keymap is indexed by and what the
    /// frame and the badge say. A dialog owns the keyboard while it is
    /// up and is named by its kind; then the minibuffer, named by its
    /// [`MiniKind`]; then the prompt's own mode. Every name here is a
    /// row in the script's `modes()`.
    pub fn mode_name(&self) -> &str {
        match &self.dialog {
            Some(d) => match d.kind {
                DialogKind::Confirm => "confirm",
                DialogKind::Choose => "choose",
                DialogKind::Pick => "pick",
                DialogKind::Page => "page",
                DialogKind::Ask => "ask",
            },
            None => match &self.mini {
                Some(m) => m.kind.as_str(),
                None if self.vault.active && !self.tracing() && !self.prompt.mode.typing() && !self.prompt.mode.is("select") => "vault",
                None => self.prompt.mode.as_str(),
            },
        }
    }

    pub fn new(model: String, session_path: String, cwd: String) -> Self {
        UiState {
            model,
            session_path,
            cwd,
            transcript: Vec::new(),
            records: Default::default(),
            struck: Default::default(),
            pending_user: Default::default(),
            prompt: crate::edit::Buffer::default(),
            last_motion: None,
            dialog: None,
            vault: Default::default(),
            working: false,
            working_since: None,
            context_tokens: None,
            windows: Default::default(),
            costs: Default::default(),
            usage_in: 0,
            usage_out: 0,
            streamed: 0,
            spend: 0.0,
            ledger: Default::default(),
            quota: None,
            tools_run: 0,
            last_tool: None,
            queued: 0,
            attachments: Vec::new(),
            arrivals: std::sync::mpsc::channel(),
            attaching: 0,
            // Off until `crate::app::run` resolves the setting: a test or
            // a headless consumer building a state has no terminal to ask.
            image_protocol: crate::image::Protocol::Off,
            cell_size: (8, 16),
            drawn_images: Vec::new(),
            scroll_up: 0,
            scroll_anchor: None,
            view_height: 0,
            view_width: 0,
            detail: Detail::default(),
            fold: Fold::default(),
            trace: crate::trace::Trace::default(),
            trace_filter: None,
            msg_start: 0,
            dispatch: crate::dispatch::Dispatch::default(),
            pending: String::new(),
            completing: None,
            menu: None,
            search: None,
            jump: None,
            view_entries: 0..0,
            opened: Vec::new(),
            selection: None,
            copied_at: None,
            notice: None,
            notices: Vec::new(),
            screen: (80, 24),
            tick: 0,
            life: crate::life::World::new(12),
            quit: false,
            script_error: None,
            modes: crate::modes::ModeTable::default(),
            theme: crate::theme::Theme::default(),
            labels: crate::labels::Labels::default(),
            wait_tags: crate::labels::WaitTags::default(),
            waits: Default::default(),
            reload: false,
            edit_draft: false,
            script_commands: Vec::new(),
            keymap: serde_json::Value::Null,
            // Empty is "nothing was configured", and the scan that
            // answers it happens on the first `T` rather than here: see
            // the launch budget in `AGENTS.md`.
            launcher: crate::launch::Launcher::default(),
            // No gate until `crate::app::run` is handed one.
            yolo: None,
            model_keys: Vec::new(),
            provider_keys: Vec::new(),
            persona_keys: Vec::new(),
            persona: None,
            transcript_rect: ratatui::layout::Rect::default(),
            mini: None,
            mini_history: Default::default(),
            sessions_dir: std::path::PathBuf::new(),
            pins: Vec::new(),
        }
    }

    /// The window the model in play declares, if it declares one.
    ///
    /// `None` is "nobody said" and not "no room": a gateway's discovered
    /// model list carries no such number, so most keys are absent, and
    /// the honest thing to draw for them is the count alone.
    pub fn context_limit(&self) -> Option<u64> {
        self.windows.get(&self.model).copied()
    }

    /// Count a settled turn: the tokens always, the dollars when the
    /// model in play is one the catalog prices, and a row in the ledger
    /// either way — priced here, at the rates of the model in play, or
    /// marked unpriced. Live and on resume alike, so the panel and the
    /// status line cannot disagree about what a session cost.
    pub fn settled(
        &mut self,
        usage: &eidolon_core::Usage,
        stop: eidolon_core::StopReason,
        timing: Option<eidolon_core::usage::TurnTiming>,
        ts_ms: u64,
    ) {
        self.usage_in += usage.total_input();
        self.usage_out += usage.output_tokens;
        let cost = self.costs.get(&self.model).map(|c| c.price(usage));
        if let Some(cost) = cost {
            self.spend += cost;
        }
        self.ledger
            .settled(&self.model, *usage, stop, cost, timing, ts_ms);
        // The settle line under the reply, from the records, so a resume
        // draws it too. Not for a cancelled turn, and not for a
        // compaction's own settle, which has no reply above it.
        let under_a_reply = matches!(self.transcript.last(), Some(Entry::Assistant { .. } | Entry::Tool { .. } | Entry::Thinking { .. }));
        if stop != eidolon_core::StopReason::Cancelled && under_a_reply {
            // How many stars are already on the transcript, which is how
            // many turns have settled: read off the transcript rather
            // than counted beside it, so a resume that rebuilds the
            // transcript from the records rebuilds the stars with it.
            let turn = self
                .transcript
                .iter()
                .filter(|e| matches!(e, Entry::Settle { .. }))
                .count();
            self.transcript.push(Entry::Settle {
                turn,
                text: settle_line(self.tools_run, timing, usage.output_tokens),
            });
        }
    }

    /// The quota reading, when it is about the model in play's provider.
    /// A reading from another account is held and not shown, so that a
    /// `:model` from z.ai to DeepSeek cannot draw z.ai's window under
    /// DeepSeek's name — and a switch back shows it again at once, with
    /// its age, while the driver reads a fresh one.
    pub fn quota(&self) -> Option<&crate::usage::QuotaReading> {
        self.quota
            .as_ref()
            .filter(|q| Some(q.provider.as_str()) == crate::usage::provider_of(&self.model))
    }

    /// Whether the reading is old enough that opening the panel should
    /// ask for another: absent, or taken more than a minute ago. The
    /// driver refreshes after every turn, so this is for the reading that
    /// went stale while nothing ran — the same key spent elsewhere.
    pub fn quota_is_stale(&self, now_ms: u64) -> bool {
        match self.quota() {
            None => true,
            Some(q) => now_ms.saturating_sub(q.read_ms) > QUOTA_STALE_MS,
        }
    }

    /// The usage panel's page, from the ledger and this moment, at the
    /// width a page is drawn at on this screen — so its tables fit the
    /// frame rather than wrapping in it.
    pub fn usage_page(&self) -> String {
        let (width, _) = crate::render::page_layout(self.screen);
        let now = crate::usage::Now {
            model: &self.model,
            rates: self.costs.get(&self.model),
            context_tokens: self.context_tokens,
            context_limit: self.context_limit(),
            quota: self.quota(),
            working_ms: self.working_since.map(|t| t.elapsed().as_millis() as u64),
            now_ms: crate::usage::now_ms(),
            width,
        };
        crate::usage::report(&self.ledger, &now)
    }

    /// Move the whole UI to a different session log.
    ///
    /// Everything drawn is *about* the transcript, so everything drawn has
    /// to go: the counters are that session's totals, a search's hits and
    /// a tag set's targets are indices into the entries this is about to
    /// replace, and a screen selection is anchored to cells whose text
    /// changes underneath it. Keeping any of them would leave the picker
    /// looking like it worked and the next `n` scrolling somewhere that no
    /// longer exists.
    ///
    /// What survives is what belongs to the operator rather than to the
    /// session: the prompt they were drafting, their history, and the
    /// [`Detail`] level, which is a preference and not a pointer. So does
    /// [`UiState::windows`], which belongs to neither — it is the catalog
    /// the harness launched with, and the adopted log's own model is
    /// looked up in it like any other.
    pub fn adopt(&mut self, path: String, cwd: String, model: String, branch: &[Record]) {
        self.session_path = path;
        self.cwd = cwd;
        self.model = model;
        self.transcript.clear();
        crate::vault::close(self);
        self.vault.trace_focus = None;
        // Indices into a transcript that is being replaced point at
        // nothing; the same reasoning that clears the search and the tags.
        self.records.clear();
        self.struck.clear();
        self.pending_user.clear();
        // Unknown until the adopted branch says otherwise — `seed` reads
        // it back from the log's own `ContextSize`, and a log written
        // before that record existed leaves it unknown, which is the
        // truth about it.
        self.context_tokens = None;
        self.usage_in = 0;
        self.usage_out = 0;
        self.spend = 0.0;
        self.ledger = Default::default();
        self.tools_run = 0;
        self.last_tool = None;
        self.queued = 0;
        self.scroll_up = 0;
        // The anchor named rows of a transcript that is being replaced;
        // it goes the way the search and the tags do.
        self.scroll_anchor = None;
        self.search = None;
        // A search line pointing at a transcript that no longer exists
        // goes with the search; so does a `:` line, whose completions
        // were the old session's. The prompt's draft is the operator's
        // and stays.
        self.mini = None;
        self.jump = None;
        self.view_entries = 0..0;
        // An opened fold names an entry of the log being left behind, so
        // it is as stale as a search hit and goes the same way — and so
        // does the trace cursor, which is an index into the transcript
        // about to be replaced.
        self.opened.clear();
        self.trace = crate::trace::Trace::default();
        self.msg_start = 0;
        self.selection = None;
        self.copied_at = None;
        self.seed(&branch.iter().collect::<Vec<_>>());
    }

    /// Seed the transcript from a resumed session's branch.
    /// The cell size to lay images out against, or `None` where the
    /// terminal draws no pixels and an image reserves no rows.
    ///
    /// One question asked in one place: the layout walk reserves rows,
    /// the frame paints over them, and if those two disagreed about
    /// whether there are any the picture would land on the caption.
    pub fn pixel_cell(&self) -> Option<(u16, u16)> {
        (self.image_protocol != crate::image::Protocol::Off).then_some(self.cell_size)
    }

    pub fn seed(&mut self, branch: &[&Record]) {
        // The pace a settle belongs to, held between the two records that
        // carry it: `TurnPace` is written immediately before its
        // `TurnSettled`, and the ledger wants both at once.
        let mut pending_pace: Option<eidolon_core::usage::TurnTiming> = None;
        for r in branch {
            // Where this record's entries begin, so they can be claimed
            // for it below. A resumed session gets the same entry→record
            // map the live path builds, which is what lets the trace
            // cursor strike a row in a session opened tomorrow.
            let from = self.transcript.len();
            match &r.kind {
                RecordKind::UserMessage(m) => {
                    self.ledger.began(r.ts_ms);
                    // Images first, as they are sent and as the live path
                    // draws them: the picture, then the caption naming it,
                    // then the words that were said about it.
                    for a in m
                        .content
                        .iter()
                        .filter_map(crate::image::Attachment::from_block)
                    {
                        self.transcript.push(Entry::Image(a));
                    }
                    let t = m.text();
                    if !t.is_empty() {
                        self.transcript.push(Entry::User(t));
                    }
                }
                RecordKind::AssistantMessage(m) => {
                    self.ledger.called(r.ts_ms);
                    // Thinking is journaled — the signature has to be
                    // replayed verbatim — so a resumed session can draw
                    // it, and used to draw nothing at all.
                    let thinking = thinking_of(m);
                    if !thinking.is_empty() {
                        self.transcript.push(Entry::Thinking {
                            text: thinking,
                            streaming: false,
                        });
                    }
                    // The reply's own words, minus the bare markers the
                    // channel's syntax leaves behind — one function for this
                    // and the live arm, so a resumed frame draws what the
                    // live one did. See [`reply_text`].
                    let text = reply_text(m);
                    if !text.is_empty() {
                        self.transcript.push(Entry::Assistant {
                            text,
                            streaming: false,
                        });
                    }
                    for (id, name, input) in m.tool_uses() {
                        self.transcript.push(Entry::Tool {
                            id: id.to_string(),
                            name: name.to_string(),
                            input: input.0.clone(),
                            output: None,
                        });
                    }
                }
                // A call the operator made: the words they typed, then the
                // call they were read as, which is exactly what the live
                // path draws — so a resumed dispatch folds identically and
                // needs no bookkeeping of its own.
                RecordKind::UserToolCall {
                    tool_use_id,
                    name,
                    input,
                    utterance,
                } => {
                    self.ledger.began(r.ts_ms);
                    if let Some(u) = utterance
                        .as_deref()
                        .map(str::trim)
                        .filter(|u| !u.is_empty())
                    {
                        self.transcript.push(Entry::User(u.to_string()));
                    }
                    self.transcript.push(Entry::Tool {
                        id: tool_use_id.clone(),
                        name: name.clone(),
                        input: input.0.clone(),
                        output: None,
                    });
                }
                // A neighbour's message, drawn as itself. It was
                // journaled at a boundary or at a mid-turn steer point, so
                // it seeds in exactly the place it appeared live.
                RecordKind::PeerMessage {
                    from,
                    channel,
                    text,
                    ..
                } => {
                    self.transcript.push(Entry::Peer {
                        from: from.clone(),
                        channel: channel.is_some(),
                        text: text.clone(),
                    });
                }
                // An outside caller's message — `eidolon send` — in the
                // peer's slot under its sender's name: what must never be
                // ambiguous is who said it, and the sender here is a
                // tool, not this operator and not a session.
                RecordKind::ExternalMessage { from, channel, text } => {
                    self.transcript.push(Entry::Peer {
                        from: from.clone(),
                        channel: channel.is_some(),
                        text: text.clone(),
                    });
                }
                RecordKind::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    if let Some(Entry::Tool { name, output, .. }) =
                        find_call(&mut self.transcript, tool_use_id)
                    {
                        *output = Some((content.clone(), *is_error));
                        let name = name.clone();
                        self.ledger.tool(&name, *is_error, r.ts_ms);
                    }
                    // The entry belongs to the result, not to the message
                    // that asked — the same claim the live path makes, for
                    // the same reason.
                    if let Some(i) = self
                        .transcript
                        .iter()
                        .position(|e| matches!(e, Entry::Tool { id, .. } if id == tool_use_id))
                    {
                        self.records.insert(i, r.id);
                    }
                }
                // Not drawn: what it does is change how the records
                // already on screen are drawn, which the set below does.
                RecordKind::Excluded { target, excluded } => {
                    if *excluded {
                        self.struck.insert(*target);
                    } else {
                        self.struck.remove(target);
                    }
                }
                RecordKind::ContextSize { tokens } => {
                    self.context_tokens = Some(*tokens);
                    self.ledger.context(*tokens);
                }
                // Held, not drawn: it belongs to the settle that follows it.
                RecordKind::TurnPace { timing, .. } => pending_pace = Some(*timing),
                RecordKind::TurnSettled { stop_reason, usage } => {
                    // The pace written just before this one, when the turn
                    // was timed at all — a log from before that record
                    // existed, or a driver that owns its own loop, resumes
                    // with `None` and says "not measured" rather than zero.
                    let pace = pending_pace.take();
                    self.settled(usage, *stop_reason, pace, r.ts_ms);
                    // The log knows why the turn ended, so a resumed session
                    // shows the same truncation notice the live one did.
                    if let Some(note) = eidolon_core::usage::stop_note(*stop_reason) {
                        self.transcript.push(Entry::Alert(note.into()));
                    }
                }
                RecordKind::Compacted {
                    replaced_messages, ..
                } => {
                    self.transcript.push(Entry::Info(format!(
                        "compacted {replaced_messages} messages"
                    )));
                    // The context after a compaction is the summary, not
                    // zero. A log written by the current core carries the
                    // measured size in the `ContextSize` that follows this
                    // record; a log from before it does not, and says so
                    // (unknown) rather than claiming an empty context.
                    self.context_tokens = None;
                    self.ledger.compacted();
                }
                // The gate's own answer, redrawn on resume exactly as it
                // was drawn live — which for every outcome but the judge's
                // is nothing at all, because the transcript already shows
                // what happened. See `policy::verdict_note`.
                RecordKind::PolicyVerdict {
                    tool,
                    outcome,
                    note,
                    ..
                } => {
                    self.ledger.verdict(outcome);
                    if let Some(line) =
                        eidolon_core::policy::verdict_note(outcome, tool, note.as_deref())
                    {
                        self.transcript.push(Entry::Alert(line));
                    }
                }
                RecordKind::ModelChanged { model } => self.model = model.clone(),
                // The worn persona, derived from the branch exactly as the
                // model is — so a resumed session, and a fork below the
                // pin, both come back in the right voice with no
                // bookkeeping of their own. Latest word wins; empty is
                // the record's way of taking it off.
                RecordKind::PersonaPinned { persona } => {
                    self.persona = (!persona.trim().is_empty()).then(|| persona.clone())
                }
                // What the turn had already spent when it was stopped,
                // journaled just before the `Cancelled` it explains.
                // Handled here rather than folded into that arm because
                // it is written only when there was a cost to write: a
                // cancel on the Claude CLI knows its call count and not
                // its usage, and an absent record is how that says so.
                RecordKind::TurnSpend { usage, .. } => {
                    self.settled(usage, eidolon_core::StopReason::Cancelled, None, r.ts_ms)
                }
                RecordKind::Cancelled => {
                    self.transcript.push(Entry::Info("cancelled".into()));
                    self.ledger.cancelled();
                }
                // The wrap-up nudge, drawn as itself for the same reason a
                // peer's message is: it happened, the model was told, and a
                // resumed session should show that it was. `budget_note` is
                // the one wording — the live event arm draws the same line.
                RecordKind::TurnBudget { calls_left } => {
                    self.transcript.push(Entry::Info(budget_note(*calls_left)));
                }
                // The harness's answers to the model's marked commands:
                // taken out of the reply that wrote them and drawn as the
                // calls they became, exactly as the live event draws them.
                RecordKind::CommandResults { lines } => self.answered(lines, r.id),
                // A park this session armed has resolved, and the wake it
                // bought is on the branch: drawn as its own kind, the
                // same line the live event drew. It used to draw nothing
                // at all here, which made a resumed session miss the
                // very thing that started its last turn.
                RecordKind::TriggerFired {
                    condition, outcome, ..
                } => self.transcript.push(Entry::Woke {
                    condition: condition.clone(),
                    outcome: outcome.clone(),
                }),
                _ => {}
            }
            self.remember(from, r.id);
        }
        // Nothing is in flight in a log that has been read to the end.
        self.msg_start = self.transcript.len();
    }

    /// Say something about the last keystroke or command, quietly. On
    /// the status line for [`NOTICE_TTL`], and in `:messages` after.
    /// Everything pushed since `from` came from `record`.
    ///
    /// `or_insert`, never overwrite: a tool entry inside an assistant
    /// message is claimed here by the message, and then claimed *again* by
    /// its own `ToolResult` when that lands — and the result is the one
    /// worth striking, because withholding an output is the common case
    /// and deleting the call that produced it is not.
    pub fn remember(&mut self, from: usize, record: RecordId) {
        for i in from..self.transcript.len() {
            self.records.entry(i).or_insert(record);
        }
    }

    /// The harness answered the marked lines of the reply it just drew: take
    /// them out of that reply and draw them as the calls they became.
    ///
    /// One method and not two, because the live path and a resume must draw
    /// the same thing: the live arm calls this when `Event::CommandResults`
    /// arrives, `seed` calls it on the same record, and the record is the
    /// only input either has — so the reply is found by *shape* (the last
    /// prose on the transcript is the one that asked; the answers are
    /// written where that reply's tool results would land, so nothing else
    /// has been drawn since) rather than by an id the log does not carry.
    ///
    /// The marks come out of the prose only when their count matches the
    /// answers'. A count that disagrees is a run cancelled between two
    /// commands, or a reply the channel scanned block by block and the
    /// transcript holds joined — and an answer drawn under a command that
    /// did not ask for it would be a lie about which line caused what,
    /// where an answer drawn bare is merely less tidy. Nothing is lost
    /// either way: unpaired marks stay in the prose, which is where a line
    /// nothing ran belongs.
    ///
    /// A reply that was *nothing but* marks leaves no prose behind, and
    /// then the entry becomes the ask rather than standing empty beside it:
    /// the model asking the vault and saying nothing else is a real shape,
    /// and an entry drawn as no lines at all is a block the trace cursor
    /// can land on and show nothing for.
    pub fn answered(&mut self, answers: &[String], record: RecordId) {
        if answers.is_empty() {
            return;
        }
        let from = self.transcript.len();
        let reply = self
            .transcript
            .iter()
            .rposition(|e| matches!(e, Entry::Assistant { .. }));
        let mut commands: Vec<Marked> = Vec::new();
        let mut emptied = None;
        if let Some(i) = reply {
            let found = match &self.transcript[i] {
                Entry::Assistant { text, .. } => marked_commands(text),
                _ => Vec::new(),
            };
            if found.len() == answers.len() {
                commands = found.iter().map(|(m, _)| m.clone()).collect();
                if let Entry::Assistant { text, .. } = &mut self.transcript[i] {
                    let kept = drop_ranges(
                        text,
                        found.iter().flat_map(|(_, ranges)| ranges.iter()),
                    );
                    if kept.is_empty() {
                        emptied = Some(i);
                    }
                    *text = kept;
                }
            }
        }
        let ask = Entry::Ask {
            commands,
            answers: answers.to_vec(),
        };
        match emptied {
            // The entry was claimed by the message that asked; it is the
            // answers' now — the same claim a tool line makes for its
            // result rather than for the call that asked for it.
            Some(i) => {
                self.transcript[i] = ask;
                self.records.insert(i, record);
            }
            None => {
                self.transcript.push(ask);
                self.remember(from, record);
            }
        }
    }

    /// A `wait_for` armed: the result said so, and it named a deadline.
    /// The rows fill in as the watcher's ticks arrive; the block draws
    /// from this and the call's own input, never the other way round.
    pub fn wait_armed(&mut self, call_id: &str, deadline_s: u64) {
        let view = self.waits.entry(call_id.to_string()).or_insert(WaitView {
            rows: Vec::new(),
            armed_at: std::time::Instant::now(),
            deadline_s,
            settled: None,
        });
        view.armed_at = std::time::Instant::now();
        view.deadline_s = deadline_s;
    }

    /// The watcher's sample moved a row: the checklist ticks.
    pub fn wait_tick(&mut self, tick: eidolon_core::wait::ParkTick) {
        self.waits
            .entry(tick.call_id)
            .or_insert(WaitView {
                rows: Vec::new(),
                armed_at: std::time::Instant::now(),
                deadline_s: 0,
                settled: None,
            })
            .rows = tick.rows;
    }

    /// The fire arrived: the block comes to rest, keeping its rows so the
    /// one that fired stays checked on screen.
    pub fn wait_settled(&mut self, call_id: &str, words: &str) {
        let view = self.waits.entry(call_id.to_string()).or_insert(WaitView {
            rows: Vec::new(),
            armed_at: std::time::Instant::now(),
            deadline_s: 0,
            settled: None,
        });
        view.settled = Some(WaitOutcome {
            words: words.to_string(),
            waited_s: Some(view.armed_at.elapsed().as_secs()),
        });
    }

    /// Is any park still armed? The countdown tick runs while the answer
    /// is yes, parked or busy: a wait that is only waiting is exactly the
    /// thing the clock is for.
    pub fn waiting(&self) -> bool {
        self.waits.values().any(|w| w.settled.is_none())
    }

    pub fn record_at(&self, i: usize) -> Option<RecordId> {
        self.records.get(&i).copied()
    }

    /// A deliberate scroll: a wheel notch, a key, a jump. The operator's
    /// hand has moved the view, so whatever the last frame anchored it to
    /// is spent — the next frame draws where the hand put it and anchors
    /// afresh. Every write of `scroll_up` outside the renderer goes
    /// through here, which is what keeps the two from fighting.
    pub fn set_scroll(&mut self, up: usize) {
        self.scroll_up = up;
        self.scroll_anchor = None;
    }

    pub fn info(&mut self, s: impl Into<String>) {
        self.say(s.into(), Level::Info);
    }

    /// [`info`](Self::info), in the `error` role: something did not
    /// happen — a launch that failed, a dispatch that abstained.
    pub fn alert(&mut self, s: impl Into<String>) {
        self.say(s.into(), Level::Alert);
    }

    fn say(&mut self, text: String, level: Level) {
        if self.notices.len() >= NOTICES_KEPT {
            self.notices.remove(0);
        }
        self.notices.push((level, text.clone()));
        self.notice = Some(Notice {
            text,
            level,
            at: Instant::now(),
        });
    }

    /// Something happened to the conversation, and the log will say so
    /// again on resume: an [`Entry::Info`] on the transcript. Reach for
    /// [`info`](Self::info) unless `seed` draws the same line from a
    /// record — that is the whole test.
    pub fn note(&mut self, s: impl Into<String>) {
        self.transcript.push(Entry::Info(s.into()));
    }

    /// [`note`](Self::note) in the `error` role: a turn that did not
    /// finish, an error the conversation has to show beside where it
    /// happened.
    pub fn note_alert(&mut self, s: impl Into<String>) {
        self.transcript.push(Entry::Alert(s.into()));
    }

    /// Take the notice down once it has had its time. Called by the UI
    /// loop every poll; `true` when the frame has to be drawn again.
    pub fn expire_notice(&mut self) -> bool {
        if self
            .notice
            .as_ref()
            .is_some_and(|n| n.at.elapsed() > NOTICE_TTL)
        {
            self.notice = None;
            return true;
        }
        false
    }

    /// What the script sees. Small on purpose: the transcript stays on the
    /// Rust side and is placed, not passed.
    pub fn snapshot(&self) -> serde_json::Value {
        let name = std::path::Path::new(&self.session_path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // One answer for the frame, the title and the badge: the script
        // cannot say one colour while the border says another, because
        // both read this.
        let mode = crate::render::mode_style(self);
        let trace = crate::trace::Where::of(self);
        serde_json::json!({
            "model": self.model,
            // The empty string is "nobody", the convention `spend` above
            // already reads: a script draws a badge when there is one and
            // nothing when there is not, with no null to take.
            "persona": self.persona.clone().unwrap_or_default(),
            "session": name,
            "cwd": self.cwd,
            "working": self.working,
            "elapsed_ms": self.working_since.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0),
            // Streamed characters over four: a live guess at this turn's output.
            "turn_out": self.streamed / 4,
            // `-1` is "nobody said", the convention `ui::context` reads —
            // a Rune script counts in numbers and has no null to take.
            // Zero could not carry it: zero is what a freshly compacted
            // session genuinely has.
            "context_tokens": self.context_tokens.map_or(-1, |t| t as i64),
            // Zero is "nobody said", the convention `usage::context_line`
            // already reads: a model whose definition declares no window
            // gets a count and no fraction, rather than a fraction of a
            // guess.
            "context_limit": self.context_limit().unwrap_or(0),
            "usage_in": self.usage_in,
            "usage_out": self.usage_out,
            // A string, formatted here, because the script has no float
            // formatting worth the name and the empty string is the one
            // value it needs to tell apart: nothing priced yet.
            "spend": if self.spend > 0.0 { eidolon_core::usage::dollars(self.spend) } else { String::new() },
            "tools_run": self.tools_run,
            "last_tool": self.last_tool,
            "dialog": self.dialog.is_some(),
            "following": self.scroll_up == 0,
            "detail": self.detail.as_str(),
            // Armed, and only armed. A script draws a badge when this is
            // true; there is nothing for it to say when the switch is
            // disarmed or absent, and a three-valued key would invite a
            // status line that announced the gate was working.
            "yolo": self.yolo.as_ref().is_some_and(|y| y.armed()),
            // What is staged for the next message. A count and the names,
            // not the images: a script draws a line saying an attachment
            // is waiting, and the bytes have no business crossing into a
            // Rune VM.
            "attachments": self.attachments.len(),
            "attachment_names": self.attachments.iter().map(|a| a.name.clone()).collect::<Vec<_>>(),
            // The name of a row in the script's `modes()` table. Usually
            // the prompt's own mode; "command" and "search" are the two
            // rows that are not modes on the buffer — the command line
            // *is* the prompt in insert with a `:` in it, and the search
            // line is the prompt slot lent to a pattern — but the badge
            // should say so anyway, which is why they are reported here
            // as modes. The look that goes with the name follows, from
            // the one answer the frame is drawn from.
            "mode": mode.name,
            "mode_colour": mode.colour,
            "mode_label": mode.label,
            "mode_sigil": mode.sigil.map(String::from).unwrap_or_default(),
            "mode_kind": mode.kind.map(crate::edit::Kind::as_str),
            // The live search, for a status line that wants to say where
            // in it you are. `hit` is 1-based and 0 when nothing is found,
            // so a script can print it without arithmetic.
            "search": self.search.as_ref().map(|s| s.pattern().to_string()),
            "hits": self.search.as_ref().map_or(0, |s| s.hits.len()),
            "hit": self.search.as_ref().and_then(|s| s.at).map_or(0, |i| i + 1),
            "matches": self.search.as_ref().map_or(0, crate::search::Search::occurrences),
            "tagged": self.jump.is_some(),
            // How many folds the operator has opened by hand, so a
            // status line can say the view is not at rest.
            "opened": self.opened.len(),
            // The trace cursor, for a status line that wants to say
            // where in the transcript you are standing. Every field is
            // computed only while the mode is on: walking the blocks is
            // cheap, but it is not free, and it is worth nothing to a
            // prompt that is being typed into.
            "tracing": self.tracing(),
            "trace_at": trace.at,
            "trace_blocks": trace.blocks,
            "trace_kind": trace.kind,
            "trace_open": trace.open,
            "trace_extend": self.trace.extend,
            "trace_span": trace.span,
            "trace_filter": self.trace_filter.as_deref().unwrap_or(""),
            // The dispatch surface the prompt is aimed at, "" when none is
            // configured. Worth a key of its own rather than folding it
            // into `mode`, because a status line wants to name the surface
            // while the badge names the mode.
            "dispatch": self.dispatch.surface().unwrap_or(""),
            "pending": self.pending,
            "empty": self.prompt.is_empty(),
            "copied": self.copied_at.is_some(),
            "queued": self.queued,
            "transcript_len": self.transcript.len(),
            // True until something speaks on the branch: the operator's
            // first message, or anything that wakes the session — a peer's
            // note, an external caller's, both drawn as Peer. A woken turn
            // answers into the transcript, and the start screen must not
            // stand in front of it. Model, persona, dispatch and cwd may
            // all change while fresh — the screen's rows read them live,
            // and the notes those changes push stay behind the screen
            // until the transcript opens.
            "fresh": self.transcript.iter().all(|e| !matches!(e, Entry::User(_) | Entry::Peer { .. })),
            "tick": self.tick,
            "life": self.life.render(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidolon_core::message::{ContentBlock, Json, Message, StopReason};
    use eidolon_core::session::Session;

    /// The window follows the model, including across the one change the
    /// UI thread makes on its own: a `ModelChanged` replayed out of the
    /// log, with no driver and no catalog anywhere near it.
    #[test]
    fn the_window_belongs_to_the_model_and_not_to_the_message_that_moved_it() {
        let mut st = seeded(vec![RecordKind::ModelChanged {
            model: "deepseek:deepseek-v4-pro".into(),
        }]);
        st.windows = [
            ("deepseek:deepseek-v4-pro".to_string(), 1_000_000u64),
            ("fau:openai/ministral-3:14b".to_string(), 131_072),
        ]
        .into_iter()
        .collect();
        st.model = "fau:openai/ministral-3:14b".into();
        assert_eq!(st.context_limit(), Some(131_072));

        // Replay moves the model; the denominator moves with it.
        st.seed(&[&Record {
            id: 0,
            parent: None,
            ts_ms: 0,
            kind: RecordKind::ModelChanged {
                model: "deepseek:deepseek-v4-pro".into(),
            },
        }]);
        assert_eq!(st.context_limit(), Some(1_000_000));
        assert_eq!(st.snapshot()["context_limit"], 1_000_000);

        // A model the catalog says nothing about draws no fraction at all,
        // rather than a fraction of zero.
        st.model = "mock".into();
        assert_eq!(st.context_limit(), None);
        assert_eq!(st.snapshot()["context_limit"], 0);
    }

    /// **What Noah saw**: `ctx 12.92M/1.00M (1292%)` on a resumed
    /// session. The gauge was reading the settle's usage, which is the
    /// *turn's* total across every model call it made — that session had
    /// one turn a hundred and forty records long — and no arithmetic
    /// turns a sum of inputs back into a context.
    ///
    /// The numerator is `RecordKind::ContextSize` now, and a log that has
    /// none says so rather than guessing.
    #[test]
    fn the_gauge_reads_the_context_record_and_never_the_settles_total() {
        let usage = eidolon_core::message::Usage {
            input_tokens: 12_920_000,
            output_tokens: 26_800,
            ..Default::default()
        };
        // A log from before the record existed: one enormous settle and
        // nothing that says how full the context was.
        let old = seeded(vec![RecordKind::TurnSettled {
            stop_reason: StopReason::EndTurn,
            usage,
        }]);
        assert_eq!(
            old.context_tokens, None,
            "the turn's total is not the context"
        );
        assert_eq!(
            old.snapshot()["context_tokens"],
            -1,
            "and the script is told so, not told zero"
        );
        assert_eq!(
            old.usage_in, 12_920_000,
            "while `in` still reports what the session cost"
        );

        // The same settle, with the record beside it.
        let now = seeded(vec![
            RecordKind::ContextSize { tokens: 148_000 },
            RecordKind::TurnSettled {
                stop_reason: StopReason::EndTurn,
                usage,
            },
        ]);
        assert_eq!(now.context_tokens, Some(148_000));
        assert_eq!(now.snapshot()["context_tokens"], 148_000);
        assert_eq!(now.usage_in, 12_920_000, "the two numbers stay two numbers");

        // A compaction restarts the history. A log from before the
        // summary's size was recorded has only the `Compacted` record, so
        // the gauge says *unknown* rather than claiming an empty context.
        let compacted = seeded(vec![
            RecordKind::ContextSize { tokens: 148_000 },
            RecordKind::Compacted {
                summary: "…".into(),
                replaced_messages: 3,
            },
        ]);
        assert_eq!(compacted.context_tokens, None);
        assert_eq!(compacted.snapshot()["context_tokens"], -1);

        // A compaction written by the current core journals the summary's
        // measured size after the `Compacted` record, and the gauge reads it.
        let summarised = seeded(vec![
            RecordKind::ContextSize { tokens: 148_000 },
            RecordKind::Compacted {
                summary: "…".into(),
                replaced_messages: 3,
            },
            RecordKind::ContextSize { tokens: 412 },
        ]);
        assert_eq!(summarised.context_tokens, Some(412));
        assert_eq!(summarised.snapshot()["context_tokens"], 412);
    }

    /// The usage panel's ledger is fed from the same records the totals
    /// are, so a resumed session's page reads as the live one did: the
    /// tool loop, the failure, the context, the price at the rates of
    /// the model then in play, and the turn on a model with no rates.
    #[test]
    fn seeding_fills_the_ledger_the_live_path_would() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(&dir.path().join("s.eid"), "prov:m", dir.path(), None).unwrap();
        let usage = eidolon_core::Usage {
            input_tokens: 1_000,
            output_tokens: 200,
            cache_read_input_tokens: 9_000,
            ..Default::default()
        };
        for k in [
            RecordKind::UserMessage(Message::user_text("build it")),
            RecordKind::AssistantMessage(Message::assistant(vec![
                call("t1", "shell"),
                call("t2", "read"),
            ])),
            RecordKind::ToolResult {
                tool_use_id: "t1".into(),
                content: "boom".into(),
                is_error: true,
            },
            RecordKind::ToolResult {
                tool_use_id: "t2".into(),
                content: "text".into(),
                is_error: false,
            },
            RecordKind::PolicyVerdict {
                tool_use_id: "t1".into(),
                tool: "shell".into(),
                reason: "rm".into(),
                structural: false,
                outcome: eidolon_core::session::PolicyOutcome::Approved,
                note: None,
            },
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text("done")])),
            RecordKind::ContextSize { tokens: 10_000 },
            RecordKind::TurnSettled {
                stop_reason: StopReason::EndTurn,
                usage,
            },
            RecordKind::ModelChanged {
                model: "other:m".into(),
            },
            RecordKind::UserMessage(Message::user_text("again")),
            RecordKind::Cancelled,
            RecordKind::TurnSettled {
                stop_reason: StopReason::MaxTokens,
                usage,
            },
        ] {
            s.append(k).unwrap();
        }
        let mut st = UiState::new("m".into(), "s".into(), "/".into());
        st.costs.insert(
            "m".into(),
            eidolon_providers::Cost {
                input: 1.0,
                output: 2.0,
                cache_read: 0.1,
                cache_write: 0.0,
            },
        );
        st.seed(&s.branch());

        let l = &st.ledger;
        assert_eq!(l.turns.len(), 2);
        let first = &l.turns[0];
        assert_eq!(
            (
                first.model.as_str(),
                first.calls,
                first.tools,
                first.tool_failures,
                first.context
            ),
            ("m", 2, 2, 1, Some(10_000))
        );
        assert!(
            first.duration_ms().is_some(),
            "a real log stamps its records"
        );
        let price = (1_000.0 * 1.0 + 200.0 * 2.0 + 9_000.0 * 0.1) / 1e6;
        assert!((first.cost.unwrap() - price).abs() < 1e-12);
        let second = &l.turns[1];
        assert_eq!(
            (second.model.as_str(), second.calls, second.cost),
            ("other:m", 0, None)
        );
        assert_eq!(second.stop, StopReason::MaxTokens);
        assert_eq!(l.cancelled, 1);
        assert_eq!(l.verdicts.approved, 1);
        assert_eq!(
            l.tools
                .iter()
                .map(|(n, t)| (n.as_str(), t.calls, t.failures))
                .collect::<Vec<_>>(),
            vec![("shell", 1, 1), ("read", 1, 0)]
        );
        // One accumulator: the totals the status line draws are the
        // ledger's own.
        assert_eq!(l.total().total_input(), st.usage_in);
        assert_eq!(l.total().output_tokens, st.usage_out);
        assert_eq!(l.spend(), Some((st.spend, 1)));
        let page = st.usage_page();
        assert!(page.contains("2 turns"), "{page}");
        assert!(page.contains("interrupted 1 time"), "{page}");
        assert!(page.contains("1 turn unpriced (on other:m)"), "{page}");
    }

    fn seeded(kinds: Vec<RecordKind>) -> UiState {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(&dir.path().join("s.eid"), "prov:m", dir.path(), None).unwrap();
        for k in kinds {
            s.append(k).unwrap();
        }
        let mut st = UiState::new("m".into(), "s".into(), "/".into());
        st.seed(&s.branch());
        st
    }

    /// A tool line on screen belongs to its **result**, not to the message
    /// that asked for it.
    ///
    /// That is the whole of what makes striking a tool line useful:
    /// withholding four thousand lines of build output is the common case,
    /// and deleting the call that produced it — which is what claiming the
    /// entry for the assistant message would do — is not what anyone means
    /// by "strike this". The assistant message keeps the prose and the
    /// thinking beside it.
    #[test]
    fn a_tool_entry_belongs_to_its_result_and_not_to_the_call() {
        use eidolon_core::message::{ContentBlock, Json, Message};
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("build it")),
            RecordKind::AssistantMessage(Message::assistant(vec![
                ContentBlock::text("on it"),
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "shell".into(),
                    input: Json("{}".into()),
                },
            ])),
            RecordKind::ToolResult {
                tool_use_id: "t1".into(),
                content: "output".into(),
                is_error: false,
            },
        ]);
        // user(#1), assistant prose(#2), the tool line(#2 → claimed by #3)
        assert_eq!(st.record_at(0), Some(1), "the operator's message");
        assert_eq!(
            st.record_at(1),
            Some(2),
            "the prose belongs to the assistant message"
        );
        assert_eq!(
            st.record_at(2),
            Some(3),
            "the tool line belongs to the result"
        );
    }

    /// The rule is the channel's, and it is the whole of what this
    /// consumer knows about it: `! ` opening a line, with something behind
    /// it. Everything else a line can do with a bang is prose.
    #[test]
    fn only_a_line_that_opens_with_the_marker_and_says_something_is_a_mark() {
        let text = [
            "An exclamation ! in a sentence.",
            "![a picture](x.png)",
            "! list notes",
            "!read",
            "!",
            "!   ",
            "  ! indented",
            "! read the note \"Groceries\"",
            "! windows\r",
            "",
        ]
        .join("\n");
        let marks = marked_commands(&text);
        assert_eq!(
            marks
                .iter()
                .map(|(m, _)| m.command.as_str())
                .collect::<Vec<_>>(),
            ["list notes", "read the note \"Groceries\"", "windows"],
            "the space after the bang, and something behind it"
        );
        // Ranges cover the line *and its newline*, so taking the marks out
        // cannot leave behind the blank line one of them was on.
        let kept = drop_ranges(&text, marks.iter().flat_map(|(_, ranges)| ranges.iter()));
        assert_eq!(
            kept.lines().collect::<Vec<_>>(),
            [
                "An exclamation ! in a sentence.",
                "![a picture](x.png)",
                "!read",
                "!",
                "!   ",
                "  ! indented",
            ]
        );
    }

    /// The marker writes two things and the channel tells them apart: a line
    /// with a command behind it, which is a call waiting on its answer, and a
    /// line that is the marker and nothing else, which asks for nothing.
    ///
    /// The second is syntax the model left unfinished — a lone `!` says
    /// nothing and ties to nothing — so it leaves the reply as it is read,
    /// whether or not anything was answered, and the record keeps the model's
    /// own words. The first waits: only the record that answered it can say a
    /// command ran, and [`UiState::answered`] is what takes it out.
    #[test]
    fn a_bare_marker_leaves_the_reply_while_a_command_waits_for_its_answer() {
        let text = "Dispatching it.\n\n! \n\n! read the note X\n\nPulling the full text:";
        assert_eq!(bare_marks(text).len(), 1, "the line with nothing behind it");
        assert_eq!(
            marked_commands(text).len(),
            1,
            "and it is not one of the commands"
        );
        assert_eq!(
            reply_text(&Message::assistant(vec![ContentBlock::text(text)])),
            "Dispatching it.\n\n\n! read the note X\n\nPulling the full text:",
            "the bare marker's line is gone; the command's line is still there"
        );
    }

    /// A marked command's payload leaves the prose with it, bound to the
    /// command in the ask: the bytes are the argument, and the argument
    /// belongs on the call — not stranded in the reply above it.
    #[test]
    fn a_payload_leaves_the_prose_with_its_command() {
        let reply = "Writing it now.\n\n! append $a to the note \"Build Log\"\n!!a\nthe exact bytes\nwith a \"quote\" in them\n!!\n\nDone.\n";
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("log it")),
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text(reply)])),
            RecordKind::CommandResults {
                lines: vec!["[vv] append → ok: appended".into()],
            },
        ]);
        let Entry::Assistant { text, .. } = &st.transcript[1] else {
            panic!("{:?}", st.transcript[1])
        };
        assert_eq!(
            text,
            "Writing it now.\n\n\nDone.\n",
            "command and fences alike leave the prose: {text:?}"
        );
        let Entry::Ask { commands, .. } = &st.transcript[2] else {
            panic!("{:?}", st.transcript[2])
        };
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command, "append $a to the note \"Build Log\"");
        assert_eq!(
            commands[0].payloads,
            vec![('a', "the exact bytes\nwith a \"quote\" in them".to_string())],
            "the bytes are the value, verbatim"
        );
    }

    /// Two holes, two blocks: a command that bound `$a` and `$b` carries
    /// both, in the order the reply wrote them. The reply is nothing but
    /// marks and fences, so the ask *is* the reply's entry — the emptied
    /// path — and it sits at one.
    #[test]
    fn a_command_carrying_two_holes_keeps_both_blocks() {
        let reply = "! replace $b with $a in X\n!!a\nnew\n!!\n!!b\nold\n!!\n";
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("fix")),
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text(reply)])),
            RecordKind::CommandResults {
                lines: vec!["[vv] replace → ok: replaced".into()],
            },
        ]);
        assert_eq!(st.transcript.len(), 2, "{:?}", st.transcript);
        let Entry::Ask { commands, .. } = &st.transcript[1] else {
            panic!("{:?}", st.transcript[1])
        };
        assert_eq!(
            commands[0].payloads,
            vec![('a', "new".to_string()), ('b', "old".to_string())],
            "both blocks, in written order"
        );
    }

    /// A hole bound twice is lifted twice — the channel's own rule, which
    /// reads every later fence against the last command and then refuses
    /// the double binding. The view draws what was sent; the answer under
    /// it says what the channel did about it.
    #[test]
    fn a_hole_bound_twice_lifts_both_blocks() {
        let reply = "! list notes\n!!a\nfirst bytes\n!!\n\nwait, no —\n!!a\nsecond bytes\n!!\n";
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("look")),
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text(reply)])),
            RecordKind::CommandResults {
                lines: vec!["[gate] payload unbound: hole `a` was bound twice".into()],
            },
        ]);
        let Entry::Ask { commands, .. } = &st.transcript[2] else {
            panic!("{:?}", st.transcript[2])
        };
        assert_eq!(
            commands[0].payloads,
            vec![('a', "first bytes".to_string()), ('a', "second bytes".to_string())],
            "both bindings, in written order"
        );
    }

    /// A fence with no marked line above it anywhere binds to nothing —
    /// prose to the channel, so prose here: it stays in the reply, and no
    /// command may claim its bytes.
    #[test]
    fn a_fence_under_no_command_stays_prose() {
        let reply = "thinking out loud:\n!!a\norphan bytes\n!!\ndone thinking.\n";
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("look")),
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text(reply)])),
            RecordKind::CommandResults {
                lines: vec!["[vv] read_note title=\"X\" → ok: it".into()],
            },
        ]);
        // One answer, no marked line: never paired, nothing lifts.
        let Entry::Assistant { text, .. } = &st.transcript[1] else {
            panic!("{:?}", st.transcript[1])
        };
        assert!(text.contains("!!a"), "the fence stays: {text:?}");
        assert!(text.contains("orphan bytes"), "{text:?}");
        let Entry::Ask { commands, .. } = &st.transcript[2] else {
            panic!("{:?}", st.transcript[2])
        };
        assert!(commands.is_empty());
    }

    /// The bytes are verbatim, fences included: a `!!`-shaped line inside
    /// the block is bytes, and a code fence around the whole block is
    /// outside the channel's grammar — the fence branch reads it, not the
    /// payload one.
    #[test]
    fn payload_bytes_keep_what_they_are_given() {
        let reply = "! append $a to X\n!!a\n!!x looks like an opener\n!!\nand this is outside\n";
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("go")),
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text(reply)])),
            RecordKind::CommandResults {
                lines: vec!["[vv] append → ok: appended".into()],
            },
        ]);
        let Entry::Ask { commands, .. } = &st.transcript[2] else {
            panic!("{:?}", st.transcript[2])
        };
        assert_eq!(
            commands[0].payloads,
            vec![('a', "!!x looks like an opener".to_string())],
            "a fence inside the bytes is bytes"
        );
    }

    /// The same words read back off the branch draw the same way, which is the
    /// whole point of doing this at the entry rather than at the frame.
    #[test]
    fn a_resumed_reply_has_no_bare_marker_to_draw() {
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("read one")),
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text(
                "! \n\nThe channel answered, but only as a preview.",
            )])),
        ]);
        let Entry::Assistant { text, .. } = &st.transcript[1] else {
            panic!("{:?}", st.transcript[1])
        };
        assert_eq!(text, "\nThe channel answered, but only as a preview.");
    }

    /// A marked line is a call, so it leaves the prose when its answer is
    /// journaled and is drawn as one — the whole of the difference between
    /// this and a line of text the model wrote.
    #[test]
    fn a_marked_line_leaves_the_prose_when_its_answer_is_journaled() {
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("what is in the vault?")),
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text(
                "Let me look.\n\n! list notes\n",
            )])),
            RecordKind::CommandResults {
                lines: vec!["[vv] list_notes → ok: 42 notes".into()],
            },
        ]);
        assert_eq!(st.transcript.len(), 3, "{:?}", st.transcript);
        let Entry::Assistant { text, .. } = &st.transcript[1] else {
            panic!("{:?}", st.transcript[1])
        };
        assert_eq!(text, "Let me look.\n\n", "the marked line is not prose");
        let Entry::Ask { commands, answers } = &st.transcript[2] else {
            panic!("{:?}", st.transcript[2])
        };
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command, "list notes");
        assert_eq!(answers, &["[vv] list_notes → ok: 42 notes"]);
        assert_eq!(
            st.record_at(2),
            Some(3),
            "the ask is drawn from the answers, so striking them strikes it"
        );
    }

    /// A count that disagrees is the one thing that must not be papered
    /// over: the answers are still drawn — they are the harness's words —
    /// and the marks stay in the prose, which is where a line nothing ran
    /// belongs.
    #[test]
    fn marks_and_answers_that_do_not_pair_are_never_paired() {
        let st = seeded(vec![
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text(
                "two reads\n\n! read a\n! read b\n",
            )])),
            RecordKind::CommandResults {
                lines: vec!["[vv] read_note title=\"a\" → ok: a".into()],
            },
        ]);
        let Entry::Assistant { text, .. } = &st.transcript[0] else {
            panic!("{:?}", st.transcript[0])
        };
        assert!(
            text.contains("! read a") && text.contains("! read b"),
            "an unpaired mark is left where it was: {text:?}"
        );
        let Entry::Ask { commands, answers } = &st.transcript[1] else {
            panic!("{:?}", st.transcript[1])
        };
        assert!(
            commands.is_empty(),
            "no command may be claimed for an answer that might not be its own"
        );
        assert_eq!(answers.len(), 1);
    }

    /// Nothing answered is nothing to draw: the marks are prose after all,
    /// which is how a session whose channel is not armed stays readable.
    #[test]
    fn an_empty_answer_draws_nothing_and_moves_nothing() {
        let mut st = seeded(vec![RecordKind::AssistantMessage(Message::assistant(
            vec![ContentBlock::text("! read the note X\n")],
        ))]);
        st.answered(&[], 99);
        let Entry::Assistant { text, .. } = &st.transcript[0] else {
            panic!("{:?}", st.transcript[0])
        };
        assert_eq!(text, "! read the note X\n");
        assert_eq!(st.transcript.len(), 1);
    }

    /// A reply of nothing but marks is a real shape — the model asked and
    /// said nothing else — and it must not leave an empty reply behind: an
    /// entry drawn as no lines is a block the trace cursor lands on and
    /// shows nothing for.
    #[test]
    fn a_reply_of_nothing_but_marks_becomes_the_ask() {
        let st = seeded(vec![
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text(
                "! list notes\n",
            )])),
            RecordKind::CommandResults {
                lines: vec!["[vv] list_notes → ok: 42 notes".into()],
            },
        ]);
        assert_eq!(st.transcript.len(), 1, "{:?}", st.transcript);
        let Entry::Ask { commands, answers } = &st.transcript[0] else {
            panic!("{:?}", st.transcript[0])
        };
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command, "list notes");
        assert_eq!(answers.len(), 1);
        assert_eq!(
            st.record_at(0),
            Some(2),
            "the entry belongs to the answers now, not to the reply"
        );
    }

    /// A struck record is known to be struck on resume, so a session
    /// reopened tomorrow draws — and re-strikes — the same rows.
    #[test]
    fn seeding_reads_back_what_was_struck() {
        use eidolon_core::message::Message;
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("one")),
            RecordKind::UserMessage(Message::user_text("two")),
            RecordKind::Excluded {
                target: 1,
                excluded: true,
            },
        ]);
        assert!(st.struck.contains(&1));
        assert!(!st.struck.contains(&2));
        // And restoring is the latest word, not a second strike.
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("one")),
            RecordKind::Excluded {
                target: 1,
                excluded: true,
            },
            RecordKind::Excluded {
                target: 1,
                excluded: false,
            },
        ]);
        assert!(st.struck.is_empty(), "restored is restored");
    }

    /// The star under a finished reply is the **turn's**, and a resumed
    /// session wears the same ones the live one did: the mark is read off
    /// how many such lines are already on the transcript, and replay
    /// rebuilds the transcript — and so the stars — in the same order
    /// from the same records.
    #[test]
    fn a_resumed_session_wears_the_stars_the_live_one_did() {
        let turn = |t: &str| {
            vec![
                RecordKind::UserMessage(Message::user_text(t)),
                RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text(t)])),
                RecordKind::TurnSettled {
                    stop_reason: StopReason::EndTurn,
                    usage: Default::default(),
                },
            ]
        };
        let st = seeded([turn("one"), turn("two")].concat());
        let stars: Vec<&str> = st
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Settle { turn, .. } => Some(*turn),
                _ => None,
            })
            .map(crate::life::star)
            .collect();
        assert_eq!(stars, vec![crate::life::star(0), crate::life::star(1)], "{:?}", st.transcript);
        assert_ne!(stars[0], stars[1], "two finished turns under one star");
        // And the line is otherwise what it always was: the word, then
        // the clock and the tokens out.
        let Some(Entry::Settle { text, .. }) = st.transcript.last() else {
            panic!("no settle line: {:?}", st.transcript);
        };
        assert!(text.starts_with("ins"), "{text}");
        assert!(text.contains("\u{b7} \u{21e3} "), "{text}");
    }

    /// A log that records why a turn ended keeps saying it. Reopening the
    /// session that motivated this must not be how the operator finds out
    /// the answer was truncated.
    #[test]
    fn a_replayed_turn_that_did_not_finish_still_says_so() {
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("how do you know?")),
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text(
                "Next Steps:\n1. Want to test a command in Eidolon (e.g., `./target/debug/e",
            )])),
            RecordKind::TurnSettled {
                stop_reason: StopReason::MaxTokens,
                usage: Default::default(),
            },
        ]);
        assert!(
            matches!(st.transcript.last(), Some(Entry::Alert(t)) if t.contains("cut off")),
            "{:?}",
            st.transcript
        );
    }

    /// ...and a turn that ended properly adds nothing.
    #[test]
    fn a_replayed_turn_that_finished_says_nothing() {
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("hi")),
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text("hello")])),
            RecordKind::TurnSettled {
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
            },
        ]);
        assert!(
            !st.transcript.iter().any(|e| matches!(e, Entry::Alert(_))),
            "{:?}",
            st.transcript
        );
    }

    fn call(id: &str, name: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input: Json("{}".into()),
        }
    }

    /// The parallel-call regression: three writes in one assistant message,
    /// three results in the order they were asked for. Pairing by "the last
    /// entry still empty" put every output under the wrong call — file 1's
    /// output beneath file 3's call, exactly reversed.
    #[test]
    fn results_land_on_the_call_that_asked_for_them() {
        let st = seeded(vec![
            RecordKind::AssistantMessage(Message::assistant(vec![
                call("a", "write"),
                call("b", "write"),
                call("c", "write"),
            ])),
            RecordKind::ToolResult {
                tool_use_id: "a".into(),
                content: "wrote one".into(),
                is_error: false,
            },
            RecordKind::ToolResult {
                tool_use_id: "b".into(),
                content: "wrote two".into(),
                is_error: false,
            },
            RecordKind::ToolResult {
                tool_use_id: "c".into(),
                content: "wrote three".into(),
                is_error: false,
            },
        ]);
        let paired: Vec<(&str, &str)> = st
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool {
                    id,
                    output: Some((c, _)),
                    ..
                } => Some((id.as_str(), c.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            paired,
            [("a", "wrote one"), ("b", "wrote two"), ("c", "wrote three")]
        );
    }

    /// A result whose call the branch does not describe must not be handed
    /// to whichever call happens to be open.
    ///
    /// A script-originated call is exactly that: the harness's own work
    /// inside another call — the vault channel's resolved `mneme_rpc`, a
    /// skill script's `eidolon::*` — and no record describes it, because
    /// only an operator's dispatch writes a call record. Its result still
    /// arrives, and pairing it by order would write it under the `vv` line
    /// that asked for it *and* leave that line's own result with no open
    /// entry to land on: a resume that says the wrong thing twice over.
    #[test]
    fn a_nested_calls_result_never_lands_on_the_call_that_made_it() {
        let st = seeded(vec![
            RecordKind::AssistantMessage(Message::assistant(vec![
                ContentBlock::text("asking the vault"),
                ContentBlock::ToolUse {
                    id: "vv_1".into(),
                    name: "vv".into(),
                    input: Json(r#"{"line":"read the note X"}"#.into()),
                },
            ])),
            RecordKind::ToolResult {
                tool_use_id: "vv_mneme_7".into(),
                content: "the nested rpc's answer".into(),
                is_error: false,
            },
            RecordKind::ToolResult {
                tool_use_id: "vv_1".into(),
                content: "what the model was told".into(),
                is_error: false,
            },
        ]);
        let paired: Vec<(&str, &str)> = st
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool {
                    id,
                    output: Some((c, _)),
                    ..
                } => Some((id.as_str(), c.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            paired,
            [("vv_1", "what the model was told")],
            "the call that asked keeps its own result, and the nested one \
             draws nothing rather than drawing under it"
        );
    }

    /// A backend that reports its tool traffic without ids still pairs by
    /// order, which is the one case there is nothing to match on.
    #[test]
    fn a_result_that_names_no_id_still_pairs_by_order() {
        let st = seeded(vec![
            RecordKind::AssistantMessage(Message::assistant(vec![call("a", "bash")])),
            RecordKind::ToolResult {
                tool_use_id: String::new(),
                content: "no id at all".into(),
                is_error: false,
            },
        ]);
        let Entry::Tool {
            id, output: Some((out, _)), ..
        } = &st.transcript[0]
        else {
            panic!("{:?}", st.transcript[0])
        };
        assert_eq!(id, "a");
        assert_eq!(out, "no id at all");
    }

    /// Out of order too, which is what an id buys over dropping the `.rev()`:
    /// parallel calls need not finish in the order they were made.
    #[test]
    fn and_still_when_the_results_come_back_out_of_order() {
        let st = seeded(vec![
            RecordKind::AssistantMessage(Message::assistant(vec![
                call("a", "read"),
                call("b", "read"),
            ])),
            RecordKind::ToolResult {
                tool_use_id: "b".into(),
                content: "second first".into(),
                is_error: false,
            },
            RecordKind::ToolResult {
                tool_use_id: "a".into(),
                content: "first second".into(),
                is_error: false,
            },
        ]);
        let paired: Vec<(&str, &str)> = st
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool {
                    id,
                    output: Some((c, _)),
                    ..
                } => Some((id.as_str(), c.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(paired, [("a", "first second"), ("b", "second first")]);
    }

    /// A resumed session draws the thinking the log kept. It used to
    /// draw none of it: the count lived only in the live stream, so
    /// reopening a session lost every trace of what the model thought.
    #[test]
    fn a_resumed_session_still_shows_what_the_model_thought() {
        let st = seeded(vec![
            RecordKind::UserMessage(Message::user_text("why?")),
            RecordKind::AssistantMessage(Message::assistant(vec![
                ContentBlock::Thinking {
                    thinking: "because of the lock order".into(),
                    signature: "sig".into(),
                },
                ContentBlock::text("Because of the lock order."),
            ])),
        ]);
        assert!(
            matches!(&st.transcript[1], Entry::Thinking { text, streaming: false } if text == "because of the lock order")
        );
        assert!(matches!(&st.transcript[2], Entry::Assistant { .. }));
    }

    /// Anything that wakes a session replaces the start screen. `fresh`
    /// used to look only for the operator's own message, so a turn woken
    /// by a colleague's note — or by `eidolon send` — ran, and was
    /// answered, behind the splash art. Both wakers are drawn as Peer, on
    /// the live transcript and on replay alike, so one condition covers
    /// them; a channel fan-out is the same kind of speech and goes too.
    #[test]
    fn a_woken_session_opens_on_the_transcript_not_the_start_screen() {
        let wakes = vec![
            RecordKind::PeerMessage {
                from: "eidolon-1".into(),
                from_cwd: "/tmp/other".into(),
                channel: None,
                text: "can you look at this".into(),
            },
            RecordKind::ExternalMessage {
                from: "cron".into(),
                channel: None,
                text: "run the sweep".into(),
            },
            RecordKind::PeerMessage {
                from: "eidolon-2".into(),
                from_cwd: "/tmp/other".into(),
                channel: Some("channel".into()),
                text: "heads up".into(),
            },
            RecordKind::UserMessage(Message::user_text("hello")),
        ];
        for kind in wakes {
            let st = seeded(vec![kind]);
            assert_eq!(
                st.snapshot()["fresh"].as_bool(),
                Some(false),
                "the start screen stood over: {:?}",
                st.transcript[0]
            );
        }
    }

    /// A resumed dispatch draws what the live one drew: the operator's
    /// words, then the call, then its output — so it folds the same way
    /// and the fold is a view, not bookkeeping.
    #[test]
    fn a_resumed_dispatch_replays_as_the_words_and_the_call() {
        let st = seeded(vec![
            RecordKind::UserToolCall {
                tool_use_id: "vv_1".into(),
                name: "mneme_rpc".into(),
                input: Json(r#"{"function":"read_note"}"#.into()),
                utterance: Some("read the note Groceries".into()),
            },
            RecordKind::ToolResult {
                tool_use_id: "vv_1".into(),
                content: "eggs, milk".into(),
                is_error: false,
            },
        ]);
        assert!(matches!(&st.transcript[0], Entry::User(t) if t == "read the note Groceries"));
        assert!(
            matches!(&st.transcript[1], Entry::Tool { name, output: Some((c, false)), .. }
            if name == "mneme_rpc" && c == "eggs, milk")
        );
        assert_eq!(st.transcript.len(), 2);
    }
}

