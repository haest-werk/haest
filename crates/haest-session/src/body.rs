//! [`Body`]: what a session's frames carry, the conversation's events or the
//! wire's own frames, and how the ring treats each.

use haest_core::LoopEvent;
use haest_wire::{Classify, Envelope, FrameKind, Outbound, State};
use serde::{Deserialize, Serialize};

/// One frame of a session's stream: its envelope and its [`Body`].
pub type Frame = Envelope<Body>;

/// What a session's frame carries. On the wire both kinds are one object with
/// a `type` (the body's fields flattened into the envelope), so a reader tells
/// them apart by `type` alone.
///
/// ```
/// use haest_core::LoopEvent;
/// use haest_session::{Body, Frame};
/// use haest_wire::{Framer, Path};
///
/// let mut framer = Framer::new("s1");
/// let frame: Frame = framer.stamp(Path::main(), Body::Loop(LoopEvent::TextDelta { text: "hi".into() }));
/// let line = serde_json::to_string(&frame)?;
/// let back: Frame = serde_json::from_str(&line)?;
/// assert!(matches!(back.body, Body::Loop(LoopEvent::TextDelta { .. })));
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Body {
    /// One of the conversation's events (a subagent's carry its path).
    Loop(LoopEvent),
    /// One of the wire's own frames: notices, status, approvals, subagents,
    /// a connection's hello, gap and reply.
    Wire(Outbound),
}

impl Body {
    /// The event, when this is one.
    pub fn as_loop(&self) -> Option<&LoopEvent> {
        match self {
            Body::Loop(ev) => Some(ev),
            Body::Wire(_) => None,
        }
    }
}

impl From<LoopEvent> for Body {
    fn from(ev: LoopEvent) -> Self {
        Body::Loop(ev)
    }
}

impl From<Outbound> for Body {
    fn from(frame: Outbound) -> Self {
        Body::Wire(frame)
    }
}

/// A model call's streamed pieces go once its answer is in; a `status` that
/// says a turn runs starts the turn the ring keeps whole, its `turn_end` ends it.
impl Classify for Body {
    fn kind(&self) -> FrameKind {
        match self {
            Body::Loop(
                LoopEvent::TextDelta { .. }
                | LoopEvent::ThinkingDelta { .. }
                | LoopEvent::ToolCallStart { .. },
            ) => FrameKind::Delta,
            Body::Loop(LoopEvent::Assistant { .. }) => FrameKind::Answer,
            Body::Loop(LoopEvent::TurnEnd { .. }) => FrameKind::TurnEnd,
            Body::Wire(Outbound::Status(s)) if s.state == State::Running => FrameKind::TurnStart,
            _ => FrameKind::Other,
        }
    }
}
