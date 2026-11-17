//! A sink's keepalive is the sink's own code: one that breaks its contract
//! must not take the connection down with it.

use std::sync::mpsc as std_mpsc;

use tokio::sync::watch;

use super::*;
use crate::TransportError;

/// The given lines, then nothing until the test says stop.
struct Asks {
    lines: Vec<String>,
    stop: watch::Receiver<bool>,
}

impl LineSource for Asks {
    async fn next_line(&mut self) -> Option<Result<String, TransportError>> {
        if !self.lines.is_empty() {
            return Some(Ok(self.lines.remove(0)));
        }
        let _ = self.stop.wait_for(|stop| *stop).await;
        None
    }
}

/// Lines to the test; a keepalive with nothing to wait for (a flush with
/// nothing buffered), so it is ready at once, every time.
struct ReadyAtOnce {
    out: std_mpsc::Sender<String>,
    kept: Arc<AtomicUsize>,
}

impl LineSink for ReadyAtOnce {
    async fn send_line(&mut self, line: String) -> Result<(), TransportError> {
        let _ = self.out.send(line);
        Ok(())
    }

    async fn keepalive(&mut self) -> Result<(), TransportError> {
        self.kept.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// The lines still go out, and the writer does not spin: on a runtime of
/// one thread, where a writer that never waits would stop everything. The
/// connection runs on a thread of its own, so a writer that spins holds that
/// thread and the test still fails, never hangs.
#[test]
fn a_keepalive_ready_at_once_never_starves_the_lines_or_spins() {
    let kept = Arc::new(AtomicUsize::new(0));
    let (out, lines) = std_mpsc::channel();
    let (stop, stopped) = watch::channel(false);
    let sink = ReadyAtOnce {
        out,
        kept: kept.clone(),
    };
    let served = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let (s, _) = session(&answers(&[]), hub());
            let asks = Asks {
                lines: vec![json!({"v": 1, "id": "r1", "type": "snapshot"}).to_string()],
                stop: stopped,
            };
            serve(s, Grant::watch(Origin::Agent), Start::default(), asks, sink).await
        })
    });

    let wait = Duration::from_secs(10);
    let line = || -> Value {
        let line = lines.recv_timeout(wait).expect("a line within 10 s");
        serde_json::from_str(&line).unwrap()
    };
    assert_eq!(line()["type"], "hello");
    let reply = line();
    assert!(is_reply(&reply, "r1"), "{reply}");

    let before = kept.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(500));
    let calls = kept.load(Ordering::Relaxed) - before;
    assert!(
        calls <= 10,
        "{calls} keepalives in 500 ms: the writer spins"
    );

    stop.send_replace(true);
    let ended = served.join().unwrap();
    assert!(matches!(ended, Ended::PeerLeft), "{ended:?}");
}
