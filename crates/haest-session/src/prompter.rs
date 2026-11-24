//! [`SessionPrompter`]: approval asks put to whoever drives the session.

use async_trait::async_trait;
use haest_mods::{ApprovalAnswer, ApprovalRequest, Prompter};
use haest_wire::{Answer, Answerer};
use tokio::sync::oneshot;

use crate::{Hub, hub::AskInfo};

/// Approval asks over the session: each is an `approval_ask` frame, and the
/// first `approval_answer` from a peer with `command` access answers it.
///
/// A [`Prompter`] for haest-mods' `ApprovalGate`, which keeps the rules: an
/// ALWAYS covers the same tool and key, an ask nobody answers is a deny after
/// the timeout, an interrupt is a deny. When the gate gives up on an ask, the
/// stream gets `approval_withdrawn` and a late answer is refused; an answered
/// ask gets `approval_answered`.
///
/// Made from the [`Hub`] before the conversation is built, since the runtime
/// takes its prompter at build time. A host that also asks the human itself
/// (a terminal) races the two with [`open`](Self::open).
///
/// ```
/// use std::time::Duration;
///
/// use haest_mods::{ApprovalAnswer, ApprovalRequest, Prompter};
/// use haest_session::{Body, Hub, SessionPrompter};
/// use haest_wire::{Answer, Outbound};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// let hub = Hub::new("s1");
/// let prompter = SessionPrompter::new(hub.clone());
/// let mut peer = hub.subscribe(0);
///
/// let req = ApprovalRequest::new("Bash", "delete the build dir")
///     .with_input_preview("rm -rf build")
///     .with_key("rm -rf build");
/// let ask = tokio::spawn(async move { prompter.prompt(&req).await });
///
/// // The peer sees the ask and answers it.
/// while let Some(frame) = peer.recv().await {
///     if let Body::Wire(Outbound::ApprovalAsk { ask: n, .. }) = frame.body {
///         hub.answer(n, Answer::Always)?;
///         break;
///     }
/// }
/// assert_eq!(ask.await?, ApprovalAnswer::AllowAlways);
/// # Ok(())
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

    /// Puts `req` to the session's peers (an `approval_ask` frame) and hands
    /// back the ask, open: for a host that also asks the human on its own
    /// side and takes the first answer ([`OpenAsk`] shows how).
    pub fn open(&self, req: &ApprovalRequest) -> OpenAsk {
        let info = AskInfo {
            tool_name: req.tool_name.clone(),
            tool_call_id: req.tool_use_id.clone(),
            description: req.description.clone(),
            preview: req.input_preview.clone(),
            key: req.key.clone(),
        };
        let (ask, answer) = self.hub.ask(info);
        OpenAsk {
            hub: self.hub.clone(),
            ask,
            answer,
        }
    }
}

/// An approval ask open on the session ([`SessionPrompter::open`]): a peer
/// may answer it, or the host's own side ([`answered_at_terminal`](Self::answered_at_terminal)).
/// The first answer wins, and the stream says whose it was
/// (`approval_answered`). Dropped unanswered (the gate gave up on it), it is
/// withdrawn (`approval_withdrawn`).
///
/// ```
/// use haest_mods::{ApprovalAnswer, ApprovalRequest};
/// use haest_session::{Body, Hub, SessionPrompter};
/// use haest_wire::{Answerer, Outbound};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let hub = Hub::new("s1");
/// let mut peer = hub.subscribe(0);
/// let mut open = SessionPrompter::new(hub.clone()).open(&ApprovalRequest::new("Bash", "rm -rf build"));
///
/// // The human at the terminal answers first (a peer's answer would end `peer_answer`).
/// let typed = async { ApprovalAnswer::Deny };
/// let answer = tokio::select! {
///     peer = open.peer_answer() => peer,
///     typed = typed => open.answered_at_terminal(typed),
/// };
/// assert_eq!(answer, ApprovalAnswer::Deny);
///
/// // A peer reads who answered.
/// let mut by = None;
/// while let Some(frame) = peer.recv().await {
///     if let Body::Wire(Outbound::ApprovalAnswered { by: who, .. }) = frame.body {
///         by = Some(who);
///         break;
///     }
/// }
/// assert_eq!(by, Some(Answerer::Terminal));
/// # }
/// ```
#[derive(Debug)]
pub struct OpenAsk {
    hub: Hub,
    ask: u64,
    answer: oneshot::Receiver<Answer>,
}

impl OpenAsk {
    /// Its number: the `approval_ask` frame's `ask`, which a terminal
    /// question on the same ask names as its `approval`.
    pub fn ask(&self) -> u64 {
        self.ask
    }

    /// A peer's answer. A deny when the session closed with the ask open, or
    /// the answer is one this prompter does not know (it fails closed).
    /// Cancel safe: dropped before it returns, it can be awaited again.
    pub async fn peer_answer(&mut self) -> ApprovalAnswer {
        match (&mut self.answer).await {
            Ok(answer) => from_wire(answer),
            Err(_) => ApprovalAnswer::Deny,
        }
    }

    /// The human answered on the host's own side: the stream says
    /// `approval_answered` by the terminal, and a peer's answer now is
    /// refused. Returns the answer that counts: `answer`, or a peer's that
    /// came first (then the stream already says so, and `answer` is dropped).
    pub fn answered_at_terminal(mut self, answer: ApprovalAnswer) -> ApprovalAnswer {
        if self
            .hub
            .answered(self.ask, to_wire(answer), Answerer::Terminal)
        {
            return answer;
        }
        match self.answer.try_recv() {
            Ok(first) => from_wire(first),
            // Withdrawn or the session closed: nothing may run on it.
            Err(_) => ApprovalAnswer::Deny,
        }
    }
}

impl Drop for OpenAsk {
    /// Unanswered, it is withdrawn; answered, nothing is said again.
    fn drop(&mut self) {
        self.hub.withdraw(self.ask);
    }
}

/// A peer's answer as the gate takes it.
fn from_wire(answer: Answer) -> ApprovalAnswer {
    match answer {
        Answer::Allow => ApprovalAnswer::Allow,
        Answer::Always => ApprovalAnswer::AllowAlways,
        // Denied, or an answer this prompter does not know: it fails closed.
        _ => ApprovalAnswer::Deny,
    }
}

/// The gate's answer as the wire says it.
fn to_wire(answer: ApprovalAnswer) -> Answer {
    match answer {
        ApprovalAnswer::Allow => Answer::Allow,
        ApprovalAnswer::AllowAlways => Answer::Always,
        // Denied, or one the wire has no word for yet: what the gate does
        // with it is a deny.
        _ => Answer::Deny,
    }
}

#[async_trait]
impl Prompter for SessionPrompter {
    async fn prompt(&self, req: &ApprovalRequest) -> ApprovalAnswer {
        self.open(req).peer_answer().await
    }

    /// An ask an earlier ALWAYS answered: a notice, so a peer's record of
    /// what ran stays complete.
    fn auto_approved(&self, req: &ApprovalRequest) {
        let text = format!("Allowed {} (always): {}", req.tool_name, req.description);
        self.hub.notice(haest_wire::Level::Info, text);
    }
}
