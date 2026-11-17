//! haest-core: the agent loop and the contracts every plug-in implements.
//!
//! An [`Agent`] holds one conversation with a model. [`Agent::run`] runs one
//! human turn: it calls the model, runs the tools the model asked for, feeds
//! the results back, and repeats until the model answers without tools. Every
//! event of the turn goes to an [`EventSink`] as it happens.
//!
//! Everything the loop touches comes in through a trait, so the same loop runs
//! behind a CLI, a web app or a test, against any model API, on a real disk or
//! on none. haest-core reads no file, environment variable or flag of its own
//! accord: the host hands it everything.
//!
//! # haest-core or `haest`
//!
//! Most hosts depend on the [`haest`](https://docs.rs/haest) facade: it puts
//! this crate's key types at its root and the crate itself under
//! `haest::core`, and adds model providers, workspaces, tools and an MCP
//! client behind features. Depend on haest-core directly when
//! you write a plug-in crate (a provider, a set of tools, a workspace) and
//! want the contracts and nothing else, or when your host brings every plug
//! point itself.
//!
//! # Plug points
//!
//! | Plug point | Trait | Implemented by |
//! |---|---|---|
//! | a model API | [`provider::Provider`], [`provider::ModelResolver`] | `haest-providers`, or the host |
//! | a tool | [`tool::Tool`], [`tool::ToolSource`] | `haest-tools`, MCP, or the host |
//! | files | [`fs::Fs`] | `haest-workspace`, [`fs::MemFs`], or the host |
//! | files + shell | [`workspace::Workspace`] | `haest-workspace`, [`workspace::ShutWorkspace`] |
//! | the host's say in the loop | [`deps::AgentHooks`] | the host |
//! | what the host knows of each model | [`catalog::ModelFacts`] | the host, [`catalog::NoFacts`] |
//! | the caps on each turn | [`caps::TurnCapsSource`] | the host, [`caps::TurnCaps`] |
//! | the time a turn's cap reads | [`deps::Clock`] | the host, [`deps::WallClock`] |
//! | the host's way past a cap | [`remedy::Remedies`] | the host |
//! | where events go | [`events::EventSink`] | the host / CLI |
//! | where durable runs are kept | [`run::RunStore`] | [`run::MemoryRunStore`], or the host |
//!
//! # Where to start
//!
//! - [`Agent`], [`AgentDeps`], [`AgentOptions`]: build a conversation and run turns;
//!   [`AgentControl`] steers or aborts a running turn from another task.
//! - [`LoopEvent`]: what a turn emits, in order.
//! - [`Tool`] to add a tool, [`AgentHooks`] to change how the loop behaves.
//! - [`messages`]: the history as data. Session files and events keep the
//!   TypeScript haest's JSON shape: either haest resumes the other's sessions.
//! - Spend: [`caps`] caps one turn, [`ledger`] one conversation, and the
//!   loop checks both before every model call. [`remedy`] adds the host's way
//!   past a cap to haest's message about it. [`threshold`]: a meter crossing
//!   a mark fires an action (remind the model, warn the human, wind down,
//!   compact, stop); the guard's nudge, the ledger's warning and compaction
//!   are thresholds, and a host adds its own. [`wind_down`] (opt-in) stops
//!   new work at a set share of a cap.
//! - Context: [`context`] builds the system prompt, [`compaction`] summarises
//!   a full window, [`deferred_tools`] keeps large tool lists out of the request.
//! - Durable runs (opt-in): [`run`] lets a turn stopped part way go on where
//!   it stopped, and returns a finished run's result under the same key.
//!
//! # What the loop guarantees
//!
//! - Every tool call in history gets exactly one result, even when the turn is
//!   aborted or stopped by a cap, so the history can always be sent again. (A
//!   provider keeps its side of this by never leaving a tool call in an
//!   answer that failed.)
//! - A provider error, an unknown model or a tool that fails or panics is a
//!   value (an error result, or a turn that ends with
//!   [`StopReason::Error`]), never a panic of the loop.
//! - A cap stops a turn only before a model call, never between a tool call
//!   and its result. The last event of a turn is always [`LoopEvent::TurnEnd`].
//!
//! # Example
//!
//! One turn with one tool. The provider is a fake that answers from a script
//! (`testing::ScriptedProvider`, behind the `testing` feature); a host plugs
//! in a real one from `haest-providers` instead.
//!
//! ```
//! # #[cfg(feature = "testing")]
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_core::BoxError> {
//! use async_trait::async_trait;
//! use haest_core::{
//!     Agent, AgentDeps, AgentOptions, JsonMap, LoopEvent, Message, ShutWorkspace, StopReason,
//!     Tool, ToolContext, ToolError, ToolOutput, Tools, req_str,
//!     testing::{ScriptedProvider, text_answer, tool_answer},
//! };
//! use serde_json::{Value, json};
//!
//! /// Upper-cases text.
//! struct Shout;
//!
//! #[async_trait]
//! impl Tool for Shout {
//!     fn name(&self) -> &str {
//!         "Shout"
//!     }
//!     fn description(&self) -> String {
//!         "Upper-case some text.".into()
//!     }
//!     fn parameters(&self) -> Value {
//!         json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
//!     }
//!     fn kind(&self) -> &str {
//!         "text.shout"
//!     }
//!     async fn execute(&self, input: JsonMap, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
//!         Ok(ToolOutput::text(req_str(&input, "text")?.to_uppercase()))
//!     }
//! }
//!
//! // The model will call Shout, then answer with text.
//! let provider = ScriptedProvider::new([
//!     tool_answer(&[("call-1", "Shout", json!({"text": "hello"}))]),
//!     text_answer("It says HELLO."),
//! ]);
//! let deps = AgentDeps::new(
//!     provider.resolver(),     // which provider serves a `provider:model` spec
//!     Tools::new().with(Shout), // what the model may call
//!     "Use your tools.",       // the system prompt
//!     "/work",                 // the working directory
//!     ShutWorkspace,           // no files, no shell
//! );
//! let mut agent = Agent::new(deps, AgentOptions::new("scripted:model-1"));
//!
//! let mut sink = |ev: LoopEvent| match ev {
//!     LoopEvent::TextDelta { text } => print!("{text}"),
//!     LoopEvent::ToolResult { call, message, .. } => println!("[{} -> {}]", call.name, message.content),
//!     _ => {}
//! };
//! let end = agent.run("shout hello", &mut sink).await;
//!
//! assert_eq!(end.stop_reason, StopReason::EndTurn);
//! // The input, the call, its result, the answer.
//! assert_eq!(agent.history().len(), 4);
//! assert!(matches!(&agent.history()[2], Message::Tool(r) if r.content == "HELLO"));
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "testing"))]
//! # fn main() {}
//! ```
//!
//! # Features
//!
//! - `testing`: the `testing` module, fakes for tests in this crate and in
//!   crates that build on it (a scripted provider, canned answers). Turn it on
//!   in `dev-dependencies` only.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]
#![warn(missing_debug_implementations)]

pub mod agent;
pub mod caps;
pub mod catalog;
pub mod compaction;
pub mod context;
pub mod deferred_tools;
pub mod deps;
mod effort;
pub mod error;
pub mod events;
mod expand;
pub mod frontmatter;
pub mod fs;
mod guard;
pub mod handed;
pub mod ledger;
pub mod media;
pub mod messages;
mod panics;
mod plug_ref;
mod pricing;
pub mod provider;
pub mod remedy;
mod request_prefix;
pub mod run;
pub mod session;
pub mod shell_command;
pub mod skills;
mod slug;
mod stable_hash;
pub mod task_board;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod threshold;
pub mod tool;
mod tool_batch;
mod tool_exec;
pub mod wind_down;
pub mod words;
pub mod workspace;

/// What the other haest crates share, and nothing a host needs: not part of
/// the API, and not covered by semver. It changes in any release.
#[doc(hidden)]
pub mod __private {
    /// For [`word_enum!`](crate::word_enum)'s serde impls in any crate,
    /// whatever serde it depends on.
    pub use serde;

    /// OpenRouter's API: haest's OpenRouter provider and GenerateImage both
    /// call it.
    pub const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";

    /// Where this host's OpenRouter is: its `OPENROUTER_BASE_URL` key, else
    /// [`OPENROUTER_URL`].
    pub fn openrouter_base_url(keys: &dyn crate::KeySource) -> String {
        keys.key("OPENROUTER_BASE_URL")
            .unwrap_or_else(|| OPENROUTER_URL.into())
    }

    pub use crate::deferred_tools::split::{advertised_tools, callable_tools};
    pub use crate::expand::{expand_vars, is_var_name, unset_note};
    pub use crate::panics::panic_message;
    pub use crate::tool_exec::{ToolIndex, run_call, run_unspilled};
}

#[doc(inline)]
pub use prelude::*;

/// The key types, the ones this crate's root re-exports: glob-import them
/// with `use haest_core::prelude::*;`. It holds types, traits and functions
/// only, never a module, so a glob import shadows nothing.
///
/// Every trait here comes with the haest types its methods name, so
/// implementing one ([`Fs`], [`Provider`], [`ModelFacts`], [`AgentHooks`],
/// [`Workspace`]) needs no other haest import: a provider builds its stream
/// with [`answer_stream`], and a tool gets its argument helpers ([`req_str`],
/// [`opt_str`], [`opt_num`], [`opt_bool`], [`truncate_middle`]). What it
/// names of other crates (serde_json's `Value`, tokio-util's
/// `CancellationToken`) comes from those crates; the `haest` facade's
/// prelude has both.
pub mod prelude {
    pub use crate::agent::{Agent, AgentControl, AgentOptions};
    pub use crate::caps::{TurnCaps, TurnCapsSource, TurnCapsSourceRef};
    pub use crate::catalog::{InputModality, ModelFacts, ModelFactsRef, NoFacts, Reasoning};
    pub use crate::deps::{
        AgentDeps, AgentHooks, AgentHooksRef, Clock, ClockRef, NoHooks, SharedDeps, SystemPrompt,
        SystemPromptRef, WallClock, now_ms,
    };
    pub use crate::error::BoxError;
    pub use crate::events::{EventSink, LoopEvent, TurnOutcome};
    pub use crate::fs::{DirEntry, FileKind, Fs, FsRef, FsStat, MemFs};
    pub use crate::handed::{ChildEnv, KeySource, KeySourceRef};
    pub use crate::messages::{
        AssistantMessage, AssistantPart, MediaPart, Message, StopReason, ToolCall,
        ToolResultMessage, Usage, UserMessage,
    };
    pub use crate::provider::{
        Effort, ModelError, ModelResolver, ModelResolverRef, Provider, ProviderEvent, ProviderRef,
        ProviderRequest, ProviderStream, ResolvedModel, answer_stream,
    };
    pub use crate::tool::{
        InnerCallEvent, JsonMap, NoServices, Tool, ToolContext, ToolError, ToolOutput, ToolRef,
        ToolServices, ToolServicesRef, ToolSource, ToolSourceRef, Tools, opt_bool, opt_num,
        opt_str, req_str, truncate_middle,
    };
    pub use crate::workspace::{
        ChangeSet, ChangeSetRef, ExecOptions, ExecResult, OutputChunks, ShutWorkspace, Workspace,
        WorkspaceMode, WorkspaceRef,
    };
}
