//! haest's side of one Code run ([`super::CodeTool`]): what the guest's
//! `tools.X()` and `see()` reach. Each inner call goes through the gate, is counted
//! against the cap, runs through haest-core's tool execution (error results and
//! canonical envelopes exactly as in the loop, but NO spill: the program gets a
//! big result whole and filters it, which is why Code exists), has its spend folded in, and
//! is REPORTED as a start / end pair ([`ToolServices::on_inner_call`]) so the
//! transcript shows it as a row of its own -- the "transparent wrapper" Claude
//! Code's REPL tool used, adapted: the Code row stays, its calls appear beneath it.

use std::sync::{Arc, Mutex};

use futures::FutureExt;
use haest_core::{
    __private::{ToolIndex, run_unspilled},
    ToolRef, Tools,
    messages::{MediaPart, ToolCall},
    tool::{JsonMap, ToolContext, ToolOutput, ToolServicesRef},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{render::CallRecord, report::InnerRow};
use crate::{
    call_gate::CallGate,
    vm::{NativeFn, VmSpec},
};

/// The most tool calls one Code run may make (100); a call past it throws in
/// the program, telling it to return what it has.
pub const MAX_CALLS: usize = 100;
/// Pictures one run may put in front of the model with `see()` (Claude Code's REPL: 4 a result).
pub const MAX_SEEN: usize = 4;
const READ_CONCURRENCY: usize = 8;

#[derive(Default)]
pub(crate) struct Tally {
    pub calls: Vec<CallRecord>,
    pub cost_usd: f64,
    pub child_cost_usd: f64,
    pub seen: Vec<MediaPart>,
    /// Pictures promised to a see() still reading them: counted against the
    /// cap with `seen`, so two calls at once cannot pass it together.
    pub held: usize,
    pub notes: Vec<String>,
}

/// A see()'s claim on its pictures; what is not used goes back when it ends,
/// however it ends (an error, or the run dropping the call).
struct Held<'a> {
    tally: &'a Mutex<Tally>,
    left: usize,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.tally.lock().unwrap().held -= self.left;
    }
}

pub(crate) struct Bridge {
    tools: ToolIndex,
    names: Vec<String>,
    schemas: serde_json::Map<String, Value>,
    /// The context every inner call runs in: the Code call's cwd, workspace and
    /// spill directory, cancelled when the run ends.
    inner: ToolContext,
    parent_id: String,
    /// Inner calls are reported only inside the loop, where there is a call to hang them under.
    report: Option<ToolServicesRef>,
    gate: CallGate,
    pub tally: Mutex<Tally>,
}

#[derive(Deserialize, Default)]
struct SeeInput {
    text: Option<String>,
    #[serde(default)]
    images: Vec<SeeImage>,
}

#[derive(Deserialize)]
struct SeeImage {
    path: Option<String>,
    label: Option<String>,
}

impl Bridge {
    pub fn new(callable: Tools, ctx: &ToolContext, cancel: CancellationToken) -> Arc<Self> {
        let schemas = callable
            .iter()
            .map(|t| (t.name().to_string(), t.parameters()))
            .collect();
        let tools = ToolIndex::new(callable);
        let names = tools.names().into_iter().map(String::from).collect();
        // A program's inner calls have no stable ids: no effect keys for them
        // (no run key). A tool Code calls winds down with the turn that called Code.
        let mut inner = ToolContext::new(ctx.cwd(), ctx.workspace().clone())
            .with_cancel(cancel)
            .with_wind_down(ctx.wind_down().clone());
        if let Some(dir) = ctx.spill_dir() {
            inner = inner.with_spill_dir(dir);
        }
        if let Some(left) = ctx.turn_time_left() {
            inner = inner.with_turn_time_left(left);
        }
        Arc::new(Self {
            tools,
            names,
            schemas,
            inner,
            parent_id: ctx
                .tool_call_id()
                .map(String::from)
                .unwrap_or_else(|| "code".into()),
            report: ctx.tool_call_id().is_some().then(|| ctx.services().clone()),
            gate: CallGate::new(READ_CONCURRENCY),
            tally: Mutex::default(),
        })
    }

    fn refuse(&self, name: &str) -> String {
        format!(
            "\"{name}\" cannot be called from Code. Callable: {}. Call it as a normal tool instead.",
            self.names.join(", ")
        )
    }

    /// The tool and this call's number, or why it may not run (a name outside the policy, the cap).
    fn admit(&self, name: &str) -> Result<(ToolRef, usize), String> {
        let tool = self
            .tools
            .get(name)
            .cloned()
            .ok_or_else(|| self.refuse(name))?;
        let mut tally = self.tally.lock().unwrap();
        if tally.calls.len() >= MAX_CALLS {
            return Err(format!(
                "Code may make at most {MAX_CALLS} tool calls per run. Return what you have, or split the work."
            ));
        }
        tally.calls.push(CallRecord {
            name: name.to_string(),
            failed: true,
        });
        Ok((tool, tally.calls.len()))
    }

    /// Run one inner call: gated, capped, charged, reported. Err only for a call that never ran.
    async fn invoke(&self, name: &str, input: JsonMap) -> Result<ToolOutput, String> {
        let (tool, n) = self.admit(name)?;
        let call = ToolCall {
            id: format!("{}.{n}", self.parent_id),
            name: name.to_string(),
            input,
        };
        let row = self
            .report
            .as_ref()
            .map(|services| InnerRow::start(services.clone(), &self.parent_id, &call));
        let ctx = self.inner.clone().with_tool_call_id(call.id.clone());
        let out = self
            .gate
            .run(!tool.read_only(), run_unspilled(&self.tools, &call, &ctx))
            .await;
        if let Some(row) = row {
            row.end(&out);
        }
        let mut tally = self.tally.lock().unwrap();
        tally.cost_usd += out.cost_usd.unwrap_or(0.0);
        tally.child_cost_usd += out.child_cost_usd.unwrap_or(0.0);
        tally.calls[n - 1].failed = out.is_error;
        Ok(out)
    }

    async fn call(&self, name: &str, args_json: &str) -> Result<String, String> {
        let input = match serde_json::from_str::<Value>(args_json) {
            Ok(Value::Object(m)) => m,
            _ => return Err(format!("tools.{name}() takes one object: its input")),
        };
        let out = self.invoke(name, input).await?;
        if out.is_error {
            return Err(out.content);
        }
        let dropped = if out.media.is_empty() {
            String::new()
        } else {
            format!(
                "\n[{} image/PDF part(s) not shown to the model -- pass the path to see() to look at it]",
                out.media.len()
            )
        };
        Ok(out.content + &dropped)
    }

    /// Each image is a gated Read; its media rides on the Code result, up to MAX_SEEN a run.
    async fn see(&self, args_json: &str) -> Result<String, String> {
        let input: SeeInput = serde_json::from_str(args_json).map_err(|e| format!("see(): {e}"))?;
        // Claimed before the first Read is awaited.
        let mut held = {
            let mut tally = self.tally.lock().unwrap();
            let already = tally.seen.len() + tally.held;
            if already + input.images.len() > MAX_SEEN {
                return Err(format!(
                    "see() shows at most {MAX_SEEN} pictures per run; {already} already attached or on the way."
                ));
            }
            tally.held += input.images.len();
            Held {
                tally: &self.tally,
                left: input.images.len(),
            }
        };
        for img in &input.images {
            let path = img
                .path
                .as_deref()
                .ok_or_else(|| "see(): every image needs a path".to_string())?;
            let mut read = JsonMap::new();
            read.insert("file_path".into(), json!(path));
            let out = self.invoke("Read", read).await?;
            if out.is_error {
                return Err(out.content);
            }
            if out.media.is_empty() {
                return Err(format!("see(): {path} is not an image or PDF"));
            }
            let mut tally = self.tally.lock().unwrap();
            tally.held -= 1;
            held.left -= 1;
            tally.seen.extend(out.media);
            let label = format!(
                "[picture {}: {}]",
                tally.seen.len(),
                img.label.as_deref().unwrap_or(path)
            );
            tally.notes.push(label);
        }
        if let Some(text) = input.text.filter(|t| !t.is_empty()) {
            self.tally.lock().unwrap().notes.push(text);
        }
        Ok(format!(
            "{} picture(s) attached to this result",
            input.images.len()
        ))
    }

    /// The guest this bridge answers: its API, the schemas as data (describe() answers
    /// inside the engine, no round trip), and the two native functions.
    pub fn spec(self: &Arc<Self>) -> VmSpec {
        let mut values = serde_json::Map::new();
        values.insert("__schemas".into(), Value::Object(self.schemas.clone()));
        values.insert("__refuse".into(), json!(self.refuse("NAME")));
        let me = self.clone();
        let call: NativeFn = Arc::new(move |args| {
            let me = me.clone();
            async move { Ok(me.call(arg(&args, 0), arg(&args, 1)).await?) }.boxed()
        });
        let me = self.clone();
        let see: NativeFn = Arc::new(move |args| {
            let me = me.clone();
            async move { Ok(me.see(arg(&args, 0)).await?) }.boxed()
        });
        VmSpec {
            prelude: GUEST_API.into(),
            values,
            native_fns: [("__call".to_string(), call), ("__see".to_string(), see)].into(),
            ..Default::default()
        }
    }
}

fn arg(args: &[String], i: usize) -> &str {
    args.get(i).map_or("", String::as_str)
}

/// The guest side of the bridge: `tools.X()`, `describe()`, `see()`, `TOOL_NAMES`.
const GUEST_API: &str = r#"{
  globalThis.tools = new Proxy({}, { get: (_, n) => typeof n === 'string' && n !== 'then' ? args => __call(n, JSON.stringify(args ?? {})) : undefined });
  globalThis.describe = n => { const s = __schemas[n]; if (!s) throw new Error(__refuse.replace('NAME', String(n))); return s; };
  globalThis.see = o => __see(JSON.stringify(o ?? {}));
  globalThis.TOOL_NAMES = Object.freeze(Object.keys(__schemas));
}"#;
