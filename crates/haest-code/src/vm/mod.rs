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
    fmt,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::{FutureExt, future::BoxFuture};
use haest_core::error::BoxError;
use serde_json::{Map, Value};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

pub use types::{VmBeat, VmCaps, VmRun, VmStop};

use thread::spawn_engine;
use types::VmJob;

/// What a native function answers: `Ok` resolves the guest's promise with the
/// text, `Err` rejects it with an `Error` whose message is the error's text.
pub(crate) type NativeFuture = BoxFuture<'static, Result<String, BoxError>>;
/// A guest function haest answers later: a promise in the guest, its args as
/// strings (each one `String(arg)`; pass objects as JSON text).
pub(crate) type NativeFn = Arc<dyn Fn(Vec<String>) -> NativeFuture + Send + Sync>;

/// What one run's guest gets, and nothing more.
///
/// [`VmSpec::new`] (and `Default`) is a bare run in function mode: `console`,
/// no globals, no host functions. The `with_*` setters add each part.
#[derive(Clone, Default)]
pub struct VmSpec {
    /// Runs before the code, after the base prelude (console, `__settle`): builds the guest API.
    pub(crate) prelude: String,
    /// JSON globals, set before the prelude.
    pub(crate) values: Map<String, Value>,
    /// Global functions answered here, later, by name: a promise in the
    /// guest; an `Err` is an Error it can catch.
    pub(crate) native_fns: BTreeMap<String, NativeFn>,
    /// None: function mode. Some: module mode, with the modules it may import.
    pub(crate) modules: Option<HashMap<String, String>>,
    /// Shown in stacks and error lines; default `code.js`.
    pub(crate) filename: Option<String>,
}

impl VmSpec {
    /// A bare run in function mode: the code is the body of an async
    /// function, and its `return` value is [`VmRun::value`].
    pub fn new() -> Self {
        Self::default()
    }

    /// JavaScript that runs before the code, after the base prelude
    /// (`console`): where a host builds the guest's API.
    pub fn with_prelude(mut self, prelude: impl Into<String>) -> Self {
        self.prelude = prelude.into();
        self
    }

    /// The global `name`, set to `value` before the prelude runs.
    pub fn with_value(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.values.insert(name.into(), value.into());
        self
    }

    /// The global function `name`, answered here, later: a promise in the
    /// guest. `f` gets the call's args as strings (each one `String(arg)`;
    /// pass objects as JSON text). Its `Ok` resolves the promise with the
    /// text; its `Err` rejects it with an `Error` whose message is the
    /// error's text (a `String` turns into one with `.into()`), which the
    /// guest can catch. A second function of the same name replaces the first.
    pub fn with_native_fn<F, Fut>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: Fn(Vec<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, BoxError>> + Send + 'static,
    {
        let f: NativeFn = Arc::new(move |args| f(args).boxed());
        self.native_fns.insert(name.into(), f);
        self
    }

    /// Module mode: the code is an ES module, and `modules` are the ones it
    /// may import (exact name -> source). A module-mode run has no value.
    pub fn with_modules(
        mut self,
        modules: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        let modules = modules.into_iter().map(|(n, s)| (n.into(), s.into()));
        self.modules = Some(modules.collect());
        self
    }

    /// The name stacks and error lines show, in place of `code.js`.
    pub fn with_filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }
}

/// The parts by name only: a native function has nothing to show.
impl fmt::Debug for VmSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmSpec")
            .field("values", &self.values.keys().collect::<Vec<_>>())
            .field("native_fns", &self.native_fns.keys().collect::<Vec<_>>())
            .field(
                "modules",
                &self.modules.as_ref().map(|m| m.keys().collect::<Vec<_>>()),
            )
            .field("filename", &self.filename)
            .finish_non_exhaustive()
    }
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
/// use haest_code::{VmCaps, VmSpec, run_in_vm};
/// use tokio_util::sync::CancellationToken;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// // One native function: the guest's `shout(text)` resolves to the text in capitals.
/// let spec = VmSpec::new()
///     .with_native_fn("shout", |args| async move {
///         let text = args.first().cloned().unwrap_or_default();
///         Ok(text.to_uppercase())
///     })
///     .with_value("WHO", "haest");
///
/// let caps = VmCaps { timeout_ms: 5_000, memory_bytes: 16 << 20, max_log_chars: 10_000 };
/// let code = "console.log('working'); return { said: await shout('hi ' + WHO) }";
/// let run = run_in_vm(code, &spec, caps, &CancellationToken::new(), None).await;
///
/// assert_eq!(run.error, None);
/// assert_eq!(run.stdout, "working\n");
/// let value: serde_json::Value = serde_json::from_str(run.value.as_deref().ok_or("no value")?)?;
/// assert_eq!(value["said"], "HI HAEST");
/// # Ok(())
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
