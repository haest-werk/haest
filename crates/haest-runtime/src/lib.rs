//! haest assembled into one conversation: providers, workspace, tools, MCP
//! servers, mods, subagents, the inbox and the session file, behind a host's
//! [`Ui`].
//!
//! A host hands the runtime everything it may use ([`RuntimeInputs`]): the
//! [`Settings`](haest_settings::Settings), the [`World`] (the real disk with
//! the environment a child process gets, or the host's own
//! [`Fs`](haest_core::fs::Fs)), the variables passed on purpose, the provider
//! keys, the custom providers, the MCP servers, OpenRouter's routing and the
//! HTTP client the model calls go through. The runtime reads no file, variable or
//! flag of its own: what is not handed in does not exist for it. haest-config
//! is the opt-in loader that fills these in from haest's files, `HAEST_*`
//! variables and flags, the way the `haest` command does.
//!
//! `haest::runtime` is this crate, behind the facade's `runtime` feature, and
//! the `haest` command is this crate and haest-config behind a terminal.
//! Depend on haest-runtime directly to take the conversation without the
//! facade.
//!
//! # The way in
//!
//! [`build`] is the whole conversation behind one [`Ui`] and the host's
//! approval [`Prompter`](haest_mods::Prompter), returned as a [`Runtime`].
//! How it is assembled is the runtime's own: a host reads the parts on the
//! [`Runtime`] and changes them through its methods, but does not put them
//! together. A host that owns some parts itself (its own models, files or
//! tools) builds its agent from haest's crates instead, an
//! `AgentDeps` over its own parts, and picks the built-in tools the settings
//! name with `haest_tools::builtin_tools_where(|w| settings.tools.has(w), ..)`:
//! it needs no runtime at all.
//!
//! Every switch in the settings is honoured here, once: a capability they
//! leave off is never built. Trust is the loader's: it lists a project's MCP
//! servers and turns its mods on only in a trusted dir, and the runtime starts
//! what it is handed.
//!
//! # A conversation's life
//!
//! 1. [`build`] it. Whatever can fail does so before anything starts: a build
//!    that fails has started no MCP server or mod and left no session file.
//! 2. Run the human's turn with [`Runtime::human_turn`] (the mods may rewrite
//!    the text, and what the inbox's gate held rides in), then every turn the
//!    inbox holds: [`Runtime::next_agent_turn`] until it is None, each run
//!    with [`Runtime::run`]. While a turn runs, what reaches the inbox is
//!    steered into it; every turn ends with the session file flushed.
//! 3. Between turns, make the changes the human asks for:
//!    [`Runtime::set_model_spec`], [`Runtime::set_effort`], [`Runtime::clear`],
//!    [`Runtime::rescan_skills`].
//! 4. At the end, [`Runtime::shut_down`]: the subagents stopped, the
//!    background jobs killed, the MCP servers closed, the session flushed.
//!
//! With feature `session`, a host hands the runtime to haest-session's
//! `start` instead, and the session's driver runs this same life with these
//! same methods (`Runtime` is its `Driven`): peers watch the stream and send
//! input, and [`HubUi`] puts the runtime's lines in that stream.
//!
//! # Examples
//!
//! A whole conversation over a host's own files, behind a host's UI. It
//! reads its instructions from those files and writes its session there, and
//! has no key it was not handed. Nothing here calls a model.
//!
//! ```
//! use std::{path::Path, sync::Arc};
//!
//! use async_trait::async_trait;
//! use haest_core::fs::{Fs, MemFs};
//! use haest_mods::{ApprovalAnswer, ApprovalRequest, Prompter};
//! use haest_runtime::{BuildOptions, RuntimeInputs, Ui, World, build};
//! use haest_settings::{BuiltinTools, ContextSources, Settings};
//!
//! /// The host's UI: here, lines on stderr.
//! struct Lines;
//!
//! impl Ui for Lines {
//!     fn info(&self, text: &str) {
//!         eprintln!("{text}");
//!     }
//!     fn warn(&self, text: &str) {
//!         eprintln!("warning: {text}");
//!     }
//!     fn error(&self, text: &str) {
//!         eprintln!("error: {text}");
//!     }
//! }
//!
//! /// The host's approvals: there is no human to ask here, so every ask is denied.
//! struct DenyAll;
//!
//! #[async_trait]
//! impl Prompter for DenyAll {
//!     async fn prompt(&self, _: &ApprovalRequest) -> ApprovalAnswer {
//!         ApprovalAnswer::Deny
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_runtime::BuildError> {
//! let fs = Arc::new(MemFs::with_files([("/work/AGENTS.md", "Answer in one line.")]));
//! let settings = Settings {
//!     context: ContextSources { agents: true, ..ContextSources::NONE },
//!     tools: BuiltinTools { read: true, grep: true, ..BuiltinTools::NONE },
//!     data_dir: "/data".into(),
//!     ..Settings::bare("/work", "/home/me")
//! };
//! let mut inputs = RuntimeInputs::new(settings, World::Fs(fs.clone()));
//! // The default model is OpenRouter's: hand in its key, and no other.
//! inputs.keys = Arc::new(|name: &str| {
//!     (name == "OPENROUTER_API_KEY").then(|| "sk-or-example".to_string())
//! });
//!
//! let rt = build(inputs, Arc::new(Lines), DenyAll, BuildOptions::default()).await?;
//!
//! let deps = rt.agent().deps();
//! let tools: Vec<String> = deps.tools().tools().iter().map(|t| t.name().to_string()).collect();
//! assert_eq!(tools, ["Read", "Grep"]);
//! assert!(deps.system_prompt().system_prompt().await.contains("Answer in one line."));
//!
//! rt.hooks.flush().await;
//! let session = Path::new("/data/sessions").join(format!("{}.jsonl", rt.session_id()));
//! assert!(fs.exists(&session).await);
//! # Ok(())
//! # }
//! ```
//!
//! Running turns, the way a host with an input line does. A turn calls the
//! model, so this is only compiled here.
//!
//! ```no_run
//! use haest_core::LoopEvent;
//! use haest_runtime::{Runtime, Ui};
//!
//! /// A turn the human started, then every turn waiting in the inbox.
//! async fn human<U: Ui>(rt: &mut Runtime<U>, text: &str) {
//!     let mut show = |event: LoopEvent| println!("{event:?}");
//!     rt.human_turn(text, &mut show).await;
//!     // None: nothing waits, or the gate holds it until the human writes again.
//!     while let Some(text) = rt.next_agent_turn() {
//!         rt.run(&text, &mut show).await;
//!     }
//! }
//!
//! /// The host is done: nothing haest started outlives it, and the session is on disk.
//! async fn quit<U: Ui>(rt: &Runtime<U>) {
//!     let stopped = rt.shut_down().await;
//!     println!("stopped {} subagents and {} jobs", stopped.subagents, stopped.jobs);
//! }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod build;
mod error;
mod hooks;
mod inbox;
mod inputs;
mod mods;
mod parent;
mod prompt;
mod runtime;
#[cfg(feature = "session")]
mod session;
mod sessions;
mod store;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;
mod tools;
mod ui;
mod world;

/// The names the runtime's code uses, laid out as the `haest` facade lays
/// them out: core's key types at the root, its modules under `core`.
mod haest {
    pub use haest_code as code;
    pub use haest_core as core;
    pub use haest_core::prelude::*;
    pub use haest_mcp as mcp;
    pub use haest_mods as mods;
    pub use haest_providers as providers;
    pub use haest_providers::http;
    pub use haest_tools as tools;
    pub use haest_workspace as workspace;
}

pub use build::build;
pub use error::{BuildError, SessionError};
pub use hooks::RuntimeHooks;
pub use inbox::InboxQueue;
pub use inputs::{BuildOptions, Resume, RuntimeInputs};
pub use runtime::{Runtime, ShutDown};
#[cfg(feature = "session")]
#[cfg_attr(docsrs, doc(cfg(feature = "session")))]
pub use session::{HubUi, subagent_frame};
pub use sessions::{SessionInfo, list_sessions};
pub use ui::Ui;
pub use world::{WorkspaceError, World, kill_jobs};
