//! What every event carries: the payload a mod file's handler gets, and a
//! [`Hook`](crate::Hook) too, built from a [`Mod`](crate::Mod) event's
//! arguments here and nowhere else. Each is camelCase JSON, byte for byte
//! what a TypeScript handler has always received.

use std::path::PathBuf;

use haest_core::{
    Agent, AssistantMessage, Effort, Message, StopReason, TurnOutcome,
    tool::{ToolContext, ToolOutput},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{api::ModToolCall, env::ModEnv, events::ToolEvent};

/// Where a tool call runs, as a tool event's payload says it: a mod file's
/// handler gets it as `ctx` (with its own `signal` added), and so does a mod
/// file tool's `execute`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ToolCallContext {
    /// The conversation's working directory.
    pub cwd: PathBuf,
    /// The call's id, when the loop gave it one (`null` in JSON otherwise).
    pub tool_call_id: Option<String>,
}

impl ToolCallContext {
    /// `ctx` as a hook sees it.
    pub fn new(ctx: &ToolContext) -> Self {
        Self {
            cwd: ctx.cwd().to_path_buf(),
            tool_call_id: ctx.tool_call_id().map(str::to_string),
        }
    }
}

/// `tool.before`: a tool call about to run. A hook answers a
/// [`Before`](crate::Before).
///
/// JSON: `{ call: { name, input, readOnly }, ctx: { cwd, toolCallId } }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ToolBeforePayload {
    /// The call, with the input the hooks before this one left.
    pub call: ModToolCall,
    /// Where it runs.
    pub ctx: ToolCallContext,
}

impl ToolBeforePayload {
    /// The payload of [`Mod::tool_before`](crate::Mod::tool_before)'s arguments.
    pub fn new(call: &ModToolCall, ctx: &ToolContext) -> Self {
        Self {
            call: call.clone(),
            ctx: ToolCallContext::new(ctx),
        }
    }
}

impl ToolEvent for ToolBeforePayload {
    fn tool_name(&self) -> &str {
        &self.call.name
    }
}

/// `tool.after`: a tool call that ran. A hook answers the output that
/// replaces this one, or `None` (`null`) to keep it.
///
/// JSON: `{ call, output: { content, isError, .. }, ctx }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ToolAfterPayload {
    /// The call as it ran.
    pub call: ModToolCall,
    /// Its output, as the hooks before this one left it.
    pub output: ToolOutput,
    /// Where it ran.
    pub ctx: ToolCallContext,
}

impl ToolAfterPayload {
    /// The payload of [`Mod::tool_after`](crate::Mod::tool_after)'s arguments.
    pub fn new(call: &ModToolCall, output: &ToolOutput, ctx: &ToolContext) -> Self {
        Self {
            call: call.clone(),
            output: output.clone(),
            ctx: ToolCallContext::new(ctx),
        }
    }
}

impl ToolEvent for ToolAfterPayload {
    fn tool_name(&self) -> &str {
        &self.call.name
    }
}

/// `prompt.submit`: the human's message before the model sees it. A hook
/// answers the new text, or `None` (`null`) to keep it.
///
/// JSON: `{ text }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PromptSubmitPayload {
    /// The message, as the hooks before this one left it.
    pub text: String,
}

impl PromptSubmitPayload {
    /// The payload of [`Mod::prompt_submit`](crate::Mod::prompt_submit)'s text.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// `system.prompt`: a model call is about to be sent. A hook answers text
/// to append to the system prompt, or `None` (`null`).
///
/// JSON: `{}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SystemPromptPayload {}

impl SystemPromptPayload {
    /// The (empty) payload of [`Mod::system_prompt`](crate::Mod::system_prompt).
    pub fn new() -> Self {
        Self {}
    }
}

/// What a loop event's payload says of the running agent: its model, its
/// effort and how long its history is. The history itself never crosses: a
/// loop event fires at every step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentView {
    /// The model in use, as `provider:model`.
    pub model_spec: String,
    /// The effort asked for, as its word; `null` when none is.
    pub effort: Option<Effort>,
    /// How many messages the history holds.
    pub history_length: usize,
}

impl AgentView {
    /// `agent`, as a loop event's payload says it.
    pub fn new(agent: &Agent) -> Self {
        Self {
            model_spec: agent.model_spec().to_string(),
            effort: agent.effort(),
            history_length: agent.history().len(),
        }
    }
}

/// `model.before`: a model call is about to be made. A hook answers a
/// [`ModelBeforeDecision`](crate::ModelBeforeDecision).
///
/// JSON: `{ modelSpec, effort, historyLength, lastMessage }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ModelBeforePayload {
    /// The agent.
    #[serde(flatten)]
    pub agent: AgentView,
    /// The last message in its history, as a session file writes it less
    /// media bytes, thinking signatures and provider state; `null` in an
    /// empty history.
    pub last_message: Option<Value>,
}

impl ModelBeforePayload {
    /// The payload of [`Mod::model_before`](crate::Mod::model_before)'s agent.
    pub fn new(agent: &Agent) -> Self {
        Self {
            agent: AgentView::new(agent),
            last_message: agent.history().last().map(light),
        }
    }
}

/// `model.after`: an answer landed, its tool calls have not run. A hook
/// answers a [`ModelAfterDecision`](crate::ModelAfterDecision).
///
/// JSON: `{ modelSpec, effort, historyLength, answer }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ModelAfterPayload {
    /// The agent, the answer last in its history.
    #[serde(flatten)]
    pub agent: AgentView,
    /// The answer, as a session file writes it less media bytes, thinking
    /// signatures and provider state.
    pub answer: Value,
}

impl ModelAfterPayload {
    /// The payload of [`Mod::model_after`](crate::Mod::model_after)'s arguments.
    pub fn new(agent: &Agent, answer: &AssistantMessage) -> Self {
        Self {
            agent: AgentView::new(agent),
            answer: light(answer),
        }
    }
}

/// `turn.end`: a turn is about to end on its own. A hook answers a
/// [`TurnEndDecision`](crate::TurnEndDecision).
///
/// JSON: `{ modelSpec, effort, historyLength, stopReason, error, answer,
/// continued }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TurnEndPayload {
    /// The agent, its answer last in its history.
    #[serde(flatten)]
    pub agent: AgentView,
    /// Why the turn ends, as its word (`end_turn`).
    pub stop_reason: StopReason,
    /// What went wrong; `null` for a turn that ended normally.
    pub error: Option<String>,
    /// The answer that ends the turn, as `model.after` has it (less media
    /// bytes, thinking signatures and provider state); `null` when the
    /// history does not end in one.
    pub answer: Option<Value>,
    /// Whether a `turn.end` decision already kept this turn going: a hook
    /// that keeps it going once checks this, or a turn it holds runs to its
    /// caps. A new turn starts `false`; the host's own
    /// [`before_turn_end`](haest_core::AgentHooks::before_turn_end) does not
    /// count.
    pub continued: bool,
}

impl TurnEndPayload {
    /// The payload of [`Mod::turn_end`](crate::Mod::turn_end)'s arguments.
    pub fn new(
        agent: &Agent,
        outcome: &TurnOutcome,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Self {
        let answer = match agent.history().last() {
            Some(Message::Assistant(answer)) => Some(light(answer)),
            _ => None,
        };
        Self {
            agent: AgentView::new(agent),
            stop_reason: outcome.stop_reason,
            error: outcome.error.clone(),
            answer,
            continued: haest.continued(cancel),
        }
    }
}

/// A message as the session file writes it, less what is bulky and means
/// nothing to a hook: media bytes, thinking signatures, provider state.
fn light(msg: &impl Serialize) -> Value {
    let mut v = serde_json::to_value(msg).unwrap_or_default();
    let Some(o) = v.as_object_mut() else {
        return v;
    };
    o.remove("providerData");
    for list in ["media", "content"] {
        let parts = o.get_mut(list).and_then(Value::as_array_mut);
        for part in parts.into_iter().flatten().filter_map(Value::as_object_mut) {
            for bulky in ["data", "signature", "redacted"] {
                part.remove(bulky);
            }
        }
    }
    v
}
