//! One MCP server tool as a kern [`Tool`].

use async_trait::async_trait;
use kern_core::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput};
use rmcp::{
    RoleClient,
    model::{CallToolRequestParams, CallToolResponse},
    service::{Peer, ServiceError},
};
use serde_json::Value;

use crate::{hub::CALL_TIMEOUT, result::mcp_result_to_output};

/// The longest tool name every provider accepts.
const MAX_NAME: usize = 64;

/// `mcp__<server>__<tool>`, cut to 64 characters.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
        .chars()
        .take(MAX_NAME)
        .collect()
}

/// Canonical kind of an MCP tool: `mcp.<server>.<tool>`.
pub fn mcp_kind(server: &str, tool: &str) -> String {
    format!("mcp.{server}.{tool}")
}

/// A tool of a connected MCP server. Calls go to the server for as long as it is
/// connected; after the hub closes, a call fails (the transport is gone).
pub struct McpTool {
    server: String,
    /// The tool's own name on its server.
    tool: String,
    name: String,
    kind: String,
    read_only: bool,
    description: String,
    parameters: Value,
    peer: Peer<RoleClient>,
}

impl McpTool {
    pub(crate) fn new(server: &str, t: rmcp::model::Tool, peer: Peer<RoleClient>) -> Self {
        let description = t.description.as_deref().map_or_else(
            || format!("{} (MCP server {server})", t.name),
            str::to_string,
        );
        Self {
            server: server.to_string(),
            name: mcp_tool_name(server, &t.name),
            kind: mcp_kind(server, &t.name),
            // Only the server's own word makes an MCP tool parallel-safe; unannotated runs alone.
            read_only: t
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint)
                .unwrap_or(false),
            description,
            parameters: Value::Object((*t.input_schema).clone()),
            tool: t.name.into_owned(),
            peer,
        }
    }
}

/// What a failed call says: a JSON-RPC error as `MCP error <code>: <message>`, anything else as itself.
fn call_error(e: ServiceError) -> String {
    match e {
        ServiceError::McpError(data) => format!("MCP error {}: {}", data.code.0, data.message),
        other => other.to_string(),
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> String {
        self.description.clone()
    }
    fn parameters(&self) -> Value {
        self.parameters.clone()
    }
    fn kind(&self) -> &str {
        &self.kind
    }
    fn read_only(&self) -> bool {
        self.read_only
    }

    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let params = CallToolRequestParams::new(self.tool.clone()).with_arguments(input);
        // Dropping the call (cancel, timeout) tells the server with notifications/cancelled.
        let call = tokio::time::timeout(CALL_TIMEOUT, self.peer.call_tool_once(params));
        let res = tokio::select! {
            r = call => r.map_err(|_| ToolError::Failed(format!(
                "MCP {} {} timed out after {}ms",
                self.server,
                self.tool,
                CALL_TIMEOUT.as_millis()
            )))?,
            () = ctx.cancel.cancelled() => {
                return Err(ToolError::Failed("the call was cancelled".into()));
            }
        };
        match res.map_err(|e| ToolError::Failed(call_error(e)))? {
            CallToolResponse::Complete(result) => {
                let raw = serde_json::to_value(&result)
                    .map_err(|e| ToolError::Failed(format!("unreadable MCP result: {e}")))?;
                Ok(mcp_result_to_output(&raw))
            }
            // kern answers no server-side input requests and polls no tasks.
            CallToolResponse::InputRequired(_) => Err(ToolError::Failed(
                "the MCP server asked for input kern cannot give".into(),
            )),
            CallToolResponse::Task(_) => Err(ToolError::Failed(
                "the MCP server answered with a task, which kern does not poll".into(),
            )),
            _ => Err(ToolError::Failed(
                "the MCP server answered with a result kern does not know".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_claude_code_convention_and_fit_every_provider() {
        assert_eq!(mcp_tool_name("srv", "echo"), "mcp__srv__echo");
        assert_eq!(mcp_kind("srv", "echo"), "mcp.srv.echo");
        let long = mcp_tool_name("server", &"t".repeat(100));
        assert_eq!(long.chars().count(), 64);
        assert!(long.starts_with("mcp__server__t"));
    }
}
