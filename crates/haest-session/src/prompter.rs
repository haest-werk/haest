//! [`SessionPrompter`]: approval asks put to whoever drives the session.

use async_trait::async_trait;
use haest_mods::{ApprovalAnswer, ApprovalRequest, Prompter};
use haest_wire::Answer;

use crate::{Hub, hub::AskInfo};

/// Approval asks over the session: each is an `approval_ask` frame, and the
/// first `approval_answer` from a peer with `command` access answers it.
///
/// A [`Prompter`] for haest-mods' `ApprovalGate`, which keeps the rules: an
/// ALWAYS covers the same tool and key, an ask nobody answers is a deny after
/// the timeout, an interrupt is a deny. When the gate gives up on an ask, the
/// stream gets `approval_withdrawn` and a late answer is refused.
///
/// Made from the [`Hub`] before the conversation is built, since the runtime
/// takes its prompter at build time.
///
/// ```
/// use std::time::Duration;
///
/// use haest_mods::{ApprovalAnswer, ApprovalRequest, Prompter};
/// use haest_session::{Body, Hub, HubOptions, SessionPrompter};
/// use haest_wire::{Answer, Outbound};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let hub = Hub::new("s1", HubOptions::default());
/// let prompter = SessionPrompter::new(hub.clone());
/// let mut peer = hub.subscribe(0);
///
/// let req = ApprovalRequest {
///     tool_name: "Bash".into(),
///     description: "delete the build dir".into(),
///     input_preview: "rm -rf build".into(),
///     tool_use_id: None,
///     key: Some("rm -rf build".into()),
/// };
/// let ask = tokio::spawn(async move { prompter.prompt(&req).await });
///
/// // The peer sees the ask and answers it.
/// let frame = peer.recv().await.unwrap();
/// let Body::Wire(Outbound::ApprovalAsk { ask: n, .. }) = frame.body else { panic!() };
/// hub.answer(n, Answer::Always).unwrap();
/// assert_eq!(ask.await.unwrap(), ApprovalAnswer::AllowAlways);
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SessionPrompter {
    hub: Hub,
}

impl SessionPrompter {
    /// Asks published into `hub`, answered through it.
    pub fn new(hub: Hub) -> Self {
        Self { hub }
    }
}

/// Withdraws the ask when the prompt's future is dropped unanswered.
struct Pending<'a> {
    hub: &'a Hub,
    ask: u64,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.hub.withdraw(self.ask);
    }
}

#[async_trait]
impl Prompter for SessionPrompter {
    async fn prompt(&self, req: &ApprovalRequest) -> ApprovalAnswer {
        let info = AskInfo {
            tool_name: req.tool_name.clone(),
            description: req.description.clone(),
            preview: req.input_preview.clone(),
            key: req.key.clone(),
        };
        let (ask, answer) = self.hub.ask(info);
        let _pending = Pending {
            hub: &self.hub,
            ask,
        };
        match answer.await {
            Ok(Answer::Allow) => ApprovalAnswer::Allow,
            Ok(Answer::Always) => ApprovalAnswer::AllowAlways,
            // Denied, or the hub closed with the ask open.
            Ok(Answer::Deny) | Err(_) => ApprovalAnswer::Deny,
        }
    }

    /// An ask an earlier ALWAYS answered: a notice, so a peer's record of
    /// what ran stays complete.
    fn auto_approved(&self, req: &ApprovalRequest) {
        let text = format!("Allowed {} (always): {}", req.tool_name, req.description);
        self.hub.notice(haest_wire::Level::Info, text);
    }
}
