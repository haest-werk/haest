//! [`JobEvents`]: what a host hears about its jobs, for its transcript.

use super::Job;

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
/// Called as things happen (a start from
/// [`JobHub::start`](super::JobHub::start), the rest from the job's own
/// task), never while the hub's table is locked: a handler may call the hub
/// back.
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
