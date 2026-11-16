//! What the default hub needs from the conversation it serves (the subagents'
//! parent): its deps, tools and system prompt to build a subagent from, and
//! where the subagents' activity goes.

use std::sync::Arc;

use async_trait::async_trait;
pub use haest_core::caps::SubagentCaps;
use haest_core::{
    AgentDeps, LoopEvent,
    provider::Effort,
    tool::{InnerCallEvent, Tool, ToolSource},
};

use super::AgentToolOptions;
use crate::inbox::Inbox;

/// A subagent's life, for the host's transcript (its own card, never the parent's).
#[derive(Debug, Clone)]
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
    Loop(LoopEvent),
    /// A tool it ran made a call of its own (Code).
    InnerCall(InnerCallEvent),
    /// Its turn ended.
    Stopped {
        /// It ended its turn with an answer: not stopped, not out of budget,
        /// no error.
        ok: bool,
    },
}

/// The parent conversation's side of the default hub
/// ([`SubagentHub`](super::SubagentHub)): what a subagent is built from, and
/// where its activity goes.
/// [`Inbox::submit`] takes what is for main: a message to "main", and a
/// background answer whose starter has finished.
///
/// [`SubagentHub`](super::SubagentHub)'s docs implement one.
#[async_trait]
pub trait SubagentParent: Inbox {
    /// The conversation's own deps. A subagent shares its models, cwd, workspace,
    /// spill dir, ledger, web search and hooks; its tools, prompt and turn caps
    /// are its own, and it neither persists its messages nor compacts.
    fn deps(&self) -> AgentDeps;
    /// The tools every agent starts from (no agent tools): the hub drops the
    /// conversation-only ones (the task list) and adds the agent's own agent tools.
    fn base_tools(&self) -> Vec<Arc<dyn Tool>>;
    /// The system prompt for an agent on `model_spec` with `tools`; the hub
    /// appends the subagent note. Built again before every model call.
    async fn system_prompt(&self, model_spec: &str, tools: &[Arc<dyn Tool>]) -> String;
    /// What a subagent runs on when the call names no model: the conversation's own.
    fn default_model(&self) -> (String, Option<Effort>);
    /// What the Agent tool of every agent here offers the model: the host's
    /// kinds of subagent (`subagent_type`) and the `model` parameter's text.
    /// Default: no kinds, haest's own text.
    fn agent_tool_options(&self) -> AgentToolOptions {
        AgentToolOptions::default()
    }
    /// The tools a subagent of `subagent_type` starts from: one of the kinds in
    /// [`agent_tool_options`](Self::agent_tool_options) (the call's, else the
    /// default kind), None when the host has none. The hub drops the
    /// conversation-only ones and adds the agent's own agent tools, as for
    /// [`base_tools`](Self::base_tools). Default: `base_tools`, whatever the kind.
    fn subagent_tools(&self, subagent_type: Option<&str>) -> Vec<Arc<dyn Tool>> {
        let _ = subagent_type;
        self.base_tools()
    }
    /// Where an agent's tool list goes through (deferral); default: as it is.
    fn tool_source(&self, tools: Vec<Arc<dyn Tool>>) -> Arc<dyn ToolSource> {
        Arc::new(tools)
    }
    /// The running set changed: the ids of every subagent running now, for
    /// the host's roster. An id (`haest-<name>-<hub>`) is unique across hubs;
    /// a name only within one.
    fn on_activity(&self, _ids: &[String]) {}
    /// One subagent's activity, keyed by its id.
    fn on_subagent(&self, _id: &str, _event: SubagentEvent) {}
}
