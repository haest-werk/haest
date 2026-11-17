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
//! `haest.command(name, run)`, `haest.ask(req, ctx.signal)` (a promise of
//! 'allow' | 'deny') and `haest.notice(text)`. Handlers of `tool.before`,
//! `tool.after` and a tool's `execute` get `ctx = { cwd, toolCallId, signal }`.
//! `.ts` is type-stripped first. There is no `node:` API, no `import`, no
//! timer, no network and no file access: a mod reaches the world through the
//! `haest` API alone.

mod channel;
mod load;
mod outbox;
#[cfg(test)]
mod tests;
mod thread;
mod tool;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::{
    ToolRef, Tools,
    error::BoxError,
    tool::{ToolContext, ToolOutput},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::api::{Before, Mod, ModEnv, ModToolCall};
use channel::{JsMeta, JsThread};
pub use load::{ModCaps, ModLoadError};
use tool::JsTool;

/// What a handler of a tool event sees as `ctx` (`signal` is added in the runtime).
fn ctx_json(ctx: &ToolContext) -> Value {
    json!({ "cwd": ctx.cwd, "toolCallId": ctx.tool_call_id })
}

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
/// | `haest.on(event, fn)` | hook `'tool.before'`, `'tool.after'`, `'prompt.submit'` or `'system.prompt'`; any other name throws |
/// | `haest.tool(t)` | add a tool: `{ name, execute(input, ctx) }`, and optionally `description`, `parameters` (a JSON Schema), `kind`, `readOnly`; a second tool of the same name replaces the first |
/// | `haest.command(name, run)` | add a slash command (a leading `/` is dropped): `run(args)` returns text for the model, or nothing |
/// | `haest.ask(req, ctx.signal)` | a promise of `'allow'` or `'deny'` from the human; `req` is an [`ApprovalRequest`](crate::ApprovalRequest) in camelCase, and with `ctx.signal` the ask ends as a deny when that call is interrupted |
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
///
/// `call` is a [`ModToolCall`] and `output` a tool output, both in camelCase.
/// `ctx` is `{ cwd, toolCallId, signal }`; a tool's `execute` gets it too,
/// and returns a string or `{ content, isError }`. `tool.before`,
/// `tool.after`, `execute` and commands may be async; `prompt.submit` and
/// `system.prompt` must answer at once (a returned promise is an error).
/// Several handlers of one event run in the order they were added. A handler
/// that throws is a failed event ([`Mods`](crate::Mods) says what that means).
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
/// caps with [`JsMod::load_with`] or
/// [`ModOptions::caps`](crate::ModOptions::caps).
///
/// The thread stops when the `JsMod` and every tool it added are dropped.
pub struct JsMod {
    name: String,
    thread: Arc<JsThread>,
    meta: Arc<Mutex<JsMeta>>,
}

impl JsMod {
    fn hooks(&self, event: &str) -> bool {
        self.meta.lock().unwrap().events.contains(event)
    }

    /// An event this mod has a handler for; Ok(Null) when it has none.
    async fn event(
        &self,
        event: &'static str,
        payload: Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, BoxError> {
        if !self.hooks(event) {
            return Ok(Value::Null);
        }
        Ok(self.thread.call(event, payload, cancel).await?)
    }
}

/// A handler returned what its event does not take.
#[derive(Debug, thiserror::Error)]
enum BadReturn {
    #[error("{event} returned {got}, not text")]
    NotText { event: &'static str, got: Value },
    #[error("tool.before returned an input that is not an object")]
    InputNotObject,
    #[error("tool.before returned {0}")]
    Before(Value),
    #[error("tool.after returned no tool output")]
    NotOutput(#[source] serde_json::Error),
}

fn text_of(v: Value, event: &'static str) -> Result<Option<String>, BoxError> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s)),
        got => Err(BadReturn::NotText { event, got }.into()),
    }
}

#[async_trait]
impl Mod for JsMod {
    fn name(&self) -> &str {
        &self.name
    }

    async fn tool_before(
        &self,
        call: &ModToolCall,
        ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        let payload = json!({ "call": call, "ctx": ctx_json(ctx) });
        match self
            .event("tool.before", payload, Some(&ctx.cancel))
            .await?
        {
            Value::Null => Ok(Before::Pass),
            Value::Object(mut v) => match (v.remove("deny"), v.remove("input")) {
                (Some(Value::String(reason)), _) => Ok(Before::Deny(reason)),
                (_, Some(Value::Object(input))) => Ok(Before::Input(input)),
                _ => Err(BadReturn::InputNotObject.into()),
            },
            other => Err(BadReturn::Before(other).into()),
        }
    }

    async fn tool_after(
        &self,
        call: &ModToolCall,
        output: &ToolOutput,
        ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Option<ToolOutput>, BoxError> {
        let payload = json!({ "call": call, "output": output, "ctx": ctx_json(ctx) });
        match self.event("tool.after", payload, Some(&ctx.cancel)).await? {
            Value::Null => Ok(None),
            v => serde_json::from_value(v)
                .map(Some)
                .map_err(|e| BadReturn::NotOutput(e).into()),
        }
    }

    async fn prompt_submit(&self, text: &str, _haest: &ModEnv) -> Result<Option<String>, BoxError> {
        let v = self
            .event("prompt.submit", json!({ "text": text }), None)
            .await?;
        text_of(v, "prompt.submit")
    }

    async fn system_prompt(&self, _haest: &ModEnv) -> Result<Option<String>, BoxError> {
        let v = self.event("system.prompt", json!({}), None).await?;
        text_of(v, "system.prompt")
    }

    fn tools(&self) -> Tools {
        let meta = self.meta.lock().unwrap();
        meta.tools
            .iter()
            .map(|m| ToolRef::new(JsTool::new(m.clone(), self.thread.clone())))
            .collect()
    }

    fn commands(&self) -> Vec<String> {
        self.meta.lock().unwrap().commands.clone()
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
