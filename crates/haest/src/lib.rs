//! An agent loop and tool harness you embed in a Rust program.
//!
//! haest runs the loop every coding agent runs: send the conversation to a
//! model, run the tools it asks for, send the results back, until the model
//! answers without tools. Around that loop it brings the parts a real agent
//! needs: providers for Anthropic, OpenAI and OpenRouter, the file, shell, search
//! and web tools, a copy-on-write sandbox, MCP servers, sub-agents, mods,
//! sessions, compaction and spending caps. Every part is a trait, so a host
//! swaps any of them for its own.
//!
//! haest reads nothing it wasn't handed. No file, environment variable or flag
//! reaches a run unless the host passes it in: keys come from a [`KeySource`],
//! the environment of a process the agent starts is a [`ChildEnv`], and the
//! files the tools touch go through the [`Workspace`] you give it.
//!
//! # Quickstart
//!
//! ```toml
//! [dependencies]
//! haest = "0.1"
//! tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
//! ```
//!
//! One turn with a real model and haest's built-in tools, in the current
//! directory:
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use haest::providers::{ProviderRegistry, RegistryOptions};
//! use haest::tools::{BuiltinOptions, builtin_tools};
//! use haest::workspace::RealWorkspace;
//! use haest::{Agent, AgentDeps, AgentOptions, ChildEnv, LoopEvent};
//!
//! #[tokio::main]
//! async fn main() {
//!     // The host decides where keys come from; here, this process's environment.
//!     let keys = |name: &str| std::env::var(name).ok();
//!     let models = ProviderRegistry::new(
//!         Arc::new(keys),
//!         Default::default(), // no providers.json entries
//!         RegistryOptions::default(),
//!     );
//!
//!     // Bash runs with only the variables you pass on.
//!     let pass = |k: &str| (k.to_string(), std::env::var(k).unwrap_or_default());
//!     let env = ChildEnv::new([pass("PATH"), pass("HOME")]);
//!
//!     let deps = AgentDeps::new(
//!         Arc::new(models),
//!         Arc::new(builtin_tools(BuiltinOptions::default())),
//!         Arc::new("You are a careful engineer.".to_string()),
//!         std::env::current_dir().unwrap(),
//!         Arc::new(RealWorkspace::new(env)),
//!     );
//!     let model = AgentOptions::new("openrouter:anthropic/claude-sonnet-5.5");
//!     let mut agent = Agent::new(deps, model);
//!
//!     // Every event of the turn goes to the sink, in order.
//!     // This one prints the answer as it streams.
//!     let mut sink = |ev: LoopEvent| match ev {
//!         LoopEvent::TextDelta { text } => print!("{text}"),
//!         LoopEvent::ToolCallStart { name, .. } => println!("\n[{name}]"),
//!         _ => {}
//!     };
//!     let end = agent.run("What does this project do?", &mut sink).await;
//!     println!("\n{:?}", end.stop_reason);
//! }
//! ```
//!
//! Run it with `OPENROUTER_API_KEY` set. `anthropic:` and `openai:` models take
//! `ANTHROPIC_API_KEY` and `OPENAI_API_KEY`; a `providers.json` adds any
//! OpenAI-compatible server (see [`providers`]).
//!
//! # How a turn runs
//!
//! ```text
//! agent.run(input, &mut sink)
//!   1. the input joins the history                          LoopEvent::User
//!   2. the model is called through the ModelResolver        LoopEvent::ModelStart, TextDelta ...
//!   3. its answer joins the history                         LoopEvent::Assistant
//!   4. each tool it called runs, inside the Workspace       LoopEvent::ToolResult
//!   5. back to 2 with the results, until it answers without tools,
//!      a cap stops it, or the host aborts                   LoopEvent::TurnEnd
//! ```
//!
//! [`Agent::run`] returns how the turn ended ([`TurnEnd`]); [`Agent::messages`]
//! is the history, ready for the next turn. Input that arrives during a turn
//! goes through [`AgentControl::steer`], and [`AgentControl::abort`] stops it
//! cleanly.
//!
//! # The plug points
//!
//! [`AgentDeps`] is everything the loop is handed. Each field is a trait with
//! haest's own implementations beside it; a host keeps those or brings its own.
//!
//! | Trait | What it is | haest's own |
//! |---|---|---|
//! | [`ModelResolver`], [`Provider`] | turns a model spec (`openrouter:z-ai/glm-5.3`) into a model API | [`providers::ProviderRegistry`] |
//! | [`ToolSource`], [`Tool`] | the tools the model may call | [`tools::builtin_tools`], [`mcp`], [`mods`], [`code`] |
//! | [`Workspace`], [`Fs`] | the files and the shell every tool goes through | [`workspace::RealWorkspace`], [`workspace::SandboxWorkspace`], [`ShutWorkspace`] |
//! | [`SystemPrompt`] | the system prompt, rebuilt for each model call | [`String`] |
//! | [`AgentHooks`] | the host's say in the loop: approvals, caps, compaction | [`NoHooks`], [`runtime`] |
//! | [`EventSink`] | where the turn's events go | closures, `Vec<LoopEvent>`, a tokio channel |
//! | [`KeySource`] | API keys and endpoints, asked for by name | closures, `HashMap<String, String>` |
//!
//! # Write a tool
//!
//! A tool is a name, a description and a JSON Schema the model sees, and an
//! async `execute`. Return [`ToolError::Input`] for a mistake the model can
//! correct; it goes back to the model as is.
//!
//! ```
//! use haest::tool::JsonMap;
//! use haest::{Tool, ToolContext, ToolError, ToolOutput, async_trait};
//! use serde_json::{Value, json};
//!
//! struct Shout;
//!
//! #[async_trait]
//! impl Tool for Shout {
//!     fn name(&self) -> &str {
//!         "Shout"
//!     }
//!     fn description(&self) -> String {
//!         "Returns the text in capitals.".into()
//!     }
//!     fn parameters(&self) -> Value {
//!         json!({
//!             "type": "object",
//!             "properties": { "text": { "type": "string" } },
//!             "required": ["text"]
//!         })
//!     }
//!     fn kind(&self) -> &str {
//!         "shout"
//!     }
//!     fn read_only(&self) -> bool {
//!         true
//!     }
//!     async fn execute(
//!         &self,
//!         input: JsonMap,
//!         _ctx: &ToolContext,
//!     ) -> Result<ToolOutput, ToolError> {
//!         let text = haest::tool::req_str(&input, "text")?;
//!         Ok(ToolOutput::text(text.to_uppercase()))
//!     }
//! }
//! ```
//!
//! Hand it to the agent with the others:
//! `let mut tools = builtin_tools(opts); tools.push(Arc::new(Shout));`.
//!
//! # Test your host
//!
//! The `testing` feature brings [`testing::ScriptedProvider`], a model that
//! answers from a script and records every request, so a test drives whole
//! turns with no network and no keys:
//!
//! ```toml
//! [dev-dependencies]
//! haest = { version = "0.1", features = ["testing"] }
//! ```
//!
//! ```
//! # use std::sync::Arc;
//! # use haest::tool::JsonMap;
//! # use haest::{Tool, ToolContext, ToolError, ToolOutput, async_trait};
//! # use serde_json::{Value, json};
//! # struct Shout;
//! # #[async_trait]
//! # impl Tool for Shout {
//! #     fn name(&self) -> &str { "Shout" }
//! #     fn description(&self) -> String { "Returns the text in capitals.".into() }
//! #     fn parameters(&self) -> Value { json!({"type": "object"}) }
//! #     fn kind(&self) -> &str { "shout" }
//! #     async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
//! #         Ok(ToolOutput::text(haest::tool::req_str(&input, "text")?.to_uppercase()))
//! #     }
//! # }
//! use haest::testing::{ScriptedProvider, text_answer, tool_answer};
//! use haest::{Agent, AgentDeps, AgentOptions, LoopEvent, ShutWorkspace, StopReason};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let model = ScriptedProvider::new([
//!     tool_answer(&[("call-1", "Shout", json!({"text": "hello"}))]),
//!     text_answer("It says HELLO."),
//! ]);
//! let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(Shout)];
//! let deps = AgentDeps::new(
//!     Arc::new(model.resolver()),
//!     Arc::new(tools),
//!     Arc::new("You shout.".to_string()),
//!     "/",
//!     Arc::new(ShutWorkspace),
//! );
//! let mut agent = Agent::new(deps, AgentOptions::new("scripted:any"));
//!
//! let mut events: Vec<LoopEvent> = Vec::new();
//! let end = agent.run("Shout hello", &mut events).await;
//!
//! assert_eq!(end.stop_reason, StopReason::EndTurn);
//! let result = events.iter().find_map(|e| match e {
//!     LoopEvent::ToolResult { output, .. } => Some(output.content.clone()),
//!     _ => None,
//! });
//! assert_eq!(result.as_deref(), Some("HELLO"));
//! // The model was offered the tool by name.
//! assert_eq!(model.requests()[0].tools[0].name, "Shout");
//! # });
//! ```
//!
//! # Features
//!
//! [`core`] (the loop and the contracts, re-exported at the root) is always
//! there; every other part is a feature, so a host links only what it uses.
//!
//! | Feature | Brings | Default |
//! |---|---|---|
//! | `providers` | [`providers`]: Anthropic Messages, OpenAI Chat Completions and Responses, OpenRouter, any OpenAI-compatible server | yes |
//! | `workspace` | [`workspace`]: the real disk and bash, or a sandbox over a copy-on-write overlay | yes |
//! | `tools` | [`tools`]: Read, Write, Edit, Glob, Grep, Bash, WebFetch, tasks, sub-agents and more | yes |
//! | `mcp` | [`mcp`]: MCP servers (stdio, streamable HTTP) and their tools | yes |
//! | `code` | [`code`]: the Code tool and the sandbox `js` command, in embedded QuickJS | yes |
//! | `mods` | [`mods`]: change how haest behaves without forking it; approvals, plan mode | yes |
//! | `settings` | [`settings`]: every setting as a plain value a host writes out in Rust | yes |
//! | `config` | [`config`]: the loader for haest's TOML files, `HAEST_*` variables and flags | no |
//! | `runtime` | [`runtime`]: a whole conversation built from settings, behind a host's UI | no |
//! | `testing` | [`testing`]: a scripted model and other fakes for tests | no |
//!
//! ```toml
//! # a host with its own provider transport, files and tools:
//! haest = { version = "0.1", default-features = false, features = [
//!     "providers", "workspace", "tools", "settings",
//! ] }
//! # the whole conversation from haest's own files, variables and flags (the CLI):
//! haest = { version = "0.1", features = ["runtime", "config"] }
//! ```
//!
//! # Going further
//!
//! - [`runtime`] builds what the `haest` CLI runs: settings in, a conversation
//!   with sessions, sub-agents, MCP, mods and compaction out. Start there for a
//!   full agent behind your own UI.
//! - [`settings`] lists every setting; the
//!   [settings reference](https://github.com/haest-werk/haest/blob/main/docs/settings.md)
//!   explains each one.
//! - The [brain example](https://github.com/haest-werk/haest/tree/main/crates/haest/examples/brain/)
//!   wires every plug point the way a real host does, against fakes;
//!   `cargo run -p haest --example brain` runs it.
//! - `cargo install haest-cli` gets the `haest` command line agent.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

#[doc(inline)]
pub use haest_core as core;
pub use haest_core::*;

/// Marks an `impl Tool` (or any other async trait impl in haest) so its
/// `async fn`s compile; the same macro haest's traits are declared with.
pub use async_trait::async_trait;

#[cfg(feature = "code")]
#[doc(inline)]
pub use haest_code as code;
#[cfg(feature = "config")]
#[doc(inline)]
pub use haest_config as config;
#[cfg(feature = "mcp")]
#[doc(inline)]
pub use haest_mcp as mcp;
#[cfg(feature = "mods")]
#[doc(inline)]
pub use haest_mods as mods;
#[cfg(feature = "providers")]
#[doc(inline)]
pub use haest_providers as providers;
#[cfg(feature = "runtime")]
#[doc(inline)]
pub use haest_runtime as runtime;
#[cfg(feature = "settings")]
#[doc(inline)]
pub use haest_settings as settings;
#[cfg(feature = "tools")]
#[doc(inline)]
pub use haest_tools as tools;
#[cfg(feature = "workspace")]
#[doc(inline)]
pub use haest_workspace as workspace;
