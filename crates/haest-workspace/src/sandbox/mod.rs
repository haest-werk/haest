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
//! host's OWN [`Fs`](haest_core::fs::Fs), so the file the agent writes with Write is the file `cat`
//! shows, wherever the host keeps it. [`SandboxWorkspace::with_command`] adds a
//! command to the shell (haest-code's `js`). Every sandbox has `xan` (CSV verbs,
//! `xan/`) beyond bashkit's builtins.

mod commands;
mod exec;
mod mounts;
mod spelled_fs;
mod workspace;
mod xan;

use std::{fmt, sync::Arc};

use haest_core::{
    ChildEnv, fs::FsRef, remedy::Remedies, shell_command::ShellCommandRef, workspace::ChangeSetRef,
};

pub use mounts::{OpenSandboxError, SandboxMounts};

use crate::bridge::{BashkitFsOverHaest, HaestFsOverBashkit};
use exec::Shell;

/// The commands every sandbox has beyond bashkit's builtins (`xan`). A host's
/// command of the same name replaces one.
fn own_commands() -> Vec<ShellCommandRef> {
    vec![xan::Xan.into()]
}

/// bashkit, a simulated bash, over files the host chooses: the project as a
/// copy-on-write overlay ([`open`](Self::open)), or the host's own [`Fs`](haest_core::fs::Fs)
/// ([`over`](Self::over)).
///
/// The tools ([`Workspace::fs`]) and the shell see the same files: a file
/// written with the Write tool is the file `cat` shows.
///
/// Each [`shell`][shell] call gets a FRESH shell over those files: variables,
/// functions and `cd` do not outlive the call, files do. [`run`][run] runs
/// its argv as `"$@"`, with no shell parsing; an empty argv is exit 127. A
/// call ends the way a real shell reports it:
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
/// Over the project, on a disk that reads two spellings of a name as one
/// (macOS's default reads `README.md` as `readme.md`; APFS, the
/// case-sensitive kind too, reads `caf\u{e9}` and `cafe\u{301}` as one; so
/// do exFAT, FAT, and on Linux a case-folding directory, `chattr +F`), a
/// path is found only as the disk spells it: `cat README.md` does not find
/// `readme.md`, where real bash would find it, and `echo x > README.md`
/// makes a second file beside it. The model reads the disk's spelling from
/// `ls`. So the sandbox shows what applying writes: `mv lib Lib` is a rename,
/// in the sandbox and on the disk, and a write that would land on a name the
/// disk holds under another spelling, a file or a directory on its way, is a
/// conflict until a delete frees that name
/// ([`OverlayChanges`](crate::OverlayChanges)). The names in a directory the
/// sandbox may enter but not list (mode `0311`) are not told apart: asked
/// under another spelling, or through a symlink, they read as the disk finds
/// them.
///
/// # Examples
///
/// A host's variable, and a command added to the shell:
///
/// ```
/// use std::time::Duration;
///
/// use async_trait::async_trait;
/// use haest_core::{
///     ChildEnv,
///     fs::MemFs,
///     shell_command::{ShellCall, ShellCommand},
///     workspace::{ExecOptions, ExecResult, Workspace},
/// };
/// use haest_workspace::SandboxWorkspace;
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
/// let ws = SandboxWorkspace::over(MemFs::new())
///     .with_env(ChildEnv::new([("GREETING".to_string(), "hej".to_string())]))
///     .with_command(Shout);
/// let opts = ExecOptions::new("/", Duration::from_secs(10));
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
/// [shell]: haest_core::workspace::Workspace::shell
/// [run]: haest_core::workspace::Workspace::run
pub struct SandboxWorkspace {
    /// What the shell runs over.
    shell_fs: Arc<dyn bashkit::FileSystem>,
    /// What the tools see: the same files.
    fs: FsRef,
    /// A host's replacement for the built note.
    note: Option<String>,
    /// The host's way to review and apply the overlay, for the note.
    remedies: Remedies,
    commands: Vec<ShellCommandRef>,
    /// The variables the host passes the shell.
    env: ChildEnv,
    changes: Option<ChangeSetRef>,
}

impl SandboxWorkspace {
    /// The default sandbox: a copy-on-write overlay of `mounts.cwd`, the real
    /// writable and readable dirs at their own paths, a scratch disk elsewhere.
    /// A relative directory is taken from this process's current directory,
    /// now.
    ///
    /// Its [`Workspace::changes`] is the overlay's
    /// [`OverlayChanges`](crate::OverlayChanges): what the human reviews and
    /// applies.
    ///
    /// [`Workspace::changes`]: haest_core::workspace::Workspace::changes
    ///
    /// # Errors
    ///
    /// When the project or a mounted directory does not exist or cannot be
    /// opened, is an empty path, or is relative and the current directory
    /// cannot be read: the [`OpenSandboxError`] names it.
    pub async fn open(mounts: SandboxMounts) -> Result<Self, OpenSandboxError> {
        let mounts = mounts.absolute()?;
        let changes = mounts::project_overlay(&mounts.cwd).await?;
        let shell_fs = mounts::sandbox_fs(&mounts, &changes).await?;
        let fs = FsRef::new(HaestFsOverBashkit::new(shell_fs.clone()));
        Ok(Self::from_parts(shell_fs, fs, Some(changes.into())))
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
    ///
    /// `fs` is the files themselves, or a handle the host keeps a clone of
    /// ([`FsRef`]) to read what the agent wrote.
    pub fn over(fs: impl Into<FsRef>) -> Self {
        let fs = fs.into();
        Self::from_parts(Arc::new(BashkitFsOverHaest::new(fs.clone())), fs, None)
    }

    /// A sandbox over the shell's file system, the tools' one and the
    /// changes to review, with no commands but its own.
    fn from_parts(
        shell_fs: Arc<dyn bashkit::FileSystem>,
        fs: FsRef,
        changes: Option<ChangeSetRef>,
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
    #[must_use]
    pub fn with_env(mut self, env: ChildEnv) -> Self {
        self.env = env;
        self
    }

    /// Replace the note the system prompt carries about this workspace
    /// ([`Workspace::prompt_note`]) with `note`, the added commands' notes
    /// included.
    ///
    /// [`Workspace::prompt_note`]: haest_core::workspace::Workspace::prompt_note
    #[must_use]
    pub fn with_prompt_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// The host's way to review the overlay's changes and put them on disk
    /// ([`Limit::SandboxEdits`]), added to the prompt note's part about them:
    /// [`Remedies`], or the closure that writes them.
    ///
    /// [`Limit::SandboxEdits`]: haest_core::remedy::Limit::SandboxEdits
    #[must_use]
    pub fn with_remedies(mut self, remedies: impl Into<Remedies>) -> Self {
        self.remedies = remedies.into();
        self
    }

    /// Add a command to the shell (haest-code's `js`). Its note, if any, joins
    /// the prompt note. A name the shell already has (a builtin, `xan`, an
    /// earlier command): this one wins.
    ///
    /// The command runs inside the shell call it is part of, so pipes, `&&`
    /// and redirects work, and it gets that call's cancel token and deadline.
    #[must_use]
    pub fn with_command(mut self, command: impl Into<ShellCommandRef>) -> Self {
        let command = command.into();
        self.commands.retain(|c| c.name() != command.name());
        self.commands.push(command);
        self
    }

    /// Add each command, in order, as [`with_command`](Self::with_command)
    /// does: haest-code's `sandbox_commands()` in one call.
    #[must_use]
    pub fn with_commands(
        self,
        commands: impl IntoIterator<Item = impl Into<ShellCommandRef>>,
    ) -> Self {
        commands.into_iter().fold(self, Self::with_command)
    }

    fn shell_world(&self) -> Shell<'_> {
        Shell {
            fs: self.shell_fs.clone(),
            haest_fs: &self.fs,
            commands: &self.commands,
            env: &self.env,
        }
    }
}

impl fmt::Debug for SandboxWorkspace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let commands: Vec<&str> = self.commands.iter().map(|c| c.name()).collect();
        f.debug_struct("SandboxWorkspace")
            .field("commands", &commands)
            .field("env", &self.env)
            .field("overlay", &self.changes.is_some())
            .finish_non_exhaustive()
    }
}
