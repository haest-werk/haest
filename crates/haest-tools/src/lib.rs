//! haest's built-in tools: the [`Tool`]s an agent reads, writes, searches,
//! runs commands, fetches pages, keeps a task list and starts subagents with.
//!
//! Each tool is a plain [`Tool`] from haest-core: a name, a JSON Schema for its
//! input, and an `execute` that gets a [`ToolContext`]. Names and input keys
//! are Claude Code's (`Read`, `file_path`, `old_string`, ...) on purpose: every
//! frontier model has seen millions of those calls.
//!
//! Most hosts take this crate as `haest::tools` (the `tools` feature of the
//! `haest` crate, on by default), and haest-runtime builds the list from the
//! settings. Depend on it directly when you run haest-core's agent loop with
//! your own composition: some of these tools beside your own, over your own
//! workspace.
//!
//! # Where the tools work
//!
//! Every tool here touches files and commands ONLY through `ctx.workspace`
//! ([`Workspace::fs`], [`Workspace::shell`], [`Workspace::run`]), so the
//! sandbox's in-memory files, a host's own store and the real disk all look
//! the same to them. haest-workspace has the workspaces that ship with haest;
//! any [`Workspace`] works. WebFetch is the one exception: it runs in the HOST,
//! never through the workspace, behind the net guard ([`net_guard`]).
//!
//! # The built-in tools
//!
//! [`builtin_tools`] makes these, in this order; [`builtin_tools_where`] picks
//! some by their word (the `tools` setting). Read-only tools may run in
//! parallel within one model step.
//!
//! | Tool | What it does | Read-only | Word | Needs from the host |
//! |---|---|---|---|---|
//! | Read | a file's lines, numbered and paged; an image, PDF, audio or video file as media | yes | `read` | the workspace's files; optionally a [`ReadIntercept`] |
//! | Write | creates or overwrites a file, making its directories | no | `write` | the workspace's files |
//! | Edit | replaces an exact, unique string in a file | no | `edit` | the workspace's files |
//! | Bash | runs a command line in the workspace's shell | no | `bash` | the workspace's shell; a [`JobHub`] for `run_in_background` |
//! | BashOutput | a background job's output since the last look | no | `bash` | the [`JobHub`] (only with one) |
//! | KillShell | stops a background job and everything it started | no | `bash` | the [`JobHub`] (only with one) |
//! | Grep | ripgrep over the files | yes | `grep` | an `rg` the workspace can run |
//! | Glob | files by glob pattern, newest first | yes | `glob` | the workspace's files |
//! | WebFetch | one public web page as markdown, or as media | yes | `web_fetch` | a [`WebFetchNet`] (the default resolves with the system and fetches with its own guarded client) |
//! | TaskCreate, TaskUpdate | add or change a task | no | `tasks` | a [`TaskBoard`] |
//! | TaskList, TaskGet | the whole list, or one task | yes | `tasks` | the same [`TaskBoard`] |
//! | Skill | a skill's full instructions | yes | `skill` | [`SkillToolOptions`] |
//!
//! Tools a host builds on their own, with no word:
//!
//! | Tool | What it does | Read-only | Built by | Needs from the host |
//! |---|---|---|---|---|
//! | Agent, SendMessage, StopAgent | start, message and stop subagents | no | [`SubagentHub::tools_for`], [`agents::agent_tools`] | an [`AgentDirectory`]; the default [`SubagentHub`] needs a [`SubagentParent`] |
//! | ToolSearch (and CallTool) | loads a deferred tool's schema (calls one, in proxy mode) | ToolSearch only | [`tool_search::DeferredTools`] | a [`ToolDeferral`](haest_core::deferred_tools::ToolDeferral) |
//! | GenerateImage | makes an image with an image model, saves and shows it | no | [`GenerateImageTool`] | an Image API key ([`GenerateImageOptions`]) |
//!
//! WebFetch and GenerateImage make their requests through [`http::HttpClient`]
//! (haest-http, re-exported as [`http`]), so a host or a test can hand either
//! one its own client.
//!
//! A turn the human did not start (a background job that exited, a subagent's
//! message for main) reaches the model through the host's [`Inbox`], counted
//! by an [`AgentTurnGate`] so two agents cannot start turns forever.
//!
//! # Example
//!
//! The built-ins over a sandboxed shell on files in memory: what Write puts
//! there, `cat` and Read see.
//!
//! ```
//! use std::sync::Arc;
//!
//! use haest_core::{fs::MemFs, tool::ToolContext};
//! use haest_tools::{BuiltinOptions, builtin_tools};
//! use haest_workspace::SandboxWorkspace;
//! use serde_json::json;
//!
//! # #[tokio::main]
//! # async fn main() {
//! let ws = SandboxWorkspace::over(Arc::new(MemFs::new()));
//! let ctx = ToolContext::new("/project", Arc::new(ws));
//! // No WebFetch: this example stays offline.
//! let tools = builtin_tools(BuiltinOptions { web_fetch: None, ..Default::default() });
//! let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
//! assert_eq!(names, ["Read", "Write", "Edit", "Bash", "Grep", "Glob"]);
//!
//! let call = |name: &str, input: serde_json::Value| {
//!     let tool = tools.iter().find(|t| t.name() == name).unwrap().clone();
//!     let ctx = ctx.clone();
//!     async move { tool.execute(input.as_object().unwrap().clone(), &ctx).await }
//! };
//! call("Write", json!({"file_path": "notes.md", "content": "hello\n"})).await.unwrap();
//! let out = call("Bash", json!({"command": "cat /project/notes.md"})).await.unwrap();
//! assert_eq!(out.content, "hello\n");
//! let out = call("Read", json!({"file_path": "notes.md"})).await.unwrap();
//! assert_eq!(out.content, "1\thello");
//! # }
//! ```
//!
//! Ported from the TS haest's `tools/*.ts`.
//!
//! [`Tool`]: haest_core::tool::Tool
//! [`ToolContext`]: haest_core::tool::ToolContext
//! [`Workspace`]: haest_core::workspace::Workspace
//! [`Workspace::fs`]: haest_core::workspace::Workspace::fs
//! [`Workspace::shell`]: haest_core::workspace::Workspace::shell
//! [`Workspace::run`]: haest_core::workspace::Workspace::run
//! [`TaskBoard`]: haest_core::task_board::TaskBoard

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

pub mod agents;
pub mod bash;
pub mod bash_jobs;
mod builtins;
mod canonical;
pub mod files;
mod fs_walk;
pub mod image;
pub mod inbox;
pub mod jobs;
pub mod net_guard;
pub mod read;
pub mod search;
pub mod skill;
pub mod tasks;
#[cfg(test)]
mod testkit;
pub mod tool_search;
pub mod turn_gate;
pub mod web_fetch;

/// The HTTP seam WebFetch and GenerateImage send through: implement
/// [`http::HttpClient`] to give either tool a client of your own.
#[doc(inline)]
pub use haest_http as http;

pub use agents::{AgentDirectory, SubagentHub, SubagentParent};
pub use bash::{BashOptions, BashTool};
pub use bash_jobs::job_tools;
pub use builtins::{
    BuiltinOptions, MissingOption, builtin_tools, builtin_tools_where, builtin_word,
};
pub use files::{EditTool, WriteTool};
pub use image::{GenerateImageOptions, GenerateImageTool};
pub use inbox::Inbox;
pub use jobs::{JobError, JobEvents, JobHub};
pub use read::{ReadIntercept, ReadTool};
pub use search::{GlobTool, GrepTool};
pub use skill::{SkillToolOptions, skill_tool};
pub use tasks::task_tools;
pub use turn_gate::AgentTurnGate;
pub use web_fetch::{WebFetchNet, WebFetchTool};
