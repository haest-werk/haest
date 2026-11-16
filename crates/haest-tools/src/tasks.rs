//! TaskCreate / TaskUpdate / TaskList / TaskGet over the conversation's
//! [`TaskBoard`] ([`task_tools`]). Names and keys are Claude Code's
//! (`subject`, `activeForm`, `taskId`, `addBlockedBy`) on purpose: every
//! frontier model has seen them.
//!
//! The board is the host's: it keeps it to show the list live and to save it
//! with the session. The task list belongs to the conversation, so the default
//! subagent hub gives no subagent these tools.

use std::sync::Arc;

use async_trait::async_trait;
use haest_core::{
    task_board::{NewTask, PatchStatus, Task, TaskBoard, TaskError, TaskPatch, TaskStatus},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_str, req_str, spelled},
};
use serde_json::{Value, json};

/// What TaskUpdate's `status` takes besides a [`TaskStatus`]: remove the task.
const DELETED: &str = "deleted";

/// Every word TaskUpdate's `status` takes: the statuses, then [`DELETED`].
fn status_words() -> Vec<&'static str> {
    TaskStatus::WORDS.iter().copied().chain([DELETED]).collect()
}

fn line(t: &Task) -> String {
    let blocked = if t.blocked_by.is_empty() {
        String::new()
    } else {
        let ids: Vec<String> = t.blocked_by.iter().map(|b| format!("#{b}")).collect();
        format!(" (blocked by {})", ids.join(", "))
    };
    format!("#{} [{}] {}{blocked}", t.id, t.status.as_str(), t.subject)
}

fn ids(input: &JsonMap, key: &str) -> Result<Vec<String>, ToolError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => Ok(a.iter().map(spelled).collect()),
        Some(_) => Err(ToolError::Input(format!(
            "\"{key}\" must be an array of task ids"
        ))),
    }
}

fn status(input: &JsonMap) -> Result<Option<PatchStatus>, ToolError> {
    let Some(s) = opt_str(input, "status")? else {
        return Ok(None);
    };
    if s == DELETED {
        return Ok(Some(PatchStatus::Deleted));
    }
    let status = s.parse::<TaskStatus>().map_err(|_| {
        ToolError::Input(format!(
            "\"status\" must be one of {}",
            status_words().join(", ")
        ))
    })?;
    Ok(Some(PatchStatus::Set(status)))
}

/// A TaskError is the model's to fix (an unknown id): it goes back as a correctable input error.
fn on_board(e: TaskError) -> ToolError {
    ToolError::Input(e.to_string())
}

fn id_param() -> Value {
    json!({"type": "string", "description": "The task id, e.g. \"3\""})
}

struct TaskCreateTool(Arc<TaskBoard>);
struct TaskUpdateTool(Arc<TaskBoard>);
struct TaskListTool(Arc<TaskBoard>);
struct TaskGetTool(Arc<TaskBoard>);

/// The four task tools over one board: TaskCreate, TaskUpdate (both kind
/// `todo.write`), TaskList and TaskGet (read-only).
///
/// An unknown task id, a bad status or a blocker that is not a task comes back
/// as an input error the model corrects.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use haest_core::{task_board::TaskBoard, tool::ToolContext, workspace::ShutWorkspace};
/// use haest_tools::task_tools;
/// use serde_json::json;
///
/// # #[tokio::main]
/// # async fn main() {
/// let board = Arc::new(TaskBoard::default());
/// let tools = task_tools(board.clone());
/// let ctx = ToolContext::new("/", Arc::new(ShutWorkspace));
/// let input = json!({"subject": "Write the docs"});
/// tools[0].execute(input.as_object().unwrap().clone(), &ctx).await.unwrap();
/// // The host sees the model's list on its own board.
/// assert_eq!(board.list()[0].subject, "Write the docs");
/// # }
/// ```
pub fn task_tools(board: Arc<TaskBoard>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(TaskCreateTool(board.clone())),
        Arc::new(TaskUpdateTool(board.clone())),
        Arc::new(TaskListTool(board.clone())),
        Arc::new(TaskGetTool(board)),
    ]
}

#[async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> &str {
        "TaskCreate"
    }
    fn kind(&self) -> &str {
        "todo.write"
    }
    fn description(&self) -> String {
        "Add a task to your task list. Use it for work with 3+ steps: one task per step, then mark each in_progress when you start it and completed when it is done. The human sees the list live.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subject": {"type": "string", "description": "Short imperative title"},
                "description": {"type": "string", "description": "What needs to be done"},
                "activeForm": {"type": "string", "description": "Present continuous label shown while in progress"}
            },
            "required": ["subject"]
        })
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let t = self.0.create(NewTask {
            subject: req_str(&input, "subject")?.into(),
            description: opt_str(&input, "description")?.map(Into::into),
            active_form: opt_str(&input, "activeForm")?.map(Into::into),
        });
        Ok(ToolOutput::text(format!(
            "Task #{} created: {}",
            t.id, t.subject
        )))
    }
}

#[async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> &str {
        "TaskUpdate"
    }
    fn kind(&self) -> &str {
        "todo.write"
    }
    fn description(&self) -> String {
        "Change a task: status (pending, in_progress, completed, or deleted to remove it), subject, description, owner, or dependencies (addBlockedBy / addBlocks take task ids).".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "taskId": id_param(),
                "status": {"type": "string", "enum": status_words()},
                "subject": {"type": "string"},
                "description": {"type": "string"},
                "activeForm": {"type": "string"},
                "owner": {"type": "string"},
                "addBlockedBy": {"type": "array", "items": {"type": "string"}, "description": "Ids that must finish first"},
                "addBlocks": {"type": "array", "items": {"type": "string"}, "description": "Ids that wait on this one"}
            },
            "required": ["taskId"]
        })
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = req_str(&input, "taskId")?;
        let patch = TaskPatch {
            status: status(&input)?,
            subject: opt_str(&input, "subject")?.map(Into::into),
            description: opt_str(&input, "description")?.map(Into::into),
            active_form: opt_str(&input, "activeForm")?.map(Into::into),
            owner: opt_str(&input, "owner")?.map(Into::into),
            add_blocked_by: ids(&input, "addBlockedBy")?,
            add_blocks: ids(&input, "addBlocks")?,
        };
        let content = match self.0.update(id, patch).map_err(on_board)? {
            Some(t) => format!("Updated {}", line(&t)),
            None => format!("Task #{id} deleted"),
        };
        Ok(ToolOutput::text(content))
    }
}

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "TaskList"
    }
    fn kind(&self) -> &str {
        "todo.write"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        "List every task with its id, status and blockers.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let all = self.0.list();
        if all.is_empty() {
            return Ok(ToolOutput::text("No tasks."));
        }
        let lines: Vec<String> = all.iter().map(line).collect();
        Ok(ToolOutput::text(lines.join("\n")))
    }
}

#[async_trait]
impl Tool for TaskGetTool {
    fn name(&self) -> &str {
        "TaskGet"
    }
    fn kind(&self) -> &str {
        "todo.write"
    }
    fn read_only(&self) -> bool {
        true
    }
    fn description(&self) -> String {
        "One task in full, including its description.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"taskId": id_param()}, "required": ["taskId"]})
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let t = self.0.get(req_str(&input, "taskId")?).map_err(on_board)?;
        let blocks = if t.blocks.is_empty() {
            String::new()
        } else {
            format!("blocks {}", t.blocks.join(", "))
        };
        let parts = [line(&t), t.description.clone().unwrap_or_default(), blocks];
        let content: Vec<String> = parts.into_iter().filter(|p| !p.is_empty()).collect();
        Ok(ToolOutput::text(content.join("\n")))
    }
}

#[cfg(test)]
mod tests;
