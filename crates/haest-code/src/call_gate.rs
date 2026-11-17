//! Concurrency for the tool calls one Code run makes: a call that changes
//! anything runs ALONE, read-only calls share the gate up to a cap. The same rule
//! haest-core's loop applies to a model's batch, held per call instead of per batch.
//!
//! [`CallGate`] is what the Code tool holds for each run.

use std::{
    future::Future,
    pin::pin,
    sync::{Mutex, MutexGuard},
};

use tokio::sync::Notify;

#[derive(Default)]
struct State {
    reads: usize,
    writing: bool,
}

/// A readers-writer gate for tool calls: an exclusive call (one that writes)
/// runs alone, shared calls (reads) run together up to a cap.
///
/// A write waits for every read in flight to finish, and while it runs
/// nothing else starts. Waiters are not queued in order: when the gate frees,
/// any of them may go first.
pub(crate) struct CallGate {
    max_reads: usize,
    state: Mutex<State>,
    changed: Notify,
}

/// Gives the gate back however the call ends: finished, failed, or dropped mid-way.
struct Held<'a> {
    gate: &'a CallGate,
    exclusive: bool,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        let mut s = self.gate.state();
        if self.exclusive {
            s.writing = false;
        } else {
            s.reads -= 1;
        }
        drop(s);
        self.gate.changed.notify_waiters();
    }
}

impl CallGate {
    /// A gate that lets at most `max_reads` shared calls run at once. With 0,
    /// no shared call ever starts.
    pub(crate) fn new(max_reads: usize) -> Self {
        Self {
            max_reads,
            state: Mutex::default(),
            changed: Notify::new(),
        }
    }

    /// Runs `call` once the gate lets it through: alone when `exclusive`, else
    /// beside other reads up to the cap. Returns what `call` returns.
    ///
    /// The place is given back however the call ends: finished, panicked, or
    /// the returned future dropped mid-way (a cancelled run). Dropping it
    /// while it still waits takes no place at all.
    pub(crate) async fn run<T>(&self, exclusive: bool, call: impl Future<Output = T>) -> T {
        let _held = self.enter(exclusive).await;
        call.await
    }

    /// Who is through the gate. Its lock is held only to count, never while a
    /// call runs, so a poisoned one is a bug here.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("the call gate's lock poisoned")
    }

    async fn enter(&self, exclusive: bool) -> Held<'_> {
        loop {
            // Registered BEFORE the check, so a release between check and wait still wakes us.
            let mut changed = pin!(self.changed.notified());
            changed.as_mut().enable();
            {
                let mut s = self.state();
                let busy = s.writing
                    || if exclusive {
                        s.reads > 0
                    } else {
                        s.reads >= self.max_reads
                    };
                if !busy {
                    if exclusive {
                        s.writing = true;
                    } else {
                        s.reads += 1;
                    }
                    return Held {
                        gate: self,
                        exclusive,
                    };
                }
            }
            changed.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use super::*;

    async fn probe(gate: &CallGate, exclusive: bool, live: &AtomicUsize, peak: &AtomicUsize) {
        gate.run(exclusive, async {
            peak.fetch_max(live.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            live.fetch_sub(1, Ordering::SeqCst);
        })
        .await;
    }

    async fn peak_of(gate: &CallGate, exclusive: bool, n: usize) -> usize {
        let (live, peak) = (AtomicUsize::new(0), AtomicUsize::new(0));
        futures::future::join_all((0..n).map(|_| probe(gate, exclusive, &live, &peak))).await;
        peak.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn writes_run_alone_and_reads_share_up_to_the_cap() {
        let gate = CallGate::new(3);
        assert_eq!(peak_of(&gate, true, 4).await, 1);
        assert_eq!(peak_of(&gate, false, 6).await, 3);
    }

    #[tokio::test]
    async fn a_write_waits_for_the_reads_and_a_dropped_call_gives_the_gate_back() {
        let gate = Arc::new(CallGate::new(8));
        let held = gate.clone();
        let reading = tokio::spawn(async move {
            held.run(false, tokio::time::sleep(Duration::from_secs(60)))
                .await;
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        let write = gate.run(true, async { "wrote" });
        let mut write = pin!(write);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), write.as_mut())
                .await
                .is_err()
        );
        reading.abort();
        assert_eq!(write.await, "wrote");
    }
}
