//! Bash: run one shell command in the session working directory, through the
//! workspace -- the real bash, or the sandbox's simulated one ([`BashTool`]).
//!
//! With a job hub ([`BashOptions::with_jobs`]) it also takes `run_in_background`:
//! the command keeps running and the call returns at once ([`crate::jobs`]);
//! BashOutput and KillShell ([`crate::bash_jobs`]) read and stop it.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use haest_core::{
    tool::{
        JsonMap, TRUNCATE_CHARS, Tool, ToolContext, ToolError, ToolOutput, opt_bool, opt_count,
        opt_str, req_str, truncate_middle,
    },
    workspace::ExecOptions,
};
use serde_json::{Value, json};

use crate::{
    canonical::pick,
    jobs::{JobError, JobHub, NewJob},
};

const MIN_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_TIMEOUT: Duration = Duration::from_secs(600);

/// What a host sets on the Bash tool.
///
/// The default: a 120 s timeout, no background jobs.
///
/// ```
/// use std::time::Duration;
///
/// use haest_tools::{BashOptions, BashTool, JobHub, NoInbox};
///
/// let jobs = JobHub::new(NoInbox);
/// let bash = BashTool::new(
///     BashOptions::default()
///         .with_default_timeout(Duration::from_secs(30))
///         .with_jobs(jobs.clone()),
/// );
/// # let _ = bash;
/// ```
#[derive(Debug, Clone)]
pub struct BashOptions {
    default_timeout: Duration,
    jobs: Option<JobHub>,
}

impl Default for BashOptions {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(120),
            jobs: None,
        }
    }
}

impl BashOptions {
    /// The timeout when a call names none (clamped to 1 s..600 s).
    #[must_use]
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Background jobs on `jobs`: Bash takes `run_in_background`, and
    /// [`builtin_tools`](crate::builtin_tools) adds BashOutput and KillShell.
    /// Without one, neither.
    #[must_use]
    pub fn with_jobs(mut self, jobs: JobHub) -> Self {
        self.jobs = Some(jobs);
        self
    }

    /// The timeout when a call names none, as set.
    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }

    /// The job hub, if any.
    pub fn jobs(&self) -> Option<&JobHub> {
        self.jobs.as_ref()
    }
}

/// The Bash tool (`Bash`, kind `shell.exec`): one command line in the
/// workspace's shell ([`Workspace::shell`](haest_core::workspace::Workspace::shell)),
/// in the context's `cwd`.
///
/// Input: `command`, and optional `description` and `timeout` (ms, clamped to
/// 1 s..600 s; default [`BashOptions::default_timeout`]). With a job hub it
/// also takes `run_in_background`, which starts a [`JobHub`] job and returns
/// at once with its id.
///
/// The model gets stdout then stderr, kept to haest-core's
/// [`TRUNCATE_CHARS`] (the middle cut), with a note when the
/// timeout killed it, when the call was interrupted, or when it exited
/// non-zero; any of those makes the result an error. The transcript gets a
/// `shell` envelope: stdout, stderr, exit code, duration. Each call is a fresh
/// shell: `cd` does not persist.
///
/// # Examples
///
/// ```
/// use haest_core::{fs::MemFs, tool::{JsonMap, Tool, ToolContext}};
/// use haest_tools::BashTool;
/// use haest_workspace::SandboxWorkspace;
/// use serde_json::json;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// let ctx = ToolContext::new("/", SandboxWorkspace::over(MemFs::new()));
/// let bash = BashTool::default();
/// let input: JsonMap = serde_json::from_value(json!({"command": "echo hi; exit 3"}))?;
/// let out = bash.execute(input, &ctx).await?;
/// assert!(out.is_error);
/// assert_eq!(out.content, "hi\n\n[exit code 3]");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct BashTool {
    default_timeout: Duration,
    jobs: Option<JobHub>,
}

impl BashTool {
    /// Bash with `opts`; the default timeout is clamped to 1 s..600 s.
    pub fn new(opts: BashOptions) -> Self {
        Self {
            default_timeout: opts.default_timeout.clamp(MIN_TIMEOUT, MAX_TIMEOUT),
            jobs: opts.jobs,
        }
    }

    /// The call's timeout: its own (in ms) or the default, always within 1s..600s.
    fn timeout(&self, input: &JsonMap) -> Result<Duration, ToolError> {
        let asked =
            opt_count(input, "timeout")?.map_or(self.default_timeout, Duration::from_millis);
        Ok(asked.clamp(MIN_TIMEOUT, MAX_TIMEOUT))
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new(BashOptions::default())
    }
}

/// Run the command now and wait for it.
async fn run_now(
    command: &str,
    timeout: Duration,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    let started = Instant::now();
    let opts = ExecOptions::new(ctx.cwd(), timeout).with_cancel(ctx.cancel().clone());
    let r = ctx.workspace().shell(command, opts).await;
    let sep = if !r.stdout.is_empty() && !r.stderr.is_empty() {
        "\n"
    } else {
        ""
    };
    let (text, truncated) =
        truncate_middle(&format!("{}{sep}{}", r.stdout, r.stderr), TRUNCATE_CHARS);
    let note = if r.timed_out {
        format!("\n[killed after {}ms timeout]", timeout.as_millis())
    } else if ctx.cancel().is_cancelled() {
        "\n[interrupted]".to_string()
    } else {
        String::new()
    };
    let exit = if r.exit_code != 0 {
        format!("\n[exit code {}]", r.exit_code)
    } else {
        String::new()
    };
    let body = if text.is_empty() {
        "(no output)"
    } else {
        &text
    };
    Ok(ToolOutput {
        content: format!("{body}{note}{exit}"),
        // A command killed by its timeout did not succeed, whatever exit code the workspace saw.
        is_error: r.exit_code != 0 || r.timed_out,
        result: Some(json!({
            "kind": "shell",
            "stdout": truncate_middle(&r.stdout, TRUNCATE_CHARS).0,
            "stderr": truncate_middle(&r.stderr, TRUNCATE_CHARS).0,
            "exitCode": r.exit_code,
            "truncated": truncated,
            "durationMs": started.elapsed().as_millis() as u64,
        })),
        ..Default::default()
    })
}

/// Start the command as a background job and return at once.
fn run_in_background(
    jobs: &JobHub,
    command: &str,
    input: &JsonMap,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    let mut job = NewJob::new(command);
    if let Some(description) = opt_str(input, "description")? {
        job = job.with_description(description);
    }
    let job = jobs
        .start(job, ctx)
        .map_err(|e: JobError| ToolError::Failed(e.to_string()))?;
    Ok(ToolOutput::text(format!(
        "Started background job {id}. Read its output with BashOutput (bash_id \"{id}\"), stop it with KillShell. You will be told when it exits -- do not poll in a loop.",
        id = job.id
    )))
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }
    fn kind(&self) -> &str {
        "shell.exec"
    }
    fn description(&self) -> String {
        let background = if self.jobs.is_some() {
            " Set run_in_background for a command that should keep running (a server, a watcher)."
        } else {
            ""
        };
        format!(
            "Run a bash command in the working directory and return combined stdout+stderr and the exit code. Each call is a fresh shell (cd does not persist). Default timeout {}s, max {}s.{background} Prefer Read/Grep/Glob for reading and searching files.",
            self.default_timeout.as_secs(),
            MAX_TIMEOUT.as_secs()
        )
    }
    fn parameters(&self) -> Value {
        let mut p = json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The command to run"},
                "description": {"type": "string", "description": "Short description of what the command does"},
                "timeout": {"type": "number", "description": format!("Timeout in milliseconds (max {})", MAX_TIMEOUT.as_millis())}
            },
            "required": ["command"]
        });
        if self.jobs.is_some() {
            p["properties"]["run_in_background"] = json!({
                "type": "boolean",
                "description": "Start the command and return at once (dev servers, long builds, watchers)"
            });
        }
        p
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        pick(
            input,
            &[
                ("command", "command"),
                ("description", "description"),
                ("timeoutMs", "timeout"),
            ],
        )
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let command = req_str(&input, "command")?;
        let background = opt_bool(&input, "run_in_background")?;
        match &self.jobs {
            Some(jobs) if background => run_in_background(jobs, command, &input, ctx),
            _ => run_now(command, self.timeout(&input)?, ctx).await,
        }
    }
}

#[cfg(test)]
mod tests;
