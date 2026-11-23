//! Bridges: mods and hooks written outside Rust, speaking haest's mod
//! protocol.
//!
//! A bridge is a [`Hook<E>`](haest_mods::Hook) or a [`Mod`](haest_mods::Mod)
//! that does three things for each event: encodes the event's payload, carries
//! it to code that is not the host's Rust (a script, a process, a server),
//! and decodes that code's answer as the event's decision. The payload and
//! decision types are haest-mods' ([`Event`](haest_mods::Event)), and their
//! JSON is the words every bridge speaks (`{ deny }`, `{ input }`, `{ stop }`,
//! `{ steer }`, `{ continue }`, `{ compact }`), so a handler means the same
//! whichever bridge carries it. What a decision then does is haest-mods' too:
//! a bridge never takes one its own way.
//!
//! Each bridge is a feature of this crate, carrying the dependencies only it
//! needs; none is on by default.
//!
//! | Feature | Bridge |
//! |---|---|
//! | `js` | `.js` / `.ts` mod files in embedded QuickJS: one file is a `JsMod`, a conversation's mod directories are one `ModFiles` that reloads them as they change |
//! | `shell` | a command per event, `ShellHook`: the event as JSON on its stdin, the decision on its stdout, exit 2 a blocking answer; run on the host by haest-workspace's real exec |
//! | `http` | a POST per event, `HttpHook`: the event as the JSON body, the decision as the response's, through haest-http's `HttpClient` |
//!
//! `ShellHook` and `HttpHook` are [`Hook`](haest_mods::Hook)s, bound per
//! event in [`Hooks`](haest_mods::Hooks) with a
//! [`Binding`](haest_mods::Binding)'s timeout, `on_error` and tool matcher
//! like a Rust closure. Each speaks a dialect: `Haest` (the default, every
//! event: the payload's own JSON with the event's name, the decision's own
//! JSON back) or `Claude` (Claude Code's hook JSON, so its hook scripts run
//! as they are, on the events Claude Code has). Their pages hold the tables:
//! what each answer and exit code does, and what fails closed.
//!
//! A config file binds the same hooks with no Rust: haest-mods'
//! [`HookSettings`](haest_mods::HookSettings) is the `hooks` setting
//! (`[[hooks."tool.before"]]` entries, each a `command` or a `url`), and
//! with `shell` or `http` on, `HookHost::bind` makes it the one
//! [`Hooks`](haest_mods::Hooks) a conversation adds as a mod. haest-config
//! reads it, haest-runtime binds it.
//!
//! A bridge proves it takes every decision as a Rust
//! [`Hooks`](haest_mods::Hooks) closure does with haest-mods' `testing`
//! feature: `decision_cases` lists one answer per decision and what it must
//! change, and `effect` drives an event through any [`Mods`](haest_mods::Mods).
//! A test of an HTTP hook posts to haest-http's `testing::Server` (its
//! `testing` feature). Like every `testing` module and feature in haest,
//! both kits are for `dev-dependencies` only and outside semver.
//!
//! # When to use this crate directly
//!
//! The `haest` facade re-exports this crate as `haest::bridges` (features
//! `bridge-js`, on by default, `bridge-shell` and `bridge-http`, off), and
//! `haest-runtime` loads a conversation's mod files from its settings for
//! you. Take this crate yourself when you assemble the mods by hand
//! ([`haest_mods::load_mods`]).
//!
//! # A mod file
//!
//! A `.js` or `.ts` file whose default export receives the `haest` API (all
//! of it is on `JsMod`). `ModFiles` finds the files in the mod directories;
//! a host can also load one itself with `JsMod::load`. This one adds a slash
//! command and a tool:
//!
//! ```
//! # use haest_mods::{ApprovalRequest, Decision, ModUi};
//! # struct Terminal;
//! # #[async_trait::async_trait]
//! # impl ModUi for Terminal {
//! #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
//! #     fn info(&self, text: &str) { println!("{text}") }
//! # }
//! const STANDUP_TS: &str = r#"
//! export default function (haest: any) {
//!   haest.command('standup', (args: string) => `Write my standup from: ${args}`)
//!   haest.tool({
//!     name: 'WordCount',
//!     description: 'Count the words in a text',
//!     parameters: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] },
//!     readOnly: true,
//!     execute: (input: { text: string }) => String(input.text.split(/\s+/).filter(Boolean).length),
//!   })
//! }
//! "#;
//!
//! # #[cfg(feature = "js")]
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_core::error::BoxError> {
//! use std::path::Path;
//!
//! use haest_bridges::JsMod;
//! use haest_core::{ShutWorkspace, ToolContext, Tools, tool::input, workspace::WorkspaceMode};
//! use haest_mods::{CommandOutcome, ModEnv, ModTools, Mods};
//! use serde_json::json;
//!
//! let env = ModEnv::new("/project", WorkspaceMode::Sandbox, Terminal);
//! let standup = JsMod::load(Path::new("/mods/standup.ts"), STANDUP_TS.into(), &env).await?;
//! let mods = Mods::new(env, [standup]);
//!
//! // The human types `/standup yesterday's commits`; the model gets the text.
//! let sent = mods.command("/standup", "yesterday's commits").await;
//! assert_eq!(sent, CommandOutcome::Prompt("Write my standup from: yesterday's commits".into()));
//!
//! // The mod's tool sits beside the host's, inside the same hooks.
//! let tools = ModTools::wrap(&mods, Tools::new());
//! let ctx = ToolContext::new("/project", ShutWorkspace);
//! let count = input(json!({ "text": "one two three" }))?;
//! assert_eq!(tools[0].execute(count, &ctx).await?.content, "3");
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "js"))]
//! # fn main() {}
//! ```
//!
//! # A mod file in the loop
//!
//! The loop events run where the host put
//! [`ModHooks`](haest_mods::ModHooks) around its hooks. This mod hides the
//! host's `Push` tool from the model, and holds every turn until the tests
//! ran: `turn.end` keeps it going with a message.
//!
//! ```
//! # use haest_mods::{ApprovalRequest, Decision, ModUi};
//! # struct Terminal;
//! # #[async_trait::async_trait]
//! # impl ModUi for Terminal {
//! #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
//! #     fn info(&self, _: &str) {}
//! # }
//! # struct Named(&'static str);
//! # #[async_trait::async_trait]
//! # impl haest_core::Tool for Named {
//! #     fn name(&self) -> &str { self.0 }
//! #     fn description(&self) -> String { self.0.into() }
//! #     fn parameters(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
//! #     fn kind(&self) -> &str { "dev.run" }
//! #     async fn execute(&self, _: haest_core::tool::JsonMap, _: &haest_core::ToolContext)
//! #         -> Result<haest_core::ToolOutput, haest_core::ToolError> {
//! #         Ok(haest_core::ToolOutput::text("ok"))
//! #     }
//! # }
//! const TESTED_FIRST_TS: &str = r#"
//! export default function (haest: any) {
//!   haest.disableTool('Push')
//!   let tested = false
//!   haest.on('tool.after', (call: { name: string }) => { if (call.name === 'Test') tested = true })
//!   haest.on('turn.end', (turn: { stopReason: string }) =>
//!     tested ? undefined : { continue: 'Run the tests before you stop.' })
//! }
//! "#;
//!
//! # #[cfg(feature = "js")]
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_core::error::BoxError> {
//! use std::path::Path;
//!
//! use haest_bridges::JsMod;
//! use haest_core::{
//!     Agent, AgentDeps, AgentOptions, LoopEvent, Message, NoHooks, SharedDeps, ShutWorkspace,
//!     Tools,
//!     testing::{ScriptedProvider, text_answer, tool_answer},
//!     workspace::WorkspaceMode,
//! };
//! use haest_mods::{ModEnv, ModHooks, ModTools, Mods};
//! use serde_json::json;
//!
//! let env = ModEnv::new("/project", WorkspaceMode::Sandbox, Terminal);
//! let rule = JsMod::load(Path::new("/mods/tested-first.ts"), TESTED_FIRST_TS.into(), &env).await?;
//! let mods = Mods::new(env, [rule]);
//!
//! let model = ScriptedProvider::new([
//!     text_answer("Done, I think."),
//!     tool_answer(&[("t1", "Test", json!({}))]),
//!     text_answer("The tests pass."),
//! ]);
//! let tools = Tools::new().with(Named("Test")).with(Named("Push"));
//! let shared = SharedDeps::new(model.resolver(), "/project", ShutWorkspace);
//! let deps = AgentDeps::new(shared, ModTools::new(tools, mods.clone()), "")
//!     .with_hooks(ModHooks::new(NoHooks, mods));
//! let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
//!
//! let end = agent.run("fix the bug", &mut Vec::<LoopEvent>::new()).await;
//! assert_eq!(end.error, None);
//!
//! // The model never saw Push, and was sent back once to run the tests.
//! let requests = model.requests();
//! assert!(requests.iter().all(|r| r.tools.iter().all(|t| t.name != "Push")));
//! assert_eq!(requests.len(), 3);
//! assert!(matches!(&agent.history()[2], Message::User(u) if u.content.contains("Run the tests")));
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "js"))]
//! # fn main() {}
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(any(feature = "shell", feature = "http"))]
mod config;
#[cfg(any(feature = "shell", feature = "http"))]
mod dialect;
#[cfg(all(test, any(feature = "shell", feature = "http")))]
mod hook_tests;
#[cfg(feature = "http")]
mod http;
#[cfg(feature = "js")]
mod js;
#[cfg(feature = "shell")]
mod shell;
#[cfg(all(test, feature = "js"))]
mod testkit;

#[cfg(any(feature = "shell", feature = "http"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "shell", feature = "http"))))]
pub use config::HookHost;
#[cfg(any(feature = "shell", feature = "http"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "shell", feature = "http"))))]
pub use dialect::{Claude, Haest};
#[cfg(feature = "http")]
#[cfg_attr(docsrs, doc(cfg(feature = "http")))]
pub use http::HttpHook;
#[cfg(feature = "shell")]
#[cfg_attr(docsrs, doc(cfg(feature = "shell")))]
pub use shell::ShellHook;

/// The caps a mod file runs in, haest-code's own: a mod file runs on the
/// engine the Code tool runs on, held to the same caps type
/// ([`VmCaps::mod_file`] by default).
#[cfg(feature = "js")]
#[cfg_attr(docsrs, doc(cfg(feature = "js")))]
#[doc(no_inline)]
pub use haest_code::VmCaps;
/// The `mod_reload` setting's word, haest-settings' own: whether
/// [`ModFiles`] reloads the files as they change.
#[cfg(feature = "js")]
#[cfg_attr(docsrs, doc(cfg(feature = "js")))]
#[doc(no_inline)]
pub use haest_settings::ModReload;
#[cfg(feature = "js")]
#[cfg_attr(docsrs, doc(cfg(feature = "js")))]
pub use js::{JsMod, ModFileOptions, ModFiles, ModLoadError};
