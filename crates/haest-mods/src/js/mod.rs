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
mod outbox;
#[cfg(test)]
mod tests;
mod thread;
mod tool;

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use haest_core::tool::{Tool, ToolContext, ToolOutput};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::api::{Before, Mod, ModEnv, ModError, ModToolCall};
pub(crate) use channel::Limits;
use channel::{JsMeta, JsThread};
use thread::Setup;
use tool::JsTool;

/// A mod whose default function does not return within this is skipped.
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

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
/// has its own QuickJS runtime on its own thread, so mods share nothing. The
/// runtime has 128 MiB of memory. Code that runs 10 s without a pause
/// (awaiting does not count) is stopped: every call into the mod in flight
/// then fails, and the next call runs as usual. The default export must
/// return within 30 s. A host cannot change these limits.
///
/// The thread stops when the `JsMod` and every tool it added are dropped.
pub struct JsMod {
    name: String,
    thread: Arc<JsThread>,
    meta: Arc<Mutex<JsMeta>>,
}

impl JsMod {
    /// Runs one mod file: strips its types (a `.ts` path, through
    /// `haest-ts`), evaluates `source` as an ES module in a fresh QuickJS
    /// runtime on its own thread, and calls its default export with the
    /// `haest` API. `path` names the mod ([`Mod::name`]) and its errors; the
    /// file is not read, `source` is the code.
    ///
    /// What the default export registers (events, tools, commands) is in
    /// place when this returns.
    ///
    /// [`load_mods`](crate::load_mods) calls this for every file in the mod
    /// directories; a host calls it to load one file of its own, then
    /// [`Mods::add`](crate::Mods::add)s it.
    ///
    /// # Errors
    ///
    /// The `Err` says why the mod did not load, for the human: no tokio
    /// runtime is running, the TypeScript or JavaScript does not parse, the
    /// module threw, its default export is not a function, its top-level
    /// `await` never finished, or the default export threw or did not return
    /// within 30 s.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::{path::Path, sync::Arc};
    /// # use haest_core::workspace::WorkspaceMode;
    /// # use haest_mods::{ApprovalRequest, Decision, ModUi};
    /// use haest_mods::{JsMod, Mod, ModEnv};
    /// # struct Terminal;
    /// # #[async_trait::async_trait]
    /// # impl ModUi for Terminal {
    /// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
    /// #     fn notice(&self, _: &str) {}
    /// # }
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let env = ModEnv::new("/project", WorkspaceMode::Sandbox, Arc::new(Terminal));
    /// let source = "export default (haest: any) => haest.on('prompt.submit', (t: string) => t.trim())";
    /// let m = JsMod::load(Path::new("/mods/trim.ts"), source.into(), &env).await.unwrap();
    /// assert_eq!(m.prompt_submit("  hello  ", &env).await.unwrap().as_deref(), Some("hello"));
    ///
    /// let broken = JsMod::load(Path::new("/mods/broken.js"), "export default 42".into(), &env).await;
    /// assert!(broken.is_err());
    /// # }
    /// ```
    pub async fn load(path: &Path, source: String, haest: &ModEnv) -> Result<Self, String> {
        Self::load_with(path, source, haest, Limits::default()).await
    }

    pub(crate) async fn load_with(
        path: &Path,
        source: String,
        haest: &ModEnv,
        limits: Limits,
    ) -> Result<Self, String> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "file mods load inside a tokio runtime".to_string())?;
        let meta = Arc::new(Mutex::new(JsMeta::default()));
        let setup = Setup {
            path: path.to_path_buf(),
            source,
            ui: haest.ui.clone(),
            meta: meta.clone(),
            limits,
            runtime,
        };
        let thread = Arc::new(JsThread::new(thread::start(setup).await?));
        let env = json!({ "env": { "cwd": haest.cwd, "workspace": haest.workspace.as_str() } });
        match tokio::time::timeout(LOAD_TIMEOUT, thread.call("load", env, None)).await {
            Err(_) => Err(format!(
                "its default export did not return within {}s",
                LOAD_TIMEOUT.as_secs()
            )),
            Ok(Err(e)) => Err(ModError::from(e).0),
            Ok(Ok(_)) => Ok(Self {
                name: path.display().to_string(),
                thread,
                meta,
            }),
        }
    }

    fn hooks(&self, event: &str) -> bool {
        self.meta.lock().unwrap().events.contains(event)
    }

    /// An event this mod has a handler for; Ok(Null) when it has none.
    async fn event(
        &self,
        event: &'static str,
        payload: Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, ModError> {
        if !self.hooks(event) {
            return Ok(Value::Null);
        }
        Ok(self.thread.call(event, payload, cancel).await?)
    }
}

fn text_of(v: Value, event: &str) -> Result<Option<String>, ModError> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s)),
        other => Err(ModError(format!("{event} returned {other}, not text"))),
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
    ) -> Result<Before, ModError> {
        let payload = json!({ "call": call, "ctx": ctx_json(ctx) });
        match self
            .event("tool.before", payload, Some(&ctx.cancel))
            .await?
        {
            Value::Null => Ok(Before::Pass),
            Value::Object(mut v) => match (v.remove("deny"), v.remove("input")) {
                (Some(Value::String(reason)), _) => Ok(Before::Deny(reason)),
                (_, Some(Value::Object(input))) => Ok(Before::Input(input)),
                _ => Err(ModError::new(
                    "tool.before returned an input that is not an object",
                )),
            },
            other => Err(ModError(format!("tool.before returned {other}"))),
        }
    }

    async fn tool_after(
        &self,
        call: &ModToolCall,
        output: &ToolOutput,
        ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Option<ToolOutput>, ModError> {
        let payload = json!({ "call": call, "output": output, "ctx": ctx_json(ctx) });
        match self.event("tool.after", payload, Some(&ctx.cancel)).await? {
            Value::Null => Ok(None),
            v => serde_json::from_value(v)
                .map(Some)
                .map_err(|e| ModError(format!("tool.after returned no tool output: {e}"))),
        }
    }

    async fn prompt_submit(&self, text: &str, _haest: &ModEnv) -> Result<Option<String>, ModError> {
        let v = self
            .event("prompt.submit", json!({ "text": text }), None)
            .await?;
        text_of(v, "prompt.submit")
    }

    async fn system_prompt(&self, _haest: &ModEnv) -> Result<Option<String>, ModError> {
        let v = self.event("system.prompt", json!({}), None).await?;
        text_of(v, "system.prompt")
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let meta = self.meta.lock().unwrap();
        meta.tools
            .iter()
            .map(|m| Arc::new(JsTool::new(m.clone(), self.thread.clone())) as Arc<dyn Tool>)
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
    ) -> Result<Option<String>, ModError> {
        let payload = json!({ "name": name, "args": args });
        let v = self.thread.call("command", payload, None).await?;
        text_of(v, "command")
    }
}
