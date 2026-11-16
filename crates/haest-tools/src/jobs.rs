//! BACKGROUND SHELL JOBS: a command the model starts and does not wait for (a dev
//! server, a long build, a test watcher). Ported from the TS haest's
//! `core/background-jobs.ts`, plus the host-neutral half of `host/jobs.ts`.
//!
//! A job runs through the conversation's workspace
//! ([`Workspace::shell_streaming`]), so it works over ANY haest workspace: the
//! real one streams a detached process group's output as it comes, and killing
//! the job (KillShell, or the hub being dropped) takes down everything it
//! spawned; a workspace that cannot stream hands the output over when the job
//! ends. Whether a conversation offers jobs at all is the host's call (the TS
//! haest offers none in the sandbox).
//!
//! Output is kept in a rolling buffer the model reads with BashOutput ("what is
//! new since I last looked"); it also streams to [`JobEvents::on_output`] for
//! the transcript. Every start and stop hands the full set of running jobs to
//! [`JobEvents::on_change`] -- a snapshot, never a delta.
//!
//! A job that EXITS on its own tells the model through the host's [`Inbox`], as a
//! turn the human did not start. A job the model killed says nothing (it knows),
//! nor does one killed at shutdown.
//!
//! Start from [`JobHub`]: the host makes one per conversation and hands it to
//! Bash through [`BashOptions::jobs`](crate::BashOptions::jobs).

mod entry;
#[cfg(test)]
pub(crate) mod tests;

use std::{
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use haest_core::workspace::{ExecOptions, OutputChunks, Workspace};
use tokio_util::sync::CancellationToken;

use crate::inbox::Inbox;
use entry::{Entry, Jobs};

/// Rolling output kept per job, in bytes; older output is dropped from the front.
pub const MAX_BUFFER: usize = 1_000_000;
/// Jobs one hub runs at once; [`JobHub::start`] refuses one more.
pub const MAX_JOBS: usize = 8;
/// Finished jobs kept for BashOutput; older ones (and their up-to-1MB output) are forgotten.
pub const MAX_FINISHED: usize = 16;
/// A job has no timeout; this only keeps the deadline arithmetic finite.
const NO_DEADLINE: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// How it stands, for the model: running for 12s, killed, exited with code 3.
    pub fn state(&self) -> String {
        match self.status {
            JobStatus::Running => format!("running for {}s", self.started.elapsed().as_secs()),
            JobStatus::Killed => "killed".into(),
            JobStatus::Exited => format!(
                "exited with code {}",
                self.exit_code.map_or("?".into(), |c| c.to_string())
            ),
        }
    }
}

/// What a host hears about jobs, for its transcript. All optional.
///
/// Called as things happen (a start from [`JobHub::start`], the rest from the
/// job's own task), never while the hub's table is locked: a handler may call
/// the hub back.
pub trait JobEvents: Send + Sync {
    /// Every start and stop: the full set of running jobs, never a delta.
    fn on_change(&self, _running: &[Job]) {}
    /// Output as it arrives; once more with an empty chunk and `done` when the job ended.
    fn on_output(&self, _job: &Job, _chunk: &str, _done: bool) {}
    /// The job ended: exited on its own, or killed.
    fn on_exit(&self, _job: &Job) {}
}

/// [`JobEvents`] that hears nothing: what [`JobHub::new`] uses.
#[derive(Debug, Default)]
pub struct NoJobEvents;
impl JobEvents for NoJobEvents {}

/// What BashOutput hands out ([`JobHub::take`]).
#[derive(Debug, Clone)]
pub struct Taken {
    /// The job as it stands.
    pub job: Job,
    /// Output new since the last take.
    pub fresh: String,
    /// Bytes that fell off the front of the buffer before they were taken.
    pub missed: usize,
}

/// The text the model gets, through the [`Inbox`], for a job that exited on
/// its own: which job, how it ended, and to read it with BashOutput.
pub fn exit_notice(job: &Job) -> String {
    format!(
        "[Background job {} {}: {}]\nRead what it printed with BashOutput (bash_id \"{}\") if it matters for the task.",
        job.id,
        job.state(),
        job.command,
        job.id
    )
}

struct Shared {
    jobs: Mutex<Jobs>,
    events: Arc<dyn JobEvents>,
    inbox: Arc<dyn Inbox>,
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

/// One conversation's background jobs. The host owns it and hands it to the
/// Bash tool ([`crate::BashOptions::jobs`]); dropping it kills every job still
/// running. [`JobHub::kill_all`] only CANCELS: each job's own task does the
/// kill, on the runtime. A host that exits calls it, then keeps the runtime up
/// until every killed job has an `exit_code` (a killed job leaves
/// [`JobHub::running`] at once, before its process is gone; the CLI's
/// `kill_jobs` waits this way), or the processes outlive it.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use haest_core::fs::MemFs;
/// use haest_tools::JobHub;
/// use haest_workspace::SandboxWorkspace;
/// use tokio::sync::mpsc::unbounded_channel;
///
/// # #[tokio::main]
/// # async fn main() {
/// // The host's inbox: here a queue it drains.
/// let (inbox, mut notices) = unbounded_channel::<String>();
/// let hub = JobHub::new(Arc::new(inbox));
/// let ws = Arc::new(SandboxWorkspace::over(Arc::new(MemFs::new())));
/// let job = hub.start(ws, "echo built", "build", "/".into()).unwrap();
/// assert_eq!(job.id, "b1");
///
/// // It exited on its own, so the model is told.
/// let notice = notices.recv().await.unwrap();
/// assert!(notice.contains("b1"));
/// let taken = hub.take("b1").unwrap();
/// assert_eq!(taken.fresh, "built\n");
/// assert_eq!(taken.job.exit_code, Some(0));
/// // Read once: the next look has nothing new.
/// assert_eq!(hub.take("b1").unwrap().fresh, "");
/// # }
/// ```
pub struct JobHub {
    shared: Arc<Shared>,
}

impl JobHub {
    /// A hub with no jobs yet. Exit notices go to `inbox` (Bash tells the
    /// model it will be told).
    pub fn new(inbox: Arc<dyn Inbox>) -> Self {
        Self::with_events(inbox, Arc::new(NoJobEvents))
    }

    /// The same, with `events` hearing every start, output chunk and end for
    /// the host's transcript.
    pub fn with_events(inbox: Arc<dyn Inbox>, events: Arc<dyn JobEvents>) -> Self {
        Self {
            shared: Arc::new(Shared {
                jobs: Mutex::default(),
                events,
                inbox,
            }),
        }
    }

    /// The jobs still running, in start order.
    pub fn running(&self) -> Vec<Job> {
        self.shared.jobs.lock().unwrap().running()
    }

    /// The job `id` as it stands, running or ended; None when it is unknown
    /// or long forgotten ([`MAX_FINISHED`]).
    pub fn get(&self, id: &str) -> Option<Job> {
        let jobs = self.shared.jobs.lock().unwrap();
        jobs.get(id).map(|e| e.job.clone())
    }

    /// Everything kept of a job's output, without marking it read.
    pub fn buffer(&self, id: &str) -> Option<String> {
        let jobs = self.shared.jobs.lock().unwrap();
        jobs.get(id).map(|e| e.output.clone())
    }

    /// Start `command` in `cwd` through `workspace`
    /// ([`Workspace::shell_streaming`]); it runs until it exits or is killed,
    /// with no timeout.
    ///
    /// # Errors
    ///
    /// A message for the model when [`MAX_JOBS`] already run, or when there is
    /// no tokio runtime to run the job on.
    pub fn start(
        &self,
        workspace: Arc<dyn Workspace>,
        command: &str,
        description: &str,
        cwd: PathBuf,
    ) -> Result<Job, String> {
        let rt = tokio::runtime::Handle::try_current()
            .map_err(|_| "background jobs need a tokio runtime".to_string())?;
        let stop = CancellationToken::new();
        let (job, running) = {
            let mut jobs = self.shared.jobs.lock().unwrap();
            if jobs.running().len() >= MAX_JOBS {
                return Err(format!(
                    "{MAX_JOBS} background jobs are already running; KillShell one first"
                ));
            }
            let job = jobs.add(command, description, stop.clone());
            (job, jobs.running())
        };
        // Before the job runs, so a job that ends at once is never reported before it started.
        self.shared.events.on_change(&running);
        rt.spawn(run(self.shared.clone(), workspace, job.clone(), cwd, stop));
        Ok(job)
    }

    /// New output since the last call, and the job as it stands; None for an
    /// unknown id. What it returns counts as read.
    pub fn take(&self, id: &str) -> Option<Taken> {
        let mut jobs = self.shared.jobs.lock().unwrap();
        jobs.get_mut(id).map(Entry::take)
    }

    /// Stop job `id`: it is marked killed and its run is cancelled, which the
    /// workspace turns into killing its process group. False when the job is
    /// unknown or no longer running.
    pub fn kill(&self, id: &str) -> bool {
        let mut jobs = self.shared.jobs.lock().unwrap();
        match jobs.get_mut(id) {
            Some(e) if e.job.status == JobStatus::Running => {
                e.job.status = JobStatus::Killed;
                e.stop.cancel();
                true
            }
            _ => false,
        }
    }

    /// [`kill`](Self::kill) every running job. It returns before their
    /// processes are gone: see the type's docs for a host that exits.
    pub fn kill_all(&self) {
        for job in self.running() {
            self.kill(&job.id);
        }
    }
}

impl Drop for JobHub {
    fn drop(&mut self) {
        self.kill_all();
    }
}

impl fmt::Debug for JobHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids: Vec<String> = self.running().into_iter().map(|j| j.id).collect();
        f.debug_struct("JobHub").field("running", &ids).finish()
    }
}

async fn run(
    shared: Arc<Shared>,
    workspace: Arc<dyn Workspace>,
    job: Job,
    cwd: PathBuf,
    stop: CancellationToken,
) {
    let (to, id) = (shared.clone(), job.id.clone());
    let on_output: OutputChunks = Arc::new(move |chunk: &str| to.output(&id, chunk));
    let opts = ExecOptions {
        cwd,
        cancel: stop,
        timeout: NO_DEADLINE,
    };
    let r = workspace
        .shell_streaming(&job.command, opts, on_output)
        .await;
    shared.finish(&job.id, r.exit_code);
}
