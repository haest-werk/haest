//! What every event carries: the payload a mod file's handler gets, and a
//! [`Hook`](crate::Hook) too, built from a [`Mod`](crate::Mod) event's
//! arguments here and nowhere else. Each is camelCase JSON, byte for byte
//! what a TypeScript handler has always received, plus `session`
//! ([`SessionView`]) when a [`HookSet`](crate::HookSet) binding or a mod
//! file's handler gets it in a conversation whose host named one.
//! [docs/hooks.md](https://github.com/haest-werk/haest/blob/main/docs/hooks.md)
//! shows each one as a hook in another process gets it.

use std::path::PathBuf;

use haest_core::{
    Agent, AssistantMessage, Effort, Message, StopReason, TurnOutcome,
    tool::{ToolContext, ToolOutput},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{api::ModToolCall, env::ModEnv, events::ToolEvent};

/// The session a conversation is on, as its host names it: its id, and
/// where the host keeps its transcript.
///
/// The host names it on the conversation's [`ModEnv`]
/// ([`ModEnv::set_session`]), and names the new one when the human's /clear
/// starts it; every payload a [`HookSet`](crate::HookSet) hands a binding,
/// and a mod file hands a handler, carries the one named as of that event
/// ([`Event::session`](crate::Event::session)), so a hook never holds on to
/// a session of its own. haest-runtime names its
/// session file's id and path.
///
/// JSON: `{ id, transcriptPath }`, `transcriptPath` left out when the host
/// keeps none.
///
/// ```
/// use haest_core::workspace::WorkspaceMode;
/// use haest_mods::{ModEnv, SessionView};
/// # use haest_mods::{ApprovalDecision, ApprovalRequest, ModUi};
/// # struct Terminal;
/// # #[async_trait::async_trait]
/// # impl ModUi for Terminal {
/// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> ApprovalDecision { ApprovalDecision::Deny }
/// #     fn info(&self, _: &str) {}
/// # }
///
/// let env = ModEnv::new("/project", WorkspaceMode::Real, Terminal)
///     .with_session(SessionView::new("s-1").with_transcript_path("/sessions/s-1.jsonl"));
/// // The human's /clear: a new session, for every mod and hook from the next event on.
/// env.set_session(SessionView::new("s-2").with_transcript_path("/sessions/s-2.jsonl"));
/// assert_eq!(env.session().map(|s| s.id), Some("s-2".into()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SessionView {
    /// The session's id, as the host knows it.
    pub id: String,
    /// Where the host keeps the session's transcript; `None` when it keeps
    /// none. In JSON a byte that is not UTF-8 is U+FFFD ([`Path::to_string_lossy`](std::path::Path::to_string_lossy)).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "lossy::some"
    )]
    pub transcript_path: Option<PathBuf>,
}

/// A path as JSON text, every byte that is not UTF-8 replaced by U+FFFD
/// ([`Path::to_string_lossy`](std::path::Path::to_string_lossy)): serde's own `PathBuf` refuses such a path,
/// and a payload that does not encode fails every hook it goes to.
mod lossy {
    use std::path::{Path, PathBuf};

    use serde::Serializer;

    pub(super) fn path<S: Serializer>(path: &Path, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&path.to_string_lossy())
    }

    #[expect(
        clippy::ref_option,
        reason = "serde's serialize_with hands the field as it is"
    )]
    pub(super) fn some<S: Serializer>(path: &Option<PathBuf>, s: S) -> Result<S::Ok, S::Error> {
        match path {
            Some(path) => s.serialize_some(&path.to_string_lossy()),
            None => s.serialize_none(),
        }
    }
}

impl SessionView {
    /// The session `id`, with no transcript.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            transcript_path: None,
        }
    }

    /// Its transcript is at `path`.
    #[must_use]
    pub fn with_transcript_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.transcript_path = Some(path.into());
        self
    }
}

/// Where a tool call runs, as a tool event's payload says it: a mod file's
/// handler gets it as `ctx` (with its own `signal` added), and so does a mod
/// file tool's `execute`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ToolCallContext {
    /// The conversation's working directory. In JSON a byte that is not
    /// UTF-8 is U+FFFD ([`Path::to_string_lossy`](std::path::Path::to_string_lossy)).
    #[serde(serialize_with = "lossy::path")]
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
/// JSON: `{ call: { name, input, readOnly }, ctx: { cwd, toolCallId } }`,
/// and `session`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ToolBeforePayload {
    /// The call, with the input the hooks before this one left.
    pub call: ModToolCall,
    /// Where it runs.
    pub ctx: ToolCallContext,
    /// The session the conversation is on ([`SessionView`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionView>,
}

impl ToolBeforePayload {
    /// The payload of [`Mod::tool_before`](crate::Mod::tool_before)'s
    /// arguments, no session named yet.
    pub fn new(call: &ModToolCall, ctx: &ToolContext) -> Self {
        Self {
            call: call.clone(),
            ctx: ToolCallContext::new(ctx),
            session: None,
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
/// JSON: `{ call, output: { content, isError, .. }, ctx }`, and `session`.
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
    /// The session the conversation is on ([`SessionView`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionView>,
}

impl ToolAfterPayload {
    /// The payload of [`Mod::tool_after`](crate::Mod::tool_after)'s
    /// arguments, no session named yet.
    pub fn new(call: &ModToolCall, output: &ToolOutput, ctx: &ToolContext) -> Self {
        Self {
            call: call.clone(),
            output: output.clone(),
            ctx: ToolCallContext::new(ctx),
            session: None,
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
/// JSON: `{ text }`, and `session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PromptSubmitPayload {
    /// The message, as the hooks before this one left it.
    pub text: String,
    /// The session the conversation is on ([`SessionView`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionView>,
}

impl PromptSubmitPayload {
    /// The payload of [`Mod::prompt_submit`](crate::Mod::prompt_submit)'s
    /// text, no session named yet.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            session: None,
        }
    }
}

/// `system.prompt`: a model call is about to be sent. A hook answers text
/// to append to the system prompt, or `None` (`null`).
///
/// JSON: `{}`, and `session`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SystemPromptPayload {
    /// The session the conversation is on ([`SessionView`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionView>,
}

impl SystemPromptPayload {
    /// The payload of [`Mod::system_prompt`](crate::Mod::system_prompt),
    /// no session named yet.
    pub fn new() -> Self {
        Self::default()
    }
}

/// What a loop event's payload says of the running agent: its model, its
/// effort, how long its history is and how full its context window. The
/// history itself never crosses: a loop event fires at every step.
///
/// JSON: `{ modelSpec, effort, historyLength, contextTokens, contextWindow }`.
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
    /// What the last successful model call put in the context window, in
    /// tokens ([`Agent::context_tokens`]); 0 before any. A hook that
    /// compacts on size reads this, not `historyLength`.
    #[serde(default)]
    pub context_tokens: u64,
    /// The model's context window, in tokens, when the host knows it
    /// ([`Agent::context_window`]); `null` when it does not.
    #[serde(default)]
    pub context_window: Option<u64>,
}

impl AgentView {
    /// `agent`, as a loop event's payload says it.
    pub fn new(agent: &Agent) -> Self {
        Self {
            model_spec: agent.model_spec().to_string(),
            effort: agent.effort(),
            history_length: agent.history().len(),
            context_tokens: agent.context_tokens(),
            context_window: agent.context_window(),
        }
    }
}

/// `model.before`: a model call is about to be made. A hook answers a
/// [`ModelBeforeDecision`](crate::ModelBeforeDecision).
///
/// JSON: the [`AgentView`]'s fields, then `lastMessage`, and `session`.
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
    /// The session the conversation is on ([`SessionView`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionView>,
}

impl ModelBeforePayload {
    /// The payload of [`Mod::model_before`](crate::Mod::model_before)'s
    /// agent, no session named yet.
    pub fn new(agent: &Agent) -> Self {
        Self {
            agent: AgentView::new(agent),
            last_message: agent.history().last().map(light),
            session: None,
        }
    }
}

/// `model.after`: an answer landed, its tool calls have not run. A hook
/// answers a [`ModelAfterDecision`](crate::ModelAfterDecision).
///
/// JSON: the [`AgentView`]'s fields, then `answer`, and `session`.
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
    /// The session the conversation is on ([`SessionView`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionView>,
}

impl ModelAfterPayload {
    /// The payload of [`Mod::model_after`](crate::Mod::model_after)'s
    /// arguments, no session named yet.
    pub fn new(agent: &Agent, answer: &AssistantMessage) -> Self {
        Self {
            agent: AgentView::new(agent),
            answer: light(answer),
            session: None,
        }
    }
}

/// `turn.end`: a turn is about to end on its own. A hook answers a
/// [`TurnEndDecision`](crate::TurnEndDecision).
///
/// JSON: the [`AgentView`]'s fields, then `stopReason`, `error`, `answer`
/// and `continued`, and `session`.
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
    /// The session the conversation is on ([`SessionView`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionView>,
}

impl TurnEndPayload {
    /// The payload of [`Mod::turn_end`](crate::Mod::turn_end)'s arguments,
    /// no session named yet.
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
            session: None,
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
