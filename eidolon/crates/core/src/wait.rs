//! Waiting: a park on a condition, and the wake it buys.
//!
//! The alternative this replaces is polling, and polling is not a matter of
//! taste here — it is the thing that does not work. A model can read a
//! background command's log or call `peers`, but it cannot sample a peer's
//! *transition*, and every attempt to approximate one inside a turn either
//! burns a model call per sample or holds the turn open on a `bash sleep`.
//! So the harness takes the sampling: the model names a condition, the loop
//! settles the turn, and the session is woken when the condition fires.
//!
//! ## The shape
//!
//! [`Parks`] is the registry — one per [`Agent`](crate::agent::Agent), and
//! shared with the tool that arms it. A park is a [`Condition`], a wake
//! level, a deadline, and the id of the `wait_for` call that asked for it.
//! A watcher task samples the condition through [`Vantage`], which is where
//! the facts that live outside core are read (a peer's doorbell, a background
//! command's exit file, the filesystem, the vault's own log), and on fire
//! or deadline marks the park spent and publishes a [`Fired`].
//!
//! **Nothing here touches the session log.** The watcher must not: the log
//! is single-writer by construction and the loop is the writer. A fire is
//! journaled by whoever wakes to it, through
//! [`Agent::take_fires`](crate::agent::Agent::take_fires).
//!
//! A park lives in this process, so a session that dies while parked loses the
//! wait itself. The `wait_for` tool_use and its result survive on the branch,
//! and **re-arming a park from the branch is not built** — but everything a
//! re-arm needs is there: the registration is the journaled tool input, so a
//! term's own arguments need no separate home, and a condition that is already
//! true fires at its first evaluation.
//!
//! ## A park belongs to the turn that armed it
//!
//! `wait_for` ends the turn it was called in, which is what makes the model
//! unable to poll around it. That decision is per *turn* rather than per
//! session: a park armed in an earlier turn and not yet fired does not end
//! a later turn that some other wake started, or a typed message could
//! never be answered. Hence [`Spec::epoch`] — the same number the loop reads
//! when it decides to settle.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::policy::Approval;
use crate::tool::{CallContext, Tool, ToolManifest, ToolOutput};

/// How often a park's condition is sampled: short enough that a wake feels
/// immediate, long enough that a wait costs nothing, because every leaf this
/// pace was chosen for is a file read or a socket round trip on localhost.
///
/// A leaf that leaves the machine sets its own pace through
/// [`Vantage::cadence`] — the same rate against a remote service is a load
/// rather than a wait — and a parked session is asleep, so what a slower
/// pace costs is latency and nothing else.
pub const POLL: Duration = Duration::from_millis(200);

/// The deadline a park gets when the tool names none, and the ceiling a
/// named one may not pass. Fifteen minutes is longer than any build, and
/// the ceiling is what keeps a model from parking a session for a week.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15 * 60);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

/// How much mail may interrupt a park: the `wake` argument, 0 through 2.
///
/// It gates **waking, never delivery** — the inbox is drained at every
/// boundary whatever this says, so `Nothing` means "I will read it when I
/// am next up", not "drop it". It never gates the operator: a steer or a
/// typed message is not mail and arrives through the user seam, which is
/// why a parked session is never unreachable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Wake {
    /// No mail wakes this session.
    Nothing,
    /// Mail marked `wake` does. The default, and what a session has always
    /// done — a park does not change that unless it says so.
    #[default]
    Waking,
    /// Any mail does, including a channel fan-out and a machine's note.
    Any,
}

impl Wake {
    pub fn from_level(level: u8) -> anyhow::Result<Wake> {
        Ok(match level {
            0 => Wake::Nothing,
            1 => Wake::Waking,
            2 => Wake::Any,
            n => bail!("`wake` is 0, 1 or 2 (nothing, waking mail, any mail), not {n}"),
        })
    }

    pub fn level(self) -> u8 {
        match self {
            Wake::Nothing => 0,
            Wake::Waking => 1,
            Wake::Any => 2,
        }
    }

    /// Would mail with this flag wake the session at this level?
    pub fn admits(self, waking: bool) -> bool {
        match self {
            Wake::Nothing => false,
            Wake::Waking => waking,
            Wake::Any => true,
        }
    }

    /// The level in the words every mention uses — the tool's
    /// description, the input schema and the arm-time result all say it
    /// one way. Three phrasings of three levels is how this got opaque
    /// once; there is one now.
    pub fn phrase(self) -> &'static str {
        match self {
            Wake::Nothing => "wake 0 — no mail will interrupt this",
            Wake::Waking => "wake 1 — only mail its sender marked to wake will interrupt this",
            Wake::Any => "wake 2 — any mail will interrupt this",
        }
    }

    /// The half of the same sentence that says what happens to the mail a
    /// level refuses: it is delivered and read at the next turn, never
    /// dropped. The distinction a model is most likely to get wrong, so
    /// it rides every mention of a level.
    pub const DELIVERY: &str = "messages still arrive and are read at your next turn";
}


/// One leaf of a condition: a fact about the world, named by the model and
/// evaluated by the harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Term {
    /// The session named is between turns — asked of its own doorbell, not
    /// of its `meta.json`, which only the TUI driver ever writes.
    PeerIdle(String),
    /// The background command whose log this is has finished: its exit
    /// status file is there.
    CommandFinished(PathBuf),
    FileExists(PathBuf),
    LogMatches { log: PathBuf, pattern: String },
    /// The vault's own log has moved past a cursor: an event with
    /// `seq > since` exists under the filter.
    ///
    /// `vault` and `since` travel together and both are required, because
    /// the log's `seq` is per vault — a cursor without its vault is a number
    /// with no meaning. The cursor is the term's own argument and rides the
    /// journaled `wait_for` input, so re-arming needs nothing else.
    MnemeSeq {
        /// The vault's name as `list_vaults` gives it: a registry name, not
        /// a folder.
        vault: String,
        /// The cursor: the `through_seq` a read returned.
        since: u64,
        /// Vault-relative path or folder prefix, to wait on one note rather
        /// than on the vault.
        path: Option<String>,
        /// Include-only substring of the writer's label. The stable part of
        /// one is `session=<id>`: persona, model and cwd move event to
        /// event.
        written_by: Option<String>,
    },
}

/// A leaf as a checklist row says it: the kind (the stable key a view's
/// tag table reads), the thing watched, and the fact waited for — with
/// any qualifiers beside it. Said in a sentence, these are
/// [`Term::describe`] — but the sentence keeps the internals a row
/// leaves out, and the two live one match apart so they age together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermParts {
    /// Which kind of thing this watches: `bg`, `peer`, `file`, `log`,
    /// `vault` — the key a tag table is read from, nothing more.
    pub kind: &'static str,
    /// The thing itself: an id, a name, a path.
    pub subject: String,
    /// The fact about it that ends the wait.
    pub predicate: String,
    /// Narrowing facts — where in the vault, by whom.
    pub qualifiers: Vec<String>,
}

impl Term {
    /// The leaf in the words the **model** reads: a sentence naming the
    /// kind of thing and the fact waited for, with the precise anchor
    /// where one exists — the vault cursor is here, because the model
    /// chose it from its own read and needs to know which threshold
    /// fired. The view draws [`Term::parts`] instead, which says the
    /// same thing without the internals.
    pub fn describe(&self) -> String {
        match self {
            Term::PeerIdle(id) => format!("session {id} is idle"),
            Term::CommandFinished(log) => match log.file_stem() {
                Some(stem) => format!("background {} has finished running", stem.to_string_lossy()),
                None => format!("background {} has finished running", log.display()),
            },
            Term::FileExists(p) => format!("{} exists", p.display()),
            // A literal search, not a pattern — so said, in quotation
            // marks, rather than in slashes that mean regex.
            Term::LogMatches { log, pattern } => match log.file_stem() {
                Some(stem) => format!("{} contains \u{201c}{pattern}\u{201d}", stem.to_string_lossy()),
                None => format!("{} contains \u{201c}{pattern}\u{201d}", log.display()),
            },
            Term::MnemeSeq { vault, since, path, written_by } => {
                let mut out = format!("vault {vault} has new entries since {since}");
                if let Some(p) = path {
                    out.push_str(&format!(" under {p}"));
                }
                if let Some(w) = written_by {
                    out.push_str(&format!(" by {w}"));
                }
                out
            }
        }
    }

    /// The leaf as the view's row: kind, subject, predicate, qualifiers —
    /// no audit cursor, no jargon a person has no use for. One match from
    /// [`Term::describe`]'s, so the two wordings cannot silently differ.
    pub fn parts(&self) -> TermParts {
        match self {
            Term::PeerIdle(id) => TermParts {
                kind: "peer",
                subject: id.to_string(),
                predicate: "is idle".into(),
                qualifiers: Vec::new(),
            },
            Term::CommandFinished(log) => TermParts {
                kind: "bg",
                subject: log
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| log.display().to_string()),
                predicate: "has finished running".into(),
                qualifiers: Vec::new(),
            },
            Term::FileExists(p) => TermParts {
                kind: "file",
                subject: p.display().to_string(),
                predicate: "exists".into(),
                qualifiers: Vec::new(),
            },
            Term::LogMatches { log, pattern } => TermParts {
                kind: "log",
                subject: log
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| log.display().to_string()),
                predicate: format!("contains \u{201c}{pattern}\u{201d}"),
                qualifiers: Vec::new(),
            },
            Term::MnemeSeq { vault, path, written_by, .. } => TermParts {
                kind: "vault",
                subject: vault.clone(),
                predicate: "has new entries".into(),
                qualifiers: {
                    let mut q = Vec::new();
                    if let Some(p) = path {
                        q.push(format!("under {p}"));
                    }
                    if let Some(w) = written_by {
                        q.push(format!("by {w}"));
                    }
                    q
                },
            },
        }
    }
}

/// A condition: leaves composed with `all`, `any` and `not`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
    Term(Term),
    /// No condition at all: only the deadline resolves this park. The shape
    /// `wait_for { timeout_s: 300 }` gets, which is what a plain sleep was
    /// for — and it is a wake, not a block, so the pane is free while it
    /// runs.
    Never,
}

impl Condition {
    /// Parse the tool's JSON. The error names what was expected rather than
    /// what arrived: a model that wrote a term wrong has to be able to fix
    /// it from the message alone.
    pub fn parse(v: &Value) -> anyhow::Result<Condition> {
        let obj = v
            .as_object()
            .context("a condition is an object with exactly one key — `all`, `any`, `not`, \
`peer_idle`, `command_finished`, `file_exists`, `log_matches` or `mneme_seq`")?;
        if obj.len() != 1 {
            bail!(
                "a condition has exactly one key, and this one has {}: {}",
                obj.len(),
                obj.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        let (key, value) = obj.iter().next().expect("len checked");
        Ok(match key.as_str() {
            "all" => Condition::All(children(key, value)?),
            "any" => Condition::Any(children(key, value)?),
            "not" => Condition::Not(Box::new(Condition::parse(value)?)),
            "peer_idle" => Condition::Term(Term::PeerIdle(
                value
                    .as_str()
                    .context("`peer_idle` takes a session id from `peers`")?
                    .to_string(),
            )),
            "command_finished" => Condition::Term(Term::CommandFinished(PathBuf::from(
                value
                    .as_str()
                    .context("`command_finished` takes the log path a background `bash` returned")?,
            ))),
            "file_exists" => Condition::Term(Term::FileExists(PathBuf::from(
                value.as_str().context("`file_exists` takes a path")?,
            ))),
            "log_matches" => {
                let inner = value
                    .as_object()
                    .context("`log_matches` takes an object: {\"log\": …, \"pattern\": …}")?;
                let log = inner
                    .get("log")
                    .and_then(Value::as_str)
                    .context("`log_matches` needs `log`: the log path to read")?;
                let pattern = inner
                    .get("pattern")
                    .and_then(Value::as_str)
                    .context("`log_matches` needs `pattern`: text to look for in the log")?;
                Condition::Term(Term::LogMatches {
                    log: PathBuf::from(log),
                    pattern: pattern.to_string(),
                })
            }
            "mneme_seq" => {
                let inner = value.as_object().context(
                    "`mneme_seq` takes an object: {\"vault\": …, \"since\": …, \"path\": …, \
\"written_by\": …}",
                )?;
                let vault = inner.get("vault").and_then(Value::as_str).context(
                    "`mneme_seq` needs `vault`: the vault's name as `list_vaults` gives it, \
which is not a folder",
                )?;
                let since = inner.get("since").and_then(Value::as_u64).context(
                    "`mneme_seq` needs `since`: the cursor to wait past, which is the \
`through_seq` a read returned",
                )?;
                let path = inner.get("path").and_then(Value::as_str).map(str::to_string);
                let written_by =
                    inner.get("written_by").and_then(Value::as_str).map(str::to_string);
                Condition::Term(Term::MnemeSeq {
                    vault: vault.to_string(),
                    since,
                    path,
                    written_by,
                })
            }
            other => bail!(
                "`{other}` is not a condition term; expected `all`, `any`, `not`, `peer_idle`, \
`command_finished`, `file_exists`, `log_matches` or `mneme_seq`"
            ),
        })
    }

    /// The condition in words, for the roster line and the fire the model
    /// reads.
    pub fn describe(&self) -> String {
        match self {
            Condition::All(cs) => join(cs, " and "),
            Condition::Any(cs) => join(cs, " or "),
            Condition::Not(c) => format!("not ({})", c.describe()),
            Condition::Term(t) => t.describe(),
            Condition::Never => "the time you asked for had passed".to_string(),
        }
    }

    /// Every leaf of this condition, each once, in the order they are
    /// written: what a sample has to ask about, and what a vantage reads to
    /// decide its own cadence.
    pub fn terms(&self) -> Vec<&Term> {
        let mut out: Vec<&Term> = Vec::new();
        self.collect_terms(&mut out);
        out
    }

    fn collect_terms<'a>(&'a self, out: &mut Vec<&'a Term>) {
        match self {
            Condition::All(cs) | Condition::Any(cs) => {
                for c in cs {
                    c.collect_terms(out);
                }
            }
            Condition::Not(c) => c.collect_terms(out),
            // A term written twice is one question: a duplicate must not
            // cost a second round trip to a leaf that leaves the machine.
            Condition::Term(t) => {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
            Condition::Never => {}
        }
    }
}

fn join(cs: &[Condition], sep: &str) -> String {
    if cs.is_empty() {
        return "(nothing)".to_string();
    }
    cs.iter().map(Condition::describe).collect::<Vec<_>>().join(sep)
}

fn children(key: &str, v: &Value) -> anyhow::Result<Vec<Condition>> {
    let items = v
        .as_array()
        .with_context(|| format!("`{key}` takes a list of conditions"))?;
    if items.is_empty() {
        bail!("`{key}` is empty: a condition needs at least one term");
    }
    items.iter().map(Condition::parse).collect()
}

/// A leaf's answer.
///
/// Three outcomes rather than a bool, because "not yet" and "nobody can say"
/// are different facts about a wait: the second resolves the park instead of
/// burning its deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The fact holds, so this leaf satisfies the condition.
    Yes,
    /// It does not hold — yet.
    No,
    /// Nobody can answer this, and nobody will: the session is gone, the
    /// vault has no such function. A condition holding it can never become
    /// true, and waiting for the deadline to say so is the polling failure
    /// in a slower costume. The string is the reason, and it is what the
    /// model reads.
    Unanswerable(String),
}

/// The facts a condition is asked about beyond core's own state.
///
/// A trait rather than direct calls because the answers live where the
/// dependencies do: a peer's state is the swarm's doorbell client, a
/// background command's exit file is the tools' shell primitive, and the
/// vault's log is a remote client's. The evaluator needs none of them, which
/// is what keeps this half testable with a fake.
#[async_trait::async_trait]
pub trait Vantage: Send + Sync {
    /// How long a watcher should wait between samples of this condition.
    ///
    /// The default is [`POLL`], the pace of a leaf that is a file read or a
    /// socket round trip on localhost. A leaf that leaves the machine
    /// answers for itself here: sampling a remote service ten times a second
    /// is a load rather than a wait, and a parked session is asleep, so a
    /// slower pace costs latency and nothing else. The slowest leaf in a
    /// condition sets the pace for all of it.
    fn cadence(&self, _condition: &Condition) -> Duration {
        POLL
    }

    /// Answer one leaf.
    ///
    /// Two rules, and both are the park's design rather than taste. A leaf
    /// is a **read**: an answer may not change what it looked at. And a leaf
    /// must **return**: the same call is made at arm time, inline in the
    /// model's turn, so it may be slow about leaving the machine but it may
    /// never hold a request open waiting for the fact to change.
    async fn ask(&self, term: &Term) -> Answer;
}

/// A condition's value at one instant.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Truth {
    True,
    False,
    /// A term nobody can answer.
    Gone(String),
}

/// One sample's facts: every leaf of a condition, asked once.
///
/// The split is what lets the one leaf that leaves the machine be awaited.
/// Asking is here; walking the tree is then a synchronous function of what
/// came back, so `all`/`any`/`not` cost nothing and the shape of the
/// condition is the only thing that decides the order.
struct Answers(Vec<(Term, Answer)>);

impl Answers {
    fn truth(&self, term: &Term) -> Truth {
        match self.0.iter().find(|(seen, _)| seen == term).map(|(_, a)| a) {
            Some(Answer::Yes) => Truth::True,
            Some(Answer::No) => Truth::False,
            Some(Answer::Unanswerable(why)) => Truth::Gone(why.clone()),
            // `gather` walks the same tree, so a leaf with no answer is a bug
            // in this file rather than a fact about the world. It reads as
            // false, which leaves the park to its deadline instead of firing
            // it on a missing entry.
            None => Truth::False,
        }
    }
}

/// Ask every leaf of a condition, once each.
async fn gather(v: &dyn Vantage, c: &Condition) -> Answers {
    let mut answers = Vec::new();
    for t in c.terms() {
        answers.push((t.clone(), v.ask(t).await));
    }
    Answers(answers)
}

fn eval(c: &Condition, answers: &Answers) -> Truth {
    match c {
        Condition::Term(t) => answers.truth(t),
        Condition::Never => Truth::False,
        Condition::All(cs) => eval_all(cs, answers),
        Condition::Any(cs) => eval_any(cs, answers),
        Condition::Not(c) => match eval(c, answers) {
            Truth::True => Truth::False,
            Truth::False => Truth::True,
            gone @ Truth::Gone(_) => gone,
        },
    }
}

/// An unanswerable term resolves the condition rather than being folded
/// into the boolean: a condition naming a session that is no longer there
/// is not going to become true, and waiting for the deadline to say so is
/// the polling failure in a slower costume.
fn eval_all(cs: &[Condition], answers: &Answers) -> Truth {
    let mut gone = None;
    for c in cs {
        match eval(c, answers) {
            Truth::False => return Truth::False,
            Truth::Gone(w) => gone = gone.or(Some(w)),
            Truth::True => {}
        }
    }
    match gone {
        Some(w) => Truth::Gone(w),
        None => Truth::True,
    }
}

fn eval_any(cs: &[Condition], answers: &Answers) -> Truth {
    let mut gone = None;
    for c in cs {
        match eval(c, answers) {
            Truth::True => return Truth::True,
            Truth::Gone(w) => gone = gone.or(Some(w)),
            Truth::False => {}
        }
    }
    match gone {
        Some(w) => Truth::Gone(w),
        None => Truth::False,
    }
}

/// The leaves whose answers made this condition true, walked in the
/// condition's own shape and said in [`Term::describe`]'s words: `any`
/// names the leaf that fired, `all` names every leaf that held, and
/// `not` names the inner leaf as negated — the deciding fact there is a
/// leaf being *false*, and the words must say that rather than claim
/// the leaf holds. Empty means the caller asked about a condition that
/// is not true; the fire then reads as the old bare "it fired".
fn deciding(c: &Condition, answers: &Answers) -> Vec<String> {
    match c {
        Condition::Term(t) => match answers.truth(t) {
            Truth::True => vec![t.describe()],
            _ => Vec::new(),
        },
        Condition::Never => Vec::new(),
        Condition::All(cs) => cs.iter().flat_map(|c| deciding(c, answers)).collect(),
        // The first satisfied child is the decider; the rest are
        // irrelevant by `any`'s own definition.
        Condition::Any(cs) => cs
            .iter()
            .map(|c| deciding(c, answers))
            .find(|got| !got.is_empty())
            .unwrap_or_default(),
        Condition::Not(c) => match eval(c, answers) {
            Truth::False => vec![format!("not ({})", c.describe())],
            _ => Vec::new(),
        },
    }
}

/// The leaves still open at the deadline, same shape, same words: for
/// `all`, the leaves that never held; for `any`, every leaf, because
/// none held or one would have fired; for `not`, the inner fact as the
/// negation still waited for.
fn open_leaves(c: &Condition, answers: &Answers) -> Vec<String> {
    match c {
        Condition::Term(t) => match answers.truth(t) {
            Truth::True => Vec::new(),
            _ => vec![t.describe()],
        },
        Condition::Never => Vec::new(),
        Condition::All(cs) | Condition::Any(cs) => {
            cs.iter().flat_map(|c| open_leaves(c, answers)).collect()
        }
        Condition::Not(c) => vec![format!("not ({})", c.describe())],
    }
}

/// Why a park resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The condition is true. The strings are the leaves that decided it,
    /// in the words [`Term::describe`] uses — a fire that said only "it
    /// fired" would send the model back to reread its own registration,
    /// which is the polling this primitive exists to prevent.
    Met(Vec<String>),
    /// The deadline arrived with the condition still false. The strings
    /// are the leaves still open — what decides re-arm, re-ask, or give
    /// up, without re-deriving it. Rendered capped.
    TimedOut(Vec<String>),
    /// The condition named something that is gone.
    Unanswerable(String),
}

/// How many still-open leaves the deadline's words name before they
/// collapse into a count: three is a list, ten is a paragraph.
const NAME_AT_MOST: usize = 3;

impl Outcome {
    /// How the model is told. The frame is built from this, so it says what
    /// happened without pretending the wait succeeded — and the words are
    /// what gets journaled, so replay reproduces them with no further work.
    pub fn words(&self) -> String {
        match self {
            Outcome::Met(leaves) => match leaves.as_slice() {
                [] => "it fired".to_string(),
                [one] => format!("it fired — {one}"),
                many => format!("it fired — {}", many.join("; ")),
            },
            Outcome::TimedOut(open) => match open.as_slice() {
                [] => "the deadline arrived first".to_string(),
                many => {
                    let mut shown: Vec<String> = many.iter().take(NAME_AT_MOST).cloned().collect();
                    if many.len() > NAME_AT_MOST {
                        shown.push(format!("and {} more", many.len() - NAME_AT_MOST));
                    }
                    format!("the deadline arrived first — still open: {}", shown.join("; "))
                }
            },
            Outcome::Unanswerable(why) => format!("{why}, so it cannot fire"),
        }
    }
}

/// One leaf of a condition, as the watcher's latest sample saw it: a row
/// of the live checklist the transcript draws while a park is armed. Ticks
/// are **view state and nothing more** — never journaled — because the log
/// keeps the arm and the fire and the ticks between are the polling made
/// visible, not facts the conversation replays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParkRow {
    /// The leaf's fact holds right now.
    pub met: bool,
    /// Why nobody can ever answer this leaf: a gone session, a vault
    /// without the function. The park fires `Unanswerable` right behind
    /// such a row, so this is the outcome's footnote rather than a state
    /// the row waits in.
    pub gone: Option<String>,
}

/// One watcher's whole sample, sent because some row changed: the
/// checklist ticks on edges, not on a heartbeat, so an armed wait that is
/// merely waiting costs the bus nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParkTick {
    /// The `wait_for` call that armed the park — the id the transcript's
    /// entry carries, so the view knows which block to tick.
    pub call_id: String,
    /// Every leaf, in the condition's own order — the order
    /// [`Condition::terms`] reads them out.
    pub rows: Vec<ParkRow>,
}

/// A park that has resolved, waiting to be journaled by whoever wakes to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fired {
    /// The `wait_for` call that armed it: what pairs a fire with its
    /// registration on the branch.
    pub call_id: String,
    /// The condition in words.
    pub condition: String,
    pub outcome: Outcome,
    /// When the park was armed and when it resolved — what a reader of the
    /// transcript needs to know how long the session waited.
    pub waited_ms: u64,
}

impl Fired {
    /// What the model reads when this wakes it.
    pub fn framed(&self) -> String {
        fired_frame(&self.condition, &self.outcome.words(), Some(self.waited_ms / 1000))
    }
}

/// What the model reads when a park resolves.
///
/// Its own frame, for the reason [`crate::peer::frame`] has one: this is
/// neither the operator speaking nor a colleague, and a model that reads it
/// as either will answer it as if someone had asked something. The journal
/// keeps the condition and the outcome — not the clock — so `waited` is
/// `None` on the replay path and the frame says the same thing without the
/// duration.
pub fn fired_frame(condition: &str, outcome: &str, waited_s: Option<u64>) -> String {
    let after = match waited_s {
        Some(s) => format!(" — after {s}s"),
        None => String::new(),
    };
    format!(
        "[You asked to be woken when {condition}. It resolved: {outcome}{after}. This is not a \
peer's message and not the operator's; it is the condition you registered with `wait_for`.]"
    )
}

/// What it takes to arm a park.
#[derive(Clone, Debug)]
pub struct Spec {
    /// The `wait_for` tool_use id.
    pub call_id: String,
    pub condition: Condition,
    pub wake: Wake,
    pub timeout: Duration,
    /// The turn that asked, so only that turn's own park can end it.
    pub epoch: u64,
}

/// A park: a condition, and everything needed to say when it resolved.
#[derive(Clone, Debug)]
struct Park {
    id: u64,
    spec: Spec,
    deadline: Instant,
    armed: Instant,
}

/// What arming came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Arm {
    /// Armed; the watcher is running under this id.
    Armed(u64),
    /// The condition is already true. Nothing to wait for — the honest
    /// answer is that, not a park that fires on its first sample.
    Already,
    /// A term cannot be answered at all (a peer that is not there). The
    /// park is not armed, and the model is told why.
    Unanswerable(String),
}

#[derive(Default)]
struct State {
    parks: Vec<Park>,
    fired: Vec<Fired>,
}

/// The registry of parks for one session.
pub struct Parks {
    state: Mutex<State>,
    /// Per-leaf samples that changed since the view last read them. Never
    /// journaled; see [`ParkRow`].
    ticked: Mutex<Vec<ParkTick>>,
    next: AtomicU64,
    /// Which turn is running. Bumped by the loop at every entry and read by
    /// the tool that arms a park, so the loop can tell "a park this turn
    /// asked for" from "a park from an earlier turn, still waiting".
    epoch: AtomicU64,
    /// Rings when a park resolves. The driver's wake, and nothing else's:
    /// mail has the doorbell, this is the harness's own condition.
    wake: broadcast::Sender<()>,
    vantage: Mutex<Option<Arc<dyn Vantage>>>,
}

impl Default for Parks {
    fn default() -> Self {
        Self::new()
    }
}

impl Parks {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            ticked: Mutex::new(Vec::new()),
            next: AtomicU64::new(1),
            epoch: AtomicU64::new(0),
            wake: broadcast::channel(16).0,
            vantage: Mutex::new(None),
        }
    }

    /// Mark a turn as started, and answer which one it is.
    pub fn begin_turn(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The turn running right now, as the tool that arms a park sees it.
    pub fn current_epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Hand the registry the facts a condition is evaluated against. Called
    /// once at session start, by whoever holds the swarm and the tools.
    pub fn set_vantage(&self, vantage: Arc<dyn Vantage>) {
        *self.vantage.lock().unwrap() = Some(vantage);
    }

    fn vantage(&self) -> Option<Arc<dyn Vantage>> {
        self.vantage.lock().unwrap().clone()
    }

    /// Wake on a park resolving. One receiver per driver; a receiver that
    /// is dropped or late loses nothing, because the fires themselves wait
    /// in the registry until [`Parks::take_fires`] reads them.
    pub fn events(&self) -> broadcast::Receiver<()> {
        self.wake.subscribe()
    }

    /// Evaluate a condition once, without arming anything: what it is worth
    /// right now, or why nobody can say.
    ///
    /// The tool does not pre-check with this — [`Parks::arm`] answers
    /// already-true and unanswerable itself, and a sample spent on a
    /// question the arming is about to ask again would be a second round
    /// trip to a leaf that leaves the machine. It is here for a caller that
    /// wants the condition's value and not a park.
    pub async fn sample(&self, condition: &Condition) -> Result<bool, String> {
        let Some(v) = self.vantage() else {
            return Err("this session has no way to check conditions".to_string());
        };
        match eval(condition, &gather(v.as_ref(), condition).await) {
            Truth::True => Ok(true),
            Truth::False => Ok(false),
            Truth::Gone(why) => Err(why),
        }
    }

    /// Arm a park and start its watcher.
    ///
    /// Must be called from a runtime: the watcher is a task, and it is the
    /// whole point that nothing waits for it inline. The condition is
    /// evaluated **once, here** — a condition that is already true is not
    /// armed, and neither is one that cannot be answered — and that single
    /// evaluation is what closes the read-then-arm race: a cursor read a
    /// moment ago and written past since is seen by this sample, so the
    /// caller is told "already true" instead of parking on a fact that has
    /// arrived.
    pub async fn arm(self: &Arc<Self>, spec: Spec) -> Arm {
        let Some(vantage) = self.vantage() else {
            return Arm::Unanswerable("this session has no way to check conditions".to_string());
        };
        match eval(&spec.condition, &gather(vantage.as_ref(), &spec.condition).await) {
            Truth::True => return Arm::Already,
            Truth::Gone(why) => return Arm::Unanswerable(why),
            Truth::False => {}
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let park = Park {
            id,
            deadline: Instant::now() + spec.timeout,
            armed: Instant::now(),
            spec,
        };
        self.state.lock().unwrap().parks.push(park.clone());
        let parks = Arc::clone(self);
        tokio::spawn(async move { watch(parks, park, vantage).await });
        Arm::Armed(id)
    }

    /// Is a park armed by `epoch` still waiting? The loop's settle
    /// decision, and the reason a park cannot end a turn it did not arm.
    pub fn armed_for(&self, epoch: u64) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .parks
            .iter()
            .find(|p| p.spec.epoch == epoch)
            .map(|p| p.spec.condition.describe())
    }

    /// What this session is waiting on, for the roster line and for `send`
    /// to tell a sender the truth. `None` when nothing is armed.
    pub fn waiting_on(&self) -> Option<String> {
        let state = self.state.lock().unwrap();
        let p = state.parks.first()?;
        Some(format!("{} (deadline in {}s)", p.spec.condition.describe(), p.deadline.saturating_duration_since(Instant::now()).as_secs()))
    }

    /// Would mail wake this session right now? A driver's question, asked
    /// when a message arrives and nobody is driving: the park's `wake` level
    /// is what decides whether it starts a turn.
    ///
    /// With nothing armed this is the ordinary rule — mail marked to wake
    /// does, and nothing else does — so a park changes how mail behaves only
    /// while one is waiting. With several armed, **any** of them admitting
    /// the mail wakes the session: a level says what its own park will be
    /// interrupted for, and one park's `Any` is not overruled by another's
    /// `Nothing`.
    ///
    /// This decides *waking* and never *delivery*. The inbox is drained at
    /// every boundary whatever this says, so mail that does not wake the
    /// session is mail it reads on the next turn, not mail it lost.
    pub fn admits(&self, waking: bool) -> bool {
        let state = self.state.lock().unwrap();
        if state.parks.is_empty() {
            return waking;
        }
        state.parks.iter().any(|p| p.spec.wake.admits(waking))
    }

    /// The wake level of every park armed right now, in arm order. Empty
    /// when nothing is armed — the ordinary rule (waking mail wakes) is
    /// the caller's to state. What the arm-time result reads when it has
    /// to say which level is deciding: with several parks up, a message
    /// wakes if **any** armed level admits it, the loosest live level.
    pub fn wake_levels(&self) -> Vec<Wake> {
        self.state
            .lock()
            .unwrap()
            .parks
            .iter()
            .map(|p| p.spec.wake)
            .collect()
    }

    pub fn is_armed(&self) -> bool {
        !self.state.lock().unwrap().parks.is_empty()
    }

    /// Take the fires that have resolved since this was last called. The
    /// caller journals them; until it does, they wait here.
    pub fn take_fires(&self) -> Vec<Fired> {
        std::mem::take(&mut self.state.lock().unwrap().fired)
    }

    /// A watcher published a changed sample: keep it for the view, and
    /// ring the same bell a fire rings. A wake that finds no fires and no
    /// ticks is a no-op for every receiver, so an extra ring is never a
    /// wrong turn — it is only a redraw.
    fn note_tick(&self, tick: ParkTick) {
        self.ticked.lock().unwrap().push(tick);
        let _ = self.wake.send(());
    }

    /// Take the changed samples the view has not drawn yet.
    pub fn take_ticks(&self) -> Vec<ParkTick> {
        std::mem::take(&mut self.ticked.lock().unwrap())
    }

    fn fire(&self, id: u64, outcome: Outcome) {
        let mut state = self.state.lock().unwrap();
        let Some(i) = state.parks.iter().position(|p| p.id == id) else {
            return;
        };
        let park = state.parks.remove(i);
        state.fired.push(Fired {
            call_id: park.spec.call_id,
            condition: park.spec.condition.describe(),
            outcome,
            waited_ms: park.armed.elapsed().as_millis() as u64,
        });
        drop(state);
        // A send with no receivers is a driver that is not listening for
        // this; the fire is in the registry either way.
        let _ = self.wake.send(());
    }

    /// Resolve every park at once — used when a session is asked to stop,
    /// so a park cannot outlive the wait that wanted it.
    pub fn clear(&self) {
        self.state.lock().unwrap().parks.clear();
    }
}

/// The watcher: sample until the condition holds, or the deadline arrives.
///
/// Polling, but not by the model: this task costs a `read` and a socket
/// round trip every [`POLL`] — or every [`Vantage::cadence`], which is what
/// a leaf that leaves the machine asks for — and the session it belongs to
/// is asleep while it runs.
async fn watch(parks: Arc<Parks>, park: Park, vantage: Arc<dyn Vantage>) {
    let cadence = vantage.cadence(&park.spec.condition);
    // The last sample published. The watcher is already polling every
    // leaf; the only cost of a tick is the noticing, so it fires on the
    // edge — the first sample, and any sample after it where some leaf's
    // answer moved.
    let mut published: Option<Vec<ParkRow>> = None;
    loop {
        let answers = gather(vantage.as_ref(), &park.spec.condition).await;
        let rows: Vec<ParkRow> = answers
            .0
            .iter()
            .map(|(_, a)| match a {
                Answer::Yes => ParkRow { met: true, gone: None },
                Answer::No => ParkRow { met: false, gone: None },
                Answer::Unanswerable(why) => ParkRow {
                    met: false,
                    gone: Some(why.clone()),
                },
            })
            .collect();
        if published.as_ref() != Some(&rows) {
            published = Some(rows.clone());
            parks.note_tick(ParkTick {
                call_id: park.spec.call_id.clone(),
                rows,
            });
        }
        match eval(&park.spec.condition, &answers) {
            Truth::True => {
                return parks.fire(
                    park.id,
                    Outcome::Met(deciding(&park.spec.condition, &answers)),
                )
            }
            Truth::Gone(why) => return parks.fire(park.id, Outcome::Unanswerable(why)),
            Truth::False => {}
        }
        let now = Instant::now();
        if now >= park.deadline {
            return parks.fire(
                park.id,
                Outcome::TimedOut(open_leaves(&park.spec.condition, &answers)),
            );
        }
        tokio::time::sleep(cadence.min(park.deadline - now)).await;
    }
}

/// The `wait_for` tool: arm a park, and end the turn.
///
/// A native tool rather than a Rune script wrapping a primitive, for the
/// reason [`crate::ToolSearch`] is: what it does is harness bookkeeping —
/// hand a condition to the registry the loop reads — and there is nothing
/// underneath it for a script to compose. Its input is the whole contract,
/// and Rust evaluates it.
///
/// The result is the teaching. A model that has just been told "you will be
/// woken" has every incentive to go and look, so the call says in as many
/// words that the turn ends here and nothing it can call sees the condition.
pub struct WaitFor {
    parks: Arc<Parks>,
    manifest: ToolManifest,
    default_timeout: Duration,
    max_timeout: Duration,
}

impl WaitFor {
    pub fn new(parks: Arc<Parks>, default_timeout: Duration, max_timeout: Duration) -> Self {
        Self {
            parks,
            manifest: ToolManifest {
                name: "wait_for".into(),
                description: "Wait for something outside this session without polling: name a \
condition, and the harness ends this turn and wakes you when it fires — or when the deadline \
arrives, which is itself a wake. `condition` is `all`/`any`/`not` over `peer_idle`, \
`command_finished`, `file_exists`, `log_matches` and `mneme_seq`; leave it out to wait only for \
the deadline. `wake` says how much mail may interrupt the wait: wake 0 — no mail will interrupt \
this; wake 1 (the default) — only mail its sender marked to wake will interrupt this; wake 2 — \
any mail will interrupt this. A level gates being woken, never delivery: messages still arrive \
and are read at your next turn, so 0 is later and not never. With more than one wait armed, a \
message wakes you if any armed level admits it — the loosest live level decides. Mail wakes a \
park only in an interactive session: a session parked in a one-shot `run` is never woken by mail \
at any level. The deadline is mandatory and defaults to 15 minutes."
                    .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "condition": {
                            "type": "object",
                            "description": "What to wait for. One key per object: `all`/`any` take a list of conditions, `not` takes one, and the terms are `peer_idle` (a session id from `peers`), `command_finished` (the log path a background `bash` returned), `file_exists` (a path), `log_matches` ({\"log\": …, \"pattern\": …}) and `mneme_seq` ({\"vault\": …, \"since\": …, \"path\": …, \"written_by\": …} — the vault's own log moved past the cursor a read returned)."
                        },
                        "timeout_s": {
                            "type": "integer",
                            "description": "Seconds until the deadline wakes you anyway (default 900, capped at 21600)."
                        },
                        "wake": {
                            "type": "integer",
                            "description": "How much mail may interrupt the wait. wake 0 — no mail will interrupt this; wake 1 (the default) — only mail its sender marked to wake will interrupt this; wake 2 — any mail will interrupt this. A level gates being woken, never delivery: messages still arrive and are read at your next turn, so 0 is later and not never. With more than one wait armed, a message wakes you if any armed level admits it — the loosest live level decides. The operator can always reach you: this gates mail, not them."
                        },
                        "note": {
                            "type": "string",
                            "description": "Why you are waiting, for the transcript."
                        }
                    }
                }),
                approval: Approval::ReadOnly,
                prompt: Some(
                    "`wait_for` ends the turn it is called in, and that is the point: nothing you \
do afterwards can see the condition, and a loop of `peers` and log reads is exactly what this \
replaces. The result says what you will be woken with. If the condition is already true the call \
says so and does not park — carry on in the same turn instead. A `mneme_seq` condition names a \
cursor, so read the vault's log first — `mneme_rpc` `audit_query` answers the `through_seq` to \
wait past — and read it again when the wake arrives: a wake says the log moved, not what it says."
                        .to_string(),
                ),
                render: None,
                deferred: false,
            },
            default_timeout,
            max_timeout,
        }
    }
}

#[async_trait::async_trait]
impl Tool for WaitFor {
    fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }

    async fn call(&self, input: Value, _ctx: CallContext) -> anyhow::Result<ToolOutput> {
        let named = input.get("condition").is_some();
        let wake = match input.get("wake") {
            Some(v) => {
                let level = v.as_u64().context("`wake` is 0, 1 or 2")? as u8;
                Wake::from_level(level)?
            }
            None => Wake::default(),
        };
        let asked = input.get("timeout_s").and_then(Value::as_u64);
        if !named && asked.is_none() {
            return Ok(ToolOutput::error(
                "give a `condition` to wait for, or a `timeout_s` to be woken at — or both",
            ));
        }
        let asked = asked.map(|s| Duration::from_secs(s.max(1)));
        let timeout = asked.unwrap_or(self.default_timeout).min(self.max_timeout);
        let condition = match input.get("condition") {
            Some(v) => Condition::parse(v)?,
            None => Condition::Never,
        };

        // Already true is not a park, and `arm` is where that is decided:
        // one evaluation, not the two a pre-check plus the arming would
        // cost, and the sample that closes the read-then-arm race is the
        // same one.
        let spec = Spec {
            call_id: _ctx.call_id.clone(),
            condition: condition.clone(),
            wake,
            timeout,
            epoch: self.parks.current_epoch(),
        };
        let id = match self.parks.arm(spec).await {
            Arm::Armed(id) => id,
            Arm::Already => {
                return Ok(ToolOutput::ok(format!(
                    "[already true] {} — there is nothing to wait for, so nothing was armed; \
carry on in this turn.",
                    condition.describe()
                )));
            }
            Arm::Unanswerable(why) => {
                return Ok(ToolOutput::ok(format!(
                    "[not armed] {why}, so {} can never become true. Nothing is waiting on it.",
                    condition.describe()
                )));
            }
        };
        let clamped = match asked {
            Some(a) if a > self.max_timeout => format!(
                " (asked for {}s; capped at {}s)",
                a.as_secs(),
                self.max_timeout.as_secs()
            ),
            _ => String::new(),
        };
        let note = input
            .get("note")
            .and_then(Value::as_str)
            .map(|n| format!("\nbecause: {n}"))
            .unwrap_or_default();
        // The level says itself in the same words the description and the
        // schema use, and the many-parks rule rides only when many parks
        // there are — naming the level that is deciding right now.
        let levels = self.parks.wake_levels();
        let multi = match levels.iter().copied().max_by_key(|w| w.level()) {
            Some(loosest) if levels.len() > 1 => format!(
                "\nMore than one wait is armed: a message wakes you if any armed level admits \
it — the loosest live level decides, and right now that is {}.",
                loosest.phrase()
            ),
            _ => String::new(),
        };
        Ok(ToolOutput::ok(format!(
            "[armed #{id}] waiting for {}{note}\n{}; {}; deadline: {}s{clamped}\n\
The turn ends here: you are woken with a frame saying how the wait resolved. Do not call \
anything to check on it — nothing you can call sees the condition.{multi}",
            condition.describe(),
            wake.phrase(),
            Wake::DELIVERY,
            timeout.as_secs(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicBool;

    /// A vantage with nothing real behind it: the leaves a test needs, and
    /// a peer whose state can be moved from another thread.
    #[derive(Default)]
    struct Fake {
        idle: AtomicBool,
        gone: AtomicBool,
        exited: AtomicBool,
        file: AtomicBool,
        log: Mutex<String>,
        vault_moved: AtomicBool,
        vault_gone: AtomicBool,
    }

    #[async_trait::async_trait]
    impl Vantage for Fake {
        async fn ask(&self, term: &Term) -> Answer {
            match term {
                Term::PeerIdle(_) => {
                    if self.gone.load(Ordering::SeqCst) {
                        // Not `Busy`: a peer nobody can ask about is a fact
                        // about the condition, and the vantage is where the
                        // words for it live.
                        Answer::Unanswerable("session p is gone".to_string())
                    } else if self.idle.load(Ordering::SeqCst) {
                        Answer::Yes
                    } else {
                        Answer::No
                    }
                }
                Term::CommandFinished(_) => yes_no(self.exited.load(Ordering::SeqCst)),
                Term::FileExists(_) => yes_no(self.file.load(Ordering::SeqCst)),
                Term::LogMatches { pattern, .. } => {
                    yes_no(self.log.lock().unwrap().contains(pattern))
                }
                Term::MnemeSeq { .. } => {
                    if self.vault_gone.load(Ordering::SeqCst) {
                        Answer::Unanswerable("vault default has no function audit_tail".to_string())
                    } else {
                        yes_no(self.vault_moved.load(Ordering::SeqCst))
                    }
                }
            }
        }
    }

    fn yes_no(holds: bool) -> Answer {
        if holds {
            Answer::Yes
        } else {
            Answer::No
        }
    }

    fn parks_with(fake: Arc<Fake>) -> Arc<Parks> {
        let p = Arc::new(Parks::new());
        p.set_vantage(fake);
        p
    }

    fn spec(condition: Condition, wake: Wake, timeout: Duration) -> Spec {
        Spec {
            call_id: "call-1".into(),
            condition,
            wake,
            timeout,
            epoch: 7,
        }
    }

    /// The compound shape `wait_for` exists for: `(bg finished AND peer
    /// idle) OR the artifact appeared`.
    #[test]
    fn a_compound_condition_parses_and_reads_back() {
        let c = Condition::parse(&json!({
            "any": [
                { "all": [ { "command_finished": "/tmp/bg-1.log" }, { "peer_idle": "eidolon-5a0e" } ] },
                { "file_exists": "/tmp/ready" }
            ]
        }))
        .unwrap();
        let described = c.describe();
        assert!(described.contains("background bg-1 has finished running"), "{described}");
        assert!(described.contains("session eidolon-5a0e is idle"), "{described}");
        assert!(described.contains("/tmp/ready exists"), "{described}");
        assert!(described.contains(" or "), "{described}");
    }

    /// Mail is not a term. Whether it interrupts a park is the park's `wake`
    /// level, and a leaf asking the same question again would only be a
    /// second place to get the answer wrong.
    #[test]
    fn mail_is_a_wake_level_and_not_a_term() {
        let e = Condition::parse(&json!({ "message": true })).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("not a condition term"), "{msg}");
        assert!(
            !msg.contains("`message`, "),
            "the list of terms must not offer it: {msg}"
        );
    }

    #[test]
    fn a_condition_that_is_not_one_says_what_it_expected() {
        let e = Condition::parse(&json!({ "when_the_build_is_done": true })).unwrap_err();
        assert!(format!("{e:#}").contains("log_matches"), "{e:#}");
        let e = Condition::parse(&json!({ "all": [] })).unwrap_err();
        assert!(format!("{e:#}").contains("at least one term"), "{e:#}");
        let e = Condition::parse(&json!({ "log_matches": { "log": "/x" } })).unwrap_err();
        assert!(format!("{e:#}").contains("pattern"), "{e:#}");
    }

    #[tokio::test]
    async fn not_inverts_and_a_gone_peer_outranks_it() {
        let fake = Arc::new(Fake::default());
        let p = parks_with(fake.clone());
        let not_idle = Condition::parse(&json!({ "not": { "peer_idle": "p" } })).unwrap();
        // The peer is busy, so "not idle" is true.
        assert_eq!(p.sample(&not_idle).await, Ok(true));
        fake.gone.store(true, Ordering::SeqCst);
        assert_eq!(
            p.sample(&not_idle).await,
            Err("session p is gone".to_string()),
            "a peer nobody can ask about resolves the condition instead of being negated"
        );
    }

    #[tokio::test]
    async fn arming_a_true_condition_does_not_park_and_a_gone_peer_refuses() {
        let fake = Arc::new(Fake::default());
        fake.idle.store(true, Ordering::SeqCst);
        let p = parks_with(fake.clone());
        let idle = Condition::parse(&json!({ "peer_idle": "p" })).unwrap();
        assert_eq!(
            p.arm(spec(idle.clone(), Wake::Waking, DEFAULT_TIMEOUT)).await,
            Arm::Already
        );
        assert!(!p.is_armed(), "an already-true condition parks nothing");

        fake.idle.store(false, Ordering::SeqCst);
        fake.gone.store(true, Ordering::SeqCst);
        match p.arm(spec(idle, Wake::Waking, DEFAULT_TIMEOUT)).await {
            Arm::Unanswerable(why) => assert!(why.contains("gone"), "{why}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The watcher is already sampling every leaf; the only new thing a
    /// tick carries is the noticing. So the edges, and only the edges: one
    /// tick for the first sample, one when the leaf flips, and none for
    /// the samples in between that changed nothing.
    /// The two wordings of a leaf, and why there are two: the model's
    /// sentence keeps the audit cursor it anchored the wait to; the
    /// view's row says the observable fact and drops the number a person
    /// has no way to read.
    #[test]
    fn a_vault_leaf_says_more_to_the_model_than_to_the_operator() {
        let t = Term::MnemeSeq {
            vault: "default".into(),
            since: 42,
            path: Some("notes/".into()),
            written_by: Some("eidolon-b26d".into()),
        };
        let described = t.describe();
        assert!(described.contains("since 42"), "{described}");
        assert!(described.contains("under notes/"), "{described}");
        assert!(described.contains("by eidolon-b26d"), "{described}");
        let parts = t.parts();
        assert_eq!(parts.kind, "vault");
        assert_eq!(parts.subject, "default");
        assert_eq!(parts.predicate, "has new entries");
        assert_eq!(parts.qualifiers, ["under notes/", "by eidolon-b26d"]);
        assert!(
            !parts.predicate.contains("42"),
            "the cursor is the model's, not the operator's: {parts:?}"
        );
        // The vague word is gone: a log leaf says what it does.
        let t = Term::LogMatches { log: "/tmp/build.log".into(), pattern: "deploy done".into() };
        assert_eq!(t.parts().predicate, "contains \u{201c}deploy done\u{201d}");
        assert!(!t.describe().contains('/'), "no regex slashes: {}", t.describe());
    }

    #[tokio::test]
    async fn a_tick_is_published_on_change_and_only_on_change() {
        let fake = Arc::new(Fake::default());
        let p = parks_with(fake.clone());
        let idle = Condition::parse(&json!({ "peer_idle": "p" })).unwrap();
        let _ = p
            .arm(spec(idle, Wake::Waking, Duration::from_secs(5)))
            .await;
        // Let the first sample land: `peer_idle` is false, so nothing
        // fires, and the arm's own sample was answered inline.
        tokio::time::sleep(Duration::from_millis(30)).await;
        let ticks = p.take_ticks();
        assert_eq!(ticks.len(), 1, "the first sample is the first edge: {ticks:?}");
        assert_eq!(ticks[0].call_id, "call-1");
        assert_eq!(ticks[0].rows.len(), 1);
        assert!(!ticks[0].rows[0].met);
        // Quietly unchanged: the second sample publishes nothing. Ticks
        // are edges, not a heartbeat.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(p.take_ticks().is_empty(), "no change, no tick");
        // The flip is the second edge — and the fire, since the park's
        // only leaf is now true. Give the watcher one POLL to notice,
        // then read every tick it published in one take.
        fake.idle.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(400)).await;
        let ticks = p.take_ticks();
        assert_eq!(ticks.len(), 1, "the flip is one tick: {ticks:?}");
        assert!(ticks[0].rows[0].met);
    }

    #[tokio::test]
    async fn the_watcher_fires_when_the_condition_comes_true() {
        let fake = Arc::new(Fake::default());
        let p = parks_with(fake.clone());
        let mut rx = p.events();
        let idle = Condition::parse(&json!({ "peer_idle": "p" })).unwrap();
        assert_eq!(
            p.arm(spec(idle, Wake::Waking, Duration::from_secs(5))).await,
            Arm::Armed(1)
        );
        assert!(p.armed_for(7).is_some(), "the turn that armed it sees the park");
        assert!(p.armed_for(8).is_none(), "another turn does not");
        fake.idle.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("the watcher never woke")
            .expect("the ring closed");
        let fires = p.take_fires();
        assert_eq!(fires.len(), 1, "{fires:?}");
        assert_eq!(
            fires[0].outcome,
            Outcome::Met(vec!["session p is idle".to_string()])
        );
        assert_eq!(fires[0].call_id, "call-1");
        assert!(!p.is_armed(), "a fire spends the park");
        assert!(p.take_fires().is_empty(), "a fire is read once");
    }

    #[tokio::test]
    async fn the_deadline_is_a_wake_and_says_so() {
        let fake = Arc::new(Fake::default());
        let p = parks_with(fake);
        let idle = Condition::parse(&json!({ "peer_idle": "never" })).unwrap();
        p.arm(spec(idle, Wake::Waking, Duration::from_millis(250))).await;
        let start = Instant::now();
        let mut fired = Vec::new();
        while start.elapsed() < Duration::from_secs(5) && fired.is_empty() {
            tokio::time::sleep(Duration::from_millis(50)).await;
            fired = p.take_fires();
        }
        assert_eq!(fired.len(), 1, "the deadline never fired");
        assert_eq!(
            fired[0].outcome,
            Outcome::TimedOut(vec!["session never is idle".to_string()])
        );
        assert!(fired[0].framed().contains("deadline arrived first"), "{}", fired[0].framed());
        assert!(
            fired[0].framed().contains("still open: session never is idle"),
            "{}",
            fired[0].framed()
        );
    }

    #[test]
    fn a_fire_is_framed_as_the_harness_and_not_as_a_peer() {
        let f = Fired {
            call_id: "c".into(),
            condition: "background bg-1 has finished running".into(),
            outcome: Outcome::Met(vec!["background bg-1 has finished running".into()]),
            waited_ms: 1_500,
        };
        let framed = f.framed();
        assert!(framed.contains("background bg-1 has finished running"), "{framed}");
        assert!(framed.contains("after 1s"), "{framed}");
        assert!(framed.contains("not a peer's message"), "{framed}");
    }

    #[test]
    fn a_wake_level_is_zero_one_or_two() {
        assert_eq!(Wake::from_level(0).unwrap(), Wake::Nothing);
        assert_eq!(Wake::from_level(1).unwrap(), Wake::Waking);
        assert_eq!(Wake::from_level(2).unwrap(), Wake::Any);
        assert_eq!(Wake::default(), Wake::Waking);
        assert!(Wake::from_level(3).is_err());
        assert!(!Wake::Nothing.admits(true));
        assert!(!Wake::Waking.admits(false));
        assert!(Wake::Any.admits(false));
    }

    /// The words are what the journal keeps, so they carry the decision:
    /// which leaf fired, and what was still open when the deadline won —
    /// capped, so a wide condition stays a sentence.
    #[test]
    fn outcome_words_name_the_trigger_or_what_stayed_open() {
        assert_eq!(
            Outcome::Met(vec!["session p is idle".into()]).words(),
            "it fired — session p is idle"
        );
        assert_eq!(
            Outcome::Met(vec!["a".into(), "b".into()]).words(),
            "it fired — a; b"
        );
        assert_eq!(
            Outcome::TimedOut(vec!["a".into(), "b".into(), "c".into(), "d".into()]).words(),
            "the deadline arrived first — still open: a; b; c; and 1 more"
        );
        assert_eq!(
            Outcome::TimedOut(vec![]).words(),
            "the deadline arrived first"
        );
    }

    /// The level is a door on *waking* alone, and this is the door a driver
    /// knocks on: mail it refuses is still drained at the next boundary, so
    /// `Nothing` means "read it later", never "drop it".
    #[tokio::test]
    async fn the_park_decides_which_mail_may_wake_the_session() {
        let fake = Arc::new(Fake::default());
        let p = parks_with(fake);
        assert!(p.admits(true), "with nothing armed, waking mail wakes");
        assert!(!p.admits(false), "and ordinary mail does not");

        let idle = Condition::parse(&json!({ "peer_idle": "never" })).unwrap();
        p.arm(spec(idle.clone(), Wake::Nothing, DEFAULT_TIMEOUT)).await;
        assert!(!p.admits(true), "level 0 ignores mail that asked to wake");
        assert!(!p.admits(false));
        p.clear();

        p.arm(spec(idle.clone(), Wake::Any, DEFAULT_TIMEOUT)).await;
        assert!(p.admits(false), "level 2 wakes for mail that did not ask");
        p.clear();

        // Two parks at once: the more permissive one wins, because a level
        // says what its own park will be interrupted for.
        p.arm(spec(idle.clone(), Wake::Nothing, DEFAULT_TIMEOUT)).await;
        p.arm(spec(idle.clone(), Wake::Any, DEFAULT_TIMEOUT)).await;
        assert!(p.admits(false), "one park's `Any` is not overruled by another's `Nothing`");
        p.clear();

        p.arm(spec(idle, Wake::Waking, DEFAULT_TIMEOUT)).await;
        assert!(p.admits(true), "level 1 is the ordinary rule");
        assert!(!p.admits(false));
    }

    /// The vault's own log as a term: the cursor is in the description,
    /// because a fire's frame quotes the description and nothing else, and
    /// that is where the woken model learns where to read from.
    #[test]
    fn a_vault_cursor_is_a_term_and_says_which_vault_and_where_from() {
        let c = Condition::parse(&json!({
            "mneme_seq": {
                "vault": "default",
                "since": 715,
                "path": "wiki/projects/Eidolon",
                "written_by": "session=eidolon-5a0e"
            }
        }))
        .unwrap();
        let described = c.describe();
        assert!(described.contains("has new entries since 715"), "{described}");
        assert!(described.contains("vault default"), "{described}");
        assert!(described.contains("under wiki/projects/Eidolon"), "{described}");
        assert!(described.contains("by session=eidolon-5a0e"), "{described}");

        // A cursor without its vault is a number with no meaning, and the
        // per-vault log is why: both are required, and the error says which
        // one is missing.
        let e = Condition::parse(&json!({ "mneme_seq": { "since": 3 } })).unwrap_err();
        assert!(format!("{e:#}").contains("`vault`"), "{e:#}");
        let e = Condition::parse(&json!({ "mneme_seq": { "vault": "default" } })).unwrap_err();
        assert!(format!("{e:#}").contains("`since`"), "{e:#}");
    }

    /// One question per distinct leaf, wherever it sits in the tree: a
    /// duplicate must not cost a second round trip to a leaf that leaves the
    /// machine, and the cadence question is asked of the same set.
    #[test]
    fn a_condition_lists_its_leaves_once_each() {
        let c = Condition::parse(&json!({
            "all": [
                { "file_exists": "/tmp/ready" },
                { "not": { "file_exists": "/tmp/ready" } },
                { "mneme_seq": { "vault": "default", "since": 715 } }
            ]
        }))
        .unwrap();
        let terms = c.terms();
        assert_eq!(terms.len(), 2, "{terms:?}");
        assert!(terms.iter().any(|t| matches!(t, Term::MnemeSeq { since: 715, .. })));
        assert!(terms.iter().any(|t| matches!(t, Term::FileExists(_))));
    }

    /// The vault's leaf is answered by the vantage like any other, and an
    /// unanswerable one resolves the park instead of burning its deadline: a
    /// vault that has no such function is not going to grow one.
    #[tokio::test]
    async fn a_vault_leaf_fires_and_a_missing_function_does_not_wait() {
        let fake = Arc::new(Fake::default());
        let p = parks_with(fake.clone());
        let cursor = Condition::parse(
            &json!({ "mneme_seq": { "vault": "default", "since": 715 } }),
        )
        .unwrap();

        // Nothing past the cursor: it parks.
        assert!(matches!(
            p.arm(spec(cursor.clone(), Wake::Waking, Duration::from_secs(5))).await,
            Arm::Armed(_)
        ));
        fake.vault_moved.store(true, Ordering::SeqCst);
        let mut fired = Vec::new();
        let start = Instant::now();
        while fired.is_empty() && start.elapsed() < Duration::from_secs(3) {
            tokio::time::sleep(Duration::from_millis(20)).await;
            fired = p.take_fires();
        }
        assert_eq!(fired.len(), 1, "the vault leaf never fired: {fired:?}");
        assert_eq!(
            fired[0].outcome,
            Outcome::Met(vec!["vault default has new entries since 715".to_string()])
        );

        p.clear();
        fake.vault_gone.store(true, Ordering::SeqCst);
        match p.arm(spec(cursor, Wake::Waking, Duration::from_secs(5))).await {
            Arm::Unanswerable(why) => assert!(why.contains("audit_tail"), "{why}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
