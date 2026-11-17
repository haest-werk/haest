//! A command a host adds to a SIMULATED shell: the sandbox's `js` (haest-code).
//! A real shell runs real programs and needs none of this; a simulated one knows
//! only its builtins, so a host hands it more here and the sandbox runs each one
//! inside the shell call it is part of (pipes, `&&`, redirects all work).
//!
//! PLUG POINT: implement [`ShellCommand`] and give it to a sandbox
//! (`haest_workspace::SandboxWorkspace::with_command`). Each run gets a
//! [`ShellCall`] and answers with an [`ExecResult`], as a program would.

use std::{borrow::Cow, collections::BTreeMap, path::PathBuf, time::Instant};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{fs::FsRef, workspace::ExecResult};

/// One run of a [`ShellCommand`]: its arguments and everything of the shell
/// call it is part of.
///
/// The sandbox builds it ([`ShellCall::new`], a test too); a command reads
/// the fields.
#[non_exhaustive]
pub struct ShellCall {
    /// The arguments, not including the command's own name.
    pub args: Vec<String>,
    /// The shell's working directory, absolute. Resolve a relative path
    /// argument against it ([`crate::fs::resolve_path`]).
    pub cwd: PathBuf,
    /// The shell's exported variables.
    pub env: BTreeMap<String, String>,
    /// What a pipe feeds it (`a | cmd`), byte for byte (a document piped into a
    /// converter is binary); None when nothing does. [`ShellCall::stdin_text`] for text.
    pub stdin: Option<Vec<u8>>,
    /// The shell's files, as haest's tools see them.
    pub fs: FsRef,
    /// The shell call's interrupt.
    pub cancel: CancellationToken,
    /// The shell call's deadline. A CPU-bound command never yields to a timer,
    /// so it checks this clock itself.
    pub ends_at: Instant,
}

impl ShellCall {
    /// A run with `args`, in `cwd` over `fs`, until `ends_at`: no variables,
    /// nothing piped in, a fresh interrupt.
    pub fn new(
        args: impl IntoIterator<Item = impl Into<String>>,
        cwd: impl Into<PathBuf>,
        fs: impl Into<FsRef>,
        ends_at: Instant,
    ) -> Self {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            cwd: cwd.into(),
            env: BTreeMap::new(),
            stdin: None,
            fs: fs.into(),
            cancel: CancellationToken::new(),
            ends_at,
        }
    }

    /// With the shell's exported variables.
    #[must_use]
    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// With `stdin` piped in.
    #[must_use]
    pub fn with_stdin(mut self, stdin: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(stdin.into());
        self
    }

    /// Interrupted by `cancel`.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// The stdin as text (invalid UTF-8 replaced).
    pub fn stdin_text(&self) -> Option<Cow<'_, str>> {
        self.stdin.as_deref().map(String::from_utf8_lossy)
    }
}

/// A command a simulated shell runs in-process, by name, like a program.
///
/// The sandbox calls [`ShellCommand::run`] inside the shell call, so the
/// command's output goes through pipes and redirects like any program's.
/// Only `stdout`, `stderr` and `exit_code` of the result are used.
///
/// Cancellation: the shell call may be dropped at any `.await` when it is
/// interrupted or runs out of time. Work that does not await (a long
/// computation) checks [`ShellCall::cancel`] and [`ShellCall::ends_at`]
/// itself.
///
/// # Examples
///
/// ```
/// use std::time::{Duration, Instant};
///
/// use async_trait::async_trait;
/// use haest_core::{
///     ExecResult, MemFs,
///     shell_command::{ShellCall, ShellCommand},
/// };
///
/// /// `upper`: stdin, or the arguments, in capitals.
/// struct Upper;
///
/// #[async_trait]
/// impl ShellCommand for Upper {
///     fn name(&self) -> &str {
///         "upper"
///     }
///     fn note(&self) -> Option<String> {
///         Some("`upper [text]` prints stdin or its arguments in capitals.".into())
///     }
///     async fn run(&self, call: ShellCall) -> ExecResult {
///         let text = match call.stdin_text() {
///             Some(stdin) => stdin.into_owned(),
///             None => call.args.join(" "),
///         };
///         ExecResult { stdout: text.to_uppercase(), ..Default::default() }
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let deadline = Instant::now() + Duration::from_secs(10);
/// let call = ShellCall::new(Vec::<String>::new(), "/project", MemFs::new(), deadline)
///     .with_stdin(b"hello\n".as_slice());
/// let out = Upper.run(call).await;
/// assert_eq!((out.stdout.as_str(), out.exit_code), ("HELLO\n", 0));
/// # }
/// ```
#[async_trait]
pub trait ShellCommand: Send + Sync {
    /// The name the shell runs it by. In the sandbox it replaces a builtin or
    /// an earlier command of the same name.
    fn name(&self) -> &str;
    /// What the model must know about it, for the workspace's system prompt
    /// note. None: it is not advertised (a command that only answers a habit,
    /// say `node`, with a pointer elsewhere).
    fn note(&self) -> Option<String> {
        None
    }
    /// Stdout, stderr and the exit code, as a program would end. `timed_out` is the shell's to set.
    ///
    /// Never fails: a failure is a nonzero exit code and a reason on stderr.
    async fn run(&self, call: ShellCall) -> ExecResult;
}

crate::plug_ref! {
    /// A [`ShellCommand`], shared: what a sandbox holds for each command it
    /// was given.
    pub struct ShellCommandRef(dyn ShellCommand);
}
