//! haest's MCP client: it connects [MCP](https://modelcontextprotocol.io)
//! servers and offers their tools to the agent as haest
//! [`Tool`](haest_core::tool::Tool)s.
//!
//! A server is reached one of three ways: a child process speaking MCP on its
//! stdin and stdout ([`McpTransport::Stdio`]), streamable HTTP
//! ([`McpTransport::Http`]), or any transport [`rmcp`] takes that the host
//! opened itself ([`McpHub::connect_transport`]: a server in the same process,
//! a socket). [`McpHub`] holds the connections and is a
//! [`ToolSource`](haest_core::tool::ToolSource): the agent loop asks it for the
//! tools before every model call, so a server connected later is offered from
//! the next call on.
//!
//! Built on the official Rust SDK, [`rmcp`]. Ported from the TypeScript
//! haest's `core/mcp.ts`.
//!
//! # Where to start
//!
//! - [`McpHub`]: [`connect`](McpHub::connect) the servers, read its
//!   [`failures`](McpHub::failures), hand it to the loop,
//!   [`close`](McpHub::close) it at the end.
//! - [`McpServerConfig`]: one server to connect, written out
//!   ([`McpServerConfig::stdio`], [`McpServerConfig::http`]) or read from a
//!   project's `.mcp.json` ([`load_project_mcp_config`]).
//! - [`mcp_tool_name`] and [`mcp_kind`]: the names a server's tools get.
//! - [`mcp_result_to_output`]: what the model reads of a tool's result.
//!
//! `haest::mcp` is this crate, behind the facade's `mcp` feature, and
//! haest-runtime connects the servers a host lists in its `RuntimeInputs` by
//! itself. Take haest-mcp directly when you build the conversation yourself,
//! or want only the MCP client beside a loop of your own.
//!
//! # Tool names
//!
//! A tool is offered as `mcp__<server>__<tool>`, Claude Code's convention, so a
//! permission list written for Claude Code still matches. Every character a
//! provider rejects becomes `_`, a name longer than 64 characters is cut and
//! tagged with a hash, and two tools that would end up with the same name are
//! told apart by a tag too ([`mcp_tool_name`]). The tool's
//! [`kind`](haest_core::tool::Tool::kind) is `mcp.<server>.<tool>`, with the
//! original names, and a call reaches the server under the tool's own name.
//! A tool is [`read_only`](haest_core::tool::Tool::read_only), and may run
//! beside other tools, only when its server says so (`readOnlyHint`). A tool
//! the server did not describe says what it is and which server it is on.
//! A result the server marks `isError` is an error output the model reads
//! ([`mcp_result_to_output`]); a JSON-RPC error, or a server that asks for
//! input or answers with a task, is a failed call, and so is any call after
//! [`McpHub::close`].
//!
//! # Failures and time limits
//!
//! A server that cannot be started, or does not connect or list its tools
//! within [`CONNECT_TIMEOUT`], is logged and kept in [`McpHub::failures`]. It
//! is never returned as an error: one broken server never takes the harness
//! down. A tool call runs for at most [`CALL_TIMEOUT`]; a call that times out
//! or is cancelled is cancelled on the server too.
//!
//! # What a server is given
//!
//! A stdio server's process gets the hub's environment ([`McpHub::with_env`];
//! none for [`McpHub::new`]) with the server's own `env` laid over it, and
//! nothing else of the host's environment: not even `PATH`, unless the host
//! hands it in. A server named by a bare command is looked up on that `PATH`,
//! or with none only in the system's default directories, and a failure to
//! find it says the `PATH` is missing. Its stderr is discarded, and it is
//! killed when its connection drops. An HTTP server's requests carry its
//! `headers`, never go through a proxy named by `HTTP_PROXY`, `HTTPS_PROXY` or
//! `ALL_PROXY`, and follow only a same-origin 307 or 308 redirect, so the
//! headers reach no one but the server.
//!
//! # Examples
//!
//! A server running in the same process, over an in-memory pipe: its tool is
//! offered under its haest name, and a call reaches it. The server side is
//! written with rmcp's `server` feature, which haest-mcp itself does not turn
//! on.
//!
//! ```
//! use std::sync::Arc;
//!
//! use haest_core::{
//!     tool::{Tool as _, ToolContext},
//!     workspace::ShutWorkspace,
//! };
//! use haest_mcp::McpHub;
//! use rmcp::{
//!     ErrorData, RoleServer, ServerHandler, ServiceExt,
//!     model::{
//!         CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
//!         InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
//!     },
//!     service::RequestContext,
//! };
//! use serde_json::json;
//!
//! /// A server with one tool, `shout`: the text it is given, in capitals.
//! struct Shout;
//!
//! impl ServerHandler for Shout {
//!     fn get_info(&self) -> InitializeResult {
//!         InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
//!     }
//!
//!     async fn list_tools(
//!         &self,
//!         _: Option<PaginatedRequestParams>,
//!         _: RequestContext<RoleServer>,
//!     ) -> Result<ListToolsResult, ErrorData> {
//!         let schema = json!({"type": "object", "properties": {"text": {"type": "string"}}});
//!         let schema = Arc::new(schema.as_object().unwrap().clone());
//!         let shout = Tool::new("shout", "The text, in capitals", schema);
//!         Ok(ListToolsResult::with_all_items(vec![shout]))
//!     }
//!
//!     async fn call_tool(
//!         &self,
//!         call: CallToolRequestParams,
//!         _: RequestContext<RoleServer>,
//!     ) -> Result<CallToolResponse, ErrorData> {
//!         let args = call.arguments.unwrap_or_default();
//!         let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
//!         Ok(CallToolResult::success(vec![ContentBlock::text(text.to_uppercase())]).into())
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! // The server serves one end of the pipe; the hub connects the other.
//! let (client_end, server_end) = tokio::io::duplex(64 * 1024);
//! tokio::spawn(async move {
//!     if let Ok(running) = Shout.serve(server_end).await {
//!         let _ = running.waiting().await;
//!     }
//! });
//! let hub = McpHub::new();
//! hub.connect_transport("loud", client_end, &|line| println!("{line}"))
//!     .await;
//! assert!(hub.failures().is_empty());
//!
//! let tools = hub.tools();
//! assert_eq!(tools[0].name(), "mcp__loud__shout");
//! assert_eq!(tools[0].kind(), "mcp.loud.shout");
//!
//! // The agent loop calls it like any other tool.
//! let ctx = ToolContext::new("/", Arc::new(ShutWorkspace));
//! let input = json!({"text": "hello"}).as_object().unwrap().clone();
//! let out = tools[0].execute(input, &ctx).await.unwrap();
//! assert_eq!(out.content, "HELLO");
//!
//! hub.close().await;
//! # }
//! ```
//!
//! A host's real servers: a program it starts and a remote endpoint, then the
//! project's `.mcp.json`. This starts a process and dials a URL, so it is only
//! compiled here.
//!
//! ```no_run
//! use std::{path::Path, sync::Arc};
//!
//! use haest_core::{ChildEnv, fs::Fs};
//! use haest_mcp::{McpHub, McpServerConfig, ProjectMcp, load_project_mcp_config};
//!
//! # async fn host(fs: &dyn Fs) {
//! let github = McpServerConfig::stdio("github", "github-mcp-server")
//!     .with_args(["stdio"])
//!     .with_env("GITHUB_TOKEN", "ghp_example");
//! let docs = McpServerConfig::http("docs", "https://docs.example.com/mcp")
//!     .with_header("Authorization", "Bearer t0ken");
//!
//! // The project's own servers, `${NAME}`s filled in from the host's variables.
//! // A file that cannot be used is reported, and the run goes on without it.
//! let vars = |name: &str| std::env::var(name).ok();
//! let project = match load_project_mcp_config(fs, Path::new("/repo"), &vars).await {
//!     Ok(project) => project,
//!     Err(e) => {
//!         eprintln!(".mcp.json not loaded: {e}");
//!         ProjectMcp::default()
//!     }
//! };
//! project.notes.iter().for_each(|note| eprintln!("{note}"));
//!
//! // What a stdio server's process gets of this one's environment: PATH only.
//! let path = std::env::var("PATH").unwrap_or_default();
//! let hub = Arc::new(McpHub::with_env(ChildEnv::new(vec![("PATH".into(), path)])));
//! let mut servers = vec![github, docs];
//! servers.extend(project.servers);
//! hub.connect(&servers, &|line| eprintln!("{line}")).await;
//! for failed in hub.failures() {
//!     eprintln!("MCP server {} did not start: {}", failed.name, failed.error);
//! }
//!
//! // `hub` is a ToolSource: hand it to the loop. At the end, shut the servers down.
//! hub.close().await;
//! # }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod config;
mod hub;
mod project;
mod result;
mod tool;
mod transport;

#[cfg(test)]
mod test_server;

pub use config::{McpServerConfig, McpTransport};
pub use hub::{CALL_TIMEOUT, CONNECT_TIMEOUT, McpFailure, McpHub};
pub use project::{McpConfigError, ProjectMcp, load_project_mcp_config, project_mcp_path};
pub use result::mcp_result_to_output;
pub use tool::{mcp_kind, mcp_tool_name};

/// rmcp, re-exported for a host that connects its own transport
/// ([`McpHub::connect_transport`]) and needs its types.
pub use rmcp;
