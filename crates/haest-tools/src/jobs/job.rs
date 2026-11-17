//! [`Job`]: one background job as it stands, and how the model reads it.

use std::{fmt, time::Instant};

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobStatus {
    /// Started and not stopped.
    Running,
    /// Ended on its own; [`Job::exit_code`] says how.
    Exited,
    /// Stopped by [`JobHub::kill`](super::JobHub::kill) (KillShell,
    /// [`JobHub::kill_all`](super::JobHub::kill_all), the hub dropped). It
    /// leaves the running set at once; [`Job::exit_code`] is set when its
    /// process is gone.
    Killed,
}

/// One job as it stands.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Job {
    /// The model's short id: "b1", "b2", ... per hub.
    pub id: String,
    /// The command line, as the model gave it.
    pub command: String,
    /// What it is for (Bash passes the call's `description`, else the command).
    pub description: String,
    /// Running, exited or killed.
    pub status: JobStatus,
    /// Set once the job has ended (a killed one: the code the workspace
    /// reports for the kill, e.g. `128 + 9`).
    pub exit_code: Option<i32>,
    /// When it was started.
    pub started: Instant,
}

impl Job {
    /// How it stands, for the model: `running for 12s`, `killed`, `exited
    /// with code 3`.
    ///
    /// ```
    /// # fn show(job: &haest_tools::Job) -> String {
    /// format!("Job {} {}", job.id, job.state())
    /// # }
    /// ```
    pub fn state(&self) -> JobState<'_> {
        JobState(self)
    }
}

/// How a job stands, as the model reads it ([`Job::state`]): shown with
/// `Display`.
#[derive(Debug, Clone, Copy)]
pub struct JobState<'a>(&'a Job);

impl fmt::Display for JobState<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let job = self.0;
        match job.status {
            JobStatus::Running => write!(f, "running for {}s", job.started.elapsed().as_secs()),
            JobStatus::Killed => f.write_str("killed"),
            JobStatus::Exited => match job.exit_code {
                Some(code) => write!(f, "exited with code {code}"),
                None => f.write_str("exited with code ?"),
            },
        }
    }
}
