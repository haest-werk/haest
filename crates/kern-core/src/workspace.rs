//! The workspace: the ONE door between kern and a file system or a shell.
//! Every command a tool runs goes through [`Workspace::shell`] / [`Workspace::run`];
//! every file operation through [`Workspace::fs`].
//!
//! Implementations live in `kern-workspace` (real disk + real bash, and a bashkit
//! sandbox over a copy-on-write overlay). [`ShutWorkspace`] is the one with
//! neither files nor a shell (brain's dream run).
//!
//! PLUG POINT: implement [`Workspace`] to give the agent a different world.

use std::{path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::fs::{Fs, ShutFs};

crate::word_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum WorkspaceMode ("workspace mode") {
        /// A simulated bash over a copy-on-write overlay of the project. Writes stay in memory.
        Sandbox = "sandbox",
        /// The real disk and the real bash.
        Real = "real",
        /// No files and no shell.
        Shut = "shut",
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

/// Where a streamed command's output goes, chunk by chunk as it arrives:
/// stdout and stderr interleaved, whole UTF-8 characters only.
pub type OutputChunks = Arc<dyn Fn(&str) + Send + Sync>;

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
    /// Like [`Workspace::shell`], but the output goes to `on_output` as it
    /// arrives and NOT into the result, which only says how the command ended:
    /// a background job may print for hours. The default runs
    /// [`Workspace::shell`] and hands the whole output over once it ends; a
    /// workspace with real processes streams.
    async fn shell_streaming(
        &self,
        command: &str,
        opts: ExecOptions,
        on_output: OutputChunks,
    ) -> ExecResult {
        let r = self.shell(command, opts).await;
        for text in [&r.stdout, &r.stderr] {
            if !text.is_empty() {
                on_output(text);
            }
        }
        ExecResult {
            stdout: String::new(),
            stderr: String::new(),
            ..r
        }
    }
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[tokio::test]
    async fn a_workspace_that_cannot_stream_hands_the_output_over_when_it_ends() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let into = seen.clone();
        let opts = ExecOptions {
            cwd: "/".into(),
            cancel: CancellationToken::new(),
            timeout: Duration::from_secs(1),
        };
        let r = ShutWorkspace
            .shell_streaming(
                "true",
                opts,
                Arc::new(move |s: &str| into.lock().unwrap().push(s.into())),
            )
            .await;
        assert_eq!(*seen.lock().unwrap(), [NO_SHELL]);
        assert_eq!(r.exit_code, EXIT_NOT_FOUND);
        assert!(r.stdout.is_empty() && r.stderr.is_empty());
    }
}
