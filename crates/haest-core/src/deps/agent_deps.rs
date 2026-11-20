//! [`AgentDeps`]: one agent's tools and system prompt, and what it shares.

use super::{SharedDeps, SystemPromptRef};
use crate::tool::ToolSourceRef;

/// Everything the loop needs: what [`Agent::new`](crate::Agent::new) takes.
/// An agent's own tools and system prompt, and what it runs on besides
/// ([`SharedDeps`]), which the agents of one conversation share.
///
/// [`AgentDeps::new`] takes what the conversation runs on (a [`SharedDeps`]:
/// models, a working directory, a workspace) and the agent's own tools and
/// system prompt, each as a plain value; the `with_*` setters set the rest,
/// the shared slots included. Cheap to clone. The loop reads the tools and
/// the prompt again before every model call, so a source that changes
/// behind its handle is seen at the next call.
///
/// # Examples
///
/// ```
/// use haest_core::{
///     AgentDeps, ModelError, ResolvedModel, SharedDeps, ShutWorkspace, Tools, TurnCaps,
/// };
///
/// // A resolver that serves nothing; a host plugs in haest-providers' registry.
/// let models = |spec: &str| -> Result<ResolvedModel, ModelError> {
///     Err(ModelError::BadSpec { spec: spec.into() })
/// };
/// let shared = SharedDeps::new(models, "/project", ShutWorkspace);
/// let deps = AgentDeps::new(shared, Tools::new(), "You are terse.")
///     .with_turn_caps(TurnCaps { max_turn_steps: 10, ..TurnCaps::default() });
/// assert_eq!(deps.shared().cwd(), std::path::Path::new("/project"));
/// ```
#[derive(Clone, Debug)]
pub struct AgentDeps {
    tools: ToolSourceRef,
    system_prompt: SystemPromptRef,
    /// `pub(super)`: `shared.rs` forwards each shared setter onto it.
    pub(super) shared: SharedDeps,
}

impl AgentDeps {
    /// An agent of the conversation `shared` runs on, with its own `tools`
    /// and `system_prompt`. A subagent is built the same way, from its
    /// parent's [`shared`](Self::shared).
    pub fn new(
        shared: SharedDeps,
        tools: impl Into<ToolSourceRef>,
        system_prompt: impl Into<SystemPromptRef>,
    ) -> Self {
        Self {
            tools: tools.into(),
            system_prompt: system_prompt.into(),
            shared,
        }
    }

    /// The tools: which to advertise, and which a call may run.
    pub fn tools(&self) -> &ToolSourceRef {
        &self.tools
    }

    /// The system prompt, built again before every model call.
    pub fn system_prompt(&self) -> &SystemPromptRef {
        &self.system_prompt
    }

    /// The models, the workspace, the host's hooks and the rest.
    pub fn shared(&self) -> &SharedDeps {
        &self.shared
    }

    /// These deps with `tools`.
    #[must_use]
    pub fn with_tools(mut self, tools: impl Into<ToolSourceRef>) -> Self {
        self.tools = tools.into();
        self
    }

    /// These deps with `system_prompt`.
    #[must_use]
    pub fn with_system_prompt(mut self, system_prompt: impl Into<SystemPromptRef>) -> Self {
        self.system_prompt = system_prompt.into();
        self
    }
}

// Every `SharedDeps` setter is on `AgentDeps` too, from the one list in
// `shared.rs`.
