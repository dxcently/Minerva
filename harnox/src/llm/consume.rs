//! One streamed response, consumed once — the retry rules and the assembly
//! that a long agent loop and a one-shot completion must not each interpret.
//!
//! Two consumers want the same thing from a `Provider`: a message assembled
//! out of its events, the usage and stop reason that came with it, how long
//! it took, and a decision about whether a failure is worth another attempt.
//! The interactive harness's agent loop built that first and privately; a
//! consumer that needs a tool-less completion needs exactly it and none of
//! the loop, so it lives here rather than being written a second time —
//! a second interpreter is how the two drift, and the drift shows up as a
//! turn that behaves differently depending on which API asked for it.
//!
//! What is *policy* stays with the caller, through [`StreamSink`]: what a
//! consumer shows while the stream arrives, and how a tool block's arguments
//! are made safe to journal. What is *mechanics* is here: the event loop, the
//! accounting merge, first-token timing, and the retry rules below.
//!
//! ## The retry rules, which are the contract
//!
//! - **Nothing published is replayed.** An attempt that produced any content
//!   is never re-run: repeating it would put the same deltas on screen and in
//!   the log twice, and the log is the truth a resume reads.
//! - **Cancellation and authentication are not outages.** A cancelled token
//!   means the caller stopped asking. An auth failure means the credential is
//!   wrong, and asking again with the same wrong credential is a loop; the
//!   error reaches the caller on the first attempt.
//! - **One elapsed budget covers the retry sleeps**, evaluated at attempt
//!   boundaries and at the decision to sleep — never in the middle of an
//!   attempt. A budget that truncated a legitimate long generation would be a
//!   worse bug than the outages it guards against, so an attempt that is
//!   producing is always allowed to finish; a caller that needs a hard stop
//!   has cancellation.
//! - **Cancellation interrupts both the stream and the backoff.**

use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use tokio_util::sync::CancellationToken;

use super::message::{ContentBlock, Json, Message, StopReason, Usage};
use super::provider::{ChatRequest, Provider, StreamEvent};

/// How many times one model call may be attempted before it fails.
///
/// Four, not more: these retries exist for the endpoint that drops a
/// connection under load, and a call that has failed four times is a
/// different problem than a blip. [`RetryPolicy`] carries it as the default
/// so a caller that raises it says so where it is read.
pub const MAX_STREAM_ATTEMPTS: u32 = 4;

/// One call's timings, from the request going out.
///
/// The clock is the client's: the provider's own durations are not on any
/// wire these clients speak — Ollama reports them on its native `/api/chat`,
/// and every surface here does not — so this is measured here or not at all,
/// and it includes connecting and whatever queue the endpoint put the request
/// in. Where a person expects the native split (load, prefill, decode), this
/// is one number and a subtraction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallTiming {
    /// From the request going out to the first event the model produced.
    /// `None` when the call produced nothing — an empty response, or a
    /// stream that ended before any content.
    pub first_token_ms: Option<u64>,
    /// From the request going out to the provider's `Stop`.
    pub total_ms: u64,
}

/// How a failing call is retried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts for one call, including the first. At least one.
    pub attempts: u32,
    /// The wait before the second attempt; doubling after each further one.
    pub initial_delay: Duration,
    /// Elapsed time, from the first attempt, after which no further attempt
    /// starts — sleeps included. See the module docs: it is read at
    /// boundaries only and never truncates an attempt in flight.
    pub budget: Duration,
}

impl Default for RetryPolicy {
    /// Four attempts, half a second apart and doubling, inside two minutes.
    ///
    /// Four attempts means three sleeps — 0.5 s, 1 s, 2 s — so the sleeps
    /// alone total 3.5 s against a 120 s budget. That leaves the budget able
    /// to bite only where a single *attempt* itself ran long, which is
    /// exactly the case the boundary rule is written for: it stops the
    /// retries without ever cutting off an attempt that was still producing.
    fn default() -> Self {
        RetryPolicy {
            attempts: MAX_STREAM_ATTEMPTS,
            initial_delay: Duration::from_millis(500),
            budget: Duration::from_secs(120),
        }
    }
}

/// A stream that ended in an error, and whether the consumer had already seen
/// part of it. `produced` is what decides whether the call may be attempted
/// again; it is never inferred from the error's text.
#[derive(Debug)]
struct StreamFailure {
    error: anyhow::Error,
    produced: bool,
}

/// Whether an error is worth another attempt.
///
/// The provider seam is `anyhow`, by the crate's own convention, so there is
/// no status code to match on and this reads the rendered message. That is
/// coarse, and deliberately biased towards *not* retrying: a wrong "no"
/// costs one turn, a wrong "yes" repeats a request the endpoint already
/// refused on its merits (a bad key, a model that does not exist).
///
/// The 4xx guard is what keeps that bias honest for every wire in this crate,
/// because each renders a refusal as `… returned HTTP {status}: {body}`
/// (and the body comes along, truncated). Two seams of the list are worth
/// knowing about, and **both are deliberate rather than pending**. `"connect"`
/// is the broad one: it catches a genuine connection failure, and equally a
/// client-side configuration error saying `cannot connect to …`, which is
/// retried to the full attempts before the same error reaches the caller —
/// bounded, self-limiting, and the one near-miss that falls on the unsafe
/// side. Narrowing it to the phrases that usually appear (`"connection
/// refused"`, `"connection reset"`) was considered and rejected: the HTTP
/// client renders *every* connect-layer failure as `tcp connect error`,
/// including the DNS and resolver family, so a phrase list would lose
/// precisely the transient case these retries exist for — a bounded extra
/// attempt is the cheaper error than a lost turn. The terms are also
/// substring tests with no word boundaries, so `"eof"` would match those
/// three letters anywhere in a body that was echoed into the message; that
/// only matters for an error which is not 4xx and not otherwise transient,
/// which is the case retrying is for anyway. `"connection"` can never change
/// the answer while `"connect"` is on the list — it is kept so that narrowing
/// the broader term later does not silently lose the narrower one (the
/// connection-closed family, for instance, is matched through it).
pub fn is_transient(e: &anyhow::Error) -> bool {
    let m = format!("{e:#}").to_lowercase();
    if m.contains("http 4") && !m.contains("http 408") && !m.contains("http 429") {
        return false;
    }
    m.contains("http 5")
        || m.contains("http 408")
        || m.contains("http 429")
        || m.contains("timed out")
        || m.contains("timeout")
        || m.contains("connection")
        || m.contains("connect")
        || m.contains("sending request")
        || m.contains("broken pipe")
        || m.contains("reset by peer")
        || m.contains("eof")
}

/// Whether an error says the credential is wrong, the login is gone, or no
/// key was found at all — the class that must never be retried as an outage.
///
/// Belt and braces beside [`is_transient`], which already refuses every 4xx
/// but 408/429: a credential problem is often not an HTTP status at all (a
/// token file that is unreadable, a named secret nothing holds), and those
/// are the failures a retry loop would otherwise spin on while the operator
/// waits. A caller renders this and acts on it — re-authenticate, or fall
/// back to a backend that *is* configured — rather than waiting.
///
/// It is consulted *before* [`is_transient`] at the one call site that
/// retries, so where the two disagree this one wins. That is the safe
/// direction for the broad terms on the list (`credential`,
/// `authentication`, `forbidden`, `unauthorized`): a transient failure whose
/// body happens to mention one of them gets one attempt instead of four, so
/// the cost of a false positive here is a turn, never a repeated
/// authentication.
pub fn is_auth_failure(e: &anyhow::Error) -> bool {
    let m = format!("{e:#}").to_lowercase();
    m.contains("http 401")
        || m.contains("http 403")
        || m.contains("unauthorized")
        || m.contains("forbidden")
        || m.contains("invalid api key")
        || m.contains("invalid_api_key")
        || m.contains("authentication")
        || m.contains("not logged in")
        // The credential sentences this crate's own sources raise, so a
        // caller that renders this class renders them too: a named secret
        // nothing holds, and a refresh token the service has rotated out.
        || m.contains("no key for secret")
        || m.contains("no secret store is configured")
        || m.contains("invalid_grant")
        || m.contains("credential")
}

/// What a consumer does with a stream while it is arriving.
///
/// Every method but [`StreamSink::tool_input`] is a notification, so a
/// consumer that wants none of them implements nothing: the default is a
/// no-op for each, and a tool-less completion overrides none. The one that
/// returns a value is the one where the consumer owns an invariant — see
/// its doc.
pub trait StreamSink: Send {
    /// The request went out and the response is open.
    ///
    /// Fires once per *attempt*, retried attempts included — a consumer that
    /// shows a reply as it arrives starts a new one here, and an attempt that
    /// produced nothing is one it should start over rather than append to.
    fn message_start(&mut self) {}
    /// A fragment of the assistant's prose.
    fn text_delta(&mut self, _text: &str) {}
    /// A fragment of its thinking.
    fn thinking_delta(&mut self, _text: &str) {}
    /// A tool call has started.
    fn tool_use_start(&mut self, _id: &str, _name: &str) {}
    /// A fragment of the open tool call's JSON arguments; `id` is `None` in
    /// the malformed case where a fragment arrives with no call open.
    ///
    /// The `None` case is reported and is *not* content: a fragment that
    /// belongs to no block is assembled into nothing, so an attempt that
    /// produced only one is still retryable ([`Collected::produced`]). A
    /// consumer that draws `None` fragments can expect to draw them again on
    /// the next attempt — the alternative would be to end a session's call
    /// over a gateway emitting a fragment whose call it never announced.
    fn tool_input_delta(&mut self, _id: Option<&str>, _partial: &str) {}
    /// A transient failure, about to be slept off and attempted again.
    fn retrying(&mut self, _attempt: u32, _of: u32, _error: &anyhow::Error) {}

    /// The JSON text a closed tool block carries, given the name and whatever
    /// the stream accumulated.
    ///
    /// The default hands the stream's own bytes through. A consumer whose
    /// records are replayed onto the wire — the interactive harness, whose
    /// `tool_use` block is re-sent on every later request of the turn — must
    /// override this so a torn or gateway-mangled blob becomes something a
    /// strict endpoint will accept, because a block that cannot be replayed
    /// poisons the rest of the session and no retry or resume recovers it.
    fn tool_input(&self, _name: &str, raw: &str) -> String {
        raw.to_string()
    }
}

/// A sink that does nothing with the stream.
///
/// For a caller that wants the assembled message and nothing else — a
/// completion whose deltas are not shown as they arrive.
pub struct QuietSink;

impl StreamSink for QuietSink {}

/// One streamed response, assembled.
#[derive(Debug)]
pub struct Collected {
    /// The assistant message: text, thinking and tool blocks in the order the
    /// stream produced them.
    pub message: Message,
    /// `Cancelled` when the token fired locally; `Other` when no terminal
    /// event ever arrived (see [`Collected::terminal`]).
    pub stop_reason: StopReason,
    /// What the wire reported. The input side comes from `Stop` where the
    /// wire says it only there, from `Start` otherwise — see the merge in
    /// `consume`.
    pub usage: Usage,
    /// This attempt's own timings — the retry sleeps are not in it. A caller
    /// that wants the whole sequence times around [`stream_with_retries`].
    pub timing: CallTiming,
    /// A terminal `Stop` event arrived. `false` is a clean EOF with no stop
    /// at all: an incomplete stream, which is not the same ending as any
    /// stop reason and must not be reported as one.
    pub terminal: bool,
    /// Whether any content was assembled — the test for whether the call
    /// could have been retried without the consumer seeing it twice.
    pub produced: bool,
    /// Which attempt produced this: 1 when nothing was retried. The timings
    /// below are this attempt's alone, so a caller accounting for a call made
    /// across several attempts needs this to know that it did.
    pub attempts: u32,
}

/// Stream one completion, retrying transient endpoint failures.
///
/// The retry rules are the module docs; what matters at the call site is that
/// a failure which produced nothing is invisible when a later attempt
/// succeeds, and that nothing else is retried at all.
pub async fn stream_with_retries(
    provider: &dyn Provider,
    req: ChatRequest,
    cancel: &CancellationToken,
    policy: &RetryPolicy,
    sink: &mut dyn StreamSink,
) -> anyhow::Result<Collected> {
    let started = Instant::now();
    let mut delay = policy.initial_delay;
    let attempts = policy.attempts.max(1);
    for attempt in 1..=attempts {
        match consume(provider, req.clone(), cancel, sink).await {
            Ok(mut c) => {
                c.attempts = attempt;
                return Ok(c);
            }
            Err(f) => {
                let last = attempt == attempts;
                if last
                    || f.produced
                    || cancel.is_cancelled()
                    || is_auth_failure(&f.error)
                    || !is_transient(&f.error)
                {
                    return Err(f.error);
                }
                // Read here and at the sleep, nowhere else: the budget decides
                // whether another attempt *starts*, never how long one runs.
                if started.elapsed() + delay > policy.budget {
                    tracing::warn!(
                        attempt,
                        budget_s = policy.budget.as_secs_f64(),
                        "not retrying: the retry budget is spent"
                    );
                    return Err(f.error);
                }
                tracing::warn!(
                    attempt,
                    of = attempts,
                    error = %format!("{:#}", f.error),
                    "transient stream failure; retrying"
                );
                sink.retrying(attempt, attempts, &f.error);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = cancel.cancelled() => return Err(f.error),
                }
                delay *= 2;
            }
        }
    }
    unreachable!("the loop returns on its last attempt")
}

/// Consume exactly one stream attempt into a message.
///
/// A failure reports whether anything had already been assembled, which is
/// what [`stream_with_retries`] decides on.
async fn consume(
    provider: &dyn Provider,
    req: ChatRequest,
    cancel: &CancellationToken,
    sink: &mut dyn StreamSink,
) -> Result<Collected, StreamFailure> {
    // The clock starts before the request goes out, so whatever the endpoint
    // spends connecting and queueing is inside the figure — it is the
    // operator's wait, which is the thing being asked about.
    let started = Instant::now();
    let mut stream = provider.stream(req, cancel.child_token());
    let mut asm = Assembler::default();
    let mut usage = Usage::default();
    let mut stop = StopReason::Other;
    let mut terminal = false;
    let mut first_token_ms: Option<u64> = None;
    sink.message_start();

    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let produced = asm.produced();
                return Ok(Collected {
                    message: asm.finish(&*sink),
                    stop_reason: StopReason::Cancelled,
                    usage,
                    timing: timing_since(started, first_token_ms),
                    terminal,
                    produced,
                    attempts: 1,
                });
            }
            n = stream.next() => n,
        };
        let Some(ev) = next else {
            // A clean EOF. Whether that is an ending is the record's job:
            // `terminal` says no `Stop` ever came, and a caller that treats
            // that as a normal stop would render a truncated answer as whole.
            break;
        };
        let ev = match ev {
            Ok(ev) => ev,
            Err(error) => {
                return Err(StreamFailure {
                    error,
                    produced: asm.produced(),
                });
            }
        };
        // The first thing the model produced, whatever kind it is: thinking
        // and a tool call are as much of an answer as prose is, and a call
        // that only thinks is one the operator is still waiting on. `Start`
        // carries usage and no content, and a `BlockStop` that arrives first
        // means the call said nothing.
        if first_token_ms.is_none()
            && matches!(
                ev,
                StreamEvent::TextDelta(_)
                    | StreamEvent::ThinkingDelta(_)
                    | StreamEvent::ThinkingSignature(_)
                    | StreamEvent::RedactedThinking(_)
                    | StreamEvent::ToolUseStart { .. }
                    | StreamEvent::ToolInputDelta(_)
            )
        {
            first_token_ms = Some(started.elapsed().as_millis() as u64);
        }
        match ev {
            StreamEvent::Start { usage: u } => usage += u,
            StreamEvent::TextDelta(t) => {
                sink.text_delta(&t);
                asm.text(t);
            }
            StreamEvent::ThinkingDelta(t) => {
                sink.thinking_delta(&t);
                asm.thinking(t);
            }
            StreamEvent::ThinkingSignature(s) => asm.signature(s),
            StreamEvent::RedactedThinking(d) => asm.redacted(d),
            StreamEvent::ToolUseStart { id, name } => {
                sink.tool_use_start(&id, &name);
                asm.tool_start(id, name);
            }
            StreamEvent::ToolInputDelta(j) => {
                match asm.current_tool_id() {
                    Some(id) => sink.tool_input_delta(Some(id), &j),
                    None => sink.tool_input_delta(None, &j),
                }
                asm.tool_input(j);
            }
            StreamEvent::BlockStop => asm.block_stop(),
            StreamEvent::Stop {
                stop_reason,
                usage: u,
            } => {
                // Output tokens arrive here. The input side — fresh,
                // cache-read, cache-written — arrived at `Start` on the
                // Anthropic wire and arrives *only here* on the
                // chat-completions one, whose one usage object is the final
                // chunk. So every input-side counter this event carries
                // replaces what `Start` said, and one it does not carry
                // leaves `Start`'s word standing. Taking the fresh count
                // alone here was what made every cache hit on that wire
                // vanish: the context read as the fresh increment, the settle
                // summed the increments, and the price never reached the
                // cache-read rate.
                usage.output_tokens += u.output_tokens;
                if u.input_tokens > 0 {
                    usage.input_tokens = u.input_tokens;
                }
                if u.cache_read_input_tokens > 0 {
                    usage.cache_read_input_tokens = u.cache_read_input_tokens;
                }
                if u.cache_creation_input_tokens > 0 {
                    usage.cache_creation_input_tokens = u.cache_creation_input_tokens;
                }
                stop = stop_reason;
                terminal = true;
            }
        }
    }

    let produced = asm.produced();
    Ok(Collected {
        message: asm.finish(&*sink),
        stop_reason: stop,
        usage,
        timing: timing_since(started, first_token_ms),
        terminal,
        produced,
        attempts: 1,
    })
}

/// One call's timings, as of now.
fn timing_since(started: Instant, first_token_ms: Option<u64>) -> CallTiming {
    CallTiming {
        first_token_ms,
        total_ms: started.elapsed().as_millis() as u64,
    }
}

/// Accumulates stream deltas into content blocks. One block is "open" at a
/// time, matching the wire's `content_block_start` … `content_block_stop`
/// bracket.
#[derive(Default)]
struct Assembler {
    done: Vec<ContentBlock>,
    open: Option<Open>,
}

enum Open {
    Text(String),
    Thinking {
        thinking: String,
        signature: String,
    },
    Tool {
        id: String,
        name: String,
        input: String,
    },
}

impl Assembler {
    fn text(&mut self, t: String) {
        match &mut self.open {
            Some(Open::Text(s)) => s.push_str(&t),
            _ => {
                self.block_stop();
                self.open = Some(Open::Text(t));
            }
        }
    }
    fn thinking(&mut self, t: String) {
        match &mut self.open {
            Some(Open::Thinking { thinking, .. }) => thinking.push_str(&t),
            _ => {
                self.block_stop();
                self.open = Some(Open::Thinking {
                    thinking: t,
                    signature: String::new(),
                });
            }
        }
    }
    fn signature(&mut self, s: String) {
        match &mut self.open {
            Some(Open::Thinking { signature, .. }) => signature.push_str(&s),
            _ => {
                self.block_stop();
                self.open = Some(Open::Thinking {
                    thinking: String::new(),
                    signature: s,
                });
            }
        }
    }
    fn redacted(&mut self, data: String) {
        self.block_stop();
        self.done.push(ContentBlock::RedactedThinking { data });
    }
    fn tool_start(&mut self, id: String, name: String) {
        self.block_stop();
        self.open = Some(Open::Tool {
            id,
            name,
            input: String::new(),
        });
    }
    fn tool_input(&mut self, j: String) {
        if let Some(Open::Tool { input, .. }) = &mut self.open {
            input.push_str(&j);
        }
    }
    /// Whether anything has been assembled yet — the test for whether a
    /// failed stream can be retried without the user seeing it twice.
    ///
    /// Not every notification a sink received counts: a tool-input fragment
    /// with no call open belongs to no block ([`StreamSink::tool_input_delta`]),
    /// so it is not here.
    fn produced(&self) -> bool {
        !self.done.is_empty() || self.open.is_some()
    }

    fn current_tool_id(&self) -> Option<&str> {
        match &self.open {
            Some(Open::Tool { id, .. }) => Some(id),
            _ => None,
        }
    }
    fn block_stop(&mut self) {
        if let Some(open) = self.open.take() {
            self.done.push(match open {
                Open::Text(text) => ContentBlock::Text { text },
                Open::Thinking {
                    thinking,
                    signature,
                } => ContentBlock::Thinking {
                    thinking,
                    signature,
                },
                Open::Tool { id, name, input } => ContentBlock::ToolUse {
                    id,
                    name,
                    input: Json(input),
                },
            });
        }
    }
    /// Close the open block and settle the message.
    ///
    /// A `tool_use` block's arguments are shaped by the sink here — the one
    /// point where a consumer's replay invariant touches assembly — and are
    /// otherwise exactly what the stream accumulated. The name is left alone:
    /// mapping a streamed name onto one that actually exists is a question
    /// about a *registry*, which this layer does not have.
    fn finish(mut self, sink: &dyn StreamSink) -> Message {
        self.block_stop();
        let blocks = self
            .done
            .into_iter()
            .map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => ContentBlock::ToolUse {
                    id,
                    input: Json(sink.tool_input(&name, &input.0)),
                    name,
                },
                other => other,
            })
            .collect();
        Message::assistant(blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::super::provider::EventStream;

    /// One step of a scripted attempt. Clone-able, because a script that runs
    /// out replays its last attempt rather than inventing an empty one.
    #[derive(Clone)]
    enum Step {
        Ev(StreamEvent),
        Fail(&'static str),
    }

    /// A provider that plays back scripted streams, one per attempt, and
    /// counts how many attempts it was asked for.
    struct Scripted {
        responses: Mutex<VecDeque<Vec<Step>>>,
        calls: AtomicU32,
    }

    impl Scripted {
        /// `responses` is one entry per attempt, in order; an attempt after
        /// the last entry replays the last entry.
        fn new(responses: Vec<Vec<Step>>) -> Arc<Self> {
            Arc::new(Scripted {
                responses: Mutex::new(responses.into()),
                calls: AtomicU32::new(0),
            })
        }
        fn calls(&self) -> u32 {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Provider for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }
        fn stream<'a>(&'a self, _req: ChatRequest, _cancel: CancellationToken) -> EventStream<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut q = self.responses.lock().unwrap();
            let events = if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                q.front().cloned().unwrap_or_default()
            };
            Box::pin(futures_util::stream::iter(events.into_iter().map(|s| match s {
                Step::Ev(ev) => Ok(ev),
                Step::Fail(why) => Err(anyhow::anyhow!("{why}")),
            })))
        }
    }

    fn text_stream(text: &str) -> Vec<Step> {
        vec![
            Step::Ev(StreamEvent::Start {
                usage: Usage {
                    input_tokens: 10,
                    ..Default::default()
                },
            }),
            Step::Ev(StreamEvent::TextDelta(text.into())),
            Step::Ev(StreamEvent::BlockStop),
            Step::Ev(StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    output_tokens: 1,
                    ..Default::default()
                },
            }),
        ]
    }

    fn req() -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            max_tokens: 16,
            ..Default::default()
        }
    }

    /// A sink that records the deltas it was told about, so the test can see
    /// what a consumer would have shown.
    #[derive(Default)]
    struct Recorder {
        text: String,
        retries: Vec<u32>,
    }

    impl StreamSink for Recorder {
        fn text_delta(&mut self, text: &str) {
            self.text.push_str(text);
        }
        fn retrying(&mut self, attempt: u32, _of: u32, _error: &anyhow::Error) {
            self.retries.push(attempt);
        }
        fn tool_input(&self, _name: &str, _raw: &str) -> String {
            "{}".into()
        }
    }

    async fn run(
        p: &Arc<Scripted>,
        policy: RetryPolicy,
        sink: &mut Recorder,
    ) -> anyhow::Result<Collected> {
        stream_with_retries(&**p, req(), &CancellationToken::new(), &policy, sink).await
    }

    /// Which failures are worth another attempt, and which are the endpoint
    /// refusing on the merits. Moved here with the function, so the harness
    /// that used to assert this privately is not asserting a copy.
    #[test]
    fn transient_failures_are_told_from_permanent_ones() {
        assert!(is_transient(&anyhow::anyhow!(
            "fau returned HTTP 500 Internal Server Error"
        )));
        assert!(is_transient(&anyhow::anyhow!(
            "fau returned HTTP 429 Too Many Requests"
        )));
        assert!(is_transient(&anyhow::anyhow!("error sending request for url")));
        assert!(is_transient(&anyhow::anyhow!("connection reset by peer")));
        // A rejection on the merits must not be repeated.
        assert!(!is_transient(&anyhow::anyhow!(
            "fau returned HTTP 401 Unauthorized"
        )));
        assert!(!is_transient(&anyhow::anyhow!(
            "fau returned HTTP 404: model not found"
        )));
        // And the credential class is named even where no status ever said
        // so: a retry loop over a missing key is a loop the operator waits on.
        assert!(is_auth_failure(&anyhow::anyhow!(
            "provider `deepseek`: no key for secret `deepseek`"
        )));
        assert!(is_auth_failure(&anyhow::anyhow!(
            "gw returned HTTP 401 Unauthorized"
        )));
        assert!(!is_auth_failure(&anyhow::anyhow!(
            "gw returned HTTP 503 Service Unavailable"
        )));
        // This crate's own two credential sentences, so a caller rendering
        // the auth class renders them too rather than only the HTTP shape.
        assert!(is_auth_failure(&anyhow::anyhow!(
            "provider `p` needs secret `k` but no secret store is configured"
        )));
        assert!(is_auth_failure(&anyhow::anyhow!(
            "ChatGPT login expired (invalid_grant): run `eidolon chatgpt-login` again"
        )));
        // Where the two disagree, the auth verdict is the one that wins —
        // pinning which function a message that is both transient and
        // auth-shaped is classified by, since the retry loop consults it
        // first.
        let both = anyhow::anyhow!("gw returned HTTP 503 Service Unavailable: credential service restarting");
        assert!(is_transient(&both) && is_auth_failure(&both));
    }

    /// A response that said nothing is not "produced", so a failure after it
    /// is still retryable; one byte of content is not.
    #[tokio::test]
    async fn produced_is_the_line_between_retryable_and_not() {
        // A reply that said nothing: blocks opened and closed, no content.
        let p = Scripted::new(vec![vec![Step::Ev(StreamEvent::BlockStop)]]);
        let mut sink = Recorder::default();
        let c = run(&p, RetryPolicy::default(), &mut sink).await.unwrap();
        assert!(!c.produced, "an empty reply produced nothing");
        assert!(!c.terminal, "and arrived with no stop event");

        // The same empty attempt, failing: retryable, because nothing was
        // published that a second attempt would repeat.
        let p = Scripted::new(vec![
            vec![
                Step::Ev(StreamEvent::BlockStop),
                Step::Fail("connection reset by peer"),
            ],
            text_stream("ok"),
        ]);
        let mut sink = Recorder::default();
        let c = run(&p, RetryPolicy::default(), &mut sink).await.unwrap();
        assert_eq!(p.calls(), 2, "the empty attempt was retried");
        assert_eq!(c.message.text(), "ok");

        // One byte of content and the line is crossed: nothing is replayed.
        let p = Scripted::new(vec![vec![
            Step::Ev(StreamEvent::TextDelta("x".into())),
            Step::Fail("connection reset by peer"),
        ]]);
        let mut sink = Recorder::default();
        run(&p, RetryPolicy::default(), &mut sink).await.unwrap_err();
        assert_eq!(p.calls(), 1, "content already arrived; nothing is replayed");
    }

    /// A transient failure before anything arrived is invisible: the retry
    /// succeeds, the consumer saw one answer, and the sink was told.
    #[tokio::test]
    async fn a_transient_failure_before_any_output_is_retried() {
        let p = Scripted::new(vec![
            vec![Step::Fail("gw returned HTTP 503 Service Unavailable")],
            text_stream("hi"),
        ]);
        let mut sink = Recorder::default();
        let c = run(&p, RetryPolicy::default(), &mut sink).await.unwrap();
        assert_eq!(c.message.text(), "hi");
        assert_eq!(sink.text, "hi", "no delta was repeated");
        assert_eq!(sink.retries, [1], "the sink was told before the sleep");
        assert_eq!(p.calls(), 2);
        assert_eq!(c.attempts, 2, "the second attempt is the one that answered");
        assert!(c.terminal && c.produced);
        assert!(c.timing.first_token_ms.is_some(), "the wait for the first token was measured");
    }

    /// A failure *after* content arrived is never replayed: the deltas are
    /// already on screen and already in the log.
    #[tokio::test]
    async fn a_failure_after_output_is_not_replayed() {
        let p = Scripted::new(vec![vec![
            Step::Ev(StreamEvent::TextDelta("par".into())),
            Step::Fail("connection reset by peer"),
        ]]);
        let mut sink = Recorder::default();
        let e = run(&p, RetryPolicy::default(), &mut sink).await.unwrap_err();
        assert!(format!("{e:#}").contains("reset by peer"), "{e:#}");
        assert_eq!(p.calls(), 1, "one attempt, and the failure reached the caller");
        assert_eq!(sink.text, "par");
        assert!(sink.retries.is_empty());
    }

    /// A message that is both transient and auth-shaped is not retried: the
    /// auth verdict is consulted first, so a 5xx that mentions a credential
    /// costs one attempt rather than four. The safe direction — a false
    /// positive loses a turn, never repeats an authentication.
    #[tokio::test]
    async fn an_auth_shaped_failure_is_not_retried_even_when_it_looks_transient() {
        let p = Scripted::new(vec![vec![Step::Fail(
            "gw returned HTTP 503 Service Unavailable: credential service restarting",
        )]]);
        let mut sink = Recorder::default();
        run(&p, RetryPolicy::default(), &mut sink).await.unwrap_err();
        assert_eq!(p.calls(), 1);
        assert!(sink.retries.is_empty());
    }

    /// Cancelling and a rejection on the merits are not outages.
    #[tokio::test]
    async fn auth_and_non_transient_failures_are_never_retried() {
        for message in [
            "gw returned HTTP 401 Unauthorized",
            "provider `deepseek`: no key for secret `deepseek`",
            "credential file /tmp/x is empty",
            "gw returned HTTP 404: model not found",
        ] {
            let p = Scripted::new(vec![vec![Step::Fail(message)]]);
            let mut sink = Recorder::default();
            let e = run(&p, RetryPolicy::default(), &mut sink).await.unwrap_err();
            assert!(format!("{e:#}").contains(message), "{e:#}");
            assert_eq!(p.calls(), 1, "`{message}` is not an outage");
            assert!(sink.retries.is_empty());
        }
    }

    /// Cancelling during the backoff ends the call at once, with the failure
    /// the attempt had already produced.
    #[tokio::test]
    async fn cancellation_interrupts_the_backoff() {
        let p = Scripted::new(vec![vec![Step::Fail("gw returned HTTP 502 Bad Gateway")]]);
        let cancel = CancellationToken::new();
        let policy = RetryPolicy {
            initial_delay: Duration::from_secs(30),
            ..RetryPolicy::default()
        };
        let token = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token.cancel();
        });
        let mut sink = Recorder::default();
        let started = Instant::now();
        let e = stream_with_retries(&*p, req(), &cancel, &policy, &mut sink)
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("502"), "{e:#}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the 30s sleep was interrupted: {:?}",
            started.elapsed()
        );
        assert_eq!(p.calls(), 1);
    }

    /// A failure that arrives with the token already fired is not retried,
    /// and no backoff is slept: the caller stopped asking, and another
    /// attempt would be one they did not ask for. (The other half of the
    /// cancellation rule — a token that fires *during* the backoff — is
    /// `cancellation_interrupts_the_backoff`.)
    #[tokio::test]
    async fn a_failure_that_arrives_after_the_token_fired_is_not_retried() {
        /// Cancels the caller's token from inside the poll that produces the
        /// failure, so the failure is the one that reaches the retry
        /// decision with the token already fired — deterministic, rather
        /// than a race between a spawned canceller and the loop.
        struct CancelsThenFails(CancellationToken);

        impl Provider for CancelsThenFails {
            fn name(&self) -> &str {
                "cancels-then-fails"
            }
            fn stream<'a>(
                &'a self,
                _req: ChatRequest,
                _cancel: CancellationToken,
            ) -> EventStream<'a> {
                let token = self.0.clone();
                Box::pin(futures_util::stream::unfold(false, move |done| {
                    let token = token.clone();
                    async move {
                        if done {
                            return None;
                        }
                        token.cancel();
                        Some((
                            Err(anyhow::anyhow!("gw returned HTTP 503 Service Unavailable")),
                            true,
                        ))
                    }
                }))
            }
        }

        let cancel = CancellationToken::new();
        let provider = CancelsThenFails(cancel.clone());
        let mut sink = Recorder::default();
        let started = Instant::now();
        let e = stream_with_retries(
            &provider,
            req(),
            &cancel,
            &RetryPolicy::default(),
            &mut sink,
        )
        .await
        .unwrap_err();
        assert!(format!("{e:#}").contains("503"), "{e:#}");
        assert!(sink.retries.is_empty(), "no retry is announced after the caller stopped");
        // Bounded by the *first delay*, which is what "no backoff was slept"
        // means here: a sleep that had run could only take the call past it,
        // while a generous bound keeps a stalled runner from failing a test
        // whose property `retries.is_empty()` already pins.
        assert!(
            started.elapsed() < RetryPolicy::default().initial_delay,
            "and no backoff was slept: {:?}",
            started.elapsed()
        );
    }

    /// The budget is read at boundaries and sleep decisions only. A spent
    /// budget stops the retries; an attempt that is still producing is never
    /// cut off by it.
    #[tokio::test]
    async fn the_budget_stops_retries_but_never_truncates_an_attempt() {
        let p = Scripted::new(vec![vec![Step::Fail("gw returned HTTP 500 Internal Server Error")]]);
        let mut sink = Recorder::default();
        let policy = RetryPolicy {
            budget: Duration::from_millis(10),
            ..RetryPolicy::default()
        };
        run(&p, policy, &mut sink).await.unwrap_err();
        assert_eq!(p.calls(), 1, "no attempt was started after the budget was spent");
        assert!(sink.retries.is_empty());

        // A producing attempt that outlives the budget still finishes: the
        // budget has nothing to say about an attempt already in flight.
        struct Slow;
        impl Provider for Slow {
            fn name(&self) -> &str {
                "slow"
            }
            fn stream<'a>(&'a self, _req: ChatRequest, _cancel: CancellationToken) -> EventStream<'a> {
                Box::pin(futures_util::stream::unfold(0u32, |n| async move {
                    match n {
                        0 => Some((Ok(StreamEvent::Start { usage: Usage::default() }), 1)),
                        1 => {
                            tokio::time::sleep(Duration::from_millis(60)).await;
                            Some((Ok(StreamEvent::TextDelta("late".into())), 2))
                        }
                        2 => Some((
                            Ok(StreamEvent::Stop {
                                stop_reason: StopReason::EndTurn,
                                usage: Usage::default(),
                            }),
                            3,
                        )),
                        _ => None,
                    }
                }))
            }
        }
        let mut sink = Recorder::default();
        let policy = RetryPolicy {
            budget: Duration::from_millis(1),
            ..RetryPolicy::default()
        };
        let c = stream_with_retries(&Slow, req(), &CancellationToken::new(), &policy, &mut sink)
            .await
            .expect("an attempt in flight is never cut off by the retry budget");
        assert_eq!(c.message.text(), "late");
        assert!(c.timing.total_ms >= 60, "{:?}", c.timing);
    }

    /// On the chat-completions wire the input counters arrive only in the
    /// final chunk; on the Anthropic wire they arrive at `Start`. A value the
    /// stop event carries replaces what start said, and one it does not
    /// leaves start's word standing — which is what keeps a cache hit
    /// readable as a cache hit rather than as a tiny fresh-input count.
    #[tokio::test]
    async fn the_terminal_usage_carries_the_input_side_where_the_wire_says_it_only_there() {
        let p = Scripted::new(vec![vec![
            Step::Ev(StreamEvent::Start {
                usage: Usage {
                    input_tokens: 10,
                    cache_read_input_tokens: 40,
                    ..Default::default()
                },
            }),
            Step::Ev(StreamEvent::TextDelta("hi".into())),
            Step::Ev(StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 555,
                    cache_read_input_tokens: 0,
                    output_tokens: 7,
                    ..Default::default()
                },
            }),
        ]]);
        let mut sink = Recorder::default();
        let c = run(&p, RetryPolicy::default(), &mut sink).await.unwrap();
        assert_eq!(c.usage.input_tokens, 555, "the fresh count came from Stop");
        assert_eq!(c.usage.cache_read_input_tokens, 40, "an uncarried counter kept Start's");
        assert_eq!(c.usage.output_tokens, 7);
        assert_eq!(c.stop_reason, StopReason::EndTurn);
    }

    /// A stream that ends without a terminal event is not an ending: the
    /// caller must be able to tell a truncated answer from a whole one.
    #[tokio::test]
    async fn an_eof_without_a_stop_is_not_terminal() {
        let p = Scripted::new(vec![vec![Step::Ev(StreamEvent::TextDelta("half an ans".into()))]]);
        let mut sink = Recorder::default();
        let c = run(&p, RetryPolicy::default(), &mut sink).await.unwrap();
        assert!(!c.terminal, "no Stop event ever arrived");
        assert_eq!(c.stop_reason, StopReason::Other);
        assert!(c.produced, "there was something to show");
        assert_eq!(p.calls(), 1, "a clean EOF is not a failure and is not retried");
        assert!(sink.retries.is_empty());
    }

    /// A cancelled token ends the stream where it is, keeping whatever the
    /// attempt had measured and assembled.
    #[tokio::test]
    async fn cancellation_ends_the_stream_with_what_arrived() {
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        struct Slow;
        impl Provider for Slow {
            fn name(&self) -> &str {
                "slow"
            }
            fn stream<'a>(&'a self, _req: ChatRequest, _cancel: CancellationToken) -> EventStream<'a> {
                Box::pin(futures_util::stream::unfold(0u32, |n| async move {
                    match n {
                        0 => Some((Ok(StreamEvent::TextDelta("par".into())), 1)),
                        _ => {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                            None
                        }
                    }
                }))
            }
        }
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            token.cancel();
        });
        let mut sink = Recorder::default();
        let c = stream_with_retries(&Slow, req(), &cancel, &RetryPolicy::default(), &mut sink)
            .await
            .unwrap();
        assert_eq!(c.stop_reason, StopReason::Cancelled);
        assert_eq!(c.message.text(), "par");
        assert!(!c.terminal);
    }

    /// Tool blocks assemble in order, the sink shapes their arguments, and
    /// the name is passed through untouched — mapping a name onto a registry
    /// is the caller's business.
    #[tokio::test]
    async fn tool_blocks_are_assembled_and_the_sink_shapes_their_arguments() {
        let p = Scripted::new(vec![vec![
            Step::Ev(StreamEvent::ThinkingDelta("hmm".into())),
            Step::Ev(StreamEvent::BlockStop),
            Step::Ev(StreamEvent::ToolUseStart {
                id: "t1".into(),
                name: "write".into(),
            }),
            Step::Ev(StreamEvent::ToolInputDelta("{\"p\":".into())),
            Step::Ev(StreamEvent::ToolInputDelta("1}".into())),
            Step::Ev(StreamEvent::BlockStop),
            Step::Ev(StreamEvent::Stop {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            }),
        ]]);
        let mut sink = Recorder::default();
        let c = run(&p, RetryPolicy::default(), &mut sink).await.unwrap();
        assert_eq!(
            c.message.content,
            vec![
                ContentBlock::Thinking {
                    thinking: "hmm".into(),
                    signature: String::new()
                },
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "write".into(),
                    input: Json("{}".into())
                },
            ]
        );
        assert_eq!(c.stop_reason, StopReason::ToolUse);
        // The default sink, by contrast, hands the bytes through.
        let mut quiet = QuietSink;
        let c = run_quiet(&p, &mut quiet).await;
        assert_eq!(
            c.message.content[1],
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "write".into(),
                input: Json("{\"p\":1}".into())
            }
        );
    }

    async fn run_quiet(p: &Arc<Scripted>, sink: &mut QuietSink) -> Collected {
        stream_with_retries(
            &**p,
            req(),
            &CancellationToken::new(),
            &RetryPolicy::default(),
            sink,
        )
        .await
        .unwrap()
    }
}
