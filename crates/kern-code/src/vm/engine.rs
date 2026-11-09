//! One engine run, ON its own thread ([`super::host`]): a fresh QuickJS runtime,
//! torn down after it. The guest reaches exactly what the job names -- JSON
//! values, the prelude that builds its API, async functions kern answers, module
//! sources in module mode -- plus one pure built-in, `__path` (posix path ops),
//! which needs no host at all ([`super::setup`]).
//!
//! Bounded on every axis: QuickJS memory and stack limits; the interrupt handler
//! stops a CPU loop when the job's deadline passes or kern sets the stop flag (an
//! atomic -- it reaches the handler even while the loop runs, where no message
//! could); a run parked on kern's answers ends on the stop message. A run ends
//! when its code has settled AND every call it started has answered.

use std::{rc::Rc, sync::Arc, time::Duration};

use rquickjs::{Coerced, Context, Ctx, Exception, Persistent, Promise, Runtime, Value};

use super::{
    host::VmPort,
    parts::{BASE_PRELUDE, error_text},
    setup::{BEAT, Clock, JobModules, Shared, eval_code, eval_prelude, install},
    types::{ToVm, VmJob, VmRun},
};

const STACK_BYTES: usize = 512 * 1024;
const NEVER_SETTLES: &str =
    "The code awaited a promise that can never settle (no host call is running).";

pub(crate) fn run_engine(job: &VmJob, port: &VmPort) -> VmRun {
    let rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return VmRun::failed(format!("kern's JS engine could not start: {e}")),
    };
    rt.set_memory_limit(job.limits.memory_bytes);
    rt.set_max_stack_size(STACK_BYTES);
    if let Some(modules) = &job.modules {
        let m = JobModules(Arc::new(modules.clone()));
        rt.set_loader(m.clone(), m);
    }
    let context = match Context::full(&rt) {
        Ok(c) => c,
        Err(e) => return VmRun::failed(format!("kern's JS engine could not start: {e}")),
    };
    let shared = Shared::new(job.limits.max_log_chars);
    let clock = Clock::new(job, shared.clone(), Box::new(port.beats()));
    let run = Run {
        job,
        port,
        context,
        rt,
        shared,
        clock,
        out: VmRun::default(),
    };
    run.go()
}

/// Fields drop in order: the context before its runtime.
struct Run<'a> {
    job: &'a VmJob,
    port: &'a VmPort,
    context: Context,
    rt: Runtime,
    shared: Rc<Shared>,
    clock: Rc<Clock>,
    out: VmRun,
}

impl Run<'_> {
    fn go(mut self) -> VmRun {
        self.out.error = self.start();
        self.finish()
    }

    /// Prelude, code, settle, drain; the error that ended it, if any.
    fn start(&mut self) -> Option<String> {
        let prelude = format!("{BASE_PRELUDE}\n{}", self.job.prelude);
        let installed = self.context.with(|ctx| {
            install(&ctx, self.job, &self.shared, self.port)
                .and_then(|()| eval_prelude(&ctx, &prelude))
                .map_err(|e| caught(&ctx, e, "prelude.js"))
        });
        if let Err(e) = installed {
            return Some(format!("kern's JS engine crashed: {e}"));
        }
        // Only now: a stop asked before the run began must stop the CODE, not our own prelude.
        let clock = self.clock.clone();
        self.rt.set_interrupt_handler(Some(Box::new(move || {
            clock.beat_if_due();
            clock.stop_reason().is_some()
        })));
        let promise = self.context.with(|ctx| {
            eval_code(&ctx, self.job)
                .map(|p| Persistent::save(&ctx, p))
                .map_err(|e| caught(&ctx, e, &self.job.filename))
        });
        let promise = match promise {
            Ok(p) => p,
            Err(e) => return Some(e),
        };
        let error = self.settle(&promise, self.job.modules.is_none());
        drop(promise);
        self.drain();
        error
    }

    fn finish(mut self) -> VmRun {
        self.out.stopped = self.out.stopped.or(self.clock.stop_reason());
        // Every value kern still holds goes before the runtime does.
        self.shared.pending.borrow_mut().clear();
        self.rt.set_interrupt_handler(None);
        let Run {
            context,
            rt,
            shared,
            mut out,
            ..
        } = self;
        drop(context);
        drop(rt);
        let logs = shared.logs.take();
        out.logs = logs.text;
        out.stdout = logs.stdout;
        out.stderr = logs.stderr;
        out.logs_truncated = logs.truncated;
        out
    }

    /// Pump the job queue and wait on kern's answers until the code's promise
    /// settles or the run is stopped; the code's error, if it failed.
    fn settle(
        &mut self,
        promise: &Persistent<Promise<'static>>,
        keep_value: bool,
    ) -> Option<String> {
        loop {
            self.pump();
            if let Some(settled) = self.settled(promise, keep_value) {
                return settled.err();
            }
            self.out.stopped = self.clock.stop_reason();
            if self.out.stopped.is_some() {
                return None;
            }
            if self.shared.inflight() == 0 && !self.rt.is_job_pending() {
                return Some(NEVER_SETTLES.into());
            }
            self.wait_once();
        }
    }

    /// None while pending; else Ok (its value on `out`) or the rejection's text.
    fn settled(
        &mut self,
        promise: &Persistent<Promise<'static>>,
        keep_value: bool,
    ) -> Option<Result<(), String>> {
        let filename = &self.job.filename;
        let (done, value) = self.context.with(|ctx| {
            let p = match promise.clone().restore(&ctx) {
                Ok(p) => p,
                Err(e) => return Some((Err(e.to_string()), None)),
            };
            Some(match p.result::<Value>()? {
                Err(e) => (Err(caught(&ctx, e, filename)), None),
                Ok(v) if keep_value && !v.is_undefined() => {
                    (Ok(()), v.get::<Coerced<String>>().map(|s| s.0).ok())
                }
                Ok(_) => (Ok(()), None),
            })
        })?;
        if value.is_some() {
            self.out.value = value;
        }
        Some(done)
    }

    /// Calls the code started and never awaited: let them answer (and their
    /// callbacks run) before the run ends.
    fn drain(&mut self) {
        while self.shared.inflight() > 0 && self.clock.stop_reason().is_none() {
            self.wait_once();
            self.pump();
        }
    }

    /// Until an answer arrives, kern asks to stop, or the deadline passes -- with a heartbeat on the way.
    fn wait_once(&mut self) {
        // Not from the interrupt handler: that runs mid-execution, where the runtime is busy.
        let used = self.rt.memory_usage().memory_used_size;
        self.shared.memory.set(used.max(0) as u64);
        self.clock.beat_if_due();
        let until =
            (std::time::Instant::now() + BEAT).min(self.clock.deadline + Duration::from_millis(5));
        let Some(first) = self
            .port
            .wait(until.saturating_duration_since(std::time::Instant::now()))
        else {
            return;
        };
        self.take(first);
        while let Some(m) = self.port.try_next() {
            self.take(m);
        }
    }

    fn take(&mut self, m: ToVm) {
        let ToVm::Answer { call_id, ok, text } = m else {
            return; // a stop: the port has set the flag
        };
        let Some((resolve, reject)) = self.shared.pending.borrow_mut().remove(&call_id) else {
            return;
        };
        self.context.with(|ctx| {
            let settled = if ok {
                resolve.restore(&ctx).and_then(|f| f.call::<_, ()>((text,)))
            } else {
                reject.restore(&ctx).and_then(|f| {
                    let e = Exception::from_message(ctx.clone(), &text)?;
                    f.call::<_, ()>((e,))
                })
            };
            if settled.is_err() {
                ctx.catch(); // never leave an exception pending for the next step
            }
        });
    }

    /// Run every queued job (promise reactions), unless the run is being stopped.
    fn pump(&self) {
        self.context.with(|ctx| {
            while ctx.execute_pending_job() {
                if self.clock.stop_reason().is_some() {
                    break;
                }
            }
        });
    }
}

/// The text of a failure: the pending exception's, else the engine error's own.
fn caught(ctx: &Ctx<'_>, e: rquickjs::Error, filename: &str) -> String {
    match e {
        rquickjs::Error::Exception => error_text(&ctx.catch(), filename),
        other => other.to_string(),
    }
}
