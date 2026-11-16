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

mod task;
#[cfg(test)]
mod tests;

use std::sync::Mutex;

pub use task::{NewTask, PatchStatus, Task, TaskError, TaskPatch, TaskStatus};

use crate::deps::{Clock, WallClock};

type OnChange = Box<dyn Fn(&[Task]) + Send + Sync>;

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
    clock: Box<dyn Clock>,
}

impl Default for TaskBoard {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

impl TaskBoard {
    /// An empty board that hands every new snapshot to `on_change`; the clock
    /// is the [`WallClock`].
    pub fn new(on_change: impl Fn(&[Task]) + Send + Sync + 'static) -> Self {
        Self::with_clock(on_change, WallClock)
    }

    /// Same, with the clock `updated_at` reads (a host's paused clock, a test's
    /// fixed one; any `Fn() -> u64` is a [`Clock`]).
    pub fn with_clock(
        on_change: impl Fn(&[Task]) + Send + Sync + 'static,
        clock: impl Clock + 'static,
    ) -> Self {
        Self {
            state: Mutex::new(State {
                tasks: Vec::new(),
                next: 1,
            }),
            on_change: Box::new(on_change),
            clock: Box::new(clock),
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
                updated_at: self.clock.now_ms(),
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
                let now = self.clock.now_ms();
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
