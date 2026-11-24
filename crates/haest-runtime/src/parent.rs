//! Subagents: the library's `SubagentHub` over this conversation. A
//! subagent gets the conversation's deps, the
//! mod-wrapped base tools (Code included) behind its own deferral, the system
//! prompt for ITS model and tools plus what the mods append, and runs on the
//! conversation's current model and effort unless the call names one. Its life
//! goes to the [`Ui`](crate::Ui).

use std::sync::Arc;

use async_trait::async_trait;
use haest_core::{
    Effort, SharedDeps, SystemPrompt, ToolRef, ToolSourceRef, Tools, deferred_tools::ToolDeferral,
    tool::ToolSource,
};
use haest_mods::{ModSystemPrompt, Mods};
use haest_tools::{DeferredTools, Inbox, SubagentEvent, SubagentHub, SubagentParent};

use crate::{
    RuntimeHooks,
    inbox::InboxSender,
    prompt::{AgentPrompt, RuntimePrompt},
    tools::WrappedTools,
};

/// The runtime's [`SubagentParent`]: what a conversation's subagents are
/// made from, behind its [`SubagentHub`].
///
/// [`build`](crate::build) fills one in and [`hub`](Self::hub) turns it into
/// the hub, which is [`Runtime::subagents`](crate::Runtime::subagents) (the
/// parent itself is then the hub's).
pub(crate) struct RuntimeParent {
    /// The runtime's hooks: the settings' caps, the current model and
    /// effort, the [`Ui`](crate::Ui) a subagent's life goes to.
    pub(crate) hooks: RuntimeHooks,
    /// What every agent of the conversation runs on (models, workspace,
    /// hooks, ledger, ...); each agent's tools and prompt are its own.
    pub(crate) deps: SharedDeps,
    /// The tools a subagent starts from: every tool but the agent tools,
    /// inside the mods' hooks.
    pub(crate) tools: Arc<WrappedTools>,
    /// The system prompt's fixed parts; each subagent's names its own model
    /// and tools.
    pub(crate) prompt: Arc<RuntimePrompt>,
    /// The mods: their hooks wrap a subagent's tools and add to its prompt.
    pub(crate) mods: Mods,
    /// Where a subagent's message for main, or a background subagent's
    /// answer, goes.
    pub(crate) inbox: InboxSender,
}

impl RuntimeParent {
    /// The hub, capped by the settings' `subagents` (how deep they nest, how
    /// many run at once, each one's spend, steps and time).
    pub(crate) fn hub(self) -> SubagentHub {
        let caps = self.hooks.settings().subagents;
        SubagentHub::new(self, caps)
    }
}

#[warn(clippy::missing_trait_methods)]
impl Inbox for RuntimeParent {
    fn submit(&self, text: String) {
        self.inbox.submit(text);
    }
    fn submit_answer(&self, text: String) {
        self.inbox.submit_answer(text);
    }
}

#[async_trait]
impl SubagentParent for RuntimeParent {
    fn deps(&self) -> SharedDeps {
        self.deps.clone()
    }

    fn base_tools(&self) -> Tools {
        self.tools.tools()
    }

    async fn system_prompt(&self, model_spec: &str, tools: &[ToolRef]) -> String {
        let prompt = AgentPrompt {
            prompt: self.prompt.clone(),
            model_spec: Some(model_spec.to_string()),
            tools: Tools::from(tools.to_vec()).into(),
        };
        ModSystemPrompt::new(prompt, self.mods.clone())
            .system_prompt()
            .await
    }

    fn default_model(&self) -> (String, Option<Effort>) {
        (self.hooks.model_spec(), self.hooks.effort())
    }

    /// The agent's fixed list behind its own deferral (ToolSearch loads per agent).
    fn tool_source(&self, tools: Tools) -> ToolSourceRef {
        let deferral = ToolDeferral::new(self.hooks.settings().deferred_tools);
        DeferredTools::new(tools, deferral).into()
    }

    fn on_activity(&self, ids: &[String]) {
        self.hooks.ui().subagents_running(ids);
    }

    fn on_subagent(&self, id: &str, event: SubagentEvent) {
        self.hooks.ui().subagent(id, &event);
    }
}
