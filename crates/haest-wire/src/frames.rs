//! [`Outbound`]: the frames the wire adds beside the conversation's events,
//! and the words they use.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::words::word_enum;

word_enum! {
    /// What a peer may do: watch the stream, or also send turns, control verbs
    /// and approval answers. The host decides it per transport; a frame cannot
    /// raise it.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub enum Access ("access") {
        /// Read only: `snapshot`, `history`, `events`.
        #[default]
        Watch = "watch",
        /// Everything `watch` may, and `turn`, `control`, `approval_answer`.
        Command = "command",
    }
}

word_enum! {
    /// Who a peer's turns count as, for the gate that holds turns nobody asked
    /// for (the `max_agent_turns` setting). The host decides it per transport.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub enum Origin ("origin") {
        /// A person: their turn starts the gate's count over.
        #[default]
        Human = "human",
        /// Another program or agent: its turns count against the gate.
        Agent = "agent",
    }
}

word_enum! {
    /// How much a [`Outbound::Notice`] matters. More levels may come: a
    /// `match` has a `_` arm.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[non_exhaustive]
    pub enum Level ("level") {
        /// Needs nothing from the human.
        Info = "info",
        /// Something was skipped or held, and the run goes on.
        Warning = "warning",
        /// Something failed.
        Error = "error",
    }
}

word_enum! {
    /// Whether a turn is running.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub enum State ("state") {
        /// No turn runs: a turn sent now starts at once (unless the host holds it).
        #[default]
        Idle = "idle",
        /// A turn runs: a turn sent now joins it at its next step.
        Running = "running",
    }
}

/// The conversation now: what a [`Outbound::Status`] frame and a `snapshot`
/// reply carry.
///
/// ```
/// use haest_wire::{Snapshot, State};
///
/// let s = Snapshot { model_spec: "anthropic:claude-opus-5-5".into(), ..Snapshot::default() };
/// let json = serde_json::to_value(&s).unwrap();
/// assert_eq!((json["state"].as_str(), json["lastSeq"].as_u64()), (Some("idle"), Some(0)));
/// assert_eq!(json["model"], "anthropic:claude-opus-5-5");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Whether a turn is running.
    pub state: State,
    /// The session file's id now (a cleared conversation has a new one).
    pub session: String,
    /// The model, `provider:model` (the key `model`).
    #[serde(rename = "model")]
    pub model_spec: String,
    /// The reasoning effort's word; None = the model's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// What the conversation has cost so far, when its models are priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd: Option<f64>,
    /// Turns waiting to run.
    pub queued: usize,
    /// Messages in the history.
    pub messages: usize,
    /// The seq of the last stream frame.
    pub last_seq: u64,
}

/// The frames the wire adds beside the conversation's events. Tagged by
/// `type` (snake_case), fields camelCase, as the events are.
///
/// The first seven are stream frames; [`Hello`](Self::Hello),
/// [`Gap`](Self::Gap) and [`Reply`](Self::Reply) belong to one connection
/// (the [crate docs](crate) say what that means for their `seq`). A new frame
/// type is additive on the wire, so a `match` has a `_` arm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum Outbound {
    /// A line for the human: haest's notices, warnings and errors.
    Notice {
        /// How much it matters.
        level: Level,
        /// The line.
        text: String,
    },
    /// A question a terminal asks the human, answered there.
    Ask {
        /// The question.
        text: String,
    },
    /// The conversation's state changed: a turn started or ended, the model,
    /// effort or session changed, a turn was queued.
    Status(Snapshot),
    /// A tool call waits for approval; a peer with `command` access answers
    /// it with `approval_answer`.
    ApprovalAsk {
        /// The ask's number, for the answer.
        ask: u64,
        /// The tool that asks.
        tool_name: String,
        /// What the call would do.
        description: String,
        /// Its input, shortened for display.
        preview: String,
        /// What an `always` answer covers: the same tool with this key (a
        /// shell command's exact text). None: the description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    /// The ask is withdrawn (the turn was interrupted, or it timed out): an
    /// answer now goes nowhere.
    ApprovalWithdrawn {
        /// The ask's number.
        ask: u64,
    },
    /// A subagent's turn is about to run; its path holds its id.
    SubagentStarted {
        /// Its address, as the agent tools name it (unique while it runs).
        name: String,
        /// The agent that started it.
        parent: String,
        /// What it was asked to do, in short.
        description: String,
    },
    /// A subagent's turn ended; its path holds its id.
    SubagentStopped {
        /// It ended with an answer: not stopped, not out of budget, no error.
        ok: bool,
    },
    /// The first frame on a connection: who this is, and what the peer may do.
    Hello {
        /// The session file's id.
        session: String,
        /// The model, `provider:model` (the key `model`).
        #[serde(rename = "model")]
        model_spec: String,
        /// The process id of the conversation.
        pid: u32,
        /// What this connection may do.
        access: Access,
        /// A credential the other end checks (a broker); None elsewhere.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    /// Frames this connection should have had are lost: it was too slow, or
    /// the ring no longer holds them. Ask `history` to catch up.
    Gap {
        /// The last seq known good before the hole.
        after: u64,
        /// How many frames were lost, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        missed: Option<u64>,
    },
    /// The answer to a request, with its `id`.
    Reply {
        /// The request's id.
        id: String,
        /// Whether it was done.
        ok: bool,
        /// What it gave back, if anything.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        /// Why it was refused or failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl Outbound {
    /// A reply that did it, giving back `result` (if any).
    pub fn done(id: impl Into<String>, result: Option<Value>) -> Self {
        Outbound::Reply {
            id: id.into(),
            ok: true,
            result,
            error: None,
        }
    }

    /// A reply that refuses, saying why.
    pub fn refused(id: impl Into<String>, error: impl std::fmt::Display) -> Self {
        Outbound::Reply {
            id: id.into(),
            ok: false,
            result: None,
            error: Some(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Envelope, Framer, Path};

    /// Every frame reads back from its line as the same frame.
    #[test]
    fn every_outbound_frame_reads_back_from_its_line() {
        let frames = [
            Outbound::Notice {
                level: Level::Warning,
                text: "n".into(),
            },
            Outbound::Ask { text: "a".into() },
            Outbound::Status(Snapshot {
                effort: Some("high".into()),
                usd: Some(0.25),
                ..Snapshot::default()
            }),
            Outbound::ApprovalAsk {
                ask: 3,
                tool_name: "Bash".into(),
                description: "rm -rf build".into(),
                preview: "rm -rf build".into(),
                key: Some("rm -rf build".into()),
            },
            Outbound::ApprovalWithdrawn { ask: 3 },
            Outbound::SubagentStarted {
                name: "reader".into(),
                parent: "main".into(),
                description: "d".into(),
            },
            Outbound::SubagentStopped { ok: true },
            Outbound::Hello {
                session: "x".into(),
                model_spec: "m".into(),
                pid: 1,
                access: Access::Command,
                token: None,
            },
            Outbound::Gap {
                after: 4,
                missed: Some(2),
            },
            Outbound::done("r1", Some(serde_json::json!({"n": 1}))),
            Outbound::refused("r2", "no"),
        ];
        let mut framer = Framer::new("s");
        for frame in frames {
            let line = serde_json::to_string(&framer.stamp(Path::main(), frame.clone())).unwrap();
            let back: Envelope<Outbound> = serde_json::from_str(&line).unwrap();
            assert_eq!(back.body, frame, "{line}");
        }
    }

    /// The tag and the field names are the event's: snake_case type, camelCase fields.
    #[test]
    fn frames_use_the_events_spelling() {
        let ask = Outbound::ApprovalAsk {
            ask: 1,
            tool_name: "Bash".into(),
            description: "d".into(),
            preview: "p".into(),
            key: None,
        };
        let json = serde_json::to_value(&ask).unwrap();
        assert_eq!(json["type"], "approval_ask");
        assert_eq!(json["toolName"], "Bash");
        assert!(json.get("key").is_none());
    }
}
