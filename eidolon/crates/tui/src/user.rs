//! `UserIo` for the TUI: every question becomes a dialog request on the
//! UI channel, carrying a oneshot the UI thread answers.

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use eidolon_core::user::{Choice, UserIo};

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
    ) -> Option<String> {
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
    ) -> Option<String> {
        self.request(DialogKind::Choose, prompt, options.to_vec(), cancel)
            .await
    }
    async fn confirm(&self, prompt: &str, cancel: &CancellationToken) -> bool {
        matches!(
            self.request(DialogKind::Confirm, prompt, Vec::new(), cancel)
                .await
                .as_deref(),
            Some("yes")
        )
    }
}
