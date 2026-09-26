//! The loop-local command channel: the model's own way into the vault.
//!
//! Verba Volantia resolves one plain-text line into one typed Mneme call. The
//! operator's way in is dispatch mode (`crate::Palette`, the TUI's `:do`,
//! `eidolon do`), where a human types the line and the harness runs it. This
//! is the **model's** way in, and it exists because the write tax is in the
//! tool call rather than in the work: a frontier model composing
//! `mneme_rpc {function, args}` against a schema pays for the schema in its
//! prefix and for every parameter name in its output, and gets the names
//! wrong often enough that the rejects are a measurable class. Given a
//! channel whose whole syntax is a line of text, it writes `read the note
//! Groceries` and the loop — not the model — does the serialising.
//!
//! ## Two entry seams, one engine
//!
//! [`VaultCommands`] is both, and what selects between them is the operator's
//! ([`Entry`]'s, in the config):
//!
//! - **`vv`, the tool** ([`Tool`]) — a loop-local native tool beside
//!   `tool_search` and `choices_user` whose only parameter is one line of
//!   prose. The model calls it, policy classifies it, the dispatcher
//!   journals it, and the result comes back as an ordinary tool result.
//! - **inline, the interceptor** ([`CommandChannel`]) — no tool at all. The
//!   model writes `! read the note Groceries` on a line of its own reply
//!   (see [`MARKER`] and [`marked`]), the loop scans the completed message,
//!   runs the same gate and the same nested dispatch, journals the answers as
//!   [`eidolon_core::session::RecordKind::CommandResults`], and makes **one
//!   more model call** so the answers are read as what they are: the answer
//!   to the marked lines, in the same turn, rather than a question the
//!   operator has to prompt past.
//!
//! Neither surface registers the other, and the model must never see both at
//! once — one channel taught two ways is two conventions to satisfy and two
//! things to measure. What the inline seam cannot serve is a backend that
//! owns its own loop (the Claude CLI): the harness never assembles that
//! turn's request, so there is nothing to intercept and nothing to inject
//! into, and the tool is what that session keeps.
//!
//! Everything below the entry is shared, and deliberately unchanged between
//! them: the same [`gate`], the same [`crate::reconcile`] table, the same
//! nested [`CallOrigin::Script`] dispatch through the chokepoint, and the
//! same harvest.
//!
//! ## It executes through the chokepoint
//!
//! `vv` is registered like `tool_search` or `choices_user`, so it is offered
//! with the rest, classified by policy, and journaled by the dispatcher like
//! anything else — and the interceptor is called *by* the loop, at the point
//! where a reply's tool results would land. What neither must do is reach
//! Mneme itself: a native seam is not licence to bypass dispatched
//! execution, so a resolved command is dispatched as a nested `mneme_rpc`
//! call — the same path a Rune script's vault call takes — with
//! [`CallOrigin::Script`], because the harness is acting on the model's
//! behalf and a `UserToolCall` journaled here would make the model's own
//! unanswered `vv` call read as interrupted.
//!
//! ## The gate is the policy, and abstention is its normal outcome
//!
//! The classifier's accept threshold is calibrated on generated dev data and
//! transfers only loosely, so nothing about the classifier's own confidence
//! is trusted to be *safe*. The gate in [`gate`] dispatches only a read of a
//! known Mneme function with every required argument bound, and never when
//! the classifier reported a conflict or left text unclaimed. Everything else
//! comes back to the model as one line saying why, plus the instruction to use
//! the ordinary tools — fallback is the expected path, not a failure.
//!
//! The first slice was one surface (Mneme) and one direction (reads). The
//! second adds writes: the payload ones (`append_to_note`, `create_note`,
//! `edit_note`, `replace_section`), the note-shaped ones (`move_note`,
//! `rename_note`, `restore_note`, `restore_version`) and `delete_note`. A
//! write-shaped intent outside [`WRITES`] is still not executed, and the attempt
//! is the valuable part of the harvest.
//!
//! What justifies the writes is recovery, not confidence: the vault has a trash,
//! a version store, and an ordered audit log, so a channel write that turns out
//! wrong is legible and undoable. What *gates* them is the classifier and the
//! payload rule below — the intent must be one of the nine, every required
//! argument must bind, and a payload must be carried rather than guessed.
//!
//! ## A write's payload is carried, not inferred
//!
//! The line a model writes for a read carries its literals and the binder lifts
//! them; there is nothing to get wrong about *where* the value is, because the
//! value is a word — a title, a path, a query. A write is a different shape: its
//! payload is the text to be written, running to thousands of characters and
//! often across lines, and the thing that must not happen is the tagger guessing
//! where that text begins and ends. So a payload rides in a fence ([`FENCE`])
//! bound to a hole the line left bare, out of the classifier's reach entirely —
//! it sees the same `append $a to the note $b` it has always seen — and the gate
//! refuses a write whose payload came from anywhere else. That includes the
//! quoted span the same line uses for literals, which is a *delimiter* and
//! therefore looks like a carrier: it is not one, because it ends at the next
//! `"` and a command is one line. `append "he said \"…\" and left"` binds
//! `content` to `he said \` and survives only if the residue happens to collide
//! with another hole.
//!
//! ## Two names for one thing
//!
//! VV's intent vocabulary and Mneme's RPC vocabulary are not the same
//! vocabulary — `read_note` takes `note` in the corpus and `title` on the
//! wire, `conventions` is `get_conventions` (or `read_convention` with a
//! slug), `read_version`'s `version` is `id`. The mapping is
//! [`crate::reconcile`]'s, one table shared with dispatch mode, so the model's
//! channel and the operator's `:do` cannot disagree about what
//! `read the note Groceries` sends. What is *this* file's is [`READS`]: the
//! allowlist of intents that may run at all, and Mneme's required arguments for
//! each, checked on the reconciled call.
//!
//! ## Harvest
//!
//! Every command attempt — dispatched or not — appends one JSON line to
//! `<session log stem>.verba.jsonl` beside the log, and the observer half
//! records the model's own hand-written `mneme_rpc` calls the same way, so
//! the reject rate of the structured path the channel is replacing is
//! measured rather than assumed. Writing the sidecar is best-effort: a log
//! that will not take a note must not stop a turn.
//!
//! An intercepted command's record also carries `trailing_chars`: how much of
//! the reply came after the last marked line. That is turn discipline, not a
//! decision — the model is taught to write commands last and stop, because
//! what it writes below a command is prose about a result it does not have
//! yet (vault: `Tool-Starved Models Confabulate Calls and Their Results`) —
//! and the number is how the discipline is measured rather than assumed.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Map, Value, json};

use eidolon_core::dispatch::{Dispatcher, ToolObserver};
use eidolon_core::message::{ContentBlock, Message};
use eidolon_core::policy::Approval;
use eidolon_core::tool::{CallContext, CallOrigin, Tool, ToolCall, ToolManifest, ToolOutput};
use eidolon_core::CommandChannel;
use tokio_util::sync::CancellationToken;

use crate::dispatcher::Verdict;
use crate::palette::{Palette, mneme_call, now_ms};
use crate::reconcile::reconcile;

/// The tool's name. Short because it is in the model's tool list on every
/// request, and because the sentence it takes is what carries the meaning.
pub const NAME: &str = "vv";

/// The inline marker: bang and space, at the start of a line.
///
/// The space is load-bearing in both directions. It is what keeps a markdown
/// image out (`![alt](url)` begins with a bang and is prose about a picture),
/// and it is what keeps an exclamation out — only a line the model began with
/// the marker is a line it is asking about. See [`marked`].
pub const MARKER: &str = "! ";

/// The payload fence: a write's bytes, on lines of their own.
///
/// `!!a` opens a block bound to the command's `$a` hole, a line that is exactly
/// `!!` closes it, and what lies between them **is** the value — every byte,
/// every newline, every quote, nothing escaped and nothing trimmed. The letter
/// is the placeholder the command wrote, so a command with two payloads
/// (`replace X with Y in the note Z`) names both holes and the two blocks cannot
/// be mistaken for each other.
///
/// Why a fence rather than the quoted span [`DESCRIPTION`] teaches for literals:
/// a command is one line and a quoted span ends at the next `"`, so the span can
/// carry neither a newline nor a quote of its own. Measured against 1,674
/// harvested real calls, that rules out 95 of 99 `append_to_note` payloads and
/// 41 of 42 `create_note` ones — 90 of those 96 append payloads contain a
/// newline, 55 contain a `"`, the median is 2,427 characters and the longest is
/// 10,803. The span stays the carrier for *literals* (a note title, a short
/// target); the fence is the carrier for a *payload*.
pub const FENCE: &str = "!!";

/// The code fence: the lines between two of these are prose, and nothing in
/// them is a command.
///
/// This channel was not code-fence aware while it only read, and it did not
/// need to be. The writes are what made it matter: a model that *explains* the
/// grammar writes the grammar, and an example `! append $a to the note …` with
/// a payload fence under it is, to a line scanner, an append. That is not
/// hypothetical — an example in a reply, fenced as code, was intercepted and
/// resolved by the live channel. It refused only because the running binary
/// predated the writes; with the writes in place it would have written.
///
/// So everything inside a code block is prose. The cost is stated rather than
/// hidden: a model that fences its *commands* gets no answer and no error, and
/// the fallback to the ordinary tools is the path this channel always takes when
/// it does not understand — the same direction as every other refusal here.
pub const CODE_FENCE: &str = "```";

/// The teaching text, verbatim as the lead wrote it. It is the whole
/// interface: the parameter is a line of prose, so the description *is* the
/// grammar, and it is the model's only statement of what the channel covers
/// and what to do when it declines.
pub const DESCRIPTION: &str = r#"vault commands — plain imperative one-liners, dispatched locally, no tool call needed. Covered (reads): read a note, search, backlinks, similar notes, index, list notes/trash/versions/handles, orphan & dead-link checks, semantic queries, embed text, skill bodies, conventions. Covered (writes): append to a note, create a note, replace text in a note, replace a section body, move, rename, delete a note, restore a note or a version. A write's text payload goes in a fenced block, never inline: leave the payload hole bare (`$a`), then a line `!!a`, the exact bytes with their newlines and quotes, then a line `!!`. A write whose payload is left inline is refused, and a delete is confirmed by the channel. Rules: one command per line; literal values inline, quoted when multi-word (read note "Groceries List"); no pronouns ('merge it' → 'merge pr 42'); don't pad with politeness. If a command comes back undispatched, use RPC as normal — fallback is expected, not an error. Anything else still goes through RPC."#;

/// The teaching the model reads when the **inline** seam is armed — the
/// marker convention and the turn discipline, then [`DESCRIPTION`] verbatim.
///
/// Reusing the description's coverage half rather than writing a second one
/// is the point: the two seams are one channel, and a model taught two
/// slightly different vocabularies by two entry points would be a channel
/// measured twice over two conventions. What the inline seam has to add is
/// only what is different about it — where the command goes, and when to
/// stop writing — because there is no tool description carrying it.
///
/// This is spliced into the system prompt by the consumer that arms the
/// interceptor ([`eidolon_core::ToolRegistry::register_prompt`]), and never
/// into a request's tool list: the channel has no tool there.
pub fn inline_prompt() -> String {
    format!(
        "Vault commands are written in your reply rather than called as a tool. Put each \
command on its own line, starting with `! ` — bang and space, at the beginning of the \
line — and the harness runs it as soon as your message ends, answering in place so you \
can go on from the result without the operator having to prompt you again.\n\
Write your commands at the END of your message and then stop. The answers arrive after \
your message is finished, so anything you write below a command is prose about a result \
you do not have yet. The one exception is a payload fence: the lines between `!!a` and \
`!!` are the bytes being written, not prose — they belong to the command above them, and \
nothing else may follow them. Every command in one message runs, in order.\n\n\
{DESCRIPTION}"
    )
}

/// One marked line, and the payload blocks bound to it.
///
/// `text` is the line with [`MARKER`] stripped and trimmed. `blocks` maps the
/// hole letter of each `!!<letter>` fence to the bytes between its fences — a
/// command with no fences carries an empty map, which is every read.
#[derive(Clone, Debug, PartialEq)]
struct Command {
    text: String,
    blocks: BTreeMap<char, String>,
}

/// The commands one assistant message marks, and how many characters of it
/// followed the last of them.
///
/// The marker is [`MARKER`] — `! ` — at the **start of a line**, so:
///
/// - a marked line is one command, marker stripped and trimmed;
/// - `![alt](url)` is never one (the space after the bang is what a markdown
///   image does not have), and neither is any other line that begins with a
///   bang and something else;
/// - a `!` in the middle of a sentence is an exclamation and nothing more;
/// - a bare `!`, or a marker with only whitespace behind it, yields nothing:
///   there is no command there to run;
/// - an *indented* line is prose too. The convention is taught as its own
///   line, and a channel that answered `  ! x` would be answering a shape it
///   never described;
/// - a line inside a code block is prose, marked or not — see [`CODE_FENCE`].
///
/// A line that is exactly `!!` closes the payload block [`FENCE`] opened, and a
/// line `!!<letter>` opens one — both only where a fence is legal, which is
/// outside another block and after a command. Inside a block every line is
/// content, including one that looks like a fence; a `!!`-shaped line with
/// anything else behind it, or with no command above it, is prose like any other
/// unmarked line. Block content is carried verbatim except that line endings
/// arrive normalised to `\n`, so a CRLF-authored reply cannot write `\r` bytes
/// into a note.
///
/// The second half of the return is turn-discipline telemetry and not a
/// decision: how much of the reply came after the last marked line, counted
/// in characters, with the last line's own newline not counted. Zero means
/// the model stopped when it had asked, which is what it is taught to do. A
/// payload block is part of its command, so a well-formed write ends at its
/// closing fence and adds nothing to this number.
///
/// Text blocks are scanned one at a time rather than as one concatenated
/// reply: blocks arrive in order but are separate things on the wire, and a
/// line that begins one is a line-initial command however the block before
/// it ended. The trailing count is the rest of the block the last command
/// was in, plus every text block after it.
fn marked(message: &Message) -> (Vec<Command>, usize) {
    let texts: Vec<&str> = message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let mut commands = Vec::new();
    let mut trailing = 0;
    for (i, block) in texts.iter().enumerate() {
        let (found, after) = marked_block(block);
        if found.is_empty() {
            continue;
        }
        commands.extend(found);
        trailing = after + texts[i + 1..].iter().map(|t| t.chars().count()).sum::<usize>();
    }
    (commands, trailing)
}

/// One text block: its marked commands, and the characters after the last
/// one. See [`marked`].
fn marked_block(block: &str) -> (Vec<Command>, usize) {
    let mut commands: Vec<Command> = Vec::new();
    let mut after = 0;
    let mut start = 0usize;
    // The block being read, as the command that owns it and the hole its fence
    // named — plus the lines between the fences, still as lines.
    let mut open: Option<(usize, char, Vec<String>)> = None;
    // Inside a code block every line is prose — see [`CODE_FENCE`].
    let mut in_code = false;
    for line in block.split_inclusive('\n') {
        let end = start + line.len();
        let body = line.strip_suffix('\n').unwrap_or(line);
        let body = body.strip_suffix('\r').unwrap_or(body);
        let code_fenced = body.trim_start().starts_with(CODE_FENCE);
        if let Some((owner, letter, mut lines)) = open.take() {
            if body == FENCE {
                // Closed: the value is what the lines between the fences made,
                // with no trailing newline of its own — a write appends or
                // replaces the bytes it was handed, and one the model did not
                // type is not one of them.
                if let Some(cmd) = commands.get_mut(owner) {
                    cmd.blocks.insert(letter, lines.join("\n"));
                }
                after = block[end..].chars().count();
            } else {
                lines.push(body.to_string());
                open = Some((owner, letter, lines));
            }
        } else if code_fenced {
            // A payload block is the one thing that outranks this: a fence
            // written inside the bytes is bytes, and the branch above has it.
            in_code = !in_code;
        } else if in_code {
            // Prose, whatever it looks like.
        } else if let Some(rest) = body.strip_prefix(MARKER) {
            let command = rest.trim();
            if !command.is_empty() {
                commands.push(Command { text: command.to_string(), blocks: BTreeMap::new() });
                // Past the terminating newline: a last line that ends the
                // block is not "text after the last command".
                after = block[end..].chars().count();
            }
        } else if let Some(rest) = body.strip_prefix(FENCE) {
            // A fence opener names one hole. Anything else behind `!!` is a
            // shape this channel never described, and an opener with no command
            // above it has nothing to bind to — both stay prose.
            let mut letters = rest.trim().chars();
            let letter = letters.next();
            let hole = letter.filter(char::is_ascii_alphanumeric);
            if let (None, Some(letter), Some(owner)) =
                (letters.next(), hole, commands.len().checked_sub(1))
            {
                open = Some((owner, letter, Vec::new()));
            }
        }
        start = end;
    }
    (commands, after)
}

/// The `vv` tool's parameter, read as one command and its payload fences.
///
/// The tool has no marker — its parameter *is* the command — so the first line
/// is the command whether or not the model wrote `! ` in front of it, and the
/// fences below it are its payloads. Same grammar as the interceptor, minus the
/// marker, and the same pass reads it. A parameter marking more than one command
/// yields the first: one `command` parameter is one command, and several of them
/// belong to the inline seam.
fn tool_command(raw: &str) -> Option<Command> {
    let body = raw.trim_start();
    let body = body.strip_prefix(MARKER).unwrap_or(body);
    marked_block(&format!("{MARKER}{body}")).0.into_iter().next()
}

/// Why a command was or was not executed. The wire spelling of the harvest
/// record, used in both the log and the model-facing line so one token means
/// one thing in both places.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Dispatched.
    Pass,
    /// The classifier said this is not a vault command, or was not confident
    /// enough to say it is one.
    Abstain,
    /// The classifier found contradictory bindings.
    Conflict,
    /// A vault command, but not one this channel may run: a write outside
    /// [`WRITES`], or an intent the corpus does not know.
    NotReadShaped,
    /// There was text in the line no slot claimed — a second instruction.
    Editorial,
    /// A write, and its payload was not *carried*: the hole held inferred words,
    /// or was missing altogether. Not dispatched — the bytes of a write are the
    /// one thing here the tagger is not allowed to guess.
    PayloadUndeclared,
    /// A payload hole was declared as `$a` and no `!!a` block supplied its
    /// bytes, or one hole was declared twice.
    PayloadUnbound,
    /// A payload block the command did not declare — a fence on a read, an extra
    /// block, or a letter nothing asked for.
    BlockOrphan,
    /// A command, but short of an argument Mneme requires. Not dispatched: the
    /// model gets a count instead of a guaranteed error.
    MissingSlots,
}

impl Gate {
    pub fn as_str(self) -> &'static str {
        match self {
            Gate::Pass => "pass",
            Gate::Abstain => "abstain",
            Gate::Conflict => "conflict",
            Gate::NotReadShaped => "not_read_shaped",
            Gate::Editorial => "editorial",
            Gate::PayloadUndeclared => "payload_undeclared",
            Gate::PayloadUnbound => "payload_unbound",
            Gate::BlockOrphan => "block_orphan",
            Gate::MissingSlots => "missing_slots",
        }
    }
}

/// What the gate decided, and the call it authorises.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub gate: Gate,
    /// `Some` exactly when the gate is [`Gate::Pass`]: the Mneme function and
    /// the arguments, already reconciled with the RPC vocabulary.
    pub call: Option<(String, Map<String, Value>)>,
}

/// One intent this channel may run: Mneme's function, the arguments it cannot
/// do without, and the holes whose bytes must be *carried* rather than inferred.
///
/// [`READS`] carries nothing — every argument it needs is a literal written on
/// the line. [`WRITES`] carries a payload, and a payload is the one thing this
/// channel will not let the tagger decide.
///
/// The required arguments are Mneme's, not the corpus's: they are checked after
/// the renames, so they name what actually goes on the wire. The payload holes
/// are the corpus's, because that is the vocabulary the placeholder on the line
/// is written in.
///
/// Precedence among the readings: the classifier's own accept/none first, then
/// conflicts, then whether the intent is one this channel may run, then
/// unclaimed text, then the payload, then arity. The order is a labelling choice
/// and nothing else — each of them refuses — but the label is what the harvest
/// counts, and the payload labels are the ones this change is measured by.
const READS: &[(&str, &[&str])] = &[
    ("read_note", &["title"]),
    ("read_canvas", &["title"]),
    ("read_version", &["note", "id"]),
    // A slug is bound into `name`, so there is nothing to be short of.
    ("conventions", &[]),
    ("search_vault", &["query"]),
    ("find_backlinks", &["note"]),
    ("similar_notes", &["ref"]),
    ("list_versions", &["note"]),
    ("semantic_query", &["expr"]),
    ("semantic_centroid", &["refs"]),
    ("semantic_subtract", &["ref", "subtract_refs"]),
    ("embed_text", &["text"]),
    ("skill_body", &["name"]),
    ("get_index", &[]),
    ("list_notes", &[]),
    ("list_trash", &[]),
    ("list_handles", &[]),
    ("check_dead_links", &[]),
    ("check_orphans", &[]),
];

/// The writes this channel may run: the intent, the arguments Mneme cannot do
/// without, and the hole a payload rides in (`&[]` when the command is all
/// literals).
///
/// The list grew twice, and by different arguments. The first three are the
/// payload writes — `append_to_note` and `create_note` because the payload *is*
/// the command and the structured path cannot carry it, `edit_note` because it is
/// the most-used function on the surface by a wide margin (484 of 1,674 harvested
/// calls, 4× the next). The rest are the ones the operator asked for with a
/// restore system and an audit log in hand: the note-shaped writes
/// (`move_note`, `rename_note`, `restore_note`, `restore_version`) and the two
/// that touch existing text (`replace_section`, `delete_note`) — every one of
/// them journaled, and every one of them recoverable, which is what makes
/// admitting them a policy choice rather than a hazard.
///
/// Deliberately absent, and not for lack of a trained intent: `empty_trash`
/// (there is nothing to restore *from*), `create_canvas` (blank canvases only,
/// and a graph is not one sentence), and `update_note`, which is reachable as
/// `edit_note`'s content-only variant and is a resolution shape this channel has
/// not been taught yet — see `crate::reconcile`.
///
/// `edit_note` declares two holes. The frame's own hole order carries the roles
/// (`target` before `content`, the order the note-last pass repaired), and naming
/// the letter on each fence means the two blocks cannot be swapped even when the
/// model writes them out of order.
///
/// The required arguments are Mneme's own, checked after the renames.
const WRITES: &[(&str, &[&str], &[&str])] = &[
    ("append_to_note", &["title", "content"], &["content"]),
    ("create_note", &["title", "content"], &["content"]),
    ("edit_note", &["title", "old_str", "new_str"], &["target", "content"]),
    ("replace_section", &["title", "heading", "new_content"], &["content"]),
    ("move_note", &["title"], &[]),
    ("rename_note", &["old_title", "new_title"], &[]),
    ("delete_note", &["title", "confirm"], &[]),
    ("restore_note", &["name"], &[]),
    ("restore_version", &["note", "id"], &[]),
];

/// The intents whose Mneme function refuses without `confirm: true`, and which
/// this channel therefore confirms.
///
/// One entry, and it is the operator's decision rather than a safety property:
/// the vault has a trash and a version store (`restore_note`, `restore_version`)
/// and an ordered audit log that records the change with its pre-change version
/// id, so a deletion made here is recoverable and legible. `empty_trash` is not
/// on the list — there is nothing to restore from — and neither is anything
/// else on the covered surface, which needs no confirmation at all.
///
/// The server-side `dispatch` tool does the opposite and hands the caller a
/// `needs_confirmation` disposition to re-invoke by hand. That exists because its
/// caller may be a client with nobody watching; a marked line is written in the
/// transcript, journaled as `CommandResults`, and executed by the loop in front
/// of whoever is reading. Both are the same protocol with the confirmation placed
/// where the watching happens — which is not the same as saying nothing is
/// watching.
const CONFIRMED: &[&str] = &["delete_note"];

/// The row for `intent`: its required RPC arguments and its payload holes, or
/// `None` when this channel may not run it at all.
fn allowed(intent: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    if let Some((_, required)) = READS.iter().find(|(name, _)| *name == intent) {
        return Some((required, &[]));
    }
    WRITES
        .iter()
        .find(|(name, _, _)| *name == intent)
        .map(|(_, required, payloads)| (*required, *payloads))
}

/// The hole letter a declared payload was written as — a bare `$a` — or `None`
/// when the slot holds anything else at all.
fn hole_letter(value: &str) -> Option<char> {
    let mut chars = value.strip_prefix('$')?.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c),
        _ => None,
    }
}

/// The gate: the whole of this channel's policy, as a pure function of one
/// verdict and the payload blocks the message carried.
///
/// The write half is the part worth stating: a write's payload must have been
/// **carried**, never inferred. Carried means the line declared the hole as a
/// bare `$a` and a `!!a` block supplied the bytes; inferred means the tagger
/// decided that a run of the model's own words was the text to write. Inference
/// is what the structured path does, and it is why an `append` payload is the
/// one thing here that is not allowed to be a guess — the truncated quoted span
/// (`append "he said \"…\""` binds `content` to `he said \`) is the same failure
/// arriving by a different road.
pub fn gate(v: &Verdict, blocks: &BTreeMap<char, String>) -> Decision {
    let refuse = |gate| Decision { gate, call: None };
    if !v.accepted() {
        return refuse(Gate::Abstain);
    }
    if !v.conflicts.is_empty() {
        return refuse(Gate::Conflict);
    }
    let Some((required, payloads)) = allowed(&v.intent) else {
        return refuse(Gate::NotReadShaped);
    };
    if v.trailing_editorial_text
        .as_deref()
        .is_some_and(|t| !t.trim().is_empty())
    {
        return refuse(Gate::Editorial);
    }
    let mut slots = v.slots.clone();
    if payloads.is_empty() {
        // A read carries nothing, so a block bound to one is a shape the channel
        // never described — refused rather than ignored: a fence the model wrote
        // for a command that cannot use it is a misunderstanding of the channel,
        // and answering it silently teaches the wrong grammar.
        if !blocks.is_empty() {
            return refuse(Gate::BlockOrphan);
        }
    } else {
        let mut letters: Vec<char> = Vec::with_capacity(payloads.len());
        for hole in payloads {
            let Some(letter) = slots.get(*hole).and_then(Value::as_str).and_then(hole_letter)
            else {
                return refuse(Gate::PayloadUndeclared);
            };
            let Some(bytes) = blocks.get(&letter) else {
                return refuse(Gate::PayloadUnbound);
            };
            if letters.contains(&letter) {
                // One hole named twice cannot be two payloads; the block would
                // have to be both, and guessing which is the failure this gate
                // exists to refuse.
                return refuse(Gate::PayloadUnbound);
            }
            letters.push(letter);
            slots.insert((*hole).to_string(), Value::from(bytes.clone()));
        }
        if blocks.len() != letters.len() {
            return refuse(Gate::BlockOrphan);
        }
    }
    let (function, mut args) = reconcile(&v.intent, &slots);
    if CONFIRMED.contains(&v.intent.as_str()) {
        // See [`CONFIRMED`]: the channel is where the confirmation is placed for
        // the one intent that needs one.
        args.insert("confirm".to_string(), Value::Bool(true));
    }
    if required.iter().any(|r| !args.contains_key(*r)) {
        return refuse(Gate::MissingSlots);
    }
    Decision {
        gate: Gate::Pass,
        call: Some((function, args)),
    }
}

/// `vv` as a registered tool. See the module docs.
pub struct VaultCommands {
    manifest: ToolManifest,
    palette: Palette,
    /// Which bundle to resolve against. One surface in v1 — the operator
    /// aimed the palette at Mneme in the config, and the gate below assumes
    /// it.
    bundle: String,
    /// The registry owns this tool and the dispatcher owns the registry, so
    /// the link back is weak and set after construction, as `tool_search`'s
    /// and the Rune host's are.
    dispatcher: OnceLock<Weak<Dispatcher>>,
}

impl VaultCommands {
    pub fn new(palette: Palette, bundle: impl Into<String>) -> Self {
        VaultCommands {
            manifest: ToolManifest {
                name: NAME.into(),
                description: DESCRIPTION.into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "One plain imperative vault command, e.g. `read the note Groceries`. A write's payload is not inline: leave its hole bare (`$a`), then a line `!!a`, the bytes exactly as they should be written, then a line `!!`.",
                        }
                    },
                    "required": ["command"]
                }),
                // Mutating, not read-only: the gate below still decides what may
                // run, and it may run three writes (see [`WRITES`]). Declaring
                // the tool read-shaped while it could edit a note would be the
                // one lie this manifest must not tell — policy reads this, and a
                // call admitted as a read is admitted without the question a
                // write would have raised.
                approval: Approval::Mutating,
                prompt: None,
                render: None,
                deferred: false,
            },
            palette,
            bundle: bundle.into(),
            dispatcher: OnceLock::new(),
        }
    }

    /// Link the chokepoint whose calls this resolves into, and which this
    /// also watches (see the `ToolObserver` half below).
    pub fn attach(&self, d: &Arc<Dispatcher>) {
        let _ = self.dispatcher.set(Arc::downgrade(d));
    }

    fn dispatcher(&self) -> Option<Arc<Dispatcher>> {
        self.dispatcher.get().and_then(Weak::upgrade)
    }

    /// Where this session's harvest goes: beside its own log, named for it.
    ///
    /// Read per attempt rather than captured, because a session can be
    /// adopted mid-process (the TUI's picker) and the harvest must follow the
    /// log the work lands in.
    async fn sidecar(&self) -> Option<(PathBuf, String)> {
        let d = self.dispatcher()?;
        let session = d.session().lock().await;
        let path = session.path().to_path_buf();
        let id = path.file_stem()?.to_str()?.to_string();
        Some((path.with_extension("verba.jsonl"), id))
    }

    /// One harvest record, appended. Never fails the call it describes.
    async fn harvest(&self, record: CommandRecord) {
        let Some((path, session_id)) = self.sidecar().await else {
            return;
        };
        let record = CommandRecord { session_id, ..record };
        append(&path, &record);
    }

    /// One command, end to end: classify, gate, dispatch, harvest — and the
    /// line the model reads back.
    ///
    /// **Never an error the caller has to recover from.** A classifier that
    /// will not answer is an abstention, and every refusal is a line saying
    /// why: the same invariant the operator's dispatch mode keeps, and the
    /// reason both entry seams can hand the line straight back.
    ///
    /// `trailing` is what the message said after this command when it came
    /// from prose ([`marked`]), and `None` when it came from the `vv` tool —
    /// where there is no message, so the field is unmeasured rather than
    /// zero.
    async fn run(
        &self,
        command: Command,
        trailing: Option<usize>,
        cancel: &CancellationToken,
    ) -> String {
        let Command { text, blocks } = command;
        let Some(d) = self.dispatcher() else {
            return ToolOutput::error("vv has no dispatcher attached yet").content;
        };
        let verdict = match self.palette.resolve_on(&self.bundle, &text).await {
            Ok((_, verdict)) => verdict,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "vv: the classifier did not answer; treating the command as an abstention");
                self.harvest(CommandRecord {
                    ts: now_ms(),
                    session_id: String::new(),
                    command: text,
                    trailing_chars: trailing,
                    verdict: VerdictRecord::unknown(),
                    gate: Gate::Abstain.as_str(),
                    executed: false,
                    result_status: None,
                    error_shape: None,
                })
                .await;
                return undispatched(Gate::Abstain);
            }
        };
        let decision = gate(&verdict, &blocks);
        let Some((function, args)) = decision.call else {
            self.harvest(CommandRecord {
                ts: now_ms(),
                session_id: String::new(),
                command: text,
                trailing_chars: trailing,
                verdict: VerdictRecord::of(&verdict),
                gate: decision.gate.as_str(),
                executed: false,
                result_status: None,
                error_shape: None,
            })
            .await;
            return undispatched(decision.gate);
        };
        let call = mneme_call(&function, args.clone(), CallOrigin::Script, &self.bundle);
        let out = d.dispatch(call, cancel.child_token()).await;
        let status = if out.is_error { "error" } else { "ok" };
        let shape = out.is_error.then(|| error_shape(&out.content));
        self.harvest(CommandRecord {
            ts: now_ms(),
            session_id: String::new(),
            command: text,
            trailing_chars: trailing,
            verdict: VerdictRecord::of(&verdict),
            gate: decision.gate.as_str(),
            executed: true,
            result_status: Some(status),
            error_shape: shape,
        })
        .await;
        executed_line(&function, &args, &out)
    }
}

#[async_trait]
impl Tool for VaultCommands {
    fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }

    async fn call(&self, input: Value, ctx: CallContext) -> anyhow::Result<ToolOutput> {
        let raw = input.get("command").and_then(Value::as_str).unwrap_or_default();
        let Some(command) = tool_command(raw) else {
            return Ok(ToolOutput::error(
                "vv needs a command — one plain imperative line, e.g. `read the note Groceries`",
            ));
        };
        if self.dispatcher().is_none() {
            return Ok(ToolOutput::error("vv has no dispatcher attached yet"));
        }
        Ok(ToolOutput::ok(self.run(command, None, &ctx.cancel).await))
    }
}

/// The inline half: the loop hands this the reply it just journaled, and gets
/// back one answer per command it marked — or `None`, which is the answer for
/// the ordinary reply that marks nothing.
///
/// Read [`crate::command`]'s module docs for the seam; what is worth saying
/// here is what this half does *not* do. It does not decide when to look
/// (the loop decides that, where a reply's tool results would land), it does
/// not decide what happens to the lines (the loop journals them as
/// [`eidolon_core::session::RecordKind::CommandResults`] and gives the model
/// another call to read them on), and it does not know the session, the
/// transcript, or the operator. It answers the message and returns.
///
/// A reply's commands run in the order it wrote them, all of them: they are
/// reads, and reads are cheap. There is no cap in this version, and one is
/// not obviously wanted — a cap would have to decide *which* of a model's
/// five reads not to run, and a model that asks for five reads and gets
/// three is worse off than one that waits a moment longer.
#[async_trait]
impl CommandChannel for VaultCommands {
    async fn answer(&self, message: &Message, cancel: CancellationToken) -> Option<Vec<String>> {
        let (commands, trailing) = marked(message);
        if commands.is_empty() {
            return None;
        }
        // No dispatcher is a wiring fault, not a command the model wrote
        // wrong: say so where the operator will see it and answer nothing.
        // Answering "undispatched" would be a line about a channel that is
        // not there, and it would cost a model call to deliver it.
        if self.dispatcher().is_none() {
            tracing::error!(
                commands = commands.len(),
                "vv: the inline channel has no dispatcher attached; the commands in this reply are not run"
            );
            return None;
        }
        let mut lines = Vec::with_capacity(commands.len());
        for command in commands {
            if cancel.is_cancelled() {
                break;
            }
            lines.push(self.run(command, Some(trailing), &cancel).await);
        }
        (!lines.is_empty()).then_some(lines)
    }
}

/// The observer half: the model's **own** structured vault calls, counted.
///
/// This is the measurement the channel is justified by — how often a
/// hand-written `mneme_rpc` call is rejected for the shape of its arguments —
/// and it exists on this side of the change because the same session's
/// sidecar already does, not because the channel needs it. Only calls the
/// *model* made count: the operator's own dispatch is a different question,
/// and this tool's nested calls are the channel's own work, already recorded.
#[async_trait]
impl ToolObserver for VaultCommands {
    async fn observed(&self, call: &ToolCall, output: &ToolOutput) {
        if call.origin != CallOrigin::Model || call.name != "mneme_rpc" {
            return;
        }
        let Some((path, session_id)) = self.sidecar().await else {
            return;
        };
        // Argument *names* only. The values are the operator's vault content
        // and this is a sidecar, not a journal.
        let arg_keys: Vec<String> = call
            .input
            .get("args")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        append(
            &path,
            &StructuredRecord {
                ts: now_ms(),
                session_id,
                kind: "structured",
                function: call
                    .input
                    .get("function")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                arg_keys,
                result_status: if output.is_error { "error" } else { "ok" },
                error_shape: output.is_error.then(|| error_shape(&output.content)),
            },
        );
    }
}

/// What the model reads when the gate refused: the reason, the instruction to
/// carry on with the ordinary tools, and a reminder that this is the designed
/// path rather than a fault.
fn undispatched(gate: Gate) -> String {
    format!(
        "[vv] undispatched (reason: {}) — use the vault tools as normal. Fallback is expected, not an error.",
        gate.as_str()
    )
}

/// What the model reads when the gate passed: the call that actually ran (its
/// *RPC* names, not the corpus's — a line that showed `note=` for a call that
/// sent `title=` would be the wrong half of the reconciliation) and the whole
/// of what came back, or the error.
///
/// **The answer is passed through, not trimmed.** It used to be clipped to a
/// couple of hundred characters with a `… (+N more chars)` tail, and trimmed
/// to one line — which made the channel a preview: a model that needed to
/// *read* something could not do it here, so it reached for `mneme_rpc` and
/// paid a structured call for what the channel had already fetched. Sizing an
/// answer is the vault's business, not the harness's; what the transcript
/// draws is a separate question and already has an answer (`tool_block`'s cap
/// and `space o`).
fn executed_line(function: &str, args: &Map<String, Value>, out: &ToolOutput) -> String {
    let mut call = function.to_string();
    for (key, value) in args {
        call.push_str(&format!(" {key}={value}"));
    }
    if out.is_error {
        format!("[vv] {call} → error: {}", out.content)
    } else {
        format!("[vv] {call} → ok: {}", out.content)
    }
}

/// Which structural class a failed call fell into.
///
/// `params` is the one that matters — an argument Mneme could not deserialize,
/// which is what a wrong parameter name or a missing one looks like from the
/// outside. The rest are kept apart so a genuine vault error (no such note) is
/// never counted as a naming failure.
fn error_shape(content: &str) -> &'static str {
    let c = content.to_ascii_lowercase();
    if c.contains("failed to deserialize parameters")
        || c.contains("missing field")
        || c.contains("unknown field")
        || c.contains("invalid type")
        || c.contains("invalid params")
        || c.contains("-32602")
    {
        "params"
    } else if c.contains("no function named") {
        "unknown_function"
    } else if c.contains("unknown tool")
        || c.contains("denied by policy")
        || c.contains("declined")
        || c.contains("cancelled")
    {
        "harness"
    } else {
        "remote"
    }
}

/// The verdict half of a harvest record. Every field is optional because a
/// verdict that never arrived is *unknown*, not zero — the difference the
/// harness keeps everywhere else.
#[derive(Serialize)]
struct VerdictRecord {
    intent: Option<String>,
    accept: Option<bool>,
    margin: Option<f64>,
    threshold: Option<f64>,
    conflicts: Option<Vec<Value>>,
    slots: Option<BTreeMap<String, Value>>,
    trailing_editorial_text: Option<String>,
}

impl VerdictRecord {
    fn of(v: &Verdict) -> Self {
        VerdictRecord {
            intent: Some(v.intent.clone()),
            accept: v.accept,
            margin: Some(v.margin),
            threshold: v.threshold,
            conflicts: Some(v.conflicts.clone()),
            slots: Some(v.slots.clone()),
            trailing_editorial_text: v.trailing_editorial_text.clone(),
        }
    }

    fn unknown() -> Self {
        VerdictRecord {
            intent: None,
            accept: None,
            margin: None,
            threshold: None,
            conflicts: None,
            slots: None,
            trailing_editorial_text: None,
        }
    }
}

/// One command attempt.
#[derive(Serialize)]
struct CommandRecord {
    ts: u128,
    session_id: String,
    command: String,
    /// Characters of assistant text after the **last** marked line in the
    /// message this command was intercepted from — turn-discipline telemetry,
    /// and the same number on every command of one message.
    ///
    /// `None` for a command that came from the `vv` tool: there is no
    /// message, so the question was never asked and the answer is unmeasured
    /// rather than zero. Zero here means something else entirely, and
    /// deliberately: the model asked and then stopped, which is what it is
    /// taught to do.
    trailing_chars: Option<usize>,
    verdict: VerdictRecord,
    gate: &'static str,
    executed: bool,
    result_status: Option<&'static str>,
    error_shape: Option<&'static str>,
}

/// One call the model composed itself.
#[derive(Serialize)]
struct StructuredRecord {
    ts: u128,
    session_id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: String,
    arg_keys: Vec<String>,
    result_status: &'static str,
    error_shape: Option<&'static str>,
}

/// Append one record. Best-effort throughout: the harvest is instrumentation,
/// and a sidecar that cannot be written is a line on stderr, never a failed
/// command.
fn append(path: &Path, record: &impl Serialize) {
    let line = match serde_json::to_string(record) {
        Ok(line) => line,
        Err(e) => {
            tracing::error!(error = %e, "vv: could not encode a harvest record");
            return;
        }
    };
    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{line}") {
                tracing::error!(error = %e, path = %path.display(), "vv: could not append to the harvest log");
            }
        }
        Err(e) => {
            tracing::error!(error = %e, path = %path.display(), "vv: could not open the harvest log");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `gate` over a command that carried no payload blocks — every read, and
    /// the refusal cases that never reach a fence.
    fn g(v: &Verdict) -> Decision {
        gate(v, &BTreeMap::new())
    }

    /// The command lines, marker already stripped: the shape the entry-seam
    /// tests compare against.
    fn texts(commands: &[Command]) -> Vec<&str> {
        commands.iter().map(|c| c.text.as_str()).collect()
    }

    /// Payload blocks, as the parser produces them.
    fn blocks(pairs: &[(char, &str)]) -> BTreeMap<char, String> {
        pairs.iter().map(|(c, v)| (*c, (*v).to_string())).collect()
    }

    fn verdict(intent: &str, accept: bool) -> Verdict {
        Verdict {
            intent: intent.into(),
            accept: Some(accept),
            intent_prob: 0.9,
            margin: 0.5,
            threshold: Some(0.39),
            ..Default::default()
        }
    }

    fn with_slots(intent: &str, slots: &[(&str, &str)]) -> Verdict {
        Verdict {
            slots: slots
                .iter()
                .map(|(k, v)| (k.to_string(), Value::from(*v)))
                .collect(),
            ..verdict(intent, true)
        }
    }

    /// A verdict with slots as the binary really prints them — which for the
    /// list-valued ones is a JSON array.
    fn with_typed_slots(intent: &str, slots: Value) -> Verdict {
        Verdict {
            slots: serde_json::from_value(slots).expect("slots object"),
            ..verdict(intent, true)
        }
    }

    /// Every intent this channel may run, end to end through the gate: it
    /// reaches the function Mneme has, under the argument names Mneme takes,
    /// with every argument Mneme requires bound. The reconciliation itself is
    /// [`crate::reconcile`]'s; what this proves is that the allowlist and that
    /// table agree.
    #[test]
    fn every_covered_read_reaches_the_function_mneme_has() {
        /// intent, corpus slots, expected function, expected args, required.
        type Case = (
            &'static str,
            &'static [(&'static str, &'static str)],
            &'static str,
            &'static [(&'static str, &'static str)],
            &'static [&'static str],
        );
        let cases: &[Case] = &[
            ("read_note", &[("note", "Groceries")], "read_note", &[("title", "\"Groceries\"")], &["title"]),
            ("read_canvas", &[("note", "Roadmap")], "read_canvas", &[("title", "\"Roadmap\"")], &["title"]),
            (
                "read_version",
                &[("note", "Groceries"), ("version", "7")],
                "read_version",
                &[("id", "\"7\""), ("note", "\"Groceries\"")],
                &["note", "id"],
            ),
            ("search_vault", &[("query", "token savings")], "search_vault", &[("query", "\"token savings\"")], &["query"]),
            ("find_backlinks", &[("note", "Groceries")], "find_backlinks", &[("note", "\"Groceries\"")], &["note"]),
            ("similar_notes", &[("ref", "Groceries")], "similar_notes", &[("ref", "\"Groceries\"")], &["ref"]),
            ("list_versions", &[("note", "Groceries")], "list_versions", &[("note", "\"Groceries\"")], &["note"]),
            ("semantic_query", &[("expr", "pooled([[X]])")], "semantic_query", &[("expr", "\"pooled([[X]])\"")], &["expr"]),
            ("semantic_centroid", &[("refs", "A, B")], "semantic_centroid", &[("refs", "\"A, B\"")], &["refs"]),
            ("semantic_subtract", &[("ref", "A"), ("subtract_refs", "B")], "semantic_subtract", &[("ref", "\"A\""), ("subtract_refs", "\"B\"")], &["ref", "subtract_refs"]),
            ("embed_text", &[("text", "hello world")], "embed_text", &[("text", "\"hello world\"")], &["text"]),
            ("skill_body", &[("name", "wiki-sync")], "skill_body", &[("name", "\"wiki-sync\"")], &["name"]),
            ("get_index", &[], "get_index", &[], &[]),
            ("list_notes", &[("folder", "wiki")], "list_notes", &[("folder", "\"wiki\"")], &[]),
            ("list_trash", &[], "list_trash", &[], &[]),
            ("list_handles", &[], "list_handles", &[], &[]),
            ("check_dead_links", &[], "check_dead_links", &[], &[]),
            ("check_orphans", &[], "check_orphans", &[], &[]),
        ];
        for (intent, slots, function, args, required) in cases {
            let decision = g(&with_slots(intent, slots));
            assert_eq!(decision.gate, Gate::Pass, "{intent}");
            let (got_function, got_args) = decision.call.expect("pass authorises a call");
            assert_eq!(&got_function, function, "{intent}");
            let expected: Map<String, Value> =
                args.iter().map(|(k, v)| (k.to_string(), v.parse().unwrap_or(Value::Null))).collect();
            assert_eq!(got_args, expected, "{intent} args");
            assert!(required.iter().all(|r| got_args.contains_key(*r)), "{intent} required");
        }
        // One case per allowlisted intent and one allowlisted intent per case —
        // `conventions` is the nineteenth, and has its own test below. An
        // intent added to `READS` without a case here would be a call nothing
        // proved.
        assert_eq!(cases.len() + 1, READS.len());
        for (intent, _) in READS {
            assert!(
                *intent == "conventions" || cases.iter().any(|(c, ..)| c == intent),
                "{intent} is allowed but nothing checks its call"
            );
        }
        // `list_notes` and the other argument-less reads take no arguments and
        // claim none.
        assert_eq!(g(&verdict("get_index", true)).call.unwrap().1.len(), 0);
    }

    /// The one intent whose *name* is decided by its own arguments.
    /// The one intent whose *name* is decided by its own arguments — and the
    /// one allowlisted intent with no case above, because its function is not
    /// one thing.
    #[test]
    fn conventions_resolves_to_the_canon_or_to_one_convention() {
        let all = g(&verdict("conventions", true)).call.unwrap();
        assert_eq!(all.0, "get_conventions");
        assert!(all.1.is_empty());
        // An empty slot is not a slug — the model bound nothing, so this is
        // the canon and not a lookup for the empty convention.
        let blank = g(&with_slots("conventions", &[("slug", "  ")])).call.unwrap();
        assert_eq!(blank.0, "get_conventions");
        let one = g(&with_slots("conventions", &[("slug", "capture")])).call.unwrap();
        assert_eq!(one.0, "read_convention");
        assert_eq!(one.1["name"], json!("capture"));
    }

    #[test]
    fn the_classifiers_trail_span_is_never_an_argument() {
        let decision = g(&with_slots("embed_text", &[("text", "hi"), ("TRAIL", "and also")]));
        let (_, args) = decision.call.expect("pass");
        assert_eq!(args, json!({ "text": "hi" }).as_object().unwrap().clone());
        // A bound slot this intent has no room for is kept: Mneme ignores an
        // argument it does not know, and dropping it here would hide a
        // binding that went nowhere.
        let (_, args) = g(&with_slots("skill_body", &[("name", "x"), ("target", "y")]))
            .call
            .expect("pass");
        assert!(args.contains_key("target"));
    }

    /// The order the refusal labels are read in, which is what the harvest
    /// counts. Every one of them refuses; only the label differs.
    #[test]
    fn the_gate_refuses_with_a_reason_that_names_the_slice() {
        // Not a command, and a command the classifier was not sure of.
        assert_eq!(g(&verdict("none", true)).gate, Gate::Abstain);
        assert_eq!(g(&verdict("UNK", true)).gate, Gate::Abstain);
        assert_eq!(g(&verdict("read_note", false)).gate, Gate::Abstain);
        // A conflict outranks everything after it.
        let conflicted = Verdict {
            conflicts: vec![json!({ "slot": "note" })],
            ..with_slots("create_note", &[("note", "x")])
        };
        assert_eq!(g(&conflicted).gate, Gate::Conflict);
        // A write outside the covered set is still refused, and named as such
        // before the editorial and arity checks — the write-shaped attempt is
        // the harvest data worth counting most clearly.
        let write = Verdict {
            trailing_editorial_text: Some("and also tell me the time".into()),
            ..with_slots("create_canvas", &[("note", "x")])
        };
        assert_eq!(g(&write).gate, Gate::NotReadShaped);
        assert!(g(&with_slots("nonsense", &[])).call.is_none());
        // Every write the shared table can name but this channel may not run:
        // the reconciliation knows how to spell them, and the gate still refuses.
        for write in ["create_canvas", "empty_trash", "update_note"] {
            let decision = g(&with_slots(write, &[("note", "x")]));
            assert_eq!(decision.gate, Gate::NotReadShaped, "{write}");
            assert!(decision.call.is_none(), "{write}");
        }
        // The three that this channel *may* run are not refused here: they fail
        // later, on the payload, which is the next test's subject.
        assert_eq!(
            g(&with_slots("append_to_note", &[("note", "x")])).gate,
            Gate::PayloadUndeclared
        );
        // Unclaimed text on a covered read.
        let editorial = Verdict {
            trailing_editorial_text: Some("also, what is the weather".into()),
            ..with_slots("read_note", &[("note", "Groceries")])
        };
        assert_eq!(g(&editorial).gate, Gate::Editorial);
        // An empty string is nothing left over, not text.
        let blank = Verdict {
            trailing_editorial_text: Some("  ".into()),
            ..with_slots("read_note", &[("note", "Groceries")])
        };
        assert_eq!(g(&blank).gate, Gate::Pass);
        // A read short of an argument Mneme requires: not dispatched, so the
        // model gets a count instead of a guaranteed error.
        assert_eq!(g(&with_slots("read_note", &[])).gate, Gate::MissingSlots);
        assert_eq!(
            g(&with_slots("read_version", &[("note", "Groceries")])).gate,
            Gate::MissingSlots
        );
        // The rename happens *before* the arity check, so a corpus-shaped slot
        // satisfies an RPC-shaped requirement.
        assert_eq!(
            g(&with_slots("read_note", &[("note", "Groceries")])).gate,
            Gate::Pass
        );
    }

    /// List-valued slots arrive as JSON arrays, are forwarded as arrays, and
    /// decode at all — a `slots: BTreeMap<String, String>` cannot hold one,
    /// and the dispatcher reads a line it cannot parse as a dead child.
    #[test]
    fn a_list_valued_slot_is_forwarded_as_a_list() {
        let centroid = g(&with_typed_slots(
            "semantic_centroid",
            json!({ "refs": ["Token Savings", "Project Envelope"] }),
        ));
        assert_eq!(centroid.gate, Gate::Pass);
        assert_eq!(centroid.call.unwrap().1["refs"], json!(["Token Savings", "Project Envelope"]));
        let subtract = g(&with_typed_slots(
            "semantic_subtract",
            json!({ "ref": "Token Savings", "subtract_refs": ["Melete"] }),
        ));
        assert_eq!(subtract.call.unwrap().1["subtract_refs"], json!(["Melete"]));
        // The whole line, exactly as the binary prints it.
        let line = r#"{"accept":true,"candidates":[],"conflicts":[],"delex":"what sits between $a and $b","intent":"semantic_centroid","intent_prob":0.95,"margin":0.9,"slots":{"refs":["Token Savings","Project Envelope"]},"threshold":0.39,"trailing_editorial_text":null,"utterance":"what sits between \"Token Savings\" and \"Project Envelope\""}"#;
        let verdict: Verdict = serde_json::from_str(line).expect("a list-slot verdict decodes");
        assert_eq!(g(&verdict).gate, Gate::Pass);
    }

    /// The type of an argument is the wire's, not the coercion's: Mneme's
    /// `read_version.id` is a string, so a stringly number must stay one —
    /// measured live, `{"id": 7}` comes back rejected. Which arguments are
    /// typed is [`crate::reconcile`]'s table; this checks it on the arguments
    /// this channel actually sends.
    #[test]
    fn only_the_arguments_the_wire_types_are_coerced() {
        let version = g(&with_slots("read_version", &[("note", "Groceries"), ("version", "7")]))
            .call
            .expect("pass");
        assert_eq!(version.1["id"], json!("7"), "id is a string on Mneme's side");
        let similar = g(&with_slots("similar_notes", &[("ref", "Groceries"), ("k", "5")]))
            .call
            .expect("pass");
        assert_eq!(similar.1["k"], json!(5), "k is a uint32 on Mneme's side");
        // A k that is not a number is sent as it arrived rather than guessed.
        let odd = g(&with_slots("similar_notes", &[("ref", "Groceries"), ("k", "many")]))
            .call
            .expect("pass");
        assert_eq!(odd.1["k"], json!("many"));
    }

    #[test]
    fn an_answer_comes_back_whole_because_sizing_it_is_the_vaults_business() {
        let short = executed_line("read_note", &json!({ "title": "Groceries" }).as_object().unwrap().clone(), &ToolOutput::ok("line one\nline two"));
        assert_eq!(
            short,
            "[vv] read_note title=\"Groceries\" → ok: line one\nline two",
            "the answer's own newlines are the vault's, not the harness's"
        );
        // The head of a long one used to be all the model saw, with the rest
        // counted as `… (+N more chars)` — which is exactly why a model that
        // needed to read reached for `mneme_rpc` instead.
        let body = "x".repeat(500);
        let line = executed_line("get_index", &Map::new(), &ToolOutput::ok(&body));
        assert_eq!(line, format!("[vv] get_index → ok: {body}"));
        assert!(!line.contains("more chars"), "{line}");
        let failed = executed_line("embed_text", &json!({ "text": "hi" }).as_object().unwrap().clone(), &ToolOutput::error("failed to deserialize parameters: missing field `text`"));
        assert_eq!(failed, "[vv] embed_text text=\"hi\" → error: failed to deserialize parameters: missing field `text`");
        // An empty answer is still a line.
        assert_eq!(executed_line("check_orphans", &Map::new(), &ToolOutput::ok("")), "[vv] check_orphans → ok: ");
    }

    #[test]
    fn an_undispatched_line_names_its_reason_and_sends_the_model_on() {
        for gate in [Gate::Abstain, Gate::Conflict, Gate::NotReadShaped, Gate::Editorial, Gate::MissingSlots] {
            let line = undispatched(gate);
            assert!(line.contains(&format!("(reason: {})", gate.as_str())), "{line}");
            assert!(line.contains("use the vault tools as normal"), "{line}");
        }
    }

    #[test]
    fn error_shapes_separate_structure_from_the_vaults_own_refusals() {
        assert_eq!(error_shape("failed to deserialize parameters: missing field `title` (code -32602)"), "params");
        assert_eq!(error_shape("Mneme tools/call failed: invalid params (code -32602)"), "params");
        assert_eq!(error_shape("RPC: no function named \"conventions\""), "unknown_function");
        assert_eq!(error_shape("unknown tool `mneme_rpc`"), "harness");
        assert_eq!(error_shape("No note \"Groceries\" found."), "remote");
        assert_eq!(error_shape("No convention named \"x\"."), "remote");
    }

    /// The note-shaped writes: admitted, reconciled onto Mneme's names, and —
    /// for the one that needs it — confirmed by the channel rather than by the
    /// caller.
    #[test]
    fn the_note_shaped_writes_are_admitted_and_reconciled() {
        // intent, the classifier's slots, the Mneme function, the args on the wire
        type Case = (
            &'static str,
            &'static [(&'static str, &'static str)],
            &'static str,
            Value,
        );
        let cases: &[Case] = &[
            (
                "move_note",
                &[("note", "A"), ("folder", "wiki")],
                "move_note",
                json!({ "title": "A", "folder": "wiki" }),
            ),
            (
                "rename_note",
                &[("note", "A"), ("name", "B")],
                "rename_note",
                json!({ "old_title": "A", "new_title": "B" }),
            ),
            (
                "delete_note",
                &[("note", "A")],
                "delete_note",
                json!({ "title": "A", "confirm": true }),
            ),
            ("restore_note", &[("note", "A")], "restore_note", json!({ "name": "A" })),
            (
                "restore_version",
                &[("note", "A"), ("version", "7")],
                "restore_version",
                json!({ "note": "A", "id": "7" }),
            ),
        ];
        for (intent, slots, function, want) in cases {
            let (f, args) = gate(&with_slots(intent, slots), &blocks(&[]))
                .call
                .unwrap_or_else(|| panic!("{intent} should pass"));
            assert_eq!(f, *function);
            assert_eq!(args, want.as_object().unwrap().clone(), "{intent}");
        }
        // Short of what Mneme requires is a count, not a call.
        assert_eq!(g(&with_slots("move_note", &[])).gate, Gate::MissingSlots);
        assert_eq!(
            g(&with_slots("restore_version", &[("note", "A")])).gate,
            Gate::MissingSlots
        );

        // A section rewrite carries its body in a fence like any other payload.
        let v = with_slots(
            "replace_section",
            &[("note", "N"), ("section", "Status"), ("content", "$a")],
        );
        let (function, args) = gate(&v, &blocks(&[('a', "the new body\n\nsecond line")]))
            .call
            .expect("pass");
        assert_eq!(function, "replace_section");
        assert_eq!(args["heading"], json!("Status"));
        assert_eq!(args["new_content"], json!("the new body\n\nsecond line"));
        assert_eq!(gate(&v, &blocks(&[])).gate, Gate::PayloadUnbound);
        // Confirmation is the channel's, and it is not a slot the tagger fills:
        // a `confirm` bound on the line is just an argument Mneme never asked
        // for, and the injected one is what the function reads.
        let (_, args) = g(&with_slots("delete_note", &[("note", "A"), ("confirm", "false")]))
            .call
            .expect("pass");
        assert_eq!(args["confirm"], json!(true));
    }

    /// A write's payload is carried, never inferred — and the role each block
    /// fills comes from the hole it names, not from the order it was written in.
    #[test]
    fn a_write_runs_only_when_its_payload_came_from_a_fence() {
        let bytes = "a paragraph,\nwith a newline and a \"quote\"";
        let v = with_slots("append_to_note", &[("note", "Build Log"), ("content", "$a")]);
        let (function, args) = gate(&v, &blocks(&[('a', bytes)])).call.expect("pass");
        assert_eq!(function, "append_to_note");
        assert_eq!(args["title"], json!("Build Log"));
        assert_eq!(args["content"], json!(bytes));

        // The same command with the payload left inline: the tagger inferred it.
        let inline = with_slots("append_to_note", &[("note", "Build Log"), ("content", "buy milk")]);
        assert_eq!(gate(&inline, &blocks(&[])).gate, Gate::PayloadUndeclared);
        // Declared, and no fence: there are no bytes to write.
        assert_eq!(gate(&v, &blocks(&[])).gate, Gate::PayloadUnbound);
        // A fence nothing asked for.
        assert_eq!(
            gate(&v, &blocks(&[('a', bytes), ('b', "extra")])).gate,
            Gate::BlockOrphan
        );
        // A read never carries one.
        assert_eq!(
            gate(&with_slots("read_note", &[("note", "Groceries")]), &blocks(&[('a', "x")])).gate,
            Gate::BlockOrphan
        );

        // Two payloads, each named on its fence and written out of order: the
        // roles come from the holes, so the blocks cannot be swapped.
        let v = with_slots("edit_note", &[("note", "N"), ("target", "$a"), ("content", "$b")]);
        let (function, args) = gate(&v, &blocks(&[('b', "new text"), ('a', "old text")]))
            .call
            .expect("pass");
        assert_eq!(function, "edit_note");
        assert_eq!(args["title"], json!("N"));
        assert_eq!(args["old_str"], json!("old text"));
        assert_eq!(args["new_str"], json!("new text"));
        // One hole named twice cannot be two payloads.
        let twice = with_slots("edit_note", &[("note", "N"), ("target", "$a"), ("content", "$a")]);
        assert_eq!(gate(&twice, &blocks(&[('a', "x")])).gate, Gate::PayloadUnbound);

        // A payload that arrives does not excuse a missing argument.
        let short = with_slots("create_note", &[("content", "$a")]);
        assert_eq!(gate(&short, &blocks(&[('a', "body")])).gate, Gate::MissingSlots);
        // And an empty payload is a payload: the fence was closed, so the bytes
        // to write are none, which is what was asked for.
        let empty = with_slots("create_note", &[("note", "N"), ("content", "$a")]);
        let (_, args) = gate(&empty, &blocks(&[('a', "")])).call.expect("pass");
        assert_eq!(args["content"], json!(""));
    }

    /// The fence, line by line: what it binds, what it carries verbatim, and
    /// what stays prose.
    #[test]
    fn a_fence_carries_bytes_and_nothing_else_does() {
        let (commands, trailing) = marked_block(
            "! append $a to the note \"Build Log\"\n!!a\nthe exact bytes\nwith a \"quote\" in them\n!!\n",
        );
        assert_eq!(texts(&commands), ["append $a to the note \"Build Log\""]);
        assert_eq!(
            commands[0].blocks[&'a'],
            "the exact bytes\nwith a \"quote\" in them"
        );
        assert_eq!(trailing, 0, "a block is part of its command");

        // Prose after the closing fence is prose again.
        let (_, trailing) =
            marked_block("! append $a to X\n!!a\nbytes\n!!\nand then there is this\n");
        assert_eq!(trailing, "and then there is this\n".chars().count());

        // Two blocks, bound by the letter they name rather than by order.
        let (commands, _) = marked_block("! replace $b with $a in X\n!!a\nnew\n!!\n!!b\nold\n!!\n");
        assert_eq!(commands[0].blocks[&'a'], "new");
        assert_eq!(commands[0].blocks[&'b'], "old");

        // An empty block is an empty payload, not a missing one.
        let (commands, _) = marked_block("! append $a to X\n!!a\n!!\n");
        assert_eq!(commands[0].blocks[&'a'], "");

        // A fence-shaped line *inside* a block is content: only a line that is
        // exactly `!!` closes.
        let (commands, _) = marked_block("! append $a to X\n!!a\n!!x\nbytes\n!!\n");
        assert_eq!(commands[0].blocks[&'a'], "!!x\nbytes");

        // An unterminated block never closes, so the command has no payload and
        // the gate refuses it (PayloadUnbound) rather than writing half a note.
        let (commands, _) = marked_block("! append $a to X\n!!a\nbytes\n");
        assert!(commands[0].blocks.is_empty());

        // Shapes the channel never described stay prose: a fence naming more
        // than one letter, and one naming none.
        for prose in ["! read the note A\n!!x y\nbytes\n!!\n", "! read the note A\n!!!\nbytes\n!!\n"] {
            let (commands, _) = marked_block(prose);
            assert!(commands[0].blocks.is_empty(), "{prose}");
        }
        // A fence with no command above it is prose too, and marks nothing.
        assert!(marked_block("!!a\nbytes\n!!\n").0.is_empty());

        // Line endings arrive normalised, so a CRLF reply cannot write `\r`.
        let (commands, _) = marked_block("! append $a to X\r\n!!a\r\nbytes\r\n!!\r\n");
        assert_eq!(commands[0].blocks[&'a'], "bytes");
    }

    /// A command written inside a code block is prose. The channel reads what it
    /// was taught to read; an example is not an instruction.
    #[test]
    fn a_fenced_example_is_not_a_command() {
        // The live specimen: a reply explaining the grammar with the grammar
        // fenced. The marked line in it was intercepted, resolved as an append,
        // and refused only by the gate that predated the writes.
        let reply = "the shape is:\n\n```\n! append $a to the note \"Build Log\"\n!!a\nthe bytes\n!!\n```\n\nand that is all.\n";
        let (commands, trailing) = marked_block(reply);
        assert!(commands.is_empty(), "{commands:?}");
        assert_eq!(trailing, 0, "nothing was asked, so there is nothing after it");

        // A real command after the closing fence still runs.
        let (commands, trailing) = marked_block("```\n! read the note A\n```\n! read the note B\n");
        assert_eq!(texts(&commands), ["read the note B"]);
        assert_eq!(trailing, 0);

        // An unterminated code block swallows the rest of the message.
        assert!(marked_block("```\n! read the note A\n").0.is_empty());

        // An indented fence is a fence — models nest them in lists — and the
        // payload fence is unaffected by any of it.
        let (commands, _) =
            marked_block("  ```\n! read the note A\n  ```\n! read the note B\n");
        assert_eq!(texts(&commands), ["read the note B"]);
        let (commands, _) = marked_block("! append $a to X\n!!a\nbytes\n!!\n");
        assert_eq!(commands[0].blocks[&'a'], "bytes");
    }

    /// The `vv` tool's parameter is the same grammar without the marker, and a
    /// write through it carries its payload the same way.
    #[test]
    fn the_tool_parameter_is_one_command_with_its_fences() {
        let command = tool_command("append $a to the note \"Build Log\"\n!!a\nthe bytes\n!!\n")
            .expect("one command");
        assert_eq!(command.text, "append $a to the note \"Build Log\"");
        assert_eq!(command.blocks[&'a'], "the bytes");
        // A model that has learned the inline shape writes the marker anyway.
        assert_eq!(tool_command("! read the note A").expect("one command").text, "read the note A");
        // Nothing to run.
        assert!(tool_command("   ").is_none());
        assert!(tool_command("").is_none());
    }

    /// VV's canonical-append dialect (`canonical-append-v1`): a deterministic
    /// verdict for `append to note "<json-quoted title>" $a` with no model run
    /// behind it — accept stamped true, probabilities a 0 sentinel, threshold
    /// null, and two fields this side has never heard of (`route`, `risk`).
    /// Nothing about that shape earns a new path: the wire decodes as any
    /// verdict, and the gate runs it exactly like a learned append — payload
    /// carried by a fence, bytes verbatim, the decoded title through
    /// [`reconcile`](crate::reconcile) untouched.
    #[test]
    fn the_canonical_append_verdict_runs_the_ordinary_gate_path() {
        // As the binary will print it. The title arrives decoded — the JSON
        // quoting belongs to the command line, and VV unwraps it before the
        // wire — and the payload hole is the bare `$a` the fence names.
        let wire = serde_json::json!({
            "utterance": "append to note \"Build \\\"Log\\\"\" $a",
            "delex": "append to note $a",
            "intent": "append_to_note",
            "intent_prob": 0.0,
            "margin": 0.0,
            "threshold": null,
            "accept": true,
            "slots": { "note": "Build \"Log\"", "content": "$a" },
            "candidates": [],
            "risk": null,
            "route": "canonical-append-v1",
        });
        let v: Verdict = serde_json::from_value(wire).expect("the canonical shape decodes");
        assert_eq!(v.intent, "append_to_note");
        assert_eq!(v.intent_prob, 0.0, "the sentinel is data here, never consulted");
        assert!(v.accepted());
        // Additive fields are the binary's business: they decode as nothing and
        // serialise as nothing, so a verdict this side has never heard of is
        // still just a verdict.
        let round: Value = serde_json::to_value(&v).unwrap();
        assert!(round.get("route").is_none() && round.get("risk").is_none(), "{round}");

        // Supplied: the fence's bytes are the payload, byte for byte.
        let payload = "the exact bytes\nwith a \"quote\", a \\ backslash,\nand a tab\ttoo";
        let d = gate(&v, &blocks(&[('a', payload)]));
        assert_eq!(d.gate, Gate::Pass);
        let (function, args) = d.call.expect("dispatched");
        assert_eq!(function, "append_to_note");
        assert_eq!(args["title"], Value::from("Build \"Log\""));
        assert_eq!(args["content"], Value::from(payload));
    }

    /// …and the canonical shape buys no relaxation: the payload still must be
    /// carried, blocks still must match holes, a refusal still refuses. The
    /// last part pins the wire contract in the other direction — the fields a
    /// canonical verdict may not omit, since a line missing one decodes as a
    /// dead classifier and the channel answers an abstention that would swallow
    /// the command silently.
    #[test]
    fn the_canonical_append_path_earns_no_gate_relaxation() {
        let wire = serde_json::json!({
            "utterance": "append to note \"Build \\\"Log\\\"\" $a",
            "delex": "append to note $a",
            "intent": "append_to_note",
            "intent_prob": 0.0,
            "margin": 0.0,
            "threshold": null,
            "accept": true,
            "slots": { "note": "Build \"Log\"", "content": "$a" },
            "candidates": [],
            "risk": null,
            "route": "canonical-append-v1",
        });
        let v: Verdict = serde_json::from_value(wire.clone()).unwrap();

        // Missing block: the hole was declared, nothing carried it.
        assert_eq!(gate(&v, &BTreeMap::new()).gate, Gate::PayloadUnbound);

        // Orphan: a second block nothing asked for.
        assert_eq!(
            gate(&v, &blocks(&[('a', "bytes"), ('b', "unrelated")])).gate,
            Gate::BlockOrphan
        );

        // Inline payload — the one thing the dialect must not buy: the slot
        // holds words instead of the hole, and no block can fix that.
        let mut inline = v.clone();
        inline.slots.insert("content".into(), Value::from("append this prose"));
        assert_eq!(gate(&inline, &BTreeMap::new()).gate, Gate::PayloadUndeclared);
        assert_eq!(gate(&inline, &blocks(&[('a', "bytes")])).gate, Gate::PayloadUndeclared);

        // A malformed canonical line refuses either way, and the label says
        // which reading failed: `accept: false` is an abstain — accept is
        // consulted before conflicts — while a contradictory binding that
        // keeps accept refuses as a conflict.
        let mut bad = v.clone();
        bad.accept = Some(false);
        bad.conflicts.push(Value::from("unterminated quoted title"));
        assert_eq!(g(&bad).gate, Gate::Abstain);
        let mut contradictory = v.clone();
        contradictory.conflicts.push(Value::from("unterminated quoted title"));
        assert_eq!(g(&contradictory).gate, Gate::Conflict);

        // The required fields, each one load-bearing: without it the verdict
        // does not decode at all.
        for required in ["utterance", "delex", "intent", "intent_prob", "margin"] {
            let mut stripped = wire.clone();
            stripped.as_object_mut().unwrap().remove(required);
            assert!(
                serde_json::from_value::<Verdict>(stripped).is_err(),
                "`{required}` must stay required"
            );
        }
    }

    /// The marker convention, line by line. What is a command and what is
    /// prose is the whole of the entry seam's surface, and the space after
    /// the bang is what separates a command from a markdown image.
    #[test]
    fn a_command_is_a_line_that_begins_with_the_marker() {
        let (commands, trailing) = marked_block("! read the note Groceries");
        assert_eq!(texts(&commands), ["read the note Groceries"]);
        assert_eq!(trailing, 0, "the command was the whole block");

        // Several, in the order they were written; the trailing count is
        // measured once, from the last of them.
        let (commands, trailing) = marked_block("here we go:\n! read the note A\n! search for B\n");
        assert_eq!(texts(&commands), ["read the note A", "search for B"]);
        assert_eq!(trailing, 0, "the last command ended the message");

        // What comes after the last command is the discipline number.
        let (commands, trailing) = marked_block("! read the note A\nthen, some prose about it.\n");
        assert_eq!(texts(&commands), ["read the note A"]);
        assert_eq!(trailing, "then, some prose about it.\n".chars().count());

        // Whitespace a model left around the command is not part of it.
        let (commands, _) = marked_block("!   read the note A  \n");
        assert_eq!(texts(&commands), ["read the note A"]);
    }

    /// Everything that looks like a command and is not.
    #[test]
    fn prose_is_not_a_command() {
        // A markdown image begins with a bang. It is a picture.
        assert!(marked_block("![alt text](shot.png)").0.is_empty());
        // Mid-line, whichever way the spacing falls.
        assert!(marked_block("look at this! read the note A").0.is_empty());
        assert!(marked_block("here ! and there").0.is_empty());
        // A bare bang, and a marker with nothing behind it: nothing to run.
        assert!(marked_block("!").0.is_empty());
        assert!(marked_block("! \n").0.is_empty());
        assert!(marked_block("!   ").0.is_empty());
        // Indented is prose: the convention is taught at the margin.
        assert!(marked_block("  ! read the note A").0.is_empty());
        assert!(marked_block("\t! read the note A").0.is_empty());
        // A closed fence or an empty message is not a command either.
        assert!(marked_block("").0.is_empty());
        assert!(marked_block("nothing here\n").0.is_empty());
    }

    /// Blocks are scanned one at a time, and the trailing count crosses
    /// them: the rest of the block the last command was in, plus every
    /// block after it.
    #[test]
    fn the_message_is_scanned_block_by_block() {
        let msg = Message::assistant(vec![
            ContentBlock::text("let me check.\n"),
            ContentBlock::text("! read the note A\n"),
            ContentBlock::text("waiting on it.\n"),
        ]);
        let (commands, trailing) = marked(&msg);
        assert_eq!(texts(&commands), ["read the note A"]);
        assert_eq!(trailing, "waiting on it.\n".chars().count());

        // Two blocks that each hold a command: both run, and the number
        // counts from the last of them.
        let msg = Message::assistant(vec![
            ContentBlock::text("! read the note A\n"),
            ContentBlock::text("! read the note B\n"),
            ContentBlock::text("and then?\n"),
        ]);
        let (commands, trailing) = marked(&msg);
        assert_eq!(texts(&commands), ["read the note A", "read the note B"]);
        assert_eq!(trailing, "and then?\n".chars().count());

        // Nothing marked anywhere: the channel answers `None`.
        let msg = Message::assistant(vec![ContentBlock::text("hello there")]);
        assert!(marked(&msg).0.is_empty());
    }

    /// The inline teaching is the tool's own words with one paragraph in
    /// front of it — the two seams must not teach two vocabularies.
    #[test]
    fn the_inline_teaching_is_the_tools_words_with_the_marker_rules_in_front() {
        let prompt = inline_prompt();
        assert!(prompt.contains("`! `"), "{prompt}");
        assert!(prompt.contains("END of your message and then stop"), "{prompt}");
        assert!(
            prompt.ends_with(DESCRIPTION),
            "the coverage half is the tool's description, verbatim: {prompt}"
        );
        assert!(prompt.contains("fallback is expected, not an error"));
    }
}

/// The end-to-end half: a real dispatcher, a real session log, a real
/// classifier child (a shell script standing in for `verba-volantia`), and a
/// stub `mneme_rpc` — so what is asserted is the wiring the harness actually
/// has, not a mock of it.
#[cfg(all(test, unix))]
mod integration {
    use std::sync::Mutex;

    use eidolon_core::agent::{Agent, AgentConfig, TurnOutcome};
    use eidolon_core::event::EventBus;
    use eidolon_core::message::ContentBlock;
    use eidolon_core::testing::ScriptedProvider;
    use eidolon_core::policy::AllowAll;
    use eidolon_core::session::Session;
    use eidolon_core::testing::ScriptedUser;
    use eidolon_core::tool::ToolRegistry;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::palette::Target;
    use crate::{Bundle, VerbaDispatcher};

    /// The line the model should read back for the command above.
    const LINE: &str = "[vv] read_note title=\"Groceries\" → ok: Groceries: eggs, milk";

    /// Everything a message list *says*, as one string — the text of the
    /// model's messages and the content of its tool results, not their debug
    /// form (which would escape the quotes this asserts on).
    fn said(messages: &[eidolon_core::message::Message]) -> String {
        messages
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A `mneme_rpc` that answers with whatever the test told it to, and
    /// remembers what it was asked.
    struct Stub {
        manifest: ToolManifest,
        reply: ToolOutput,
        seen: Mutex<Vec<Value>>,
    }

    impl Stub {
        fn new(reply: ToolOutput) -> Arc<Self> {
            Arc::new(Stub {
                manifest: ToolManifest {
                    name: "mneme_rpc".into(),
                    description: "Call one function on the Mneme vault server.".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "function": { "type": "string" },
                            "args": { "type": "object" }
                        },
                        "required": ["function"]
                    }),
                    approval: Approval::ReadOnly,
                    prompt: None,
                    render: None,
                    deferred: false,
                },
                reply,
                seen: Mutex::new(Vec::new()),
            })
        }

        fn asked(&self) -> Vec<Value> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Tool for Stub {
        fn manifest(&self) -> &ToolManifest {
            &self.manifest
        }
        async fn call(&self, input: Value, _: CallContext) -> anyhow::Result<ToolOutput> {
            self.seen.lock().unwrap().push(input);
            Ok(self.reply.clone())
        }
    }

    struct Fixture {
        dispatcher: Arc<Dispatcher>,
        stub: Arc<Stub>,
        /// The same channel the tool is: the inline seam is this object
        /// answering a message rather than a call.
        commands: Arc<VaultCommands>,
        sidecar: PathBuf,
        _dir: tempfile::TempDir,
    }

    /// One canned verdict per command — the classifier child answers the same
    /// thing to whatever line it is handed, which is all any one case needs.
    fn fixture(reply: ToolOutput, intent: &str, accept: bool, slots: &[(&str, &str)]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let verdict = json!({
            "utterance": "x",
            "delex": "x",
            "intent": intent,
            "intent_prob": 0.9,
            "margin": 0.5,
            "threshold": 0.39,
            "accept": accept,
            "slots": slots.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<std::collections::BTreeMap<_, _>>(),
            "candidates": [],
            "conflicts": [],
            "trailing_editorial_text": null,
        });
        let binary = dir.path().join("fake-verba");
        // The JSON has no single quote in it, so a shell literal is safe.
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\nwhile IFS= read -r line; do\nprintf '%s\\n' '{}'\ndone\n",
                serde_json::to_string(&verdict).unwrap()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let weights = dir.path().join("weights");
        std::fs::create_dir_all(&weights).unwrap();
        let mut palette = Palette::new();
        palette.add(Bundle::new(
            "mneme",
            Target::MnemeRpc,
            VerbaDispatcher::new(binary.display().to_string(), weights.display().to_string()),
        ));
        let log = dir.path().join("1789000000000.eid");
        let session = Session::create(&log, "test", dir.path(), None).unwrap();
        let stub = Stub::new(reply);
        let commands = Arc::new(VaultCommands::new(palette, "mneme"));
        let mut reg = ToolRegistry::new();
        reg.register(stub.clone());
        reg.register(commands.clone());
        let dispatcher = Arc::new(Dispatcher::new(
            reg,
            Arc::new(AllowAll),
            ScriptedUser::new(false),
            EventBus::default(),
            Arc::new(AsyncMutex::new(session)),
            dir.path().to_path_buf(),
        ));
        commands.attach(&dispatcher);
        dispatcher.observe(commands.clone());
        Fixture {
            dispatcher,
            stub,
            commands,
            sidecar: log.with_extension("verba.jsonl"),
            _dir: dir,
        }
    }

    impl Fixture {
        /// The `vv` call a model makes.
        async fn command(&self, command: &str) -> ToolOutput {
            self.dispatcher
                .dispatch(
                    ToolCall {
                        id: "tu_1".into(),
                        name: NAME.into(),
                        input: json!({ "command": command }),
                        origin: CallOrigin::Model,
                    },
                    CancellationToken::new(),
                )
                .await
        }

        /// The model's own hand-written call, on the path the channel is
        /// replacing.
        async fn structured(&self, function: &str, args: Value, origin: CallOrigin) -> ToolOutput {
            self.dispatcher
                .dispatch(
                    ToolCall {
                        id: "tu_2".into(),
                        name: "mneme_rpc".into(),
                        input: json!({ "function": function, "args": args }),
                        origin,
                    },
                    CancellationToken::new(),
                )
                .await
        }

        fn records(&self) -> Vec<Value> {
            std::fs::read_to_string(&self.sidecar)
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).expect("one JSON record per line"))
                .collect()
        }
    }

    /// A write the channel may now run, end to end: the payload arrives in a
    /// fence, the classifier sees the same bare hole it always saw, and Mneme
    /// gets the bytes verbatim under its own argument name.
    #[tokio::test]
    async fn a_fenced_write_dispatches_with_the_bytes_it_carried() {
        let f = fixture(
            ToolOutput::ok("Appended 2 lines."),
            "append_to_note",
            true,
            &[("note", "Build Log"), ("content", "$a")],
        );
        let out = f
            .command("append $a to the note \"Build Log\"\n!!a\nthe bytes,\nwith a newline\n!!\n")
            .await;
        assert!(!out.is_error, "the channel is not an error path: {}", out.content);
        assert_eq!(
            f.stub.asked(),
            vec![json!({
                "function": "append_to_note",
                "args": { "title": "Build Log", "content": "the bytes,\nwith a newline" }
            })]
        );
        let records = f.records();
        assert_eq!(records.len(), 1, "one attempt, one record: {records:?}");
        assert_eq!(records[0]["gate"], "pass");
        assert_eq!(records[0]["executed"], true);
        assert_eq!(records[0]["result_status"], "ok");
        // The command the harvest keeps is the line, not the payload: the
        // sidecar is a record of what was asked, not a second copy of the note.
        assert_eq!(records[0]["command"], "append $a to the note \"Build Log\"");

        // And the same write through the *inline* seam, where the command is a
        // marked line and the fence is a block below it.
        let f = fixture(
            ToolOutput::ok("Appended 2 lines."),
            "append_to_note",
            true,
            &[("note", "Build Log"), ("content", "$a")],
        );
        let (agent, provider) = inline(
            &f,
            vec![
                ScriptedProvider::text(
                    "appending.\n\n! append $a to the note \"Build Log\"\n!!a\nthe bytes,\nwith a newline\n!!\n",
                ),
                ScriptedProvider::text("noted."),
            ],
        );
        agent
            .run_turn(vec![ContentBlock::text("add that line")], CancellationToken::new())
            .await
            .expect("the turn runs");
        assert_eq!(
            f.stub.asked(),
            vec![json!({
                "function": "append_to_note",
                "args": { "title": "Build Log", "content": "the bytes,\nwith a newline" }
            })]
        );
        let records = f.records();
        assert_eq!(records[0]["gate"], "pass", "{records:?}");
        // The block belongs to its command, so the discipline number is zero —
        // the reply stopped where the payload did.
        assert_eq!(records[0]["trailing_chars"], 0, "{records:?}");
        assert_eq!(provider.requests.lock().unwrap().len(), 2, "the answers are read in the same turn");
    }

    /// The whole slice, once: a prose command goes in, a reconciled Mneme read
    /// comes out, and the model reads one line about it.
    #[tokio::test]
    async fn a_read_is_reconciled_dispatched_and_harvested() {
        let f = fixture(ToolOutput::ok("Groceries: eggs, milk"), "read_note", true, &[("note", "Groceries")]);
        let out = f.command("read the note Groceries").await;
        assert!(!out.is_error, "the channel is not an error path: {}", out.content);
        assert_eq!(out.content, "[vv] read_note title=\"Groceries\" → ok: Groceries: eggs, milk");
        // The call that actually reached Mneme carries Mneme's argument name,
        // and it went through the chokepoint rather than around it.
        assert_eq!(
            f.stub.asked(),
            vec![json!({ "function": "read_note", "args": { "title": "Groceries" } })]
        );
        let records = f.records();
        assert_eq!(records.len(), 1, "one attempt, one record: {records:?}");
        let r = &records[0];
        assert_eq!(r["gate"], "pass");
        assert_eq!(r["executed"], true);
        assert_eq!(r["result_status"], "ok");
        assert_eq!(r["error_shape"], Value::Null);
        assert_eq!(r["command"], "read the note Groceries");
        assert_eq!(r["session_id"], "1789000000000");
        // A command that came from the tool has no message behind it, so the
        // discipline number is unmeasured rather than zero.
        assert_eq!(r["trailing_chars"], Value::Null, "{r}");
        assert_eq!(r["verdict"]["intent"], "read_note");
        assert_eq!(r["verdict"]["accept"], true);
        assert!(r["verdict"]["margin"].is_number());
        // The channel's own nested call is not the model's structured one: it
        // is Script-origin and already counted above.
        assert!(records.iter().all(|r| r["type"] != "structured"));
    }

    /// A write-shaped command that is not one of the covered three is the
    /// harvest's most valuable row, and it must not run.
    #[tokio::test]
    async fn a_write_shaped_command_is_undispatched_and_counted() {
        let f = fixture(ToolOutput::ok("created"), "create_canvas", true, &[("note", "Foo")]);
        let out = f.command("make a blank canvas called Foo").await;
        assert!(!out.is_error);
        assert_eq!(
            out.content,
            "[vv] undispatched (reason: not_read_shaped) — use the vault tools as normal. Fallback is expected, not an error."
        );
        assert!(f.stub.asked().is_empty(), "nothing ran");
        let records = f.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["gate"], "not_read_shaped");
        assert_eq!(records[0]["executed"], false);
        assert_eq!(records[0]["result_status"], Value::Null);
        assert_eq!(records[0]["verdict"]["intent"], "create_canvas");
    }

    /// A covered write whose payload was never carried is refused for that
    /// reason, and the reason is the row: the payload labels are what this
    /// change is measured by.
    #[tokio::test]
    async fn a_write_with_an_inferred_payload_is_undispatched_and_counted() {
        let f = fixture(ToolOutput::ok("created"), "create_note", true, &[("note", "Foo")]);
        let out = f.command("create a note called Foo").await;
        assert!(!out.is_error);
        assert_eq!(
            out.content,
            "[vv] undispatched (reason: payload_undeclared) — use the vault tools as normal. Fallback is expected, not an error."
        );
        assert!(f.stub.asked().is_empty(), "nothing ran");
        let records = f.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["gate"], "payload_undeclared");
        assert_eq!(records[0]["executed"], false);
        assert_eq!(records[0]["verdict"]["intent"], "create_note");
    }

    /// The binding gap the lead's probe found: an accepted read with nothing
    /// bound is a count, not a dispatched call and a guaranteed error.
    #[tokio::test]
    async fn a_read_short_of_a_slot_is_undispatched_rather_than_failed() {
        let f = fixture(ToolOutput::ok(""), "read_note", true, &[]);
        let out = f.command("read the note Groceries").await;
        assert!(out.content.contains("(reason: missing_slots)"), "{}", out.content);
        assert!(f.stub.asked().is_empty());
        assert_eq!(f.records()[0]["gate"], "missing_slots");
        assert_eq!(f.records()[0]["executed"], false);
    }

    #[tokio::test]
    async fn a_sub_threshold_or_none_verdict_abstains() {
        let none = fixture(ToolOutput::ok(""), "none", true, &[]);
        none.command("tell me about the weather").await;
        assert_eq!(none.records()[0]["gate"], "abstain");
        let unsure = fixture(ToolOutput::ok(""), "read_note", false, &[("note", "X")]);
        unsure.command("read the note X").await;
        assert_eq!(unsure.records()[0]["gate"], "abstain");
        assert!(unsure.stub.asked().is_empty());
    }

    /// A dispatch that fails on the vault's side is reported, shaped, and
    /// still a harvest row.
    #[tokio::test]
    async fn a_failed_dispatch_is_reported_and_shaped() {
        let f = fixture(
            ToolOutput::error("failed to deserialize parameters: missing field `title` (code -32602)"),
            "read_note",
            true,
            &[("note", "Groceries")],
        );
        let out = f.command("read the note Groceries").await;
        assert!(out.content.starts_with("[vv] read_note title=\"Groceries\" → error: failed to deserialize"), "{}", out.content);
        let records = f.records();
        assert_eq!(records[0]["result_status"], "error");
        assert_eq!(records[0]["error_shape"], "params");
    }

    /// The measurement's other half: the model's own structured call is
    /// recorded, and only the model's.
    #[tokio::test]
    async fn the_models_own_structured_calls_are_observed_and_script_ones_are_not() {
        let f = fixture(ToolOutput::error("failed to deserialize parameters: missing field `query`"), "get_index", true, &[]);
        f.structured("search_vault", json!({ "q": "x" }), CallOrigin::Model).await;
        f.structured("read_note", json!({ "title": "Groceries" }), CallOrigin::Script).await;
        let records = f.records();
        assert_eq!(records.len(), 1, "{records:?}");
        let r = &records[0];
        assert_eq!(r["type"], "structured");
        assert_eq!(r["function"], "search_vault");
        assert_eq!(r["arg_keys"], json!(["q"]));
        assert_eq!(r["result_status"], "error");
        assert_eq!(r["error_shape"], "params");
        assert_eq!(r["session_id"], "1789000000000");
        // Names only: the value the model typed is not in the sidecar.
        assert!(!serde_json::to_string(r).unwrap().contains("\"x\""));
    }

    /// The whole point, at loop level: the model writes *one line*, the loop
    /// serialises and runs it, and the model reads the result back as the
    /// answer to its own call — not as a synthesised interruption.
    ///
    /// That last part is the load-bearing one. A nested call made with
    /// `CallOrigin::User` would journal a `UserToolCall` between the model's
    /// `vv` tool_use and its result, and the session's message assembly
    /// answers every open call it meets one — so the transcript would tell
    /// the model its own command had been interrupted and then hand it the
    /// vault's answer as unrelated text.
    #[tokio::test]
    async fn the_loop_reads_one_line_back_for_its_own_command() {
        let f = fixture(ToolOutput::ok("Groceries: eggs, milk"), "read_note", true, &[("note", "Groceries")]);
        let provider = ScriptedProvider::new(vec![
            ScriptedProvider::tool("tu_vv", NAME, json!({ "command": "read the note Groceries" })),
            ScriptedProvider::text("eggs and milk, then."),
        ]);
        let agent = Agent::new(
            provider.clone(),
            f.dispatcher.clone(),
            AgentConfig {
                model: "scripted".into(),
                ..Default::default()
            },
        );
        let outcome = agent
            .run_turn(vec![ContentBlock::text("what is on the list?")], CancellationToken::new())
            .await
            .expect("the turn runs");
        assert!(matches!(outcome, TurnOutcome::Settled { .. }), "{outcome:?}");
        // The next request carries the result of the call the model made.
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "one call for the command, one to settle");
        let second = said(&requests[1].messages);
        assert!(second.contains(LINE), "{second}");
        assert!(!second.contains("interrupted before it produced a result"), "{second}");
        drop(requests);
        // And the journal is truth: reopening the log reads the same line back
        // as the answer to that call.
        let reopened = Session::open(&f._dir.path().join("1789000000000.eid")).unwrap();
        let journaled = said(&reopened.messages());
        assert!(journaled.contains(LINE), "{journaled}");
        let records = f.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0]["gate"], "pass");
        assert_eq!(records[0]["result_status"], "ok");
    }

    /// A classifier that will not answer is an abstention, never an error the
    /// turn has to recover from.
    #[tokio::test]
    async fn a_dead_classifier_abstains() {
        let mut f = fixture(ToolOutput::ok(""), "read_note", true, &[("note", "X")]);
        let binary = f._dir.path().join("fake-verba");
        std::fs::write(&binary, "#!/bin/sh\nwhile IFS= read -r line; do :; done\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // The palette holds the path it was built with, so point a second
        // fixture's tool at the silent child instead: the dispatcher is the
        // same, the child is not.
        let out = f.command("read the note X").await;
        assert!(!out.is_error, "{}", out.content);
        // The first child answered (the file was replaced after it started),
        // so this asserts the *shape* of every outcome rather than which one.
        assert!(
            out.content.starts_with("[vv] read_note") || out.content.contains("undispatched"),
            "{}",
            out.content
        );
        drop(f);
        f = fixture(ToolOutput::ok(""), "read_note", true, &[("note", "X")]);
        assert!(f._dir.path().exists());
    }

    /// Build a loop whose model writes prose, with the inline channel armed.
    /// The fixture's dispatcher and its fake classifier child are the real
    /// ones — what is scripted is the model, and what is read back is what
    /// the harness actually sent it.
    fn inline(
        f: &Fixture,
        replies: Vec<Vec<eidolon_core::StreamEvent>>,
    ) -> (Agent, Arc<ScriptedProvider>) {
        let provider = ScriptedProvider::new(replies);
        let agent = Agent::new(
            provider.clone(),
            f.dispatcher.clone(),
            AgentConfig {
                model: "scripted".into(),
                ..Default::default()
            },
        );
        agent.set_command_channel(f.commands.clone());
        (agent, provider)
    }

    /// The inline seam, end to end: the model writes a marked line in prose,
    /// the loop intercepts the finished message, the same reconciled read
    /// runs through the same chokepoint, and the model reads the answer back
    /// as the answer to its own line — on the next call of the same turn,
    /// journaled so a resumed session reads the same thing.
    #[tokio::test]
    async fn a_marked_line_in_prose_is_intercepted_and_answered_in_the_same_turn() {
        let f = fixture(ToolOutput::ok("Groceries: eggs, milk"), "read_note", true, &[("note", "Groceries")]);
        let (agent, provider) = inline(
            &f,
            vec![
                ScriptedProvider::text("let me look.\n! read the note Groceries\n"),
                ScriptedProvider::text("eggs and milk, then."),
            ],
        );
        let outcome = agent
            .run_turn(vec![ContentBlock::text("what is on the list?")], CancellationToken::new())
            .await
            .expect("the turn runs");
        assert!(matches!(outcome, TurnOutcome::Settled { .. }), "{outcome:?}");
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "one call for the command, one to answer from it");
        let second = said(&requests[1].messages);
        assert!(second.contains(LINE), "{second}");
        assert!(
            second.contains("From the harness — not the operator"),
            "the answers are the harness's words, not the operator's: {second}"
        );
        assert!(!second.contains("interrupted before it produced a result"), "{second}");
        drop(requests);
        // The same call reached Mneme, under Mneme's argument name.
        assert_eq!(
            f.stub.asked(),
            vec![json!({ "function": "read_note", "args": { "title": "Groceries" } })]
        );
        // And the log is truth: reopening the session reads the answer as
        // the answer to the reply that asked.
        let reopened = Session::open(&f._dir.path().join("1789000000000.eid")).unwrap();
        let journaled = said(&reopened.messages());
        assert!(journaled.contains(LINE), "{journaled}");
        assert!(journaled.contains("From the harness — not the operator"), "{journaled}");
        let records = f.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0]["gate"], "pass");
        assert_eq!(records[0]["result_status"], "ok");
        assert_eq!(records[0]["trailing_chars"], 0, "the command ended the reply");
    }

    /// What the model reads back is the vault's own words, whole.
    ///
    /// The channel used to hand it the first 200 characters with a
    /// `… (+N more chars)` tail — invisible to the short answers above, and
    /// the reason a model that needed to *read* something reached for
    /// `mneme_rpc` instead, paying a structured call for text the channel had
    /// already fetched. One marked line, a note longer than that clip, and the
    /// next request of the turn carries all of it, newlines and all.
    #[tokio::test]
    async fn a_marked_line_hands_the_model_the_whole_answer() {
        let body = (0..40)
            .map(|i| format!("line {i} of the note"))
            .collect::<Vec<_>>()
            .join("\n");
        let f = fixture(ToolOutput::ok(&body), "read_note", true, &[("note", "Big")]);
        let (agent, provider) = inline(
            &f,
            vec![
                ScriptedProvider::text("reading it.\n! read the note Big\n"),
                ScriptedProvider::text("read."),
            ],
        );
        let outcome = agent
            .run_turn(vec![ContentBlock::text("what does it say?")], CancellationToken::new())
            .await
            .expect("the turn runs");
        assert!(matches!(outcome, TurnOutcome::Settled { .. }), "{outcome:?}");
        let requests = provider.requests.lock().unwrap();
        let second = said(&requests[1].messages);
        assert!(
            second.contains("line 39 of the note"),
            "the end of the note, not a count of it: {second}"
        );
        assert!(!second.contains("more chars"), "nothing was cut: {second}");
        drop(requests);
        // And the log is truth, so a resume reads the same whole answer.
        let reopened = Session::open(&f._dir.path().join("1789000000000.eid")).unwrap();
        let journaled = said(&reopened.messages());
        assert!(journaled.contains("line 39 of the note"), "{journaled}");
    }

    /// Every marked line runs, in the order it was written, all of them —
    /// and each record carries the same turn-discipline number: how much of
    /// the reply came after the last command.
    #[tokio::test]
    async fn every_marked_line_runs_and_the_discipline_number_is_measured() {
        let f = fixture(ToolOutput::ok("found"), "search_vault", true, &[("query", "eggs")]);
        let after = "and let me know.";
        let (agent, provider) = inline(
            &f,
            vec![
                ScriptedProvider::text(&format!(
                    "checking.\n! search for eggs\n! search for eggs\n{after}"
                )),
                ScriptedProvider::text("found it."),
            ],
        );
        agent
            .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
            .await
            .expect("the turn runs");
        assert_eq!(provider.requests.lock().unwrap().len(), 2);
        assert_eq!(f.stub.asked().len(), 2, "both commands ran, in order");
        let second = said(&provider.requests.lock().unwrap()[1].messages);
        assert_eq!(second.matches("[vv] search_vault query=\"eggs\" → ok: found").count(), 2, "{second}");
        let records = f.records();
        assert_eq!(records.len(), 2, "one record per command: {records:?}");
        assert_eq!(records[0]["command"], "search for eggs");
        assert_eq!(records[1]["command"], "search for eggs");
        for r in &records {
            assert_eq!(r["gate"], "pass");
            assert_eq!(
                r["trailing_chars"],
                after.chars().count(),
                "the same message-level number on every command: {r}"
            );
        }
    }

    /// A refusal in prose answers in prose: the model gets the
    /// `undispatched` line and a continuation to read it on, and nothing
    /// reached the vault.
    #[tokio::test]
    async fn a_refused_command_in_prose_answers_undispatched_and_carries_on() {
        let f = fixture(ToolOutput::ok("created"), "create_canvas", true, &[("note", "Foo")]);
        let (agent, provider) = inline(
            &f,
            vec![
                ScriptedProvider::text("! make a blank canvas called Foo\n"),
                ScriptedProvider::text("then I cannot write it."),
            ],
        );
        agent
            .run_turn(vec![ContentBlock::text("make a note")], CancellationToken::new())
            .await
            .expect("the turn runs");
        assert!(f.stub.asked().is_empty(), "a write-shaped command does not run");
        assert_eq!(provider.requests.lock().unwrap().len(), 2, "the refusal still answers");
        let second = said(&provider.requests.lock().unwrap()[1].messages);
        assert!(second.contains("undispatched (reason: not_read_shaped)"), "{second}");
        assert!(second.contains("use the vault tools as normal"), "{second}");
        let records = f.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0]["gate"], "not_read_shaped");
        assert_eq!(records[0]["executed"], false);
        assert_eq!(records[0]["verdict"]["intent"], "create_canvas");
    }
}
