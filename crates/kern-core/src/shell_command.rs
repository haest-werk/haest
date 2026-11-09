//! A command a host adds to a SIMULATED shell: the sandbox's `js` (kern-code).
//! A real shell runs real programs and needs none of this; a simulated one knows
//! only its builtins, so a host hands it more here and the sandbox runs each one
//! inside the shell call it is part of (pipes, `&&`, redirects all work).
//!
//! PLUG POINT: implement [`ShellCommand`] and give it to a sandbox
//! (`kern_workspace::SandboxWorkspace::with_command`).

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Instant};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{fs::Fs, workspace::ExecResult};

/// One run of a [`ShellCommand`].
pub struct ShellCall {
    /// The arguments, not including the command's own name.
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The shell's exported variables.
    pub env: BTreeMap<String, String>,
    /// What a pipe feeds it (`a | cmd`); None when nothing does.
    pub stdin: Option<String>,
    /// The shell's files, as kern's tools see them.
    pub fs: Arc<dyn Fs>,
    /// The shell call's interrupt.
    pub cancel: CancellationToken,
    /// The shell call's deadline. A CPU-bound command never yields to a timer,
    /// so it checks this clock itself.
    pub ends_at: Instant,
}

#[async_trait]
pub trait ShellCommand: Send + Sync {
    /// The name the shell runs it by.
    fn name(&self) -> &str;
    /// What the model must know about it, for the workspace's system prompt
    /// note. None: it is not advertised (a command that only answers a habit,
    /// say `node`, with a pointer elsewhere).
    fn note(&self) -> Option<String> {
        None
    }
    /// Stdout, stderr and the exit code, as a program would end. `timed_out` is the shell's to set.
    async fn run(&self, call: ShellCall) -> ExecResult;
}
