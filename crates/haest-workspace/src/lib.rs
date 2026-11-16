//! Workspaces: the worlds haest's tools and shell run in (the contract is
//! `haest_core::workspace`).
//!
//! - [`RealWorkspace`]: the real disk and the real bash -- the opt-out.
//! - [`SandboxWorkspace`]: bashkit, a simulated bash, over a copy-on-write
//!   overlay of the project (reads from disk, writes in memory), with
//!   [`OverlayChanges`] for `/changes` and `/apply`; or over any haest `Fs`
//!   ([`SandboxWorkspace::over`]).
//! - [`HaestFsOverBashkit`] / [`BashkitFsOverHaest`]: the two adapters that let the
//!   tools and the shell share ONE file system (PLAN.md D4).
//!
//! Ported from the TS haest's `core/workspace-real.ts`, `workspace-sandbox.ts`
//! and `overlay-changes.ts`.

mod bridge;
mod overlay;
mod real;
mod sandbox;

pub use bridge::{BashkitFsOverHaest, HaestFsOverBashkit};
pub use overlay::OverlayChanges;
pub use real::{RealFs, RealWorkspace, kill_group};
pub use sandbox::{SandboxMounts, SandboxWorkspace};

/// The bashkit this crate is built on, for a host that implements its own
/// `bashkit::FileSystem` or reads [`SandboxWorkspace::shell_fs`].
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
