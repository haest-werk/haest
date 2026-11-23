//! THE QuickJS engine under all of haest's JavaScript. A Code tool run and a
//! sandbox `js` script ([`crate::vm`]) and each of haest-bridges' mod files
//! run on a [`JsEngine`] of their own, on a thread of their own
//! ([`spawn_thread`]), bounded by one caps type, [`VmCaps`].
//!
//! An engine is one QuickJS runtime and its context, held to
//! [`VmCaps::memory_bytes`] of heap and a stack cap, and its [`Watch`]: the
//! interrupt QuickJS polls inside every loop and every regex step. While the
//! watch is armed, a poll stops the guest once its stop flag is set or the
//! stretch it was armed for has run out. A Code run arms it once, for the
//! whole run ([`VmCaps::timeout`]); a mod file arms it for each stretch of
//! its code ([`JsEngine::timed`], [`VmCaps::slice`]).
//!
//! What stays with each caller is what differs for a reason: the loader (a
//! Code run in module mode imports, a mod file does not), the stack cap, who
//! holds the stop flag, and the protocol on top.

use std::{
    cell::{Cell, RefCell},
    fmt, io,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use haest_core::__private::panic_message;
use rquickjs::{Context, Runtime};

use crate::vm::VmCaps;

/// An engine thread's stack: room for QuickJS's own stack cap plus the Rust
/// frames around it.
pub(crate) const THREAD_STACK_BYTES: usize = 8 * 1024 * 1024;

/// One QuickJS runtime and its context, held to its caps, with its interrupt
/// watch. Make it on the thread that runs it: QuickJS measures its stack from
/// there.
pub struct JsEngine {
    // Field order is drop order: the context goes before its runtime.
    context: Context,
    runtime: Runtime,
    watch: Rc<Watch>,
}

impl fmt::Debug for JsEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsEngine").finish_non_exhaustive()
    }
}

impl JsEngine {
    /// A fresh runtime held to `caps.memory_bytes` of heap and `stack_bytes`
    /// of stack, its context, and its watch, disarmed. `stop` is the watch's
    /// stop flag: whoever else holds it stops the guest at its next poll
    /// while the watch is armed.
    ///
    /// # Errors
    ///
    /// QuickJS could not make the runtime or the context.
    pub fn new(caps: &VmCaps, stack_bytes: usize, stop: Arc<AtomicBool>) -> rquickjs::Result<Self> {
        let runtime = Runtime::new()?;
        runtime.set_memory_limit(caps.memory_bytes);
        runtime.set_max_stack_size(stack_bytes);
        let watch = Rc::new(Watch::new(stop));
        let polled = watch.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || polled.poll())));
        let context = Context::full(&runtime)?;
        Ok(Self {
            context,
            runtime,
            watch,
        })
    }

    /// The engine's one context.
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// The engine's runtime: its job queue, memory use, loader.
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    pub(crate) fn watch(&self) -> &Watch {
        &self.watch
    }

    /// Runs `f` as one stretch of guest code: the watch stops the guest once
    /// it has run `limit`. Hands back what `f` returned and how long it ran.
    pub fn timed<T>(&self, limit: Duration, f: impl FnOnce() -> T) -> (T, Duration) {
        self.watch.arm(Instant::now(), limit);
        let out = f();
        (out, self.watch.disarm())
    }
}

impl Drop for JsEngine {
    /// What the interrupt holds goes before the runtime does.
    fn drop(&mut self) {
        self.watch.disarm();
        self.watch.on_poll.take();
        self.runtime.set_interrupt_handler(None);
    }
}

/// What stops the guest mid-run. QuickJS polls it inside every loop and
/// every regex step: an atomic stop flag reaches a CPU loop where no message
/// could, and the clock is read where the guest is running.
pub(crate) struct Watch {
    stop: Arc<AtomicBool>,
    /// The armed stretch: when it began and how long it may run. None: the
    /// watch is disarmed and the guest runs on.
    armed: Cell<Option<(Instant, Duration)>>,
    /// Called at every poll while armed: a Code run's heartbeat.
    on_poll: RefCell<Option<Box<dyn Fn()>>>,
}

impl Watch {
    fn new(stop: Arc<AtomicBool>) -> Self {
        Self {
            stop,
            armed: Cell::new(None),
            on_poll: RefCell::new(None),
        }
    }

    /// From now on, stop the guest once `limit` has passed since `since`, or
    /// once the stop flag is set.
    pub(crate) fn arm(&self, since: Instant, limit: Duration) {
        self.armed.set(Some((since, limit)));
    }

    /// Stop watching; how long the stretch ran since it was armed.
    pub(crate) fn disarm(&self) -> Duration {
        self.armed
            .take()
            .map_or(Duration::ZERO, |(since, _)| since.elapsed())
    }

    /// `f` runs at every poll while the watch is armed.
    pub(crate) fn on_poll(&self, f: impl Fn() + 'static) {
        *self.on_poll.borrow_mut() = Some(Box::new(f));
    }

    /// True = stop the guest now.
    fn poll(&self) -> bool {
        let Some((since, limit)) = self.armed.get() else {
            return false;
        };
        if let Some(f) = &*self.on_poll.borrow() {
            f();
        }
        self.stop.load(Ordering::SeqCst) || since.elapsed() > limit
    }
}

/// Runs `body` on a thread of its own, named `name`, with room for QuickJS's
/// stack, then hands `done` what it returned, or the message of the panic
/// that ended it: a caller waiting on the thread hears from it however it
/// ends.
///
/// # Errors
///
/// The thread could not start.
pub fn spawn_thread<T>(
    name: &str,
    body: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(Result<T, String>) + Send + 'static,
) -> io::Result<()> {
    thread::Builder::new()
        .name(name.into())
        .stack_size(THREAD_STACK_BYTES)
        .spawn(move || {
            let out = catch_unwind(AssertUnwindSafe(body)).map_err(|panic| {
                panic_message(panic.as_ref())
                    .unwrap_or("unknown panic")
                    .to_string()
            });
            done(out);
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rquickjs::{Ctx, Error};

    use super::*;

    fn engine(caps: &VmCaps) -> (JsEngine, Arc<AtomicBool>) {
        let stop = Arc::new(AtomicBool::new(false));
        let engine = JsEngine::new(caps, 512 * 1024, stop.clone()).unwrap();
        (engine, stop)
    }

    fn eval(engine: &JsEngine, code: &str) -> Result<(), String> {
        engine.context().with(|ctx: Ctx<'_>| {
            ctx.eval::<(), _>(code).map_err(|e| match e {
                Error::Exception => format!("{:?}", ctx.catch()),
                other => other.to_string(),
            })
        })
    }

    #[test]
    fn an_allocation_past_the_memory_cap_fails_in_the_guest() {
        let (engine, _) = engine(&VmCaps::default().with_memory_bytes(4 << 20));
        let big = "(() => { const a = []; for (;;) a.push('x'.repeat(1 << 16) + a.length) })()";
        assert!(eval(&engine, big).is_err());
        // The engine is still usable once what it held is gone.
        assert_eq!(eval(&engine, "1 + 1"), Ok(()));
    }

    #[test]
    fn a_disarmed_watch_lets_the_guest_run_and_an_armed_one_stops_it() {
        let (engine, _) = engine(&VmCaps::default());
        let busy = "{ let n = 0; for (let i = 0; i < 2e6; i++) n += i }";
        assert_eq!(eval(&engine, busy), Ok(()));
        let (stopped, took) =
            engine.timed(Duration::from_millis(50), || eval(&engine, "for (;;) {}"));
        assert!(stopped.is_err());
        assert!(took >= Duration::from_millis(50) && took < Duration::from_secs(5));
        // Disarmed again: the next stretch runs to its end.
        assert_eq!(eval(&engine, busy), Ok(()));
    }

    #[test]
    fn the_stop_flag_stops_the_guest_only_while_the_watch_is_armed() {
        let (engine, stop) = engine(&VmCaps::default());
        stop.store(true, Ordering::SeqCst);
        assert_eq!(eval(&engine, "for (let i = 0; i < 2e6; i++) {}"), Ok(()));
        engine.watch().arm(Instant::now(), Duration::from_secs(60));
        assert!(eval(&engine, "for (;;) {}").is_err());
    }

    #[test]
    fn the_poll_hook_runs_while_armed() {
        let (engine, _) = engine(&VmCaps::default());
        let polls = Rc::new(Cell::new(0u32));
        let counted = polls.clone();
        engine
            .watch()
            .on_poll(move || counted.set(counted.get() + 1));
        eval(&engine, "for (let i = 0; i < 2e6; i++) {}").unwrap();
        assert_eq!(polls.get(), 0);
        engine.watch().arm(Instant::now(), Duration::from_secs(60));
        eval(&engine, "for (let i = 0; i < 2e6; i++) {}").unwrap();
        assert!(polls.get() > 0);
    }

    #[test]
    fn a_panic_on_an_engine_thread_reaches_done_as_its_message() {
        let (tx, rx) = std::sync::mpsc::channel();
        let boom = || -> u32 { panic!("boom") };
        spawn_thread("haest-test", boom, move |r| tx.send(r).unwrap()).unwrap();
        assert_eq!(rx.recv().unwrap(), Err("boom".into()));
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_thread("haest-test", || 7, move |r| tx.send(r).unwrap()).unwrap();
        assert_eq!(rx.recv().unwrap(), Ok(7));
    }
}
