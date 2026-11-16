//! The sandbox workspace (the default): bashkit, a bash interpreter, over a
//! mount table that keeps every path identical to the real one (see
//! [`SandboxMounts`]): the project is a copy-on-write overlay -- reads from
//! disk, writes in memory until the human applies them.
//!
//! No host environment reaches the shell (no API keys) unless the host passes
//! a variable on purpose ([`SandboxWorkspace::with_env`]); no network, no real
//! binaries.
//!
//! PLUG POINT: [`SandboxWorkspace::over`] puts the same sandboxed bash over a
//! host's OWN [`Fs`], so the file the agent writes with Write is the file `cat`
//! shows, wherever the host keeps it. [`SandboxWorkspace::with_command`] adds a
//! command to the shell (haest-code's `js`). Every sandbox has `xan` (CSV verbs,
//! `xan/`) beyond bashkit's builtins.

mod commands;
mod exec;
mod mounts;
mod xan;

use std::{io, sync::Arc};

use async_trait::async_trait;
use haest_core::{
    ChildEnv,
    fs::Fs,
    remedy::{Limit, Remedies},
    shell_command::ShellCommand,
    workspace::{ChangeSet, ExecOptions, ExecResult, Workspace, WorkspaceMode},
};

pub use mounts::SandboxMounts;

use crate::{
    bridge::{BashkitFsOverHaest, HaestFsOverBashkit},
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
const OVERLAY_NOTE: &str = "Reads come from the real project; every file you write or edit lives in memory until the human reviews it and puts it on disk -- otherwise it is discarded when this conversation ends. When you finish edits worth keeping, tell the human they are waiting for review. If the task needs real builds, tests or git, say so plainly.";

/// bashkit, a simulated bash, over files the host chooses: the project as a
/// copy-on-write overlay ([`open`](Self::open)), or the host's own [`Fs`]
/// ([`over`](Self::over)).
///
/// The tools ([`Workspace::fs`]) and the shell see the same files: a file
/// written with the Write tool is the file `cat` shows.
///
/// Each [`shell`](Workspace::shell) call gets a FRESH shell over those files:
/// variables, functions and `cd` do not outlive the call, files do.
/// [`run`](Workspace::run) runs its argv as `"$@"`, with no shell parsing; an
/// empty argv is exit 127. A call ends the way a real shell reports it:
///
/// | Ended by | `exit_code` | `timed_out` | Output |
/// |---|---|---|---|
/// | the command | its own | false | all of it |
/// | the call's timeout | 124 | true | what it printed so far |
/// | the cancel token | 130 | false | what it printed so far |
/// | a syntax error | 2 | false | `bash: ...` on stderr |
///
/// A call has generous limits of its own, so a runaway script ends: 100,000
/// commands, 100,000 loop iterations, 10 minutes whatever the timeout, and at
/// most 64 MB of output kept per stream. The project overlay and the scratch
/// disk each hold at most 256 MB in memory; past that a write fails.
///
/// The shell has no network, no real programs and none of the host's
/// environment: only what [`with_env`](Self::with_env) passes, over the
/// shell's own `HOME` and `USER` (`/home/user`, `user`).
/// [`Workspace::prompt_note`] tells the model all of that, and, over the
/// overlay, that its edits wait for the human to apply them.
///
/// # Examples
///
/// A host's variable, and a command added to the shell:
///
/// ```
/// use std::{sync::Arc, time::Duration};
///
/// use async_trait::async_trait;
/// use haest_core::{
///     ChildEnv,
///     fs::MemFs,
///     shell_command::{ShellCall, ShellCommand},
///     workspace::{ExecOptions, ExecResult, Workspace},
/// };
/// use haest_workspace::SandboxWorkspace;
/// use tokio_util::sync::CancellationToken;
///
/// /// `shout WORDS`: the words in capitals.
/// struct Shout;
///
/// #[async_trait]
/// impl ShellCommand for Shout {
///     fn name(&self) -> &str {
///         "shout"
///     }
///     async fn run(&self, call: ShellCall) -> ExecResult {
///         ExecResult {
///             stdout: format!("{}\n", call.args.join(" ").to_uppercase()),
///             ..Default::default()
///         }
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() {
/// let ws = SandboxWorkspace::over(Arc::new(MemFs::new()))
///     .with_env(ChildEnv::new([("GREETING".to_string(), "hej".to_string())]))
///     .with_command(Arc::new(Shout));
/// let opts = ExecOptions {
///     cwd: "/".into(),
///     cancel: CancellationToken::new(),
///     timeout: Duration::from_secs(10),
/// };
/// let r = ws.shell("shout \"$GREETING\" there", opts.clone()).await;
/// assert_eq!(r.stdout, "HEJ THERE\n");
///
/// // `cd` and variables end with the call; files stay.
/// ws.shell("mkdir /a && cd /a && X=1 && echo hi > f", opts.clone()).await;
/// let r = ws.shell("pwd; echo \"[$X]\"; cat /a/f", opts).await;
/// assert_eq!(r.stdout, "/\n[]\nhi\n");
/// # }
/// ```
///
/// [`Workspace::fs`]: haest_core::workspace::Workspace::fs
/// [`Workspace::prompt_note`]: haest_core::workspace::Workspace::prompt_note
pub struct SandboxWorkspace {
    /// What the shell runs over.
    shell_fs: Arc<dyn bashkit::FileSystem>,
    /// What the tools see: the same files.
    fs: Arc<dyn Fs>,
    /// A host's replacement for the built note.
    note: Option<String>,
    /// The host's way to review and apply the overlay, for the note.
    remedies: Remedies,
    commands: Vec<Arc<dyn ShellCommand>>,
    /// The variables the host passes the shell.
    env: ChildEnv,
    changes: Option<Arc<OverlayChanges>>,
}

impl SandboxWorkspace {
    /// The default sandbox: a copy-on-write overlay of `mounts.cwd`, the real
    /// writable and readable dirs at their own paths, a scratch disk elsewhere.
    ///
    /// Its [`Workspace::changes`] (and [`overlay_changes`](Self::overlay_changes))
    /// is the overlay's [`OverlayChanges`]: what the human reviews and applies.
    ///
    /// # Errors
    ///
    /// When the project or a mounted directory does not exist or cannot be
    /// opened.
    pub async fn open(mounts: SandboxMounts) -> io::Result<Self> {
        let changes = Arc::new(mounts::project_overlay(&mounts.cwd).await?);
        let shell_fs = mounts::sandbox_fs(&mounts, &changes).await?;
        let fs = Arc::new(HaestFsOverBashkit::new(shell_fs.clone()));
        Ok(Self::with_parts(shell_fs, fs, Some(changes)))
    }

    /// A sandboxed bash over a host's own file system: the shell's whole world
    /// is `fs`, and the tools write to `fs` itself. No overlay and no changes
    /// to review -- the host decides what a write means.
    ///
    /// Every path the shell names is normalized (`/a/./b/../c` is `/a/c`)
    /// before it reaches `fs`. What a haest `Fs` cannot hold, stated:
    /// - no symlinks: `ln -s` fails;
    /// - no permission bits: a `chmod` is remembered in memory, for the life
    ///   of this workspace, so `chmod +x f && ./f` works;
    /// - no settable times: `touch` on an existing file succeeds and leaves the
    ///   time to `fs`.
    ///
    /// An error `fs` returns reads like a shell's (`cat: f: No such file or
    /// directory`), except a permission refusal with a message of the host's
    /// own ("you can't edit there; tell them instead"), which reaches the
    /// shell's stderr as written.
    pub fn over(fs: Arc<dyn Fs>) -> Self {
        Self::with_parts(Arc::new(BashkitFsOverHaest::new(fs.clone())), fs, None)
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
            remedies: Remedies::default(),
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

    /// Replace the note the system prompt carries about this workspace
    /// ([`Workspace::prompt_note`]) with `note`, the added commands' notes
    /// included.
    pub fn with_prompt_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// The host's way to review the overlay's changes and put them on disk
    /// ([`Limit::SandboxEdits`]), added to the prompt note's part about them.
    pub fn with_remedies(mut self, remedies: Remedies) -> Self {
        self.remedies = remedies;
        self
    }

    /// Add a command to the shell (haest-code's `js`). Its note, if any, joins
    /// the prompt note. A name the shell already has (a builtin, `xan`, an
    /// earlier command): this one wins.
    ///
    /// The command runs inside the shell call it is part of, so pipes, `&&`
    /// and redirects work, and it gets that call's cancel token and deadline.
    pub fn with_command(mut self, command: Arc<dyn ShellCommand>) -> Self {
        self.commands.retain(|c| c.name() != command.name());
        self.commands.push(command);
        self
    }

    fn shell_world(&self) -> Shell<'_> {
        Shell {
            fs: self.shell_fs.clone(),
            haest_fs: &self.fs,
            commands: &self.commands,
            env: &self.env,
        }
    }

    /// The project overlay's edits, typed; [`Workspace::changes`] is the same
    /// object. None for a sandbox made with [`over`](Self::over).
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
            parts.push(self.remedies.add_to(OVERLAY_NOTE, Limit::SandboxEdits));
        }
        format!("# Sandbox\nYou run in a SANDBOX. {}", parts.join(" "))
    }

    fn changes(&self) -> Option<Arc<dyn ChangeSet>> {
        self.changes.clone().map(|c| c as Arc<dyn ChangeSet>)
    }
}
