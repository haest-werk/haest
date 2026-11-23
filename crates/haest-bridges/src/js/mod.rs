//! FILE MODS: a `.js` or `.ts` file whose default export receives the `haest`
//! API, run in embedded QuickJS -- one runtime and one thread per file. The API
//! mirrors the TypeScript haest's:
//!
//! ```js
//! export default function (haest) {
//!   haest.on('tool.before', call =>
//!     call.name === 'Bash' && /rm -rf \//.test(String(call.input.command))
//!       ? { deny: 'not on my watch' } : undefined)
//! }
//! ```
//!
//! `haest.cwd`, `haest.workspace`, `haest.on(event, fn)`, `haest.tool(t)`,
//! `haest.removeTool(name)`, `haest.disableTool(name)`,
//! `haest.enableTool(name)`, `haest.command(name, run)`,
//! `haest.ask(req, ctx.signal)` (a promise of 'allow' | 'deny') and
//! `haest.notice(text)`. The events are haest-mods' list (`ModEvent`),
//! handed to the JavaScript half (bootstrap.js) at load. Handlers of
//! `tool.before`, `tool.after` and a tool's `execute` get
//! `ctx = { cwd, toolCallId, signal }`. `.ts` is type-stripped first. There
//! is no `node:` API, no `import`, no timer, no network and no file access:
//! a mod reaches the world through the `haest` API alone.
//!
//! A conversation's mod files, found in its mod directories and reloaded
//! as they change, are one mod: [`ModFiles`] (files.rs).

mod channel;
mod files;
mod load;
#[cfg(test)]
mod loop_tests;
mod outbox;
#[cfg(test)]
mod protocol_tests;
mod returns;
#[cfg(test)]
mod tests;
mod thread;
mod tool;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::{
    Agent, AssistantMessage, LiveTools, ToolRef, Tools, TurnOutcome,
    error::BoxError,
    tool::{ToolContext, ToolOutput, ToolSource},
};
use haest_mods::{
    Before, Event, Mod, ModEnv, ModEvent, ModToolCall, ModelAfterDecision, ModelAfterPayload,
    ModelBeforeDecision, ModelBeforePayload, PromptSubmitPayload, Step, SystemPromptPayload,
    ToolAfterPayload, ToolBeforePayload, TurnEnd, TurnEndDecision, TurnEndPayload,
};
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use channel::{JsMeta, JsThread, ToolChange};
pub use files::{ModFileOptions, ModFiles};
pub use load::{ModCaps, ModLoadError};
use returns::{BadReturn, text_of};
use tool::JsTool;

/// One loaded mod file: a `.js` or `.ts` ES module run in embedded QuickJS,
/// whose default export receives the `haest` API.
///
/// ```js
/// export default function (haest) {
///   haest.on('tool.before', call =>
///     call.name === 'Bash' && /rm -rf \//.test(String(call.input.command))
///       ? { deny: 'not on my watch' } : undefined)
/// }
/// ```
///
/// # The `haest` API
///
/// | Member | What |
/// |---|---|
/// | `haest.cwd` | the conversation's working directory |
/// | `haest.workspace` | `'sandbox'`, `'real'` or `'shut'` |
/// | `haest.on(event, fn)` | hook one of the events below; any other name throws |
/// | `haest.tool(t)` | add a tool: `{ name, execute(input, ctx) }`, and optionally `description`, `parameters` (a JSON Schema), `kind`, `readOnly`; a second tool of the same name replaces the first where it stands |
/// | `haest.removeTool(name)` | drop a tool this mod added; true when it had one of that name. Another's tool is not this mod's to remove: hide it |
/// | `haest.disableTool(name)` | hide any tool, the host's, an MCP server's or a mod's: neither offered to the model nor run ([`Mod::disabled_tools`]) |
/// | `haest.enableTool(name)` | show a hidden tool again, as it was, where it was |
/// | `haest.command(name, run)` | add a slash command (a leading `/` is dropped): `run(args)` returns text for the model, or nothing |
/// | `haest.ask(req, ctx.signal)` | a promise of `'allow'` or `'deny'` from the human; `req` is an [`ApprovalRequest`](haest_mods::ApprovalRequest) in camelCase, and with `ctx.signal` the ask ends as a deny when that call is interrupted |
/// | `haest.notice(text)` | a line the human sees |
///
/// What each event's handler gets and may return:
///
/// | Event | Called with | Returns |
/// |---|---|---|
/// | `tool.before` | `call`, `ctx` | `{ deny: reason }` refuses, `{ input: {...} }` replaces the input for the handlers after it, anything else passes |
/// | `tool.after` | `call`, `output`, `ctx` | a new output (`{ content, isError }`), or nothing to keep it |
/// | `prompt.submit` | `text` | the new text, or nothing |
/// | `system.prompt` | nothing | text to append, or nothing |
/// | `model.before` | the agent, `lastMessage` | `{ stop: reason }` ends the turn, `{ steer: text }` steers it, `{ compact: true }` or `{ compact: { instructions } }` compacts it first; anything else passes |
/// | `model.after` | the agent, `answer` | `{ stop: reason }` ends the turn and its calls do not run, `{ steer: text }` steers it; anything else passes |
/// | `turn.end` | the agent, `stopReason`, `error`, `answer`, `continued` | `{ continue: text }` keeps the turn going with `text`, `{ compact: ... }` compacts it as it ends; anything else passes |
///
/// "The agent" is one object's fields, the same in every loop event
/// ([`AgentView`](haest_mods::AgentView)): `modelSpec`, `effort`,
/// `historyLength`, `contextTokens` (what the last answer's request held)
/// and `contextWindow` (`null` when the host does not know it).
///
/// What a handler gets is its event's payload and what it returns is read
/// as its event's decision, the types [`Event`] ties together
/// ([haest-mods' docs](haest_mods) list them): a [`Hook`](haest_mods::Hook)
/// in Rust or in another process gets and answers the same.
/// `call` is a [`ModToolCall`] and `output` a tool output, both in camelCase.
/// `ctx` is `{ cwd, toolCallId, signal }`; a tool's `execute` gets it too,
/// and returns a string or `{ content, isError }`. `lastMessage` and
/// `answer` are messages as a session file writes them, less media bytes,
/// thinking signatures and provider state; the loop events never see the
/// whole history. `continued` is true once a `turn.end` answer has kept
/// this turn going: a handler that keeps a turn going once checks it. A loop event's handlers run in order: every steer is
/// kept, the first compact counts, the first stop or continue ends the run.
/// `tool.before`, `tool.after`, the loop events, `execute` and commands may
/// be async (an interrupt stops the wait); `prompt.submit` and
/// `system.prompt` must answer at once (a returned promise is an error).
/// Several handlers of one event run in the order they were added. A handler
/// that throws is a failed event ([`Mods`](haest_mods::Mods) says what that
/// means).
///
/// # What a mod file cannot do
///
/// There is no `node:` API, no `import`, no timer, no network and no file
/// access: a mod reaches the world through the `haest` API alone. Each file
/// has its own QuickJS runtime on its own thread, so mods share nothing.
/// [`ModCaps`] bounds it: by default the runtime has 128 MiB of memory,
/// code that runs 10 s without a pause (awaiting does not count) is stopped
/// (every call into the mod in flight then fails, and the next call runs as
/// usual), and the default export must return within 30 s. A host sets other
/// caps with [`JsMod::load_with`] or [`ModFileOptions::with_caps`].
///
/// The thread stops when the `JsMod` and every tool it added are dropped.
pub struct JsMod {
    name: String,
    thread: Arc<JsThread>,
    meta: Arc<Mutex<JsMeta>>,
    /// The tools it added, in the order it added them.
    tools: LiveTools,
}

impl std::fmt::Debug for JsMod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsMod")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl JsMod {
    fn hooks(&self, event: ModEvent) -> bool {
        JsMeta::lock(&self.meta).events.contains(event.as_str())
    }

    /// `E`'s handlers on the payload `payload` builds (only when the mod
    /// has one), and their answer: `E`'s decision, or a loop event's list
    /// of them; its default (a pass) when the mod has no handler.
    async fn answer<E: Event, T: DeserializeOwned + Default>(
        &self,
        payload: impl FnOnce() -> E,
        cancel: Option<&CancellationToken>,
    ) -> Result<T, BoxError> {
        let event = E::NAME;
        if !self.hooks(event) {
            return Ok(T::default());
        }
        let payload = serde_json::to_value(payload())
            .map_err(|source| BadReturn::Payload { event, source })?;
        let got = self.thread.call(event.as_str(), payload, cancel).await?;
        let answer: Option<T> =
            serde_json::from_value(got).map_err(|source| BadReturn::Answer { event, source })?;
        Ok(answer.unwrap_or_default())
    }
}

/// Every method is named, so an event `Mod` gains fails the gates here
/// until it has its twin in a mod file (haest-mods' events, bootstrap.js).
#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl Mod for JsMod {
    fn name(&self) -> &str {
        &self.name
    }

    /// A mod file never judges a call after the others.
    fn is_gate(&self) -> bool {
        false
    }

    async fn tool_before(
        &self,
        call: &ModToolCall,
        ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        let payload = || ToolBeforePayload::new(call, ctx);
        self.answer(payload, Some(ctx.cancel())).await
    }

    async fn tool_after(
        &self,
        call: &ModToolCall,
        output: &ToolOutput,
        ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Option<ToolOutput>, BoxError> {
        let payload = || ToolAfterPayload::new(call, output, ctx);
        self.answer(payload, Some(ctx.cancel())).await
    }

    async fn prompt_submit(&self, text: &str, _haest: &ModEnv) -> Result<Option<String>, BoxError> {
        self.answer(|| PromptSubmitPayload::new(text), None).await
    }

    async fn system_prompt(&self, _haest: &ModEnv) -> Result<Option<String>, BoxError> {
        self.answer(SystemPromptPayload::new, None).await
    }

    async fn model_before(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
        _haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        let payload = || ModelBeforePayload::new(agent);
        let decided = self.answer(payload, Some(cancel)).await?;
        ModelBeforeDecision::apply(decided, agent, cancel).await
    }

    async fn model_after(
        &self,
        agent: &mut Agent,
        answer: &AssistantMessage,
        cancel: &CancellationToken,
        _haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        let payload = || ModelAfterPayload::new(agent, answer);
        let decided = self.answer(payload, Some(cancel)).await?;
        Ok(ModelAfterDecision::apply(decided, agent))
    }

    async fn turn_end(
        &self,
        agent: &mut Agent,
        outcome: &TurnOutcome,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<TurnEnd, BoxError> {
        let payload = || TurnEndPayload::new(agent, outcome, cancel, haest);
        let decided = self.answer(payload, Some(cancel)).await?;
        TurnEndDecision::apply(decided, agent, cancel).await
    }

    /// The tools as the mod left them: what it added or replaced since the
    /// last look goes into its [`LiveTools`] first, in the order it did it.
    fn tools(&self) -> Tools {
        let mut meta = JsMeta::lock(&self.meta);
        for change in meta.tool_changes.drain(..) {
            match change {
                ToolChange::Set(m) => {
                    let tool = JsTool::new(m, self.thread.clone());
                    self.tools.set_tool(ToolRef::new(tool));
                }
                ToolChange::Remove(name) => {
                    self.tools.remove_tool(&name);
                }
            }
        }
        drop(meta);
        self.tools.tools()
    }

    fn disabled_tools(&self) -> Vec<String> {
        JsMeta::lock(&self.meta).disabled.clone()
    }

    fn commands(&self) -> Vec<String> {
        JsMeta::lock(&self.meta).commands.clone()
    }

    async fn command(
        &self,
        name: &str,
        args: &str,
        _haest: &ModEnv,
    ) -> Result<Option<String>, BoxError> {
        let payload = json!({ "name": name, "args": args });
        let v = self.thread.call("command", payload, None).await?;
        text_of(v, "command")
    }
}
