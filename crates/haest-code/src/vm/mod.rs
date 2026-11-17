//! THE engine behind haest's JavaScript: QuickJS (native, via `rquickjs`) on a
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
//! The guest reaches nothing of its own: what it can do is exactly the
//! [`VmSpec`] -- JSON values, a prelude, async functions answered here, module
//! sources. Every run also gets `console` (into the [`VmRun`]'s logs) and a
//! pure posix `path` helper the prelude can build on; nothing else: no file
//! system, network, timer or process.
//!
//! Start from [`run_in_vm`]; [`VmCaps`] bounds a run and [`VmRun`] is how
//! it ended.

mod engine;
mod parts;
mod path;
mod setup;
mod supervise;
#[cfg(test)]
mod tests;
mod thread;
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

pub use types::{VmBeat, VmCaps, VmRun, VmStop};

use thread::spawn_engine;
use types::VmJob;

/// What a native function answers: `Ok` resolves the guest's promise with the
/// text, `Err` rejects it with an `Error` whose message is the error's text
/// (its `Display`; a `String` turns into one with `.into()`).
pub type NativeFuture = BoxFuture<'static, Result<String, haest_core::error::BoxError>>;
/// A guest function haest answers later: a promise in the guest, its args as
/// strings (each one `String(arg)`; pass objects as JSON text).
pub type NativeFn = Arc<dyn Fn(Vec<String>) -> NativeFuture + Send + Sync>;

/// What one run's guest gets, and nothing more.
///
/// The default is a bare run in function mode: `console`, no globals, no host
/// functions.
#[derive(Clone, Default)]
pub struct VmSpec {
    /// Runs before the code, after the base prelude (console, `__settle`): builds the guest API.
    pub prelude: String,
    /// JSON globals, set before the prelude.
    pub values: Map<String, Value>,
    /// Global functions answered here, later, by name: a promise in the
    /// guest; an `Err` is an Error it can catch.
    pub native_fns: BTreeMap<String, NativeFn>,
    /// None: function mode -- the code is the body of an async function, and
    /// its `return` value is [`VmRun::value`]. Some: module mode -- the code is
    /// an ES module, and these are the modules it may import (exact name ->
    /// source); there is no value.
    pub modules: Option<HashMap<String, String>>,
    /// Shown in stacks and error lines; default `code.js`.
    pub filename: Option<String>,
}

/// The default memory limit of a run's engine, 64 MiB (the Code tool's and
/// the sandbox `js` command's).
pub const VM_MEMORY_BYTES: usize = 64 * 1024 * 1024;
/// The most runs at once in this process (4); [`run_in_vm`] waits for a free
/// one.
pub const MAX_ENGINES: usize = 4;
/// How long a run asked to stop has before it is abandoned and reported
/// [`killed`](VmRun::killed).
pub const STOP_GRACE: Duration = Duration::from_millis(500);

/// At most MAX_ENGINES runs at once; the rest wait their turn (or their abort).
static ENGINES: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(MAX_ENGINES));

/// Runs `code` against `spec` on an engine thread of its own and waits for it
/// to end. Never fails: every way a run can end is a [`VmRun`].
///
/// The run ends when the code has settled AND every native function call it
/// started has answered -- a call nobody awaited still finishes -- or when
/// it is stopped. Code that awaits a promise nothing can settle ends at once
/// with an error saying so.
///
/// `caps` bounds it: [`VmCaps::timeout_ms`] stops it (wall clock, from
/// when the engine starts, waiting on native functions included), memory past
/// [`VmCaps::memory_bytes`] is an error inside the guest, and console
/// output past [`VmCaps::max_log_chars`] is dropped.
///
/// `cancel` is the human's interrupt: it stops the run, even a CPU loop,
/// within one interrupt poll, and what it printed is kept
/// ([`VmStop::Interrupted`]). A run that does not stop within [`STOP_GRACE`]
/// is abandoned ([`VmRun::killed`]). Cancelled while waiting for one of the
/// [`MAX_ENGINES`] slots, it returns at once, stopped. Dropping the returned
/// future stops the run too.
///
/// `on_beat` hears, about once a second and on the task awaiting this call,
/// how a long run is doing.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use futures::FutureExt;
/// use haest_code::{NativeFn, VmCaps, VmSpec, run_in_vm};
/// use tokio_util::sync::CancellationToken;
///
/// # #[tokio::main]
/// # async fn main() {
/// // One native function: the guest's `shout(text)` resolves to the text in capitals.
/// let shout: NativeFn = Arc::new(|args| {
///     let text = args.first().cloned().unwrap_or_default();
///     async move { Ok(text.to_uppercase()) }.boxed()
/// });
/// let mut spec = VmSpec::default();
/// spec.native_fns.insert("shout".into(), shout);
/// spec.values.insert("WHO".into(), "haest".into());
///
/// let caps = VmCaps { timeout_ms: 5_000, memory_bytes: 16 << 20, max_log_chars: 10_000 };
/// let code = "console.log('working'); return { said: await shout('hi ' + WHO) }";
/// let run = run_in_vm(code, &spec, caps, &CancellationToken::new(), None).await;
///
/// assert_eq!(run.error, None);
/// assert_eq!(run.stdout, "working\n");
/// let value: serde_json::Value = serde_json::from_str(run.value.as_deref().unwrap()).unwrap();
/// assert_eq!(value["said"], "HI HAEST");
/// # }
/// ```
pub async fn run_in_vm(
    code: &str,
    spec: &VmSpec,
    caps: VmCaps,
    cancel: &CancellationToken,
    on_beat: Option<&(dyn Fn(VmBeat) + Send + Sync)>,
) -> VmRun {
    run_with_slots(&ENGINES, code, spec, caps, cancel, on_beat).await
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
    caps: VmCaps,
    cancel: &CancellationToken,
    on_beat: Option<&(dyn Fn(VmBeat) + Send + Sync)>,
) -> VmRun {
    let _slot = tokio::select! {
        biased;
        slot = slots.acquire() => match slot {
            Ok(slot) => slot,
            Err(_) => return VmRun::failed("haest's JS engine pool is closed"),
        },
        () = cancel.cancelled() => return VmRun::stopped(VmStop::Interrupted),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let _stop_on_drop = StopOnDrop(stop.clone());
    let job = VmJob {
        code: code.to_string(),
        prelude: spec.prelude.clone(),
        values: spec.values.clone(),
        async_names: spec.native_fns.keys().cloned().collect(),
        modules: spec.modules.clone(),
        filename: spec.filename.clone().unwrap_or_else(|| "code.js".into()),
        caps,
        stop: stop.clone(),
    };
    match spawn_engine(job) {
        Ok(engine) => supervise::supervise(engine, spec, caps, stop, cancel, on_beat).await,
        Err(e) => VmRun::failed(format!("haest's JS engine could not start: {e}")),
    }
}
