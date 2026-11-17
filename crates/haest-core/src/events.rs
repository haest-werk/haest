//! What one turn of the loop emits, in order: [`LoopEvent`], delivered to an
//! [`EventSink`] the host passes to [`Agent::run`](crate::Agent::run).
//!
//! A host renders them, a CLI prints them, `--json` writes each as one NDJSON
//! line (the serde shape is stable, and a Rust program reads such a line back
//! with `serde_json::from_str::<LoopEvent>`). One turn emits:
//!
//! 1. [`LoopEvent::User`] for the turn's input;
//! 2. per model call: [`LoopEvent::ModelStart`], the stream's
//!    [`TextDelta`](LoopEvent::TextDelta) / [`ThinkingDelta`](LoopEvent::ThinkingDelta) /
//!    [`ToolCallStart`](LoopEvent::ToolCallStart) events, then
//!    [`LoopEvent::Assistant`] with the whole answer;
//! 3. one [`LoopEvent::ToolResult`] per tool call, in call order (an aborted
//!    turn still answers every call); then the turn's thresholds: a
//!    [`LoopEvent::Guard`] if the model was reminded (the guard's nudge),
//!    another for the wind-down notice (a warning to the human goes to
//!    [`AgentHooks::on_warning`](crate::AgentHooks::on_warning)); and a
//!    [`LoopEvent::User`] if steering input was queued. Back to 2 while the
//!    model calls tools or steering arrived;
//! 4. [`LoopEvent::TurnEnd`], always last, exactly once.
//!
//! A model call that never went out (no provider for the spec, a request
//! over [`max_request_tokens`](crate::caps::TurnCaps::max_request_tokens))
//! is an [`Assistant`](LoopEvent::Assistant) event with no `ModelStart`
//! before it. A
//! turn refused because another one is running emits only `TurnEnd`.
//!
//! ```
//! use haest_core::{EventSink, LoopEvent};
//!
//! // A closure is a sink; so is a `Vec<LoopEvent>` and a tokio unbounded sender.
//! let mut events = Vec::new();
//! let mut sink = |ev: LoopEvent| events.push(ev);
//! sink.emit(LoopEvent::TextDelta { text: "hi".into() });
//! // Each event is one JSON line in `--json` output and the TypeScript haest's shape.
//! assert_eq!(serde_json::to_string(&events[0])?, r#"{"type":"text_delta","text":"hi"}"#);
//! # Ok::<(), serde_json::Error>(())
//! ```

use serde::{Deserialize, Serialize};

use crate::{
    messages::{AssistantMessage, StopReason, ToolCall, ToolResultMessage, Usage, UserMessage},
    remedy::Limit,
    tool::ToolOutput,
};

/// One thing that happened in a turn. Serialized with a snake_case `type` tag
/// and camelCase fields (`{"type":"turn_end","stopReason":"end_turn",...}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum LoopEvent {
    /// A user message entered history (the turn's input, or queued steering).
    User {
        /// The message as history keeps it, attached media included.
        message: UserMessage,
    },
    /// A model call is about to go out.
    ModelStart {
        /// The id of the provider serving it.
        provider: String,
        /// The provider's model id.
        model: String,
    },
    /// A piece of answer text, as the model streams it.
    TextDelta {
        /// The new text.
        text: String,
    },
    /// A piece of the model's reasoning, as it streams.
    ThinkingDelta {
        /// The new reasoning text.
        text: String,
    },
    /// A tool call began in the model's stream: the text before it was narration.
    ToolCallStart {
        /// The call's id.
        id: String,
        /// The tool's name.
        name: String,
    },
    /// The model's complete answer (also a refused call that never went out).
    Assistant {
        /// The answer, as history keeps it. A refused call's answer is NOT
        /// in history: nothing was sent.
        message: AssistantMessage,
    },
    /// A tool call was answered; its result is in history.
    ToolResult {
        /// The call it answers.
        call: ToolCall,
        /// The result as history keeps it and the model reads it.
        message: ToolResultMessage,
        /// The tool's whole output, including what only the host sees
        /// (`result`, `title`, costs).
        output: ToolOutput,
        /// From the start of the call's batch to its answer.
        duration_ms: u64,
    },
    /// The turn guard nudged the model (the text it was sent); the human must see it too.
    Guard {
        /// The nudge, as the model received it.
        text: String,
    },
    /// The turn is over. Always the last event of a turn, exactly once.
    TurnEnd {
        /// How the turn ended.
        stop_reason: StopReason,
        /// What the whole turn cost: every model call, a summary a hook paid
        /// for, and every tool's own spend.
        usage: Usage,
        /// Wall-clock time of the turn.
        duration_ms: u64,
        /// Why the turn stopped, for an error or a cap.
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// How a turn ended: the [`LoopEvent::TurnEnd`] event without its tallies.
/// [`Agent::run`](crate::Agent::run) returns it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
#[must_use = "a turn can end in an error"]
pub struct TurnOutcome {
    /// Why it ended. [`StopReason::MaxTokens`] here means the model's own
    /// output limit cut its last answer (`limit` None), or a turn cap or the
    /// conversation budget stopped the turn (`limit` says which).
    pub stop_reason: StopReason,
    /// What went wrong, or which cap was hit; None for a turn that ended normally.
    pub error: Option<String>,
    /// The cap that stopped it, when one did: a turn cap, the conversation's
    /// budget, a request too big to send, a threshold that stops. A host
    /// matches it (`Some(Limit::Conversation(_))`) instead of reading `error`.
    pub limit: Option<Limit>,
}

impl TurnOutcome {
    /// An ending with no error text.
    pub fn new(stop_reason: StopReason) -> Self {
        Self {
            stop_reason,
            error: None,
            limit: None,
        }
    }
    /// An ending that says why.
    pub fn error(stop_reason: StopReason, error: impl Into<String>) -> Self {
        Self {
            stop_reason,
            error: Some(error.into()),
            limit: None,
        }
    }
    /// The same ending, stopped by `limit` (None: no cap stopped it).
    pub fn with_limit(mut self, limit: impl Into<Option<Limit>>) -> Self {
        self.limit = limit.into();
        self
    }
}

/// Where a turn's events go.
///
/// Implemented for any `FnMut(LoopEvent) + Send` closure, for `Vec<LoopEvent>`
/// (collects them) and for a tokio `UnboundedSender<LoopEvent>` (a closed
/// channel drops the event). [`emit`](EventSink::emit) is called inline on
/// the loop's task: a slow sink slows the turn.
pub trait EventSink: Send {
    /// Take one event. Events arrive in the order described in the
    /// [module docs](self).
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

/// Any sink, the caller's own type or a `dyn EventSink`, as one the loop
/// holds as `&mut dyn EventSink`: an unsized one cannot be coerced to it
/// directly, this sized wrapper can.
pub(crate) struct Forward<'a, S: ?Sized>(pub(crate) &'a mut S);

impl<S: EventSink + ?Sized> EventSink for Forward<'_, S> {
    fn emit(&mut self, ev: LoopEvent) {
        self.0.emit(ev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{text_answer, tool_answer};

    /// What `--json` writes, a Rust program reads back as the same event.
    #[test]
    fn every_event_reads_back_from_its_json_line() {
        let call = tool_answer(&[("c1", "Read", serde_json::json!({"path": "/a"}))]);
        let call = call.tool_calls().next().unwrap().clone();
        let events = [
            LoopEvent::User {
                message: UserMessage {
                    content: "hi".into(),
                    media: Vec::new(),
                },
            },
            LoopEvent::ModelStart {
                provider: "p".into(),
                model: "m".into(),
            },
            LoopEvent::TextDelta { text: "t".into() },
            LoopEvent::ThinkingDelta { text: "r".into() },
            LoopEvent::ToolCallStart {
                id: "c1".into(),
                name: "Read".into(),
            },
            LoopEvent::Assistant {
                message: text_answer("done"),
            },
            LoopEvent::ToolResult {
                call: call.clone(),
                message: ToolResultMessage {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    content: "content".into(),
                    is_error: false,
                    media: Vec::new(),
                },
                output: ToolOutput::text("content"),
                duration_ms: 3,
            },
            LoopEvent::Guard { text: "g".into() },
            LoopEvent::TurnEnd {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                duration_ms: 9,
                error: None,
            },
        ];
        for ev in events {
            let line = serde_json::to_string(&ev).unwrap();
            let back: LoopEvent = serde_json::from_str(&line).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), line);
        }
    }
}
