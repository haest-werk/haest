//! The hub's post office: a message from an agent or the person, and a
//! background answer on its way back.

use super::SubagentHub;
use crate::agents::{AgentError, AgentMessage, MAIN_AGENT, Sender, SubagentEvent, members::Status};

impl SubagentHub {
    /// Hand `message` to the agent it is for, main or a running subagent,
    /// and say what to tell its sender. A subagent reads it at its next
    /// step; one for main goes to the parent's
    /// [`Inbox::submit`](crate::Inbox::submit). The receiver reads who wrote
    /// above the text: SendMessage sends from [`Sender::Agent`]
    /// ([`AgentDirectory::deliver`](super::AgentDirectory::deliver)), and a
    /// host lets the person write with [`Sender::Person`].
    ///
    /// Every delivery tells the parent ([`SubagentEvent::Message`]); a
    /// subagent that has finished, or never ran here, refuses the message and
    /// nothing is told.
    ///
    /// # Errors
    ///
    /// [`AgentError::Finished`] or [`AgentError::UnknownAgent`], with who a
    /// message can reach now.
    pub fn deliver(&self, message: AgentMessage) -> Result<String, AgentError> {
        let AgentMessage { from, to, text } = &message;
        let body = format!("{}\n\n{text}", from.header());
        let (id, said) = if to == MAIN_AGENT {
            self.0.parent.submit(body);
            (self.id_of(from), "Delivered to main.".to_string())
        } else {
            let mut members = self.lock_members();
            let Some(m) = members.get_mut(to).filter(|m| m.status == Status::Running) else {
                let (name, running) = (to.to_string(), members.reachable());
                return Err(if members.known(to) {
                    AgentError::Finished { name, running }
                } else {
                    AgentError::UnknownAgent { name, running }
                });
            };
            m.steer(body);
            let said = format!("Delivered to \"{to}\"; it reads it at its next step.");
            (m.id.clone(), said)
        };
        self.0
            .parent
            .on_subagent(&id, SubagentEvent::Message(message));
        Ok(said)
    }

    /// Whose card a message to main goes on: its sender's, or main's.
    fn id_of(&self, sender: &Sender) -> String {
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
