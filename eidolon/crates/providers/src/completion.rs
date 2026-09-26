//! A first-class tool-less completion: one provider-backed round trip whose
//! request type *cannot* express a tool.
//!
//! This is the shape a consumer needs when it wants an answer rather than an
//! agent: a skill's `ask_model`, a summary, a classification, a one-shot
//! prompt. The interactive harness reaches such answers through its agent
//! loop — registry, dispatch, session log, persona, project instructions —
//! and a consumer that wants none of that must not have to assemble a
//! workspace to get one, nor reinterpret a stream to get a string.
//!
//! **Tool-less is structural, not conventional.** [`CompletionRequest`] has
//! no tools field, so a caller cannot pass one by mistake; the request that
//! leaves here always offers zero tools and `ToolChoice::Auto`. Nothing in
//! this module reads a project file, a persona, a registry or a session, and
//! nothing it returns can be dispatched: a completion's result is data.
//!
//! Resolution and pricing are the catalog's, because that is where a model
//! string becomes something billable; the streaming, the retry rules and the
//! assembly are [`harnox::llm::stream_with_retries`], shared with the agent
//! loop rather than reimplemented here. What this module adds is the shape a
//! consumer actually consumes: a typed ending, the usage and timing that came
//! with it, the catalog's own rates beside them, and a text projection that
//! refuses to present an empty answer as a successful one.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, bail};
use tokio_util::sync::CancellationToken;

use eidolon_core::message::{Message, StopReason, Usage};
use eidolon_core::provider::{CacheOptions, ChatRequest, ThinkingConfig, ToolChoice};

use eidolon_core::provider::Provider;
use harnox::llm::{Collected, QuietSink, RetryPolicy};

use crate::catalog::{Catalog, ClaimedKey, Resolved, mock};
use crate::def::Cost;

/// What to ask for: real messages, request settings, cancellation.
///
/// No tools, by construction — see the module docs. `model` is any spec the
/// catalog resolves (`provider:id`, a bare id the catalog serves
/// unambiguously, `mock`), and `provider` is the same override `--provider`
/// takes: a provider name to resolve `model`'s id inside.
#[derive(Clone, Debug)]
pub struct CompletionRequest {
    pub model: String,
    pub provider: Option<String>,
    pub system: Option<String>,
    /// The transcript as the wire should carry it. A caller whose own
    /// history allows two turns of one role in a row merges them first: the
    /// wire wants strict alternation and this layer does not guess at a
    /// consumer's narration.
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    pub thinking: Option<ThinkingConfig>,
    pub temperature: Option<f64>,
    pub cache: CacheOptions,
    /// The retry rules for this call. [`RetryPolicy::default`] is the
    /// harness's: four attempts inside two minutes, none of them after
    /// output has been published.
    pub retries: RetryPolicy,
}

/// How one completion ended, as a value rather than a sentence.
///
/// The distinction this type exists for is the one a text-only API erases: a
/// model that answered with reasoning and no answer, one that was cut off at
/// its output cap, one the operator stopped, and one whose stream simply
/// ended without saying why are four different outcomes, and three of them
/// look exactly like a successful short reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionEnding {
    /// A terminal event arrived and there is text to show.
    Complete,
    /// A terminal event arrived and there is nothing a caller can show: no
    /// text and no thinking.
    ///
    /// A `tool_use` block does not count as content here, and the asymmetry
    /// is deliberate: a tool-less request has nothing that could execute a
    /// call and nothing to dispatch one with, so the block travels as *data*
    /// in [`CompletionResult::message`] for a caller that wants to look at
    /// it — and a reply that is only a tool call is `Empty`, with
    /// [`CompletionResult::stop_reason`] saying `ToolUse` for a caller that
    /// asks which it was. The mock provider's script is the case in the
    /// tests: a tool call, no text, `Empty`.
    Empty,
    /// Thinking and no text: the model reasoned without answering in words.
    /// A tool block riding along with it is data as above, not an answer.
    ThinkingOnly,
    /// The token fired, or the wire said the message was cancelled.
    Cancelled,
    /// An answer arrived and the model hit its output cap mid-thought: a real
    /// reply, just a short one, with [`CompletionResult::stop_reason`] saying
    /// `MaxTokens`. A cap reached with nothing on the wire is `Empty` — this
    /// ending means there is something to show that was cut short.
    Truncated,
    /// The stream ended with no terminal event: an incomplete stream, which
    /// is not the same ending as any stop reason and must not be reported
    /// as one. Whatever text arrived is real and partial; only the caller
    /// can decide whether partial is enough.
    IncompleteStream,
}

/// When the completion happened, from the caller's clock.
///
/// Two numbers, and they are not the same measurement: `total_ms` covers the
/// whole call *including* the retry sleeps and every attempt that was not
/// the one that answered, while `first_token_ms` is the wait the answering
/// attempt took — the figure a person reads as "how long before it started".
/// Retry sleeps are in the total, so a call that failed twice before
/// succeeding reads as slow rather than as instant, which is what the caller
/// experienced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompletionTiming {
    pub first_token_ms: Option<u64>,
    pub total_ms: u64,
}

/// One completion's outcome.
#[derive(Debug)]
pub struct CompletionResult {
    /// The catalog key this resolved to (`provider:id`), which is what a
    /// consumer journals and bills.
    pub key: String,
    pub provider: String,
    /// The bare wire id the backend was told to use.
    pub model: String,
    /// The assistant message as assembled: text and thinking in the order
    /// the stream produced them.
    pub message: Message,
    pub stop_reason: StopReason,
    /// What the answering attempt measured. A cancelled or truncated stream
    /// carries what arrived — a floor, never an invented total — and an
    /// attempt that failed outright contributes nothing, because inventing
    /// usage for it would make a partial total look final.
    pub usage: Usage,
    pub timing: CompletionTiming,
    pub ending: CompletionEnding,
    /// Which attempt answered: 1 when the first one did.
    pub attempts: u32,
    /// The catalog's rates for this model. `None` is *unpriced* — a gateway
    /// model nobody declared a price for, the mock, a driver — and is never
    /// zero: a consumer that recorded it as free would print a floor as a
    /// bill.
    pub cost: Option<Cost>,
    /// The token-pool lane this resolution claimed, when the provider
    /// declares one. Carried out rather than journalled here: per-session
    /// key attribution is a fact about the caller's conversation, and only
    /// the caller has one.
    pub claim: Option<ClaimedKey>,
}

impl CompletionResult {
    /// What this call cost at the catalog's rates, or `None` when the model
    /// is unpriced. `Some(0.0)` is a known zero — a definition that declared
    /// its rates as zero — and the two are different facts.
    pub fn price(&self) -> Option<f64> {
        self.cost.as_ref().map(|c| c.price(&self.usage))
    }

    /// The answer, as one trimmed string.
    ///
    /// An empty answer is an error here rather than an `Ok("")`, because
    /// every caller of this projection is about to show the string to
    /// somebody, and an empty one reads as the model having said nothing to
    /// say. A caller that wants to distinguish *why* it was empty — thinking
    /// only, cancelled, a stream that never ended — reads
    /// [`CompletionResult::ending`] instead of going through this, or guards
    /// on [`CompletionResult::has_text`], which asks the same question this
    /// does and never answers it wrongly.
    pub fn text(&self) -> anyhow::Result<String> {
        let text = self.message.text().trim().to_string();
        if text.is_empty() {
            bail!(
                "the model returned no text (stop reason: {:?}, ending: {:?})",
                self.stop_reason,
                self.ending
            );
        }
        Ok(text)
    }

    /// The reasoning the model produced, for a caller that wants to show it
    /// beside the answer. Never the answer itself — see
    /// [`Message::thinking`].
    pub fn thinking(&self) -> String {
        self.message.thinking()
    }

    /// Whether there is an answer to show — the same question [`Self::text`]
    /// answers, so a caller may guard on this without meeting the error it
    /// promised not to. It is a question about the *message* and not about
    /// the ending: a cancelled or incomplete stream can have text or none,
    /// which is why the ending cannot carry it.
    pub fn has_text(&self) -> bool {
        !self.message.text().trim().is_empty()
    }
}

impl Catalog {
    /// Run one tool-less completion, resolving `spec` through the catalog.
    ///
    /// `Err` here is either a *resolution* failure (a spec this catalog
    /// cannot serve) or a failed request. Neither is a credential *state*:
    /// what a picker or a health check asks about a provider is
    /// [`Catalog::readiness`], which answers without touching the network.
    pub async fn complete(
        &self,
        req: CompletionRequest,
        cancel: CancellationToken,
    ) -> anyhow::Result<CompletionResult> {
        let spec = req.model.clone();
        match self.resolve(&spec, req.provider.as_deref())? {
            Resolved::Mock => self.complete_with(mock(), "mock", None, req, cancel).await,
            // A driver-backed spec (`claude-cli:sonnet`, or a bare `sonnet`)
            // is an **agent-turn** route: the Claude CLI driver serves turns
            // and has no tool-less completion to offer. Emulating one by
            // handing it a workspace, or quietly swapping in another model,
            // would answer a question nobody asked — so say what happened and
            // name the way through.
            Resolved::ClaudeCli { model, .. } => bail!(
                "{spec:?} resolves to the Claude CLI driver ({}) — agent turns only, since a \
                 driver has no tool-less completion. Name a provider-backed model instead, e.g. \
                 \"deepseek-v4-pro\", or a \"provider:model\" catalog key",
                model.unwrap_or_else(|| "claude-cli".into())
            ),
            Resolved::Http {
                provider,
                model,
                claim,
            } => {
                let key = format!("{}:{model}", provider.name());
                self.complete_with(provider, &key, claim, req, cancel).await
            }
        }
    }

    /// The half of [`Catalog::complete`] that does not resolve anything: one
    /// tool-less completion on a provider the caller already holds, priced
    /// from this catalog.
    ///
    /// Public because there are two honest callers for it — a caller that
    /// resolved its backend earlier and would rather not resolve twice, and
    /// a test that wants the real rules over a scripted stream, which no
    /// catalog can be made to produce. The rules are the same either way:
    /// this is where they live, and `complete` is resolution in front of it.
    pub async fn complete_with(
        &self,
        provider: Arc<dyn Provider>,
        key: &str,
        claim: Option<ClaimedKey>,
        req: CompletionRequest,
        cancel: CancellationToken,
    ) -> anyhow::Result<CompletionResult> {
        let started = Instant::now();
        let request = ChatRequest {
            model: model_id_of(key, &provider),
            system: req.system,
            messages: req.messages,
            // The module's whole point, and the reason the request type has
            // no field that could say otherwise.
            tools: Vec::new(),
            max_tokens: req.max_tokens,
            thinking: req.thinking,
            cache: req.cache,
            // Both are the *definition's* answers, filled in by the provider
            // on the way past: the catalog's own model row knows whether this
            // model can see and what it samples at, and neither belongs in a
            // request the caller wrote.
            vision: None,
            temperature: req.temperature,
            tool_choice: ToolChoice::Auto,
        };
        let mut sink = QuietSink;
        let collected = harnox::llm::stream_with_retries(
            &*provider,
            request,
            &cancel,
            &req.retries,
            &mut sink,
        )
        .await
        .with_context(|| format!("tool-less completion on {key}"))?;
        Ok(self.result_from(collected, key, claim, started, cancel.is_cancelled()))
    }

    /// Shape one collected stream into a result: the ending, the timings and
    /// the rates are decisions, not reads.
    fn result_from(
        &self,
        collected: Collected,
        key: &str,
        claim: Option<ClaimedKey>,
        started: Instant,
        cancelled: bool,
    ) -> CompletionResult {
        let text = collected.message.text();
        let thinking = collected.message.thinking();
        // Two questions, in this order: how it ended (stopped, or never
        // ended), then what is actually there. Content before the stop
        // reason, so `Truncated` cannot mean "the cap was hit" while the
        // wire is empty — a cap reached with nothing to show is `Empty`, and
        // `stop_reason` still says `MaxTokens` for a caller that asks which.
        let ending = if cancelled || collected.stop_reason == StopReason::Cancelled {
            CompletionEnding::Cancelled
        } else if !collected.terminal {
            // No terminal event at all: the stream ended, and whether that
            // was a whole answer is exactly what cannot be assumed.
            CompletionEnding::IncompleteStream
        } else if text.trim().is_empty() && thinking.trim().is_empty() {
            CompletionEnding::Empty
        } else if text.trim().is_empty() {
            CompletionEnding::ThinkingOnly
        } else if collected.stop_reason == StopReason::MaxTokens {
            CompletionEnding::Truncated
        } else {
            CompletionEnding::Complete
        };
        let (provider, model) = key.split_once(':').unwrap_or((key, ""));
        CompletionResult {
            key: key.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            message: collected.message,
            stop_reason: collected.stop_reason,
            usage: collected.usage,
            timing: CompletionTiming {
                first_token_ms: collected.timing.first_token_ms,
                total_ms: started.elapsed().as_millis() as u64,
            },
            ending,
            attempts: collected.attempts,
            cost: self.cost(key),
            claim,
        }
    }
}

/// The bare id a backend is told to use for `key`, falling back to the key
/// itself where it carries no `provider:` prefix (`mock`).
fn model_id_of(key: &str, provider: &Arc<dyn Provider>) -> String {
    match key.split_once(':') {
        Some((_, "")) => provider.name().to_string(),
        Some((_, id)) => id.to_string(),
        None => key.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use eidolon_core::provider::{EventStream, StreamEvent};
    use eidolon_core::{StopReason, Usage};

    use crate::script::ProviderScript;

    /// A provider that plays back exactly the beats it is given: no implicit
    /// `Start` or `Stop`, one script per attempt, and a beat that hangs where
    /// a test needs a stream to stay open until it is cancelled. It also
    /// keeps every request, so a test can assert what actually went out.
    struct Scripted {
        attempts: Mutex<VecDeque<Vec<Beat>>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    #[derive(Clone)]
    enum Beat {
        Ev(StreamEvent),
        Fail(&'static str),
        /// Yield nothing further, ever: the attempt stays open until the
        /// token fires.
        Hang,
    }

    impl Scripted {
        fn new(attempts: Vec<Vec<Beat>>) -> Arc<Self> {
            Arc::new(Scripted {
                attempts: Mutex::new(attempts.into()),
                requests: Mutex::new(Vec::new()),
            })
        }
        fn calls(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
        fn last_request(&self) -> ChatRequest {
            self.requests.lock().unwrap().last().cloned().expect("a request went out")
        }
    }

    impl Provider for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }
        fn stream<'a>(&'a self, req: ChatRequest, _cancel: CancellationToken) -> EventStream<'a> {
            self.requests.lock().unwrap().push(req);
            let mut q = self.attempts.lock().unwrap();
            let beats = if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                q.front().cloned().unwrap_or_default()
            };
            Box::pin(futures_util::stream::unfold(beats.into_iter(), |mut rest| async move {
                let beat = rest.next()?;
                match beat {
                    Beat::Ev(ev) => Some((Ok(ev), rest)),
                    Beat::Fail(why) => Some((Err(anyhow::anyhow!("{why}")), Vec::new().into_iter())),
                    Beat::Hang => {
                        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                        None
                    }
                }
            }))
        }
    }

    /// A catalog with one provider whose model carries declared rates — the
    /// pricing half of what a completion has to get right.
    fn cat() -> Catalog {
        let mut c = Catalog::new();
        let src = r#"pub fn provider() {
    #{ name: "gw", wire: "openai", base_url: "http://127.0.0.1:1", token: "t",
       models: [ #{ id: "m", cost: #{ input: 1.0, output: 2.0, cache_read: 0.1, cache_write: 0.0 } },
                 #{ id: "free" } ] }
}"#;
        let (def, script) = ProviderScript::load("gw", src).unwrap();
        c.add_def(def, Some(script), "test");
        c
    }

    fn req(model: &str) -> CompletionRequest {
        CompletionRequest {
            model: model.into(),
            provider: None,
            system: None,
            messages: vec![Message::user_text("hello")],
            max_tokens: 64,
            thinking: None,
            temperature: None,
            cache: CacheOptions::off(),
            retries: RetryPolicy::default(),
        }
    }

    fn text_events(text: &str) -> Vec<Beat> {
        vec![
            Beat::Ev(StreamEvent::Start {
                usage: Usage { input_tokens: 10, ..Default::default() },
            }),
            Beat::Ev(StreamEvent::TextDelta(text.into())),
            Beat::Ev(StreamEvent::BlockStop),
            Beat::Ev(StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                usage: Usage { input_tokens: 555, output_tokens: 7, ..Default::default() },
            }),
        ]
    }

    async fn run(
        cat: &Catalog,
        provider: &Arc<Scripted>,
        key: &str,
        req: CompletionRequest,
        cancel: CancellationToken,
    ) -> anyhow::Result<CompletionResult> {
        cat.complete_with(provider.clone(), key, None, req, cancel).await
    }

    /// The request that leaves is tool-less by construction, carries the
    /// caller's transcript byte for byte, and names the model the key does —
    /// not the key, which belongs to the journal.
    #[tokio::test]
    async fn a_tool_less_request_offers_no_tools_and_leaves_the_transcript_alone() {
        let p = Scripted::new(vec![text_events("hi")]);
        let mut request = req("gw:m");
        // Two turns of one role in a row: the wire wants alternation, and the
        // normalisation that produces it is the *caller's* — this layer does
        // not guess at a consumer's narration, so it passes them through.
        request.messages = vec![Message::user_text("one"), Message::user_text("two")];
        let out = run(&cat(), &p, "gw:m", request, CancellationToken::new())
            .await
            .unwrap();
        let seen = p.last_request();
        assert!(seen.tools.is_empty(), "a completion never offers a tool");
        assert_eq!(seen.tool_choice, ToolChoice::Auto);
        assert_eq!(seen.model, "m", "the backend is told the bare id");
        assert_eq!(seen.messages.len(), 2, "the transcript arrived unmerged");
        assert_eq!(out.key, "gw:m");
        assert_eq!(out.provider, "gw");
        assert_eq!(out.model, "m");
        assert_eq!(out.text().unwrap(), "hi");
        assert_eq!(out.ending, CompletionEnding::Complete);
        assert_eq!(out.attempts, 1);
    }

    /// Usage is what the wire said, the rates are the catalog's, and the two
    /// together are the price. The input side comes from the terminal event
    /// on the wire that only sends it there.
    #[tokio::test]
    async fn usage_rates_and_price_travel_with_the_result() {
        let p = Scripted::new(vec![vec![
            Beat::Ev(StreamEvent::Start {
                usage: Usage { input_tokens: 10, cache_read_input_tokens: 40, ..Default::default() },
            }),
            Beat::Ev(StreamEvent::TextDelta("hi".into())),
            Beat::Ev(StreamEvent::Stop {
                stop_reason: StopReason::EndTurn,
                usage: Usage { input_tokens: 555, output_tokens: 7, cache_read_input_tokens: 100, ..Default::default() },
            }),
        ]]);
        let out = run(&cat(), &p, "gw:m", req("gw:m"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.usage.input_tokens, 555);
        assert_eq!(out.usage.cache_read_input_tokens, 100);
        assert_eq!(out.usage.output_tokens, 7);
        let cost = out.cost.clone().expect("the definition declares rates");
        let expected = cost.price(&out.usage);
        assert_eq!(out.price(), Some(expected));
        assert!(expected > 0.0);
    }

    /// An unpriced model is unpriced: never zero, and never presented as a
    /// bill of nothing. Asserted through the branch that actually matters —
    /// a *declared* provider whose model has no price row, which is every id
    /// a gateway discovered and nobody wrote a `[[models]]` line for — and
    /// then through the mock, which has no rates by construction.
    #[tokio::test]
    async fn an_unpriced_model_carries_no_price_at_all() {
        let p = Scripted::new(vec![text_events("hi")]);
        let out = run(&cat(), &p, "gw:free", req("gw:free"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.key, "gw:free");
        assert_eq!(out.cost, None);
        assert_eq!(out.price(), None);

        let p = Scripted::new(vec![text_events("hi")]);
        let out = run(&cat(), &p, "mock", req("mock"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.cost, None);
        assert_eq!(out.price(), None);
    }

    /// The endings a text-only API erases are told apart here, and one of
    /// them is a stream that never said why it stopped.
    #[tokio::test]
    async fn the_endings_are_told_apart() {
        let cat = cat();
        let cases = [
            (
                vec![Beat::Ev(StreamEvent::BlockStop), Beat::Ev(StreamEvent::Stop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                })],
                CompletionEnding::Empty,
            ),
            (
                vec![
                    Beat::Ev(StreamEvent::ThinkingDelta("hmm".into())),
                    Beat::Ev(StreamEvent::Stop { stop_reason: StopReason::EndTurn, usage: Usage::default() }),
                ],
                CompletionEnding::ThinkingOnly,
            ),
            (
                vec![
                    Beat::Ev(StreamEvent::TextDelta("half".into())),
                    Beat::Ev(StreamEvent::Stop { stop_reason: StopReason::MaxTokens, usage: Usage::default() }),
                ],
                CompletionEnding::Truncated,
            ),
            // A clean EOF with no terminal event at all.
            (vec![Beat::Ev(StreamEvent::TextDelta("half".into()))], CompletionEnding::IncompleteStream),
        ];
        for (beats, expected) in cases {
            let p = Scripted::new(vec![beats]);
            let out = run(&cat, &p, "gw:m", req("gw:m"), CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(out.ending, expected, "stop reason {:?}", out.stop_reason);
        }
    }

    /// Cancellation ends the completion where it is, keeps what arrived, and
    /// says so — rather than presenting a partial answer as a whole one.
    #[tokio::test]
    async fn a_cancelled_completion_keeps_what_arrived_and_says_it_was_cancelled() {
        let p = Scripted::new(vec![vec![
            Beat::Ev(StreamEvent::TextDelta("par".into())),
            Beat::Hang,
        ]]);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            token.cancel();
        });
        let started = Instant::now();
        let out = run(&cat(), &p, "gw:m", req("gw:m"), cancel).await.unwrap();
        assert_eq!(out.ending, CompletionEnding::Cancelled);
        assert_eq!(out.stop_reason, StopReason::Cancelled);
        assert_eq!(out.text().unwrap(), "par");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    /// A transient failure before anything was published is invisible: the
    /// retry answers, the caller sees one result, and the count says two.
    /// The retry sleep is inside the total, because that is the wait the
    /// caller experienced.
    #[tokio::test]
    async fn a_transient_failure_before_output_is_retried_and_counted() {
        let p = Scripted::new(vec![
            vec![Beat::Fail("gw returned HTTP 503 Service Unavailable")],
            text_events("hi"),
        ]);
        let out = run(&cat(), &p, "gw:m", req("gw:m"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.text().unwrap(), "hi");
        assert_eq!(out.ending, CompletionEnding::Complete);
        assert_eq!(out.attempts, 2);
        assert_eq!(p.calls(), 2);
        assert!(
            out.timing.total_ms >= 500,
            "the backoff is in the total: {:?}",
            out.timing
        );
    }

    /// A failure after output is never replayed, and an auth failure is never
    /// retried at all: both reach the caller as errors, on the first attempt.
    #[tokio::test]
    async fn a_failure_after_output_and_an_auth_failure_both_reach_the_caller() {
        let p = Scripted::new(vec![vec![
            Beat::Ev(StreamEvent::TextDelta("par".into())),
            Beat::Fail("connection reset by peer"),
        ]]);
        let e = run(&cat(), &p, "gw:m", req("gw:m"), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("connection reset by peer"), "{e:#}");
        assert!(format!("{e:#}").contains("gw:m"), "the failure names the call: {e:#}");
        assert_eq!(p.calls(), 1, "published output is never re-run");

        let p = Scripted::new(vec![vec![Beat::Fail("gw returned HTTP 401 Unauthorized")]]);
        let e = run(&cat(), &p, "gw:m", req("gw:m"), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("401"), "{e:#}");
        assert_eq!(p.calls(), 1, "a wrong key is not an outage");
    }

    /// The projection a consumer shows, and the reason it is a `Result`: an
    /// empty answer is not a success. The structured layer still says which
    /// kind of empty it was.
    #[tokio::test]
    async fn text_refuses_an_empty_answer_while_the_ending_explains_it() {
        let p = Scripted::new(vec![vec![
            Beat::Ev(StreamEvent::ThinkingDelta("thinking, not answering".into())),
            Beat::Ev(StreamEvent::BlockStop),
            Beat::Ev(StreamEvent::Stop { stop_reason: StopReason::EndTurn, usage: Usage::default() }),
        ]]);
        let out = run(&cat(), &p, "gw:m", req("gw:m"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.ending, CompletionEnding::ThinkingOnly);
        assert!(out.thinking().contains("thinking, not answering"));
        let e = out.text().unwrap_err().to_string();
        assert!(e.contains("no text") && e.contains("ThinkingOnly"), "{e}");
        assert!(!out.has_text(), "the guard agrees with `text()`");
    }

    /// The cap can fall with nothing on the wire, and then the ending is
    /// `Empty` — `Truncated` means an answer was cut short, so a caller that
    /// guards on `has_text` is never sent to an error that promised text.
    /// The stop reason still says which it was.
    #[tokio::test]
    async fn a_cap_reached_with_nothing_to_show_is_empty_and_says_so_in_the_stop_reason() {
        let cat = cat();
        for beats in [
            vec![Beat::Ev(StreamEvent::Stop {
                stop_reason: StopReason::MaxTokens,
                usage: Usage::default(),
            })],
            vec![
                Beat::Ev(StreamEvent::ThinkingDelta("thought, then capped".into())),
                Beat::Ev(StreamEvent::Stop { stop_reason: StopReason::MaxTokens, usage: Usage::default() }),
            ],
        ] {
            let p = Scripted::new(vec![beats]);
            let out = run(&cat, &p, "gw:m", req("gw:m"), CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(out.stop_reason, StopReason::MaxTokens);
            assert!(
                matches!(out.ending, CompletionEnding::Empty | CompletionEnding::ThinkingOnly),
                "{:?}",
                out.ending
            );
            assert!(!out.has_text() && out.text().is_err(), "{:?}", out.ending);
        }
    }

    /// A driver is refused explicitly, and the refusal names the way through.
    /// Nothing is spawned, nothing is emulated, and no model is swapped in.
    #[tokio::test]
    async fn a_driver_spec_is_an_explicit_unsupported_operation() {
        let cat = Catalog::new().with_claude(Some(crate::catalog::ClaudeCliEntry {
            binary: None,
            models: vec!["sonnet".into()],
            tools: Default::default(),
        }));
        let e = cat
            .complete(req("sonnet"), CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("driver") && e.contains("tool-less completion"), "{e}");
        assert!(e.contains("provider-backed model"), "{e}");
    }

    /// `complete` resolves, and the mock is the provider every catalog has:
    /// its script's first reply is a *tool call*, which a tool-less
    /// completion neither executes nor hides — there is no registry here to
    /// execute one with, and there is nothing to dispatch. The result says
    /// `Empty` because there is no text to show, `stop_reason` says which
    /// kind of end it was, and the block is carried as data for a caller that
    /// wants to look at it.
    #[tokio::test]
    async fn the_mock_resolves_and_its_tool_call_stays_data() {
        let out = Catalog::new()
            .complete(req("mock"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.key, "mock");
        assert_eq!(out.stop_reason, StopReason::ToolUse);
        assert_eq!(out.ending, CompletionEnding::Empty);
        assert!(!out.has_text() && out.text().is_err());
        assert!(
            out.message.tool_uses().next().is_some(),
            "the block arrived as data: nothing dispatched it, nothing hid it"
        );
        assert_eq!(out.cost, None, "the mock has no rates to declare");
    }

    /// A spec this catalog cannot serve is a resolution failure, and the two
    /// are not the same error: this one never reached a credential at all.
    #[tokio::test]
    async fn an_unresolvable_spec_fails_before_anything_is_asked() {
        // An unknown `provider:` prefix is not a resolution: the catalog reads
        // the whole spec as a model id and says which one it could not serve.
        let e = cat()
            .complete(req("nope:nothing"), CancellationToken::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("no provider serves") && e.contains("nope:nothing"), "{e}");
    }

    /// The lane a pooled resolution took is carried out to the caller, who is
    /// the only one that can attribute it to a conversation.
    #[tokio::test]
    async fn the_claimed_lane_is_carried_out() {
        let p = Scripted::new(vec![text_events("hi")]);
        let claim = ClaimedKey {
            provider: "pool-gw".into(),
            secret: "pool-gw".into(),
            fresh: true,
        };
        let out = cat()
            .complete_with(p.clone(), "gw:m", Some(claim.clone()), req("gw:m"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.claim, Some(claim));
    }
}
