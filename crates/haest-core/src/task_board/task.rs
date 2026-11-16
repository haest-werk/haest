//! What the board holds: a [`Task`], and what creating and changing one takes.

use serde::{Deserialize, Serialize};

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

/// A change to one task ([`TaskBoard::update`](super::TaskBoard::update)). Only the fields that are
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

/// What [`TaskBoard::create`](super::TaskBoard::create) needs to make a task.
#[derive(Debug, Clone, Default)]
pub struct NewTask {
    /// What to do, in a line.
    pub subject: String,
    /// The details, if any.
    pub description: Option<String>,
    /// Present-continuous label shown while in_progress ("Running tests").
    pub active_form: Option<String>,
}
