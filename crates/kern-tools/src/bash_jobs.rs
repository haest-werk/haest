//! BashOutput / KillShell: reading and stopping background jobs ([`crate::jobs`]).
//! Keys follow the common agent-tool convention (`bash_id`, `shell_id`), so
//! models already know the calls.

use std::sync::Arc;

use async_trait::async_trait;
use kern_core::tool::{
    JsonMap, TRUNCATE_CHARS, Tool, ToolContext, ToolError, ToolOutput, req_str, truncate_middle,
};
use serde_json::{Value, json};

use crate::jobs::{Job, JobHub, MAX_BUFFER};

/// The job, or an error that names the ones running.
fn known(jobs: &JobHub, id: &str) -> Result<Job, ToolError> {
    jobs.get(id).ok_or_else(|| {
        let running: Vec<String> = jobs.running().into_iter().map(|j| j.id).collect();
        let running = if running.is_empty() {
            "none".to_string()
        } else {
            running.join(", ")
        };
        ToolError::Input(format!("no background job \"{id}\"; running: {running}"))
    })
}

fn id_schema(key: &str) -> Value {
    json!({
        "type": "object",
        "properties": {key: {"type": "string", "description": "The job id, e.g. \"b1\""}},
        "required": [key]
    })
}

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
        known(&self.jobs, id)?;
        let got = self
            .jobs
            .take(id)
            .ok_or_else(|| ToolError::Input(format!("no background job \"{id}\"")))?;
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
        let job = known(&self.jobs, id)?;
        Ok(ToolOutput::text(if self.jobs.kill(id) {
            format!("Killed job {id} ({}).", job.command)
        } else {
            format!("Job {id} already {}.", job.state())
        }))
    }
}

/// The tools that read and stop the jobs of `jobs`, in the order the model sees them.
pub fn job_tools(jobs: Arc<JobHub>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(BashOutputTool { jobs: jobs.clone() }),
        Arc::new(KillShellTool { jobs }),
    ]
}

#[cfg(test)]
mod tests;
