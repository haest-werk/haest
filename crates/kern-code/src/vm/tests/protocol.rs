//! The thread protocol (code-vm-run.test.ts `createWorkerHandler`): kern's side
//! of [`crate::vm::host`] driven by hand -- calls out, answers and a stop in.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use super::limits;
use crate::vm::{
    VmLimits, VmRun, VmStop,
    host::{Engine, spawn_engine},
    types::{FromVm, ToVm, VmJob},
};

fn job(code: &str, async_names: &[&str]) -> VmJob {
    VmJob {
        code: code.into(),
        prelude: String::new(),
        values: Default::default(),
        async_names: async_names.iter().map(|s| s.to_string()).collect(),
        modules: None,
        filename: "code.js".into(),
        limits: limits(5000),
        stop: Arc::new(AtomicBool::new(false)),
    }
}

/// Everything the engine posts until `Done`, answering calls with `answer`.
async fn drive(
    engine: Engine,
    answer: impl Fn(&str, &[String]) -> Option<String>,
) -> (Vec<&'static str>, VmRun) {
    let Engine { to_vm, mut from_vm } = engine;
    let mut kinds = Vec::new();
    while let Some(m) = from_vm.recv().await {
        match m {
            FromVm::Call {
                call_id,
                name,
                args,
            } => {
                kinds.push("call");
                if let Some(text) = answer(&name, &args) {
                    to_vm
                        .send(ToVm::Answer {
                            call_id,
                            ok: true,
                            text,
                        })
                        .unwrap();
                }
            }
            FromVm::Beat(_) => kinds.push("beat"),
            FromVm::Done(run) => {
                kinds.push("done");
                return (kinds, run);
            }
        }
    }
    panic!("the engine ended without Done");
}

#[tokio::test]
async fn a_run_asks_for_a_call_takes_the_answer_and_posts_its_result() {
    let engine = spawn_engine(job("return await __call('x')", &["__call"])).unwrap();
    let (kinds, run) = drive(engine, |_, args| Some(format!("echo {}", args[0]))).await;
    assert_eq!(kinds, ["call", "done"]);
    assert_eq!(run.value.as_deref(), Some("echo x"));
}

#[tokio::test]
async fn a_stop_message_ends_a_run_parked_on_a_call_that_never_answers() {
    let engine = spawn_engine(job("await __call()", &["__call"])).unwrap();
    let to_vm = engine.to_vm.clone();
    let stopper = async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        to_vm.send(ToVm::Stop).unwrap();
    };
    let started = Instant::now();
    let ((kinds, run), ()) = tokio::join!(drive(engine, |_, _| None), stopper);
    assert_eq!(kinds.last(), Some(&"done"));
    assert_eq!(run.stopped, Some(VmStop::Interrupted));
    assert!(started.elapsed() < Duration::from_millis(2000));
}

#[tokio::test]
async fn the_stop_flag_and_the_deadline_both_stop_a_cpu_loop() {
    let flagged = job("while (true) {}", &[]);
    flagged.stop.store(true, Ordering::SeqCst);
    let (_, run) = drive(spawn_engine(flagged).unwrap(), |_, _| None).await;
    assert_eq!(run.stopped, Some(VmStop::Interrupted));
    let mut timed = job("while (true) {}", &[]);
    timed.limits = VmLimits {
        timeout_ms: 200,
        memory_bytes: 32 << 20,
        max_log_chars: 100,
    };
    let (_, run) = drive(spawn_engine(timed).unwrap(), |_, _| None).await;
    assert_eq!(run.stopped, Some(VmStop::Timeout));
}
