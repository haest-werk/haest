//! The workspace: the ONE door between kern and a file system or a shell.
//! Every command a tool runs goes through [`Workspace::shell`] / [`Workspace::run`];
//! every file operation through [`Workspace::fs`].
//!
//! Implementations live in `kern-workspace` (real disk + real bash, and a bashkit
//! sandbox over a copy-on-write overlay). [`ShutWorkspace`] is the one with
//! neither files nor a shell (brain's dream run).
//!
//! PLUG POINT: implement [`Workspace`] to give the agent a different world.

use std::{fmt, path::PathBuf, str::FromStr, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::fs::{Fs, ShutFs};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceMode {
    /// A simulated bash over a copy-on-write overlay of the project. Writes stay in memory.
    Sandbox,
    /// The real disk and the real bash.
    Real,
    /// No files and no shell.
    Shut,
}

impl WorkspaceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceMode::Sandbox => "sandbox",
            WorkspaceMode::Real => "real",
            WorkspaceMode::Shut => "shut",
        }
    }
}

impl fmt::Display for WorkspaceMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WorkspaceMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "sandbox" => Ok(Self::Sandbox),
            "real" => Ok(Self::Real),
            "shut" => Ok(Self::Shut),
            _ => Err(format!(
                "unknown workspace mode \"{s}\" (sandbox, real, shut)"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecOptions {
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

/// Exit code for "no such program", the shell convention every mode reports.
pub const EXIT_NOT_FOUND: i32 = 127;

/// One path the sandbox changed, for `/changes` and `/apply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// Overlay path, e.g. /src/a.ts.
    pub path: String,
    pub kind: ChangeKind,
    /// The real file changed since the sandbox first touched it: applying would
    /// overwrite the human's edit, so apply skips it.
    pub conflict: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub applied: Vec<Change>,
    pub skipped: Vec<Change>,
}

/// What a sandbox changed, and putting it on the real disk.
#[async_trait]
pub trait ChangeSet: Send + Sync {
    async fn list(&self) -> Vec<Change>;
    /// A unified diff of one change.
    async fn diff(&self, change: &Change) -> String;
    async fn apply(&self) -> std::io::Result<ApplyReport>;
}

#[async_trait]
pub trait Workspace: Send + Sync {
    fn mode(&self) -> WorkspaceMode;
    fn fs(&self) -> Arc<dyn Fs>;
    /// A command line, interpreted by bash (real or simulated).
    async fn shell(&self, command: &str, opts: ExecOptions) -> ExecResult;
    /// A program and its argv, no shell parsing.
    async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult;
    /// What the model must know about this workspace, for the system prompt.
    fn prompt_note(&self) -> String;
    /// The sandbox's in-memory edits to the project; None for every other mode.
    fn changes(&self) -> Option<Arc<dyn ChangeSet>> {
        None
    }
}

/// No files, no shell.
#[derive(Debug, Default)]
pub struct ShutWorkspace;

const NO_SHELL: &str = "this workspace has no shell";

#[async_trait]
impl Workspace for ShutWorkspace {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Shut
    }
    fn fs(&self) -> Arc<dyn Fs> {
        Arc::new(ShutFs)
    }
    async fn shell(&self, _: &str, _: ExecOptions) -> ExecResult {
        ExecResult {
            stderr: NO_SHELL.into(),
            exit_code: EXIT_NOT_FOUND,
            ..Default::default()
        }
    }
    async fn run(&self, _: &[String], _: ExecOptions) -> ExecResult {
        ExecResult {
            stderr: NO_SHELL.into(),
            exit_code: EXIT_NOT_FOUND,
            ..Default::default()
        }
    }
    fn prompt_note(&self) -> String {
        "You have no file system and no shell here; work only with the tools you were given.".into()
    }
}
