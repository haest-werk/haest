//! haest's tool calls, results, task list and stop reasons in the
//! protocol's words. Pure: the live stream ([`crate::forward`]) and a loaded
//! session's replay ([`crate::replay`]) both say a call this way.

use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{
    Diff, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, StopReason as AcpStop,
    ToolCall as AcpCall, ToolCallContent, ToolCallLocation, ToolCallStatus, ToolCallUpdateFields,
    ToolKind,
};
use haest_core::{
    JsonMap, StopReason, ToolCall,
    remedy::Limit,
    task_board::{Task, TaskStatus},
};
use serde_json::Value;

/// The protocol's kind for a tool of haest's `kind` ([`Tool::kind`]): how a
/// client draws the call.
///
/// [`Tool::kind`]: haest_core::Tool::kind
pub(crate) fn tool_kind(kind: &str) -> ToolKind {
    match kind {
        "file.read" => ToolKind::Read,
        "file.write" | "file.edit" => ToolKind::Edit,
        "file.glob" | "text.search" => ToolKind::Search,
        "shell.exec" | "repl.exec" => ToolKind::Execute,
        "web.fetch" => ToolKind::Fetch,
        "task.spawn" | "todo.write" => ToolKind::Think,
        _ => ToolKind::Other,
    }
}

/// A string input, when it is there and not empty.
fn arg<'a>(input: &'a JsonMap, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// What the call does, in a line: the command a shell runs, the file a file
/// tool touches, the pattern a search looks for; else the tool's name.
pub(crate) fn title(name: &str, kind: &str, input: &JsonMap) -> String {
    let shown = match kind {
        "file.read" => arg(input, "file_path").map(|p| format!("Read {p}")),
        "file.write" => arg(input, "file_path").map(|p| format!("Write {p}")),
        "file.edit" => arg(input, "file_path").map(|p| format!("Edit {p}")),
        "file.glob" => arg(input, "pattern").map(|p| format!("Find {p}")),
        "text.search" => arg(input, "pattern").map(|p| format!("Search for {p}")),
        "shell.exec" => arg(input, "command").map(str::to_string),
        "web.fetch" => arg(input, "url").map(|u| format!("Fetch {u}")),
        "task.spawn" => arg(input, "description").map(str::to_string),
        _ => None,
    };
    shown.unwrap_or_else(|| name.to_string())
}

/// `path` against `cwd`: the protocol's paths are absolute.
fn absolute(cwd: &Path, path: &str) -> PathBuf {
    cwd.join(path)
}

/// The files a call touches, for a client that follows along: a file tool's
/// file (at the line a read starts from), a search's directory.
pub(crate) fn locations(kind: &str, input: &JsonMap, cwd: &Path) -> Vec<ToolCallLocation> {
    match kind {
        "file.read" | "file.write" | "file.edit" => {
            let Some(path) = arg(input, "file_path") else {
                return Vec::new();
            };
            let line = (kind == "file.read")
                .then(|| input.get("offset").and_then(Value::as_u64))
                .flatten()
                .and_then(|n| u32::try_from(n).ok());
            let at = ToolCallLocation::new(absolute(cwd, path));
            vec![match line {
                Some(line) => at.line(line),
                None => at,
            }]
        }
        "file.glob" | "text.search" => arg(input, "path")
            .map(|p| vec![ToolCallLocation::new(absolute(cwd, p))])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// What an edit or a write changes, as the protocol's diff: an edit's old
/// and new text (the part it replaces, not the whole file), a write's
/// content. Nothing for any other call.
fn diff(kind: &str, input: &JsonMap, cwd: &Path) -> Option<ToolCallContent> {
    let path = absolute(cwd, arg(input, "file_path")?);
    let text = |key| input.get(key).and_then(Value::as_str).unwrap_or_default();
    match kind {
        "file.edit" => Some(
            Diff::new(path, text("new_string"))
                .old_text(text("old_string"))
                .into(),
        ),
        "file.write" => Some(Diff::new(path, text("content")).into()),
        _ => None,
    }
}

/// Everything the model's call says: its title, kind, input, files and, for
/// an edit, the diff. Its status is the caller's to set.
pub(crate) fn call_fields(call: &ToolCall, kind: &str, cwd: &Path) -> ToolCallUpdateFields {
    let fields = ToolCallUpdateFields::new()
        .title(title(&call.name, kind, &call.input))
        .kind(tool_kind(kind))
        .raw_input(Value::Object(call.input.clone()))
        .locations(locations(kind, &call.input, cwd));
    match diff(kind, &call.input, cwd) {
        Some(diff) => fields.content(vec![diff]),
        None => fields,
    }
}

/// The call as a new entry: [`call_fields`] with `status`.
pub(crate) fn new_call(call: &ToolCall, kind: &str, cwd: &Path, status: ToolCallStatus) -> AcpCall {
    let mut entry = AcpCall::new(call.id.clone(), call.name.clone());
    entry.update(call_fields(call, kind, cwd).status(status));
    entry
}

/// How a call ended. `text` is what the model read; it is the call's
/// content, except for an edit that went through, whose diff stays.
/// `output` is the tool's structured result, `title` the one it gave itself.
pub(crate) fn result_fields(
    kind: &str,
    is_error: bool,
    text: &str,
    output: Option<&Value>,
    title: Option<&str>,
) -> ToolCallUpdateFields {
    let status = if is_error {
        ToolCallStatus::Failed
    } else {
        ToolCallStatus::Completed
    };
    let mut fields = ToolCallUpdateFields::new().status(status);
    let keeps_diff = !is_error && matches!(kind, "file.edit" | "file.write");
    if !keeps_diff && !text.is_empty() {
        fields = fields.content(vec![ToolCallContent::from(text.to_string())]);
    }
    if let Some(output) = output {
        fields = fields.raw_output(output.clone());
    }
    if let Some(title) = title {
        fields = fields.title(title.to_string());
    }
    fields
}

/// The task list as the protocol's plan: every task, in order, with its status.
pub(crate) fn plan(tasks: &[Task]) -> Plan {
    let entries = tasks
        .iter()
        .map(|t| {
            let status = match t.status {
                TaskStatus::InProgress => PlanEntryStatus::InProgress,
                TaskStatus::Completed => PlanEntryStatus::Completed,
                // Pending, and a status newer than this crate: not started.
                _ => PlanEntryStatus::Pending,
            };
            PlanEntry::new(t.subject.clone(), PlanEntryPriority::Medium, status)
        })
        .collect();
    Plan::new(entries)
}

/// How a turn that did not fail ended, in the protocol's words; None for
/// [`StopReason::Error`], which the prompt's answer reports as an error.
pub(crate) fn stop_reason(reason: StopReason, limit: Option<&Limit>) -> Option<AcpStop> {
    Some(match reason {
        StopReason::Error => return None,
        StopReason::MaxTokens if matches!(limit, Some(Limit::TurnSteps(_))) => {
            AcpStop::MaxTurnRequests
        }
        StopReason::MaxTokens => AcpStop::MaxTokens,
        StopReason::Refusal => AcpStop::Refusal,
        StopReason::Aborted => AcpStop::Cancelled,
        // EndTurn, ToolUse (a turn never ends on it) and a reason newer than
        // this crate: the turn is over.
        _ => AcpStop::EndTurn,
    })
}

#[cfg(test)]
mod tests;
