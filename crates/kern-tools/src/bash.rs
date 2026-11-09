//! Bash: run one shell command in the session working directory, through the
//! workspace -- the real bash, or the sandbox's simulated one.
//!
//! SEAM: background jobs (`run_in_background`, and the BashOutput / KillShell
//! tools) come with a job manager in a later wave. They add one property to
//! [`BashTool::parameters`], one sentence to the description and one branch in
//! `execute` in front of [`run_now`]; nothing else here changes.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use kern_core::{
    tool::{
        JsonMap, TRUNCATE_CHARS, Tool, ToolContext, ToolError, ToolOutput, opt_num, req_str,
        truncate_middle,
    },
    workspace::ExecOptions,
};
use serde_json::{Value, json};

use crate::canonical::pick;

const MIN_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct BashOptions {
    /// The timeout when a call names none (clamped to 1s..600s).
    pub default_timeout: Duration,
}

impl Default for BashOptions {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(120),
        }
    }
}

pub struct BashTool {
    default_timeout: Duration,
}

impl BashTool {
    pub fn new(opts: BashOptions) -> Self {
        Self {
            default_timeout: opts.default_timeout.clamp(MIN_TIMEOUT, MAX_TIMEOUT),
        }
    }

    /// The call's timeout: its own (in ms) or the default, always within 1s..600s.
    fn timeout(&self, input: &JsonMap) -> Result<Duration, ToolError> {
        let asked = match opt_num(input, "timeout")? {
            // `as` saturates: a negative or huge number lands on a bound below.
            Some(ms) => Duration::from_millis(ms.floor() as u64),
            None => self.default_timeout,
        };
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
    let opts = ExecOptions {
        cwd: ctx.cwd.clone(),
        cancel: ctx.cancel.clone(),
        timeout,
    };
    let r = ctx.workspace.shell(command, opts).await;
    let sep = if !r.stdout.is_empty() && !r.stderr.is_empty() {
        "\n"
    } else {
        ""
    };
    let (text, truncated) =
        truncate_middle(&format!("{}{sep}{}", r.stdout, r.stderr), TRUNCATE_CHARS);
    let note = if r.timed_out {
        format!("\n[killed after {}ms timeout]", timeout.as_millis())
    } else if ctx.cancel.is_cancelled() {
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

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }
    fn kind(&self) -> &str {
        "shell.exec"
    }
    fn description(&self) -> String {
        format!(
            "Run a bash command in the working directory and return combined stdout+stderr and the exit code. Each call is a fresh shell (cd does not persist). Default timeout {}s, max {}s. Prefer Read/Grep/Glob for reading and searching files.",
            self.default_timeout.as_secs(),
            MAX_TIMEOUT.as_secs()
        )
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The command to run"},
                "description": {"type": "string", "description": "Short description of what the command does"},
                "timeout": {"type": "number", "description": format!("Timeout in milliseconds (max {})", MAX_TIMEOUT.as_millis())}
            },
            "required": ["command"]
        })
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
        run_now(command, self.timeout(&input)?, ctx).await
    }
}

#[cfg(test)]
mod tests;
