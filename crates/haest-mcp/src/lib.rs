//! MCP client hub: connects to MCP servers and exposes their tools as haest
//! tools named `mcp__<server>__<tool>` (the Claude Code convention), rewritten to
//! what every provider accepts and unique across the hub ([`mcp_tool_name`]),
//! canonical kind `mcp.<server>.<tool>` (the original names).
//!
//! Sources: whatever the host adds (its own MCP endpoint, when it has one) plus the
//! project's `.mcp.json` (Claude Code format, [`load_project_mcp_config`]). A server
//! that fails to connect is logged and skipped -- one broken MCP server never
//! takes the harness down.
//!
//! PORT: `core/mcp.ts`. Built on the official Rust SDK (`rmcp`).
//!
//! ```ignore
//! let hub = Arc::new(McpHub::new());
//! let project = load_project_mcp_config(fs, cwd, &|name| env.get(name).cloned()).await?;
//! project.notes.iter().for_each(|n| eprintln!("{n}"));
//! hub.connect(&project.servers, &|m| eprintln!("{m}")).await;
//! for f in hub.failures() { /* tell the human */ }
//! // hub is a ToolSource: the loop asks it for tools before every model call.
//! hub.close().await; // or drop it: every server shuts down either way
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

mod config;
mod hub;
mod result;
mod tool;
mod transport;

#[cfg(test)]
mod test_server;

pub use config::{
    McpConfigError, McpServerConfig, McpTransport, ProjectMcp, load_project_mcp_config,
    project_mcp_path,
};
pub use hub::{CALL_TIMEOUT, CONNECT_TIMEOUT, McpFailure, McpHub};
pub use result::mcp_result_to_output;
pub use tool::{McpTool, mcp_kind, mcp_tool_name};

/// rmcp, re-exported for a host that connects its own transport ([`McpHub::connect_transport`]).
pub use rmcp;
