//! The usage panel: what the session has spent, turn by turn, and the
//! page (`:usage`, `space u`) that says so.
//!
//! The status line carries four numbers — context, in, out, spend — and
//! they are the right four for a glance. What they cannot answer is the
//! question that follows the glance: *where did it go?* Which turn cost
//! the most, and why; how much of the input was served from cache;
//! whether the model that ran the expensive half is even one the harness
//! can price; what one more turn on this context is going to cost before
//! a word is typed. Those are per-turn questions, and a running total has
//! thrown the per-turn figures away by construction.
//!
//! So this module keeps a [`Ledger`]: one [`Turn`] per settled turn,
//! carrying the settle's usage, the model it ran on, the price at that
//! model's rates (or the fact that it has none), how many model calls and
//! tool calls it took, the context size it left behind and when it began
//! and ended. Everything is fed through the same handful of methods from
//! **both** paths — the live event handlers and `UiState::seed` — so a
//! resumed session's panel reads exactly as the live one did, and there
//! is no bookkeeping a record could fail to reproduce. The ledger is
//! *derived* from the log and never journaled; it holds nothing the branch
//! does not.
//!
//! ## What is a turn here
//!
//! A settle. `TurnSettled` is the published boundary (see the invariant in
//! `AGENTS.md`), and its `usage` is the turn's total over every model call
//! it made — a tool loop of forty calls is one turn with forty calls, and
//! `calls` says so. `context` is the separate `ContextSize` figure, which
//! is the *last* call's input alone; the two are kept apart here for the
//! reason `eidolon_core::usage` keeps them apart everywhere. A cancel is
//! an *interruption*, not a turn: the branch is usually continued and
//! settles later, and that settle's usage covers only the calls made
//! after the continue — the calls before it happened (and are counted
//! in `calls`) but reported no usage, so whatever they spent is in the
//! provider's ledger and not in this one. The panel counts the
//! interruptions rather than pretending they were free.
//!
//! ## What the account has left is not in the log either
//!
//! On a subscription — z.ai's GLM Coding Plan — the dollars above are an
//! estimate of what the turns *would* have cost, and the figure that
//! actually decides whether the next turn runs is how much of the plan's
//! window is gone. That is the account's number and not the session's:
//! every session on the key spends from it, so it cannot be derived from
//! this branch, and it moves under a session that is idle. It comes from
//! the provider's `quota()` hook (`eidolon_providers::quota`), read live
//! off the driver — at startup, after every turn, when the model moves
//! and when the page is opened on a stale reading — and kept here as a
//! [`QuotaReading`] with the time it was taken, so the page can say how
//! old it is. It is drawn only while the model in play is that
//! provider's: a `:model` onto a metered provider leaves the reading in
//! place and unshown rather than drawing one account's window under
//! another's name.
//!
//! ## What the page shows
//!
//! [`report`] is one function from the ledger and the moment (the model
//! in play, the context gauge, the turn in flight) to the page text, so
//! there is one wording. It is width-aware: every table is a [`Table`]
//! whose columns carry a drop order, and a page drawn on a 70-column
//! terminal loses the column that matters least rather than wrapping the
//! row that matters most. Prices are `eidolon_core::usage::dollars` and
//! counts are `eidolon_core::usage::human`, because a figure that reads
//! differently here and on the status line would be two figures.
//!
//! Nothing here is a `Cost` lookup: the price of a turn is decided where
//! the settle is counted (`UiState::settled`), at the rates of the model
//! then in play, and handed in. A ledger that priced turns itself would
//! be a second place the rate table is read, and a mid-session `:model`
//! would be a place for the two to disagree.

use eidolon_core::UsageExt;
use eidolon_core::message::{StopReason, Usage};
use eidolon_core::session::PolicyOutcome;
use eidolon_core::usage::{TurnTiming, dollars, human};

/// One settled turn, as the panel sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    /// The catalog key of the model the turn ran on — `model_key`, never
    /// the bare id, because the rates and the window are keyed on it.
    pub model: String,
    /// The settle's usage: the turn's total over every model call.
    pub usage: Usage,
    pub stop: StopReason,
    /// Dollars at the model's rates, or `None` for a model whose
    /// definition declares none. Not zero: an unpriced turn is a figure
    /// the harness does not have, and summing it in as free would make
    /// the total a floor that reads as a bill.
    pub cost: Option<f64>,
    /// Model calls the turn made (one per assistant message).
    pub calls: u32,
    /// Tool calls that finished during the turn, and how many of those
    /// came back as errors.
    pub tools: u32,
    pub tool_failures: u32,
    /// The `ContextSize` published just before the settle: how full the
    /// window was when the turn ended.
    pub context: Option<u64>,
    /// Wall-clock milliseconds since the epoch, from the record on resume
    /// and from the clock live. Zero is "nobody said" — a log written by a
    /// test, or a turn whose start was never seen — and reads as unknown,
    /// never as 1970.
    pub started_ms: u64,
    pub settled_ms: u64,
    /// What this turn's model calls took, as the harness timed them around
    /// each request — or `None` when nobody did: a driver that owns its own
    /// loop (the Claude CLI), a cancelled turn, or a log written before the
    /// pace was journaled. `None` draws a dash rather than a zero, the same
    /// way an unpriced turn does.
    pub timing: Option<TurnTiming>,
}

impl Turn {
    /// How long the turn took, when both ends are known.
    pub fn duration_ms(&self) -> Option<u64> {
        (self.started_ms > 0 && self.settled_ms >= self.started_ms)
            .then(|| self.settled_ms - self.started_ms)
    }
}

/// Counts of what the gate decided, by outcome. Silent allows are absent
/// for the reason `PolicyVerdict` never records them: nothing was
/// decided, so there is nothing to count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verdicts {
    pub approved: u32,
    pub declined: u32,
    pub judged: u32,
    pub yolo: u32,
    pub refused: u32,
}

impl Verdicts {
    fn total(&self) -> u32 {
        self.approved + self.declined + self.judged + self.yolo + self.refused
    }
}

/// A subscription's allowances as last read, and when.
///
/// See the module docs for why it is a reading and not a ledger column.
/// `provider` is what the page checks the model in play against before
/// drawing it, and `read_ms` is what lets the page say `as of 2m ago`:
/// the fetch after a settle is a round trip, and until it lands the page
/// is showing the reading from before the turn.
#[derive(Clone, Debug, PartialEq)]
pub struct QuotaReading {
    /// The provider whose account was read: the `zai` of `zai:glm-5.3`.
    pub provider: String,
    /// When the reading was taken, as milliseconds since the epoch; zero
    /// is "nobody said" and draws no age.
    pub read_ms: u64,
    /// The reading, or the sentence about why there is none — the
    /// endpoint's own where it gave one, the harness's otherwise.
    pub result: Result<eidolon_providers::Quota, String>,
}

/// The provider half of a catalog key: `zai` of `zai:glm-5.3`. `None` for
/// a key with no provider in it — the mock — which no account is behind.
pub fn provider_of(key: &str) -> Option<&str> {
    key.split_once(':').map(|(p, _)| p)
}

/// What has happened since the last settle: the half of a turn the
/// ledger sees before it has a usage figure to close it with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Open {
    started_ms: u64,
    calls: u32,
    tools: u32,
    tool_failures: u32,
    context: Option<u64>,
}

/// Per-tool totals over the whole branch: `(calls, failures)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolTally {
    pub calls: u32,
    pub failures: u32,
}

/// The per-turn record of a session, derived from its branch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ledger {
    pub turns: Vec<Turn>,
    /// Every tool ever reached for, by name, in first-use order — the
    /// order the transcript's clusters use, so the two tables agree on
    /// what came first.
    pub tools: Vec<(String, ToolTally)>,
    pub verdicts: Verdicts,
    /// Cancels: turns interrupted, whether or not they were continued
    /// to a settle afterwards.
    pub cancelled: u32,
    pub compactions: u32,
    open: Open,
}

impl Ledger {
    /// The first thing a turn did, at `ts_ms`. Idempotent: only the first
    /// call after a settle sets the start, so every event handler can
    /// say it without checking whether another already has.
    pub fn began(&mut self, ts_ms: u64) {
        if self.open.started_ms == 0 && ts_ms > 0 {
            self.open.started_ms = ts_ms;
        }
    }

    /// A model call was made (an assistant message landed).
    pub fn called(&mut self, ts_ms: u64) {
        self.began(ts_ms);
        self.open.calls += 1;
    }

    /// A tool call finished.
    pub fn tool(&mut self, name: &str, failed: bool, ts_ms: u64) {
        self.began(ts_ms);
        self.open.tools += 1;
        if failed {
            self.open.tool_failures += 1;
        }
        let tally = match self.tools.iter_mut().find(|(n, _)| n == name) {
            Some((_, t)) => t,
            None => {
                self.tools.push((name.to_string(), ToolTally::default()));
                &mut self.tools.last_mut().expect("just pushed").1
            }
        };
        tally.calls += 1;
        if failed {
            tally.failures += 1;
        }
    }

    /// The context size was measured — published just before a settle.
    pub fn context(&mut self, tokens: u64) {
        self.open.context = Some(tokens);
    }

    /// The branch was compacted: the context restarts at the summary, whose
    /// measured size arrives in the `ContextSize` that follows the
    /// compaction. Until it does — an older log has no such record — the
    /// honest value is *unknown*, not zero.
    pub fn compacted(&mut self) {
        self.compactions += 1;
        self.open.context = None;
    }

    /// The gate decided something out loud.
    pub fn verdict(&mut self, outcome: &PolicyOutcome) {
        match outcome {
            PolicyOutcome::Approved => self.verdicts.approved += 1,
            PolicyOutcome::Declined => self.verdicts.declined += 1,
            PolicyOutcome::Judged => self.verdicts.judged += 1,
            PolicyOutcome::Yolo => self.verdicts.yolo += 1,
            PolicyOutcome::Refused => self.verdicts.refused += 1,
        }
    }

    /// The turn was cancelled: counted here, and priced separately by
    /// the [`Ledger::settled`] the caller makes beside this one when the
    /// backend reported a usage — see `RecordKind::TurnSpend`. It used
    /// to be counted and never priced, because there was no figure to
    /// price: the loop summed one and threw it away. What is priced is a
    /// *floor* — the call that was actually interrupted reports nothing
    /// — which is why the row it makes carries `StopReason::Cancelled`
    /// rather than passing for a settle.
    ///
    /// What was open stays open — the calls and tools it ran happened,
    /// and a `continue_turn` that finishes the branch settles them.
    pub fn cancelled(&mut self) {
        self.cancelled += 1;
    }

    /// A turn settled on `model`, priced at `cost` by whoever holds the
    /// rate table. Closes what was open into a [`Turn`].
    pub fn settled(
        &mut self,
        model: &str,
        usage: Usage,
        stop: StopReason,
        cost: Option<f64>,
        timing: Option<TurnTiming>,
        ts_ms: u64,
    ) {
        let open = std::mem::take(&mut self.open);
        self.turns.push(Turn {
            model: model.to_string(),
            usage,
            stop,
            cost,
            calls: open.calls,
            tools: open.tools,
            tool_failures: open.tool_failures,
            context: open.context,
            started_ms: open.started_ms,
            settled_ms: ts_ms,
            timing,
        });
    }

    /// What the turn in flight has done so far, for the panel's
    /// *in flight* line: `(model calls, tool calls, since ms)`.
    pub fn in_flight(&self) -> (u32, u32, u64) {
        (self.open.calls, self.open.tools, self.open.started_ms)
    }

    /// Every settled turn's usage, summed.
    pub fn total(&self) -> Usage {
        let mut total = Usage::default();
        for t in &self.turns {
            total += t.usage;
        }
        total
    }

    /// Dollars over the priced turns, and how many turns were not priced.
    /// `None` when nothing has been priced at all, which is a session on
    /// a backend that bills elsewhere and not a session that cost nothing.
    pub fn spend(&self) -> Option<(f64, usize)> {
        let priced: Vec<f64> = self.turns.iter().filter_map(|t| t.cost).collect();
        let unpriced = self.turns.len() - priced.len();
        (!priced.is_empty()).then(|| (priced.iter().sum(), unpriced))
    }

    /// Milliseconds the model was actually working, summed over the turns
    /// whose ends are both known — not the span of the session, which
    /// includes every minute the operator spent reading.
    pub fn active_ms(&self) -> u64 {
        self.turns.iter().filter_map(Turn::duration_ms).sum()
    }

    /// Model calls over every settled turn.
    pub fn calls(&self) -> u32 {
        self.turns.iter().map(|t| t.calls).sum()
    }

    /// The models the branch ran on, in the order they were first
    /// reached for, each with its turns.
    pub fn by_model(&self) -> Vec<(&str, Vec<&Turn>)> {
        let mut out: Vec<(&str, Vec<&Turn>)> = Vec::new();
        for t in &self.turns {
            match out.iter_mut().find(|(m, _)| *m == t.model) {
                Some((_, turns)) => turns.push(t),
                None => out.push((t.model.as_str(), vec![t])),
            }
        }
        out
    }
}

/// The moment the page is drawn at: what the ledger cannot know because
/// it is not in the log, or not yet.
#[derive(Clone, Debug, Default)]
pub struct Now<'a> {
    /// The model in play — where the *next* turn's rates come from.
    pub model: &'a str,
    /// Its rates, when its definition declares any. `None` is said on
    /// the page, because a spend that stops growing after a `:model` is
    /// otherwise a mystery.
    pub rates: Option<&'a eidolon_providers::Cost>,
    pub context_tokens: Option<u64>,
    pub context_limit: Option<u64>,
    /// What the subscription behind the model in play has left, as last
    /// read — already checked against the model by whoever built this,
    /// so a reading here is one to draw.
    pub quota: Option<&'a QuotaReading>,
    /// Milliseconds the turn in flight has been running, when one is.
    pub working_ms: Option<u64>,
    /// Wall-clock now, for the in-flight line when the ledger's own
    /// start is known.
    pub now_ms: u64,
    /// Columns the page has. Tables drop their least important columns
    /// to fit it rather than wrapping.
    pub width: usize,
}

/// The page.
pub fn report(ledger: &Ledger, now: &Now) -> String {
    let width = now.width.max(40);
    // Tables sit two columns in from the labelled lines.
    let table_width = width - 2;
    let mut lines: Vec<String> = Vec::new();
    let total = ledger.total();
    let spend = ledger.spend();

    // ---------------------------------------------------------- headline
    let turns = ledger.turns.len();
    let mut head = vec![format!("{turns} turn{}", plural(turns))];
    if ledger.calls() > 0 {
        head.push(format!(
            "{} model call{}",
            ledger.calls(),
            plural(ledger.calls() as usize)
        ));
    }
    if let Some((usd, _)) = spend {
        head.push(dollars(usd));
    }
    let active = ledger.active_ms();
    if active >= 1_000 {
        head.push(format!("{} active", duration(active)));
    }
    lines.push(head.join(" \u{b7} "));
    lines.push(String::new());

    // ------------------------------------------------------------ gauge
    let gauge_bar = match (now.context_tokens, now.context_limit.filter(|l| *l > 0)) {
        (Some(t), Some(l)) => format!("  {}", bar(t as f64 / l as f64, 20)),
        _ => String::new(),
    };
    lines.push(kv(
        "context",
        &format!(
            "{}{gauge_bar}",
            eidolon_core::usage::context_line(now.context_tokens, now.context_limit)
        ),
    ));

    // ---------------------------------------------------------- in flight
    if let Some(ms) = now.working_ms {
        let (calls, tools, since) = ledger.in_flight();
        let since_ms = if since > 0 && now.now_ms > since {
            now.now_ms - since
        } else {
            ms
        };
        let mut parts = vec![duration(since_ms)];
        if calls > 0 {
            parts.push(format!("{calls} model call{}", plural(calls as usize)));
        }
        if tools > 0 {
            parts.push(format!("{tools} tool call{}", plural(tools as usize)));
        }
        parts.push("not yet priced".into());
        lines.push(kv("in flight", &parts.join(" \u{b7} ")));
    }

    // ------------------------------------------------------------ totals
    if turns == 0 {
        lines.push(kv("turns", "none settled yet"));
    } else {
        let mut parts = vec![format!("{turns} settled")];
        if ledger.cancelled > 0 {
            parts.push(format!(
                "interrupted {} time{}",
                ledger.cancelled,
                plural(ledger.cancelled as usize)
            ));
        }
        if ledger.compactions > 0 {
            parts.push(format!(
                "{} compaction{}",
                ledger.compactions,
                plural(ledger.compactions as usize)
            ));
        }
        let cut: usize = ledger
            .turns
            .iter()
            .filter(|t| matches!(t.stop, StopReason::MaxTokens))
            .count();
        if cut > 0 {
            parts.push(format!("{cut} cut off at the output cap"));
        }
        lines.push(kv("turns", &parts.join(" \u{b7} ")));

        let tool_calls: u32 = ledger.tools.iter().map(|(_, t)| t.calls).sum();
        let tool_failures: u32 = ledger.tools.iter().map(|(_, t)| t.failures).sum();
        if tool_calls > 0 {
            let mut parts = vec![format!("{tool_calls} call{}", plural(tool_calls as usize))];
            if tool_failures > 0 {
                parts.push(format!("{tool_failures} failed"));
            }
            lines.push(kv("tools", &parts.join(" \u{b7} ")));
        }

        let mut input = vec![format!("{} fresh", human(total.input_tokens))];
        if total.cache_read_input_tokens > 0 {
            input.push(format!(
                "{} from cache ({})",
                human(total.cache_read_input_tokens),
                pct(total.cache_read_input_tokens, total.total_input())
            ));
        }
        if total.cache_creation_input_tokens > 0 {
            input.push(format!(
                "{} written to cache",
                human(total.cache_creation_input_tokens)
            ));
        }
        lines.push(kv(
            "input",
            &format!(
                "{:<7} {}",
                human(total.total_input()),
                input.join(" \u{b7} ")
            ),
        ));
        lines.push(kv("output", &human(total.output_tokens)));
        lines.push(kv("total", &format!("{} tokens", human(total.total()))));

        match spend {
            Some((usd, unpriced)) => {
                let mut s = dollars(usd);
                if unpriced > 0 {
                    let on: Vec<&str> = {
                        let mut seen: Vec<&str> = Vec::new();
                        for t in ledger.turns.iter().filter(|t| t.cost.is_none()) {
                            if !seen.contains(&t.model.as_str()) {
                                seen.push(&t.model);
                            }
                        }
                        seen
                    };
                    s.push_str(&format!(
                        " \u{b7} {unpriced} turn{} unpriced (on {})",
                        plural(unpriced),
                        on.join(", ")
                    ));
                }
                lines.push(kv("spend", &s));
            }
            None => lines.push(kv(
                "spend",
                "unpriced: no turn ran on a model whose definition declares rates",
            )),
        }

        // A rate needs a denominator worth dividing by: a mock turn that
        // took four milliseconds extrapolates to millions of tokens an
        // hour, which is arithmetic and not information.
        if active >= RATE_FLOOR_MS {
            let hours = active as f64 / 3_600_000.0;
            let mut parts = Vec::new();
            if let Some((usd, 0)) = spend {
                parts.push(format!("{}/h", dollars(usd / hours)));
            }
            parts.push(format!(
                "{} tokens/h",
                human((total.total() as f64 / hours) as u64)
            ));
            let timed = ledger
                .turns
                .iter()
                .filter(|t| t.duration_ms().is_some())
                .count();
            parts.push(format!(
                "{} per turn",
                duration(active / timed.max(1) as u64)
            ));
            lines.push(kv(
                "rate",
                &format!(
                    "{}  (over the {} the model was working)",
                    parts.join(" \u{b7} "),
                    duration(active)
                ),
            ));
        }
    }

    // -------------------------------------------------------------- gate
    if ledger.verdicts.total() > 0 {
        let v = &ledger.verdicts;
        let mut parts = Vec::new();
        for (n, word) in [
            (v.approved, "approved"),
            (v.declined, "declined"),
            (v.judged, "judged"),
            (v.yolo, "yolo"),
            (v.refused, "refused"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {word}"));
            }
        }
        lines.push(kv(
            "gate",
            &format!(
                "{} question{} \u{b7} {}",
                v.total(),
                plural(v.total() as usize),
                parts.join(" \u{b7} ")
            ),
        ));
    }

    // -------------------------------------------------------- next turn
    lines.push(String::new());
    lines.push(kv("model", now.model));
    match now.rates {
        Some(c) => {
            let per_m = |r: f64| {
                if r > 0.0 {
                    format!("${r}")
                } else {
                    "\u{2014}".into()
                }
            };
            lines.push(kv(
                "rates",
                &format!(
                    "in {} \u{b7} out {} \u{b7} cache read {} \u{b7} cache write {}  per M tokens",
                    per_m(c.input),
                    per_m(c.output),
                    per_m(c.cache_read),
                    per_m(c.cache_write)
                ),
            ));
            if let Some(ctx) = now.context_tokens.filter(|c| *c > 0) {
                let (rate, at) = if c.cache_read > 0.0 {
                    (c.cache_read, "cache-read")
                } else {
                    (c.input, "input")
                };
                let floor = ctx as f64 * rate / 1_000_000.0;
                lines.push(kv("next turn", &format!("\u{2265} {} to re-read {} of context at the {at} rate, before a word is written", dollars(floor), human(ctx))));
            }
        }
        None => lines.push(kv(
            "rates",
            "none declared: turns on this model are unpriced",
        )),
    }

    // -------------------------------------------------------------- quota
    if let Some(q) = now.quota {
        lines.extend(quota_lines(q, now.now_ms, width));
    }

    // ---------------------------------------------------------- by model
    let by_model = ledger.by_model();
    if by_model.len() > 1 || by_model.first().is_some_and(|(m, _)| *m != now.model) {
        lines.push(String::new());
        lines.push("by model".into());
        let cols = [
            Col::left("model", 0),
            Col::right("turns", 3),
            Col::right("calls", 4),
            Col::right("in", 0),
            Col::right("cached", 2),
            Col::right("out", 0),
            Col::right("spend", 0),
        ];
        let rows: Vec<Vec<String>> = by_model
            .iter()
            .map(|(model, turns)| {
                let mut u = Usage::default();
                for t in turns {
                    u += t.usage;
                }
                let calls: u32 = turns.iter().map(|t| t.calls).sum();
                let priced: Vec<f64> = turns.iter().filter_map(|t| t.cost).collect();
                let spend = match priced.len() {
                    0 => "unpriced".to_string(),
                    n if n == turns.len() => dollars(priced.iter().sum()),
                    n => format!(
                        "{} ({} unpriced)",
                        dollars(priced.iter().sum()),
                        turns.len() - n
                    ),
                };
                vec![
                    (*model).to_string(),
                    turns.len().to_string(),
                    calls.to_string(),
                    human(u.total_input()),
                    pct(u.cache_read_input_tokens, u.total_input()),
                    human(u.output_tokens),
                    spend,
                ]
            })
            .collect();
        lines.extend(
            Table::new(&cols, rows)
                .render(table_width)
                .into_iter()
                .map(|l| format!("  {l}")),
        );
    }

    // ----------------------------------------------------------- by tool
    if !ledger.tools.is_empty() {
        lines.push(String::new());
        lines.push("by tool".into());
        let cols = [
            Col::left("tool", 0),
            Col::right("calls", 0),
            Col::right("failed", 0),
        ];
        let mut rows: Vec<Vec<String>> = ledger
            .tools
            .iter()
            .map(|(name, t)| {
                vec![
                    name.clone(),
                    t.calls.to_string(),
                    if t.failures > 0 {
                        t.failures.to_string()
                    } else {
                        "\u{b7}".into()
                    },
                ]
            })
            .collect();
        // Busiest first: the question is what the model kept reaching
        // for, and the transcript already has the order of first use.
        rows.sort_by(|a, b| {
            b[1].parse::<u32>()
                .unwrap_or(0)
                .cmp(&a[1].parse::<u32>().unwrap_or(0))
        });
        lines.extend(
            Table::new(&cols, rows)
                .render(table_width)
                .into_iter()
                .map(|l| format!("  {l}")),
        );
    }

    // ------------------------------------------------------------- turns
    if !ledger.turns.is_empty() {
        lines.push(String::new());
        lines.push("turns".into());
        let cols = [
            Col::right("#", 0),
            Col::right("calls", 2),
            Col::right("tools", 3),
            Col::right("in", 0),
            Col::right("cached", 4),
            Col::right("out", 0),
            Col::right("ctx", 1),
            Col::right("cost", 0),
            Col::right("time", 5),
            Col::left("ended", 0),
        ];
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut model: Option<&str> = None;
        for (i, t) in ledger.turns.iter().enumerate() {
            // The model as a break in the table rather than a column:
            // it changes rarely, and a column that repeats one key
            // fourteen times is the width the `time` column needed.
            if by_model.len() > 1 && model != Some(t.model.as_str()) {
                model = Some(&t.model);
                rows.push(vec![format!("\u{2500}\u{2500} {}", t.model)]);
            }
            // Failures wear the mark the `ended` column uses, so one
            // glyph means "something went wrong" across the row.
            let tools = match t.tool_failures {
                0 => t.tools.to_string(),
                f => format!("{} \u{26a0}{f}", t.tools),
            };
            rows.push(vec![
                (i + 1).to_string(),
                t.calls.to_string(),
                tools,
                human(t.usage.total_input()),
                pct(t.usage.cache_read_input_tokens, t.usage.total_input()),
                human(t.usage.output_tokens),
                t.context.map(human).unwrap_or_else(|| "\u{b7}".into()),
                t.cost.map(dollars).unwrap_or_else(|| "\u{b7}".into()),
                {
                    let mut time = t
                        .duration_ms()
                        .map(duration)
                        .unwrap_or_else(|| "\u{b7}".into());
                    // The rate rides in the time column rather than taking
                    // one of its own: it answers the same question the
                    // duration does, and it is a dot far more often —
                    // nothing journals a client-measured pace, so a turn
                    // read back out of the log has none.
                    if let Some(rate) = t.timing.and_then(|x| x.tok_per_sec(t.usage.output_tokens))
                    {
                        time.push_str(&format!(" \u{b7} {rate:.0}/s"));
                    }
                    time
                },
                ended(t.stop).into(),
            ]);
        }
        lines.extend(
            Table::new(&cols, rows)
                .render(table_width)
                .into_iter()
                .map(|l| format!("  {l}")),
        );
        if ledger.turns.iter().any(|t| t.cost.is_none()) {
            lines.push(String::new());
            lines.push(
                "  \u{b7} in the cost column is a turn on a model with no declared rates.".into(),
            );
        }
        if ledger
            .turns
            .iter()
            .any(|t| t.timing.and_then(|x| x.tok_per_sec(t.usage.output_tokens)).is_some())
        {
            lines.push(String::new());
            lines.push(
                "  \u{b7}/s in the time column is output tokens per second of decode, timed here \
around each call and journaled with the turn; a log written before that record existed has \
none."
                    .into(),
            );
        }
    }

    lines.join("\n")
}

/// The `quota` lines: the provider, its plan and the reading's age on the
/// first, then a gauge per window.
///
/// ```text
/// quota      zai · lite plan · as of 12s ago
///            5h      7%  █░░░░░░░░░░░░░░░░░░░  158 of 2000 · resets in 3h 42m
///            week    1%  ░░░░░░░░░░░░░░░░░░░░  158 of 10000 · resets in 6d 19h
/// ```
///
/// The percentage leads and the count follows, because the count is in
/// the endpoint's own unnamed unit — it says how finely the window is
/// metered and what one turn moved it by, and nothing about what it
/// costs. On a narrow page the count goes first and the bar shortens,
/// as a table sheds its least important column. A reading that failed is
/// one line saying so, cut to the page rather than wrapped: the useful
/// part of an HTTP error is its front.
fn quota_lines(q: &QuotaReading, now_ms: u64, width: usize) -> Vec<String> {
    let age = (q.read_ms > 0 && now_ms >= q.read_ms)
        .then(|| format!("as of {} ago", duration(now_ms - q.read_ms)));
    let quota = match &q.result {
        Ok(quota) => quota,
        Err(e) => {
            let mut s = format!("{}: could not be read: {e}", q.provider);
            if let Some(age) = age {
                s.push_str(&format!(" ({age})"));
            }
            return vec![kv("quota", &clip(&s, width.saturating_sub(11)))];
        }
    };
    let mut head = vec![q.provider.clone()];
    if let Some(plan) = &quota.plan {
        head.push(format!("{plan} plan"));
    }
    if quota.windows.is_empty() {
        head.push("no windows reported".into());
    }
    head.extend(age);
    let mut lines = vec![kv("quota", &head.join(" \u{b7} "))];

    let wide = width >= 72;
    let bar_width = if wide { 20 } else { 10 };
    // One column each for the labels and the percentages, so the bars
    // line up whatever the words and however many decimals.
    let pct = |w: &eidolon_providers::QuotaWindow| {
        if w.percent.fract() == 0.0 {
            format!("{}%", w.percent as u64)
        } else {
            format!("{:.1}%", w.percent)
        }
    };
    let label_width = quota
        .windows
        .iter()
        .map(|w| cells(&w.label))
        .max()
        .unwrap_or(0)
        .max(4);
    let pct_width = quota
        .windows
        .iter()
        .map(|w| cells(&pct(w)))
        .max()
        .unwrap_or(0)
        .max(4);
    for w in &quota.windows {
        let mut rest = Vec::new();
        if let (Some(used), Some(limit), true) = (w.used, w.limit, wide) {
            rest.push(format!("{used} of {limit}"));
        }
        if let Some(at) = w.resets_ms.filter(|_| now_ms > 0) {
            rest.push(if at > now_ms {
                format!("resets in {}", duration(at - now_ms))
            } else {
                "reset due".into()
            });
        }
        let mut line = format!(
            "{:<11}{:<label_width$}  {:>pct_width$}  {}",
            "",
            w.label,
            pct(w),
            bar(w.percent / 100.0, bar_width)
        );
        if !rest.is_empty() {
            line.push_str("  ");
            line.push_str(&rest.join(" \u{b7} "));
        }
        lines.push(line);
    }
    lines
}

/// Wall-clock milliseconds since the epoch: what a live event is stamped
/// with, in the unit `Record::ts_ms` already uses, so a ledger row fed
/// live and one fed on resume carry the same kind of number.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The least the model has to have been working, summed over the turns,
/// before a per-hour rate is quoted from it.
const RATE_FLOOR_MS: u64 = 10_000;

/// A labelled line: the label in a fixed column so the values line up.
fn kv(label: &str, value: &str) -> String {
    format!("{label:<10} {value}")
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// `82%`, or `0%` for nothing of nothing.
pub fn pct(part: u64, whole: u64) -> String {
    match whole {
        0 => "0%".into(),
        _ => format!("{}%", (part as f64 / whole as f64 * 100.0).round() as u64),
    }
}

/// `42s`, `1m 12s`, `1h 03m`, `2d 4h`: the unit a person reads a wait
/// in, and never milliseconds.
pub fn duration(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        3600..=86_399 => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d {}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// A gauge of `width` cells, `frac` of them filled.
fn bar(frac: f64, width: usize) -> String {
    let filled = ((frac.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    format!(
        "{}{}",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(width - filled)
    )
}

/// The stop reason as a word for a column. The healthy ending is `ok`;
/// the ones `usage::stop_note` would have raised an alert for carry the
/// same mark here, so a truncated turn is as visible in the table as it
/// was on the transcript.
pub fn ended(stop: StopReason) -> &'static str {
    match stop {
        StopReason::EndTurn => "ok",
        StopReason::ToolUse => "tool use",
        StopReason::MaxTokens => "\u{26a0} cut off",
        StopReason::Refusal => "\u{26a0} refused",
        StopReason::StopSequence => "stop seq",
        StopReason::Cancelled => "cancelled",
        StopReason::Other => "\u{26a0} unknown",
        // Healthy, and marked as neither: the session parked on a condition
        // it named, and the turn will be continued by the wake.
        StopReason::Waiting => "waiting",
        // The operator closed the question; not a failure, like
        // `cancelled` above.
        StopReason::Dismissed => "dismissed",
    }
}

/// A column of a [`Table`].
#[derive(Clone, Copy, Debug)]
pub struct Col {
    head: &'static str,
    right: bool,
    /// When the table is too wide, columns go in *descending* order of
    /// this — the highest number first — and `0` never goes. So the
    /// essential columns are `0` and the rest rank what a narrow
    /// terminal can spare.
    drop: u8,
}

impl Col {
    pub fn left(head: &'static str, drop: u8) -> Self {
        Col {
            head,
            right: false,
            drop,
        }
    }

    pub fn right(head: &'static str, drop: u8) -> Self {
        Col {
            head,
            right: true,
            drop,
        }
    }
}

/// A fixed-column table that fits its width by shedding columns.
///
/// Rows with a single cell are *breaks*: drawn as they are across the
/// table, with no columns — the model heading inside the turns table.
pub struct Table {
    cols: Vec<Col>,
    rows: Vec<Vec<String>>,
}

const GUTTER: usize = 2;

impl Table {
    pub fn new(cols: &[Col], rows: Vec<Vec<String>>) -> Self {
        Table {
            cols: cols.to_vec(),
            rows,
        }
    }

    /// The lines, none wider than `width` unless the essential columns
    /// alone are — in which case the widest left-aligned one is cut
    /// with an ellipsis rather than the row being wrapped.
    pub fn render(&self, width: usize) -> Vec<String> {
        let mut keep: Vec<usize> = (0..self.cols.len()).collect();
        let widths = |keep: &[usize]| -> Vec<usize> {
            keep.iter()
                .map(|&c| {
                    self.rows
                        .iter()
                        .filter(|r| r.len() > 1)
                        .map(|r| cells(&r[c]))
                        .chain(std::iter::once(cells(self.cols[c].head)))
                        .max()
                        .unwrap_or(0)
                })
                .collect()
        };
        let total = |w: &[usize]| w.iter().sum::<usize>() + GUTTER * w.len().saturating_sub(1);
        let mut w = widths(&keep);
        while total(&w) > width {
            let Some(pos) = (0..keep.len())
                .filter(|&i| self.cols[keep[i]].drop > 0)
                .max_by_key(|&i| self.cols[keep[i]].drop)
            else {
                break;
            };
            keep.remove(pos);
            w = widths(&keep);
        }
        // Still too wide on the essentials alone: cut the widest
        // left-aligned column down to what is left.
        if total(&w) > width
            && let Some(pos) = (0..keep.len())
                .filter(|&i| !self.cols[keep[i]].right)
                .max_by_key(|&i| w[i])
        {
            let others = total(&w) - w[pos];
            w[pos] = width.saturating_sub(others).max(4);
        }

        let mut out = Vec::new();
        let line = |cells_: Vec<String>| -> String {
            let mut s = String::new();
            for (i, (cell, &c)) in cells_.iter().zip(&keep).enumerate() {
                if i > 0 {
                    s.push_str(&" ".repeat(GUTTER));
                }
                let cell = clip(cell, w[i]);
                let pad = w[i].saturating_sub(cells(&cell));
                if self.cols[c].right {
                    s.push_str(&" ".repeat(pad));
                    s.push_str(&cell);
                } else {
                    s.push_str(&cell);
                    s.push_str(&" ".repeat(pad));
                }
            }
            s.trim_end().to_string()
        };
        out.push(line(
            keep.iter()
                .map(|&c| self.cols[c].head.to_string())
                .collect(),
        ));
        for r in &self.rows {
            if r.len() == 1 {
                out.push(clip(&r[0], width));
            } else {
                out.push(line(keep.iter().map(|&c| r[c].clone()).collect()));
            }
        }
        out
    }
}

/// Columns a string takes: characters, since nothing here is wide.
fn cells(s: &str) -> usize {
    s.chars().count()
}

/// `s`, or its first `width - 1` characters and an ellipsis.
fn clip(s: &str, width: usize) -> String {
    if cells(s) <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(fresh: u64, cached: u64, written: u64, out: u64) -> Usage {
        Usage {
            input_tokens: fresh,
            output_tokens: out,
            cache_creation_input_tokens: written,
            cache_read_input_tokens: cached,
        }
    }

    fn cost() -> eidolon_providers::Cost {
        eidolon_providers::Cost {
            input: 0.28,
            output: 0.42,
            cache_read: 0.028,
            cache_write: 0.0,
        }
    }

    fn now<'a>(width: usize) -> Now<'a> {
        Now {
            model: "deepseek:deepseek-chat",
            context_tokens: Some(142_300),
            context_limit: Some(200_000),
            width,
            ..Default::default()
        }
    }

    /// A branch of three turns: two priced on DeepSeek, one unpriced on
    /// the Claude CLI, with a tool loop in the second.
    fn session() -> Ledger {
        let mut l = Ledger::default();
        l.began(1_000);
        l.called(1_000);
        l.context(4_700);
        l.settled(
            "deepseek:deepseek-chat",
            u(3_900, 0, 0, 812),
            StopReason::EndTurn,
            Some(0.0014),
            None,
            6_000,
        );

        l.began(10_000);
        l.called(10_000);
        l.tool("shell", false, 11_000);
        l.tool("shell", true, 12_000);
        l.tool("read", false, 13_000);
        l.called(14_000);
        l.verdict(&PolicyOutcome::Approved);
        l.context(42_000);
        l.settled(
            "deepseek:deepseek-chat",
            u(20_000, 60_000, 2_000, 2_100),
            StopReason::MaxTokens,
            Some(0.0312),
            None,
            70_000,
        );

        l.began(100_000);
        l.called(100_000);
        l.context(110_000);
        l.settled(
            "claude-cli:sonnet",
            u(12, 105_000, 5_000, 4_700),
            StopReason::EndTurn,
            None,
            None,
            130_000,
        );
        l
    }

    #[test]
    fn totals_are_the_sum_of_the_settles_and_spend_leaves_unpriced_turns_out() {
        let l = session();
        let t = l.total();
        assert_eq!(t.total_input(), 3_900 + 82_000 + 110_012);
        assert_eq!(t.output_tokens, 812 + 2_100 + 4_700);
        assert_eq!(t.cache_read_input_tokens, 165_000);
        let (usd, unpriced) = l.spend().unwrap();
        assert!((usd - 0.0326).abs() < 1e-9);
        assert_eq!(
            unpriced, 1,
            "the CLI turn is counted, not summed in as free"
        );
        assert_eq!(l.calls(), 4);
        assert_eq!(l.active_ms(), 5_000 + 60_000 + 30_000);
    }

    #[test]
    fn what_was_open_closes_into_the_turn_that_settles() {
        let l = session();
        let second = &l.turns[1];
        assert_eq!(
            (second.calls, second.tools, second.tool_failures),
            (2, 3, 1)
        );
        assert_eq!(second.context, Some(42_000));
        assert_eq!(second.duration_ms(), Some(60_000));
        // And the next turn starts from nothing.
        let third = &l.turns[2];
        assert_eq!((third.calls, third.tools), (1, 0));
        assert_eq!(l.in_flight(), (0, 0, 0));
    }

    #[test]
    fn tools_are_tallied_by_name_in_first_use_order() {
        let l = session();
        assert_eq!(l.tools.len(), 2);
        assert_eq!(
            l.tools[0],
            (
                "shell".into(),
                ToolTally {
                    calls: 2,
                    failures: 1
                }
            )
        );
        assert_eq!(
            l.tools[1],
            (
                "read".into(),
                ToolTally {
                    calls: 1,
                    failures: 0
                }
            )
        );
    }

    #[test]
    fn models_come_in_the_order_they_were_reached_for() {
        let l = session();
        let by = l.by_model();
        assert_eq!(
            by.iter().map(|(m, t)| (*m, t.len())).collect::<Vec<_>>(),
            vec![("deepseek:deepseek-chat", 2), ("claude-cli:sonnet", 1)]
        );
    }

    #[test]
    fn a_turn_whose_start_nobody_saw_has_no_duration() {
        let mut l = Ledger::default();
        l.settled("m", u(1, 0, 0, 1), StopReason::EndTurn, None, None, 5_000);
        assert_eq!(l.turns[0].duration_ms(), None);
        assert_eq!(l.active_ms(), 0);
        // A zero timestamp (a test log) never becomes a start either.
        l.began(0);
        assert_eq!(l.in_flight().2, 0);
    }

    #[test]
    fn the_page_says_where_it_went() {
        let l = session();
        let mut n = now(100);
        let c = cost();
        n.rates = Some(&c);
        let page = report(&l, &n);
        // The headline and the totals.
        assert!(
            page.starts_with("3 turns \u{b7} 4 model calls \u{b7} $0.033 \u{b7} 1m 35s active"),
            "{page}"
        );
        assert!(
            page.contains("$0.031"),
            "a turn keeps its own figure: {page}"
        );
        assert!(
            page.contains("context    142.3k/200.0k (71%)  \u{2588}"),
            "{page}"
        );
        assert!(page.contains("1 cut off at the output cap"), "{page}");
        assert!(
            page.contains("tools      3 calls \u{b7} 1 failed"),
            "{page}"
        );
        assert!(page.contains("165.0k from cache (84%)"), "{page}");
        assert!(
            page.contains("spend      $0.033 \u{b7} 1 turn unpriced (on claude-cli:sonnet)"),
            "{page}"
        );
        assert!(
            page.contains("gate       1 question \u{b7} 1 approved"),
            "{page}"
        );
        // The rates in play and what the next turn costs before it starts.
        assert!(page.contains("rates      in $0.28 \u{b7} out $0.42 \u{b7} cache read $0.028 \u{b7} cache write \u{2014}"), "{page}");
        assert!(
            page.contains(
                "next turn  \u{2265} $0.0040 to re-read 142.3k of context at the cache-read rate"
            ),
            "{page}"
        );
        // The three tables.
        assert!(page.contains("by model"), "{page}");
        assert!(page.contains("claude-cli:sonnet"), "{page}");
        assert!(page.contains("unpriced"), "{page}");
        assert!(page.contains("by tool"), "{page}");
        assert!(
            page.contains("\u{2500}\u{2500} deepseek:deepseek-chat"),
            "the turns table breaks on a model change: {page}"
        );
        assert!(page.contains("\u{26a0} cut off"), "{page}");
        assert!(page.contains("3 \u{26a0}1"), "{page}");
        // No rate per hour when a turn is unpriced: dollars over an hour
        // that includes an unpriced turn would be a floor.
        let rate = page
            .lines()
            .find(|l| l.starts_with("rate"))
            .expect("a rate line");
        assert!(!rate.contains('$'), "{rate}");
        assert!(rate.contains("tokens/h"), "{rate}");
    }

    /// A plan's windows are drawn under the model in play with their age,
    /// a failed reading is one line, and a moment with no reading draws
    /// no section at all — a metered provider has nothing to say here.
    #[test]
    fn the_plan_is_drawn_with_its_windows_and_its_age() {
        use eidolon_providers::{Quota, QuotaWindow};
        let l = session();
        let page = report(&l, &now(100));
        assert!(!page.contains("quota"), "{page}");

        let reading = QuotaReading {
            provider: "zai".into(),
            read_ms: 1_000_000,
            result: Ok(Quota {
                plan: Some("lite".into()),
                windows: vec![
                    // Reset times are measured from *now*, not from the reading.
                    QuotaWindow {
                        label: "5h".into(),
                        percent: 7.0,
                        used: Some(158),
                        limit: Some(2000),
                        resets_ms: Some(1_012_000 + 13_320_000),
                    },
                    QuotaWindow {
                        label: "week".into(),
                        percent: 12.5,
                        used: Some(1250),
                        limit: Some(10000),
                        resets_ms: Some(1_012_000 + 6 * 86_400_000 + 19 * 3_600_000),
                    },
                    QuotaWindow {
                        label: "month".into(),
                        percent: 100.0,
                        used: None,
                        limit: None,
                        resets_ms: Some(1_000_000 - 1),
                    },
                ],
            }),
        };
        let n = Now {
            model: "zai:glm-5.3",
            quota: Some(&reading),
            now_ms: 1_012_000,
            width: 100,
            ..Default::default()
        };
        let page = report(&l, &n);
        assert!(
            page.contains("quota      zai \u{b7} lite plan \u{b7} as of 12s ago"),
            "{page}"
        );
        assert!(page.contains("           5h        7%  \u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}  158 of 2000 \u{b7} resets in 3h 42m"), "{page}");
        assert!(
            page.contains("           week   12.5%  "),
            "a fraction keeps one decimal: {page}"
        );
        assert!(
            page.contains("1250 of 10000 \u{b7} resets in 6d 19h"),
            "{page}"
        );
        assert!(
            page.contains(
                &("           month   100%  ".to_string() + &"\u{2588}".repeat(20) + "  reset due")
            ),
            "no count when the endpoint gave none, and a reset in the past says so: {page}"
        );
        // The section sits with the model it is about.
        let at = |needle: &str| {
            page.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {page}"))
        };
        assert!(
            at("model      zai") < at("quota      zai") && at("quota      zai") < at("by model"),
            "{page}"
        );

        // Narrow: the count goes and the bar halves, so the line fits.
        let page = report(&l, &Now { width: 60, ..n });
        assert!(page.contains("           5h        7%  \u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}  resets in 3h 42m"), "{page}");
        assert!(!page.contains("158 of 2000"), "{page}");

        // A reading nobody dated has no age; one with no windows says so.
        let bare = QuotaReading {
            provider: "zai".into(),
            read_ms: 0,
            result: Ok(Quota::default()),
        };
        let page = report(
            &l,
            &Now {
                quota: Some(&bare),
                ..n
            },
        );
        assert!(
            page.contains("quota      zai \u{b7} no windows reported\n"),
            "{page}"
        );

        // A failed reading is one line, cut to the page.
        let down = QuotaReading { provider: "zai".into(), read_ms: 1_000_000, result: Err("GET https://api.z.ai/api/monitor/usage/quota/limit: HTTP 401: {\"code\":401,\"msg\":\"unauthorized, a very long body that keeps going\"}".into()) };
        let page = report(
            &l,
            &Now {
                quota: Some(&down),
                width: 60,
                ..n
            },
        );
        let line = page.lines().find(|l| l.starts_with("quota")).unwrap();
        assert!(
            line.starts_with("quota      zai: could not be read: GET https://api.z.ai"),
            "{line}"
        );
        assert!(
            line.ends_with('\u{2026}') && line.chars().count() <= 60,
            "{line}"
        );
        assert_eq!(page.matches("could not be read").count(), 1);
    }

    #[test]
    fn a_rate_is_only_quoted_when_every_turn_was_priced() {
        let mut l = Ledger::default();
        l.began(1_000);
        l.settled(
            "m",
            u(1_000, 0, 0, 100),
            StopReason::EndTurn,
            Some(0.5),
            None,
            1_000 + 30 * 60 * 1000,
        );
        let page = report(&l, &now(100));
        assert!(page.contains("rate       $1.00/h"), "{page}");
        assert!(page.contains("30m 00s per turn"), "{page}");
    }

    #[test]
    fn a_measured_turn_carries_its_pace_and_an_unmeasured_one_does_not() {
        // A local daemon turn: 6.6k of prefill, 40 output tokens, 0.3s to
        // the first token and 3.3s of call — so 3s of decode, 13/s.
        let mut t = TurnTiming::default();
        t.add(eidolon_core::usage::CallTiming {
            first_token_ms: Some(300),
            total_ms: 3_300,
        });
        let mut l = Ledger::default();
        l.began(1_000);
        l.settled(
            "ollama-local:qwen2.5:7b",
            u(6_600, 0, 0, 40),
            StopReason::EndTurn,
            None,
            Some(t),
            4_000,
        );
        let page = report(&l, &now(100));
        assert!(page.contains("3s \u{b7} 13/s"), "{page}");
        assert!(page.contains("journaled with the turn"), "{page}");

        // The same turn read back out of the log: nothing timed it, so
        // there is no rate, no footnote, and the duration still stands.
        let mut l = Ledger::default();
        l.began(1_000);
        l.settled(
            "ollama-local:qwen2.5:7b",
            u(6_600, 0, 0, 40),
            StopReason::EndTurn,
            None,
            None,
            4_000,
        );
        let page = report(&l, &now(100));
        assert!(page.contains("3s"), "{page}");
        assert!(!page.contains("/s"), "{page}");
    }

    #[test]
    fn an_empty_session_still_has_a_page() {
        let l = Ledger::default();
        let page = report(&l, &now(80));
        assert!(page.starts_with("0 turns"), "{page}");
        assert!(page.contains("turns      none settled yet"), "{page}");
        assert!(page.contains("rates      none declared"), "{page}");
        assert!(!page.contains("by tool"), "{page}");
        // A backend that bills elsewhere is said, not shown as $0.
        let mut l = Ledger::default();
        l.settled(
            "claude-cli:sonnet",
            u(10, 0, 0, 10),
            StopReason::EndTurn,
            None,
            None,
            0,
        );
        let page = report(&l, &now(80));
        assert!(page.contains("spend      unpriced"), "{page}");
        assert!(!page.contains("$0.00"), "{page}");
    }

    #[test]
    fn the_turn_in_flight_is_on_the_page() {
        let mut l = session();
        l.began(200_000);
        l.called(200_000);
        l.tool("shell", false, 201_000);
        let n = Now {
            working_ms: Some(1_000),
            now_ms: 242_000,
            ..now(100)
        };
        let page = report(&l, &n);
        assert!(
            page.contains(
                "in flight  42s \u{b7} 1 model call \u{b7} 1 tool call \u{b7} not yet priced"
            ),
            "{page}"
        );
        // Without a known start, the elapsed the UI measured stands in.
        let mut l2 = Ledger::default();
        l2.called(0);
        let page = report(
            &l2,
            &Now {
                working_ms: Some(65_000),
                ..now(100)
            },
        );
        assert!(
            page.contains("in flight  1m 05s \u{b7} 1 model call"),
            "{page}"
        );
    }

    #[test]
    fn a_narrow_page_sheds_columns_rather_than_wrapping() {
        let l = session();
        for width in [40usize, 56, 68, 80, 120] {
            let page = report(&l, &now(width));
            for line in page
                .lines()
                .filter(|l| l.starts_with("  ") && !l.starts_with("  \u{b7}"))
            {
                assert!(
                    line.chars().count() <= width,
                    "at {width}: {line:?} is {} wide",
                    line.chars().count()
                );
            }
        }
        // The narrow one keeps what matters and drops the rest.
        let page = report(&l, &now(40));
        assert!(page.contains("cost"), "{page}");
        assert!(page.contains("ended"), "{page}");
        assert!(!page.contains("time"), "{page}");
        let page = report(&l, &now(120));
        assert!(page.contains("time"), "{page}");
        assert!(page.contains("1m 00s"), "{page}");
    }

    #[test]
    fn a_table_cuts_a_wide_label_when_nothing_else_will_go() {
        let cols = [Col::left("name", 0), Col::right("n", 0)];
        let rows = vec![vec![
            "a-very-long-tool-name-indeed".to_string(),
            "12".to_string(),
        ]];
        let lines = Table::new(&cols, rows).render(16);
        assert_eq!(lines[1], "a-very-long…  12");
        assert!(lines.iter().all(|l| l.chars().count() <= 16), "{lines:?}");
        // A break row is drawn across the table as it is.
        let rows = vec![
            vec!["── heading".to_string()],
            vec!["x".to_string(), "1".to_string()],
        ];
        let lines = Table::new(&cols, rows).render(40);
        assert_eq!(lines[1], "── heading");
    }

    #[test]
    fn words_for_time_and_share() {
        assert_eq!(duration(0), "0s");
        assert_eq!(duration(42_000), "42s");
        assert_eq!(duration(72_000), "1m 12s");
        assert_eq!(duration(3_780_000), "1h 03m");
        assert_eq!(duration(100 * 3_600_000), "4d 4h");
        assert_eq!(pct(0, 0), "0%");
        assert_eq!(pct(1, 3), "33%");
        assert_eq!(bar(0.71, 20), "\u{2588}".repeat(14) + &"\u{2591}".repeat(6));
        assert_eq!(bar(2.0, 4), "\u{2588}\u{2588}\u{2588}\u{2588}");
        assert_eq!(ended(StopReason::EndTurn), "ok");
        assert!(ended(StopReason::MaxTokens).starts_with('\u{26a0}'));
    }

    #[test]
    fn the_gate_is_tallied_by_outcome() {
        let mut l = Ledger::default();
        for o in [
            PolicyOutcome::Approved,
            PolicyOutcome::Approved,
            PolicyOutcome::Declined,
            PolicyOutcome::Judged,
            PolicyOutcome::Yolo,
            PolicyOutcome::Refused,
        ] {
            l.verdict(&o);
        }
        assert_eq!(
            l.verdicts,
            Verdicts {
                approved: 2,
                declined: 1,
                judged: 1,
                yolo: 1,
                refused: 1
            }
        );
        l.cancelled();
        l.compacted();
        l.settled("m", u(1, 0, 0, 1), StopReason::EndTurn, Some(0.0), None, 0);
        let page = report(&l, &now(100));
        assert!(
            page.contains("1 settled \u{b7} interrupted 1 time \u{b7} 1 compaction"),
            "{page}"
        );
        assert!(page.contains("gate       6 questions \u{b7} 2 approved \u{b7} 1 declined \u{b7} 1 judged \u{b7} 1 yolo \u{b7} 1 refused"), "{page}");
        assert_eq!(
            l.turns[0].context, None,
            "a compaction without a following context record is unknown, not empty"
        );
    }
}
