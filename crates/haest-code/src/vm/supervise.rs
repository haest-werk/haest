//! haest's side of one running engine: answer its calls, pass its beats on, and
//! stop it -- on the human's interrupt, at its deadline, or by abandoning it when
//! it does not stop within [`super::STOP_GRACE`].

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

use super::{
    STOP_GRACE, VmSpec,
    thread::Engine,
    types::{FromVm, ToVm, VmBeat, VmRun, VmStop},
};

type Answering = FuturesUnordered<BoxFuture<'static, (u64, Result<String, String>)>>;

struct Stopper {
    to_vm: mpsc::Sender<ToVm>,
    stop: Arc<AtomicBool>,
    reason: Option<VmStop>,
    kill_at: Option<Instant>,
}

impl Stopper {
    /// Cooperative first: the flag (a CPU loop sees it) and a message (a run parked on calls wakes).
    fn request(&mut self, why: VmStop) {
        if self.reason.is_some() {
            return;
        }
        self.reason = Some(why);
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.to_vm.send(ToVm::Stop);
        self.kill_at = Some(Instant::now() + STOP_GRACE);
    }

    fn finish(&self, run: VmRun) -> VmRun {
        match self.reason {
            Some(why) => VmRun {
                stopped: Some(why),
                ..run
            },
            None => run,
        }
    }
}

pub(super) async fn supervise(
    engine: Engine,
    spec: &VmSpec,
    stop: Arc<AtomicBool>,
    cancel: &CancellationToken,
    on_beat: &(dyn Fn(VmBeat) + Send + Sync),
) -> VmRun {
    let deadline = Instant::now() + Duration::from_millis(spec.caps.timeout_ms);
    let mut answering = Answering::new();
    let Engine { to_vm, mut from_vm } = engine;
    let mut stopper = Stopper {
        to_vm,
        stop,
        reason: None,
        kill_at: None,
    };
    loop {
        tokio::select! {
            msg = from_vm.recv() => match msg {
                Some(FromVm::Done(run)) => return stopper.finish(run),
                Some(FromVm::Beat(b)) => on_beat(b),
                Some(FromVm::Call { call_id, name, args }) => match spec.native_fns.get(&name) {
                    Some(f) => answering.push(f(args).map(move |r| (call_id, r.map_err(|e| e.to_string()))).boxed()),
                    None => {
                        let text = format!("no native function {name}");
                        let _ = stopper.to_vm.send(ToVm::Answer { call_id, ok: false, text });
                    }
                },
                None => {
                    return stopper.finish(VmRun::failed("haest's JS engine ended without a result"));
                }
            },
            Some((call_id, answer)) = answering.next(), if !answering.is_empty() => {
                let (ok, text) = match answer {
                    Ok(text) => (true, text),
                    Err(text) => (false, text),
                };
                let _ = stopper.to_vm.send(ToVm::Answer { call_id, ok, text });
            }
            () = cancel.cancelled(), if stopper.reason.is_none() => stopper.request(VmStop::Interrupted),
            () = sleep_until(deadline), if stopper.reason.is_none() => stopper.request(VmStop::Timeout),
            () = sleep_until(stopper.kill_at.unwrap_or(deadline)), if stopper.kill_at.is_some() => {
                return stopper.finish(VmRun { killed: true, ..VmRun::default() });
            }
        }
    }
}
