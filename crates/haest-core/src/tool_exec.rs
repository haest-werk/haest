//! Running one tool call, and the synthetic assistant message for a model call
//! that never happened. Both turn every failure into a value the loop can put
//! in history -- nothing here fails.
//!
//! Two ways to run a call:
//! - [`run_call`]: what the loop uses. A cancelled turn answers the call as
//!   interrupted without running it; a result over
//!   [`MAX_TOOL_OUTPUT`](crate::tool::MAX_TOOL_OUTPUT) characters is spilled
//!   to a file and the model gets a preview.
//! - [`run_unspilled`]: runs it with every error and panic answered as a
//!   result, and no spill. For a caller whose results never reach the model
//!   as they are (the Code tool).
//!
//! Tools are looked up by name in a [`ToolIndex`]. Other haest crates reach
//! these through [`__private`](crate::__private); a host never needs them.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::{
    deps::now_ms,
    fs::ensure_dir,
    messages::{AssistantMessage, StopReason, ToolCall, ToolResultMessage, Usage},
    panics::panic_message,
    tool::{MAX_TOOL_OUTPUT, Tool, ToolContext, ToolError, ToolOutput, spelled},
};

/// A model call that never went out (no provider, a refused request): an
/// empty answer from provider `"none"` with [`StopReason::Error`] and `error`.
pub(crate) fn error_assistant(model: &str, error: &str) -> AssistantMessage {
    let mut m = AssistantMessage::start("none", model);
    m.stop_reason = StopReason::Error;
    m.error_message = Some(error.to_string());
    m
}

/// The tools of one model call, looked up by name. A name listed twice: the later one wins.
pub struct ToolIndex(Vec<Arc<dyn Tool>>);

impl ToolIndex {
    /// An index over `tools`, in the order given.
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        Self(tools)
    }

    /// The tool called `name`; the later one when two share it.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.0.iter().rev().find(|t| t.name() == name)
    }

    /// Every name, first-listed order, once each.
    pub fn names(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for t in &self.0 {
            if !out.contains(&t.name()) {
                out.push(t.name());
            }
        }
        out
    }
}

const PREVIEW_CHARS: usize = 2_000;

fn file_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Where the full output went, as the notice tells the model.
async fn spill(content: &str, tool_name: &str, ctx: &ToolContext) -> String {
    let Some(dir) = &ctx.spill_dir else {
        return "it was NOT saved (no spill directory) -- call the tool again with a narrower request".into();
    };
    let ms = now_ms();
    // The call id keeps two calls spilling in the same millisecond (a parallel batch) apart.
    let id = ctx
        .tool_call_id
        .as_deref()
        .map(|id| format!("-{}", file_safe(id)))
        .unwrap_or_default();
    let path = dir.join(format!("{ms}-{}{id}.txt", file_safe(tool_name)));
    // Through the workspace, so the Read / Grep the notice points at can see it.
    let fs = ctx.workspace.fs();
    let written = async {
        ensure_dir(fs.as_ref(), dir).await?;
        fs.write(&path, content.as_bytes()).await
    };
    match written.await {
        Ok(()) => format!(
            "the full output is in {} -- read it in pieces with Read (offset/limit) or search it with Grep",
            path.display()
        ),
        // The tool DID run: its result must not turn into a failure the model may retry.
        Err(e) => {
            format!("it could NOT be saved ({e}) -- call the tool again with a narrower request")
        }
    }
}

/// A result too big for the context goes to a FILE; the model gets its size, the
/// path and how to read it in pieces. One tool call once put ~600k tokens into a
/// conversation -- no tool, MCP or built-in, may reach the model unbounded. A tool
/// that sizes its own output (`sized`, i.e. Read) is exempt: spilling a Read would
/// answer it with "go Read a copy of this".
async fn spill_if_huge(out: ToolOutput, tool_name: &str, ctx: &ToolContext) -> ToolOutput {
    let len = out.content.chars().count();
    if out.sized || len <= MAX_TOOL_OUTPUT {
        return out;
    }
    let preview: String = out.content.chars().take(PREVIEW_CHARS).collect();
    let size = format!(
        "{len} characters (~{} tokens)",
        (len as f64 / 4.0).round() as u64
    );
    let where_ = spill(&out.content, tool_name, ctx).await;
    let content = format!(
        "[{tool_name} returned {size}: too large for the context, so {where_}. Prefer narrowing the request next time.]\n\nFirst {PREVIEW_CHARS} characters:\n{preview}"
    );
    ToolOutput {
        result: Some(json!({"kind": "text", "text": content})),
        content,
        ..out
    }
}

/// One call of a turn, as the loop runs it: answered as interrupted when the
/// turn was cancelled, else [`run_unspilled`], then a result too big for the
/// context is spilled to a file. Every call keeps its result.
pub async fn run_call(tools: &ToolIndex, call: &ToolCall, ctx: &ToolContext) -> ToolOutput {
    if ctx.cancel.is_cancelled() {
        return ToolOutput::error("Interrupted by the user before this tool ran.");
    }
    let out = run_unspilled(tools, call, ctx).await;
    spill_if_huge(out, &call.name, ctx).await
}

/// Fold a tool's own spend -- an image model's bill, a subagent's -- into the
/// turn, so the turn total and the USD cap both see it.
///
/// Adds [`ToolOutput::cost_usd`] and [`ToolOutput::child_cost_usd`] to
/// `usage.cost_usd`; leaves `usage` untouched when the tool spent nothing.
pub(crate) fn fold_spend(usage: &mut Usage, out: &ToolOutput) {
    let spent = out.cost_usd.unwrap_or(0.0) + out.child_cost_usd.unwrap_or(0.0);
    if spent != 0.0 {
        usage.cost_usd = Some(usage.cost_usd.unwrap_or(0.0) + spent);
    }
}

/// The history entry for one finished call. Media rides along only when there is some.
pub(crate) fn tool_result_message(call: &ToolCall, out: &ToolOutput) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content: out.content.clone(),
        is_error: out.is_error,
        media: out.media.clone(),
    }
}

/// One call with every error answered as a result, and NO spill: the output
/// whole. For a caller whose results never enter the context -- the Code
/// tool's program filters a big result itself, which is the reason it exists.
///
/// An unknown name is answered with an error listing the tools there are;
/// arguments that were not JSON (`_unparsed`) with an error, without running
/// the tool. A [`ToolError`] or a panic becomes an error result with a
/// `{"kind": "error"}` envelope.
///
/// ```
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// use std::sync::Arc;
///
/// use haest_core::{
///     ShutWorkspace, ToolCall, ToolContext,
///     __private::{ToolIndex, run_unspilled},
/// };
///
/// let call = ToolCall { id: "c1".into(), name: "Nope".into(), input: Default::default() };
/// let ctx = ToolContext::new("/", Arc::new(ShutWorkspace));
/// let out = run_unspilled(&ToolIndex::new(Vec::new()), &call, &ctx).await;
/// assert!(out.is_error); // an answer the model reads, not a failure of the loop
/// # }
/// ```
pub async fn run_unspilled(tools: &ToolIndex, call: &ToolCall, ctx: &ToolContext) -> ToolOutput {
    let Some(tool) = tools.get(&call.name) else {
        return ToolOutput::error(format!(
            "Unknown tool \"{}\". Available: {}",
            call.name,
            tools.names().join(", ")
        ));
    };
    // A provider that could not parse the model's arguments hands them over raw.
    if let Some(raw) = call.input.get("_unparsed") {
        let raw = spelled(raw);
        return ToolOutput::error(format!(
            "Your arguments for {} were not valid JSON: {raw}",
            call.name
        ));
    }
    // A tool that panics (its own bug) is a failed call like any other: the
    // loop, and whatever host drives it, goes on with a result in history.
    let run = std::panic::AssertUnwindSafe(tool.execute(call.input.clone(), ctx));
    let done = match futures::FutureExt::catch_unwind(run).await {
        Ok(done) => done,
        Err(panic) => Err(ToolError::Failed(format!(
            "panicked: {}",
            panic_message(panic.as_ref()).unwrap_or("no message")
        ))),
    };
    match done {
        Ok(out) => out,
        Err(e) => {
            // An Input error is a message written FOR the model; anything else gets named.
            let message = e.to_string();
            let content = match e {
                ToolError::Input(_) => message.clone(),
                ToolError::Failed(_) => format!("{} failed: {message}", call.name),
            };
            ToolOutput::error(content).with_result(error_result(&message))
        }
    }
}

fn error_result(message: &str) -> Value {
    json!({"kind": "error", "message": message})
}

#[cfg(test)]
mod tests;
