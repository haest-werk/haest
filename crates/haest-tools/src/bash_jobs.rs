//! BashOutput / KillShell: reading and stopping background jobs
//! ([`crate::jobs`]). [`job_tools`] makes both over one [`JobHub`];
//! [`builtin_tools`](crate::builtin_tools) adds them after Bash when
//! [`BashOptions::jobs`](crate::BashOptions::jobs) is set.
//!
//! Keys follow the common agent-tool convention (`bash_id`, `shell_id`), so
//! models already know the calls.

use std::sync::Arc;

use async_trait::async_trait;
use haest_core::tool::{
    JsonMap, TRUNCATE_CHARS, Tool, ToolContext, ToolError, ToolOutput, req_str, truncate_middle,
};
use serde_json::{Value, json};

use crate::jobs::{JobHub, MAX_BUFFER};

/// No job `id`: the error names the ones running.
fn unknown(jobs: &JobHub, id: &str) -> ToolError {
    let running: Vec<String> = jobs.running().into_iter().map(|j| j.id).collect();
    let running = if running.is_empty() {
        "none".to_string()
    } else {
        running.join(", ")
    };
    ToolError::Input(format!("no background job \"{id}\"; running: {running}"))
}

fn id_schema(key: &str) -> Value {
    json!({
        "type": "object",
        "properties": {key: {"type": "string", "description": "The job id, e.g. \"b1\""}},
        "required": [key]
    })
}

/// The BashOutput tool (`BashOutput`, kind `shell.exec`): a background job's
/// output since the model last looked, and how the job stands. Made by
/// [`job_tools`].
///
/// Input: `bash_id`. Reading marks the output read (so this is not
/// read-only). When the job printed more than the 1 MB buffer holds between
/// two looks, a note says how many bytes were dropped. An unknown id is an
/// input error that lists the running jobs.
pub struct BashOutputTool {
    jobs: Arc<JobHub>,
}

#[async_trait]
impl Tool for BashOutputTool {
    fn name(&self) -> &str {
        "BashOutput"
    }
    fn kind(&self) -> &str {
        "shell.exec"
    }
    fn description(&self) -> String {
        "New output of a background job since you last read it, and whether it is still running."
            .into()
    }
    fn parameters(&self) -> Value {
        id_schema("bash_id")
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = req_str(&input, "bash_id")?;
        let got = self.jobs.take(id).ok_or_else(|| unknown(&self.jobs, id))?;
        let missed = if got.missed > 0 {
            format!(
                "[{} bytes dropped: the buffer keeps the last {MAX_BUFFER}]\n",
                got.missed
            )
        } else {
            String::new()
        };
        let body = if got.fresh.is_empty() {
            "(no new output)".to_string()
        } else {
            truncate_middle(&got.fresh, TRUNCATE_CHARS).0
        };
        Ok(ToolOutput::text(format!(
            "Job {id} ({}): {}\n{missed}{body}",
            got.job.command,
            got.job.state()
        )))
    }
}

/// The KillShell tool (`KillShell`, kind `shell.exec`): stops a background job
/// and everything it started ([`JobHub::kill`]). Made by [`job_tools`].
///
/// Input: `shell_id`. A job that already ended is reported as such; an
/// unknown id is an input error that lists the running jobs.
pub struct KillShellTool {
    jobs: Arc<JobHub>,
}

#[async_trait]
impl Tool for KillShellTool {
    fn name(&self) -> &str {
        "KillShell"
    }
    fn kind(&self) -> &str {
        "shell.exec"
    }
    fn description(&self) -> String {
        "Stop a background job and everything it started.".into()
    }
    fn parameters(&self) -> Value {
        id_schema("shell_id")
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = req_str(&input, "shell_id")?;
        let job = self.jobs.get(id).ok_or_else(|| unknown(&self.jobs, id))?;
        Ok(ToolOutput::text(if self.jobs.kill(id) {
            format!("Killed job {id} ({}).", job.command)
        } else {
            format!("Job {id} already {}.", job.state())
        }))
    }
}

/// The tools that read and stop the jobs of `jobs`, in the order the model
/// sees them: BashOutput, KillShell.
pub fn job_tools(jobs: Arc<JobHub>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(BashOutputTool { jobs: jobs.clone() }),
        Arc::new(KillShellTool { jobs }),
    ]
}

#[cfg(test)]
mod tests;
