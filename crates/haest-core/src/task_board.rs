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
//! use std::sync::mpsc;
//!
//! use haest_core::task_board::{NewTask, PatchStatus, Task, TaskBoard, TaskPatch, TaskStatus};
//!
//! // Where the host saves and publishes each snapshot.
//! let (save, saved) = mpsc::channel();
//! let board = TaskBoard::new(move |tasks: &[Task]| {
//!     let _ = save.send(tasks.to_vec());
//! });
//! // The Task tools get a clone; it is the same board.
//! let tools_board = board.clone();
//!
//! let schema = tools_board.create(NewTask { subject: "Write the schema".into(), ..Default::default() })?;
//! let api = board.create(NewTask { subject: "Write the API".into(), ..Default::default() })?;
//! board.update(&api.id, TaskPatch { add_blocked_by: vec![schema.id.clone()], ..Default::default() })?;
//! let done = TaskPatch { status: Some(PatchStatus::Set(TaskStatus::Completed)), ..Default::default() };
//! board.update(&schema.id, done)?;
//!
//! assert_eq!(board.get("2")?.blocked_by, ["1"]);
//! assert_eq!(board.get("1")?.status, TaskStatus::Completed);
//! assert_eq!(saved.try_iter().count(), 4, "one snapshot per change");
//! # Ok::<(), haest_core::task_board::TaskError>(())
//! ```

mod task;
#[cfg(test)]
mod tests;

use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

pub use task::{NewTask, PatchStatus, Task, TaskError, TaskPatch, TaskStatus};

use crate::deps::{ClockRef, WallClock};

type OnChange = Box<dyn Fn(&[Task]) + Send + Sync>;

/// What every clone of a board is.
struct Shared {
    state: Mutex<State>,
    on_change: OnChange,
    /// Read with the state unlocked, and the handle cloned out first: a
    /// host's clock may take its time.
    clock: Mutex<ClockRef>,
}

struct State {
    /// Insertion order is list order.
    tasks: Vec<Task>,
    /// The id the next task gets; None once the ids have run out.
    next: Option<u64>,
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

/// The conversation's task list: create, update, list.
///
/// A cheap `Clone`: every clone is the same board, its tasks and its clock,
/// so the host keeps one and hands the Task tools another.
///
/// Every change hands the whole new list to `on_change`, with the board's lock
/// released, so `on_change` may read the board. [`Default`] is a board whose
/// `on_change` does nothing.
#[derive(Clone)]
pub struct TaskBoard(Arc<Shared>);

/// The tasks, as [`TaskBoard::list`] has them; `<locked>` while another
/// thread holds the board.
impl fmt::Debug for TaskBoard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("TaskBoard");
        match self.0.state.try_lock() {
            Ok(state) => out.field("tasks", &state.tasks),
            Err(_) => out.field("tasks", &format_args!("<locked>")),
        };
        out.finish_non_exhaustive()
    }
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
        Self(Arc::new(Shared {
            state: Mutex::new(State {
                tasks: Vec::new(),
                next: Some(1),
            }),
            on_change: Box::new(on_change),
            clock: Mutex::new(ClockRef::new(WallClock)),
        }))
    }

    /// The clock `updated_at` reads from now on (a host's paused clock, a
    /// test's fixed one; any `Fn() -> u64` is a [`Clock`](crate::deps::Clock)),
    /// for every clone of this board.
    pub fn set_clock(&self, clock: impl Into<ClockRef>) {
        *self.clock() = clock.into();
    }

    /// The board. Its lock is held only while this file's own code runs
    /// (never `on_change`, never the clock), so a poisoned one is a bug here.
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().expect("the task board's lock poisoned")
    }

    /// The board's clock slot. Its lock is held only to swap or clone the
    /// handle, never while the clock runs, so a poisoned one is a bug here.
    fn clock(&self) -> MutexGuard<'_, ClockRef> {
        self.0
            .clock
            .lock()
            .expect("the task board's clock lock poisoned")
    }

    /// The board's clock, read with no lock held.
    fn now_ms(&self) -> u64 {
        let clock = self.clock().clone();
        clock.now_ms()
    }

    /// Every task, in the order they were created.
    pub fn list(&self) -> Vec<Task> {
        self.state().tasks.clone()
    }

    /// The task with this id.
    ///
    /// # Errors
    ///
    /// A [`TaskError`] naming the ids that exist, when none matches.
    pub fn get(&self, id: &str) -> Result<Task, TaskError> {
        let s = self.state();
        s.find(id).map(|i| s.tasks[i].clone())
    }

    /// Add a task at the end of the list, `pending`, with the next id; fires
    /// `on_change`. Returns the new task.
    ///
    /// # Errors
    ///
    /// [`TaskError::IdsExhausted`] once an id of `u64::MAX` has been handed
    /// out or restored: the board never hands out an id twice. Nothing
    /// changes and nothing fires.
    pub fn create(&self, input: NewTask) -> Result<Task, TaskError> {
        let now = self.now_ms();
        let task = {
            let mut s = self.state();
            let id = s.next.ok_or(TaskError::IdsExhausted)?;
            let task = Task {
                id: id.to_string(),
                subject: input.subject,
                description: input.description,
                active_form: input.active_form,
                status: TaskStatus::Pending,
                blocked_by: Vec::new(),
                blocks: Vec::new(),
                owner: None,
                updated_at: now,
                completed_at: None,
            };
            s.next = id.checked_add(1);
            s.tasks.push(task.clone());
            task
        };
        self.changed();
        Ok(task)
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
        let now = self.now_ms();
        let out = {
            let mut s = self.state();
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
    /// is not a number counts as 0); an empty list starts again at "1". A
    /// restored id of `u64::MAX` leaves none: [`create`](Self::create) then
    /// refuses.
    pub fn restore(&self, tasks: Vec<Task>) {
        let mut s = self.state();
        let max = tasks
            .iter()
            .map(|t| t.id.parse::<u64>().unwrap_or(0))
            .max()
            .unwrap_or(0);
        s.next = max.checked_add(1);
        s.tasks = tasks;
    }

    /// The snapshot goes out with the lock released, so `on_change` may read the board.
    fn changed(&self) {
        let snapshot = self.list();
        (self.0.on_change)(&snapshot);
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
