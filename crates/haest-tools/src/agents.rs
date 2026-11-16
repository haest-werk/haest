//! SUBAGENTS: Agent / SendMessage / StopAgent, and the hub that runs them.
//! Ported from the TS haest's `tools/agent-tools.ts`, `host/agent-hub.ts` and
//! `host/agent-run.ts`. The crate docs ("Subagents") say how they work;
//! [`AgentDirectory`] is the plug point.

mod budget;
mod events;
mod hub;
mod member;
mod members;
mod parent;
mod run;
#[cfg(test)]
mod tests;
mod tools;

use std::time::Duration;

use async_trait::async_trait;
use haest_core::caps::SubagentCaps;
use tokio_util::sync::CancellationToken;

pub use budget::SubagentBudget;
pub use events::{Sender, SubagentEvent};
pub use hub::SubagentHub;
pub use member::{Ending, InProcessRun, MemberCaps, MemberEnd, MemberRun, MemberStart, Spend};
pub use parent::SubagentParent;
pub use run::subagent_note;
pub use tools::{agent_tools, agent_tools_with};

/// The conversation's own agent.
pub const MAIN_AGENT: &str = "main";

/// An agent as the tools know it: its address and how deep it runs (main = 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRef {
    /// Its address: [`MAIN_AGENT`], or a subagent's unique name.
    pub name: String,
    /// How many spawns deep it runs: main is 0, its subagents 1.
    pub depth: u32,
}

impl AgentRef {
    /// The conversation's own agent.
    pub fn main() -> Self {
        Self {
            name: MAIN_AGENT.into(),
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
    pub model_spec: Option<String>,
    /// `run_in_background`: return at once; the answer arrives as a message.
    pub background: bool,
    /// One of the host's [`SubagentTypes`] (the named one, else its default);
    /// None when the host has none. The default [`SubagentHub`] builds the
    /// subagent from that kind's tools ([`SubagentParent::subagent_tools`]).
    pub subagent_type: Option<String>,
    /// The call's `budget`, each value already clamped to the caps; None when
    /// budgets are off ([`AgentToolOptions::budget_caps`]). The default hub
    /// runs the subagent under it and holds its USD on the conversation's
    /// ledger while it runs; a host's own directory applies it as it sees fit.
    pub budget: Option<SubagentBudget>,
}

/// A host's kinds of subagent, under the common `subagent_type` key: what
/// the model may pick, and what it reads about each. The default hub has the
/// ones its parent gives ([`SubagentParent::agent_tool_options`]); none unless
/// it gives some.
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
    /// The `model` parameter's description, e.g. the models this host offers;
    /// None = haest's own ("provider:model to run it on ...").
    pub model_description: Option<String>,
    /// Some = the Agent tool takes a `budget {usd, minutes}`, each value
    /// clamped to these caps (it only tightens; an absent one is the cap), and
    /// the time must fit what is left of the caller's turn. None = no such
    /// parameter (Claude Code's schema). [`SubagentHub::tools_for`] sets it
    /// from its caps when [`SubagentCaps::budgets`] is on, and clears it when off.
    pub budget_caps: Option<SubagentBudget>,
}

/// An agent tool's refusal, for the model to correct: it goes back as an
/// input error.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AgentError {
    /// The caller already runs as deep as agents may nest.
    #[error("agents may nest {max_depth} deep")]
    TooDeep {
        /// How deep agents may nest ([`SubagentCaps::max_depth`]).
        max_depth: u32,
    },
    /// As many subagents run as may at once.
    #[error(
        "{running} subagents are already running, at most {max} at once; wait for one, or StopAgent one"
    )]
    TooMany {
        /// How many run now.
        running: usize,
        /// How many may ([`SubagentCaps::max_concurrent`]).
        max: usize,
    },
    /// No agent of this conversation goes by the name.
    #[error("no agent \"{name}\"; running: {}", running.join(", "))]
    UnknownAgent {
        /// The name, as given.
        name: String,
        /// Who a message can reach now: main, then the running subagents.
        running: Vec<String>,
    },
    /// The agent has finished: nothing reaches it any more.
    #[error("agent \"{name}\" has finished; running: {}", running.join(", "))]
    Finished {
        /// Its name.
        name: String,
        /// Who a message can reach now: main, then the running subagents.
        running: Vec<String>,
    },
    /// A host's own refusal, its text for the model.
    #[error("{0}")]
    Refused(String),
    /// The subagent's spend does not fit what is left of the conversation's
    /// budget, counting what running subagents hold ([`SubagentBudget`]).
    #[error("{}", budget::over_budget(*asked, *available))]
    OverBudget {
        /// The USD it would hold; 0 = it has no spend cap.
        asked: f64,
        /// What is left: the cap, less what was spent and what is held.
        available: f64,
    },
    /// The subagent's wall clock does not fit what is left of its caller's turn.
    #[error("{}", budget::over_time(*asked, *left, setting))]
    OverTime {
        /// Its wall clock; zero = it has no time cap.
        asked: Duration,
        /// What is left of the caller's turn.
        left: Duration,
        /// The setting that caps the caller's turn.
        setting: &'static str,
    },
}

/// What a spawn hands back to the caller's turn.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnOutcome {
    /// The agent's final answer (or, in the background, that it started).
    pub text: String,
    /// What it spent, already charged to the ledger by its own loop.
    pub cost_usd: f64,
}

/// What the agent tools need from a hub. An Err is an [`AgentError`] (a cap,
/// an unknown name) the model corrects: it goes back as an input error.
///
/// [`SubagentHub`] is the default; a host with its own idea of a subagent
/// (one that runs elsewhere, with its own types) implements this and keeps
/// the tools ([`agent_tools_with`]). None of them is read-only:
///
/// | Tool | Kind | What it does |
/// |---|---|---|
/// | Agent | `task.spawn` | runs a subagent on a brief and returns its final answer (or, with `run_in_background`, returns at once; the answer arrives as a message) |
/// | SendMessage | `agent.unknown` | a message to `main` or a running subagent, read at its next step |
/// | StopAgent | `agent.unknown` | stops a running subagent |
///
/// # Examples
///
/// A directory of the host's own: every subagent runs elsewhere, and its
/// answer comes back as the Agent call's result.
///
/// ```
/// use std::sync::Arc;
///
/// use async_trait::async_trait;
/// use haest_core::{tool::ToolContext, workspace::ShutWorkspace};
/// use haest_tools::{
///     AgentDirectory, AgentError, AgentRef, AgentToolOptions, SpawnInput, SpawnOutcome,
///     agent_tools_with,
/// };
/// use serde_json::json;
/// use tokio_util::sync::CancellationToken;
///
/// struct Remote;
///
/// #[async_trait]
/// impl AgentDirectory for Remote {
///     async fn spawn(
///         &self,
///         from: &AgentRef,
///         input: SpawnInput,
///         _cancel: CancellationToken,
///     ) -> Result<SpawnOutcome, AgentError> {
///         // Here the host would run `input.prompt` somewhere.
///         let text = format!("{} asked: {}", from.name, input.description);
///         Ok(SpawnOutcome { text, cost_usd: 0.01 })
///     }
///     fn deliver(&self, _from: &str, to: &str, _text: &str) -> Result<String, AgentError> {
///         Err(AgentError::UnknownAgent { name: to.into(), running: vec!["main".into()] })
///     }
///     fn stop(&self, _name: &str) -> bool {
///         false
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() {
/// let tools = agent_tools_with(Arc::new(Remote), AgentRef::main(), true, AgentToolOptions::default());
/// let ctx = ToolContext::new("/", Arc::new(ShutWorkspace));
/// let input = json!({"description": "find the bug", "prompt": "Look at parse.rs."});
/// let out = tools[0].execute(input.as_object().unwrap().clone(), &ctx).await.unwrap();
/// assert_eq!(out.content, "main asked: find the bug");
/// // Its spend counts toward the caller's turn, never charged twice.
/// assert_eq!(out.child_cost_usd, Some(0.01));
/// # }
/// ```
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
    ) -> Result<SpawnOutcome, AgentError>;
    /// SendMessage: hand `text` from agent `from` to agent `to`, and say what
    /// to tell the sender. The SendMessage tool refuses `to == from` itself.
    fn deliver(&self, from: &str, to: &str, text: &str) -> Result<String, AgentError>;
    /// StopAgent: stop the agent `name`; false when it is not running.
    fn stop(&self, name: &str) -> bool;
}
