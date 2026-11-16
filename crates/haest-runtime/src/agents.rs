//! Subagents: the library's `SubagentHub` over this conversation (TS
//! `host/agent-hub.ts`). A subagent gets the conversation's deps, the
//! mod-wrapped base tools (Code included) behind its own deferral, the system
//! prompt for ITS model and tools plus what the mods append, and runs on the
//! conversation's current model and effort unless the call names one. Its life
//! goes to the [`Ui`](crate::Ui).

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    Host,
    haest::{
        AgentDeps, Effort, SystemPrompt,
        deferred_tools::ToolDeferral,
        mods::{ModSystemPrompt, Mods},
        tool::{Tool, ToolSource},
        tools::{
            Inbox,
            agents::{SubagentEvent, SubagentHost, SubagentHub},
            tool_search::DeferredTools,
        },
    },
    inbox::InboxSender,
    prompt::{AgentPrompt, RuntimePrompt},
    tools::WrappedTools,
};

pub struct Agents {
    pub host: Arc<Host>,
    /// The conversation's deps. Their tools and prompt are left out (empty):
    /// the hub replaces both for every agent, and main's tools hold the hub.
    pub deps: AgentDeps,
    pub tools: Arc<WrappedTools>,
    pub prompt: Arc<RuntimePrompt>,
    pub mods: Arc<Mods>,
    pub inbox: Arc<InboxSender>,
}

impl Agents {
    /// The hub, capped by the settings' `subagents`.
    pub fn hub(self) -> Arc<SubagentHub> {
        let caps = self.host.settings.subagents;
        SubagentHub::new(Arc::new(self), caps)
    }
}

impl Inbox for Agents {
    fn submit(&self, text: String) {
        self.inbox.submit(text);
    }
}

#[async_trait]
impl SubagentHost for Agents {
    fn deps(&self) -> AgentDeps {
        self.deps.clone()
    }

    fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.tools()
    }

    async fn system_prompt(&self, model_spec: &str, tools: &[Arc<dyn Tool>]) -> String {
        let prompt = AgentPrompt {
            prompt: self.prompt.clone(),
            model: Some(model_spec.to_string()),
            tools: Arc::new(tools.to_vec()),
        };
        ModSystemPrompt::new(Arc::new(prompt), self.mods.clone())
            .system_prompt()
            .await
    }

    fn default_model(&self) -> (String, Option<Effort>) {
        (self.host.model(), self.host.effort())
    }

    /// The agent's fixed list behind its own deferral (ToolSearch loads per agent).
    fn tool_source(&self, tools: Vec<Arc<dyn Tool>>) -> Arc<dyn ToolSource> {
        let deferral = ToolDeferral::new(self.host.settings.deferred_tools);
        Arc::new(DeferredTools::new(tools, deferral))
    }

    fn on_activity(&self, running: &[String]) {
        self.host.ui.subagents_running(running);
    }

    fn on_subagent(&self, id: &str, event: SubagentEvent) {
        self.host.ui.subagent(id, &event);
    }
}
