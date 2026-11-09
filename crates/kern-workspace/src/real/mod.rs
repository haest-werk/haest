//! The real workspace: the host's disk and the host's bash. The opt-out from
//! the sandbox, and always what the HOST itself uses for its own files (session
//! log, config) -- through the same [`Fs`] interface as everything else.
//!
//! Children get their own process group so a timeout or an interrupt kills the
//! whole tree, not just bash.

mod exec;
mod fs;

use std::sync::Arc;

use async_trait::async_trait;
use kern_core::{
    fs::Fs,
    workspace::{EXIT_NOT_FOUND, ExecOptions, ExecResult, OutputChunks, Workspace, WorkspaceMode},
};

pub use exec::kill_group;
use exec::{Output, which};
pub use fs::RealFs;

pub struct RealWorkspace {
    fs: Arc<dyn Fs>,
}

impl RealWorkspace {
    /// The real disk through a fresh [`RealFs`].
    pub fn new() -> Self {
        Self::with_fs(Arc::new(RealFs))
    }

    /// Pass the host's own real [`Fs`] to share one instance.
    pub fn with_fs(fs: Arc<dyn Fs>) -> Self {
        Self { fs }
    }
}

impl Default for RealWorkspace {
    fn default() -> Self {
        Self::new()
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
        let args = ["-c".to_string(), command.to_string()];
        exec::spawn_collect("bash".as_ref(), &args, opts, Output::Collect).await
    }

    /// The output streams straight from the pipes, nothing is kept: a
    /// background job's dev server may print for hours.
    async fn shell_streaming(
        &self,
        command: &str,
        opts: ExecOptions,
        on_output: OutputChunks,
    ) -> ExecResult {
        let args = ["-c".to_string(), command.to_string()];
        exec::spawn_collect("bash".as_ref(), &args, opts, Output::Stream(on_output)).await
    }

    async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult {
        let Some((program, args)) = argv.split_first() else {
            return ExecResult {
                stderr: "no command given".into(),
                exit_code: EXIT_NOT_FOUND,
                ..Default::default()
            };
        };
        match which(program, &opts.cwd) {
            Some(bin) => exec::spawn_collect(&bin, args, opts, Output::Collect).await,
            None => ExecResult {
                stderr: format!("{program}: command not found"),
                exit_code: EXIT_NOT_FOUND,
                ..Default::default()
            },
        }
    }

    fn prompt_note(&self) -> String {
        String::new()
    }
}
