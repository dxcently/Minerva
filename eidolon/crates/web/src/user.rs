//! `UserIo` for the web: an ask goes to every open stream and its answer
//! comes back on `POST /api/answer`. Any number may be pending; an answer
//! resolves only the `ask_id` it names.
//!
//! ```text
//!   {"type":"ask","kind":"approval","ask_id":3,"call_id":"tu1","tool":"write",
//!    "input":{"path":"/tmp/x"},"prompt":"write — outside the working directory. Run it?",
//!    "reason":"outside-cwd","structural":true,"judged":null,"yolo":false,
//!    "answers":["yes","no"]}
//! ```
//!
//! ```text
//!   {"type":"ask","kind":"question","ask_id":4,"prompt":"pick one",
//!    "options":[{"label":"a"},{"label":"b","description":"…"}]}
//! ```
//!
//! ```text
//!   {"type":"ask-settled","ask_id":3,"how":"answered","answer":"yes"}
//!   {"type":"ask-settled","ask_id":3,"how":"cancelled"}
//! ```

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use eidolon_core::policy::{Ruling, Verdict};
use eidolon_core::tool::{ToolCall, ToolManifest};
use eidolon_core::user::{Choice, UserIo};

const YES: &str = "yes";
const NO: &str = "no";

#[derive(Clone, Serialize)]
pub struct AskOption {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Not on the bus: `Event::AskUser` is a transcript line, not something to answer.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Ask {
    Approval {
        ask_id: u64,
        call_id: String,
        tool: String,
        input: serde_json::Value,
        prompt: String,
        reason: Option<String>,
        structural: bool,
        judged: Option<String>,
        yolo: bool,
        answers: [&'static str; 2],
    },
    /// [`UserIo::choose`], and so [`UserIo::confirm`].
    Question { ask_id: u64, prompt: String, options: Vec<AskOption> },
}

impl Ask {
    pub(crate) fn ask_id(&self) -> u64 {
        match self {
            Ask::Approval { ask_id, .. } | Ask::Question { ask_id, .. } => *ask_id,
        }
    }

    fn accepts(&self, answer: &str) -> bool {
        match self {
            Ask::Approval { answers, .. } => answers.contains(&answer),
            Ask::Question { options, .. } => options.iter().any(|o| o.label == answer),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settled {
    Answered(String),
    /// The token fired. Nothing above `UserIo` says why.
    Cancelled,
}

impl Settled {
    pub(crate) fn how(&self) -> &'static str {
        match self {
            Settled::Answered(_) => "answered",
            Settled::Cancelled => "cancelled",
        }
    }

    pub(crate) fn answer(&self) -> Option<&str> {
        match self {
            Settled::Answered(a) => Some(a),
            Settled::Cancelled => None,
        }
    }
}

#[derive(Clone)]
pub enum AskEvent {
    Opened(Ask),
    Settled { ask_id: u64, how: Settled },
}

/// One per `POST /api/answer` status: 204, 409, 422.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Accepted,
    /// Never opened, or already settled.
    Unknown,
    /// Not an answer it offers; it stays pending.
    Rejected,
}

struct Pending {
    ask: Ask,
    reply: oneshot::Sender<Option<String>>,
}

pub struct WebUser {
    next_id: std::sync::atomic::AtomicU64,
    /// Ordered, so a reconnecting stream gets the asks oldest first.
    pending: Mutex<BTreeMap<u64, Pending>>,
    asks: broadcast::Sender<AskEvent>,
}

impl Default for WebUser {
    fn default() -> Self {
        Self::new()
    }
}

impl WebUser {
    pub fn new() -> Self {
        Self::with_ask_capacity(16)
    }

    /// For the ask-lag test in `crate::stream`.
    pub(crate) fn with_ask_capacity(capacity: usize) -> Self {
        let (asks, _) = broadcast::channel(capacity);
        WebUser {
            next_id: std::sync::atomic::AtomicU64::new(1),
            pending: Mutex::new(BTreeMap::new()),
            asks,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AskEvent> {
        self.asks.subscribe()
    }

    /// Oldest first.
    pub fn pending(&self) -> Vec<Ask> {
        self.pending.lock().unwrap().values().map(|p| p.ask.clone()).collect()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    pub fn answer(&self, ask_id: u64, answer: &str) -> Answer {
        let entry = {
            let mut pending = self.pending.lock().unwrap();
            let Some(entry) = pending.get(&ask_id) else { return Answer::Unknown };
            if !entry.ask.accepts(answer) {
                return Answer::Rejected;
            }
            pending.remove(&ask_id).expect("just looked it up")
        };
        let _ = self.asks.send(AskEvent::Settled {
            ask_id,
            how: Settled::Answered(answer.to_string()),
        });
        // Fails only if the waiter's cancel fired first; nobody is left to tell.
        let _ = entry.reply.send(Some(answer.to_string()));
        Answer::Accepted
    }

    async fn ask(&self, ask: Ask, cancel: &CancellationToken) -> Option<String> {
        let ask_id = ask.ask_id();
        let (reply, rx) = oneshot::channel();
        // Registered before it is announced, so `pending` is complete for any
        // stream that sees the frame or opens in between.
        self.pending.lock().unwrap().insert(ask_id, Pending { ask: ask.clone(), reply });
        let _ = self.asks.send(AskEvent::Opened(ask));
        let answer = tokio::select! {
            r = rx => r.ok().flatten(),
            _ = cancel.cancelled() => None,
        };
        // `answer` removes the entry, so one still here means the token won.
        // Exactly one settle goes out either way.
        if self.pending.lock().unwrap().remove(&ask_id).is_some() {
            let _ = self.asks.send(AskEvent::Settled { ask_id, how: Settled::Cancelled });
        }
        answer
    }

    fn next_ask_id(&self) -> u64 {
        self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

#[async_trait]
impl UserIo for WebUser {
    async fn choose(&self, prompt: &str, options: &[Choice], cancel: &CancellationToken) -> Option<String> {
        let ask = Ask::Question {
            ask_id: self.next_ask_id(),
            prompt: prompt.to_string(),
            options: options
                .iter()
                .map(|c| AskOption { label: c.label.clone(), description: c.description.clone() })
                .collect(),
        };
        self.ask(ask, cancel).await
    }

    /// Overridden so the page gets the call and the ruling, not a sentence.
    async fn approve(&self, call: &ToolCall, _manifest: &ToolManifest, ruling: &Ruling, cancel: &CancellationToken) -> bool {
        // The fallback is the trait default's; the dispatcher only calls this on `Ask`.
        let prompt = match &ruling.verdict {
            Verdict::Ask(p) => p.clone(),
            _ => format!("Run `{}`?", call.name),
        };
        let ask = Ask::Approval {
            ask_id: self.next_ask_id(),
            call_id: call.id.clone(),
            tool: call.name.clone(),
            input: call.input.clone(),
            prompt,
            reason: ruling.reason.clone(),
            structural: ruling.structural,
            judged: ruling.judged.clone(),
            yolo: ruling.yolo,
            answers: [YES, NO],
        };
        matches!(self.ask(ask, cancel).await.as_deref(), Some(YES))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use eidolon_core::policy::Approval;
    use eidolon_core::tool::CallOrigin;

    use super::*;

    fn choice(label: &str) -> Choice {
        Choice { label: label.into(), description: None }
    }

    fn note_call() -> ToolCall {
        ToolCall {
            id: "tu1".into(),
            name: "note".into(),
            input: serde_json::json!({"text": "hi"}),
            origin: CallOrigin::Model,
        }
    }

    fn asking_ruling() -> Ruling {
        Ruling {
            verdict: Verdict::Ask("note — outside the working directory. Run it?".into()),
            reason: Some("outside-cwd".into()),
            structural: true,
            judged: None,
            yolo: false,
        }
    }

    /// The page reads these fields by name.
    #[tokio::test]
    async fn an_approval_frame_carries_the_call_the_prompt_and_the_ruling() {
        let user = Arc::new(WebUser::new());
        let mut asks = user.subscribe();
        let cancel = CancellationToken::new();
        let manifest = ToolManifest::synthetic("note", Approval::Mutating);
        let approving = tokio::spawn({
            let user = user.clone();
            let cancel = cancel.clone();
            let ruling = asking_ruling();
            async move { user.approve(&note_call(), &manifest, &ruling, &cancel).await }
        });

        let opened = match asks.recv().await.unwrap() {
            AskEvent::Opened(a) => a,
            _ => panic!("expected Opened"),
        };
        assert_eq!(
            serde_json::to_value(&opened).unwrap(),
            serde_json::json!({
                "kind": "approval", "ask_id": 1, "call_id": "tu1", "tool": "note",
                "input": {"text": "hi"},
                "prompt": "note — outside the working directory. Run it?",
                "reason": "outside-cwd", "structural": true, "judged": null, "yolo": false,
                "answers": ["yes", "no"],
            })
        );
        assert_eq!(user.answer(1, "yes"), Answer::Accepted);
        assert!(approving.await.unwrap());
    }

    #[tokio::test]
    async fn a_confirm_frame_is_a_question_with_yes_and_no_labels() {
        let user = Arc::new(WebUser::new());
        let mut asks = user.subscribe();
        let cancel = CancellationToken::new();
        let confirming = tokio::spawn({
            let user = user.clone();
            let cancel = cancel.clone();
            async move { user.confirm("run it?", &cancel).await }
        });
        let opened = match asks.recv().await.unwrap() {
            AskEvent::Opened(a) => a,
            _ => panic!("expected Opened"),
        };
        assert_eq!(
            serde_json::to_value(&opened).unwrap(),
            serde_json::json!({
                "kind": "question", "ask_id": 1, "prompt": "run it?",
                "options": [{"label": "yes"}, {"label": "no"}],
            })
        );
        assert_eq!(user.answer(1, "yes"), Answer::Accepted);
        assert!(confirming.await.unwrap());
    }

    #[tokio::test]
    async fn answer_resolves_pending_ask() {
        let user = Arc::new(WebUser::new());
        let cancel = CancellationToken::new();
        let opts = [choice("a"), choice("b")];
        let mut asks = user.subscribe();
        let asked = tokio::spawn({
            let user = user.clone();
            let cancel = cancel.clone();
            async move { user.choose("pick one", &opts, &cancel).await }
        });
        let ask_id = match asks.recv().await.unwrap() {
            AskEvent::Opened(a) => a.ask_id(),
            _ => panic!("expected Opened"),
        };
        assert_eq!(user.answer(ask_id, "b"), Answer::Accepted);
        assert_eq!(asked.await.unwrap().as_deref(), Some("b"));
        assert_eq!(user.answer(ask_id, "a"), Answer::Unknown);
    }

    #[tokio::test]
    async fn an_ask_refuses_an_answer_it_does_not_offer_and_stays_pending() {
        let user = Arc::new(WebUser::new());
        let cancel = CancellationToken::new();
        let opts = [choice("a"), choice("b")];
        let mut asks = user.subscribe();
        let asked = tokio::spawn({
            let user = user.clone();
            let cancel = cancel.clone();
            async move { user.choose("pick one", &opts, &cancel).await }
        });
        let ask_id = match asks.recv().await.unwrap() {
            AskEvent::Opened(a) => a.ask_id(),
            _ => panic!("expected Opened"),
        };
        assert_eq!(user.answer(ask_id, "c"), Answer::Rejected);
        assert_eq!(user.pending_count(), 1, "a refused answer settles nothing");
        assert!(!asked.is_finished(), "the ask is still waiting");
        assert_eq!(user.answer(ask_id, "a"), Answer::Accepted);
        assert_eq!(asked.await.unwrap().as_deref(), Some("a"));
        assert_eq!(user.pending_count(), 0);
    }

    #[tokio::test]
    async fn an_approval_refuses_an_answer_that_is_neither_yes_nor_no() {
        let user = Arc::new(WebUser::new());
        let mut asks = user.subscribe();
        let cancel = CancellationToken::new();
        let manifest = ToolManifest::synthetic("note", Approval::Mutating);
        let approving = tokio::spawn({
            let user = user.clone();
            let cancel = cancel.clone();
            async move { user.approve(&note_call(), &manifest, &asking_ruling(), &cancel).await }
        });
        let ask_id = match asks.recv().await.unwrap() {
            AskEvent::Opened(a) => a.ask_id(),
            _ => panic!("expected Opened"),
        };
        assert_eq!(user.answer(ask_id, "maybe"), Answer::Rejected);
        assert_eq!(user.pending_count(), 1);
        assert_eq!(user.answer(ask_id, "no"), Answer::Accepted);
        assert!(!approving.await.unwrap(), "an approval only ever says yes to `yes`");
    }

    #[tokio::test]
    async fn two_pending_asks_each_resolve_only_by_their_own_id_oldest_first() {
        let (user, first, second, ids) = two_open_asks().await;

        assert_eq!(user.answer(ids[0], "a"), Answer::Accepted);
        assert_eq!(first.await.unwrap().as_deref(), Some("a"));
        assert!(!second.is_finished(), "the first answer must not resolve the second ask");
        assert_eq!(user.pending_count(), 1);

        assert_eq!(user.answer(ids[1], "b"), Answer::Accepted);
        assert_eq!(second.await.unwrap().as_deref(), Some("b"));
        assert_eq!(user.pending_count(), 0);
    }

    #[tokio::test]
    async fn two_pending_asks_each_resolve_only_by_their_own_id_newest_first() {
        let (user, first, second, ids) = two_open_asks().await;

        assert_eq!(user.answer(ids[1], "b"), Answer::Accepted);
        assert_eq!(second.await.unwrap().as_deref(), Some("b"));
        assert!(!first.is_finished(), "the second answer must not resolve the first ask");
        assert_eq!(user.pending_count(), 1);

        assert_eq!(user.answer(ids[0], "a"), Answer::Accepted);
        assert_eq!(first.await.unwrap().as_deref(), Some("a"));
        assert_eq!(user.pending_count(), 0);
    }

    #[tokio::test]
    async fn two_asks_are_pending_together_and_pending_lists_them_oldest_first() {
        let (user, _first, _second, ids) = two_open_asks().await;
        assert_eq!(user.pending_count(), 2);
        assert_eq!(user.pending().iter().map(Ask::ask_id).collect::<Vec<_>>(), ids);
    }

    #[tokio::test]
    async fn cancel_settles_the_ask_as_cancelled() {
        let user = Arc::new(WebUser::new());
        let cancel = CancellationToken::new();
        let mut asks = user.subscribe();
        let task = tokio::spawn({
            let user = user.clone();
            let cancel = cancel.clone();
            async move { user.confirm("run it?", &cancel).await }
        });
        let ask_id = loop {
            match asks.recv().await.unwrap() {
                AskEvent::Opened(a) => break a.ask_id(),
                _ => continue,
            }
        };
        cancel.cancel();
        assert!(!task.await.unwrap());
        match asks.recv().await.unwrap() {
            AskEvent::Settled { ask_id: settled, how } => {
                assert_eq!(settled, ask_id);
                assert_eq!(how, Settled::Cancelled);
            }
            _ => panic!("expected Settled"),
        }
        assert_eq!(user.pending_count(), 0);
        assert_eq!(user.answer(ask_id, "yes"), Answer::Unknown, "a cancelled ask is nobody's to answer");
    }

    /// Each `Opened` is awaited before the next raise, so which handle holds
    /// which id does not depend on the scheduler.
    async fn two_open_asks() -> (
        Arc<WebUser>,
        tokio::task::JoinHandle<Option<String>>,
        tokio::task::JoinHandle<Option<String>>,
        Vec<u64>,
    ) {
        let user = Arc::new(WebUser::new());
        let mut asks = user.subscribe();
        let first = raise(&user, "first?");
        let id1 = next_opened(&mut asks).await;
        let second = raise(&user, "second?");
        let id2 = next_opened(&mut asks).await;
        assert!(id1 < id2, "ask ids are handed out in raising order: {id1} then {id2}");
        (user, first, second, vec![id1, id2])
    }

    async fn next_opened(asks: &mut broadcast::Receiver<AskEvent>) -> u64 {
        match asks.recv().await.unwrap() {
            AskEvent::Opened(a) => a.ask_id(),
            _ => panic!("expected Opened"),
        }
    }

    fn raise(user: &Arc<WebUser>, prompt: &'static str) -> tokio::task::JoinHandle<Option<String>> {
        let user = user.clone();
        let cancel = CancellationToken::new();
        tokio::spawn(async move { user.choose(prompt, &[choice("a"), choice("b")], &cancel).await })
    }
}
