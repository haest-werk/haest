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

use std::{path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::fs::{Fs, ShutFs};

crate::word_enum! {
    /// What kind of world a [`Workspace`] is (the `workspace` setting's words).
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

/// How to run one command: where, until when, and the interrupt.
#[derive(Debug, Clone)]
pub struct ExecOptions {
    /// The working directory, absolute.
    pub cwd: PathBuf,
    /// The interrupt: when it fires, the command is stopped and what it
    /// printed so far comes back.
    pub cancel: CancellationToken,
    /// How long the command may run before it is stopped.
    pub timeout: Duration,
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

/// Exit code for "no such program", the shell convention every mode reports.
pub const EXIT_NOT_FOUND: i32 = 127;

/// Where a streamed command's output goes, chunk by chunk as it arrives:
/// stdout and stderr interleaved, whole UTF-8 characters only.
pub type OutputChunks = Arc<dyn Fn(&str) + Send + Sync>;

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

/// What happened to a path in the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    /// It is new.
    Added,
    /// Its contents changed.
    Modified,
    /// It was removed.
    Deleted,
}

/// Why a change was not applied. The words are the TS haest's (`ApplyFailure`
/// in `overlay-apply.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplyFailureCode {
    /// A symlink on the real disk is on its way, so the write could land
    /// outside the project.
    OutsideProject,
    /// The change is a symlink the sandbox made, or sits behind one: applying
    /// never makes a link.
    Symlink,
    /// The disk refused it (permissions, a full disk).
    WriteFailed,
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
/// use std::{sync::Arc, time::Duration};
///
/// use async_trait::async_trait;
/// use haest_core::{
///     ExecOptions, ExecResult, Fs, MemFs, Workspace, WorkspaceMode,
///     workspace::EXIT_NOT_FOUND,
/// };
/// use tokio_util::sync::CancellationToken;
///
/// struct Tiny {
///     fs: Arc<MemFs>,
/// }
///
/// #[async_trait]
/// impl Workspace for Tiny {
///     fn mode(&self) -> WorkspaceMode {
///         WorkspaceMode::Sandbox
///     }
///     fn fs(&self) -> Arc<dyn Fs> {
///         self.fs.clone()
///     }
///     async fn shell(&self, command: &str, opts: ExecOptions) -> ExecResult {
///         self.run(&[command.to_string()], opts).await
///     }
///     async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult {
///         match argv.first().map(String::as_str) {
///             Some("pwd") => ExecResult {
///                 stdout: format!("{}\n", opts.cwd.display()),
///                 ..Default::default()
///             },
///             _ => ExecResult {
///                 stderr: "command not found\n".into(),
///                 exit_code: EXIT_NOT_FOUND,
///                 ..Default::default()
///             },
///         }
///     }
///     fn prompt_note(&self) -> String {
///         "Your shell knows only `pwd`.".into()
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let ws = Tiny { fs: Arc::new(MemFs::new()) };
/// let opts = ExecOptions {
///     cwd: "/project".into(),
///     cancel: CancellationToken::new(),
///     timeout: Duration::from_secs(5),
/// };
/// assert_eq!(ws.shell("pwd", opts.clone()).await.stdout, "/project\n");
/// assert_eq!(ws.shell("make", opts).await.exit_code, EXIT_NOT_FOUND);
/// # }
/// ```
#[async_trait]
pub trait Workspace: Send + Sync {
    /// What kind of world this is.
    fn mode(&self) -> WorkspaceMode;
    /// The files the agent sees, the same files the shell sees.
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
    /// A program and its argv, no shell parsing: `argv[0]` is the program.
    async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult;
    /// What the model must know about this workspace, for the system prompt;
    /// empty when there is nothing to say.
    fn prompt_note(&self) -> String;
    /// The sandbox's in-memory edits to the project; None for every other mode.
    fn changes(&self) -> Option<Arc<dyn ChangeSet>> {
        None
    }
}

/// No files, no shell: every file call fails ([`ShutFs`]) and every command
/// ends with [`EXIT_NOT_FOUND`]. The prompt note tells the model to work with
/// the tools it was given.
#[derive(Debug, Default)]
pub struct ShutWorkspace;

const NO_SHELL: &str = "this workspace has no shell";

fn no_shell() -> ExecResult {
    ExecResult {
        stderr: NO_SHELL.into(),
        exit_code: EXIT_NOT_FOUND,
        ..Default::default()
    }
}

#[async_trait]
impl Workspace for ShutWorkspace {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Shut
    }
    fn fs(&self) -> Arc<dyn Fs> {
        Arc::new(ShutFs)
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

    /// The TS haest's `ApplyResult`: a failed change is the change's own keys
    /// plus `code` and `reason`.
    #[test]
    fn an_apply_report_has_the_ts_haests_wire_shape() {
        let change = Change {
            path: "/l".into(),
            kind: ChangeKind::Added,
            conflict: false,
        };
        let report = ApplyReport {
            applied: vec![],
            skipped: vec![],
            failed: vec![FailedChange {
                change,
                code: ApplyFailureCode::Symlink,
                reason: "why".into(),
            }],
        };
        let wire = serde_json::json!({
            "applied": [],
            "skipped": [],
            "failed": [{"path": "/l", "kind": "added", "conflict": false, "code": "symlink", "reason": "why"}],
        });
        assert_eq!(serde_json::to_value(&report).unwrap(), wire);
        assert_eq!(serde_json::from_value::<ApplyReport>(wire).unwrap(), report);
        let words = [
            (ApplyFailureCode::OutsideProject, "outside-project"),
            (ApplyFailureCode::Symlink, "symlink"),
            (ApplyFailureCode::WriteFailed, "write-failed"),
        ];
        for (code, word) in words {
            assert_eq!(serde_json::to_value(code).unwrap(), word);
        }
    }
}
