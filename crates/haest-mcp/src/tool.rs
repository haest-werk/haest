//! One MCP server tool as a haest [`Tool`].

use std::collections::HashSet;

use async_trait::async_trait;
use haest_core::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput};
use rmcp::{
    RoleClient,
    model::{CallToolRequestParams, CallToolResponse},
    service::{Peer, ServiceError},
};
use serde_json::Value;

use crate::{hub::CALL_TIMEOUT, result::mcp_result_to_output};

/// The longest tool name every provider accepts.
const MAX_NAME: usize = 64;
/// `_` and 8 hex digits: what a name that is too long or already taken ends in.
const TAG_LEN: usize = 9;

/// The name the model sees for `server`'s tool `tool`: `mcp__<server>__<tool>`
/// as every provider accepts it.
///
/// Only `A-Z`, `a-z`, `0-9`, `_` and `-` are kept; any other character becomes
/// `_`, as Claude Code does. A name over 64 characters is cut and tagged with
/// `_` and 8 hex digits, a hash of the two original names, so the same tool
/// gets the same name every run (a resumed session's calls still match).
///
/// This is the name a tool gets when no other tool in its [`McpHub`] has it.
/// When one already does (two servers or tools that differ only in characters
/// a provider rejects, or past the 64th), the later one gets a tagged name
/// instead. The tool keeps its own name for calls to its server.
///
/// [`McpHub`]: crate::McpHub
///
/// ```
/// use haest_mcp::mcp_tool_name;
///
/// assert_eq!(mcp_tool_name("github", "create_issue"), "mcp__github__create_issue");
/// assert_eq!(mcp_tool_name("docs.site", "search docs"), "mcp__docs_site__search_docs");
///
/// let long = mcp_tool_name("server", &"t".repeat(100));
/// assert_eq!(long.len(), 64);
/// assert_eq!(long, mcp_tool_name("server", &"t".repeat(100)));
/// ```
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    unique_tool_name(server, tool, &HashSet::new())
}

/// [`mcp_tool_name`], unless `taken` has it: then the first tagged name `taken`
/// does not have. The tool keeps its own name for calls; this one is the model's.
pub(crate) fn unique_tool_name(server: &str, tool: &str, taken: &HashSet<String>) -> String {
    let plain: String = format!("mcp__{server}__{tool}")
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '-' => c,
            _ => '_',
        })
        .collect();
    if plain.len() <= MAX_NAME && !taken.contains(&plain) {
        return plain;
    }
    // ASCII by now, so a byte cut is a character cut.
    let stem = &plain[..plain.len().min(MAX_NAME - TAG_LEN)];
    (0u32..)
        .map(|salt| format!("{stem}_{:08x}", fnv1a(&[server, "\0", tool], salt)))
        .find(|name| !taken.contains(name))
        .expect("a u32 of salts outlasts any server's tool list")
}

/// FNV-1a over the parts then the salt: stable across builds and platforms, so
/// a tagged name is the same every run (a resumed session's calls still match).
fn fnv1a(parts: &[&str], salt: u32) -> u32 {
    let bytes = parts
        .iter()
        .flat_map(|p| p.bytes())
        .chain(salt.to_le_bytes());
    bytes.fold(0x811c_9dc5, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// The [`kind`](haest_core::tool::Tool::kind) of `server`'s tool `tool`:
/// `mcp.<server>.<tool>`, with both names as the server gives them.
///
/// Unlike the name the model sees ([`mcp_tool_name`]), the kind keeps the
/// original names, unchanged and uncut.
pub(crate) fn mcp_kind(server: &str, tool: &str) -> String {
    format!("mcp.{server}.{tool}")
}

/// A tool of a connected MCP server, as a haest [`Tool`].
///
/// [`McpHub::tools`](crate::McpHub::tools) hands them out, as `ToolRef`;
/// there is no other way to get one. Its name is the hub's
/// ([`mcp_tool_name`]), its kind [`mcp_kind`], its description and input
/// schema the server's (a tool the server did not describe says what it is
/// and which server it is on), and it is
/// [`read_only`](Tool::read_only) only when the server's `readOnlyHint` says
/// so.
///
/// A call goes to the server for as long as it is connected, and fails once
/// the hub has closed. It fails after [`CALL_TIMEOUT`](crate::CALL_TIMEOUT),
/// and at once when the call's cancel token fires; either way the server is
/// told the call was cancelled. A result the server marks `isError` is an
/// error output the model reads ([`mcp_result_to_output`]); a JSON-RPC error,
/// or a server that asks for input or answers with a task, is a failed call.
///
/// [`mcp_result_to_output`]: crate::mcp_result_to_output
pub(crate) struct McpTool {
    server: String,
    /// The tool's own name on its server: what a call asks for.
    tool: String,
    /// The name the model sees, unique in its hub ([`unique_tool_name`]).
    name: String,
    kind: String,
    read_only: bool,
    description: String,
    parameters: Value,
    peer: Peer<RoleClient>,
}

impl McpTool {
    pub(crate) fn new(
        server: &str,
        name: String,
        t: rmcp::model::Tool,
        peer: Peer<RoleClient>,
    ) -> Self {
        let description = t.description.as_deref().map_or_else(
            || format!("{} (MCP server {server})", t.name),
            str::to_string,
        );
        Self {
            server: server.to_string(),
            name,
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
            // haest answers no server-side input requests and polls no tasks.
            CallToolResponse::InputRequired(_) => Err(ToolError::Failed(
                "the MCP server asked for input haest cannot give".into(),
            )),
            CallToolResponse::Task(_) => Err(ToolError::Failed(
                "the MCP server answered with a task, which haest does not poll".into(),
            )),
            _ => Err(ToolError::Failed(
                "the MCP server answered with a result haest does not know".into(),
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
        // What Claude Code calls them, so an allowlist written for it still matches.
        assert_eq!(mcp_tool_name("docs.site", "a b/é"), "mcp__docs_site__a_b__");
        let long = mcp_tool_name("server", &"t".repeat(100));
        assert_eq!(long.len(), 64);
        assert!(long.starts_with("mcp__server__t"));
        // The same tool gets the same name every run.
        assert_eq!(long, mcp_tool_name("server", &"t".repeat(100)));
    }

    #[test]
    fn a_taken_name_is_tagged_and_the_tag_differs_per_original() {
        let plain = mcp_tool_name("s", "a.b");
        let taken = HashSet::from([plain.clone()]);
        let dot = unique_tool_name("s", "a.b", &taken);
        let underscore = unique_tool_name("s", "a_b", &taken);
        assert!(!taken.contains(&dot) && !taken.contains(&underscore));
        assert_ne!(dot, underscore);
        assert!(dot.len() <= 64 && underscore.len() <= 64);
        // Even when the tagged name is taken too, there is a free one.
        let both = HashSet::from([plain, dot]);
        assert!(!both.contains(&unique_tool_name("s", "a.b", &both)));
    }
}
