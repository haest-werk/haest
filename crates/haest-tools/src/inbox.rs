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
/// default [`SubagentHub`](crate::SubagentHub) (a message for main, a
/// background answer for main). An `UnboundedSender<String>` is one: the host
/// drains the receiver.
pub trait Inbox: Send + Sync {
    /// Leave `text`, the whole user message of a turn to run: a message for
    /// main (SendMessage, the person), a job that exited. It should return at
    /// once: it is called from inside a tool call or a job's task.
    fn submit(&self, text: String);
    /// Leave a background subagent's answer for main: main started it, or its
    /// starter finished first. A host that holds answers until every subagent
    /// is done (one turn for all of them, not one per answer) while a message
    /// goes through at once tells the two apart here. Default:
    /// [`submit`](Self::submit).
    fn submit_answer(&self, text: String) {
        self.submit(text);
    }
}

haest_core::plug_ref! {
    /// An [`Inbox`], shared: what a [`JobHub`](crate::JobHub) and a
    /// [`SubagentHub`](crate::SubagentHub) hold. Built from the inbox itself,
    /// or from an `Arc` of one when the host keeps it too.
    pub struct InboxRef(dyn Inbox);
}

/// The queue form: the host drains the receiving end. A closed receiver drops
/// the text.
impl Inbox for UnboundedSender<String> {
    fn submit(&self, text: String) {
        let _ = self.send(text);
    }
}

/// Nowhere: for a host that hears about exits some other way (and tells the model so itself).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoInbox;

impl Inbox for NoInbox {
    fn submit(&self, _text: String) {}
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::unbounded_channel;

    use super::Inbox;

    #[test]
    fn an_inbox_that_only_submits_gets_answers_through_submit() {
        let (tx, mut rx) = unbounded_channel();
        tx.submit("a message".into());
        tx.submit_answer("an answer".into());
        assert_eq!(rx.try_recv().as_deref(), Ok("a message"));
        assert_eq!(rx.try_recv().as_deref(), Ok("an answer"));
    }
}
