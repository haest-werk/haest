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

use super::{Job, JobEventsRef, JobOutput, JobStatus, entry::Jobs};
use crate::inbox::InboxRef;

/// A job has no timeout; this only keeps the deadline arithmetic finite.
const NO_DEADLINE: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// What a hub and its jobs' tasks share.
pub(super) struct Shared {
    pub(super) jobs: Mutex<Jobs>,
    /// Set once, by `JobHub::with_events`; read (and the handle cloned) for every event.
    pub(super) events: Mutex<JobEventsRef>,
    pub(super) inbox: InboxRef,
}

/// The text the model gets, through the [`Inbox`](crate::Inbox), for a job
/// that exited on its own: which job, how it ended, and to read it with
/// BashOutput.
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
    /// The listener, never called with a lock held: a handler may call the hub back.
    pub(super) fn events(&self) -> JobEventsRef {
        self.events.lock().unwrap().clone()
    }

    pub(super) fn running(&self) -> Vec<Job> {
        self.jobs.lock().unwrap().running()
    }

    pub(super) fn kill(&self, id: &str) -> bool {
        let mut jobs = self.jobs.lock().unwrap();
        match jobs.get_mut(id) {
            Some(e) if e.job.status == JobStatus::Running => {
                e.job.status = JobStatus::Killed;
                e.stop.cancel();
                true
            }
            _ => false,
        }
    }

    pub(super) fn kill_all(&self) {
        for job in self.running() {
            self.kill(&job.id);
        }
    }

    fn output(&self, id: &str, chunk: &str) {
        let job = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(e) = jobs.get_mut(id) else { return };
            e.append(chunk);
            e.job.clone()
        };
        self.events().on_output(&job, JobOutput::Chunk(chunk));
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
        let events = self.events();
        events.on_output(&job, JobOutput::Done);
        events.on_change(&running);
        events.on_exit(&job);
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
