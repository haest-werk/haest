//! A tool a file mod added with `haest.tool(t)`: advertised from what it
//! registered, executed in the mod's own runtime.

use std::sync::Arc;

use async_trait::async_trait;
use haest_core::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput};
use haest_mods::ToolCallContext;
use serde_json::{Value, json};

use super::channel::{CallError, JsThread, ToolMeta};

pub(crate) struct JsTool {
    meta: ToolMeta,
    thread: Arc<JsThread>,
}

impl JsTool {
    pub(crate) fn new(meta: ToolMeta, thread: Arc<JsThread>) -> Self {
        Self { meta, thread }
    }
}

#[async_trait]
impl Tool for JsTool {
    fn name(&self) -> &str {
        &self.meta.name
    }
    fn description(&self) -> String {
        self.meta.description.clone()
    }
    fn parameters(&self) -> Value {
        self.meta.parameters.clone()
    }
    fn kind(&self) -> &str {
        &self.meta.kind
    }
    fn read_only(&self) -> bool {
        self.meta.read_only
    }

    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let ctx_seen = ToolCallContext::new(ctx);
        let payload = json!({ "name": self.meta.name, "input": input, "ctx": ctx_seen });
        match self
            .thread
            .call("execute", payload, Some(ctx.cancel()))
            .await
        {
            Ok(v) => serde_json::from_value(v)
                .map_err(|e| ToolError::Failed(format!("its result is not a tool output: {e}"))),
            Err(CallError::Interrupted) => Ok(ToolOutput::error(
                "Interrupted by the user while this tool ran.",
            )),
            Err(CallError::Failed(message)) => Err(ToolError::Failed(message)),
            Err(CallError::Stopped) => Err(ToolError::Failed(
                "the mod that adds this tool is no longer running".into(),
            )),
        }
    }
}
