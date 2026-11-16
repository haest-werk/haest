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

/// Where the tools leave a turn the human did not start, for the host to run.
///
/// Used by [`JobHub`](crate::JobHub) (a job that exited on its own) and the
/// default [`SubagentHub`](crate::SubagentHub) (a message for main, a late
/// background answer). An `UnboundedSender<String>` is one: the host drains
/// the receiver.
pub trait Inbox: Send + Sync {
    /// Leave `text`, the whole user message of a turn to run. It should return
    /// at once: it is called from inside a tool call or a job's task.
    fn submit(&self, text: String);
}

/// The queue form: the host drains the receiving end. A closed receiver drops
/// the text.
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
