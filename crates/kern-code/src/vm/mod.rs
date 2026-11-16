//! THE engine behind kern's JavaScript: QuickJS (native, via `rquickjs`) on a
//! thread of its own, one per run. Two entry points share it -- the Code tool
//! ([`crate::code`], the body of an async function that may `return`) and the
//! sandbox `js` command ([`crate::sandbox_js`], an ES module file with imports).
//!
//! Why a thread: a CPU-bound guest blocks the thread it runs on, so on the async
//! runtime neither the human's interrupt nor a timer could reach it. On a thread
//! of its own the stop flag ends it within one interrupt poll, its output kept,
//! and the runtime stays live.
//!
//! STOPPING, two steps: set the stop flag and send `Stop` (cooperative -- the run
//! returns normally, logs included); no answer within [`STOP_GRACE`] -> the run is
//! abandoned (whatever it printed is lost, and the result says so with `killed`).
//! A thread cannot be killed from outside; an abandoned one keeps its stop flag
//! set and ends at its next interrupt poll. At most [`MAX_ENGINES`] runs at once.
//!
//! The guest has NO host reach of its own: what it can do is exactly the
//! [`VmSpec`] -- JSON values, a prelude, async functions answered here, module sources.

mod engine;
mod host;
mod parts;
mod path;
mod setup;
mod supervise;
#[cfg(test)]
mod tests;
mod types;

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::future::BoxFuture;
use serde_json::{Map, Value};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

pub use types::{VmBeat, VmLimits, VmRun, VmStop};

use host::spawn_engine;
use types::VmJob;

/// What a host function answers: the text, or the message of the Error the guest sees.
pub type HostFuture = BoxFuture<'static, Result<String, String>>;
/// A guest function kern answers later: a promise in the guest, its args as strings.
pub type HostFn = Arc<dyn Fn(Vec<String>) -> HostFuture + Send + Sync>;

/// What one run's guest gets, and nothing more.
#[derive(Clone, Default)]
pub struct VmSpec {
    /// Runs before the code, after the base prelude (console, `__settle`): builds the guest API.
    pub prelude: String,
    /// JSON globals, set before the prelude.
    pub values: Map<String, Value>,
    /// Answered here, later: a promise in the guest; an `Err` is an Error it can catch.
    pub host_fns: BTreeMap<String, HostFn>,
    /// Run as an ES module; these are the importable modules (name -> source).
    pub modules: Option<HashMap<String, String>>,
    /// Shown in stacks and error lines; default `code.js`.
    pub filename: Option<String>,
}

pub const VM_MEMORY_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ENGINES: usize = 4;
pub const STOP_GRACE: Duration = Duration::from_millis(500);

/// At most MAX_ENGINES runs at once; the rest wait their turn (or their abort).
static ENGINES: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(MAX_ENGINES));

/// Run `code` against `spec`. Never fails: every way a run can end is a [`VmRun`].
/// `cancel` is the human's interrupt; `on_beat` hears about once a second how a long run is doing.
pub async fn run_in_vm(
    code: &str,
    spec: &VmSpec,
    limits: VmLimits,
    cancel: &CancellationToken,
    on_beat: Option<&(dyn Fn(VmBeat) + Send + Sync)>,
) -> VmRun {
    run_with_slots(&ENGINES, code, spec, limits, cancel, on_beat).await
}

/// A dropped run future stops its engine too.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

pub(crate) async fn run_with_slots(
    slots: &Semaphore,
    code: &str,
    spec: &VmSpec,
    limits: VmLimits,
    cancel: &CancellationToken,
    on_beat: Option<&(dyn Fn(VmBeat) + Send + Sync)>,
) -> VmRun {
    let _slot = tokio::select! {
        biased;
        slot = slots.acquire() => match slot {
            Ok(slot) => slot,
            Err(_) => return VmRun::failed("kern's JS engine pool is closed"),
        },
        () = cancel.cancelled() => return VmRun::stopped(VmStop::Interrupted),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let _stop_on_drop = StopOnDrop(stop.clone());
    let job = VmJob {
        code: code.to_string(),
        prelude: spec.prelude.clone(),
        values: spec.values.clone(),
        async_names: spec.host_fns.keys().cloned().collect(),
        modules: spec.modules.clone(),
        filename: spec.filename.clone().unwrap_or_else(|| "code.js".into()),
        limits,
        stop: stop.clone(),
    };
    match spawn_engine(job) {
        Ok(engine) => supervise::supervise(engine, spec, limits, stop, cancel, on_beat).await,
        Err(e) => VmRun::failed(format!("kern's JS engine could not start: {e}")),
    }
}
