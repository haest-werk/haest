//! haest's MCP client: it connects [MCP](https://modelcontextprotocol.io)
//! servers and offers their tools to the agent as haest
//! [`Tool`](haest_core::tool::Tool)s.
//!
//! A server is reached one of three ways: a child process speaking MCP on its
//! stdin and stdout ([`McpTransport::Stdio`]), streamable HTTP
//! ([`McpTransport::Http`]), or, with the `rmcp` feature, any transport rmcp
//! takes that the host opened itself (`McpHub::connect_transport`: a server
//! in the same process, a socket). [`McpHub`] holds the connections and is a
//! [`ToolSource`](haest_core::tool::ToolSource): the agent loop asks it for the
//! tools before every model call, so a server connected later is offered from
//! the next call on.
//!
//! Built on the official Rust SDK, [rmcp](https://docs.rs/rmcp).
//!
//! # Features
//!
//! - `rmcp` (off): rmcp in this crate's API, for a host that connects a
//!   transport of its own: `McpHub::connect_transport` and the `rmcp`
//!   re-export (the version haest-mcp is built on). Without it, rmcp is an
//!   implementation detail, and an rmcp release that breaks its API is not
//!   a breaking release of haest-mcp.
//!
//! # Where to start
//!
//! - [`McpHub`]: [`connect`](McpHub::connect) the servers, read its
//!   [`failures`](McpHub::failures), hand it to the loop,
//!   [`close`](McpHub::close) it at the end.
//! - [`McpServerConfig`]: one server to connect, written out (a name over
//!   [`McpTransport::stdio`] or [`McpTransport::http`]) or read from a
//!   project's `.mcp.json` ([`load_project_mcp_config`]).
//! - [`mcp_tool_name`]: the name the model sees for a server's tool, for a
//!   host that names MCP tools in its own lists (deferral, `code_tools`).
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
//! A result the server marks `isError` is an error output the model reads;
//! text and images reach it, anything else as a line; a JSON-RPC error, or a server that asks for
//! input or answers with a task, is a failed call, and so is any call after
//! [`McpHub::close`].
//!
//! # Failures and time limits
//!
//! A server that cannot be started, or does not connect or list its tools
//! within [`CONNECT_TIMEOUT`], is logged and kept in [`McpHub::failures`]. It
//! is never returned as an error: one broken server never takes the harness
//! down. A tool call runs for at most [`CALL_TIMEOUT`]
//! ([`McpHub::with_call_timeout`] sets another); a call that times out
//! or is cancelled is cancelled on the server too. An HTTP server's URL shows
//! in a failure, a log line or a failed call without its query, fragment or
//! userinfo, since a key may ride in one (`?api_key=...`).
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
//! A server running in the same process, over an in-memory pipe (feature
//! `rmcp`): its tool is offered under its haest name, and a call reaches it.
//! The server side is written with rmcp's `server` feature, which haest-mcp
//! itself does not turn on.
//!
//! ```
//! use haest_core::{
//!     tool::{JsonMap, Tool as _, ToolContext},
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
//!         let schema = schema.as_object().cloned().unwrap_or_default();
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
//! # #[cfg(feature = "rmcp")]
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_core::error::BoxError> {
//! // The server serves one end of the pipe; the hub connects the other.
//! let (client_end, server_end) = tokio::io::duplex(64 * 1024);
//! tokio::spawn(async move {
//!     if let Ok(running) = Shout.serve(server_end).await {
//!         let _ = running.waiting().await;
//!     }
//! });
//! let hub = McpHub::new().with_log(|line| println!("{line}"));
//! hub.connect_transport("loud", client_end).await;
//! assert!(hub.failures().is_empty());
//!
//! let tools = hub.tools();
//! assert_eq!(tools[0].name(), "mcp__loud__shout");
//! assert_eq!(tools[0].kind(), "mcp.loud.shout");
//!
//! // The agent loop calls it like any other tool.
//! let ctx = ToolContext::new("/", ShutWorkspace);
//! let input: JsonMap = serde_json::from_value(json!({"text": "hello"}))?;
//! let out = tools[0].execute(input, &ctx).await?;
//! assert_eq!(out.content, "HELLO");
//!
//! hub.close().await;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "rmcp"))]
//! # fn main() {}
//! ```
//!
//! A host's real servers: a program it starts and a remote endpoint, then the
//! project's `.mcp.json`. This starts a process and dials a URL, so it is only
//! compiled here.
//!
//! ```no_run
//! use std::path::Path;
//!
//! use haest_core::{ChildEnv, error::report, fs::Fs};
//! use haest_mcp::{McpHub, McpServerConfig, McpTransport, ProjectMcp, load_project_mcp_config};
//!
//! # async fn host(fs: &dyn Fs) {
//! let github = McpTransport::stdio("github-mcp-server")
//!     .with_args(["stdio"])
//!     .with_env_var("GITHUB_TOKEN", "ghp_example");
//! let github = McpServerConfig::new("github", github);
//! let docs = McpTransport::http("https://docs.example.com/mcp")
//!     .with_header("Authorization", "Bearer t0ken");
//! let docs = McpServerConfig::new("docs", docs);
//!
//! // The project's own servers, `${NAME}`s filled in from the host's variables.
//! // A file that cannot be used is reported, and the run goes on without it.
//! let vars = |name: &str| std::env::var(name).ok();
//! let project = match load_project_mcp_config(fs, Path::new("/repo"), vars).await {
//!     Ok(project) => project,
//!     Err(e) => {
//!         eprintln!(".mcp.json not loaded: {}", report(&e));
//!         ProjectMcp::default()
//!     }
//! };
//! project.notes.iter().for_each(|note| eprintln!("{note}"));
//!
//! // What a stdio server's process gets of this one's environment: PATH only.
//! let path = std::env::var("PATH").unwrap_or_default();
//! let hub = McpHub::new()
//!     .with_env(ChildEnv::new([("PATH", path)]))
//!     .with_log(|line| eprintln!("{line}"));
//! let mut servers = vec![github, docs];
//! servers.extend(project.servers);
//! hub.connect(&servers).await;
//! for failed in hub.failures() {
//!     eprintln!("MCP server {} did not start: {}", failed.name(), report(failed.error()));
//! }
//!
//! // `hub` is a ToolSource: hand the loop a clone. At the end, shut the servers down.
//! hub.close().await;
//! # }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

mod config;
mod hub;
mod project;
mod result;
mod tool;
mod transport;

#[cfg(test)]
mod test_server;

pub use config::{HttpTransport, McpServerConfig, McpTransport, StdioTransport};
pub use hub::{CALL_TIMEOUT, CONNECT_TIMEOUT, McpFailure, McpHub, McpStartError};
pub use project::{McpConfigError, ProjectMcp, load_project_mcp_config, project_mcp_path};
pub use tool::mcp_tool_name;

/// rmcp, re-exported for a host that connects its own transport
/// ([`McpHub::connect_transport`]) and needs its types (feature `rmcp`).
///
/// With the feature on, its types are part of haest-mcp's API: a breaking
/// rmcp release (4.0 after 3.x) breaks a host that uses them.
#[cfg(feature = "rmcp")]
pub use rmcp;
