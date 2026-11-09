//! What the default hub needs from the host: the conversation's deps, tools and
//! system prompt to build a subagent from, and where its activity goes.

use std::sync::Arc;

use async_trait::async_trait;
use kern_core::{
    AgentDeps, LoopEvent,
    provider::Effort,
    tool::{InnerCallEvent, Tool, ToolSource},
};

use crate::inbox::Inbox;

/// The caps on the subagent tree. `max_depth` and `max_concurrent` are NOT
/// "0 = off": 0 means no subagents at all (no Agent tool). The three per-agent
/// caps replace the conversation's turn caps for each subagent's one turn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubagentCaps {
    /// How deep agents may nest (main = 0); KERN_MAX_AGENT_DEPTH.
    pub max_depth: u32,
    /// Subagents running at once, across the tree; KERN_MAX_SUBAGENTS.
    pub max_concurrent: usize,
    /// One subagent's spend; KERN_SUBAGENT_USD.
    pub usd: f64,
    /// One subagent's model calls; KERN_SUBAGENT_STEPS.
    pub steps: u32,
    /// One subagent's wall clock; KERN_SUBAGENT_MINUTES.
    pub minutes: u64,
}

impl Default for SubagentCaps {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_concurrent: 4,
            usd: 1.0,
            steps: 50,
            minutes: 15,
        }
    }
}

/// A subagent's life, for the host's transcript (its own card, never the parent's).
#[derive(Debug, Clone)]
pub enum SubagentEvent {
    Started {
        name: String,
        parent: String,
        description: String,
    },
    /// One event of its turn.
    Loop(LoopEvent),
    /// A tool it ran made a call of its own (Code).
    InnerCall(InnerCallEvent),
    Stopped {
        ok: bool,
    },
}

/// The host's side of the default hub. [`Inbox::submit`] takes what is for
/// main: a message to "main", and a background answer whose starter has finished.
#[async_trait]
pub trait SubagentHost: Inbox {
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
    /// Where an agent's tool list goes through (deferral); default: as it is.
    fn tool_source(&self, tools: Vec<Arc<dyn Tool>>) -> Arc<dyn ToolSource> {
        Arc::new(tools)
    }
    /// The running set changed (agent ids): the host's roster of subagents.
    fn on_activity(&self, _running: &[String]) {}
    /// One subagent's activity, keyed by its id.
    fn on_subagent(&self, _id: &str, _event: SubagentEvent) {}
}
