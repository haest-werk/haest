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
mod new_job;
mod task;
#[cfg(test)]
pub(crate) mod tests;

pub use new_job::NewJob;

use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Instant,
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

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobStatus {
    /// Started and not stopped.
    Running,
    /// Ended on its own; [`Job::exit_code`] says how.
    Exited,
    /// Stopped by [`JobHub::kill`] (KillShell, [`JobHub::kill_all`], the hub
    /// dropped). It leaves the running set at once; [`Job::exit_code`] is set
    /// when its process is gone.
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

/// What a job printed, as [`JobEvents::on_output`] hears it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobOutput<'a> {
    /// Output as it arrived.
    Chunk(&'a str),
    /// The job ended: nothing more will come.
    Done,
}

/// What a host hears about jobs, for its transcript. All optional.
///
/// Called as things happen (a start from [`JobHub::start`], the rest from the
/// job's own task), never while the hub's table is locked: a handler may call
/// the hub back.
///
/// A panic in [`on_output`](Self::on_output) does not stop the job: that
/// handler hears no more of its output (the buffer still keeps it), the job
/// ends as it would have and is reported ended, then the panic goes on, on
/// the job's own task.
pub trait JobEvents: Send + Sync {
    /// Every start and stop: the full set of running jobs, never a delta.
    /// One call at a time, and never an older set after a newer one: the
    /// changes made while one call runs are handed over next as one, the
    /// newest. So the last set it hears is the hub's.
    fn on_change(&self, _running: &[Job]) {}
    /// Output as it arrives, then [`JobOutput::Done`] once when the job ended.
    fn on_output(&self, _job: &Job, _output: JobOutput<'_>) {}
    /// The job ended: exited on its own, or killed. A job that started and
    /// ended while one [`on_change`](Self::on_change) call ran may be in no
    /// set it hears: its sets were folded into the newest.
    fn on_exit(&self, _job: &Job) {}
}

/// [`JobEvents`] that hears nothing: what [`JobHub::new`] uses.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoJobEvents;
impl JobEvents for NoJobEvents {}

haest_core::plug_ref! {
    /// [`JobEvents`], shared: what [`JobHub::set_events`] takes. Built from
    /// the listener itself, or from an `Arc` of one the host keeps too.
    pub struct JobEventsRef(dyn JobEvents);
}

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
