//! An agent loop and tool harness you embed in a Rust program.
//!
//! haest runs the loop every coding agent runs: send the conversation to a
//! model, run the tools it asks for, send the results back, until the model
//! answers without tools. Around that loop it brings the parts a real agent
//! needs: providers for Anthropic, OpenAI and OpenRouter, the file, shell, search
//! and web tools, a copy-on-write sandbox, MCP servers, subagents, mods,
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
//! haest = "0.6"
//! tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
//! ```
//!
//! One turn with a real model and haest's built-in tools, in the current
//! directory:
//!
//! ```no_run
//! # // Built with the default features; without them there is nothing to run.
//! # #[cfg(all(feature = "providers", feature = "tools", feature = "workspace"))]
//! # mod quickstart {
//! use haest::providers::ProviderRegistry;
//! use haest::tools::{BuiltinOptions, builtin_tools};
//! use haest::workspace::RealWorkspace;
//! use haest::{Agent, AgentDeps, AgentOptions, BoxError, ChildEnv, LoopEvent, SharedDeps};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), BoxError> {
//!     // The host decides where keys come from; here, this process's environment.
//!     let keys = |name: &str| std::env::var(name).ok();
//!     // The built-in providers; `.with_custom(..)` adds providers.json's.
//!     let models = ProviderRegistry::new(keys);
//!
//!     // Bash runs with only the variables you pass on.
//!     let pass = |k: &'static str| (k, std::env::var(k).unwrap_or_default());
//!     let env = ChildEnv::new([pass("PATH"), pass("HOME")]);
//!
//!     // What the conversation runs on: models, a working directory, files and shell.
//!     let shared = SharedDeps::new(models, std::env::current_dir()?, RealWorkspace::new(env));
//!     // The agent's own: its tools and its system prompt.
//!     let tools = builtin_tools(BuiltinOptions::default());
//!     let deps = AgentDeps::new(shared, tools, "You are a careful engineer.");
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
//!     Ok(())
//! }
//! # }
//! # fn main() {}
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
//! [`Agent::run`] returns how the turn ended ([`TurnOutcome`]); [`Agent::history`]
//! is the history, ready for the next turn. Input that arrives during a turn
//! goes through [`AgentControl::steer`], and [`AgentControl::abort`] stops it
//! cleanly.
//!
//! # The plug points
//!
//! [`AgentDeps`] and the [`SharedDeps`] inside it are everything the loop is
//! handed. Each part is a trait with haest's own implementations beside it; a
//! host keeps those or brings its own. [`SharedDeps::new`], [`AgentDeps::new`]
//! and every `with_*` setter take the part itself (a closure, a `String`, a host's own type), or
//! an `Arc` of one the host already shares, so one object can fill several
//! slots. Each slot holds it in a cheap handle named after its trait
//! ([`ToolSourceRef`], [`AgentHooksRef`], ...).
//!
//! | Trait | What it is | haest's own |
//! |---|---|---|
//! | [`ModelResolver`], [`Provider`] | turns a model spec (`openrouter:z-ai/glm-5.3`) into a model API | [`providers::ProviderRegistry`] |
//! | [`http::HttpClient`] | how every model call and web request travels | [`http::ReqwestClient`] |
//! | [`ToolSource`], [`Tool`] | the tools the model may call | [`Tools`], [`tools::builtin_tools`], [`mcp`], [`mods`], [`code`] |
//! | [`Workspace`], [`Fs`] | the files and the shell every tool goes through | [`workspace::RealWorkspace`], [`workspace::SandboxWorkspace`], [`ShutWorkspace`] |
//! | [`SystemPrompt`] | the system prompt, rebuilt for each model call | [`String`], `&'static str`, closures |
//! | [`AgentHooks`] | the host's say in the loop: each message to save, compaction before a model call, media | [`NoHooks`], [`runtime`] |
//! | [`ToolServices`] | what tools hand the host while they run: inner-call rows, debug lines | [`NoServices`], [`runtime`] |
//! | [`ModelFacts`] | what the host knows of each model: the inputs it takes, its efforts | [`NoFacts`], [`runtime`] |
//! | [`TurnCapsSource`] | the caps on each turn, per model | [`TurnCaps`] (the same for every model), [`runtime`] |
//! | [`Clock`] | the time a turn's time cap reads | [`WallClock`] |
//! | [`EventSink`] | where the turn's events go | closures, `Vec<LoopEvent>`, a tokio channel |
//! | [`KeySource`] | API keys and endpoints, asked for by name | closures, `HashMap<String, String>` |
//!
//! Approvals are a mod's ([`mods`]), not the loop's.
//!
//! # Write a tool
//!
//! A tool is a name, a description and a JSON Schema the model sees, and an
//! async `execute`. Return [`ToolError::Input`] for a mistake the model can
//! correct; it goes back to the model as is.
//!
//! ```
//! use haest::prelude::*;
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
//!         let text = req_str(&input, "text")?;
//!         Ok(ToolOutput::text(text.to_uppercase()))
//!     }
//! }
//! ```
//!
//! Hand it to the agent with the others:
//! `let tools = builtin_tools(opts).with(Shout);`.
//!
//! # Test your host
//!
//! The `testing` feature brings [`core::testing`]: a model that answers from
//! a script and records every request, so a test drives whole turns with no
//! network and no keys. Its docs run a turn with a tool call. With
//! `providers` it adds `providers::testing` (a fake HTTP client, SSE bodies),
//! with `http` an HTTP server on 127.0.0.1 (`http::testing::Server`). Every
//! `testing` module is for `dev-dependencies` only and outside semver: it
//! changes in any release.
//!
//! ```toml
//! [dev-dependencies]
//! haest = { version = "0.6", features = ["testing"] }
//! ```
//!
//! # Features
//!
//! [`core`] (the loop and the contracts) is always there: its key types at
//! the root (`haest::Agent`, `haest::Tool`) and in [`prelude`], its modules
//! under [`core`] (`haest::core::fs`, `haest::core::session`). Every other
//! part is a feature, so a host links only what it uses, and each one has a
//! module of its name at the root: `haest::workspace` is always the workspace
//! crate.
//!
//! | Feature | Brings | Default |
//! |---|---|---|
//! | `http` | [`http`]: the HTTP seam the model calls and GenerateImage send through (WebFetch and MCP servers keep clients of their own); on with `providers` and `tools` | yes |
//! | `reqwest` | `http::ReqwestClient`, the default client over reqwest and rustls; off with `providers` alone, the model calls send only through the client a host hands in (`tools` and `mcp` turn it on: WebFetch and rmcp run on reqwest) | yes |
//! | `providers` | [`providers`]: Anthropic Messages, OpenAI Chat Completions and Responses, OpenRouter, any OpenAI-compatible server | yes |
//! | `workspace` | [`workspace`]: the real disk and bash, or a sandbox over a copy-on-write overlay | yes |
//! | `tools` | [`tools`]: Read, Write, Edit, Glob, Grep, Bash, WebFetch, tasks, subagents and more | yes |
//! | `mcp` | [`mcp`]: MCP servers (stdio, streamable HTTP) and their tools | yes |
//! | `rmcp` | `mcp::rmcp` and `mcp::McpHub::connect_transport`: an MCP server over a transport the host opened (rmcp's types in haest's API); turns on `mcp` | no |
//! | `code` | [`code`]: the Code tool and the sandbox `js` command, in embedded QuickJS | yes |
//! | `mods` | [`mods`]: change how haest behaves without forking it, in Rust: native mods, hooks per event; approvals, plan mode | yes |
//! | `bridge-js` | [`bridges`]: `.js` / `.ts` mod files in embedded QuickJS, reloaded as they change; turns on `mods` | yes |
//! | `bridge-shell` | `bridges::ShellHook`: a hook that is a command run on the host per event, JSON on its stdin and stdout, in haest's dialect or Claude Code's (`bridges::Claude`, so its hook scripts run as they are); turns on `mods` | no |
//! | `bridge-http` | `bridges::HttpHook`: a hook behind a URL, a POST per event through [`http`], in the same two dialects; turns on `mods` and `http` | no |
//! | `settings` | [`settings`]: every setting as a plain value a host writes out in Rust | yes |
//! | `config` | [`config`]: the loader for haest's TOML files, `HAEST_*` variables and flags; with `mcp` too, it reads a trusted project's `.mcp.json`; turns on `mods` (the `hooks` setting is a `mods::HookSettings`) | no |
//! | `runtime` | [`runtime`]: a whole conversation built from settings, behind a host's UI; turns on the three bridges (the `hooks` setting's commands and URLs) | no |
//! | `wire` | [`wire`]: the envelope around a conversation's events, the requests a peer sends, a replay ring with gaps, an NDJSON codec | no |
//! | `session` | [`session`]: a conversation another program watches and drives while it runs (input mid-turn, catch-up, approvals over it); with `runtime`, the whole conversation is one | no |
//! | `transport` | [`transport`]: the session served to another program, one connection's rules over any line source and sink | no |
//! | `transport-stdio` | `transport::Stdio`: a parent process drives the conversation over this one's stdin and stdout | no |
//! | `transport-broker` | `transport::Broker`: the conversation dials out to a broker over a WebSocket (pulls a WebSocket client on rustls) | no |
//! | `transport-unix` | `transport::UnixSocket`: programs on this machine watch (or drive) the conversation over a Unix socket; `transport::running_sessions` and `transport::connect_session` find and reach one | no |
//! | `prompt` | [`prompt`]: the system prompt as the host's Jinja template, set up like Hugging Face chat templates; with `runtime`, `runtime::RuntimeInputs::with_prompt_template` renders it before every model call | no |
//! | `testing` | [`core::testing`]: a scripted model and other fakes for tests; with `providers` `providers::testing`, with `http` `http::testing::Server`. `dev-dependencies` only, outside semver | no |
//!
//! ```toml
//! # a host with its own HTTP client, files and tools:
//! haest = { version = "0.6", default-features = false, features = [
//!     "providers", "workspace", "tools", "settings",
//! ] }
//! # the whole conversation from haest's own files, variables and flags (the CLI):
//! haest = { version = "0.6", features = ["runtime", "config"] }
//! ```
//!
//! # Going further
//!
//! - [`runtime`] builds what the `haest` CLI runs: settings in, a conversation
//!   with sessions, subagents, MCP, mods and compaction out. Start there for a
//!   full agent behind your own UI.
//! - [`settings`] lists every setting; the
//!   [settings reference](https://github.com/haest-werk/haest/blob/main/docs/settings.md)
//!   explains each one.
//! - The [notebook example](https://github.com/haest-werk/haest/tree/main/crates/haest/examples/notebook/)
//!   wires every plug point the way a real host does, against fakes;
//!   `cargo run -p haest --example notebook` runs it.
//! - [`core::threshold`]: a meter crossing a mark fires an action (remind
//!   the model, warn the human, wind down, stop). A host adds its own
//!   beside haest's; `cargo run -p haest --example threshold` runs two.
//! - `cargo install haest-cli` gets the `haest` command line agent.

#![cfg_attr(docsrs, feature(doc_cfg))]

#[doc(inline)]
pub use haest_core::prelude::*;

/// Marks an `impl Tool` (or any other async trait impl in haest) so its
/// `async fn`s compile; the same macro haest's traits are declared with.
pub use async_trait::async_trait;

/// The turn's cancel token, as [`AgentHooks::before_model_call`],
/// [`ToolContext::cancel`] and [`ProviderRequest::cancel`] hand it out:
/// tokio-util's, re-exported so a host names the same version.
pub use tokio_util::sync::CancellationToken;

/// The JSON a tool's schema and input are made of ([`Tool::parameters`],
/// [`JsonMap`]): the serde_json haest is built with.
pub use serde_json;

#[doc(inline)]
pub use haest_core as core;

/// The key types, [`async_trait`](macro@async_trait),
/// [`CancellationToken`] and serde_json's `Value` and `json!`, for a glob
/// import: `use haest::prelude::*;` is all an `impl Tool`, `impl Provider`
/// or `impl AgentHooks` needs.
///
/// Glob-import this rather than the facade's root: the root also holds
/// [`core`], which a glob import would let shadow the standard library's
/// `core`.
///
/// ```
/// use haest::prelude::*;
///
/// // `core` is still the standard library's.
/// let none: core::option::Option<Message> = None;
/// assert!(none.is_none());
/// ```
pub mod prelude {
    #[doc(no_inline)]
    pub use crate::{CancellationToken, async_trait};
    #[doc(no_inline)]
    pub use haest_core::prelude::*;
    #[doc(no_inline)]
    pub use serde_json::{Value, json};
}

#[cfg(any(
    feature = "bridge-js",
    feature = "bridge-shell",
    feature = "bridge-http"
))]
#[doc(inline)]
pub use haest_bridges as bridges;
#[cfg(feature = "code")]
#[doc(inline)]
pub use haest_code as code;
#[cfg(feature = "config")]
#[doc(inline)]
pub use haest_config as config;
#[cfg(feature = "http")]
#[doc(inline)]
pub use haest_http as http;
#[cfg(feature = "mcp")]
#[doc(inline)]
pub use haest_mcp as mcp;
#[cfg(feature = "mods")]
#[doc(inline)]
pub use haest_mods as mods;
#[cfg(feature = "prompt")]
#[doc(inline)]
pub use haest_prompt as prompt;
#[cfg(feature = "providers")]
#[doc(inline)]
pub use haest_providers as providers;
#[cfg(feature = "runtime")]
#[doc(inline)]
pub use haest_runtime as runtime;
#[cfg(feature = "session")]
#[doc(inline)]
pub use haest_session as session;
#[cfg(feature = "settings")]
#[doc(inline)]
pub use haest_settings as settings;
#[cfg(feature = "tools")]
#[doc(inline)]
pub use haest_tools as tools;
#[cfg(feature = "transport")]
#[doc(inline)]
pub use haest_transport as transport;
#[cfg(feature = "wire")]
#[doc(inline)]
pub use haest_wire as wire;
#[cfg(feature = "workspace")]
#[doc(inline)]
pub use haest_workspace as workspace;

/// What the loader read, as the runtime's inputs on the real disk: the
/// settings, the child environment and the variables passed on purpose, the
/// provider keys (the loader's environment), providers.json, the trusted
/// `.mcp.json` servers, the OpenRouter routing and the `hooks` setting's
/// commands and URLs, sent over the default reqwest client.
///
/// Show [`Loaded::notes`](config::Loaded::notes) to the human first: they are
/// not part of the inputs. A host in another world sets its own
/// [`with_world`](runtime::RuntimeInputs::with_world) on the result; one with
/// its own HTTP client, [`with_http`](runtime::RuntimeInputs::with_http).
#[cfg(all(feature = "config", feature = "runtime"))]
#[cfg_attr(docsrs, doc(cfg(all(feature = "config", feature = "runtime"))))]
pub fn runtime_inputs(loaded: config::Loaded) -> runtime::RuntimeInputs {
    let world = runtime::World::disk(loaded.child_env);
    let inputs = runtime::RuntimeInputs::new(loaded.config.settings, world)
        .with_passed_env(loaded.passed_env)
        .with_keys(loaded.keys)
        .with_providers(loaded.providers)
        .with_mcp_servers(loaded.mcp_servers)
        .with_hook_settings(loaded.hooks);
    match loaded.routing {
        Some(routing) => inputs.with_routing(routing),
        None => inputs,
    }
}
