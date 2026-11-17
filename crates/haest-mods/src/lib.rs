//! Mods: code that changes how a haest agent behaves, without a change to haest itself.
//!
//! A mod is plain Rust (a [`Mod`]) or a `.js` / `.ts` file run in embedded
//! QuickJS (a [`JsMod`]). Both kinds hook the same four events of the agent
//! loop --
//!
//! | Event | When | May |
//! |---|---|---|
//! | `tool.before` | before every tool call | refuse it (the model reads the reason) or replace its input; the first deny wins |
//! | `tool.after` | after the call ran | replace its output |
//! | `prompt.submit` | a human message, before the model sees it | rewrite it |
//! | `system.prompt` | before every model call | append text to the system prompt |
//!
//! -- add tools and slash commands, and reach the human through the host:
//! `ask` puts an Allow / Deny prompt in front of them, `notice` (a native
//! mod's [`ModEnv::info`]) writes a line they see. Two mods are built in: [`ApprovalMod`] (in the real workspace, a
//! Bash command that destroys work waits for the human's Allow) and
//! [`PlanMod`] (`/plan`: read-only until `/plan off`).
//!
//! A mod runs with the HOST's full trust, sandbox or not: a native mod is the
//! host's own code, and a mod file can do whatever its `haest` API reaches.
//! Load a project's mod files only from a directory the human trusts
//! ([`ModOptions::with_project_mods`]).
//!
//! # When to use this crate directly
//!
//! The `haest` facade re-exports this crate as `haest::mods` (feature `mods`,
//! on by default), and `haest-runtime` loads mods from its settings and puts
//! them around the loop for you. Take this crate yourself when you build the
//! agent's [`AgentDeps`](haest_core::AgentDeps) by hand, or to write a native mod.
//!
//! | Piece | What |
//! |---|---|
//! | [`Mod`] | a native mod: implement only the events it hooks |
//! | [`JsMod`] | a `.js` / `.ts` mod file, and the `haest` API its default export gets; [`ModCaps`] bounds it, [`ModLoadError`] says why one did not load |
//! | [`load_mods`] | the built-ins plus one conversation's mod files, as [`Mods`] |
//! | [`Mods`], [`ModsBuilder`] | every loaded mod, in order, as a cheap handle that never changes once built; a host's own native mods go in with [`ModOptions::with_mod`] or [`ModsBuilder::with`] |
//! | [`ModTools`], [`ModSystemPrompt`] | the wrappers a host puts around its tool source and its system prompt |
//! | [`Mods::rewrite_prompt`], [`Mods::command`] | `prompt.submit` and slash commands, for the host's input path |
//! | [`ModUi`] | PLUG POINT: how the host asks the human and shows them a line |
//! | [`ApprovalGate`], [`Prompter`] | the generic half of a host's ask: scoped ALWAYS answers, a timeout, interrupts; [`ApprovalGate::mod_ui`] makes it the host's whole [`ModUi`] |
//! | [`ModRef`], [`ModUiRef`], [`AutoJudgeRef`], [`PrompterRef`] | the handles the plug points are held by; a parameter that takes `impl Into<..Ref>` takes the value itself, or an `Arc` of it |
//!
//! # Putting mods in the loop
//!
//! Load the mods, wrap the agent's tool source in [`ModTools`] and its system
//! prompt in [`ModSystemPrompt`], and pass the human's text through
//! [`Mods::rewrite_prompt`] before it goes to the agent. The model here is
//! scripted, so the example runs offline.
//!
//! ```
//! use haest_core::{
//!     Agent, AgentDeps, AgentOptions, MemFs, Message, SharedDeps, ShutWorkspace, Tools,
//!     testing::{ScriptedProvider, text_answer, tool_answer},
//!     tool::Tool,
//!     workspace::WorkspaceMode,
//! };
//! use haest_mods::{ModOptions, ModSystemPrompt, ModTools, load_mods};
//! use serde_json::json;
//! # use haest_mods::{ApprovalRequest, Decision, ModUi};
//! # struct Terminal;
//! # #[async_trait::async_trait]
//! # impl ModUi for Terminal {
//! #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
//! #     fn info(&self, text: &str) { println!("{text}") }
//! # }
//! # struct Bash;
//! # #[async_trait::async_trait]
//! # impl Tool for Bash {
//! #     fn name(&self) -> &str { "Bash" }
//! #     fn description(&self) -> String { "Run a command".into() }
//! #     fn parameters(&self) -> serde_json::Value { json!({"type": "object"}) }
//! #     fn kind(&self) -> &str { "shell.exec" }
//! #     async fn execute(&self, i: haest_core::tool::JsonMap, _: &haest_core::ToolContext)
//! #         -> Result<haest_core::ToolOutput, haest_core::ToolError> {
//! #         Ok(haest_core::ToolOutput::text(format!("ran {}", i["command"])))
//! #     }
//! # }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! // The human's own mod file, in the host's config directory.
//! let files = MemFs::from_files([(
//!     "/home/me/.config/haest/mods/guard.js",
//!     r#"export default function (haest) {
//!          haest.on('tool.before', call =>
//!            call.name === 'Bash' && String(call.input.command).includes('rm -rf /')
//!              ? { deny: 'not on this machine' } : undefined)
//!          haest.on('system.prompt', () => 'Ask before deleting anything.')
//!        }"#,
//! )]);
//! let config = ModOptions::new("/project", WorkspaceMode::Real, "/home/me/.config/haest")
//!     .with_user_mods(true);
//! let mods = load_mods(config, &files, Terminal).await;
//! assert_eq!(mods.names(), ["approval", "plan", "/home/me/.config/haest/mods/guard.js"]);
//!
//! // The agent's tools and system prompt, inside the mods.
//! let tools = Tools::new().with(Bash);
//! let prompt = "You keep this machine tidy.";
//! let model = ScriptedProvider::new([
//!     tool_answer(&[("t1", "Bash", json!({"command": "rm -rf /"}))]),
//!     text_answer("Understood, I will not."),
//! ]);
//! let deps = AgentDeps::new(
//!     SharedDeps::new(model.resolver(), "/project", ShutWorkspace),
//!     ModTools::new(tools, mods.clone()),
//!     ModSystemPrompt::new(prompt, mods.clone()),
//! );
//! let mut agent = Agent::new(deps, AgentOptions::new("scripted:model"));
//! let text = mods.rewrite_prompt("free up some disk").await;
//! let outcome = agent.run(&text, &mut Vec::new()).await;
//! assert_eq!(outcome.error, None);
//!
//! // The model saw the mod's line, and its Bash call came back refused.
//! assert!(model.requests()[0].system.contains("Ask before deleting anything."));
//! let refusal = agent.history().iter().find_map(|m| match m {
//!     Message::Tool(result) => Some(result),
//!     _ => None,
//! });
//! assert!(refusal.is_some_and(|r| r.is_error && r.content.contains("not on this machine")));
//! # }
//! ```
//!
//! # A native mod
//!
//! Implement [`Mod`]: a name, and only the events it hooks. Add it to the
//! loaded [`Mods`]; [`ModTools::wrap`] then runs every tool inside it.
//!
//! ```
//! use async_trait::async_trait;
//! use haest_core::{
//!     ShutWorkspace, ToolContext, Tools, error::BoxError, tool::input, workspace::WorkspaceMode,
//! };
//! use haest_mods::{Before, Mod, ModEnv, ModToolCall, ModTools, Mods};
//! use serde_json::json;
//! # use haest_mods::{ApprovalRequest, Decision, ModUi};
//! # struct Terminal;
//! # #[async_trait]
//! # impl ModUi for Terminal {
//! #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
//! #     fn info(&self, text: &str) { println!("{text}") }
//! # }
//! # struct Bash;
//! # #[async_trait]
//! # impl haest_core::Tool for Bash {
//! #     fn name(&self) -> &str { "Bash" }
//! #     fn description(&self) -> String { "Run a command".into() }
//! #     fn parameters(&self) -> serde_json::Value { json!({"type": "object"}) }
//! #     fn kind(&self) -> &str { "shell.exec" }
//! #     async fn execute(&self, i: haest_core::tool::JsonMap, _: &ToolContext)
//! #         -> Result<haest_core::ToolOutput, haest_core::ToolError> {
//! #         Ok(haest_core::ToolOutput::text(format!("ran {}", i["command"])))
//! #     }
//! # }
//!
//! /// Keeps `.env` files out of the transcript.
//! struct NoDotEnv;
//!
//! #[async_trait]
//! impl Mod for NoDotEnv {
//!     fn name(&self) -> &str {
//!         "no-dotenv"
//!     }
//!
//!     async fn tool_before(&self, call: &ModToolCall, _: &ToolContext, _: &ModEnv) -> Result<Before, BoxError> {
//!         let command = call.input.get("command").and_then(|c| c.as_str()).unwrap_or("");
//!         Ok(if call.name == "Bash" && command.contains(".env") {
//!             Before::Deny("this project keeps its secrets out of the transcript".into())
//!         } else {
//!             Before::Pass
//!         })
//!     }
//!
//!     async fn system_prompt(&self, _: &ModEnv) -> Result<Option<String>, BoxError> {
//!         Ok(Some("Never print a .env file.".into()))
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), BoxError> {
//! let env = ModEnv::new("/project", WorkspaceMode::Real, Terminal);
//! let mods = Mods::builder(env).with(NoDotEnv).build();
//! assert_eq!(mods.system_append().await, "Never print a .env file.");
//!
//! let tools = ModTools::wrap(&mods, Tools::new().with(Bash));
//! let ctx = ToolContext::new("/project", ShutWorkspace);
//! let call = |command: &str| input(json!({ "command": command }));
//! let refused = tools[0].execute(call("cat .env")?, &ctx).await;
//! assert!(refused.is_err_and(|e| e.to_string().contains("secrets")));
//! tools[0].execute(call("ls")?, &ctx).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # A mod file
//!
//! A `.js` or `.ts` file whose default export receives the `haest` API (all
//! of it is on [`JsMod`]). [`load_mods`] finds the files in the mod
//! directories; a host can also load one itself with [`JsMod::load`]. This one
//! adds a slash command and a tool:
//!
//! ```
//! use std::path::Path;
//!
//! use haest_core::{ShutWorkspace, ToolContext, Tools, tool::input, workspace::WorkspaceMode};
//! use haest_mods::{CommandOutcome, JsMod, ModEnv, ModTools, Mods};
//! use serde_json::json;
//! # use haest_mods::{ApprovalRequest, Decision, ModUi};
//! # struct Terminal;
//! # #[async_trait::async_trait]
//! # impl ModUi for Terminal {
//! #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
//! #     fn info(&self, text: &str) { println!("{text}") }
//! # }
//!
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
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_core::error::BoxError> {
//! let env = ModEnv::new("/project", WorkspaceMode::Sandbox, Terminal);
//! let standup = JsMod::load(Path::new("/mods/standup.ts"), STANDUP_TS.into(), &env).await?;
//! let mods = Mods::builder(env).with(standup).build();
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
//! ```
//!
//! # Approvals
//!
//! [`ApprovalMod`] asks before a Bash command in the real workspace that can
//! destroy work ([`irreversible_reason`]). The ask goes to the host's
//! [`ModUi::ask`]; most hosts answer it through an [`ApprovalGate`] over
//! their own [`Prompter`] (a terminal question, a dialog). An ALWAYS answer
//! covers the same command only, never the whole tool. [`ApprovalGate`'s
//! example](ApprovalGate#examples) builds such a host.
//!
//! ```
//! assert!(haest_mods::irreversible_reason("git push --force").is_some());
//! assert!(haest_mods::irreversible_reason("git push").is_none());
//! ```
//!
//! # Plan mode
//!
//! [`load_mods`] always adds [`PlanMod`]. `/plan` (or a host's plan toggle,
//! which runs the same command) turns it on: the system prompt gains the
//! planning brief ([`PLAN_PROMPT`]) and every tool that is not read-only is
//! refused until `/plan off`. [`PlanMod`'s example](PlanMod#examples) drives
//! it.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod api;
mod approval;
#[cfg(test)]
mod conversation_tests;
mod decorate;
mod gate;
mod js;
mod loader;
mod plan;
mod registry;
#[cfg(test)]
mod testkit;

pub use api::{
    ApprovalRequest, Before, Decision, Mod, ModEnv, ModRef, ModToolCall, ModUi, ModUiRef,
};
pub use approval::{
    ApprovalMod, AutoJudge, AutoJudgeRef, BashVerdict, SHELL_TOOLS, irreversible_reason,
};
pub use decorate::{ModSystemPrompt, ModTools};
pub use gate::{ASK_TIMEOUT, ApprovalAnswer, ApprovalGate, ModLine, Prompter, PrompterRef};
pub use js::{JsMod, ModCaps, ModLoadError};
pub use loader::{ModOptions, load_mods};
pub use plan::{PLAN_COMMAND, PLAN_PROMPT, PLANNING_TOOLS, PlanMod};
pub use registry::{CommandOutcome, Mods, ModsBuilder};
