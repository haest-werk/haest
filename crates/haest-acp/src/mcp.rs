//! The MCP servers a client hands over, as haest starts them.

use agent_client_protocol::v1::schema::McpServer;
use haest_mcp::{McpServerConfig, McpTransport};

/// `servers` as haest's MCP server entries: a command over stdio, or a URL
/// over streamable HTTP with its headers. An SSE server (haest never says it
/// takes one) or a kind newer than this crate is refused; the error says
/// which, never its headers or environment (they may hold a secret).
pub(crate) fn servers(servers: Vec<McpServer>) -> Result<Vec<McpServerConfig>, String> {
    servers
        .into_iter()
        .map(|server| match server {
            McpServer::Stdio(s) => {
                let mut stdio = McpTransport::stdio(s.command.to_string_lossy()).with_args(s.args);
                for var in s.env {
                    stdio = stdio.with_env_var(var.name, var.value);
                }
                Ok(McpServerConfig::new(s.name, stdio))
            }
            McpServer::Http(s) => {
                let mut http = McpTransport::http(s.url);
                for header in s.headers {
                    http = http.with_header(header.name, header.value);
                }
                Ok(McpServerConfig::new(s.name, http))
            }
            McpServer::Sse(s) => Err(format!(
                "MCP server {}: haest takes stdio and streamable HTTP servers, not SSE",
                s.name
            )),
            _ => Err("an MCP server over a transport haest does not take".to_string()),
        })
        .collect()
}
