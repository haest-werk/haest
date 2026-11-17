//! [`JobEvents`]: what a host hears about its jobs, for its transcript.

use super::Job;

/// What a job printed, as [`JobEvents::on_output`] hears it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobOutput<'a> {
    /// Output as it arrived.
    Chunk(&'a str),
    /// The job ended: nothing more will come. Not sent once
    /// [`on_output`](JobEvents::on_output) panicked on this job's output.
    Done,
}

/// What a host hears about jobs, for its transcript. All optional.
///
/// Called as things happen (a start from
/// [`JobHub::start`](super::JobHub::start), the rest from the job's own
/// task), never while the hub's table is locked: a handler may call the hub
/// back.
///
/// A panic in a handler stops nothing: the job runs to its end with its own
/// exit code and is reported ended (the running set,
/// [`on_exit`](Self::on_exit), the model's notice through the inbox), one
/// call's panic skipping none of the others. Then one panic goes on, the
/// first: out of [`JobHub::start`](super::JobHub::start) when it was the
/// start's [`on_change`](Self::on_change) (the job is started all the same),
/// else on the job's own task. Once [`on_output`](Self::on_output) panicked,
/// it hears no more of that job's output, [`JobOutput::Done`] included (the
/// buffer still keeps it).
pub trait JobEvents: Send + Sync {
    /// Every start and stop: the full set of running jobs, never a delta.
    /// One call at a time, and never an older set after a newer one: the
    /// changes made while one call runs are handed over next as one, the
    /// newest. So the last set it hears is the hub's.
    fn on_change(&self, _running: &[Job]) {}
    /// Output as it arrives, then [`JobOutput::Done`] once when the job
    /// ended; nothing more of a job once a call for it panicked.
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
