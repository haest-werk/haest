//! The real workspace: the host's disk and the host's bash. The opt-out from
//! the sandbox, and always what the HOST itself uses for its own files (session
//! log, config) -- through the same [`Fs`] interface as everything else.
//!
//! Children get their own process group so a timeout or an interrupt kills the
//! whole tree, not just bash. Every child runs with the [`ChildEnv`] the host
//! handed in and nothing else; a program is found on THAT env's `PATH`.

mod exec;
mod fs;

use std::sync::Arc;

use async_trait::async_trait;
use kern_core::{
    ChildEnv,
    fs::Fs,
    workspace::{EXIT_NOT_FOUND, ExecOptions, ExecResult, OutputChunks, Workspace, WorkspaceMode},
};

pub use exec::kill_group;
use exec::{Output, which};
pub use fs::RealFs;

pub struct RealWorkspace {
    fs: Arc<dyn Fs>,
    env: ChildEnv,
}

impl RealWorkspace {
    /// The real disk through a fresh [`RealFs`]; children get `env`.
    pub fn new(env: ChildEnv) -> Self {
        Self::with_fs(Arc::new(RealFs), env)
    }

    /// Pass the host's own real [`Fs`] to share one instance.
    pub fn with_fs(fs: Arc<dyn Fs>, env: ChildEnv) -> Self {
        Self { fs, env }
    }

    fn not_found(program: &str) -> ExecResult {
        ExecResult {
            stderr: format!("{program}: command not found"),
            exit_code: EXIT_NOT_FOUND,
            ..Default::default()
        }
    }

    async fn bash(&self, command: &str, opts: ExecOptions, output: Output) -> ExecResult {
        let Some(bash) = which("bash", &opts.cwd, &self.env) else {
            return Self::not_found("bash");
        };
        let args = ["-c".to_string(), command.to_string()];
        exec::spawn_collect(&bash, &args, opts, output, &self.env).await
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
        self.bash(command, opts, Output::Collect).await
    }

    /// The output streams straight from the pipes, nothing is kept: a
    /// background job's dev server may print for hours.
    async fn shell_streaming(
        &self,
        command: &str,
        opts: ExecOptions,
        on_output: OutputChunks,
    ) -> ExecResult {
        self.bash(command, opts, Output::Stream(on_output)).await
    }

    async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult {
        let Some((program, args)) = argv.split_first() else {
            return ExecResult {
                stderr: "no command given".into(),
                exit_code: EXIT_NOT_FOUND,
                ..Default::default()
            };
        };
        match which(program, &opts.cwd, &self.env) {
            Some(bin) => exec::spawn_collect(&bin, args, opts, Output::Collect, &self.env).await,
            None => Self::not_found(program),
        }
    }

    fn prompt_note(&self) -> String {
        String::new()
    }
}
