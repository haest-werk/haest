//! The real workspace: the host's disk and the host's bash. The opt-out from
//! the sandbox, and always what the HOST itself uses for its own files (session
//! log, config) -- through the same [`Fs`] interface as everything else
//! ([`RealFs`]).
//!
//! Children get their own process group so a timeout or an interrupt kills the
//! whole tree, not just bash. Every child runs with the [`ChildEnv`] the host
//! handed in and nothing else; a program is found on THAT env's `PATH`.

mod exec;
mod fs;

use std::sync::Arc;

use async_trait::async_trait;
use haest_core::{
    ChildEnv,
    fs::Fs,
    workspace::{EXIT_NOT_FOUND, ExecOptions, ExecResult, OutputChunks, Workspace, WorkspaceMode},
};

pub use fs::RealFs;

/// The real disk and the real `bash`: the workspace where the agent's edits
/// land on disk at once and its commands are real programs.
///
/// - [`Workspace::shell`] runs `bash -c <command>`; [`Workspace::run`] runs a
///   program with its argv, no shell parsing. A bare program name is looked up
///   on the `PATH` in the [`ChildEnv`] (`/usr/bin:/bin` when it has none), a
///   name with a `/` is taken from the call's `cwd`. Not found: exit 127 and
///   `<program>: command not found` on stderr. An empty argv: exit 127.
/// - A child gets the [`ChildEnv`] and nothing else of the host's environment,
///   plus `TERM=dumb` and `NO_COLOR=1`. Its stdin is empty.
/// - A child leads its own process group. The call's timeout and its cancel
///   token both kill the whole group (`npm run dev` spawning node spawning
///   esbuild), as does dropping the call's future before it ends. A timeout
///   sets [`ExecResult::timed_out`]; a killed child reports exit `128 + 9`.
/// - A call ends when the child has exited AND its output pipes have closed:
///   a command that leaves a process behind holding them (`server &`) runs
///   until that process closes them or the timeout kills the group.
/// - Output is decoded as UTF-8, invalid bytes replaced.
///
/// [`Workspace::prompt_note`] is empty: the real world needs no explaining.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use haest_core::{
///     ChildEnv,
///     workspace::{ExecOptions, Workspace},
/// };
/// use haest_workspace::RealWorkspace;
/// use tokio_util::sync::CancellationToken;
///
/// # #[tokio::main]
/// # async fn main() {
/// // The children see this PATH and no other variable of the host's.
/// let env = ChildEnv::new([("PATH".to_string(), "/usr/bin:/bin".to_string())]);
/// let ws = RealWorkspace::new(env);
/// let opts = ExecOptions {
///     cwd: std::env::temp_dir(),
///     cancel: CancellationToken::new(),
///     timeout: Duration::from_secs(10),
/// };
/// let r = ws.shell("echo \"HOME is [$HOME]\"", opts.clone()).await;
/// assert_eq!(r.stdout, "HOME is []\n");
///
/// let r = ws.run(&["no-such-program".to_string()], opts).await;
/// assert_eq!(r.exit_code, 127);
/// # }
/// ```
///
/// [`Workspace`]: haest_core::workspace::Workspace
/// [`Workspace::shell`]: haest_core::workspace::Workspace::shell
/// [`Workspace::run`]: haest_core::workspace::Workspace::run
/// [`Workspace::prompt_note`]: haest_core::workspace::Workspace::prompt_note
/// [`ExecResult::timed_out`]: haest_core::workspace::ExecResult::timed_out
pub struct RealWorkspace {
    fs: Arc<dyn Fs>,
    env: ChildEnv,
}

impl RealWorkspace {
    /// The real disk through a fresh [`RealFs`]; every child process gets `env`
    /// and nothing else.
    pub fn new(env: ChildEnv) -> Self {
        Self::with_fs(Arc::new(RealFs), env)
    }

    /// The same, with the host's own [`Fs`] for the files, so the host and the
    /// tools share one instance. `fs` should be the real disk: the shell
    /// always is.
    pub fn with_fs(fs: Arc<dyn Fs>, env: ChildEnv) -> Self {
        Self { fs, env }
    }

    /// `program args...`, found on the handed env's `PATH`.
    async fn spawn(
        &self,
        program: &str,
        args: &[String],
        opts: ExecOptions,
        stream: Option<OutputChunks>,
    ) -> ExecResult {
        match exec::which(program, &opts.cwd, &self.env) {
            Some(bin) => exec::spawn_collect(&bin, args, opts, stream, &self.env).await,
            None => ExecResult {
                stderr: format!("{program}: command not found"),
                exit_code: EXIT_NOT_FOUND,
                ..Default::default()
            },
        }
    }

    async fn bash(
        &self,
        command: &str,
        opts: ExecOptions,
        stream: Option<OutputChunks>,
    ) -> ExecResult {
        let args = ["-c".to_string(), command.to_string()];
        self.spawn("bash", &args, opts, stream).await
    }
}

#[async_trait]
impl Workspace for RealWorkspace {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Real
    }

    fn fs(&self) -> Arc<dyn Fs> {
        self.fs.clone()
    }

    async fn shell(&self, command: &str, opts: ExecOptions) -> ExecResult {
        self.bash(command, opts, None).await
    }

    /// The output streams straight from the pipes, nothing is kept: a
    /// background job's dev server may print for hours.
    async fn shell_streaming(
        &self,
        command: &str,
        opts: ExecOptions,
        on_output: OutputChunks,
    ) -> ExecResult {
        self.bash(command, opts, Some(on_output)).await
    }

    async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult {
        match argv.split_first() {
            Some((program, args)) => self.spawn(program, args, opts, None).await,
            None => crate::no_command(),
        }
    }

    fn prompt_note(&self) -> String {
        String::new()
    }
}
