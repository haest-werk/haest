//! One job's own task: it runs the command through the workspace, streams its
//! output into the table, and reports the end (an exit notice to the inbox
//! when it exited on its own).
//!
//! A panic on the way, in a host's [`JobEvents::on_output`](super::JobEvents::on_output)
//! or in the workspace, never skips the end: the job is reported ended,
//! then the panic goes on, as the session and the transport raise a host's.

use std::{
    any::Any,
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
/// The exit code of a job whose workspace panicked: it ended, how is unknown.
const EXIT_UNKNOWN: i32 = -1;

/// What `catch_unwind` hands over: the payload a panic carries.
type Panic = Box<dyn Any + Send>;

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

    /// Where every event goes from now on.
    pub(super) fn set_events(&self, events: JobEventsRef) {
        *self.listener() = events;
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

    fn finish(&self, id: &str, code: i32) {
        let job = {
            let mut jobs = self.lock_jobs();
            let Some(e) = jobs.get_mut(id) else { return };
            e.job.exit_code = Some(code);
            if e.job.status == JobStatus::Running {
                e.job.status = JobStatus::Exited;
            }
            let job = e.job.clone();
            jobs.forget_old_finished();
            self.running.put(jobs.running());
            job
        };
        // Each event goes to the listener in place when it is made.
        self.events().on_output(&job, JobOutput::Done);
        self.report_running();
        self.events().on_exit(&job);
        if job.status == JobStatus::Exited {
            self.inbox.submit(exit_notice(&job));
        }
    }
}

/// Where one job's output goes: its buffer, then the listener. A listener
/// that panics hears no more of this job's output, and the job runs on (its
/// output still kept, a real process still read): the panic is kept for
/// [`run`] to raise once the job has ended.
struct Output {
    shared: Arc<Shared>,
    id: String,
    panicked: Mutex<Option<Panic>>,
}

impl Output {
    fn chunk(&self, chunk: &str) {
        let Some(job) = self.shared.append(&self.id, chunk) else {
            return;
        };
        if self.panicked().is_some() {
            return;
        }
        let events = self.shared.events();
        let heard = panic::catch_unwind(AssertUnwindSafe(|| {
            events.on_output(&job, JobOutput::Chunk(chunk));
        }));
        if let Err(panic) = heard {
            self.panicked().get_or_insert(panic);
        }
    }

    /// The listener's panic, if it panicked. Held only to read or set it,
    /// never while the listener runs, so a poisoned one is a bug here.
    fn panicked(&self) -> MutexGuard<'_, Option<Panic>> {
        self.panicked
            .lock()
            .expect("the job output's panic lock poisoned")
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
    let output = Arc::new(Output {
        shared: shared.clone(),
        id: job.id.clone(),
        panicked: Mutex::default(),
    });
    let on_output = OutputChunks::from({
        let output = output.clone();
        move |chunk: &str| output.chunk(chunk)
    });
    let opts = ExecOptions::new(cwd, NO_DEADLINE).with_cancel(stop);
    let ran = AssertUnwindSafe(workspace.shell_streaming(&job.command, opts, on_output))
        .catch_unwind()
        .await;
    shared.finish(&job.id, ran.as_ref().map_or(EXIT_UNKNOWN, |r| r.exit_code));
    if let Some(panic) = ran.err().or_else(|| output.panicked().take()) {
        panic::resume_unwind(panic);
    }
}
