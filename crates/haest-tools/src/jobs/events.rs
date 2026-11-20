//! [`JobEvents`]: what a host hears about its jobs, for its transcript.

use super::Job;

/// What a job printed, as [`JobEvents::on_output`] hears it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobOutput<'a> {
    /// Output as it arrived.
    Chunk(&'a str),
    /// The job ended: nothing more will come. Not sent when the job's run
    /// panicked.
    Done,
}

/// What a host hears about jobs, for its transcript. All optional.
///
/// Called as things happen (a start from
/// [`JobHub::start`](super::JobHub::start), the rest from the job's own
/// task), never while the hub's table is locked: a handler may call the hub
/// back.
///
/// A handler must not panic. One that panics on a job's output loses the rest
/// of that output (BashOutput never sees it), and the job is reported ended
/// with exit code -1 and no [`JobOutput::Done`]; one that panics on a job's
/// end skips the events after it (the running set, [`on_exit`](Self::on_exit),
/// the model's notice). The panic goes on, out of
/// [`JobHub::start`](super::JobHub::start) for a start's
/// [`on_change`](Self::on_change), else on the job's own task. What still
/// holds: the hub's table changes before any handler runs, and no lock is held
/// while one does, so a started job runs and is marked ended there, and
/// BashOutput and KillShell read it as it is.
pub trait JobEvents: Send + Sync {
    /// Every start and stop: the full set of running jobs, never a delta.
    /// One call at a time, and never an older set after a newer one: the
    /// changes made while one call runs are handed over next as one, the
    /// newest. So the last set it hears is the hub's.
    fn on_change(&self, _running: &[Job]) {}
    /// Output as it arrives, then [`JobOutput::Done`] once when the job
    /// ended.
    fn on_output(&self, _job: &Job, _output: JobOutput<'_>) {}
    /// The job ended: exited on its own, or killed. A job that ends before
    /// its start's set is handed over (while another
    /// [`on_change`](Self::on_change) call runs, say) may be in no set it
    /// hears: its sets were folded into the newest.
    fn on_exit(&self, _job: &Job) {}
}

/// [`JobEvents`] that hears nothing: what
/// [`JobHub::new`](super::JobHub::new) uses.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoJobEvents;
impl JobEvents for NoJobEvents {}

haest_core::plug_ref! {
    /// [`JobEvents`], shared: what [`JobHub::set_events`](super::JobHub::set_events)
    /// takes. Built from the listener itself, or from an `Arc` of one the host
    /// keeps too.
    pub struct JobEventsRef(dyn JobEvents);
}
