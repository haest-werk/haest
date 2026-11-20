//! One job's own task: it runs the command through the workspace, streams its
//! output into the table, and reports the end (an exit notice to the inbox
//! when it exited on its own).
//!
//! A panic in the run (the workspace's own, or the
//! [`on_output`](super::JobEvents::on_output) it raises) still ends the job:
//! it is marked ended and reported, then the panic goes on.

use std::{
    mem,
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use futures::FutureExt;
use haest_core::{
    newest::Newest,
    workspace::{ExecOptions, OutputChunks, WorkspaceRef},
};
use tokio_util::sync::CancellationToken;

use super::{Job, JobEventsRef, JobOutput, JobStatus, entry::Jobs};
use crate::inbox::InboxRef;

/// A job has no timeout; this only keeps the deadline arithmetic finite.
const NO_DEADLINE: Duration = Duration::from_secs(365 * 24 * 60 * 60);
/// The exit code of a job whose run panicked: it ended, how is unknown.
const EXIT_UNKNOWN: i32 = -1;

/// What a hub and its jobs' tasks share.
pub(super) struct Shared {
    pub(super) jobs: Mutex<Jobs>,
    /// Set by `JobHub::set_events`; read (and the handle cloned) for every event.
    pub(super) events: Mutex<JobEventsRef>,
    pub(super) inbox: InboxRef,
    /// The running set after the latest start or end, put under the table's
    /// lock and handed to `on_change` with it released: in order, the
    /// newest last.
    pub(super) running: Newest<Vec<Job>>,
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
        self.listener().clone()
    }

    /// Where every event goes from now on. The old listener is dropped once
    /// the lock is released: its drop is a host's code, and may call back.
    pub(super) fn set_events(&self, events: JobEventsRef) {
        let old = mem::replace(&mut *self.listener(), events);
        drop(old);
    }

    /// The listener's slot. Held only to swap the handle, never while a
    /// handler runs, so a poisoned one is a bug here.
    fn listener(&self) -> MutexGuard<'_, JobEventsRef> {
        self.events
            .lock()
            .expect("the job hub's listener lock poisoned")
    }

    /// The table, held until the guard drops. Never held while a handler
    /// or a job runs, so a poisoned one is a bug here.
    pub(super) fn lock_jobs(&self) -> MutexGuard<'_, Jobs> {
        self.jobs.lock().expect("the job hub's table lock poisoned")
    }

    pub(super) fn running(&self) -> Vec<Job> {
        self.lock_jobs().running()
    }

    pub(super) fn kill(&self, id: &str) -> bool {
        let mut jobs = self.lock_jobs();
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

    /// Keep `chunk` in job `id`'s buffer; the job as it stands then.
    fn append(&self, id: &str, chunk: &str) -> Option<Job> {
        let mut jobs = self.lock_jobs();
        let e = jobs.get_mut(id)?;
        e.append(chunk);
        Some(e.job.clone())
    }

    /// Hand the listener in place the running set put last, unless another
    /// caller is handing it over: then that one hands this one's over too.
    /// The listener is looked up for each set, so a delivery that began before
    /// [`set_events`](Self::set_events) hands the later sets to the new one.
    pub(super) fn report_running(&self) {
        self.running
            .deliver(|running| self.events().on_change(running));
    }

    /// Mark job `id` ended with `code` (None: its run panicked), then report
    /// it: Done (unless the run panicked), the running set, the end, the
    /// model's notice. Each event goes to the listener in place when it is
    /// made.
    fn finish(&self, id: &str, code: Option<i32>) {
        let job = {
            let mut jobs = self.lock_jobs();
            let Some(e) = jobs.get_mut(id) else {
                return;
            };
            e.job.exit_code = Some(code.unwrap_or(EXIT_UNKNOWN));
            if e.job.status == JobStatus::Running {
                e.job.status = JobStatus::Exited;
            }
            let job = e.job.clone();
            jobs.forget_old_finished();
            self.running.put(jobs.running());
            job
        };
        if code.is_some() {
            self.events().on_output(&job, JobOutput::Done);
        }
        self.report_running();
        self.events().on_exit(&job);
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
    // Each chunk is kept in the job's buffer, then heard.
    let on_output = OutputChunks::from({
        let shared = shared.clone();
        let id = job.id.clone();
        move |chunk: &str| {
            if let Some(job) = shared.append(&id, chunk) {
                shared.events().on_output(&job, JobOutput::Chunk(chunk));
            }
        }
    });
    let opts = ExecOptions::new(cwd, NO_DEADLINE).with_cancel(stop);
    let ran = AssertUnwindSafe(workspace.shell_streaming(&job.command, opts, on_output))
        .catch_unwind()
        .await;
    shared.finish(&job.id, ran.as_ref().ok().map(|r| r.exit_code));
    if let Err(panic) = ran {
        panic::resume_unwind(panic);
    }
}
