//! A host's own `select!` that drops `serve` ends the connection there: its
//! sink is called no more and goes with it.

use std::sync::atomic::AtomicBool;

use super::*;
use crate::TransportError;

/// What a host's sink saw after the host dropped `serve`.
#[derive(Clone, Default)]
struct AfterDrop {
    dropped: Arc<AtomicBool>,
    lines: Arc<AtomicUsize>,
    sink_alive: Arc<AtomicBool>,
}

struct Records(AfterDrop);

impl LineSink for Records {
    async fn send_line(&mut self, _line: String) -> Result<(), TransportError> {
        if self.0.dropped.load(Ordering::SeqCst) {
            self.0.lines.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

impl Drop for Records {
    fn drop(&mut self) {
        self.0.sink_alive.store(false, Ordering::SeqCst);
    }
}

/// A peer that follows the stream, then sends nothing more.
struct Follows(bool);

impl LineSource for Follows {
    async fn next_line(&mut self) -> Option<Result<String, TransportError>> {
        if std::mem::replace(&mut self.0, true) {
            std::future::pending().await
        } else {
            let follow = json!({"v": 1, "id": "e", "type": "events", "after": 0, "follow": true});
            Some(Ok(follow.to_string()))
        }
    }
}

/// The writer ran on a task of its own, aborted when `serve` dropped. An
/// abort lands only once that task is next polled, so on a runtime with
/// several workers it went on calling the host's sink after the host had
/// dropped `serve` while the stream was busy, and the sink outlived it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_serve_calls_its_sink_no_more_and_drops_it() {
    for run in 0..50u64 {
        let hub = hub();
        let (s, _) = session(&answers(&[]), hub.clone());
        // The conversation keeps making frames.
        let busy = tokio::spawn({
            let hub = hub.clone();
            async move {
                let mut n = 0u64;
                loop {
                    hub.notice(Level::Info, format!("frame {n}"));
                    n = n.wrapping_add(1);
                    if n % 8 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
            }
        });
        let seen = AfterDrop::default();
        seen.sink_alive.store(true, Ordering::SeqCst);
        let grant = Grant::watch(Origin::Agent);
        let sink = Records(seen.clone());
        let mut serving = Box::pin(serve(
            s.clone(),
            grant,
            Start::default(),
            Follows(false),
            sink,
        ));
        let wait = Duration::from_micros(2_000 + (run * 7919) % 8_000);
        tokio::select! {
            ended = &mut serving => panic!("run {run}: serve ended: {ended:?}"),
            () = tokio::time::sleep(wait) => {}
        }
        drop(serving);
        seen.dropped.store(true, Ordering::SeqCst);
        assert!(
            !seen.sink_alive.load(Ordering::SeqCst),
            "run {run}: the sink outlived serve"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        busy.abort();
        let lines = seen.lines.load(Ordering::SeqCst);
        assert_eq!(
            lines, 0,
            "run {run}: the sink was called after serve dropped"
        );
        s.close().await;
    }
}
