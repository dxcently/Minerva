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
//! Any answer may carry a `note`.
//!
//! ```text
//!   {"type":"ask-settled","ask_id":3,"how":"answered","answer":"yes"}
//!   {"type":"ask-settled","ask_id":3,"how":"answered","answer":"no","note":"not on main"}
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
use eidolon_core::user::{Choice, Consent, Reply, UserIo, Words, WordsRefused};

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

/// A `POST /api/answer` body's `answer` and `note` as a [`Reply`]; a blank note is no note. `Err` is the 422's reason.
pub fn reply(answer: String, note: Option<String>) -> Result<Reply, String> {
    match note.as_deref().map(Words::new) {
        None | Some(Err(WordsRefused::Blank)) => Ok(Reply::picked(answer)),
        Some(Ok(note)) => Ok(Reply { label: answer, operator_note: Some(note) }),
        Some(Err(why)) => Err(format!("note {why}")),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settled {
    Answered(Reply),
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

    pub(crate) fn reply(&self) -> Option<&Reply> {
        match self {
            Settled::Answered(r) => Some(r),
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
    reply: oneshot::Sender<Option<Reply>>,
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

    pub fn answer(&self, ask_id: u64, reply: Reply) -> Answer {
        let entry = {
            let mut pending = self.pending.lock().unwrap();
            let Some(entry) = pending.get(&ask_id) else { return Answer::Unknown };
            if !entry.ask.accepts(&reply.label) {
                return Answer::Rejected;
            }
            pending.remove(&ask_id).expect("just looked it up")
        };
        let _ = self.asks.send(AskEvent::Settled {
            ask_id,
            how: Settled::Answered(reply.clone()),
        });
        // Fails only if the waiter's cancel fired first; nobody is left to tell.
        let _ = entry.reply.send(Some(reply));
        Answer::Accepted
    }

    async fn ask(&self, ask: Ask, cancel: &CancellationToken) -> Option<Reply> {
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
    async fn choose(&self, prompt: &str, options: &[Choice], cancel: &CancellationToken) -> Option<Reply> {
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
    async fn approve(&self, call: &ToolCall, _manifest: &ToolManifest, ruling: &Ruling, cancel: &CancellationToken) -> Consent {
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
        self.ask(ask, cancel).await.into()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use eidolon_core::policy::Approval;
    use eidolon_core::tool::CallOrigin;
    use eidolon_core::user::MAX_WORDS_BYTES;

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
            vouched: None,
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
        assert_eq!(user.answer(1, Reply::picked("yes")), Answer::Accepted);
        assert!(approving.await.unwrap().given);
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
        assert_eq!(user.answer(1, Reply::picked("yes")), Answer::Accepted);
        assert!(confirming.await.unwrap().given);
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
        assert_eq!(user.answer(ask_id, Reply::picked("b")), Answer::Accepted);
        assert_eq!(asked.await.unwrap(), Some(Reply::picked("b")));
        assert_eq!(user.answer(ask_id, Reply::picked("a")), Answer::Unknown);
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
        assert_eq!(user.answer(ask_id, Reply::picked("c")), Answer::Rejected);
        assert_eq!(user.pending_count(), 1, "a refused answer settles nothing");
        assert!(!asked.is_finished(), "the ask is still waiting");
        assert_eq!(user.answer(ask_id, Reply::picked("a")), Answer::Accepted);
        assert_eq!(asked.await.unwrap(), Some(Reply::picked("a")));
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
        assert_eq!(user.answer(ask_id, Reply::picked("maybe")), Answer::Rejected);
        assert_eq!(user.pending_count(), 1);
        assert_eq!(user.answer(ask_id, Reply::picked("no")), Answer::Accepted);
        assert!(!approving.await.unwrap().given, "an approval only ever says yes to `yes`");
    }

    #[tokio::test]
    async fn two_pending_asks_each_resolve_only_by_their_own_id_oldest_first() {
        let (user, first, second, ids) = two_open_asks().await;

        assert_eq!(user.answer(ids[0], Reply::picked("a")), Answer::Accepted);
        assert_eq!(first.await.unwrap(), Some(Reply::picked("a")));
        assert!(!second.is_finished(), "the first answer must not resolve the second ask");
        assert_eq!(user.pending_count(), 1);

        assert_eq!(user.answer(ids[1], Reply::picked("b")), Answer::Accepted);
        assert_eq!(second.await.unwrap(), Some(Reply::picked("b")));
        assert_eq!(user.pending_count(), 0);
    }

    #[tokio::test]
    async fn two_pending_asks_each_resolve_only_by_their_own_id_newest_first() {
        let (user, first, second, ids) = two_open_asks().await;

        assert_eq!(user.answer(ids[1], Reply::picked("b")), Answer::Accepted);
        assert_eq!(second.await.unwrap(), Some(Reply::picked("b")));
        assert!(!first.is_finished(), "the second answer must not resolve the first ask");
        assert_eq!(user.pending_count(), 1);

        assert_eq!(user.answer(ids[0], Reply::picked("a")), Answer::Accepted);
        assert_eq!(first.await.unwrap(), Some(Reply::picked("a")));
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
        assert!(!task.await.unwrap().given);
        match asks.recv().await.unwrap() {
            AskEvent::Settled { ask_id: settled, how } => {
                assert_eq!(settled, ask_id);
                assert_eq!(how, Settled::Cancelled);
            }
            _ => panic!("expected Settled"),
        }
        assert_eq!(user.pending_count(), 0);
        assert_eq!(user.answer(ask_id, Reply::picked("yes")), Answer::Unknown, "a cancelled ask is nobody's to answer");
    }

    fn words(text: &str) -> Words {
        Words::new(text).unwrap()
    }

    #[tokio::test]
    async fn an_approval_answered_with_a_note_hands_the_note_to_the_dispatcher_and_the_settle() {
        let user = Arc::new(WebUser::new());
        let mut asks = user.subscribe();
        let cancel = CancellationToken::new();
        let manifest = ToolManifest::synthetic("note", Approval::Mutating);
        let approving = tokio::spawn({
            let user = user.clone();
            let cancel = cancel.clone();
            async move { user.approve(&note_call(), &manifest, &asking_ruling(), &cancel).await }
        });
        let ask_id = next_opened(&mut asks).await;
        let reply = Reply { label: "no".into(), operator_note: Some(words("not on main")) };
        assert_eq!(user.answer(ask_id, reply.clone()), Answer::Accepted);
        assert_eq!(approving.await.unwrap(), Consent { given: false, operator_note: Some(words("not on main")) });
        match asks.recv().await.unwrap() {
            AskEvent::Settled { how, .. } => assert_eq!(how, Settled::Answered(reply)),
            _ => panic!("expected Settled"),
        }
    }

    #[test]
    fn an_answer_without_a_note_is_the_bare_pick() {
        assert_eq!(reply("b".into(), None), Ok(Reply::picked("b")));
    }

    #[test]
    fn a_note_is_trimmed_and_a_blank_one_is_no_note() {
        assert_eq!(
            reply("yes".into(), Some("  once\n\tonly \n".into())),
            Ok(Reply { label: "yes".into(), operator_note: Some(words("once\n\tonly")) })
        );
        assert_eq!(reply("yes".into(), Some(" \n ".into())), Ok(Reply::picked("yes")));
    }

    #[test]
    fn a_refused_note_is_named_with_the_core_reason() {
        let long = "x".repeat(MAX_WORDS_BYTES + 1);
        assert_eq!(reply("a".into(), Some(long)), Err(format!("note {}", WordsRefused::OverLong)));
        assert_eq!(reply("a".into(), Some("bad\0note".into())), Err(format!("note {}", WordsRefused::ControlCharacter)));
    }

    /// Each `Opened` is awaited before the next raise, so which handle holds
    /// which id does not depend on the scheduler.
    async fn two_open_asks() -> (
        Arc<WebUser>,
        tokio::task::JoinHandle<Option<Reply>>,
        tokio::task::JoinHandle<Option<Reply>>,
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

    fn raise(user: &Arc<WebUser>, prompt: &'static str) -> tokio::task::JoinHandle<Option<Reply>> {
        let user = user.clone();
        let cancel = CancellationToken::new();
        tokio::spawn(async move { user.choose(prompt, &[choice("a"), choice("b")], &cancel).await })
    }
}
