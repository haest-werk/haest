//! Workspaces: the worlds haest's tools and shell run in.
//!
//! Every file a tool reads or writes and every command it runs goes through a
//! [`Workspace`] (the contract is in `haest_core::workspace`). This crate has
//! the ones that ship with haest:
//!
//! | Workspace | Files | Shell | A write lands |
//! |---|---|---|---|
//! | [`SandboxWorkspace::open`] (the default) | the project, copy-on-write over the real disk | bashkit, a simulated bash: no real programs, no network | in memory, until the human applies it |
//! | [`SandboxWorkspace::over`] | any haest [`Fs`] the host has | the same simulated bash | in that `Fs` |
//! | [`RealWorkspace`] | the real disk ([`RealFs`]) | the real `bash`, in its own process group | on disk, at once |
//!
//! [`ShutWorkspace`](haest_core::workspace::ShutWorkspace), with neither files
//! nor a shell, lives in haest-core.
//!
//! Most hosts take this crate as `haest::workspace` (the `workspace` feature
//! of the `haest` crate, on by default), and haest-runtime picks one from the
//! `workspace` setting. Depend on it directly when you build a conversation by
//! hand, or need a sandboxed bash over your own storage without the rest of
//! haest.
//!
//! # The real disk or the sandbox
//!
//! [`RealWorkspace`] is the opt-out: the agent edits the project and runs real
//! programs (git, a build, tests). Every child process gets the
//! [`ChildEnv`](haest_core::ChildEnv) the host hands in and nothing else, and
//! a timeout or a cancel kills its whole process group.
//!
//! [`SandboxWorkspace::open`] mounts the project at its own path as a
//! copy-on-write overlay: reads come from disk, writes stay in memory. The
//! directories a host lists in [`SandboxMounts`] appear at their real paths too
//! (read-write or read-only); every other path is an in-memory scratch disk.
//! The shell is bashkit: coreutils, grep, rg, sed, awk, jq, find, diff, tar and
//! the CSV tool `xan`, but no real program and no network. The model is told so
//! through [`Workspace::prompt_note`].
//!
//! # Reviewing and applying the sandbox's changes
//!
//! The overlay records every path a tool or the shell changes ([`OverlayChanges`],
//! reached through [`Workspace::changes`]), for the human to review and apply
//! through the host (the `haest` command's `/changes` and `/apply`). `list`
//! names every change; `diff` shows one; `apply` writes every change to the
//! real disk, skips a path the human edited on disk meanwhile (a conflict), and
//! refuses one that would write through a symlink.
//!
//! ```
//! use std::time::Duration;
//!
//! use haest_core::workspace::{ChangeKind, ExecOptions, Workspace};
//! use haest_workspace::{SandboxMounts, SandboxWorkspace};
//! use tokio_util::sync::CancellationToken;
//!
//! # #[tokio::main]
//! # async fn main() -> std::io::Result<()> {
//! let tmp = tempfile::tempdir()?;
//! let project = tmp.path().to_path_buf();
//! std::fs::write(project.join("notes.md"), "old\n")?;
//!
//! let ws = SandboxWorkspace::open(SandboxMounts::new(&project)).await?;
//! let opts = ExecOptions {
//!     cwd: project.clone(),
//!     cancel: CancellationToken::new(),
//!     timeout: Duration::from_secs(10),
//! };
//! let r = ws.shell("echo new > notes.md && cat notes.md", opts).await;
//! assert_eq!(r.stdout, "new\n");
//! // The disk has not changed.
//! assert_eq!(std::fs::read_to_string(project.join("notes.md"))?, "old\n");
//!
//! // Each change by its path inside the project.
//! let changes = ws.changes().expect("the default sandbox records its changes");
//! let list = changes.list().await;
//! assert_eq!(list[0].path, "/notes.md");
//! assert_eq!(list[0].kind, ChangeKind::Modified);
//! assert!(changes.diff(&list[0]).await.contains("+new"));
//!
//! // Applied: the change goes to disk, and is no longer listed.
//! let report = changes.apply().await?;
//! assert_eq!(report.applied.len(), 1);
//! assert_eq!(std::fs::read_to_string(project.join("notes.md"))?, "new\n");
//! assert!(changes.list().await.is_empty());
//! # Ok(())
//! # }
//! ```
//!
//! # Your own files under the sandboxed shell
//!
//! A host that keeps its files somewhere else (a database, a synced store)
//! implements haest's [`Fs`] and hands it to [`SandboxWorkspace::over`]. The
//! tools write to that `Fs`, and the shell runs over the same object through
//! [`BashkitFsOverHaest`], so the file the agent writes with Write is the file
//! `cat` shows. There is no overlay and no changes to review: the host decides
//! what a write means.
//!
//! ```
//! use std::{path::Path, sync::Arc, time::Duration};
//!
//! use haest_core::{
//!     fs::{Fs, MemFs},
//!     workspace::{ExecOptions, Workspace},
//! };
//! use haest_workspace::SandboxWorkspace;
//! use tokio_util::sync::CancellationToken;
//!
//! # #[tokio::main]
//! # async fn main() -> std::io::Result<()> {
//! let files = Arc::new(MemFs::new());
//! let ws = SandboxWorkspace::over(files.clone());
//! let opts = ExecOptions {
//!     cwd: "/".into(),
//!     cancel: CancellationToken::new(),
//!     timeout: Duration::from_secs(10),
//! };
//! let r = ws.shell("mkdir -p /notes && echo hi > /notes/a.md", opts).await;
//! assert_eq!(r.exit_code, 0, "{}", r.stderr);
//! // The shell wrote into the host's own Fs.
//! assert_eq!(files.read_to_string(Path::new("/notes/a.md")).await?, "hi\n");
//! assert!(ws.changes().is_none());
//! # Ok(())
//! # }
//! ```
//!
//! [`HaestFsOverBashkit`] is the other direction: a bashkit file system seen as
//! a haest `Fs` (what the tools see inside the default sandbox).
//!
//! Ported from the TS haest's `core/workspace-real.ts`, `workspace-sandbox.ts`
//! and `overlay-changes.ts`.
//!
//! [`Workspace`]: haest_core::workspace::Workspace
//! [`Workspace::prompt_note`]: haest_core::workspace::Workspace::prompt_note
//! [`Workspace::changes`]: haest_core::workspace::Workspace::changes
//! [`Fs`]: haest_core::fs::Fs

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod bridge;
mod overlay;
mod real;
mod sandbox;

pub use bridge::{BashkitFsOverHaest, HaestFsOverBashkit};
pub use overlay::OverlayChanges;
pub use real::{RealFs, RealWorkspace, kill_group};
pub use sandbox::{SandboxMounts, SandboxWorkspace};

/// The bashkit this crate is built on (pinned: it is pre-1.0), for a host that
/// implements its own `bashkit::FileSystem` or reads
/// [`SandboxWorkspace::shell_fs`].
pub use bashkit;

use haest_core::workspace::{EXIT_NOT_FOUND, ExecResult};

/// What `run` answers for an empty argv, in either workspace.
fn no_command() -> ExecResult {
    ExecResult {
        stderr: "no command given".into(),
        exit_code: EXIT_NOT_FOUND,
        ..Default::default()
    }
}
