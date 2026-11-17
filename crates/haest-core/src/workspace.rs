//! The workspace: the ONE door between haest and a file system or a shell.
//! Every command a tool runs goes through [`Workspace::shell`] / [`Workspace::run`];
//! every file operation through [`Workspace::fs`].
//!
//! Implementations live in `haest-workspace` (real disk + real bash, and a bashkit
//! sandbox over a copy-on-write overlay). [`ShutWorkspace`] is the one with
//! neither files nor a shell (a run that only talks, or only uses a host's own
//! tools).
//!
//! PLUG POINT: implement [`Workspace`] to give the agent a different world.
//! The loop hands it to every tool ([`AgentDeps`](crate::AgentDeps)'
//! `workspace`). A sandbox also reports what it changed ([`ChangeSet`]) so a
//! human can review and apply it.

use std::{fmt, ops::Deref, path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::fs::{FsRef, ShutFs};

crate::word_enum! {
    /// What kind of world a [`Workspace`] is (the `workspace` setting's words).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum WorkspaceMode ("workspace mode") {
        /// A simulated bash over a copy-on-write overlay of the project. Writes stay in memory.
        Sandbox = "sandbox",
        /// The real disk and the real bash.
        Real = "real",
        /// No files and no shell.
        Shut = "shut",
    }
}

/// How to run one command: where, until when, and the interrupt.
///
/// Built with [`ExecOptions::new`]; a workspace reads the fields.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExecOptions {
    /// The working directory, absolute.
    pub cwd: PathBuf,
    /// The interrupt: when it fires, the command is stopped and what it
    /// printed so far comes back.
    pub cancel: CancellationToken,
    /// How long the command may run before it is stopped.
    pub timeout: Duration,
}

impl ExecOptions {
    /// Run in `cwd` for at most `timeout`, with a fresh interrupt
    /// ([`with_cancel`](Self::with_cancel) shares the caller's).
    pub fn new(cwd: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            cwd: cwd.into(),
            cancel: CancellationToken::new(),
            timeout,
        }
    }

    /// Stopped when `cancel` fires.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }
}

/// How a command ended: what it printed, its exit code, and whether it ran
/// out of time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecResult {
    /// Standard output, as text (invalid UTF-8 replaced).
    pub stdout: String,
    /// Standard error, as text; also where a workspace says why a command
    /// could not run.
    pub stderr: String,
    /// The exit code: 0 for success; [`EXIT_NOT_FOUND`] for no such program.
    pub exit_code: i32,
    /// The command was stopped because [`ExecOptions::timeout`] ran out.
    pub timed_out: bool,
}

impl ExecResult {
    /// A command that succeeded (exit code 0) and printed `stdout`.
    pub fn ok(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            ..Self::default()
        }
    }

    /// A command that ended with `exit_code`, saying why on `stderr`.
    ///
    /// ```
    /// use haest_core::workspace::{EXIT_NOT_FOUND, ExecResult};
    ///
    /// let missing = ExecResult::failed("upper: command not found\n", EXIT_NOT_FOUND);
    /// assert_eq!((missing.stdout.as_str(), missing.exit_code), ("", EXIT_NOT_FOUND));
    /// assert_eq!(ExecResult::ok("hi\n").exit_code, 0);
    /// ```
    pub fn failed(stderr: impl Into<String>, exit_code: i32) -> Self {
        Self {
            stderr: stderr.into(),
            exit_code,
            ..Self::default()
        }
    }
}

/// Exit code for "no such program", the shell convention every mode reports.
pub const EXIT_NOT_FOUND: i32 = 127;

/// Where a streamed command's output goes, chunk by chunk as it arrives:
/// stdout and stderr interleaved, whole UTF-8 characters only.
///
/// Built from a closure (`OutputChunks::from(|text: &str| ..)`) and called
/// like one (`on_output(text)`); cheap to clone.
#[derive(Clone)]
pub struct OutputChunks(Arc<dyn Fn(&str) + Send + Sync>);

impl OutputChunks {
    /// Output that goes to `f`.
    pub fn new(f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }
}

impl<F: Fn(&str) + Send + Sync + 'static> From<F> for OutputChunks {
    fn from(f: F) -> Self {
        Self::new(f)
    }
}

impl Deref for OutputChunks {
    type Target = dyn Fn(&str) + Send + Sync;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl fmt::Debug for OutputChunks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OutputChunks").finish_non_exhaustive()
    }
}

/// One path the sandbox changed: what a host lists for the human to review,
/// and applies ([`ChangeSet`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// Overlay path, e.g. /src/a.ts.
    pub path: String,
    /// Added, modified or deleted.
    pub kind: ChangeKind,
    /// The real file changed since the sandbox first touched it: applying would
    /// overwrite the human's edit, so apply skips it.
    pub conflict: bool,
}

crate::word_enum! {
    /// What happened to a path in the sandbox.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ChangeKind ("change kind") {
        /// It is new.
        Added = "added",
        /// Its contents changed.
        Modified = "modified",
        /// It was removed.
        Deleted = "deleted",
    }
}

crate::word_enum! {
    /// Why a change was not applied. The words are the TS haest's (`ApplyFailure`
    /// in `overlay-apply.ts`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ApplyFailureCode ("apply failure") {
        /// A symlink on the real disk is on its way, so the write could land
        /// outside the project.
        OutsideProject = "outside-project",
        /// The change is a symlink the sandbox made, or sits behind one: applying
        /// never makes a link.
        Symlink = "symlink",
        /// The disk refused it (permissions, a full disk).
        WriteFailed = "write-failed",
    }
}

/// A change applying could not put on disk. It stays in the sandbox, still listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedChange {
    /// The change (its keys inline on the wire, beside `code` and `reason`).
    #[serde(flatten)]
    pub change: Change,
    /// Why, as a word a host can act on.
    pub code: ApplyFailureCode,
    /// Why, for the human.
    pub reason: String,
}

/// What applying did with every change ([`ChangeSet::apply`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReport {
    /// Now on the real disk, and gone from the sandbox's list.
    pub applied: Vec<Change>,
    /// Conflicts: left alone so the human's edit survives.
    pub skipped: Vec<Change>,
    /// Not applied, each with why.
    pub failed: Vec<FailedChange>,
}

/// What a sandbox changed, and putting it on the real disk.
/// [`Workspace::changes`] hands it out; only a sandbox has one.
#[async_trait]
pub trait ChangeSet: Send + Sync {
    /// Every path the sandbox changed and has not applied yet.
    async fn list(&self) -> Vec<Change>;
    /// A unified diff of one change.
    async fn diff(&self, change: &Change) -> String;
    /// Put every change on the real disk, but conflicts (skipped) and
    /// changes that cannot be applied safely (failed, each with why). A
    /// change that fails stops no other; the report says what happened to
    /// each.
    async fn apply(&self) -> ApplyReport;
}

crate::plug_ref! {
    /// A [`ChangeSet`], shared: what [`Workspace::changes`] hands out.
    pub struct ChangeSetRef(dyn ChangeSet);
}

/// The world the agent works in: a file system and a shell, one door for both.
///
/// Every tool reaches files through [`Workspace::fs`] and runs commands through
/// [`Workspace::shell`] or [`Workspace::run`], so swapping the workspace swaps
/// the world: the real disk, a sandbox whose writes stay in memory
/// (`haest-workspace`), or nothing at all ([`ShutWorkspace`]).
///
/// Running a command never fails: a program that cannot start, a timeout and
/// a cancel all come back as an [`ExecResult`] (a nonzero exit code, the
/// reason on stderr, `timed_out` when [`ExecOptions::timeout`] ran out). A
/// stopped command returns what it printed so far.
///
/// # Examples
///
/// A workspace whose files are in memory and whose only "shell" answers
/// `pwd`:
///
/// ```
/// use std::time::Duration;
///
/// use async_trait::async_trait;
/// use haest_core::{
///     ExecOptions, ExecResult, FsRef, MemFs, Workspace, WorkspaceMode,
///     workspace::EXIT_NOT_FOUND,
/// };
///
/// struct Tiny {
///     fs: FsRef,
/// }
///
/// #[async_trait]
/// impl Workspace for Tiny {
///     fn mode(&self) -> WorkspaceMode {
///         WorkspaceMode::Sandbox
///     }
///     fn fs(&self) -> FsRef {
///         self.fs.clone()
///     }
///     async fn shell(&self, command: &str, opts: ExecOptions) -> ExecResult {
///         self.run(&[command.to_string()], opts).await
///     }
///     async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult {
///         match argv.first().map(String::as_str) {
///             Some("pwd") => ExecResult::ok(format!("{}\n", opts.cwd.display())),
///             _ => ExecResult::failed("command not found\n", EXIT_NOT_FOUND),
///         }
///     }
///     fn prompt_note(&self) -> String {
///         "Your shell knows only `pwd`.".into()
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// let ws = Tiny { fs: MemFs::new().into() };
/// let opts = ExecOptions::new("/project", Duration::from_secs(5));
/// assert_eq!(ws.shell("pwd", opts.clone()).await.stdout, "/project\n");
/// assert_eq!(ws.shell("make", opts).await.exit_code, EXIT_NOT_FOUND);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait Workspace: Send + Sync {
    /// What kind of world this is.
    fn mode(&self) -> WorkspaceMode;
    /// The files the agent sees, the same files the shell sees.
    fn fs(&self) -> FsRef;
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
    /// A program and its argv, no shell parsing: `argv[0]` is the program.
    async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult;
    /// What the model must know about this workspace, for the system prompt;
    /// empty when there is nothing to say.
    fn prompt_note(&self) -> String;
    /// The sandbox's in-memory edits to the project; None for every other mode.
    fn changes(&self) -> Option<ChangeSetRef> {
        None
    }
}

crate::plug_ref! {
    /// A [`Workspace`], shared: what [`SharedDeps::workspace`](crate::SharedDeps::workspace)
    /// and every [`ToolContext`](crate::ToolContext) hold.
    pub struct WorkspaceRef(dyn Workspace);
}

/// No files, no shell: every file call fails ([`ShutFs`]) and every command
/// ends with [`EXIT_NOT_FOUND`]. The prompt note tells the model to work with
/// the tools it was given.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ShutWorkspace;

const NO_SHELL: &str = "this workspace has no shell";

fn no_shell() -> ExecResult {
    ExecResult::failed(NO_SHELL, EXIT_NOT_FOUND)
}

#[async_trait]
impl Workspace for ShutWorkspace {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Shut
    }
    fn fs(&self) -> FsRef {
        ShutFs.into()
    }
    async fn shell(&self, _: &str, _: ExecOptions) -> ExecResult {
        no_shell()
    }
    async fn run(&self, _: &[String], _: ExecOptions) -> ExecResult {
        no_shell()
    }
    fn prompt_note(&self) -> String {
        "You have no file system and no shell here; work only with the tools you were given.".into()
    }
}

#[cfg(test)]
mod tests;
