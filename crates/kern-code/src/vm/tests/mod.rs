//! Ported from code-vm.test.ts: the engine on its thread, stopping, heartbeats,
//! the run cap. One run's behaviour is in [`run`], the thread protocol in [`protocol`].

mod protocol;
mod run;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use futures::FutureExt;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::{HostFn, MAX_ENGINES, VmBeat, VmLimits, VmRun, VmSpec, VmStop, run_with_slots};

fn limits(timeout_ms: u64) -> VmLimits {
    VmLimits {
        timeout_ms,
        memory_bytes: 32 << 20,
        max_log_chars: 10_000,
    }
}

/// A host fn from a closure over its args.
fn host_fn<F, Fut>(f: F) -> HostFn
where
    F: Fn(Vec<String>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<String, String>> + Send + 'static,
{
    Arc::new(move |args| f(args).boxed())
}

fn spec(fns: impl IntoIterator<Item = (&'static str, HostFn)>) -> VmSpec {
    VmSpec {
        host_fns: fns.into_iter().map(|(n, f)| (n.to_string(), f)).collect(),
        ..Default::default()
    }
}

/// A run on a private pool, so parallel tests never queue behind each other.
async fn run(code: &str, spec: &VmSpec, limits: VmLimits, cancel: &CancellationToken) -> VmRun {
    let slots = Semaphore::new(MAX_ENGINES);
    run_with_slots(&slots, code, spec, limits, cancel, None).await
}

fn cancel_after(ms: u64) -> CancellationToken {
    let c = CancellationToken::new();
    let later = c.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        later.cancel();
    });
    c
}

#[tokio::test]
async fn an_interrupt_stops_a_cpu_loop_at_once_and_what_it_printed_is_kept() {
    let started = Instant::now();
    let code = "console.log('before'); while (true) {}";
    let r = run(code, &VmSpec::default(), limits(30_000), &cancel_after(100)).await;
    assert_eq!(r.stopped, Some(VmStop::Interrupted));
    assert!(!r.killed);
    assert_eq!(r.stdout, "before\n");
    assert!(started.elapsed() < Duration::from_millis(1000));
}

#[tokio::test]
async fn the_deadline_stops_a_cpu_loop_and_is_reported_as_a_timeout() {
    let r = run(
        "while (true) {}",
        &VmSpec::default(),
        limits(300),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(r.stopped, Some(VmStop::Timeout));
}

#[tokio::test]
async fn a_cpu_bound_run_keeps_sending_heartbeats() {
    let beats: Mutex<Vec<VmBeat>> = Mutex::default();
    let on_beat = |b: VmBeat| beats.lock().unwrap().push(b);
    let slots = Semaphore::new(1);
    let cancel = cancel_after(2600);
    let spec = VmSpec::default();
    run_with_slots(
        &slots,
        "while (true) {}",
        &spec,
        limits(30_000),
        &cancel,
        Some(&on_beat),
    )
    .await;
    let beats = beats.into_inner().unwrap();
    assert!(beats.len() >= 2, "{beats:?}");
    assert!(beats[1].elapsed_ms > beats[0].elapsed_ms);
}

#[tokio::test]
async fn no_more_than_four_engines_run_at_once_the_rest_wait_their_turn() {
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (l, p) = (live.clone(), peak.clone());
    let hold = host_fn(move |_| {
        let (l, p) = (l.clone(), p.clone());
        async move {
            p.fetch_max(l.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(150)).await;
            l.fetch_sub(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }
    });
    let spec = spec([("__hold", hold)]);
    let slots = Semaphore::new(MAX_ENGINES);
    let never = CancellationToken::new();
    let runs = futures::future::join_all((0..6).map(|_| {
        run_with_slots(
            &slots,
            "return await __hold()",
            &spec,
            limits(30_000),
            &never,
            None,
        )
    }))
    .await;
    let values: Vec<_> = runs.iter().map(|r| r.value.as_deref()).collect();
    assert_eq!(values, vec![Some("ok"); 6]);
    assert_eq!(peak.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_run_waiting_for_a_slot_can_be_interrupted_without_ever_starting() {
    let hold = host_fn(|_| async {
        tokio::time::sleep(Duration::from_millis(400)).await;
        Ok("ok".to_string())
    });
    let spec = spec([("__hold", hold)]);
    let slots = Semaphore::new(MAX_ENGINES);
    let never = CancellationToken::new();
    let busy = futures::future::join_all((0..4).map(|_| {
        run_with_slots(
            &slots,
            "return await __hold()",
            &spec,
            limits(30_000),
            &never,
            None,
        )
    }));
    let cancel = CancellationToken::new();
    let waiting = async {
        tokio::task::yield_now().await;
        cancel.cancel();
        run_with_slots(&slots, "return 1", &spec, limits(30_000), &cancel, None).await
    };
    let (busy, waiting) = tokio::join!(busy, waiting);
    assert_eq!(waiting.stopped, Some(VmStop::Interrupted));
    assert!(busy.iter().all(|r| r.value.as_deref() == Some("ok")));
}
