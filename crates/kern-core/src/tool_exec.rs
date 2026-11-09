//! Running one tool call, and the synthetic assistant message for a model call
//! that never happened. Both turn every failure into a value the loop can put
//! in history -- nothing here fails.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

use crate::{
    fs::ensure_dir,
    messages::{AssistantMessage, StopReason, ToolCall, ToolResultMessage, Usage},
    tool::{Tool, ToolContext, ToolError, ToolOutput},
};

/// A model call that never went out (no provider, a refused request).
pub fn error_assistant(model: &str, error: &str) -> AssistantMessage {
    let mut m = AssistantMessage::start("none", model);
    m.stop_reason = StopReason::Error;
    m.error_message = Some(error.to_string());
    m
}

/// The tools of one model call, looked up by name. A name listed twice: the later one wins.
pub struct ToolIndex(Vec<Arc<dyn Tool>>);

impl ToolIndex {
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        Self(tools)
    }

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

/// The most any one tool result may put into the model's context (~5k tokens), in characters.
pub const MAX_TOOL_OUTPUT: usize = 20_000;
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
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // The call id keeps two calls spilling in the same millisecond (a parallel batch) apart.
    let id = ctx
        .tool_call_id
        .as_deref()
        .map(|id| format!("-{}", file_safe(id)))
        .unwrap_or_default();
    let path = dir.join(format!("{ms}-{}{id}.txt", file_safe(tool_name)));
    // Through the workspace, so the Read / Grep the notice points at can see it.
    let fs = ctx.workspace.fs();
    let written = match ensure_dir(fs.as_ref(), dir).await {
        Ok(()) => fs.write(&path, content.as_bytes()).await,
        Err(e) => Err(e),
    };
    match written {
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

/// One call of a turn: run it, or answer it as interrupted, so every call keeps its result.
pub async fn run_call(tools: &ToolIndex, call: &ToolCall, ctx: &ToolContext) -> ToolOutput {
    if ctx.cancel.is_cancelled() {
        return ToolOutput::error("Interrupted by the user before this tool ran.");
    }
    execute_tool(tools, call, ctx).await
}

/// Fold a tool's own spend -- an image model's bill, a sub-agent's -- into the
/// turn, so the turn total and the USD cap both see it.
pub fn fold_spend(usage: &mut Usage, out: &ToolOutput) {
    let spent = out.cost_usd.unwrap_or(0.0) + out.child_cost_usd.unwrap_or(0.0);
    if spent != 0.0 {
        usage.cost_usd = Some(usage.cost_usd.unwrap_or(0.0) + spent);
    }
}

/// The history entry for one finished call. Media rides along only when there is some.
pub fn tool_result_message(call: &ToolCall, out: &ToolOutput) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content: out.content.clone(),
        is_error: out.is_error,
        media: out.media.clone(),
    }
}

/// One call as the loop runs it: [`run_tool`], then a result too big for the
/// context is spilled to a file.
pub async fn execute_tool(tools: &ToolIndex, call: &ToolCall, ctx: &ToolContext) -> ToolOutput {
    let out = run_tool(tools, call, ctx).await;
    spill_if_huge(out, &call.name, ctx).await
}

/// One call with every error answered as a result, and NO spill: the output
/// whole. For a caller whose results never enter the context -- the Code
/// tool's program filters a big result itself, which is the reason it exists.
pub async fn run_tool(tools: &ToolIndex, call: &ToolCall, ctx: &ToolContext) -> ToolOutput {
    let Some(tool) = tools.get(&call.name) else {
        return ToolOutput::error(format!(
            "Unknown tool \"{}\". Available: {}",
            call.name,
            tools.names().join(", ")
        ));
    };
    // A provider that could not parse the model's arguments hands them over raw.
    if let Some(raw) = call.input.get("_unparsed") {
        let raw = raw.as_str().map_or_else(|| raw.to_string(), str::to_string);
        return ToolOutput::error(format!(
            "Your arguments for {} were not valid JSON: {raw}",
            call.name
        ));
    }
    match tool.execute(call.input.clone(), ctx).await {
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
