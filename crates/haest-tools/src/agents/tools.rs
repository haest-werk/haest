//! The agent tools. Each agent gets its own copies, bound to its name -- that is
//! the `from` on every message it sends.

use std::sync::Arc;

use async_trait::async_trait;
use haest_core::tool::{
    JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_bool, opt_str, req_str,
};
use serde_json::{Value, json};

use super::{AgentDirectory, AgentRef, AgentToolOptions, SpawnInput};

struct AgentTool {
    hub: Arc<dyn AgentDirectory>,
    me: AgentRef,
    opts: AgentToolOptions,
}

impl AgentTool {
    /// The named kind, else the host's default; None when the host has no kinds.
    fn subagent_type(&self, input: &JsonMap) -> Result<Option<String>, ToolError> {
        let Some(types) = &self.opts.types else {
            return Ok(None);
        };
        let kind = opt_str(input, "subagent_type")?.unwrap_or(&types.default);
        if !types.names.iter().any(|n| n == kind) {
            return Err(ToolError::Input(format!(
                "subagent_type \"{kind}\" is not one of: {}",
                types.names.join(", ")
            )));
        }
        Ok(Some(kind.to_string()))
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        "Agent"
    }
    fn kind(&self) -> &str {
        "task.spawn"
    }
    fn description(&self) -> String {
        "Start a subagent: a separate agent with its own context that does one self-contained task and returns its final answer to you. Use it to keep a big search or a side task out of your own context, or (run_in_background) to work in parallel -- its answer then arrives as a message. Give it a complete brief: it sees nothing of this conversation.".into()
    }
    fn parameters(&self) -> Value {
        let model = self
            .opts
            .models
            .as_deref()
            .unwrap_or("provider:model to run it on (optional; defaults to yours)");
        let mut params = json!({
            "type": "object",
            "properties": {
                "description": {"type": "string", "description": "Short (3-5 words) label for the task"},
                "prompt": {"type": "string", "description": "The complete brief: goal, what you know, what to return"},
                "name": {"type": "string", "description": "Address for SendMessage (optional; derived from description)"},
                "model": {"type": "string", "description": model},
                "run_in_background": {"type": "boolean", "description": "Return at once; the answer arrives as a message"}
            },
            "required": ["description", "prompt"]
        });
        if let Some(types) = &self.opts.types {
            params["properties"]["subagent_type"] =
                json!({"type": "string", "enum": types.names, "description": types.description});
        }
        params
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let spawn = SpawnInput {
            description: req_str(&input, "description")?.into(),
            prompt: req_str(&input, "prompt")?.into(),
            name: opt_str(&input, "name")?.map(Into::into),
            model: opt_str(&input, "model")?.map(Into::into),
            background: opt_bool(&input, "run_in_background"),
            subagent_type: self.subagent_type(&input)?,
        };
        let out = self
            .hub
            .spawn(&self.me, spawn, ctx.cancel.clone())
            .await
            .map_err(|e| ToolError::Input(e.to_string()))?;
        Ok(ToolOutput {
            content: out.text,
            child_cost_usd: Some(out.cost_usd),
            ..Default::default()
        })
    }
}

struct SendMessageTool {
    hub: Arc<dyn AgentDirectory>,
    me: AgentRef,
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "SendMessage"
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    fn description(&self) -> String {
        "Send a message to another running agent of this conversation: \"main\" (the conversation's own agent) or a subagent by name. It reads it at its next step.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to": {"type": "string", "description": "\"main\" or an agent name"},
                "message": {"type": "string"}
            },
            "required": ["to", "message"]
        })
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let to = req_str(&input, "to")?;
        if to == self.me.name {
            return Err(ToolError::Input("that is you".into()));
        }
        let message = req_str(&input, "message")?;
        let said = self
            .hub
            .deliver(&self.me.name, to, message)
            .map_err(|e| ToolError::Input(e.to_string()))?;
        Ok(ToolOutput::text(said))
    }
}

struct StopAgentTool {
    hub: Arc<dyn AgentDirectory>,
}

#[async_trait]
impl Tool for StopAgentTool {
    fn name(&self) -> &str {
        "StopAgent"
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    fn description(&self) -> String {
        "Stop a running subagent by name. A stopped foreground agent returns what it had so far."
            .into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        })
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let name = req_str(&input, "name")?;
        Ok(ToolOutput::text(if self.hub.stop(name) {
            format!("Stopped \"{name}\".")
        } else {
            format!("\"{name}\" is not running.")
        }))
    }
}

/// The agent tools `me` gets, over `hub`: Agent only when it may still spawn
/// (`can_spawn`), SendMessage and StopAgent always. Each is bound to `me`: it
/// is the `from` of every spawn and message.
///
/// [`SubagentHub::tools_for`](super::SubagentHub::tools_for) works out
/// `can_spawn` from the hub's caps.
pub fn agent_tools(
    hub: Arc<dyn AgentDirectory>,
    me: AgentRef,
    can_spawn: bool,
) -> Vec<Arc<dyn Tool>> {
    agent_tools_with(hub, me, can_spawn, AgentToolOptions::default())
}

/// [`agent_tools`] with the host's kinds of subagent and `model` text
/// ([`AgentToolOptions`]). The module docs have an example.
pub fn agent_tools_with(
    hub: Arc<dyn AgentDirectory>,
    me: AgentRef,
    can_spawn: bool,
    opts: AgentToolOptions,
) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    if can_spawn {
        tools.push(Arc::new(AgentTool {
            hub: hub.clone(),
            me: me.clone(),
            opts,
        }));
    }
    tools.push(Arc::new(SendMessageTool {
        hub: hub.clone(),
        me,
    }));
    tools.push(Arc::new(StopAgentTool { hub }));
    tools
}
