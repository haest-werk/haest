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
//! never through the workspace, behind the net guard: [`assert_public_url`]
//! lets a URL through only when it is http(s) and every address its host
//! resolves to is public (no localhost, LAN or cloud metadata endpoint, see
//! [`is_blocked`]), on the first request and on every redirect hop. The
//! default client resolves names through the same [`Resolve`] when it
//! connects, so a name that answers public, then private, cannot slip
//! between the check and the connect.
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
//! | Agent, SendMessage, StopAgent | start, message and stop subagents | no | [`SubagentHub::tools_for`], [`agent_tools`] | an [`AgentDirectory`]; the default [`SubagentHub`] needs a [`SubagentParent`] |
//! | ToolSearch (and CallTool) | loads a deferred tool's schema (calls one, in proxy mode) | ToolSearch only | [`DeferredTools`] | a [`ToolDeferral`](haest_core::deferred_tools::ToolDeferral) |
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
//! # Subagents
//!
//! A subagent is a second [`Agent`] loop in this process with its own
//! history, its own turn caps ([`SubagentCaps`]) and the conversation's
//! shared ledger, so the whole tree stays under the conversation's budget.
//! Its spend comes back as the Agent call's `child_cost_usd`: the parent's
//! turn (and its USD cap) counts it, and the ledger is not charged twice.
//!
//! The conversation's own loop is [`MAIN_AGENT`]; every subagent has a
//! unique name. A message lands at the receiver's next step boundary
//! ([`AgentControl::steer`]); one for main goes to the host's [`Inbox`]. The
//! person writes to a running subagent through the hub
//! ([`SubagentHub::deliver_as`]).
//! Main is depth 0, an agent at the depth cap gets no Agent tool, and at
//! most `max_concurrent` run at once across the whole tree. Wind-down
//! (opt-in, [`haest_core::wind_down`]): once the caller's turn is past its
//! share of a cap, the Agent tool starts nothing and says why
//! ([`AgentError::WindingDown`]), whatever directory it speaks to.
//!
//! The agent tools speak to an [`AgentDirectory`], never to the hub, so a
//! host with its own idea of a subagent implements that trait and keeps the
//! tools. [`SubagentHub`] is the default, over a [`SubagentParent`]; a parent
//! whose members run on runs of its own (its own models, caps, records, and
//! an end it must close) keeps the hub and implements
//! [`SubagentParent::start_member`], which hands back a [`MemberRun`].
//! [`AgentToolOptions`] gives the Agent tool the host's kinds of subagent
//! (`subagent_type`) and its own `model` text: to [`agent_tools_with`]
//! directly, or to the hub through [`SubagentParent::agent_tool_options`],
//! whose [`subagent_tools`](SubagentParent::subagent_tools) then says what
//! each kind may use.
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
//! [`Agent`]: haest_core::Agent
//! [`AgentControl::steer`]: haest_core::AgentControl::steer
//! [`SubagentCaps`]: haest_core::caps::SubagentCaps

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod agents;
mod bash;
mod bash_jobs;
mod builtins;
mod canonical;
mod files;
mod fs_walk;
mod guide;
mod image;
mod inbox;
mod jobs;
mod net_guard;
mod read;
mod search;
mod skill;
mod tasks;
#[cfg(test)]
mod testkit;
mod tool_search;
mod turn_gate;
mod web_fetch;

/// The HTTP seam WebFetch and GenerateImage send through: implement
/// [`http::HttpClient`] to give either tool a client of your own.
#[doc(inline)]
pub use haest_http as http;

pub use agents::{
    AgentDirectory, AgentError, AgentRef, AgentToolOptions, Ending, InProcessRun, MAIN_AGENT,
    MemberCaps, MemberEnd, MemberRun, MemberStart, Sender, SpawnInput, SpawnOutcome, Spend,
    SubagentBudget, SubagentEvent, SubagentHub, SubagentParent, SubagentTypes, agent_tools,
    agent_tools_with, subagent_note,
};
pub use bash::{BashOptions, BashTool};
pub use bash_jobs::job_tools;
pub use builtins::{
    BuiltinOptions, MissingOption, builtin_tools, builtin_tools_where, builtin_word,
};
pub use files::{EditTool, WriteTool};
pub use guide::{FILE_TOOLS, tool_guide, touches_workspace};
pub use image::{
    DEFAULT_IMAGE_MODEL, DEFAULT_IMAGE_MODELS, GenerateImageOptions, GenerateImageTool,
    ImageCatalog, ImageError, ImageReply, ModelSettings, SettingSpec, call_image_api,
    sniff_image_type,
};
pub use inbox::{Inbox, NoInbox};
pub use jobs::{
    Job, JobError, JobEvents, JobHub, JobStatus, MAX_FINISHED, MAX_JOBS, NoJobEvents, Taken,
};
pub use net_guard::{Resolve, SystemResolve, UrlRefused, assert_public_url, is_blocked};
pub use read::{READ_TOKEN_BUDGET, ReadIntercept, ReadTool, media_output, read_text_output};
pub use search::{GlobTool, GrepTool};
pub use skill::{SkillToolOptions, skill_tool};
pub use tasks::task_tools;
pub use tool_search::DeferredTools;
pub use turn_gate::{AgentTurnGate, AgentTurnSite};
pub use web_fetch::{WebFetchNet, WebFetchTool, markdown_of};
