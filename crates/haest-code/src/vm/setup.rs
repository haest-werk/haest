//! What one run puts into a fresh QuickJS context before the code runs
//! ([`super::engine`]): `__log`, `__path`, the job's JSON values, its async native
//! functions, and -- in module mode -- the importable modules. Nothing else: the
//! guest has no fs, network, timers or process of its own.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use rquickjs::{
    Coerced, Ctx, Exception, Function, Module, Persistent, Promise,
    context::EvalOptions,
    function::Rest,
    loader::{ImportAttributes, Loader, Resolver},
    module::Declared,
};

use super::{
    parts::{Logs, as_function},
    path::path_op,
    thread::VmPort,
    types::{VmBeat, VmJob, VmStop},
};

/// A heartbeat at most this often.
pub(crate) const BEAT: Duration = Duration::from_millis(1000);

type Settlers = (Persistent<Function<'static>>, Persistent<Function<'static>>);

/// What the guest's native functions and the run loop share, on the engine thread.
pub(crate) struct Shared {
    pub logs: RefCell<Logs>,
    /// Every call haest has yet to answer: its promise's resolve and reject.
    pub pending: RefCell<HashMap<u64, Settlers>>,
    next_id: Cell<u64>,
    pub calls: Cell<u64>,
    pub memory: Cell<u64>,
}

impl Shared {
    pub(super) fn new(max_log_chars: usize) -> Rc<Self> {
        Rc::new(Self {
            logs: RefCell::new(Logs::new(max_log_chars)),
            pending: RefCell::default(),
            next_id: Cell::new(0),
            calls: Cell::new(0),
            memory: Cell::new(0),
        })
    }

    pub(super) fn inflight(&self) -> usize {
        self.pending.try_borrow().map_or(0, |p| p.len())
    }
}

/// The run's clock and its heartbeat: read by the loop AND, through the
/// engine's watch, the interrupt.
pub(crate) struct Clock {
    pub started: Instant,
    pub deadline: Instant,
    stop: Arc<AtomicBool>,
    last_beat: Cell<Instant>,
    shared: Rc<Shared>,
    send: Box<dyn Fn(VmBeat)>,
}

impl Clock {
    pub(super) fn new(job: &VmJob, shared: Rc<Shared>, send: Box<dyn Fn(VmBeat)>) -> Rc<Self> {
        let started = Instant::now();
        Rc::new(Self {
            started,
            deadline: started + job.caps.timeout,
            stop: job.stop.clone(),
            last_beat: Cell::new(started),
            shared,
            send,
        })
    }

    pub(super) fn stop_reason(&self) -> Option<VmStop> {
        if self.stop.load(Ordering::SeqCst) {
            Some(VmStop::Interrupted)
        } else if Instant::now() > self.deadline {
            Some(VmStop::Timeout)
        } else {
            None
        }
    }

    pub(super) fn beat_if_due(&self) {
        let now = Instant::now();
        if now.duration_since(self.last_beat.get()) < BEAT {
            return;
        }
        self.last_beat.set(now);
        (self.send)(VmBeat {
            elapsed_ms: now.duration_since(self.started).as_millis() as u64,
            calls: self.shared.calls.get(),
            inflight: self.shared.inflight() as u64,
            memory_bytes: self.shared.memory.get(),
        });
    }
}

fn throw(ctx: &Ctx<'_>, message: &str) -> rquickjs::Error {
    Exception::throw_message(ctx, message)
}

/// The globals every run gets, then the job's own; errors are QuickJS exceptions.
pub(crate) fn install<'js>(
    ctx: &Ctx<'js>,
    job: &VmJob,
    shared: &Rc<Shared>,
    port: &VmPort,
) -> rquickjs::Result<()> {
    let g = ctx.globals();
    let logs = shared.clone();
    let log = move |level: Coerced<String>, line: Coerced<String>| {
        logs.logs.borrow_mut().add(&level.0, &line.0);
    };
    g.set("__log", Function::new(ctx.clone(), log)?)?;
    let path = |ctx: Ctx<'js>, op: Coerced<String>, args: Coerced<String>| {
        path_op(&op.0, &args.0).map_err(|e| throw(&ctx, &e))
    };
    g.set("__path", Function::new(ctx.clone(), path)?)?;
    for (name, value) in &job.values {
        let json = serde_json::to_string(value).map_err(|e| throw(ctx, &e.to_string()))?;
        g.set(name.as_str(), ctx.json_parse(json)?)?;
    }
    for name in &job.async_names {
        g.set(name.as_str(), async_fn(ctx, name, shared, port)?)?;
    }
    Ok(())
}

/// A guest call haest answers later: a promise in the guest, settled when the answer comes back.
fn async_fn<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    shared: &Rc<Shared>,
    port: &VmPort,
) -> rquickjs::Result<Function<'js>> {
    let shared = shared.clone();
    let call = port.caller();
    let name = name.to_string();
    let f = move |ctx: Ctx<'js>, args: Rest<Coerced<String>>| -> rquickjs::Result<Promise<'js>> {
        let (promise, resolve, reject) = ctx.promise()?;
        let id = shared.next_id.get() + 1;
        shared.next_id.set(id);
        shared.calls.set(shared.calls.get() + 1);
        let settlers = (
            Persistent::save(&ctx, resolve),
            Persistent::save(&ctx, reject),
        );
        shared.pending.borrow_mut().insert(id, settlers);
        call(id, name.clone(), args.0.into_iter().map(|a| a.0).collect());
        Ok(promise)
    };
    Function::new(ctx.clone(), f)
}

fn script(filename: &str) -> EvalOptions {
    let mut o = EvalOptions::default();
    o.strict = false;
    o.filename = Some(filename.to_string());
    o
}

pub(crate) fn eval_prelude(ctx: &Ctx<'_>, prelude: &str) -> rquickjs::Result<()> {
    ctx.eval_with_options::<(), _>(prelude, script("prelude.js"))
}

/// The code as a promise: an async function's (function mode) or a module's evaluation.
pub(crate) fn eval_code<'js>(ctx: &Ctx<'js>, job: &VmJob) -> rquickjs::Result<Promise<'js>> {
    if job.modules.is_some() {
        Module::evaluate(ctx.clone(), job.filename.as_str(), job.code.as_str())
    } else {
        ctx.eval_with_options(as_function(&job.code), script(&job.filename))
    }
}

/// Module mode: exactly the job's modules are importable, by their exact names.
#[derive(Clone)]
pub(crate) struct JobModules(pub Arc<HashMap<String, String>>);

impl Resolver for JobModules {
    fn resolve<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        _base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if self.0.contains_key(name) {
            Ok(name.to_string())
        } else {
            Err(throw(ctx, &format!("module \"{name}\" is not available")))
        }
    }
}

impl Loader for JobModules {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        path: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        match self.0.get(path) {
            Some(source) => Module::declare(ctx.clone(), path, source.as_str()),
            None => Err(throw(ctx, &format!("module \"{path}\" is not available"))),
        }
    }
}
