//! The conversation's task list: what the model plans and ticks off with the Task
//! tools (`haest-tools` `tasks`). Same shape as Claude Code's tasks -- id, subject,
//! status, blocked-by edges -- so a host renders haest's list with the renderer it
//! already has, and every frontier model already knows the moves.
//!
//! Pure state: `on_change` hands every new snapshot to whoever persists and
//! publishes it (the host). Ids are short counters ("1", "2", ...), stable for the
//! session; a restored snapshot continues the count.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::deps::now_ms;

crate::word_enum! {
    /// `as_str` is the wire name (`in_progress`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum HaestTaskStatus ("task status") {
        Pending = "pending",
        InProgress = "in_progress",
        Completed = "completed",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HaestTask {
    pub id: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Present-continuous label shown while in_progress ("Running tests").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    pub status: HaestTaskStatus,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default)]
    pub blocks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
}

/// What an update may set: a status, or removing the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchStatus {
    Set(HaestTaskStatus),
    Deleted,
}

/// Only the fields that are `Some` change; an absent field leaves the task's value alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskPatch {
    pub subject: Option<String>,
    pub description: Option<String>,
    pub active_form: Option<String>,
    pub status: Option<PatchStatus>,
    pub owner: Option<String>,
    pub add_blocked_by: Vec<String>,
    pub add_blocks: Vec<String>,
}

/// A refusal the model can correct (an unknown id, a self-dependency).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct TaskError(pub String);

/// The new subject of a task.
#[derive(Debug, Clone, Default)]
pub struct NewTask {
    pub subject: String,
    pub description: Option<String>,
    pub active_form: Option<String>,
}

type OnChange = Box<dyn Fn(&[HaestTask]) + Send + Sync>;
type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

struct State {
    /// Insertion order is list order.
    tasks: Vec<HaestTask>,
    next: u64,
}

impl State {
    fn find(&self, id: &str) -> Result<usize, TaskError> {
        self.tasks.iter().position(|t| t.id == id).ok_or_else(|| {
            let ids: Vec<&str> = self.tasks.iter().map(|t| t.id.as_str()).collect();
            let existing = if ids.is_empty() {
                "none".to_string()
            } else {
                ids.join(", ")
            };
            TaskError(format!("no task \"{id}\"; existing ids: {existing}"))
        })
    }

    /// `blocker` must finish before `blocked`. Kept on both ends, like Claude Code.
    fn link(&mut self, blocker: &str, blocked: &str) -> Result<(), TaskError> {
        let a = self.find(blocker)?;
        let b = self.find(blocked)?;
        if !self.tasks[a].blocks.iter().any(|x| x == blocked) {
            self.tasks[a].blocks.push(blocked.to_string());
        }
        if !self.tasks[b].blocked_by.iter().any(|x| x == blocker) {
            self.tasks[b].blocked_by.push(blocker.to_string());
        }
        Ok(())
    }

    fn remove(&mut self, id: &str) {
        self.tasks.retain(|t| t.id != id);
        for t in &mut self.tasks {
            t.blocks.retain(|x| x != id);
            t.blocked_by.retain(|x| x != id);
        }
    }
}

pub struct TaskBoard {
    state: Mutex<State>,
    on_change: OnChange,
    now: Clock,
}

impl Default for TaskBoard {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

impl TaskBoard {
    /// A board that hands every new snapshot to `on_change`; the clock is the system's.
    pub fn new(on_change: impl Fn(&[HaestTask]) + Send + Sync + 'static) -> Self {
        Self::with_clock(on_change, now_ms)
    }

    /// Same, with the clock `updated_at` reads (a host's paused clock, a test's fixed one).
    pub fn with_clock(
        on_change: impl Fn(&[HaestTask]) + Send + Sync + 'static,
        now: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: Mutex::new(State {
                tasks: Vec::new(),
                next: 1,
            }),
            on_change: Box::new(on_change),
            now: Box::new(now),
        }
    }

    pub fn list(&self) -> Vec<HaestTask> {
        self.state.lock().unwrap().tasks.clone()
    }

    pub fn get(&self, id: &str) -> Result<HaestTask, TaskError> {
        let s = self.state.lock().unwrap();
        s.find(id).map(|i| s.tasks[i].clone())
    }

    pub fn create(&self, input: NewTask) -> HaestTask {
        let task = {
            let mut s = self.state.lock().unwrap();
            let task = HaestTask {
                id: s.next.to_string(),
                subject: input.subject,
                description: input.description,
                active_form: input.active_form,
                status: HaestTaskStatus::Pending,
                blocked_by: Vec::new(),
                blocks: Vec::new(),
                owner: None,
                updated_at: (self.now)(),
                completed_at: None,
            };
            s.next += 1;
            s.tasks.push(task.clone());
            task
        };
        self.changed();
        task
    }

    /// The updated task, or None when the patch deleted it.
    pub fn update(&self, id: &str, patch: TaskPatch) -> Result<Option<HaestTask>, TaskError> {
        let out = {
            let mut s = self.state.lock().unwrap();
            let at = s.find(id)?;
            // Every refusal comes before the first change: an error after it would
            // leave an unsaved, unpublished edit.
            for other in patch.add_blocked_by.iter().chain(&patch.add_blocks) {
                s.find(other)?;
                if other == id {
                    return Err(TaskError(format!("task {id} cannot block itself")));
                }
            }
            if patch.status == Some(PatchStatus::Deleted) {
                s.remove(id);
                None
            } else {
                let now = (self.now)();
                set_fields(&mut s.tasks[at], &patch, now);
                for b in &patch.add_blocked_by {
                    s.link(b, id)?;
                }
                for b in &patch.add_blocks {
                    s.link(id, b)?;
                }
                Some(s.tasks[at].clone())
            }
        };
        self.changed();
        Ok(out)
    }

    /// Replace the whole list (session replay); does not fire `on_change`.
    pub fn restore(&self, tasks: Vec<HaestTask>) {
        let mut s = self.state.lock().unwrap();
        let max = tasks
            .iter()
            .map(|t| t.id.parse::<u64>().unwrap_or(0))
            .max()
            .unwrap_or(0);
        s.next = max + 1;
        s.tasks = tasks;
    }

    /// The snapshot goes out with the lock released, so `on_change` may read the board.
    fn changed(&self) {
        let snapshot = self.list();
        (self.on_change)(&snapshot);
    }
}

fn set_fields(task: &mut HaestTask, patch: &TaskPatch, now: u64) {
    let set = |field: &mut Option<String>, v: &Option<String>| {
        if let Some(v) = v {
            *field = Some(v.clone());
        }
    };
    if let Some(s) = &patch.subject {
        task.subject = s.clone();
    }
    set(&mut task.description, &patch.description);
    set(&mut task.active_form, &patch.active_form);
    set(&mut task.owner, &patch.owner);
    if let Some(PatchStatus::Set(status)) = patch.status {
        task.status = status;
    }
    task.updated_at = now;
    if patch.status == Some(PatchStatus::Set(HaestTaskStatus::Completed)) {
        task.completed_at = Some(now);
    }
}

#[cfg(test)]
mod tests;
