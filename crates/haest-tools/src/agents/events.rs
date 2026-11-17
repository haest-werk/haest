//! What the hub tells its parent about a subagent ([`SubagentEvent`]), and a
//! message between agents ([`AgentMessage`]) and who wrote it ([`Sender`]).

use haest_core::{LoopEvent, tool::InnerCallEvent};

/// Who wrote an [`AgentMessage`].
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
    /// person ([`SubagentHub::deliver`](super::SubagentHub::deliver)).
    /// Keyed by the receiving subagent's id; one for main by the sending
    /// subagent's, and one from the person to main by [`MAIN_AGENT`](super::MAIN_AGENT).
    /// A refused message fires nothing.
    Message(
        /// The message as it was sent, without the header the receiver reads.
        AgentMessage,
    ),
}

/// A message for an agent of the conversation: who wrote it, which agent it
/// is for, and what it says.
///
/// SendMessage hands one from the agent that called it to
/// [`AgentDirectory::deliver`](super::AgentDirectory::deliver); a host lets
/// the person write to an agent with one from [`Sender::Person`]
/// ([`SubagentHub::deliver`](super::SubagentHub::deliver)). Its fields are
/// named, so the sender and the receiver cannot trade places unseen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMessage {
    /// Who wrote it.
    pub from: Sender,
    /// The agent it is for: [`MAIN_AGENT`](super::MAIN_AGENT) or a subagent's name.
    pub to: String,
    /// What it says.
    pub text: String,
}
