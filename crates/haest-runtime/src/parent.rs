//! Subagents: the library's `SubagentHub` over this conversation (TS
//! `host/agent-hub.ts`). A subagent gets the conversation's deps, the
//! mod-wrapped base tools (Code included) behind its own deferral, the system
//! prompt for ITS model and tools plus what the mods append, and runs on the
//! conversation's current model and effort unless the call names one. Its life
//! goes to the [`Ui`](crate::Ui).

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    RuntimeHooks,
    haest::{
        Effort, SharedDeps, SystemPrompt,
        core::deferred_tools::ToolDeferral,
        core::tool::{Tool, ToolSource},
        mods::{ModSystemPrompt, Mods},
        tools::{DeferredTools, Inbox, SubagentEvent, SubagentHub, SubagentParent},
    },
    inbox::InboxSender,
    prompt::{AgentPrompt, RuntimePrompt},
    tools::WrappedTools,
};

/// The runtime's [`SubagentParent`]: what a conversation's subagents are
/// made from, behind its [`SubagentHub`].
///
/// [`build`](crate::build) fills one in and [`hub`](Self::hub) turns it into
/// the hub, which is [`Runtime::subagents`](crate::Runtime::subagents) (the
/// parent itself is then the hub's). A host that takes the pieces fills one
/// in itself.
pub struct RuntimeParent {
    /// The runtime's hooks: the settings' caps, the current model and
    /// effort, the [`Ui`](crate::Ui) a subagent's life goes to.
    pub hooks: Arc<RuntimeHooks>,
    /// What every agent of the conversation runs on (models, workspace,
    /// hooks, ledger, ...); each agent's tools and prompt are its own.
    pub deps: SharedDeps,
    /// The tools a subagent starts from: every tool but the agent tools,
    /// inside the mods' hooks.
    pub tools: Arc<WrappedTools>,
    /// The system prompt's fixed parts; each subagent's names its own model
    /// and tools.
    pub prompt: Arc<RuntimePrompt>,
    /// The mods: their hooks wrap a subagent's tools and add to its prompt.
    pub mods: Arc<Mods>,
    /// Where a subagent's message for main, or a background subagent's
    /// answer, goes.
    pub inbox: Arc<InboxSender>,
}

impl RuntimeParent {
    /// The hub, capped by the settings' `subagents` (how deep they nest, how
    /// many run at once, each one's spend, steps and time).
    pub fn hub(self) -> Arc<SubagentHub> {
        let caps = self.hooks.settings.subagent_caps();
        SubagentHub::new(Arc::new(self), caps)
    }
}

impl Inbox for RuntimeParent {
    fn submit(&self, text: String) {
        self.inbox.submit(text);
    }
}

#[async_trait]
impl SubagentParent for RuntimeParent {
    fn deps(&self) -> SharedDeps {
        self.deps.clone()
    }

    fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.tools()
    }

    async fn system_prompt(&self, model_spec: &str, tools: &[Arc<dyn Tool>]) -> String {
        let prompt = AgentPrompt {
            prompt: self.prompt.clone(),
            model_spec: Some(model_spec.to_string()),
            tools: Arc::new(tools.to_vec()),
        };
        ModSystemPrompt::new(Arc::new(prompt), self.mods.clone())
            .system_prompt()
            .await
    }

    fn default_model(&self) -> (String, Option<Effort>) {
        (self.hooks.model_spec(), self.hooks.effort())
    }

    /// The agent's fixed list behind its own deferral (ToolSearch loads per agent).
    fn tool_source(&self, tools: Vec<Arc<dyn Tool>>) -> Arc<dyn ToolSource> {
        let deferral = ToolDeferral::new(self.hooks.settings.deferred_tools);
        Arc::new(DeferredTools::new(tools, deferral))
    }

    fn on_activity(&self, ids: &[String]) {
        self.hooks.ui.subagents_running(ids);
    }

    fn on_subagent(&self, id: &str, event: SubagentEvent) {
        self.hooks.ui.subagent(id, &event);
    }
}
