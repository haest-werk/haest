//! The sandbox workspace (the default): bashkit, a bash interpreter, over a
//! mount table that keeps every path identical to the real one (see
//! [`SandboxMounts`]): the project is a copy-on-write overlay -- reads from
//! disk, writes in memory until `/apply`.
//!
//! No host environment reaches the shell (no API keys) unless the host passes
//! a variable on purpose ([`SandboxWorkspace::with_env`]); no network, no real
//! binaries.
//!
//! PLUG POINT: [`SandboxWorkspace::over`] puts the same sandboxed bash over a
//! host's OWN [`Fs`], so the file the agent writes with Write is the file `cat`
//! shows, wherever the host keeps it. [`SandboxWorkspace::with_command`] adds a
//! command to the shell (kern-code's `js`). Every sandbox has `xan` (CSV verbs,
//! `xan/`) beyond bashkit's builtins.

mod commands;
mod exec;
mod mounts;
mod xan;

use std::{io, sync::Arc};

use async_trait::async_trait;
use kern_core::{
    ChildEnv,
    fs::Fs,
    shell_command::ShellCommand,
    workspace::{ChangeSet, ExecOptions, ExecResult, Workspace, WorkspaceMode},
};

pub use mounts::SandboxMounts;

use crate::{
    bridge::{BashkitFsOverKern, KernFsOverBashkit},
    overlay::OverlayChanges,
};
use exec::Shell;

/// What the shell is, for every sandbox. Each added command's own note follows it.
const SHELL_NOTE: &str = "Bash is a simulated shell: coreutils, grep, rg, sed, awk, jq, find, diff, tar and friends, but NO git, bun, node, npm, python or network, and no other real programs (for a web page, use the WebFetch tool -- it runs outside the sandbox).";

/// The commands every sandbox has beyond bashkit's builtins (`xan`). A host's
/// command of the same name replaces one.
fn own_commands() -> Vec<Arc<dyn ShellCommand>> {
    vec![Arc::new(xan::Xan)]
}

/// Where edits go in the default sandbox, and how they reach the disk.
const OVERLAY_NOTE: &str = "Reads come from the real project; every file you write or edit lives in memory until the human reviews it with /changes and puts it on disk with /apply -- otherwise it is discarded when this conversation ends. When you finish edits worth keeping, tell the human to run /changes and /apply. If the task needs real builds, tests or git, say so plainly -- the human can relaunch this conversation with the real file system.";

pub struct SandboxWorkspace {
    /// What the shell runs over.
    shell_fs: Arc<dyn bashkit::FileSystem>,
    /// What the tools see: the same files.
    fs: Arc<dyn Fs>,
    /// A host's replacement for the built note.
    note: Option<String>,
    commands: Vec<Arc<dyn ShellCommand>>,
    /// The variables the host passes the shell.
    env: ChildEnv,
    changes: Option<Arc<OverlayChanges>>,
}

impl SandboxWorkspace {
    /// The default sandbox: a copy-on-write overlay of `mounts.cwd`, the real
    /// writable and readable dirs at their own paths, a scratch disk elsewhere.
    /// Fails when a mounted directory does not exist.
    pub async fn open(mounts: SandboxMounts) -> io::Result<Self> {
        let changes = Arc::new(mounts::project_overlay(&mounts.cwd).await?);
        let shell_fs = mounts::sandbox_fs(&mounts, &changes).await?;
        let fs = Arc::new(KernFsOverBashkit::new(shell_fs.clone()));
        Ok(Self::with_parts(shell_fs, fs, Some(changes)))
    }

    /// A sandboxed bash over a host's own file system: the shell's whole world
    /// is `fs`, and the tools write to `fs` itself. No overlay and no
    /// `/changes` -- the host decides what a write means.
    pub fn over(fs: Arc<dyn Fs>) -> Self {
        Self::with_parts(Arc::new(BashkitFsOverKern::new(fs.clone())), fs, None)
    }

    fn with_parts(
        shell_fs: Arc<dyn bashkit::FileSystem>,
        fs: Arc<dyn Fs>,
        changes: Option<Arc<OverlayChanges>>,
    ) -> Self {
        Self {
            shell_fs,
            fs,
            note: None,
            commands: own_commands(),
            env: ChildEnv::default(),
            changes,
        }
    }

    /// Variables every command in the shell sees, over the shell's own
    /// (`HOME`, `USER`, ...). The host's environment never reaches the shell
    /// any other way.
    pub fn with_env(mut self, env: ChildEnv) -> Self {
        self.env = env;
        self
    }

    /// Replace the note the system prompt carries about this workspace.
    pub fn with_prompt_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Add a command to the shell. Its note, if any, joins the prompt note. A
    /// name the shell already has (a builtin, an earlier command): this one wins.
    pub fn with_command(mut self, command: Arc<dyn ShellCommand>) -> Self {
        self.commands.retain(|c| c.name() != command.name());
        self.commands.push(command);
        self
    }

    fn shell_world(&self) -> Shell<'_> {
        Shell {
            fs: self.shell_fs.clone(),
            kern_fs: &self.fs,
            commands: &self.commands,
            env: &self.env,
        }
    }

    /// The file system the shell runs over (the tools' [`Workspace::fs`] is the same files).
    pub fn shell_fs(&self) -> Arc<dyn bashkit::FileSystem> {
        self.shell_fs.clone()
    }

    /// The project overlay's edits, typed; [`Workspace::changes`] is the same object.
    pub fn overlay_changes(&self) -> Option<Arc<OverlayChanges>> {
        self.changes.clone()
    }
}

/// `"$@"` runs the positional parameters as one command: argv, no shell parsing.
const RUN_ARGV: &str = "\"$@\"";

#[async_trait]
impl Workspace for SandboxWorkspace {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Sandbox
    }

    fn fs(&self) -> Arc<dyn Fs> {
        self.fs.clone()
    }

    async fn shell(&self, command: &str, opts: ExecOptions) -> ExecResult {
        exec::exec(self.shell_world(), command, &[], opts).await
    }

    async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult {
        if argv.first().is_none_or(String::is_empty) {
            return crate::no_command();
        }
        exec::exec(self.shell_world(), RUN_ARGV, argv, opts).await
    }

    fn prompt_note(&self) -> String {
        if let Some(note) = &self.note {
            return note.clone();
        }
        let mut parts = vec![SHELL_NOTE.to_string()];
        parts.extend(self.commands.iter().filter_map(|c| c.note()));
        if self.changes.is_some() {
            parts.push(OVERLAY_NOTE.into());
        }
        format!("# Sandbox\nYou run in a SANDBOX. {}", parts.join(" "))
    }

    fn changes(&self) -> Option<Arc<dyn ChangeSet>> {
        self.changes.clone().map(|c| c as Arc<dyn ChangeSet>)
    }
}
