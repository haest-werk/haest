//! The conversation's task list: what the model plans and ticks off with the Task
//! tools (`haest-tools` `tasks`). Same shape as Claude Code's tasks -- id, subject,
//! status, blocked-by edges -- so a host renders haest's list with the renderer it
//! already has, and every frontier model already knows the moves.
//!
//! Pure state: `on_change` hands every new snapshot to whoever persists and
//! publishes it (the host). Ids are short counters ("1", "2", ...), stable for the
//! session; a restored snapshot continues the count.
//!
//! Start from [`TaskBoard`]. A host writes each snapshot as a
//! [`SessionRecord::Tasks`](crate::session::SessionRecord::Tasks) and hands the
//! replayed list back to [`TaskBoard::restore`] on resume.
//!
//! # Examples
//!
//! ```
//! use std::sync::{Arc, Mutex};
//!
//! use haest_core::task_board::{NewTask, PatchStatus, TaskBoard, TaskPatch, TaskStatus};
//!
//! let published = Arc::new(Mutex::new(0));
//! let seen = published.clone();
//! let board = TaskBoard::new(move |_tasks| *seen.lock().unwrap() += 1);
//!
//! let schema = board.create(NewTask { subject: "Write the schema".into(), ..Default::default() });
//! let api = board.create(NewTask { subject: "Write the API".into(), ..Default::default() });
//! board.update(&api.id, TaskPatch { add_blocked_by: vec![schema.id.clone()], ..Default::default() })?;
//! let done = TaskPatch { status: Some(PatchStatus::Set(TaskStatus::Completed)), ..Default::default() };
//! board.update(&schema.id, done)?;
//!
//! assert_eq!(board.get("2")?.blocked_by, ["1"]);
//! assert_eq!(board.get("1")?.status, TaskStatus::Completed);
//! assert_eq!(*published.lock().unwrap(), 4, "one snapshot per change");
//! # Ok::<(), haest_core::task_board::TaskError>(())
//! ```

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::deps::now_ms;

crate::word_enum! {
    /// Where a task stands. `as_str` is the wire name (`in_progress`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TaskStatus ("task status") {
        /// Not started; every new task begins here.
        Pending = "pending",
        /// Being worked on now.
        InProgress = "in_progress",
        /// Done.
        Completed = "completed",
    }
}

/// One task on the list, in Claude Code's shape (camelCase on the wire).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// The board's counter as text ("1", "2", ...).
    pub id: String,
    /// What to do, in a line.
    pub subject: String,
    /// The details, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Present-continuous label shown while in_progress ("Running tests").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    /// Where it stands.
    pub status: TaskStatus,
    /// The ids that must finish before this one.
    #[serde(default)]
    pub blocked_by: Vec<String>,
    /// The ids waiting on this one (the other end of `blocked_by`).
    #[serde(default)]
    pub blocks: Vec<String>,
    /// Who works on it, free text, if the model named anyone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// When it last changed, by the board's clock (ms since the epoch).
    pub updated_at: u64,
    /// When it was last marked completed; never cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
}

/// What an update may set: a status, or removing the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchStatus {
    /// Move the task to this status.
    Set(TaskStatus),
    /// Remove the task, and every link to it from the others.
    Deleted,
}

/// A change to one task ([`TaskBoard::update`]). Only the fields that are
/// `Some` change; an absent field leaves the task's value alone. The `add_*`
/// lists add links and never remove one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskPatch {
    /// A new subject.
    pub subject: Option<String>,
    /// A new description.
    pub description: Option<String>,
    /// A new present-continuous label.
    pub active_form: Option<String>,
    /// A new status, or [`PatchStatus::Deleted`].
    pub status: Option<PatchStatus>,
    /// A new owner.
    pub owner: Option<String>,
    /// Ids that must finish before this task (both ends get the link).
    pub add_blocked_by: Vec<String>,
    /// Ids that wait on this task (both ends get the link).
    pub add_blocks: Vec<String>,
}

/// A refusal the model can correct. The text is for the model: an unknown id
/// lists the ids that exist.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskError {
    /// No task has this id.
    #[error("no task \"{id}\"; existing ids: {}", existing_ids(existing))]
    UnknownId {
        /// The id, as given.
        id: String,
        /// The ids there are, in list order.
        existing: Vec<String>,
    },
    /// A task named as blocking itself.
    #[error("task {id} cannot block itself")]
    BlocksItself {
        /// The task's id.
        id: String,
    },
}

fn existing_ids(ids: &[String]) -> String {
    if ids.is_empty() {
        "none".into()
    } else {
        ids.join(", ")
    }
}

/// What [`TaskBoard::create`] needs to make a task.
#[derive(Debug, Clone, Default)]
pub struct NewTask {
    /// What to do, in a line.
    pub subject: String,
    /// The details, if any.
    pub description: Option<String>,
    /// Present-continuous label shown while in_progress ("Running tests").
    pub active_form: Option<String>,
}

type OnChange = Box<dyn Fn(&[Task]) + Send + Sync>;
type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

struct State {
    /// Insertion order is list order.
    tasks: Vec<Task>,
    next: u64,
}

impl State {
    fn find(&self, id: &str) -> Result<usize, TaskError> {
        self.tasks
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| TaskError::UnknownId {
                id: id.into(),
                existing: self.tasks.iter().map(|t| t.id.clone()).collect(),
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

/// The conversation's task list: create, update, list. Shared by reference
/// (an `Arc`) between the Task tools and the host.
///
/// Every change hands the whole new list to `on_change`, with the board's lock
/// released, so `on_change` may read the board. [`Default`] is a board whose
/// `on_change` does nothing.
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
    /// An empty board that hands every new snapshot to `on_change`; the clock
    /// is the system's ([`now_ms`]).
    pub fn new(on_change: impl Fn(&[Task]) + Send + Sync + 'static) -> Self {
        Self::with_clock(on_change, now_ms)
    }

    /// Same, with the clock `updated_at` reads (a host's paused clock, a test's fixed one).
    pub fn with_clock(
        on_change: impl Fn(&[Task]) + Send + Sync + 'static,
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

    /// Every task, in the order they were created.
    pub fn list(&self) -> Vec<Task> {
        self.state.lock().unwrap().tasks.clone()
    }

    /// The task with this id.
    ///
    /// # Errors
    ///
    /// A [`TaskError`] naming the ids that exist, when none matches.
    pub fn get(&self, id: &str) -> Result<Task, TaskError> {
        let s = self.state.lock().unwrap();
        s.find(id).map(|i| s.tasks[i].clone())
    }

    /// Add a task at the end of the list, `pending`, with the next id; fires
    /// `on_change`. Returns the new task.
    pub fn create(&self, input: NewTask) -> Task {
        let task = {
            let mut s = self.state.lock().unwrap();
            let task = Task {
                id: s.next.to_string(),
                subject: input.subject,
                description: input.description,
                active_form: input.active_form,
                status: TaskStatus::Pending,
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

    /// Apply `patch` to task `id`; fires `on_change`. Returns the updated
    /// task, or None when the patch deleted it.
    ///
    /// `updated_at` is stamped from the board's clock; setting `completed`
    /// stamps `completed_at` too.
    ///
    /// # Errors
    ///
    /// A [`TaskError`] when `id` or an id in `add_blocked_by` / `add_blocks`
    /// does not exist, or when the task would block itself. Every check runs
    /// before the first change: a refused patch changes nothing and fires
    /// nothing.
    pub fn update(&self, id: &str, patch: TaskPatch) -> Result<Option<Task>, TaskError> {
        let out = {
            let mut s = self.state.lock().unwrap();
            let at = s.find(id)?;
            // Every refusal comes before the first change: an error after it would
            // leave an unsaved, unpublished edit.
            for other in patch.add_blocked_by.iter().chain(&patch.add_blocks) {
                s.find(other)?;
                if other == id {
                    return Err(TaskError::BlocksItself { id: id.into() });
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
    /// The next id is one past the highest numeric id in `tasks` (an id that
    /// is not a number counts as 0); an empty list starts again at "1".
    pub fn restore(&self, tasks: Vec<Task>) {
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

fn set_fields(task: &mut Task, patch: &TaskPatch, now: u64) {
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
    if patch.status == Some(PatchStatus::Set(TaskStatus::Completed)) {
        task.completed_at = Some(now);
    }
}

#[cfg(test)]
mod tests;
