//! What one turn of the loop emits, in order. A host renders them, a CLI prints
//! them, `--json` writes each as one NDJSON line (the serde shape is stable).

use serde::Serialize;

use crate::{
    messages::{AssistantMessage, StopReason, ToolCall, ToolResultMessage, Usage, UserMessage},
    tool::ToolOutput,
};

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LoopEvent {
    /// A user message entered history (the turn's input, or queued steering).
    User {
        message: UserMessage,
    },
    ModelStart {
        provider: String,
        model: String,
    },
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    /// A tool call began in the model's stream: the text before it was narration.
    ToolCallStart {
        id: String,
        name: String,
    },
    /// The model's complete answer (also a refused call that never went out).
    Assistant {
        message: AssistantMessage,
    },
    ToolResult {
        call: ToolCall,
        message: ToolResultMessage,
        output: ToolOutput,
        duration_ms: u64,
    },
    /// The turn guard nudged the model (the text it was sent); the human must see it too.
    Guard {
        text: String,
    },
    TurnEnd {
        stop_reason: StopReason,
        usage: Usage,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// How a turn ended: the `TurnEnd` event without its tallies.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnEnd {
    pub stop_reason: StopReason,
    pub error: Option<String>,
}

impl TurnEnd {
    pub fn new(stop_reason: StopReason) -> Self {
        Self {
            stop_reason,
            error: None,
        }
    }
    pub fn with_error(stop_reason: StopReason, error: impl Into<String>) -> Self {
        Self {
            stop_reason,
            error: Some(error.into()),
        }
    }
}

/// Where a turn's events go.
pub trait EventSink: Send {
    fn emit(&mut self, ev: LoopEvent);
}

impl<F: FnMut(LoopEvent) + Send> EventSink for F {
    fn emit(&mut self, ev: LoopEvent) {
        self(ev)
    }
}

impl EventSink for tokio::sync::mpsc::UnboundedSender<LoopEvent> {
    fn emit(&mut self, ev: LoopEvent) {
        let _ = self.send(ev);
    }
}

/// Collects every event, for tests.
impl EventSink for Vec<LoopEvent> {
    fn emit(&mut self, ev: LoopEvent) {
        self.push(ev)
    }
}
