//! When the hub has nothing in flight: every spawn is open from the call
//! until its answer is back (refused, returned, or handed back), and a pulse
//! goes out each time one closes.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{sync::watch, time::Instant};

use super::SubagentHub;

/// Spawns not finished yet, and a pulse bumped as each one finishes.
#[derive(Default)]
pub(super) struct Work {
    open: AtomicUsize,
    pulse: watch::Sender<u64>,
}

/// One spawn in flight: finished when dropped, whichever way it ends.
pub(super) struct Open(Arc<Work>);

impl Open {
    pub(super) fn begin(work: &Arc<Work>) -> Self {
        work.open.fetch_add(1, Ordering::SeqCst);
        Self(work.clone())
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
        self.0.pulse.send_modify(|n| *n = n.wrapping_add(1));
    }
}

impl SubagentHub {
    /// Nothing in flight: no subagent starting or running, and every
    /// background answer handed back (to its starter, or to the parent's
    /// [`Inbox::submit_answer`](crate::Inbox::submit_answer)).
    pub fn idle(&self) -> bool {
        self.0.work.open.load(Ordering::SeqCst) == 0
    }

    /// Wait until the hub is [idle](Self::idle), at most `timeout`: true once
    /// it is (at once when it already is), false when the time ran out first.
    /// A host that waits for its subagents (to go on with main once every
    /// answer is in, or before a one-shot run exits) waits here.
    /// `Duration::MAX` waits for as long as it takes.
    pub async fn settled(&self, timeout: Duration) -> bool {
        let mut pulse = self.0.work.pulse.subscribe();
        let deadline = Instant::now().checked_add(timeout);
        while !self.idle() {
            let changed = match deadline {
                Some(at) => tokio::time::timeout_at(at, pulse.changed()).await.ok(),
                None => Some(pulse.changed().await),
            };
            // Out of time (or no pulse can come any more): say how it is now.
            if !matches!(changed, Some(Ok(()))) {
                return self.idle();
            }
        }
        true
    }
}
