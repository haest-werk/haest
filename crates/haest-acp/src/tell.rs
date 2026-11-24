//! [`Teller`]: how a session's updates reach the client.

use agent_client_protocol::{
    Client,
    v1::{
        ConnectionTo,
        schema::{
            ContentChunk, Notice, NoticeSeverity, SessionId, SessionNotification, SessionUpdate,
        },
    },
};
use haest_wire::Level;

use crate::state::ClientCaps;

/// One session's way to the client: the connection, the session's id, and
/// the optional updates the client takes.
#[derive(Debug, Clone)]
pub(crate) struct Teller {
    pub(crate) conn: ConnectionTo<Client>,
    pub(crate) session: SessionId,
    pub(crate) caps: ClientCaps,
}

impl Teller {
    /// `update` about `session` (this one, or a subagent's child session).
    pub(crate) fn send(&self, session: &SessionId, update: SessionUpdate) {
        // A client that has gone reads nothing more; the session ends with it.
        let _gone = self
            .conn
            .send_notification(SessionNotification::new(session.clone(), update));
    }

    /// `update` about this session.
    pub(crate) fn tell(&self, update: SessionUpdate) {
        self.send(&self.session, update);
    }

    /// A line for the human: a notice where the client takes them, else a
    /// warning or an error as a quoted part of the agent's message. An info
    /// line is left out there: what it says the client shows another way (a
    /// mode, a model), or it is not needed.
    pub(crate) fn notice(&self, level: Level, text: &str) {
        let severity = match level {
            Level::Warning => NoticeSeverity::Warning,
            Level::Error => NoticeSeverity::Error,
            // Info, and a level newer than this crate.
            _ => NoticeSeverity::Info,
        };
        if self.caps.notices {
            return self.tell(SessionUpdate::Notice(Notice::new(severity, text)));
        }
        let label = match severity {
            NoticeSeverity::Warning => "Warning",
            NoticeSeverity::Error => "Error",
            _ => return,
        };
        let quoted: Vec<String> = text.lines().map(|l| format!("> {l}")).collect();
        self.tell(agent_text(format!(
            "\n\n> **{label}:**\n{}\n\n",
            quoted.join("\n")
        )));
    }
}

/// Text of the agent's message.
pub(crate) fn agent_text(text: impl Into<String>) -> SessionUpdate {
    SessionUpdate::AgentMessageChunk(ContentChunk::new(text.into().into()))
}

/// Reasoning of the agent's.
pub(crate) fn agent_thought(text: impl Into<String>) -> SessionUpdate {
    SessionUpdate::AgentThoughtChunk(ContentChunk::new(text.into().into()))
}

/// Text of the user's message (a loaded session's history).
pub(crate) fn user_text(text: impl Into<String>) -> SessionUpdate {
    SessionUpdate::UserMessageChunk(ContentChunk::new(text.into().into()))
}
