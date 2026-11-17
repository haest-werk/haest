//! [`AgentDeps`]: one agent's tools and system prompt, and what it shares.

use std::path::PathBuf;

use super::{AgentHooksRef, ClockRef, SharedDeps, SystemPromptRef};
use crate::{
    caps::TurnCapsSourceRef,
    catalog::ModelFactsRef,
    ledger::Ledger,
    provider::{ModelResolverRef, WebSearchOptions},
    remedy::Remedies,
    run::RunStoreRef,
    threshold::{Thresholds, TurnSite},
    tool::{ToolServicesRef, ToolSourceRef},
    workspace::WorkspaceRef,
};

/// Everything the loop needs: what [`Agent::new`](crate::Agent::new) takes.
/// An agent's own tools and system prompt, and what it runs on besides
/// ([`SharedDeps`]), which the agents of one conversation share.
///
/// [`AgentDeps::new`] takes the five things every agent needs, each as a
/// plain value; the `with_*` setters set the rest, the shared slots
/// included. Cheap to clone. The loop reads the tools and the prompt again
/// before every model call, so a source that changes behind its handle is
/// seen at the next call.
///
/// # Examples
///
/// ```
/// use haest_core::{AgentDeps, ModelError, ResolvedModel, ShutWorkspace, Tools, TurnCaps};
///
/// // A resolver that serves nothing; a host plugs in haest-providers' registry.
/// let models = |spec: &str| -> Result<ResolvedModel, ModelError> {
///     Err(ModelError::BadSpec { spec: spec.into() })
/// };
/// let deps = AgentDeps::new(models, Tools::new(), "You are terse.", "/project", ShutWorkspace)
///     .with_turn_caps(TurnCaps { max_turn_steps: 10, ..TurnCaps::default() });
/// assert_eq!(deps.shared().cwd(), std::path::Path::new("/project"));
/// ```
#[derive(Clone, Debug)]
pub struct AgentDeps {
    tools: ToolSourceRef,
    system_prompt: SystemPromptRef,
    shared: SharedDeps,
}

impl AgentDeps {
    /// The minimum: models, tools, a system prompt, a working directory and
    /// a workspace; the rest as [`SharedDeps::new`] leaves it.
    pub fn new(
        models: impl Into<ModelResolverRef>,
        tools: impl Into<ToolSourceRef>,
        system_prompt: impl Into<SystemPromptRef>,
        cwd: impl Into<PathBuf>,
        workspace: impl Into<WorkspaceRef>,
    ) -> Self {
        Self::from_shared(
            SharedDeps::new(models, cwd, workspace),
            tools,
            system_prompt,
        )
    }

    /// An agent of the conversation `shared` runs on, with its own tools and
    /// system prompt: how a subagent is built from its parent's deps.
    pub fn from_shared(
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

    /// These deps on `shared`.
    #[must_use]
    pub fn with_shared(mut self, shared: SharedDeps) -> Self {
        self.shared = shared;
        self
    }
}

/// Each `with_*` of [`SharedDeps`], on an agent's shared deps.
macro_rules! forward {
    ($($name:ident($arg:ident: $ty:ty)),* $(,)?) => {
        impl AgentDeps {
            $(
                #[doc = concat!("[`SharedDeps::", stringify!($name), "`], on these deps' shared part.")]
                #[must_use]
                pub fn $name(mut self, $arg: $ty) -> Self {
                    self.shared = self.shared.$name($arg);
                    self
                }
            )*
        }
    };
}

forward! {
    with_models(models: impl Into<ModelResolverRef>),
    with_cwd(cwd: impl Into<PathBuf>),
    with_workspace(workspace: impl Into<WorkspaceRef>),
    with_spill_dir(dir: impl Into<PathBuf>),
    with_hooks(hooks: impl Into<AgentHooksRef>),
    with_services(services: impl Into<ToolServicesRef>),
    with_facts(facts: impl Into<ModelFactsRef>),
    with_turn_caps(caps: impl Into<TurnCapsSourceRef>),
    with_clock(clock: impl Into<ClockRef>),
    with_ledger(ledger: Ledger),
    with_web_search(web_search: impl Into<Option<WebSearchOptions>>),
    with_remedies(remedies: Remedies),
    with_run_store(store: impl Into<RunStoreRef>),
    with_thresholds(thresholds: Thresholds<TurnSite>),
}
