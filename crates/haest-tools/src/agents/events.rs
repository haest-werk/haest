//! What the hub tells its parent about a subagent ([`SubagentEvent`]), and who
//! wrote a message ([`Sender`]).

use haest_core::{LoopEvent, tool::InnerCallEvent};

/// Who wrote a message ([`SubagentHub::deliver_as`](super::SubagentHub::deliver_as)).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Sender {
    /// An agent of the conversation, by its address ([`MAIN_AGENT`](super::MAIN_AGENT)
    /// or a subagent's name): SendMessage.
    Agent(String),
    /// The person the conversation works for, by the name the host shows for
    /// them.
    Person(String),
}

impl Sender {
    /// The line the receiver reads above the text.
    pub(super) fn header(&self) -> String {
        match self {
            Self::Agent(name) => format!("[Message from agent \"{name}\"]"),
            Self::Person(name) => format!("[Message from {name}, the person you work for]"),
        }
    }
}

/// A subagent's life, for the host's transcript (its own card, never the parent's).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SubagentEvent {
    /// Its turn is about to run.
    Started {
        /// Its address.
        name: String,
        /// The agent that started it.
        parent: String,
        /// The Agent call's `description`.
        description: String,
    },
    /// One event of its turn.
    Loop(
        /// The event, as the subagent's own loop emitted it.
        LoopEvent,
    ),
    /// A tool it ran made a call of its own (Code).
    InnerCall(
        /// The inner call, as the subagent's tool reported it (its
        /// `parent_id` is the id of that tool's call).
        InnerCallEvent,
    ),
    /// Its turn ended.
    Stopped {
        /// It ended its turn with an answer: not stopped, not out of budget,
        /// no error.
        ok: bool,
    },
    /// A message reached an agent of the conversation: SendMessage, or the
    /// person ([`SubagentHub::deliver_as`](super::SubagentHub::deliver_as)).
    /// Keyed by the receiving subagent's id; one for main by the sending
    /// subagent's, and one from the person to main by [`MAIN_AGENT`](super::MAIN_AGENT).
    /// A refused message fires nothing.
    Message {
        /// Who wrote it.
        from: Sender,
        /// Its receiver: [`MAIN_AGENT`](super::MAIN_AGENT) or a subagent's name.
        to: String,
        /// What they wrote, without the header the receiver reads.
        text: String,
    },
}
