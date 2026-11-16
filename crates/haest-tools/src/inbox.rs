//! THE INBOX: where a turn the human did not start goes -- a background job that
//! exited on its own, a subagent's message for the conversation's own agent, a
//! background subagent's answer whose starter has finished.
//!
//! PLUG POINT: the host implements [`Inbox`]. It runs the text as a turn when the
//! agent is idle, or steers it into the turn running
//! ([`haest_core::AgentControl::steer`]); either way it should pass through an
//! [`crate::turn_gate::AgentTurnGate`] first, so two agents (or a job loop)
//! cannot start turns forever.

use tokio::sync::mpsc::UnboundedSender;

pub trait Inbox: Send + Sync {
    fn submit(&self, text: String);
}

/// The queue form: the host drains the receiving end.
impl Inbox for UnboundedSender<String> {
    fn submit(&self, text: String) {
        let _ = self.send(text);
    }
}

/// Nowhere: for a host that hears about exits some other way (and tells the model so itself).
#[derive(Debug, Default)]
pub struct NoInbox;

impl Inbox for NoInbox {
    fn submit(&self, _text: String) {}
}
