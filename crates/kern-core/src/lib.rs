//! kern-core: the agent loop and the contracts every plug-in implements.
//!
//! | Plug point | Trait | Implemented by |
//! |---|---|---|
//! | a model API | [`provider::Provider`], [`provider::ModelResolver`] | `kern-providers`, or the host |
//! | a tool | [`tool::Tool`], [`tool::ToolSource`] | `kern-tools`, MCP, or the host |
//! | files | [`fs::Fs`] | `kern-workspace`, [`fs::MemFs`], or the host (brain's BrainFs) |
//! | files + shell | [`workspace::Workspace`] | `kern-workspace`, [`workspace::ShutWorkspace`] |
//! | the host's say in the loop | [`deps::AgentHooks`] | the host |
//! | where events go | [`events::EventSink`] | the host / CLI |

pub mod agent;
pub mod catalog;
pub mod compaction;
pub mod context;
pub mod deferred_tools;
pub mod deps;
pub mod effort;
pub mod events;
pub mod expand;
pub mod fs;
pub mod guard;
pub mod handed;
pub mod ledger;
pub mod limits;
pub mod media;
pub mod messages;
pub mod panics;
pub mod pricing;
pub mod provider;
pub mod request_prefix;
pub mod session;
pub mod shell_command;
pub mod skills;
pub mod task_board;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tool;
pub mod tool_batch;
pub mod tool_exec;
pub mod words;
pub mod workspace;

/// For [`word_enum!`]'s serde impls in any crate, whatever serde it depends on.
#[doc(hidden)]
pub use serde as __serde;

pub use agent::{Agent, AgentControl, AgentOptions};
pub use deps::{AgentDeps, AgentHooks, NoHooks, SystemPrompt, now_ms};
pub use events::{EventSink, LoopEvent, TurnEnd};
pub use fs::{Fs, MemFs};
pub use handed::{ChildEnv, KeySource};
pub use messages::{
    AssistantMessage, AssistantPart, Message, StopReason, ToolCall, ToolResultMessage, Usage,
    UserMessage,
};
pub use provider::{
    Effort, ModelResolver, Provider, ProviderEvent, ProviderRequest, ResolvedModel,
};
pub use tool::{Tool, ToolContext, ToolError, ToolHost, ToolOutput, ToolSource};
pub use workspace::{ExecOptions, ExecResult, ShutWorkspace, Workspace, WorkspaceMode};
