//! FILE MODS: a `.js` or `.ts` file whose default export receives the `kern`
//! API, run in embedded QuickJS -- one runtime and one thread per file. The API
//! mirrors the TypeScript kern's:
//!
//! ```js
//! export default function (kern) {
//!   kern.on('tool.before', call =>
//!     call.name === 'Bash' && /rm -rf \//.test(String(call.input.command))
//!       ? { deny: 'not on my watch' } : undefined)
//! }
//! ```
//!
//! `kern.cwd`, `kern.workspace`, `kern.on(event, fn)`, `kern.tool(t)`,
//! `kern.command(name, run)`, `kern.ask(req, ctx.signal)` (a promise of
//! 'allow' | 'deny') and `kern.notice(text)`. Handlers of `tool.before`,
//! `tool.after` and a tool's `execute` get `ctx = { cwd, toolCallId, signal }`.
//! `.ts` is type-stripped first. There is no `node:` API, no `import`, no
//! timer, no network and no file access: a mod reaches the world through the
//! `kern` API alone.

mod channel;
mod outbox;
mod strip;
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
use kern_core::tool::{Tool, ToolContext, ToolOutput};
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

/// One loaded mod file.
pub struct JsMod {
    name: String,
    thread: Arc<JsThread>,
    meta: Arc<Mutex<JsMeta>>,
}

impl JsMod {
    /// Run one mod file: strip its types (`.ts`), evaluate it in a fresh QuickJS
    /// runtime on its own thread, and call its default export with the `kern`
    /// API. Err = why it did not load (the host reports it and moves on).
    pub async fn load(path: &Path, source: String, kern: &ModEnv) -> Result<Self, String> {
        Self::load_with(path, source, kern, Limits::default()).await
    }

    pub(crate) async fn load_with(
        path: &Path,
        source: String,
        kern: &ModEnv,
        limits: Limits,
    ) -> Result<Self, String> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "file mods load inside a tokio runtime".to_string())?;
        let meta = Arc::new(Mutex::new(JsMeta::default()));
        let setup = Setup {
            path: path.to_path_buf(),
            source,
            host: kern.host.clone(),
            meta: meta.clone(),
            limits,
            runtime,
        };
        let thread = Arc::new(JsThread::new(thread::start(setup).await?));
        let env = json!({ "env": { "cwd": kern.cwd, "workspace": kern.workspace.as_str() } });
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
        _kern: &ModEnv,
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
        _kern: &ModEnv,
    ) -> Result<Option<ToolOutput>, ModError> {
        let payload = json!({ "call": call, "output": output, "ctx": ctx_json(ctx) });
        match self.event("tool.after", payload, Some(&ctx.cancel)).await? {
            Value::Null => Ok(None),
            v => serde_json::from_value(v)
                .map(Some)
                .map_err(|e| ModError(format!("tool.after returned no tool output: {e}"))),
        }
    }

    async fn prompt_submit(&self, text: &str, _kern: &ModEnv) -> Result<Option<String>, ModError> {
        let v = self
            .event("prompt.submit", json!({ "text": text }), None)
            .await?;
        text_of(v, "prompt.submit")
    }

    async fn system_prompt(&self, _kern: &ModEnv) -> Result<Option<String>, ModError> {
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
        _kern: &ModEnv,
    ) -> Result<Option<String>, ModError> {
        let payload = json!({ "name": name, "args": args });
        let v = self.thread.call("command", payload, None).await?;
        text_of(v, "command")
    }
}
