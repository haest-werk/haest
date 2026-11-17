//! What the crate's tests serve: a session over a scripted model, with a
//! tool that runs until it is let go or stopped.

use std::sync::Arc;

use async_trait::async_trait;
use haest_core::{
    Agent, AgentDeps, AgentOptions, JsonMap, ShutWorkspace, Tool, ToolContext, ToolError,
    ToolOutput, ToolRef, Tools,
    testing::{ScriptedProvider, text_answer},
};
use haest_session::{Hub, SessionHandle, start};
use serde_json::{Value, json};
use tokio::sync::Notify;

/// A tool that answers once it is let go (or its call is stopped).
pub(crate) struct Wait(Arc<Notify>);

#[async_trait]
impl Tool for Wait {
    fn name(&self) -> &str {
        "Wait"
    }
    fn description(&self) -> String {
        "Waits until it is let go.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "wait"
    }
    async fn execute(&self, _: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        tokio::select! {
            _ = self.0.notified() => Ok(ToolOutput::text("waited")),
            _ = ctx.cancel().cancelled() => Ok(ToolOutput::text("stopped")),
        }
    }
}

/// A session over a scripted model, with the Wait tool.
pub(crate) fn session(model: &ScriptedProvider, hub: Hub) -> (SessionHandle<Agent>, Arc<Notify>) {
    let go = Arc::new(Notify::new());
    let tools: Tools = Tools::from_iter([ToolRef::new(Wait(go.clone()))]);
    let shared = haest_core::SharedDeps::new(model.resolver(), "/", ShutWorkspace);
    let deps = AgentDeps::new(shared, tools, "");
    let agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    (start(agent, hub), go)
}

pub(crate) fn hub() -> Hub {
    Hub::new("s1")
}

pub(crate) fn answers(texts: &[&str]) -> ScriptedProvider {
    ScriptedProvider::new(texts.iter().map(|t| text_answer(t)))
}
