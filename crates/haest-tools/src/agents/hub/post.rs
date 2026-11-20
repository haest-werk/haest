//! The hub's post office: whose card a message to main goes on, and a
//! background answer on its way back.

use super::SubagentHub;
use crate::agents::{MAIN_AGENT, Sender, members::Status};

impl SubagentHub {
    /// Whose card a message to main goes on: its sender's, or main's.
    pub(super) fn id_of(&self, sender: &Sender) -> String {
        let Sender::Agent(name) = sender else {
            return MAIN_AGENT.to_string();
        };
        let mut members = self.lock_members();
        members
            .get_mut(name)
            .map_or_else(|| MAIN_AGENT.to_string(), |m| m.id.clone())
    }

    /// A background agent's answer goes to whoever started it -- or to main
    /// ([`Inbox::submit_answer`](crate::Inbox::submit_answer)) when main
    /// started it or its starter has finished meanwhile. Never fails: an
    /// answer must not be lost to a race.
    pub(super) fn hand_back(&self, parent: &str, text: String) {
        let mut members = self.lock_members();
        match members.get_mut(parent) {
            Some(p) if p.status == Status::Running => p.steer(text),
            _ => {
                drop(members);
                self.0.parent.submit_answer(text);
            }
        }
    }
}
