//! The thread a file mod's engine lives on: haest-code's [`JsEngine`], the
//! one the Code tool runs on, on a thread of its own ([`spawn_thread`]).
//! QuickJS is single-threaded and its calls block, so it never runs on the
//! async runtime's workers: the async half sends a request over a channel
//! and awaits a oneshot reply. After every request the thread runs the job
//! queue (the mod's promises) and forwards what the mod sent out (outbox.rs).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
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
    channel::{JsMeta, Reply, ToJs},
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

/// Start the thread: Ok once the module ran and its default export is a function.
pub(crate) async fn start(setup: Setup) -> Result<mpsc::Sender<ToJs>, ModLoadError> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let own = tx.clone();
    // A panic drops the thread's ends of the channels on its way out: the
    // load hears `Stopped`, and so does every call in flight.
    let ended = |_: Result<(), String>| {};
    spawn_thread("haest-mod", move || serve(setup, &rx, own, ready_tx), ended)
        .map_err(ModLoadError::Thread)?;
    match ready_rx.await {
        Ok(Ok(())) => Ok(tx),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(ModLoadError::Stopped),
    }
}

fn serve(
    setup: Setup,
    rx: &mpsc::Receiver<ToJs>,
    own: mpsc::Sender<ToJs>,
    ready: oneshot::Sender<Result<(), ModLoadError>>,
) {
    let mut js = match Js::init(setup, own) {
        Ok(js) => js,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    while let Ok(msg) = rx.recv() {
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
    next_id: u64,
}

enum Loaded {
    Ready,
    NotAFunction,
    NeverFinished,
}

impl Js {
    fn init(setup: Setup, own: mpsc::Sender<ToJs>) -> Result<Js, ModLoadError> {
        let code = runnable(&setup.path, setup.source)?;
        // Nothing else holds the engine's stop flag: a mod file stops
        // between requests (`ToJs::Stop`), and its slice stops a CPU loop.
        let engine = JsEngine::new(&setup.caps, STACK_BYTES, Arc::default())
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
        let sent = self.enter(|ctx| {
            let f: Function<'_> = haest(ctx)?.get("dispatch")?;
            f.call::<_, ()>((id as f64, op, payload))
        });
        if let Err(e) = sent {
            self.finish(id, Err(e));
        }
    }

    fn answer(&mut self, id: u64, result: Result<&'static str, String>) {
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
