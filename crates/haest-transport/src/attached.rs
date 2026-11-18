//! [`Attached`]: a transport serving on a task of its own, stopped when it
//! is detached or dropped.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::task::JoinHandle;

use crate::Ended;

/// An attached transport. Dropping it detaches it, as [`detach`](Self::detach) does.
#[derive(Debug)]
#[must_use = "dropping it detaches the transport"]
pub struct Attached {
    task: Option<JoinHandle<Ended>>,
    detached: StopFlag,
}

impl Attached {
    /// The transport running on `task`.
    ///
    /// Detaching aborts `task`. An abort lands at the task's next poll: a
    /// task being polled on another worker at that moment finishes that
    /// poll, so a frame the conversation makes in that instant may still go
    /// out. The transports in this crate also tell their connections to
    /// write nothing more.
    pub fn new(task: JoinHandle<Ended>) -> Self {
        Self::stopping(task, StopFlag::default())
    }

    /// The transport running on `task`, whose connections write nothing
    /// more once `detached` is set.
    pub(crate) fn stopping(task: JoinHandle<Ended>, detached: StopFlag) -> Self {
        Self {
            task: Some(task),
            detached,
        }
    }

    /// Wait for it to end on its own: why it did. Given up on before it
    /// resolves (raced against a timeout, its task aborted), it stops the
    /// transport, as dropping the `Attached` does.
    pub async fn ended(mut self) -> Ended {
        // Awaited in place: dropping this wait drops `self`, which stops it.
        let Some(task) = self.task.as_mut() else {
            return Ended::Closed;
        };
        match task.await {
            Ok(ended) => ended,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            // The runtime is shutting down.
            Err(_) => Ended::Closed,
        }
    }

    /// Stop serving now. The conversation goes on; nothing it makes from
    /// here on goes out, frames not yet written are dropped, and a line
    /// being written may be cut short (a reader skips a last line with no
    /// newline).
    pub fn detach(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        // Set first: a connection in the middle of a step on another worker
        // runs that step to its end before the abort lands, and checks the
        // flag before each line it writes.
        self.detached.set();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Set when an [`Attached`] stops its transport. A connection's writer
/// checks it before each line, so nothing the conversation makes after a
/// detach goes out, also from a step that was being run at that moment.
#[derive(Debug, Clone, Default)]
pub(crate) struct StopFlag(Arc<AtomicBool>);

impl StopFlag {
    fn set(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn is_set(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
