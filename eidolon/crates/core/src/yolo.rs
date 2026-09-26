//! Yolo: every question answered yes, by nobody.
//!
//! The gate is built to be quiet and it is quiet, but "quiet" is measured
//! against a model that mostly knows what it is doing. Point a 14B at a
//! directory as a general-purpose tool and the shape of the session
//! changes: it reaches for a subshell to do what one command would do, it
//! writes a file one directory over, it pipes something into something —
//! none of it dangerous, all of it *structural*, and every one of those is
//! a question by construction, because the algebra above the table refuses
//! to pretend it understood a `for` loop. The operator who wanted a swiss
//! army knife is now a keypress in a loop.
//!
//! [`crate::escalate::Judge`] is the considered answer to that and costs an
//! API call per question. This is the other answer, and its whole content
//! is that the operator already made the decision — once, out loud, for the
//! session — so nothing needs to make it again per call.
//!
//! ## What it may do
//!
//! Turn a [`Verdict::Ask`] into a [`Verdict::Allow`]. That is the entire
//! surface.
//!
//! It does **not** touch [`Verdict::Deny`], and that is not timidity: the
//! table denies three kinds of thing — privilege escalation, powering the
//! box down, destroying a filesystem — and none of them is work anybody
//! reaches for a small model to do. A refusal there is not the gate being
//! cautious, it is the gate having recognised something specific, and a
//! switch that also switched *that* off would be doing something the
//! operator arming it was not thinking about. The honest way to have no
//! gate at all is `[policy] enabled = false`, which says so in the config
//! where it can be read, rather than a flag on one launch.
//!
//! Unlike the judge it rewrites the verdict rather than annotating it —
//! there is no asymmetry to preserve here, because there is no model to be
//! wrong. What it leaves alone is the ruling's `reason`: the classifier's
//! own word for what it would have asked about survives onto the record, so
//! the log of a yolo session is still a legible list of the questions
//! nobody was asked.
//!
//! ## Where it sits
//!
//! **Under the judge, not over it.** Both are decorators on the same seam
//! and the order decides who pays: with yolo innermost, an armed switch has
//! already turned the question into an allow by the time the judge looks,
//! and the judge — which only ever acts on an `Ask` — does nothing and
//! calls nothing. Outermost, it would let the model be asked and paid for
//! and then override it. An operator running yolo has said they do not want
//! to spend that.
//!
//! ## Arming
//!
//! [`Switch`] is shared, so the TUI can flip it mid-session from `:yolo`
//! and the hook running inside the dispatcher sees it on the next call. It
//! is *not* journaled and does not survive a resume: it is a posture the
//! operator is holding, like the detail level or the trace cursor, and a
//! reopened session comes back gated. A switch that persisted would be one
//! an operator could leave armed in a log they open again a week later,
//! having forgotten.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use crate::policy::{PolicyHook, Ruling, Verdict};
use crate::tool::{ToolCall, ToolManifest};

/// Armed or not, shared between whoever flips it and the hook that reads it.
///
/// A handle rather than a bool on the hook because the two live on opposite
/// sides of the harness: the switch is thrown on the UI thread, by a
/// keystroke, and read inside [`crate::dispatch::Dispatcher::adjudicate`]
/// on the driver. `Relaxed` is the right ordering for both — nothing else
/// is being published alongside it, and the worst a stale read can do is
/// ask a question the operator had just decided to stop being asked.
#[derive(Clone, Debug)]
pub struct Switch(Arc<AtomicBool>);

impl Switch {
    pub fn new(armed: bool) -> Self {
        Switch(Arc::new(AtomicBool::new(armed)))
    }

    pub fn armed(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set(&self, armed: bool) {
        self.0.store(armed, Ordering::Relaxed);
    }

    /// Flip it, and report where it landed.
    pub fn toggle(&self) -> bool {
        !self.0.fetch_xor(true, Ordering::Relaxed)
    }
}

impl Default for Switch {
    fn default() -> Self {
        Switch::new(false)
    }
}

/// The blanket yes, wrapped around the gate it answers for.
///
/// A decorator for the same reason [`crate::escalate::Judge`] is one: the
/// chokepoint keeps evaluating exactly one hook, and this stays a thing the
/// operator can have or not have.
pub struct Yolo {
    inner: Arc<dyn PolicyHook>,
    switch: Switch,
}

impl Yolo {
    pub fn new(inner: Arc<dyn PolicyHook>, switch: Switch) -> Self {
        Yolo { inner, switch }
    }

    pub fn switch(&self) -> Switch {
        self.switch.clone()
    }
}

#[async_trait]
impl PolicyHook for Yolo {
    async fn pre_tool(&self, call: &ToolCall, manifest: &ToolManifest, cwd: &Path) -> Ruling {
        let ruling = self.inner.pre_tool(call, manifest, cwd).await;
        // Disarmed is not a mode: it is the harness as it was, and it must
        // cost nothing to have this in the stack.
        if !self.switch.armed() || !matches!(ruling.verdict, Verdict::Ask(_)) {
            return ruling;
        }
        Ruling {
            verdict: Verdict::Allow,
            yolo: true,
            ..ruling
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Approval;

    struct Fixed(Ruling);

    #[async_trait]
    impl PolicyHook for Fixed {
        async fn pre_tool(&self, _: &ToolCall, _: &ToolManifest, _: &Path) -> Ruling {
            self.0.clone()
        }
    }

    fn call() -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: "bash".into(),
            input: serde_json::json!({}),
            origin: crate::tool::CallOrigin::Model,
        }
    }

    fn manifest() -> ToolManifest {
        ToolManifest::synthetic("bash", Approval::Mutating)
    }

    async fn under(inner: Ruling, armed: bool) -> Ruling {
        let yolo = Yolo::new(Arc::new(Fixed(inner)), Switch::new(armed));
        yolo.pre_tool(&call(), &manifest(), Path::new("/")).await
    }

    #[tokio::test]
    async fn armed_answers_the_question_and_keeps_the_reason() {
        let asked = Ruling {
            reason: Some("subshell".into()),
            structural: true,
            ..Ruling::ask("run it?")
        };
        let out = under(asked, true).await;
        assert_eq!(out.verdict, Verdict::Allow);
        assert!(out.yolo);
        // The ledger's unit survives: what the gate would have asked about
        // is still on the record, which is the only thing making a yolo
        // session legible afterwards.
        assert_eq!(out.reason.as_deref(), Some("subshell"));
        assert!(out.structural);
    }

    #[tokio::test]
    async fn armed_leaves_a_refusal_alone() {
        let out = under(Ruling::deny("privilege escalation"), true).await;
        assert_eq!(out.verdict, Verdict::Deny("privilege escalation".into()));
        assert!(!out.yolo);
    }

    #[tokio::test]
    async fn disarmed_changes_nothing() {
        let out = under(Ruling::ask("run it?"), false).await;
        assert_eq!(out.verdict, Verdict::Ask("run it?".into()));
        assert!(!out.yolo);
    }

    #[tokio::test]
    async fn an_allow_is_not_a_yolo() {
        let out = under(Ruling::allow(), true).await;
        assert_eq!(out.verdict, Verdict::Allow);
        assert!(
            !out.yolo,
            "nothing was waived; the table said yes on its own"
        );
    }

    #[test]
    fn the_switch_is_shared() {
        let a = Switch::new(false);
        let b = a.clone();
        assert!(a.toggle());
        assert!(b.armed());
        b.set(false);
        assert!(!a.armed());
    }
}
