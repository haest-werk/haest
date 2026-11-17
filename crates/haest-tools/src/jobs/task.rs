//! One job's own task: it runs the command through the workspace, streams its
//! output into the table, and reports the end (an exit notice to the inbox
//! when it exited on its own).

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use haest_core::workspace::{ExecOptions, OutputChunks, WorkspaceRef};
use tokio_util::sync::CancellationToken;

use super::{Job, JobEvents, JobStatus, entry::Jobs};
use crate::inbox::Inbox;

/// A job has no timeout; this only keeps the deadline arithmetic finite.
const NO_DEADLINE: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// What a hub and its jobs' tasks share.
pub(super) struct Shared {
    pub(super) jobs: Mutex<Jobs>,
    pub(super) events: Arc<dyn JobEvents>,
    pub(super) inbox: Arc<dyn Inbox>,
}

/// The text the model gets, through the [`Inbox`], for a job that exited on
/// its own: which job, how it ended, and to read it with BashOutput.
fn exit_notice(job: &Job) -> String {
    format!(
        "[Background job {} {}: {}]\nRead what it printed with BashOutput (bash_id \"{}\") if it matters for the task.",
        job.id,
        job.state(),
        job.command,
        job.id
    )
}

impl Shared {
    fn output(&self, id: &str, chunk: &str) {
        let job = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(e) = jobs.get_mut(id) else { return };
            e.append(chunk);
            e.job.clone()
        };
        self.events.on_output(&job, chunk, false);
    }

    fn finish(&self, id: &str, code: i32) {
        let (job, running) = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(e) = jobs.get_mut(id) else { return };
            e.job.exit_code = Some(code);
            if e.job.status == JobStatus::Running {
                e.job.status = JobStatus::Exited;
            }
            let job = e.job.clone();
            jobs.forget_old_finished();
            (job, jobs.running())
        };
        self.events.on_output(&job, "", true);
        self.events.on_change(&running);
        self.events.on_exit(&job);
        if job.status == JobStatus::Exited {
            self.inbox.submit(exit_notice(&job));
        }
    }
}

/// Run `job` until it exits or `stop` fires (the workspace kills it then).
pub(super) async fn run(
    shared: Arc<Shared>,
    workspace: WorkspaceRef,
    job: Job,
    cwd: PathBuf,
    stop: CancellationToken,
) {
    let (to, id) = (shared.clone(), job.id.clone());
    let on_output = OutputChunks::from(move |chunk: &str| to.output(&id, chunk));
    let opts = ExecOptions::new(cwd, NO_DEADLINE).with_cancel(stop);
    let r = workspace
        .shell_streaming(&job.command, opts, on_output)
        .await;
    shared.finish(&job.id, r.exit_code);
}
