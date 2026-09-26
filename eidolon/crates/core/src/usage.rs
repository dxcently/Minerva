//! Token accounting: what a turn actually cost, and how large the context
//! actually is.
//!
//! [`crate::message::Usage`] splits input across three fields, because the
//! endpoint bills them at three different rates:
//!
//! - `input_tokens` — read fresh, at full price;
//! - `cache_creation_input_tokens` — read and written to the cache;
//! - `cache_read_input_tokens` — served from the cache, at a large discount.
//!
//! They are three prices for one body of text, not three bodies of text.
//! Reporting `input_tokens` alone is therefore not a cheaper measure of the
//! same thing — it is a different thing, and on a cache-warm backend it is
//! off by orders of magnitude. A Claude CLI turn that read 205k tokens of
//! context reports `input_tokens: 12`, because the other 205,571 were
//! cached; printing "in=12" invites the reader to conclude the turn was
//! nearly free and that the context has room to spare, and both are false.
//!
//! So: [`UsageExt::total_input`] is the number to show a person, and the
//! cached share is detail shown beside it, never instead of it.
//!
//! ## Cumulative usage is not context size
//!
//! Summing `total_input` across a session's turns gives what was billed,
//! and it grows without bound because every turn resends the history. The
//! *context* is the input of the single most recent turn
//! ([`crate::session::Session::last_input_tokens`]). The two diverge fast —
//! ten turns of a 50k context bill 500k input — so a consumer that shows one
//! number must say which one it is.
//!
//! ## And what the turn's ending meant
//!
//! [`stop_note`] lives here for the same reason: it is the other half of
//! what a consumer says when a turn settles, and saying it in one place is
//! what stops the TUI and `chat` from wording it two ways.

use serde::{Deserialize, Serialize};

use crate::message::{StopReason, Usage};

pub trait UsageExt {
    /// Every token the endpoint read to produce the response, whatever it
    /// charged for them. This is the input figure to report.
    fn total_input(&self) -> u64;

    /// Input plus output.
    fn total(&self) -> u64;

    /// The share of input served from cache, for the detail beside the
    /// total. `cache_creation` is deliberately excluded: those tokens were
    /// read at full price this turn and only pay off later.
    fn cached_input(&self) -> u64;
}

impl UsageExt for Usage {
    fn total_input(&self) -> u64 {
        self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }

    fn total(&self) -> u64 {
        self.total_input() + self.output_tokens
    }

    fn cached_input(&self) -> u64 {
        self.cache_read_input_tokens
    }
}

/// A token count at a glance: `812`, `12.4k`, `1.31M`.
///
/// Exact digits past a few thousand tokens are noise — nobody acts on the
/// difference between 187,097 and 187,100 — and the compact form makes the
/// comparison that does matter (context against its limit) legible in a
/// status line.
pub fn human(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.2}M", n as f64 / 1_000_000.0),
    }
}

/// A dollar figure at a glance: `$0.0042` while a session is in the
/// fractions of a cent a cheap model charges, `$0.033` under a dollar,
/// `$1.23` once there are dollars to show. Two decimals everywhere would
/// print a DeepSeek turn as `$0.00`, which is a claim and a false one —
/// and would print every turn of a three-cent session as `$0.03`, which
/// is true of each and says nothing about which of them cost the most.
pub fn dollars(usd: f64) -> String {
    if usd < 0.01 {
        format!("${usd:.4}")
    } else if usd < 1.0 {
        format!("${usd:.3}")
    } else {
        format!("${usd:.2}")
    }
}

/// `"142.3k/200.0k (71%)"` when both are known, `"142.3k"` with no
/// window, and `"\u{2014}"` for a branch that has never recorded a context size.
///
/// Unknown is its own answer and not zero. A log written before
/// [`crate::session::RecordKind::ContextSize`] existed carries no such
/// record, and the figure cannot be recovered from what it does carry —
/// `TurnSettled`'s usage is a turn total, which is what made this gauge
/// read 1292% of a million-token window. Drawing a dash says the harness
/// does not know; drawing `0%` would be a claim, and a false one.
pub fn context_line(tokens: Option<u64>, limit: Option<u64>) -> String {
    let limit = limit.filter(|l| *l > 0);
    match (tokens, limit) {
        (Some(t), Some(l)) => format!(
            "{}/{} ({}%)",
            human(t),
            human(l),
            (t as f64 / l as f64 * 100.0).round() as u64
        ),
        (Some(t), None) => human(t),
        (None, Some(l)) => format!("\u{2014}/{}", human(l)),
        (None, None) => "\u{2014}".into(),
    }
}

/// What to tell the operator about how a turn ended, or `None` when it
/// ended the way a turn is meant to.
///
/// A truncated response looks exactly like a finished one: the text simply
/// stops, mid-word when the cap fell mid-word, and the prompt comes back.
/// The stop reason is the only thing that tells them apart, so a consumer
/// that drops it — as every one of ours did — leaves the operator to
/// conclude the model had nothing more to say. The same rule as the folded
/// tool run: a failure may not leave the screen quietly.
///
/// `Cancelled` says nothing here because the operator is the one who did
/// it and the consumers already report it.
pub fn stop_note(stop: StopReason) -> Option<&'static str> {
    match stop {
        StopReason::EndTurn | StopReason::ToolUse | StopReason::Cancelled => None,
        StopReason::MaxTokens => Some(
            "response cut off: the model reached its output cap — raise `max_tokens` in the config, or ask for less at once",
        ),
        StopReason::Refusal => Some("response stopped: the model declined to continue"),
        StopReason::StopSequence => Some("response stopped at a stop sequence"),
        StopReason::Other => Some(
            "response ended without a stop reason: the backend closed the stream without saying why",
        ),
        // Not a failure: the session asked to be woken by a condition and
        // the loop settled the turn so nothing would poll for it. Worth
        // saying out loud all the same — a settle that is not the model
        // finishing looks exactly like one that is.
        StopReason::Waiting => Some(
            "waiting: the session parked on a condition it named and will wake when it fires, or when its deadline arrives",
        ),
        // Same reason `Waiting` says itself out loud: a settle that is not
        // the model finishing looks exactly like one that is. The operator
        // closed the question; the turn ended rather than re-asking.
        StopReason::Dismissed => Some(
            "turn ended: the question was dismissed without an answer — send a message to continue",
        ),
    }
}

/// One model call's wall clock, taken by the shared stream consumer around
/// the request.
///
/// A re-export: the consumer that takes the measurement is
/// [`harnox::llm::stream_with_retries`], because both this crate's agent loop
/// and a one-shot completion need the same number and it must not have two
/// definitions — see that module for what the two fields mean and why the
/// client's clock is the only one available.
pub use harnox::llm::CallTiming;

/// A turn's model calls, summed: what a consumer draws when a turn settles.
///
/// Journaled as it is measured — [`crate::session::RecordKind::TurnPace`],
/// written just before the settle it belongs to — because a pace is a fact
/// about the turn that no later reading can recover: the settle's timestamps
/// bracket the whole turn, tool calls and approvals included, and the
/// provider reports nothing on the wires the harness speaks. A turn the
/// harness never timed (a driver that owns its own loop, a cancel, a log
/// written before the record existed) carries no record and reads as
/// unmeasured rather than as zero.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, bitcode::Encode, bitcode::Decode,
)]
pub struct TurnTiming {
    pub calls: u32,
    /// The *first* call's wait for its first token — the figure a person
    /// reads as "how long before it started answering", so it is not
    /// summed across calls the way the rest of this is.
    pub first_token_ms: Option<u64>,
    /// Time the calls spent producing output: each call's total less its
    /// own wait for the first token. This is the denominator of the rate.
    pub decode_ms: u64,
    pub total_ms: u64,
}

impl TurnTiming {
    pub fn add(&mut self, one: CallTiming) {
        self.calls += 1;
        self.total_ms += one.total_ms;
        self.first_token_ms = self.first_token_ms.or(one.first_token_ms);
        // A call that never produced a token contributes no decode time
        // rather than all of its own: charging a 30-second silence to
        // decode would make the rate a lie in the other direction.
        self.decode_ms += one
            .total_ms
            .saturating_sub(one.first_token_ms.unwrap_or(one.total_ms));
    }

    /// Output tokens per second of *decode*, or `None` when either half is
    /// unknown. Derived, not reported: no endpoint here says what its
    /// prefill cost, so the wait for the first token is subtracted instead
    /// and the remainder is what the tokens came out of.
    pub fn tok_per_sec(&self, output_tokens: u64) -> Option<f64> {
        (self.decode_ms > 0 && output_tokens > 0)
            .then(|| output_tokens as f64 * 1000.0 / self.decode_ms as f64)
    }
}

/// What the turn's own clock says: `"16.0 tok/s · first token 0.31s · 1.4s
/// over 2 calls"`, minus whichever parts were not measured.
///
/// Beside the token footer rather than instead of it, for the reason the
/// cache share is: `in`/`out` are what the turn cost and this is how fast it
/// went, and a pace alone invites the same misreading a fresh-token count
/// does. `None` when no call was timed — a driver that owns its own loop, a
/// turn that never reached a model, or a log written before the pace was
/// journaled — and no consumer prints a zero in its place.
pub fn pace_note(t: &TurnTiming, output_tokens: u64) -> Option<String> {
    if t.calls == 0 || t.total_ms == 0 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(rate) = t.tok_per_sec(output_tokens) {
        parts.push(format!("{rate:.1} tok/s"));
    }
    if let Some(first) = t.first_token_ms {
        parts.push(format!("first token {}", millis(first)));
    }
    parts.push(format!(
        "{} over {} call{}",
        millis(t.total_ms),
        t.calls,
        if t.calls == 1 { "" } else { "s" }
    ));
    Some(parts.join(" \u{b7} "))
}

/// A duration at the scale one call lives at: `840.0ms` is not what anyone
/// says, so the sub-10-second band keeps one decimal of seconds and the
/// bands above it are the ones [`crate::usage::human`] would not fit.
fn millis(ms: u64) -> String {
    let s = ms / 1000;
    match ms {
        0..=9_999 => format!("{:.1}s", ms as f64 / 1000.0),
        _ if s < 60 => format!("{s}s"),
        _ if s < 3_600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3_600, (s % 3_600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Claude CLI turn that motivated this: 12 fresh tokens, ~205k read.
    #[test]
    fn a_cache_warm_turn_reports_what_it_actually_read() {
        let u = Usage {
            input_tokens: 12,
            output_tokens: 3282,
            cache_creation_input_tokens: 18_474,
            cache_read_input_tokens: 187_097,
        };
        assert_eq!(u.total_input(), 205_583);
        assert_eq!(u.cached_input(), 187_097);
        assert_eq!(u.total(), 208_865);
        // The old display claimed 12.
        assert_ne!(u.total_input(), u.input_tokens);
    }

    #[test]
    fn an_uncached_turn_is_unchanged_by_the_new_arithmetic() {
        let u = Usage {
            input_tokens: 3876,
            output_tokens: 68,
            ..Default::default()
        };
        assert_eq!(u.total_input(), 3876);
        assert_eq!(u.cached_input(), 0);
    }

    #[test]
    fn counts_are_rendered_compactly() {
        assert_eq!(human(0), "0");
        assert_eq!(human(812), "812");
        assert_eq!(human(9_999), "9999");
        assert_eq!(human(12_400), "12.4k");
        assert_eq!(human(205_583), "205.6k");
        assert_eq!(human(1_310_000), "1.31M");
    }

    #[test]
    fn a_cheap_turn_is_not_rounded_to_nothing() {
        assert_eq!(dollars(0.0042), "$0.0042");
        assert_eq!(dollars(0.009_99), "$0.0100");
        assert_eq!(dollars(0.0326), "$0.033");
        assert_eq!(dollars(0.312), "$0.312");
        assert_eq!(dollars(1.234), "$1.23");
        assert_eq!(dollars(12.0), "$12.00");
    }

    #[test]
    fn a_known_window_becomes_a_percentage() {
        assert_eq!(
            context_line(Some(142_300), Some(200_000)),
            "142.3k/200.0k (71%)"
        );
        assert_eq!(context_line(Some(142_300), None), "142.3k");
        // A zero limit is missing information, not a division by zero.
        assert_eq!(context_line(Some(142_300), Some(0)), "142.3k");
    }

    /// A context nobody recorded draws a dash, not a zero.
    ///
    /// The distinction is the whole reason the argument is an `Option`: a
    /// log written before `RecordKind::ContextSize` has no context size in
    /// it, and `0%` of a window is a claim about a session rather than an
    /// admission about the log. Zero cannot stand in for unknown either,
    /// because zero is exactly what a freshly compacted branch has.
    #[test]
    fn a_context_nobody_recorded_says_so_rather_than_claiming_zero() {
        assert_eq!(context_line(None, Some(1_000_000)), "\u{2014}/1.00M");
        assert_eq!(context_line(None, None), "\u{2014}");
        assert_eq!(
            context_line(Some(0), Some(1_000_000)),
            "0/1.00M (0%)",
            "a recorded zero is still a zero, not unknown"
        );
    }

    /// The 346-token turn that motivated this: the gateway said `length`,
    /// every consumer destructured the reason away, and the operator saw a
    /// sentence end in the middle of a path.
    #[test]
    fn a_turn_that_did_not_finish_says_so() {
        assert!(
            stop_note(StopReason::MaxTokens)
                .unwrap()
                .contains("cut off")
        );
        assert!(stop_note(StopReason::Other).is_some());
        assert!(stop_note(StopReason::Refusal).is_some());
        // The two healthy endings, and the one the operator already knows
        // about, stay quiet.
        assert_eq!(stop_note(StopReason::EndTurn), None);
        assert_eq!(stop_note(StopReason::ToolUse), None);
        assert_eq!(stop_note(StopReason::Cancelled), None);
    }

    #[test]
    fn summing_turns_is_billing_not_context() {
        // Three turns over a context that grows 10k -> 20k -> 30k bill 60k.
        let mut billed = Usage::default();
        for n in [10_000u64, 20_000, 30_000] {
            billed += Usage {
                input_tokens: n,
                output_tokens: 100,
                ..Default::default()
            };
        }
        // ...while the context never exceeded the last turn's 30k.
        assert_eq!(billed.total_input(), 60_000);
    }

    /// Two calls of a tool loop, timed as the client times them: 0.3s to
    /// first token and 1.3s total, then 0.2s and 2.2s. Decode is the two
    /// totals less their own first-token waits — 1.0s + 2.0s — and the rate
    /// is the turn's 300 output tokens over that, not over the 4.0s wall
    /// clock, which includes the wait a person already paid.
    #[test]
    fn a_turns_pace_is_decode_over_the_whole_turn() {
        let mut t = TurnTiming::default();
        t.add(CallTiming {
            first_token_ms: Some(300),
            total_ms: 1_300,
        });
        t.add(CallTiming {
            first_token_ms: Some(200),
            total_ms: 2_200,
        });
        assert_eq!(t.calls, 2);
        assert_eq!(t.total_ms, 3_500);
        // The first call's wait, not the second's and not their sum.
        assert_eq!(t.first_token_ms, Some(300));
        assert_eq!(t.decode_ms, 3_000);
        let rate = t.tok_per_sec(300).unwrap();
        assert!((rate - 100.0).abs() < 0.01, "{rate}");
        assert_eq!(
            pace_note(&t, 300).unwrap(),
            "100.0 tok/s · first token 0.3s · 3.5s over 2 calls"
        );
    }

    /// The honesty rules: nothing timed says nothing, and a call that never
    /// produced a token is not charged to decode as though it had.
    #[test]
    fn unmeasured_is_not_a_pace_of_zero() {
        assert_eq!(pace_note(&TurnTiming::default(), 100), None);
        // A call that produced nothing: no rate, and its silence stays out
        // of the decode denominator.
        let mut t = TurnTiming::default();
        t.add(CallTiming {
            first_token_ms: None,
            total_ms: 4_000,
        });
        assert_eq!(t.decode_ms, 0);
        assert_eq!(t.tok_per_sec(50), None);
        assert_eq!(pace_note(&t, 50).unwrap(), "4.0s over 1 call");
        // Tokens with no time is not a rate either.
        let mut t = TurnTiming::default();
        t.add(CallTiming {
            first_token_ms: Some(10),
            total_ms: 20,
        });
        assert_eq!(t.tok_per_sec(0), None);
        // Sub-second waits keep the scale they are read at.
        assert_eq!(millis(840), "0.8s");
        assert_eq!(millis(45_000), "45s");
        assert_eq!(millis(123_000), "2m 03s");
    }
}
