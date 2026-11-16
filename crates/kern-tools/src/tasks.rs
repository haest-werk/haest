//! TaskCreate / TaskUpdate / TaskList / TaskGet over the conversation's
//! [`TaskBoard`]. Names and keys are Claude Code's (`subject`, `activeForm`,
//! `taskId`, `addBlockedBy`) on purpose: every frontier model has seen them.

use std::sync::Arc;

use async_trait::async_trait;
use kern_core::{
    task_board::{KernTask, KernTaskStatus, NewTask, PatchStatus, TaskBoard, TaskError, TaskPatch},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, opt_str, req_str},
};
use serde_json::{Value, json};

use crate::canonical::spelled;

const STATUSES: [&str; 4] = ["pending", "in_progress", "completed", "deleted"];

fn line(t: &KernTask) -> String {
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
    Ok(Some(match s {
        "pending" => PatchStatus::Set(KernTaskStatus::Pending),
        "in_progress" => PatchStatus::Set(KernTaskStatus::InProgress),
        "completed" => PatchStatus::Set(KernTaskStatus::Completed),
        "deleted" => PatchStatus::Deleted,
        _ => {
            return Err(ToolError::Input(format!(
                "\"status\" must be one of {}",
                STATUSES.join(", ")
            )));
        }
    }))
}

/// A TaskError is the model's to fix (an unknown id): it goes back as a correctable input error.
fn on_board(e: TaskError) -> ToolError {
    ToolError::Input(e.0)
}

fn id_param() -> Value {
    json!({"type": "string", "description": "The task id, e.g. \"3\""})
}

struct TaskCreate(Arc<TaskBoard>);
struct TaskUpdate(Arc<TaskBoard>);
struct TaskList(Arc<TaskBoard>);
struct TaskGet(Arc<TaskBoard>);

/// The four task tools over one board.
pub fn create_task_tools(board: Arc<TaskBoard>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(TaskCreate(board.clone())),
        Arc::new(TaskUpdate(board.clone())),
        Arc::new(TaskList(board.clone())),
        Arc::new(TaskGet(board)),
    ]
}

#[async_trait]
impl Tool for TaskCreate {
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
impl Tool for TaskUpdate {
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
                "status": {"type": "string", "enum": STATUSES},
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
impl Tool for TaskList {
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
impl Tool for TaskGet {
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
