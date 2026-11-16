//! Concurrency for the tool calls one Code run makes: a call that changes
//! anything runs ALONE, read-only calls share the gate up to a cap. The same rule
//! `haest_core::tool_batch` applies to a model's batch, held per call instead of per batch.

use std::{future::Future, pin::pin, sync::Mutex};

use tokio::sync::Notify;

#[derive(Default)]
struct State {
    reads: usize,
    writing: bool,
}

pub struct CallGate {
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
        let mut s = self.gate.state.lock().unwrap();
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
    pub fn new(max_reads: usize) -> Self {
        Self {
            max_reads,
            state: Mutex::default(),
            changed: Notify::new(),
        }
    }

    /// Run `call` once the gate lets it through: alone when `exclusive`, else
    /// beside other reads up to the cap.
    pub async fn run<T>(&self, exclusive: bool, call: impl Future<Output = T>) -> T {
        let _held = self.enter(exclusive).await;
        call.await
    }

    async fn enter(&self, exclusive: bool) -> Held<'_> {
        loop {
            // Registered BEFORE the check, so a release between check and wait still wakes us.
            let mut changed = pin!(self.changed.notified());
            changed.as_mut().enable();
            {
                let mut s = self.state.lock().unwrap();
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
