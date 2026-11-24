//! The thread a file mod's engine lives on: haest-code's [`JsEngine`], the
//! one the Code tool runs on, on a thread of its own ([`spawn_thread`]).
//! QuickJS is single-threaded and its calls block, so it never runs on the
//! async runtime's workers: the async half sends a request over a channel
//! and awaits a oneshot reply. After every request the thread runs the job
//! queue (the mod's promises) and forwards what the mod sent out (outbox.rs).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool, mpsc},
    time::{Duration, Instant},
};

use haest_code::{
    __private::{
        engine::{JsEngine, spawn_thread},
        quickjs::{self, Coerced, Ctx, Function, Module, Object, Value},
    },
    VmCaps,
};
use haest_mods::ModUiRef;
use tokio::{runtime::Handle, sync::oneshot};
use tokio_util::sync::CancellationToken;

use super::{
    channel::{JsMeta, JsThread, Reply, ToJs},
    load::ModLoadError,
};

const BOOTSTRAP: &str = include_str!("bootstrap.js");
/// QuickJS's stack cap for a mod file: QuickJS's own default.
const STACK_BYTES: usize = 1 << 20;

pub(crate) struct Setup {
    pub path: PathBuf,
    pub source: String,
    pub ui: ModUiRef,
    pub meta: Arc<Mutex<JsMeta>>,
    pub caps: VmCaps,
    pub runtime: Handle,
}

/// Start the thread: its handle once the module ran and its default export
/// is a function.
pub(crate) async fn start(setup: Setup) -> Result<JsThread, ModLoadError> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let own = tx.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let engine_stop = stop.clone();
    // A panic drops the thread's ends of the channels on its way out: the
    // load hears `Stopped`, and so does every call in flight.
    let ended = |_: Result<(), String>| {};
    let body = move || serve(setup, &rx, own, engine_stop, ready_tx);
    spawn_thread("haest-mod", body, ended).map_err(ModLoadError::Thread)?;
    match ready_rx.await {
        Ok(Ok(())) => Ok(JsThread::new(tx, stop)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(ModLoadError::Stopped),
    }
}

fn serve(
    setup: Setup,
    rx: &mpsc::Receiver<ToJs>,
    own: mpsc::Sender<ToJs>,
    stop: Arc<AtomicBool>,
    ready: oneshot::Sender<Result<(), ModLoadError>>,
) {
    let mut js = match Js::init(setup, own, stop) {
        Ok(js) => js,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    loop {
        // A call's deadline is the one thing that wakes the thread with no request.
        let next = match js.next_due() {
            None => rx
                .recv()
                .map_err(|mpsc::RecvError| mpsc::RecvTimeoutError::Disconnected),
            Some(due) => rx.recv_timeout(due.saturating_duration_since(Instant::now())),
        };
        let msg = match next {
            Ok(msg) => msg,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                js.expire();
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match msg {
            ToJs::Stop => break,
            ToJs::Call {
                op,
                payload,
                cancel,
                reply,
            } => js.dispatch(op, payload, cancel, reply),
            ToJs::Answer { id, result } => js.answer(id, result),
        }
        js.settle();
    }
}

/// What a thrown value says.
pub(super) fn error_text(ctx: &Ctx<'_>, e: &quickjs::Error) -> String {
    if !e.is_exception() {
        return e.to_string();
    }
    let thrown = ctx.catch();
    if let Some(message) = thrown
        .as_object()
        .and_then(|o| o.get::<_, String>("message").ok())
    {
        return message;
    }
    thrown
        .get::<Coerced<String>>()
        .map_or_else(|_| "an error that cannot be shown".into(), |s| s.0)
}

fn haest<'js>(ctx: &Ctx<'js>) -> quickjs::Result<Object<'js>> {
    ctx.globals().get("__haest")
}

/// A `.ts` mod as JavaScript, through haest's one type stripper (haest-ts), so its
/// error lines are the file's own.
fn runnable(path: &Path, source: String) -> Result<String, ModLoadError> {
    if path.extension().is_none_or(|e| e != "ts") {
        return Ok(source);
    }
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    haest_ts::strip_types(&source, &name).map_err(ModLoadError::TypeScript)
}

pub(super) struct Js {
    engine: JsEngine,
    /// The longest the mod's code may run without pausing: each stretch of
    /// it runs [`JsEngine::timed`] to this.
    slice: Duration,
    pub(super) ui: ModUiRef,
    pub(super) meta: Arc<Mutex<JsMeta>>,
    pub(super) runtime: Handle,
    pub(super) own: mpsc::Sender<ToJs>,
    pub(super) pending: HashMap<u64, Reply>,
    /// The cancel token of each tool call in flight, by the id its handlers see as `ctx.signal`.
    pub(super) signals: HashMap<u64, CancellationToken>,
    /// How long each call has to settle ([`VmCaps::call_timeout`]).
    call_timeout: Duration,
    /// Each call in flight's op and when it is due (`None`: no deadline).
    pub(super) deadlines: HashMap<u64, (&'static str, Option<Instant>)>,
    /// `haest.ask`s waiting on the human, and since when one has: while
    /// any is, no call's clock runs.
    asks_out: usize,
    paused_since: Option<Instant>,
    next_id: u64,
}

enum Loaded {
    Ready,
    NotAFunction,
    NeverFinished,
}

impl Js {
    fn init(
        setup: Setup,
        own: mpsc::Sender<ToJs>,
        stop: Arc<AtomicBool>,
    ) -> Result<Js, ModLoadError> {
        let code = runnable(&setup.path, setup.source)?;
        // The mod's handle (`JsThread`) holds the engine's stop flag and sets
        // it when it drops: the code stops at the next poll, even mid-loop,
        // and the thread ends at `ToJs::Stop`. While the mod lives, its slice
        // stops a CPU loop.
        let engine = JsEngine::new(&setup.caps, STACK_BYTES, stop)
            .map_err(|e| ModLoadError::Evaluate(e.to_string()))?;
        let js = Js {
            engine,
            slice: setup.caps.slice,
            ui: setup.ui,
            meta: setup.meta,
            runtime: setup.runtime,
            own,
            pending: HashMap::new(),
            signals: HashMap::new(),
            call_timeout: setup.caps.call_timeout,
            deadlines: HashMap::new(),
            asks_out: 0,
            paused_since: None,
            next_id: 0,
        };
        let name = setup.path.display().to_string();
        let loaded = js.enter(|ctx| {
            ctx.eval::<(), _>(BOOTSTRAP)?;
            let (module, done) = Module::declare(ctx.clone(), name, code)?.eval()?;
            match done.finish::<()>() {
                Err(quickjs::Error::WouldBlock) => return Ok(Loaded::NeverFinished),
                other => other?,
            }
            let main: Value<'_> = module.get("default")?;
            if !main.is_function() {
                return Ok(Loaded::NotAFunction);
            }
            ctx.globals().set("__haestMod", main)?;
            Ok(Loaded::Ready)
        });
        match loaded.map_err(ModLoadError::Evaluate)? {
            Loaded::Ready => Ok(js),
            Loaded::NotAFunction => Err(ModLoadError::NotAFunction),
            Loaded::NeverFinished => Err(ModLoadError::NeverFinished),
        }
    }

    /// Run `f` in the mod's context, timing it as one slice.
    fn enter<T>(&self, f: impl FnOnce(&Ctx<'_>) -> quickjs::Result<T>) -> Result<T, String> {
        let context = self.engine.context();
        let entered = || context.with(|ctx| f(&ctx).map_err(|e| error_text(&ctx, &e)));
        self.engine.timed(self.slice, entered).0
    }

    fn dispatch(
        &mut self,
        op: &'static str,
        payload: String,
        cancel: Option<CancellationToken>,
        reply: Reply,
    ) {
        self.next_id += 1;
        let id = self.next_id;
        self.pending.insert(id, reply);
        if let Some(cancel) = cancel {
            self.signals.insert(id, cancel);
        }
        // `load` has its own deadline, the caps' `timeout` (load.rs). A call
        // made while the human holds an ask starts its clock when they answer.
        if op != "load" {
            let from = self.paused_since.unwrap_or_else(Instant::now);
            self.deadlines
                .insert(id, (op, from.checked_add(self.call_timeout)));
        }
        let sent = self.enter(|ctx| {
            let f: Function<'_> = haest(ctx)?.get("dispatch")?;
            f.call::<_, ()>((id as f64, op, payload))
        });
        if let Err(e) = sent {
            self.finish(id, Err(e));
        }
    }

    fn answer(&mut self, id: u64, result: Result<&'static str, String>) {
        self.ask_ended();
        let (ok, value) = match result {
            Ok(decision) => (true, decision.to_string()),
            Err(e) => (false, e),
        };
        if let Err(e) = self.enter(|ctx| {
            let f: Function<'_> = haest(ctx)?.get("answer")?;
            f.call::<_, ()>((id as f64, ok, value))
        }) {
            self.fail_all(&e);
        }
    }

    /// An ask went to the human: every call's clock stops until none waits.
    pub(super) fn ask_started(&mut self) {
        self.asks_out += 1;
        if self.paused_since.is_none() {
            self.paused_since = Some(Instant::now());
        }
    }

    /// An ask came back: with none left waiting, every call's clock runs on
    /// from where it stopped.
    fn ask_ended(&mut self) {
        self.asks_out = self.asks_out.saturating_sub(1);
        if self.asks_out > 0 {
            return;
        }
        if let Some(since) = self.paused_since.take() {
            let held = since.elapsed();
            for (_, due) in self.deadlines.values_mut() {
                *due = due.and_then(|d| d.checked_add(held));
            }
        }
    }

    /// When the thread must next wake for a deadline: none while an ask waits.
    fn next_due(&self) -> Option<Instant> {
        if self.paused_since.is_some() {
            return None;
        }
        self.deadlines.values().filter_map(|(_, due)| *due).min()
    }

    /// Fail every call past its deadline: its promise never settled. The
    /// mod runs on, and a late settle of it is dropped.
    fn expire(&mut self) {
        let now = Instant::now();
        let late: Vec<(u64, &'static str)> = self
            .deadlines
            .iter()
            .filter(|(_, (_, due))| due.is_some_and(|d| d <= now))
            .map(|(id, (op, _))| (*id, *op))
            .collect();
        for (id, op) in late {
            let why = format!(
                "its {op} call did not settle within {:.1}s: it waits on a promise nothing resolved",
                self.call_timeout.as_secs_f64()
            );
            self.finish(id, Err(why));
        }
    }

    /// Run every job the last request made possible, then forward what came out.
    fn settle(&mut self) {
        let stopped = self.run_jobs();
        match self.enter(|ctx| {
            let f: Function<'_> = haest(ctx)?.get("drain")?;
            f.call::<_, String>(())
        }) {
            Ok(json) => self.apply(&json),
            Err(e) => self.fail_all(&e),
        }
        // A job that never finished can belong to any call in flight: they all fail.
        if let Some(why) = stopped {
            self.fail_all(&why);
        }
    }

    /// The job queue, to empty. Some = a job was stopped (it ran too long, or out of memory).
    fn run_jobs(&self) -> Option<String> {
        let mut stopped = None;
        let runtime = self.engine.runtime();
        loop {
            let (ran, took) = self
                .engine
                .timed(self.slice, || runtime.execute_pending_job());
            match ran {
                Ok(true) => {}
                Ok(false) => return stopped,
                Err(job) => {
                    let thrown = job
                        .0
                        .with(|ctx| error_text(&ctx, &quickjs::Error::Exception));
                    stopped = Some(if took > self.slice {
                        format!(
                            "its code ran over {:.1}s without pausing and was stopped",
                            self.slice.as_secs_f64()
                        )
                    } else {
                        format!("its code was stopped: {thrown}")
                    });
                }
            }
        }
    }
}
