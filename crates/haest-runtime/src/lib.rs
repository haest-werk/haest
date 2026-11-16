//! haest assembled into one conversation: providers, workspace, tools, MCP
//! servers, mods, subagents, the inbox and the session file, behind a host's
//! [`Ui`].
//!
//! A host hands the runtime everything it may use ([`RuntimeInputs`]): the
//! [`Settings`](haest_settings::Settings), the [`World`] (the real disk with
//! the environment a child process gets, or the host's own
//! [`Fs`](haest_core::fs::Fs)), the provider keys, the custom providers, the
//! MCP servers and OpenRouter's routing. The runtime reads no file, variable or
//! flag of its own: what is not handed in does not exist for it. haest-config
//! is the opt-in loader that fills these in from haest's files, `HAEST_*`
//! variables and flags, the way the `haest` command does.
//!
//! `haest::runtime` is this crate, behind the facade's `runtime` feature, and
//! the `haest` command is this crate and haest-config behind a terminal.
//! Depend on haest-runtime directly to take the conversation without the
//! facade.
//!
//! # Two ways in
//!
//! - [`build`]: the whole conversation behind one [`Ui`] and the host's
//!   approval [`Prompter`](haest_mods::Prompter), returned as a [`Runtime`].
//! - The pieces, one by one, for a host that owns some parts itself (its own
//!   models, files or tools): [`tools::own_tools`],
//!   [`prompt::instruction_options`], [`prompt::skill_options`],
//!   [`world::workspace_for`], [`world::connect_mcp`], [`mods::load`]. A host
//!   that wants only the built-in tools picks them with
//!   `haest_tools::builtin_tools_where(|w| settings.tools.has(w), ..)` and
//!   needs no runtime at all.
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
//! 2. Run turns with [`Runtime::run`]: the human's text first through the
//!    mods' `rewrite_prompt` and [`InboxQueue::human`](inbox::InboxQueue::human).
//!    While a turn runs, `run` steers what reaches the inbox into it; after
//!    it, run what waits there, one turn at a time ([`InboxQueue::take`](inbox::InboxQueue::take)
//!    and [`InboxQueue::agent_turn`](inbox::InboxQueue::agent_turn)).
//! 3. Between turns, make the changes the human asks for:
//!    [`Runtime::set_model_spec`], [`Runtime::set_effort`], [`Runtime::clear`],
//!    [`Runtime::rescan_skills`].
//! 4. Flush the session file after every turn ([`RuntimeHooks::flush`]). At the end,
//!    stop the subagents, kill the background jobs ([`world::kill_jobs`]),
//!    close the MCP servers and flush once more.
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
//! let tools: Vec<String> = deps.tools.tools().iter().map(|t| t.name().to_string()).collect();
//! assert_eq!(tools, ["Read", "Grep"]);
//! assert!(deps.system_prompt.system_prompt().await.contains("Answer in one line."));
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
//! use haest_runtime::{Runtime, Ui, world::kill_jobs};
//!
//! /// A turn the human started, then every turn waiting in the inbox.
//! async fn human<U: Ui>(rt: &mut Runtime<U>, text: &str) {
//!     let text = rt.mods.rewrite_prompt(text).await;
//!     let text = rt.inbox.human(&text);
//!     turn(rt, &text).await;
//!     while let Some(waiting) = rt.inbox.take(&rt.agent().control()) {
//!         // None: the gate holds it until the human writes again.
//!         if let Some(text) = rt.inbox.agent_turn(&waiting) {
//!             turn(rt, &text).await;
//!         }
//!     }
//! }
//!
//! /// One turn: its events shown, what reaches the inbox meanwhile steered in.
//! async fn turn<U: Ui>(rt: &mut Runtime<U>, text: &str) {
//!     let mut show = |event: LoopEvent| println!("{event:?}");
//!     let end = rt.run(text, &mut show).await;
//!     println!("turn ended: {:?}", end.stop_reason);
//!     rt.hooks.flush().await;
//! }
//!
//! /// The host is done: nothing haest started outlives it, and the session is on disk.
//! async fn shut_down<U: Ui>(rt: &Runtime<U>) {
//!     rt.subagents.stop_all();
//!     if let Some(jobs) = &rt.jobs {
//!         kill_jobs(jobs).await;
//!     }
//!     rt.mcp.close().await;
//!     rt.hooks.flush().await;
//! }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod build;
mod error;
mod hooks;
pub mod inbox;
mod inputs;
pub mod mods;
mod parent;
pub mod prompt;
mod runtime;
pub mod sessions;
mod store;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;
pub mod tools;
mod ui;
pub mod world;

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
pub use hooks::{RuntimeHooks, session_usd};
pub use inputs::{BuildOptions, Resume, RuntimeInputs};
pub use parent::RuntimeParent;
pub use runtime::Runtime;
pub use ui::Ui;
pub use world::World;
