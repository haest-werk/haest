//! BACKGROUND SHELL JOBS: a command the model starts and does not wait for (a dev
//! server, a long build, a test watcher). Ported from the TS haest's
//! `core/background-jobs.ts`, plus the host-neutral half of `host/jobs.ts`.
//!
//! A job runs through the conversation's workspace
//! ([`Workspace::shell_streaming`](haest_core::Workspace::shell_streaming)), so it works over ANY haest workspace: the
//! real one streams a detached process group's output as it comes, and killing
//! the job (KillShell, or the hub's last clone being dropped) takes down everything it
//! spawned; a workspace that cannot stream hands the output over when the job
//! ends. Whether a conversation offers jobs at all is the host's call (the TS
//! haest offers none in the sandbox).
//!
//! Output is kept in a rolling buffer the model reads with BashOutput ("what is
//! new since I last looked"); it also streams to [`JobEvents::on_output`] for
//! the transcript. Every start and stop hands the full set of running jobs to
//! [`JobEvents::on_change`] -- a snapshot, never a delta, and never an older
//! one after a newer one.
//!
//! A job that EXITS on its own tells the model through the host's
//! [`Inbox`](crate::Inbox), as a turn the human did not start. A job the model
//! killed says nothing (it knows), nor does one killed at shutdown.
//!
//! Start from [`JobHub`]: the host makes one per conversation and hands it to
//! Bash through [`BashOptions::with_jobs`](crate::BashOptions::with_jobs).

mod entry;
mod events;
mod job;
mod new_job;
mod task;
#[cfg(test)]
pub(crate) mod tests;

pub use events::{JobEvents, JobEventsRef, JobOutput, NoJobEvents};
pub use job::{Job, JobState, JobStatus};
pub use new_job::NewJob;

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use haest_core::{newest::Newest, tool::ToolContext};
use tokio_util::sync::CancellationToken;

use crate::inbox::InboxRef;
use entry::Entry;
use task::{Shared, run};

/// Rolling output kept per job, in bytes; older output is dropped from the front.
pub(crate) const MAX_BUFFER: usize = 1_000_000;
/// Jobs one hub runs at once; [`JobHub::start`] refuses one more.
pub const MAX_JOBS: usize = 8;
/// Finished jobs kept for BashOutput; older ones (and their up-to-1MB output) are forgotten.
pub const MAX_FINISHED: usize = 16;

/// Why [`JobHub::start`] started nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum JobError {
    /// As many jobs run as one hub allows ([`MAX_JOBS`]): the model stops one
    /// first.
    #[error("{max} background jobs are already running; KillShell one first")]
    TooMany {
        /// How many may run at once.
        max: usize,
    },
    /// Called outside a tokio runtime: a job runs on a task of its own.
    #[error("background jobs need a tokio runtime")]
    NoRuntime,
}

/// What BashOutput hands out ([`JobHub::take`]).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Taken {
    /// The job as it stands.
    pub job: Job,
    /// Output new since the last take.
    pub fresh: String,
    /// Bytes that fell off the front of the buffer before they were taken.
    pub missed: usize,
}

/// One conversation's background jobs. The host makes one and hands it to the
/// Bash tool ([`BashOptions::with_jobs`](crate::BashOptions::with_jobs)).
///
/// A hub is a cheap handle: clones share the jobs. Dropping the LAST clone
/// kills every job still running; dropping any other clone does nothing.
/// [`JobHub::kill_all`] only CANCELS: each job's own task does the kill, on
/// the runtime. A host that exits calls it, then keeps the runtime up until
/// every killed job has an `exit_code` (a killed job leaves
/// [`JobHub::running`] at once, before its process is gone; the CLI's
/// `kill_jobs` waits this way), or the processes outlive it.
///
/// # Examples
///
/// ```
/// use haest_core::{fs::MemFs, tool::ToolContext};
/// use haest_tools::{JobHub, NewJob};
/// use haest_workspace::SandboxWorkspace;
/// use tokio::sync::mpsc::unbounded_channel;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// // The host's inbox: here a queue it drains.
/// let (inbox, mut notices) = unbounded_channel::<String>();
/// let hub = JobHub::new(inbox);
/// let ctx = ToolContext::new("/", SandboxWorkspace::over(MemFs::new()));
/// let job = hub.start(NewJob::new("echo built").with_description("build"), &ctx)?;
/// assert_eq!(job.id, "b1");
///
/// // It exited on its own, so the model is told.
/// let notice = notices.recv().await.ok_or("no notice")?;
/// assert!(notice.contains("b1"));
/// let taken = hub.take("b1").ok_or("no job b1")?;
/// assert_eq!(taken.fresh, "built\n");
/// assert_eq!(taken.job.exit_code, Some(0));
/// // Read once: the next look has nothing new.
/// assert_eq!(hub.take("b1").ok_or("no job b1")?.fresh, "");
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct JobHub(Arc<Owner>);

/// What the hub's clones share; its drop is the last clone's, and kills
/// what runs. The jobs' tasks hold the [`Shared`] under it, never this.
struct Owner(Arc<Shared>);

impl Drop for Owner {
    fn drop(&mut self) {
        self.0.kill_all();
    }
}

impl JobHub {
    /// A hub with no jobs yet. Exit notices go to `inbox` (Bash tells the
    /// model it will be told): the inbox itself, or an `Arc` of one the host
    /// keeps too.
    pub fn new(inbox: impl Into<InboxRef>) -> Self {
        Self(Arc::new(Owner(Arc::new(Shared {
            jobs: Mutex::default(),
            events: Mutex::new(JobEventsRef::new(NoJobEvents)),
            inbox: inbox.into(),
            running: Newest::new(),
        }))))
    }

    /// `events` hears every start, output chunk and end from now on, for the
    /// host's transcript: this hub's and every clone's.
    pub fn set_events(&self, events: impl Into<JobEventsRef>) {
        self.shared().set_events(events.into());
    }

    fn shared(&self) -> &Arc<Shared> {
        &self.0.0
    }

    /// The jobs still running, in start order.
    pub fn running(&self) -> Vec<Job> {
        self.shared().running()
    }

    /// The job `id` as it stands, running or ended; None when it is unknown
    /// or long forgotten ([`MAX_FINISHED`]).
    pub fn get(&self, id: &str) -> Option<Job> {
        let jobs = self.shared().lock_jobs();
        jobs.get(id).map(|e| e.job.clone())
    }

    /// Everything kept of a job's output, without marking it read.
    pub fn buffer(&self, id: &str) -> Option<String> {
        let jobs = self.shared().lock_jobs();
        jobs.get(id).map(|e| e.output.clone())
    }

    /// Start `job`'s command in the context's `cwd`, through its workspace
    /// ([`Workspace::shell_streaming`](haest_core::Workspace::shell_streaming));
    /// it runs until it exits or is killed, with no timeout.
    ///
    /// # Errors
    ///
    /// [`JobError::TooMany`] when [`MAX_JOBS`] already run,
    /// [`JobError::NoRuntime`] when there is no tokio runtime to run the job on.
    pub fn start(&self, job: NewJob, ctx: &ToolContext) -> Result<Job, JobError> {
        let (command, description) = job.into_parts();
        let rt =
            tokio::runtime::Handle::try_current().map_err(|_no_runtime| JobError::NoRuntime)?;
        let shared = self.shared();
        let stop = CancellationToken::new();
        let job = {
            let mut jobs = shared.lock_jobs();
            if jobs.running().len() >= MAX_JOBS {
                return Err(JobError::TooMany { max: MAX_JOBS });
            }
            let job = jobs.add(command, description, stop.clone());
            shared.running.put(jobs.running());
            job
        };
        // Before the job runs, so its set is put before its end's. While
        // another caller is handing a set over, that one hands this one over
        // too, and a job that ends meanwhile is folded into the newer set
        // (`JobEvents::on_exit` says so).
        shared.report_running();
        rt.spawn(run(
            shared.clone(),
            ctx.workspace().clone(),
            job.clone(),
            ctx.cwd().to_path_buf(),
            stop,
        ));
        Ok(job)
    }

    /// New output since the last call, and the job as it stands; None for an
    /// unknown id. What it returns counts as read.
    pub fn take(&self, id: &str) -> Option<Taken> {
        let mut jobs = self.shared().lock_jobs();
        jobs.get_mut(id).map(Entry::take)
    }

    /// Stop job `id`: it is marked killed and its run is cancelled, which the
    /// workspace turns into killing its process group. False when the job is
    /// unknown or no longer running.
    pub fn kill(&self, id: &str) -> bool {
        self.shared().kill(id)
    }

    /// [`kill`](Self::kill) every running job. It returns before their
    /// processes are gone: see the type's docs for a host that exits.
    pub fn kill_all(&self) {
        self.shared().kill_all();
    }
}

impl fmt::Debug for JobHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids: Vec<String> = self.running().into_iter().map(|j| j.id).collect();
        f.debug_struct("JobHub")
            .field("running", &ids)
            .finish_non_exhaustive()
    }
}
