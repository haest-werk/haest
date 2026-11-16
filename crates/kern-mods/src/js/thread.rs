//! The thread a file mod's QuickJS runtime lives on. QuickJS is single-threaded
//! and its calls block, so it never runs on the async runtime's workers: the
//! async half sends a request over a channel and awaits a oneshot reply.
//! After every request the thread runs the job queue (the mod's promises) and
//! forwards what the mod sent out (outbox.rs).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use rquickjs::{Context, Ctx, Function, Module, Object, Runtime, Value};
use tokio::{runtime::Handle, sync::oneshot};
use tokio_util::sync::CancellationToken;

use super::channel::{JsMeta, Limits, Reply, ToJs};
use crate::api::ModHost;

const BOOTSTRAP: &str = include_str!("bootstrap.js");
const STACK_BYTES: usize = 4 << 20;

pub(crate) struct Setup {
    pub path: PathBuf,
    pub source: String,
    pub host: Arc<dyn ModHost>,
    pub meta: Arc<Mutex<JsMeta>>,
    pub limits: Limits,
    pub runtime: Handle,
}

/// Start the thread: Ok once the module ran and its default export is a function.
pub(crate) async fn start(setup: Setup) -> Result<mpsc::Sender<ToJs>, String> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let own = tx.clone();
    std::thread::Builder::new()
        .name("kern-mod".into())
        .stack_size(STACK_BYTES)
        .spawn(move || serve(setup, rx, own, ready_tx))
        .map_err(|e| format!("its thread could not start: {e}"))?;
    match ready_rx.await {
        Ok(Ok(())) => Ok(tx),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("its thread stopped while loading".into()),
    }
}

fn serve(
    setup: Setup,
    rx: mpsc::Receiver<ToJs>,
    own: mpsc::Sender<ToJs>,
    ready: oneshot::Sender<Result<(), String>>,
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

/// How long the mod's code has run without pausing; the interrupt handler stops it past the limit.
struct Slice {
    limit: Duration,
    since: Mutex<Option<Instant>>,
}

impl Slice {
    fn begin(&self) {
        *self.since.lock().unwrap() = Some(Instant::now());
    }
    /// How long that slice ran.
    fn end(&self) -> Duration {
        let since = self.since.lock().unwrap().take();
        since.map_or(Duration::ZERO, |t| t.elapsed())
    }
    fn over(&self) -> bool {
        let since = *self.since.lock().unwrap();
        since.is_some_and(|t| t.elapsed() > self.limit)
    }
}

/// What a thrown value says.
pub(super) fn error_text(ctx: &Ctx<'_>, e: rquickjs::Error) -> String {
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
        .get::<rquickjs::convert::Coerced<String>>()
        .map_or_else(|_| "an error that cannot be shown".into(), |s| s.0)
}

fn kern<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    ctx.globals().get("__kern")
}

/// A `.ts` mod as JavaScript, through kern's one type stripper (kern-ts), so its
/// error lines are the file's own.
fn runnable(path: &Path, source: String) -> Result<String, String> {
    if path.extension().is_none_or(|e| e != "ts") {
        return Ok(source);
    }
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    kern_ts::strip_types(&source, &name).map_err(|why| format!("it is not valid TypeScript: {why}"))
}

pub(super) struct Js {
    // Field order is drop order: the context goes before its runtime.
    ctx: Context,
    rt: Runtime,
    slice: Arc<Slice>,
    pub(super) host: Arc<dyn ModHost>,
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
    fn init(setup: Setup, own: mpsc::Sender<ToJs>) -> Result<Js, String> {
        let code = runnable(&setup.path, setup.source)?;
        let rt = Runtime::new().map_err(|e| e.to_string())?;
        rt.set_memory_limit(setup.limits.memory_bytes);
        let slice = Arc::new(Slice {
            limit: setup.limits.slice,
            since: Mutex::default(),
        });
        let watch = slice.clone();
        rt.set_interrupt_handler(Some(Box::new(move || watch.over())));
        let ctx = Context::full(&rt).map_err(|e| e.to_string())?;
        let js = Js {
            ctx,
            rt,
            slice,
            host: setup.host,
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
                Err(rquickjs::Error::WouldBlock) => return Ok(Loaded::NeverFinished),
                other => other?,
            }
            let main: Value = module.get("default")?;
            if !main.is_function() {
                return Ok(Loaded::NotAFunction);
            }
            ctx.globals().set("__kernMod", main)?;
            Ok(Loaded::Ready)
        })?;
        match loaded {
            Loaded::Ready => Ok(js),
            Loaded::NotAFunction => Err("its default export is not a function".into()),
            Loaded::NeverFinished => Err("its top-level await never finished".into()),
        }
    }

    /// Run `f` in the mod's context, timing it as one slice.
    fn enter<T>(&self, f: impl FnOnce(&Ctx<'_>) -> rquickjs::Result<T>) -> Result<T, String> {
        self.slice.begin();
        let out = self
            .ctx
            .with(|ctx| f(&ctx).map_err(|e| error_text(&ctx, e)));
        self.slice.end();
        out
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
            let f: Function = kern(ctx)?.get("dispatch")?;
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
            let f: Function = kern(ctx)?.get("answer")?;
            f.call::<_, ()>((id as f64, ok, value))
        }) {
            self.fail_all(&e);
        }
    }

    /// Run every job the last request made possible, then forward what came out.
    fn settle(&mut self) {
        let stopped = self.run_jobs();
        match self.enter(|ctx| {
            let f: Function = kern(ctx)?.get("drain")?;
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
        loop {
            self.slice.begin();
            let ran = self.rt.execute_pending_job();
            let took = self.slice.end();
            match ran {
                Ok(true) => {}
                Ok(false) => return stopped,
                Err(job) => {
                    let thrown = job
                        .0
                        .with(|ctx| error_text(&ctx, rquickjs::Error::Exception));
                    stopped = Some(if took > self.slice.limit {
                        format!(
                            "its code ran over {:.1}s without pausing and was stopped",
                            self.slice.limit.as_secs_f64()
                        )
                    } else {
                        format!("its code was stopped: {thrown}")
                    });
                }
            }
        }
    }
}
