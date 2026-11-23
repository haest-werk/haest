//! Mods: code that changes how a haest agent behaves, without a change to haest itself.
//!
//! A mod is plain Rust (a [`Mod`]) or a set of [`Hooks`]: one handler per
//! event, a Rust closure or anything else that implements [`Hook`]. A `.js`
//! / `.ts` mod file is a mod too, through a bridge: haest-bridges (feature
//! `js`) runs it in embedded QuickJS. All of them hook the same events of
//! the agent loop --
//!
//! | Event | When | May | Payload | Decision |
//! |---|---|---|---|---|
//! | `tool.before` | before every tool call | refuse it (the model reads the reason) or replace its input; the first deny wins | [`ToolBeforePayload`] | [`Before`] |
//! | `tool.after` | after the call ran | replace its output | [`ToolAfterPayload`] | `Option<ToolOutput>` |
//! | `prompt.submit` | a human message, before the model sees it | rewrite it | [`PromptSubmitPayload`] | `Option<String>` |
//! | `system.prompt` | before every model call | append text to the system prompt | [`SystemPromptPayload`] | `Option<String>` |
//! | `model.before` | before every model call, after the host's own hook | stop the turn, steer it, compact it | [`ModelBeforePayload`] | [`ModelBeforeDecision`] |
//! | `model.after` | an answer landed, its tool calls have not run | stop the turn (its calls do not run), steer it | [`ModelAfterPayload`] | [`ModelAfterDecision`] |
//! | `turn.end` | a turn is about to end on its own | keep it going with a message (the first wins), compact it | [`TurnEndPayload`] | [`TurnEndDecision`] |
//!
//! The payload is what a handler gets and the decision what it answers,
//! typed once ([`Event`]): as JSON they are exactly what a mod file's
//! handler has always been handed and returned (`{ deny }`, `{ input }`,
//! `{ stop }`, `{ steer }`, `{ continue }`, `{ compact }`), so a hook in
//! another process or behind a URL speaks the same words.
//!
//! -- add, replace and remove their own tools, hide any tool (the host's,
//! an MCP server's, another mod's) and show it again, add slash commands,
//! and reach the human through the host: `ask` puts an Allow / Deny prompt
//! in front of them, `notice` (a native mod's [`ModEnv::info`]) writes a
//! line they see. Two mods are built in: [`ApprovalMod`] (in the real
//! workspace, a Bash command that destroys work waits for the human's
//! Allow) and [`PlanMod`] (`/plan`: read-only until `/plan off`).
//!
//! A mod runs with the HOST's full trust, sandbox or not: a native mod is the
//! host's own code, and a mod file can do whatever its bridge lets it reach.
//! Load a project's mod files only from a directory the human trusts.
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
//! | [`Hooks`], [`Hook`], [`Binding`] | one handler per event as a mod: a closure, an `async fn` or any [`Hook`], each bound with a timeout, an [`OnError`] and (tool events) a tool-name matcher |
//! | [`Event`], [`ModEvent`] and the payload and decision types | the events' protocol: what a handler gets and answers, typed once, the mod files' own JSON |
//! | [`HookSettings`], [`HookEntry`], [`HookDialect`] | the `hooks` setting: commands and URLs bound per event, read from a config file or built in Rust, and checked; haest-bridges' `HookHost::bind` makes them a [`Hooks`] |
//! | [`load_mods`] | the built-ins plus the host's own mods, as [`Mods`] |
//! | [`Mods`] | every loaded mod, in order, as a cheap handle that never changes once built; a host's own native mods go in with [`ModOptions::with_mod`] or [`Mods::new`]. A `Mods` is a [`Mod`] itself, so a set can run inside another in its place |
//! | [`ModTools`], [`ModSystemPrompt`], [`ModLoopHooks`] | the wrappers a host puts around its tool source, its system prompt and its [`AgentHooks`](haest_core::AgentHooks) |
//! | [`Mods::rewrite_prompt`], [`Mods::command`] | `prompt.submit` and slash commands, for the host's input path |
//! | [`ModUi`] | PLUG POINT: how the host asks the human and shows them a line |
//! | [`ApprovalGate`], [`Prompter`] | the generic half of a host's ask: scoped ALWAYS answers, a timeout, interrupts; [`ApprovalGate::mod_ui`] makes it the host's whole [`ModUi`] |
//! | [`ModRef`], [`ModUiRef`], [`AutoJudgeRef`], [`PrompterRef`] | the handles the plug points are held by; a parameter that takes `impl Into<..Ref>` takes the value itself, or an `Arc` of it |
//! | `testing` (feature `testing`) | fakes for tests of mods, and the decision cases a bridge must answer the way [`Hooks`] does; for `dev-dependencies` only, and outside semver like every `testing` module in haest |
//!
//! # Putting mods in the loop
//!
//! Load the mods, wrap the agent's tool source in [`ModTools`] and its system
//! prompt in [`ModSystemPrompt`] (and its hooks in [`ModLoopHooks`], for
//! the loop events: [`Hooks`' example](Hooks#examples) uses them), and pass
//! the human's text through [`Mods::rewrite_prompt`] before it goes to the
//! agent. The model here is scripted, so the example runs offline.
//!
//! ```
//! use haest_core::{
//!     Agent, AgentDeps, AgentOptions, Message, SharedDeps, ShutWorkspace, Tools,
//!     error::BoxError,
//!     testing::{ScriptedProvider, text_answer, tool_answer},
//!     tool::Tool,
//!     workspace::WorkspaceMode,
//! };
//! use haest_mods::{
//!     Before, Binding, Hooks, ModOptions, ModSystemPrompt, ModTools, SystemPromptPayload,
//!     ToolBeforePayload, load_mods,
//! };
//! use serde_json::json;
//! use tokio_util::sync::CancellationToken;
//! # use haest_mods::{ApprovalRequest, Decision, ModUi};
//! # struct Terminal;
//! # #[async_trait::async_trait]
//! # impl ModUi for Terminal {
//! #     async fn ask(&self, _: ApprovalRequest, _: &CancellationToken) -> Decision { Decision::Deny }
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
//! // The host's own rule for this machine, as two hooks.
//! let guard = |p: ToolBeforePayload, _: CancellationToken| async move {
//!     let command = p.call.input.get("command").and_then(|c| c.as_str()).unwrap_or("");
//!     Ok::<_, BoxError>(if command.contains("rm -rf /") {
//!         Before::Deny("not on this machine".into())
//!     } else {
//!         Before::Pass
//!     })
//! };
//! let line = |_: SystemPromptPayload, _: CancellationToken| async {
//!     Ok::<_, BoxError>(Some("Ask before deleting anything.".to_string()))
//! };
//! let tidy = Hooks::new()
//!     .with_name("tidy")
//!     .on_tool_before(Binding::new(guard).with_matcher("Bash"))
//!     .on_system_prompt(line);
//! let config = ModOptions::new("/project", WorkspaceMode::Real).with_mod(tidy);
//! let mods = load_mods(config, Terminal);
//! assert_eq!(mods.names(), ["approval", "plan", "tidy"]);
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
//! // The model saw the hook's line, and its Bash call came back refused.
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
//! let mods = Mods::new(env, [NoDotEnv]);
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
//! # Hooks
//!
//! When a mod is only a few handlers, bind them per event instead:
//! [`Hooks`] takes a closure (or any [`Hook`]) for each event, with a
//! timeout, what its failure does ([`OnError`]) and, on a tool event, the
//! tools it is for, and goes into [`Mods`] like any mod.
//! [`Hooks`' example](Hooks#examples) guards Bash with a matcher and sends
//! a turn back once.
//!
//! # Mod files
//!
//! A `.js` or `.ts` file whose default export receives the `haest` API is a
//! bridge's mod, not this crate's: haest-bridges (feature `js`) loads one as
//! its `JsMod`, and a conversation's mod files as one host mod, its
//! `ModFiles`, which reloads them as they change. A host adds that with
//! [`ModOptions::with_mod`], first among its own; haest-runtime does.
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
mod ask;
#[cfg(test)]
mod conversation_tests;
mod decision;
mod decorate;
mod env;
mod events;
mod fold;
mod gate;
mod hook;
mod hook_set;
mod hook_settings;
mod loader;
mod loop_hooks;
mod payload;
mod plan;
#[cfg(test)]
mod protocol_tests;
mod registry;
#[cfg(any(test, feature = "testing"))]
#[cfg_attr(docsrs, doc(cfg(feature = "testing")))]
pub mod testing;

/// What the other haest crates share, and nothing a host needs: not part of
/// the API, and not covered by semver. It changes in any release.
#[doc(hidden)]
pub mod __private {
    /// How a mod made of handlers asks them and folds their answers: a mod
    /// file's (haest-bridges' `JsMod`) by the same code as [`Hooks`](crate::Hooks)'.
    pub mod fold {
        pub use crate::fold::*;
    }
}

pub use api::{Before, Mod, ModRef, ModToolCall, Step, TurnEnd};
pub use approval::{
    ApprovalMod, AutoJudge, AutoJudgeRef, BashVerdict, SHELL_TOOLS, irreversible_reason,
};
pub use ask::{ApprovalRequest, Decision};
pub use decision::{Compact, ModelAfterDecision, ModelBeforeDecision, TurnEndDecision};
pub use decorate::{ModSystemPrompt, ModTools};
pub use env::{ModEnv, ModUi, ModUiRef};
pub use events::{Event, ModEvent, ToolEvent};
pub use gate::{ASK_TIMEOUT, ApprovalAnswer, ApprovalGate, ModLine, Prompter, PrompterRef};
pub use hook::{Binding, DEFAULT_HOOK_TIMEOUT, Hook, HookError, OnError};
pub use hook_set::Hooks;
pub use hook_settings::{HookDialect, HookEntry, HookEntryError, HookSettings};
pub use loader::{ModOptions, load_mods};
pub use loop_hooks::ModLoopHooks;
pub use payload::{
    AgentView, ModelAfterPayload, ModelBeforePayload, PromptSubmitPayload, SessionView,
    SystemPromptPayload, ToolAfterPayload, ToolBeforePayload, ToolCallContext, TurnEndPayload,
};
pub use plan::{PLAN_COMMAND, PLAN_PROMPT, PLANNING_TOOLS, PlanMod};
pub use registry::{CommandOutcome, Mods};
