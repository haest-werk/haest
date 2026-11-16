//! SUBAGENTS: Agent / SendMessage / StopAgent, and the hub that runs them.
//! Ported from the TS haest's `tools/agent-tools.ts`, `host/agent-hub.ts` and
//! `host/agent-run.ts`.
//!
//! A subagent is a second [`haest_core::Agent`] loop in this process with its own
//! history, its own turn caps ([`SubagentCaps`]) and the conversation's shared
//! ledger -- so the whole tree stays under the conversation's budget. Its spend
//! comes back as the Agent call's `child_cost_usd`: the parent's turn (and its
//! USD cap) counts it, the ledger is not charged twice.
//!
//! Addresses: the conversation's own loop is [`MAIN`]; every subagent has a
//! unique name. A message lands at the receiver's next step boundary
//! ([`haest_core::AgentControl::steer`]); one for "main" goes to the host's
//! [`crate::Inbox`], a turn the human did not start.
//!
//! Depth: main is 0; an agent at the depth cap gets no Agent tool. Breadth: at
//! most `max_concurrent` run at once, across the whole tree.
//!
//! PLUG POINT: the tools speak to an [`AgentDirectory`], never to the hub, so a
//! host with its own idea of a subagent (one that runs elsewhere, with its own
//! types) implements that trait and keeps these tools. [`SubagentHub`] is the
//! default, over a [`SubagentHost`]. [`AgentToolOptions`] gives the Agent tool
//! the host's kinds of subagent (`subagent_type`) and its own `model` text.
//!
//! | Tool | Kind | What it does |
//! |---|---|---|
//! | Agent | `task.spawn` | runs a subagent on a brief and returns its final answer (or, with `run_in_background`, returns at once; the answer arrives as a message) |
//! | SendMessage | `agent.unknown` | a message to `main` or a running subagent, read at its next step |
//! | StopAgent | `agent.unknown` | stops a running subagent |
//!
//! None is read-only. Start from [`SubagentHub::new`] and
//! [`SubagentHub::tools_for`], or [`agent_tools_with`] over your own
//! directory.
//!
//! # Examples
//!
//! A directory of the host's own: every subagent runs elsewhere, and its
//! answer comes back as the Agent call's result.
//!
//! ```
//! use std::sync::Arc;
//!
//! use async_trait::async_trait;
//! use haest_core::{tool::ToolContext, workspace::ShutWorkspace};
//! use haest_tools::agents::{
//!     AgentDirectory, AgentRef, AgentToolOptions, SpawnInput, SpawnOutcome, agent_tools_with,
//! };
//! use serde_json::json;
//! use tokio_util::sync::CancellationToken;
//!
//! struct Remote;
//!
//! #[async_trait]
//! impl AgentDirectory for Remote {
//!     async fn spawn(
//!         &self,
//!         from: &AgentRef,
//!         input: SpawnInput,
//!         _cancel: CancellationToken,
//!     ) -> Result<SpawnOutcome, String> {
//!         // Here the host would run `input.prompt` somewhere.
//!         let text = format!("{} asked: {}", from.name, input.description);
//!         Ok(SpawnOutcome { text, cost_usd: 0.01 })
//!     }
//!     fn deliver(&self, _from: &str, to: &str, _text: &str) -> Result<String, String> {
//!         Err(format!("\"{to}\" is not running"))
//!     }
//!     fn stop(&self, _name: &str) -> bool {
//!         false
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() {
//! let tools = agent_tools_with(Arc::new(Remote), AgentRef::main(), true, AgentToolOptions::default());
//! let ctx = ToolContext::new("/", Arc::new(ShutWorkspace));
//! let input = json!({"description": "find the bug", "prompt": "Look at parse.rs."});
//! let out = tools[0].execute(input.as_object().unwrap().clone(), &ctx).await.unwrap();
//! assert_eq!(out.content, "main asked: find the bug");
//! // Its spend counts toward the caller's turn, never charged twice.
//! assert_eq!(out.child_cost_usd, Some(0.01));
//! # }
//! ```

mod host;
mod hub;
mod members;
mod run;
#[cfg(test)]
mod tests;
mod tools;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

pub use host::{SubagentCaps, SubagentEvent, SubagentHost};
pub use hub::SubagentHub;
pub use run::{RunOutcome, subagent_note};
pub use tools::{agent_tools, agent_tools_with};

/// The conversation's own agent.
pub const MAIN: &str = "main";

/// An agent as the tools know it: its address and how deep it runs (main = 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRef {
    /// Its address: [`MAIN`], or a subagent's unique name.
    pub name: String,
    /// How many spawns deep it runs: main is 0, its subagents 1.
    pub depth: u32,
}

impl AgentRef {
    /// The conversation's own agent.
    pub fn main() -> Self {
        Self {
            name: MAIN.into(),
            depth: 0,
        }
    }
}

/// One Agent call, in the common agent-tool keys (`description`, `prompt`,
/// `name`, `model`, `run_in_background`), so every model already knows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpawnInput {
    /// A short label for the task (3-5 words).
    pub description: String,
    /// The complete brief: the subagent sees nothing else of the conversation.
    pub prompt: String,
    /// The address the caller asked for; the hub makes one from the
    /// description when None, and keeps names unique.
    pub name: Option<String>,
    /// `provider:model`; None = the conversation's own.
    pub model: Option<String>,
    /// `run_in_background`: return at once; the answer arrives as a message.
    pub background: bool,
    /// One of the host's [`SubagentTypes`] (the named one, else its default);
    /// None when the host has none. The default [`SubagentHub`] has none and
    /// ignores it.
    pub subagent_type: Option<String>,
}

/// A host's kinds of subagent, under the common `subagent_type` key: what
/// the model may pick, and what it reads about each. The default hub has none.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentTypes {
    /// The kinds the model may name; the parameter's `enum`. An Agent call
    /// that names another is an input error.
    pub names: Vec<String>,
    /// The parameter's description: what each kind may do.
    pub description: String,
    /// What an Agent call that names no kind gets; one of `names`.
    pub default: String,
}

/// PLUG POINT: what a host changes about the agent tools ([`agent_tools_with`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentToolOptions {
    /// The host's kinds of subagent: adds the `subagent_type` parameter.
    pub types: Option<SubagentTypes>,
    /// The `model` parameter's description, e.g. the models this host offers.
    pub models: Option<String>,
}

/// What a spawn hands back to the caller's turn.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnOutcome {
    /// The agent's final answer (or, in the background, that it started).
    pub text: String,
    /// What it spent, already charged to the ledger by its own loop.
    pub cost_usd: f64,
}

/// What the agent tools need from a hub. An Err is a refusal (a cap, an
/// unknown name) the model corrects: it goes back as an input error.
///
/// [`SubagentHub`] is the default; the module docs implement one of a host's
/// own.
#[async_trait]
pub trait AgentDirectory: Send + Sync {
    /// Agent: run a subagent for `from`; `cancel` is the caller's turn (a
    /// foreground agent stops with it). The outcome's text is the Agent call's
    /// result, its cost the call's `child_cost_usd`.
    async fn spawn(
        &self,
        from: &AgentRef,
        input: SpawnInput,
        cancel: CancellationToken,
    ) -> Result<SpawnOutcome, String>;
    /// SendMessage: hand `text` from agent `from` to agent `to`, and say what
    /// to tell the sender. The SendMessage tool refuses `to == from` itself.
    fn deliver(&self, from: &str, to: &str, text: &str) -> Result<String, String>;
    /// StopAgent: stop the agent `name`; false when it is not running.
    fn stop(&self, name: &str) -> bool;
}
