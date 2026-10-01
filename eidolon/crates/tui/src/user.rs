//! `UserIo` for the TUI: every question becomes a dialog request on the
//! UI channel, carrying a oneshot the UI thread answers.

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use eidolon_core::user::{Choice, Consent, Reply, UserIo};

use crate::app::{DialogRequest, UiMsg};
use crate::state::DialogKind;

pub struct TuiUser {
    tx: mpsc::UnboundedSender<UiMsg>,
}

impl TuiUser {
    pub fn new(tx: mpsc::UnboundedSender<UiMsg>) -> Self {
        TuiUser { tx }
    }

    async fn request(
        &self,
        kind: DialogKind,
        prompt: &str,
        options: Vec<Choice>,
        cancel: &CancellationToken,
    ) -> Option<Reply> {
        let (reply, rx) = oneshot::channel();
        let req = DialogRequest {
            kind,
            prompt: prompt.to_string(),
            options,
            reply,
        };
        if self.tx.send(UiMsg::Dialog(req)).is_err() {
            return None;
        }
        tokio::select! {
            r = rx => r.ok().flatten(),
            _ = cancel.cancelled() => None,
        }
    }
}

#[async_trait]
impl UserIo for TuiUser {
    async fn choose(
        &self,
        prompt: &str,
        options: &[Choice],
        cancel: &CancellationToken,
    ) -> Option<Reply> {
        self.request(DialogKind::Choose, prompt, options.to_vec(), cancel)
            .await
    }
    async fn confirm(&self, prompt: &str, cancel: &CancellationToken) -> Consent {
        self.request(DialogKind::Confirm, prompt, Vec::new(), cancel).await.into()
    }
    async fn respond(&self, prompt: &str, cancel: &CancellationToken) -> Option<String> {
        // The text hop's answer is its field: a `Reply` with no note, since
        // the words said *are* the answer.
        self.request(DialogKind::Respond, prompt, Vec::new(), cancel)
            .await
            .map(|r| r.label)
    }
}
