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
use haest_core::{caps::SubagentCaps, wind_down::WindingDown};
use tokio_util::sync::CancellationToken;

pub use budget::SubagentBudget;
pub use events::{AgentMessage, Sender, SubagentEvent};
pub use hub::SubagentHub;
pub use member::{Ending, Member, MemberCaps, MemberEnd, MemberRun, MemberStart, Spend};
pub use parent::{SubagentParent, SubagentParentRef};
pub use run::subagent_note;
pub use tools::agent_tools;

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
///
/// A host that spawns one itself builds it with [`SpawnInput::new`] and the
/// setters; the Agent tool reads one from the model's call.
///
/// ```
/// use haest_tools::SpawnInput;
///
/// let input = SpawnInput::new("List every TODO under src/ with its file and line.")
///     .with_description("find the TODOs")
///     .with_subagent_type("explore")
///     .with_background(true);
/// assert!(input.background);
/// assert_eq!(input.subagent_type.as_deref(), Some("explore"));
/// assert_eq!(input.name, None, "the hub names it from the description");
///
/// // A host holding an `Option` sets what it has.
/// let asked: Option<&str> = Some("todo-finder");
/// let input = match asked {
///     Some(name) => input.with_name(name),
///     None => input,
/// };
/// assert_eq!(input.name.as_deref(), Some("todo-finder"));
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
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
    /// budgets are off ([`AgentToolOptions::with_budget_caps`]). The default hub
    /// runs the subagent under it and holds its USD on the conversation's
    /// ledger while it runs; a host's own directory applies it as it sees fit.
    pub budget: Option<SubagentBudget>,
}

impl SpawnInput {
    /// A call with this brief (`prompt`) and nothing else: no description,
    /// the conversation's model, in the foreground.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            ..Self::default()
        }
    }

    /// The same, with a short label for the task (3-5 words).
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// The same, asking for this address (without one, the hub makes one
    /// from the description).
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// The same, on this `provider:model` (without one, the conversation's own).
    #[must_use]
    pub fn with_model_spec(mut self, model_spec: impl Into<String>) -> Self {
        self.model_spec = Some(model_spec.into());
        self
    }

    /// The same, of this one of the host's [`SubagentTypes`] (without one,
    /// the default).
    #[must_use]
    pub fn with_subagent_type(mut self, subagent_type: impl Into<String>) -> Self {
        self.subagent_type = Some(subagent_type.into());
        self
    }

    /// The same, under this budget (without one, the caps alone).
    #[must_use]
    pub fn with_budget(mut self, budget: SubagentBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// The same, in the background (`true`): the call returns at once and
    /// the answer arrives as a message. In the foreground by default.
    #[must_use]
    pub fn with_background(mut self, background: bool) -> Self {
        self.background = background;
        self
    }
}

/// A host's kinds of subagent, under the common `subagent_type` key: what
/// the model may pick, and what it reads about each. The default hub has the
/// ones its parent gives ([`SubagentParent::agent_tool_options`]); none unless
/// it gives some.
///
/// The default kind is always one of the kinds, so building them cannot fail.
///
/// ```
/// use haest_tools::SubagentTypes;
///
/// let kinds = SubagentTypes::new("explore")
///     .with_kinds(["explore", "general"])
///     .with_description("explore only reads");
/// assert_eq!(kinds.default_kind(), "explore");
/// assert_eq!(kinds.names(), ["explore", "general"]);
/// // A default left out of the kinds is one of them, first.
/// assert_eq!(SubagentTypes::new("general").with_kinds(["explore"]).names(), ["general", "explore"]);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentTypes {
    names: Vec<String>,
    default: String,
    description: String,
}

impl SubagentTypes {
    /// One kind, `default`: what an Agent call that names none gets. Add the
    /// others with [`with_kinds`](Self::with_kinds).
    pub fn new(default: impl Into<String>) -> Self {
        let default = default.into();
        Self {
            names: vec![default.clone()],
            default,
            description: String::new(),
        }
    }

    /// The same, the model offered `kinds` (the parameter's `enum`), in this
    /// order; the default kind first when it is not among them.
    #[must_use]
    pub fn with_kinds(mut self, kinds: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut names: Vec<String> = Vec::new();
        for kind in kinds.into_iter().map(Into::into) {
            if !names.contains(&kind) {
                names.push(kind);
            }
        }
        if !names.contains(&self.default) {
            names.insert(0, self.default.clone());
        }
        self.names = names;
        self
    }

    /// The same, with the parameter's description: what each kind may do.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// The kinds the model may name. An Agent call that names another is an
    /// input error.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// What an Agent call that names no kind gets; one of [`names`](Self::names).
    pub fn default_kind(&self) -> &str {
        &self.default
    }

    /// The parameter's description: what each kind may do.
    pub fn description(&self) -> &str {
        &self.description
    }
}

/// PLUG POINT: what a host changes about the agent tools ([`agent_tools`]).
///
/// The default: the Agent tool, haest's own `model` text, no kinds, no
/// budgets. The `with_*` setters change each.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentToolOptions {
    /// The Agent tool is there.
    can_spawn: bool,
    /// Adds the `subagent_type` parameter.
    types: Option<SubagentTypes>,
    /// None = haest's own.
    model_description: Option<String>,
    /// None = no `budget` parameter.
    budget_caps: Option<SubagentBudget>,
}

impl Default for AgentToolOptions {
    fn default() -> Self {
        Self {
            can_spawn: true,
            types: None,
            model_description: None,
            budget_caps: None,
        }
    }
}

impl AgentToolOptions {
    /// Whether the caller may still start subagents: the Agent tool is there.
    /// False at the depth cap (SendMessage and StopAgent stay). True by
    /// default; [`SubagentHub::tools_for`] sets it from its caps.
    #[must_use]
    pub fn with_can_spawn(mut self, can_spawn: bool) -> Self {
        self.can_spawn = can_spawn;
        self
    }

    /// The host's kinds of subagent: adds the `subagent_type` parameter.
    #[must_use]
    pub fn with_types(mut self, types: SubagentTypes) -> Self {
        self.types = Some(types);
        self
    }

    /// The `model` parameter's description, e.g. the models this host
    /// offers, in place of haest's own ("provider:model to run it on ...").
    #[must_use]
    pub fn with_model_description(mut self, description: impl Into<String>) -> Self {
        self.model_description = Some(description.into());
        self
    }

    /// The Agent tool takes a `budget {usd, minutes}`, each value clamped to
    /// `caps` (it only tightens; an absent one is the cap), and the time must
    /// fit what is left of the caller's turn. Without it there is no such
    /// parameter (Claude Code's schema). [`SubagentHub::tools_for`] sets it
    /// from its caps when [`SubagentCaps::budgets`] is on, and clears it when
    /// off.
    #[must_use]
    pub fn with_budget_caps(mut self, caps: SubagentBudget) -> Self {
        self.budget_caps = Some(caps);
        self
    }
}

/// An agent tool's refusal, for the model to correct: it goes back as an
/// input error.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    /// The caller already runs as deep as agents may nest.
    #[error("agents may nest {max_depth} deep")]
    TooDeep {
        /// How deep agents may nest ([`SubagentCaps::max_depth`]).
        max_depth: u32,
    },
    /// As many subagents run as may at once. One StopAgent stopped counts
    /// until its turn ends, so the text names both.
    #[error(
        "{running} subagents are running or stopping, at most {max} at once; wait for one to end, or StopAgent one still running"
    )]
    TooMany {
        /// How many run now, those stopping included.
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
    /// The caller's turn is winding down: spend crossed its share of a cap
    /// ([`haest_core::wind_down`]), so no new subagent starts. The text states
    /// what was spent of which cap, then the host's remedy.
    #[error("{}", .0.refusal("no new subagents"))]
    WindingDown(WindingDown),
}

/// What a spawn hands back to the caller's turn.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SpawnOutcome {
    /// The agent's final answer (or, in the background, that it started).
    pub text: String,
    /// What it spent, already charged to the ledger by its own loop.
    pub cost_usd: f64,
}

impl SpawnOutcome {
    /// The answer `text`, having spent `cost_usd`.
    pub fn new(text: impl Into<String>, cost_usd: f64) -> Self {
        Self {
            text: text.into(),
            cost_usd,
        }
    }
}

/// What the agent tools need from a hub. An Err is an [`AgentError`] (a cap,
/// an unknown name) the model corrects: it goes back as an input error.
///
/// [`SubagentHub`] is the default; a host with its own idea of a subagent
/// (one that runs elsewhere, with its own types) implements this and keeps
/// the tools ([`agent_tools`]). None of them is read-only:
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
/// use async_trait::async_trait;
/// use haest_core::{tool::{JsonMap, ToolContext}, workspace::ShutWorkspace};
/// use haest_tools::{
///     AgentDirectory, AgentError, AgentMessage, AgentRef, AgentToolOptions, SpawnInput,
///     SpawnOutcome, agent_tools,
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
///         _cancel: &CancellationToken,
///     ) -> Result<SpawnOutcome, AgentError> {
///         // Here the host would run `input.prompt` somewhere.
///         let text = format!("{} asked: {}", from.name, input.description);
///         Ok(SpawnOutcome::new(text, 0.01))
///     }
///     fn deliver(&self, message: AgentMessage) -> Result<String, AgentError> {
///         Err(AgentError::UnknownAgent { name: message.to, running: vec!["main".into()] })
///     }
///     fn stop(&self, _name: &str) -> bool {
///         false
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// let tools = agent_tools(Remote, AgentRef::main(), AgentToolOptions::default());
/// let ctx = ToolContext::new("/", ShutWorkspace);
/// let input: JsonMap =
///     serde_json::from_value(json!({"description": "find the bug", "prompt": "Look at parse.rs."}))?;
/// let out = tools[0].execute(input, &ctx).await?;
/// assert_eq!(out.content, "main asked: find the bug");
/// // Its spend counts toward the caller's turn, never charged twice.
/// assert_eq!(out.child_cost_usd, Some(0.01));
/// # Ok(())
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
        cancel: &CancellationToken,
    ) -> Result<SpawnOutcome, AgentError>;
    /// SendMessage: hand `message` to the agent it is for, and say what to
    /// tell its sender ([`Sender::Agent`], the agent that called the tool).
    /// The SendMessage tool refuses a message to its own sender itself.
    ///
    /// # Errors
    ///
    /// Why the agent it is for cannot take the message, which the sender
    /// reads: the [`SubagentHub`]'s are [`AgentError::UnknownAgent`] and
    /// [`AgentError::Finished`].
    fn deliver(&self, message: AgentMessage) -> Result<String, AgentError>;
    /// StopAgent: stop the agent `name`; false when it is not running.
    fn stop(&self, name: &str) -> bool;
}

haest_core::plug_ref! {
    /// An [`AgentDirectory`], shared: what the agent tools speak to
    /// ([`agent_tools`]). Built from the directory itself (a
    /// [`SubagentHub`] clone, a host's own), or from an `Arc` of one.
    pub struct AgentDirectoryRef(dyn AgentDirectory);
}
