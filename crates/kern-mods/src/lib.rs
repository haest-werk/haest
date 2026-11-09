//! KERN MODS: code that changes how kern behaves, without a kern-core change.
//! One idea of "a mod" covers native Rust and mod files: something that hooks
//! these events --
//!
//! | Event | When | May |
//! |---|---|---|
//! | `tool.before` | before every tool call | refuse it (the model reads the reason) or replace its input; the first deny wins |
//! | `tool.after` | after it | replace the output |
//! | `prompt.submit` | a human message, before the model sees it | rewrite it |
//! | `system.prompt` | before every model call | append text to the system prompt |
//!
//! -- and adds tools and slash commands, and reaches the human through the host:
//! `ask` puts an Allow / Deny prompt in front of them, `notice` writes a line
//! they see. A mod runs with the HOST's full trust, sandbox or not.
//!
//! | Piece | What |
//! |---|---|
//! | [`Mod`] | a native mod (the built-ins [`ApprovalMod`] and [`PlanMod`] are ones) |
//! | [`JsMod`] | a `.js` / `.ts` mod file in embedded QuickJS |
//! | [`load_mods`] | built-ins + the mod files of one conversation, into [`Mods`] |
//! | [`ModTools`], [`ModSystemPrompt`] | the decorators a host puts around its `ToolSource` and `SystemPrompt` |
//! | [`Mods::rewrite_prompt`], [`Mods::command`] | `prompt.submit` and slash commands, for the host's input path |
//! | [`ModHost`] | PLUG POINT: the host's ask / notice; [`ApprovalGate`] keeps ALWAYS and timeouts for it |

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

pub use api::{ApprovalRequest, Before, Decision, Mod, ModEnv, ModError, ModHost, ModToolCall};
pub use approval::{ApprovalMod, AutoJudge, BashVerdict, irreversible_reason};
pub use decorate::{ModSystemPrompt, ModTools};
pub use gate::{ASK_TIMEOUT, Answer, ApprovalGate, Prompter};
pub use js::JsMod;
pub use loader::{ModConfig, load_mods, mod_files};
pub use plan::{PLAN_COMMAND, PLAN_PROMPT, PlanListener, PlanMod};
pub use registry::{CommandOutcome, Mods};
