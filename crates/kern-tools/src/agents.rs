//! SUBAGENTS: Agent / SendMessage / StopAgent, and the hub that runs them.
//! Ported from the TS kern's `tools/agent-tools.ts`, `host/agent-hub.ts` and
//! `host/agent-run.ts`.
//!
//! A subagent is a second [`kern_core::Agent`] loop in this process with its own
//! history, its own turn caps ([`SubagentCaps`]) and the conversation's shared
//! ledger -- so the whole tree stays under the conversation's budget. Its spend
//! comes back as the Agent call's `child_cost_usd`: the parent's turn (and its
//! USD cap) counts it, the ledger is not charged twice.
//!
//! Addresses: the conversation's own loop is [`MAIN`]; every subagent has a
//! unique name. A message lands at the receiver's next step boundary
//! ([`kern_core::AgentControl::steer`]); one for "main" goes to the host's
//! [`crate::Inbox`], a turn the human did not start.
//!
//! Depth: main is 0; an agent at the depth cap gets no Agent tool. Breadth: at
//! most `max_concurrent` run at once, across the whole tree.
//!
//! PLUG POINT: the tools speak to an [`AgentDirectory`], never to the hub, so a
//! host with its own idea of a subagent (one that runs elsewhere, with its own
//! types) implements that trait and keeps these tools. [`SubagentHub`] is the
//! default, over a [`SubagentHost`].

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
pub use tools::agent_tools;

/// The conversation's own agent.
pub const MAIN: &str = "main";

/// An agent as the tools know it: its address and how deep it runs (main = 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRef {
    pub name: String,
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
    pub description: String,
    pub prompt: String,
    pub name: Option<String>,
    /// `provider:model`; None = the conversation's own.
    pub model: Option<String>,
    pub background: bool,
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
#[async_trait]
pub trait AgentDirectory: Send + Sync {
    /// Run a subagent for `from`; `cancel` is the caller's turn (a foreground
    /// agent stops with it).
    async fn spawn(
        &self,
        from: &AgentRef,
        input: SpawnInput,
        cancel: CancellationToken,
    ) -> Result<SpawnOutcome, String>;
    /// SendMessage: what to tell the sender.
    fn deliver(&self, from: &str, to: &str, text: &str) -> Result<String, String>;
    /// StopAgent: false when `name` is not running.
    fn stop(&self, name: &str) -> bool;
}
