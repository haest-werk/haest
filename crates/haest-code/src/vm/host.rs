//! The engine thread and haest's side of its protocol ([`super::types`]): one
//! job in; haest's [`ToVm::Answer`]s and a [`ToVm::Stop`] while it runs;
//! [`FromVm::Call`]s, [`FromVm::Beat`]s and exactly one [`FromVm::Done`] out.
//!
//! One thread per run, so the async runtime is never blocked by a guest and a
//! CPU-bound guest never starves a neighbour. A thread cannot be killed from
//! outside: the stop flag ends it at its next interrupt poll, which QuickJS makes
//! inside every loop and every regex step.

use std::{
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use haest_core::panics::panic_message;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::{
    engine::run_engine,
    types::{FromVm, ToVm, VmBeat, VmJob, VmRun},
};

/// Room for QuickJS's own 512 KB stack limit plus the Rust frames around it.
const THREAD_STACK_BYTES: usize = 8 * 1024 * 1024;

/// haest's end of one engine thread.
pub(crate) struct Engine {
    pub to_vm: mpsc::Sender<ToVm>,
    pub from_vm: UnboundedReceiver<FromVm>,
}

/// The engine thread's end: how a run asks haest for something and hears back.
pub(crate) struct VmPort {
    to_haest: UnboundedSender<FromVm>,
    from_haest: mpsc::Receiver<ToVm>,
    stop: Arc<AtomicBool>,
}

impl VmPort {
    /// How a guest function asks haest to answer it; the answer arrives through [`Self::wait`].
    pub fn caller(&self) -> impl Fn(u64, String, Vec<String>) + 'static {
        let to = self.to_haest.clone();
        move |call_id, name, args| {
            let _ = to.send(FromVm::Call {
                call_id,
                name,
                args,
            });
        }
    }

    /// A beat sender the interrupt handler can own.
    pub fn beats(&self) -> impl Fn(VmBeat) + 'static {
        let to = self.to_haest.clone();
        move |b| {
            let _ = to.send(FromVm::Beat(b));
        }
    }

    /// haest's next message, waiting at most `timeout`. A stop -- or haest gone
    /// away -- also sets the stop flag, so the run sees it however it asks.
    pub fn wait(&self, timeout: Duration) -> Option<ToVm> {
        match self.from_haest.recv_timeout(timeout) {
            Ok(m) => Some(self.seen(m)),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(self.seen(ToVm::Stop)),
        }
    }

    /// A message already waiting, without blocking.
    pub fn try_next(&self) -> Option<ToVm> {
        self.from_haest.try_recv().ok().map(|m| self.seen(m))
    }

    fn seen(&self, m: ToVm) -> ToVm {
        if matches!(m, ToVm::Stop) {
            self.stop.store(true, Ordering::SeqCst);
        }
        m
    }
}

/// Start one run on a thread of its own. An engine crash is an answer too: a
/// thread that never posts `Done` would hold the run until its deadline.
pub(crate) fn spawn_engine(job: VmJob) -> io::Result<Engine> {
    let (to_vm, from_haest) = mpsc::channel();
    let (to_haest, from_vm) = unbounded_channel();
    let port = VmPort {
        to_haest: to_haest.clone(),
        from_haest,
        stop: job.stop.clone(),
    };
    thread::Builder::new()
        .name("haest-js".into())
        .stack_size(THREAD_STACK_BYTES)
        .spawn(move || {
            let run = catch_unwind(AssertUnwindSafe(|| run_engine(&job, &port))).unwrap_or_else(
                |panic| {
                    let said = panic_message(panic.as_ref()).unwrap_or("unknown panic");
                    VmRun::failed(format!("haest's JS engine crashed: {said}"))
                },
            );
            let _ = to_haest.send(FromVm::Done(run));
        })?;
    Ok(Engine { to_vm, from_vm })
}
