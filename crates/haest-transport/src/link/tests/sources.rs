//! A host's own line source loses nothing to `serve`: it is asked for a
//! line only when the connection takes one, and each read it starts is kept
//! until it resolves. A source that takes its line when called, or reads
//! one over several awaits, needs no cancel-safety of its own.

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc,
};

use super::*;
use crate::link::id_of;

/// Lines into a channel.
struct Collect(mpsc::UnboundedSender<String>);

impl LineSink for Collect {
    fn send_line(
        &mut self,
        line: String,
    ) -> impl Future<Output = Result<(), crate::TransportError>> + Send {
        let _ = self.0.send(line);
        std::future::ready(Ok(()))
    }
}

/// `n` `history` requests, `h0` on: each starts a backend call, and the
/// session answers it at once while no turn runs.
fn histories(n: usize) -> Vec<String> {
    (0..n)
        .map(|n| json!({"v": 1, "id": format!("h{n}"), "type": "history"}).to_string())
        .collect()
}

/// What the connection sent and nobody read yet.
fn drain(sent: &mut mpsc::UnboundedReceiver<String>) -> Vec<String> {
    std::iter::from_fn(|| sent.try_recv().ok()).collect()
}

/// The ids among `asks` that got no reply in `sent`.
fn unanswered(asks: &[String], sent: &[String]) -> Vec<String> {
    let answered: Vec<String> = sent
        .iter()
        .filter(|line| ty(&serde_json::from_str(line).unwrap()) == "reply")
        .map(|line| id_of(line))
        .collect();
    asks.iter()
        .map(|ask| id_of(ask))
        .filter(|id| !answered.contains(id))
        .collect()
}

/// A source that takes its line when `next_line` is called, and hands it
/// over in a future that is ready at once: `std::future::ready`, as clippy's
/// `unused_async_trait_impl` rewrites an `async fn` that never awaits.
struct TakesWhenCalled(std::vec::IntoIter<String>);

impl LineSource for TakesWhenCalled {
    fn next_line(
        &mut self,
    ) -> impl Future<Output = Option<Result<String, crate::TransportError>>> + Send {
        std::future::ready(self.0.next().map(Ok))
    }
}

/// Every line such a source hands out is served: none goes with a future
/// the connection made and never polled.
async fn every_line_a_source_takes_when_called_is_served(runs: usize) {
    for run in 0..runs {
        let (s, _) = session(&answers(&[]), hub());
        let asks = histories(20);
        let (tx, mut sent) = mpsc::unbounded_channel();
        let source = TakesWhenCalled(asks.clone().into_iter());
        let grant = Grant::watch(Origin::Agent);
        let ended = serve(s.clone(), grant, Start::default(), source, Collect(tx)).await;

        assert!(matches!(ended, Ended::PeerLeft), "run {run}: {ended:?}");
        let unanswered = unanswered(&asks, &drain(&mut sent));
        assert!(
            unanswered.is_empty(),
            "run {run}: no reply to {unanswered:?}"
        );
        s.close().await;
    }
}

#[tokio::test]
async fn every_line_a_source_takes_when_called_is_served_on_one_thread() {
    every_line_a_source_takes_when_called_is_served(1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_line_a_source_takes_when_called_is_served_on_many_threads() {
    every_line_a_source_takes_when_called_is_served(20).await;
}

/// A source that reads a line over several awaits and loses what it read
/// when its future is dropped: `read_line`, which tokio documents as not
/// cancel-safe.
struct ReadsInPieces(BufReader<DuplexStream>);

impl LineSource for ReadsInPieces {
    async fn next_line(&mut self) -> Option<Result<String, crate::TransportError>> {
        let mut line = String::new();
        match self.0.read_line(&mut line).await {
            Ok(0) => None,
            Ok(_) => Some(Ok(line.trim_end().to_string())),
            Err(e) => Some(Err(e.into())),
        }
    }
}

/// A line that arrives in two pieces, with a reply going out between them,
/// still reaches the session whole through such a source.
async fn a_line_read_in_pieces_survives_a_reply_going_out_meanwhile(runs: usize) {
    for run in 0..runs {
        let (s, _) = session(&answers(&[]), hub());
        let (mut peer, ours) = tokio::io::duplex(1 << 16);
        let (tx, mut sent) = mpsc::unbounded_channel();
        let source = ReadsInPieces(BufReader::new(ours));
        let grant = Grant::watch(Origin::Agent);
        let served = tokio::spawn(serve(
            s.clone(),
            grant,
            Start::default(),
            source,
            Collect(tx),
        ));

        let first = json!({"v": 1, "id": "s1", "type": "snapshot"});
        let second = json!({"v": 1, "id": "h2", "type": "history"}).to_string();
        let (head, tail) = second.split_at(12);
        let asks = format!("{first}\n{head}");
        peer.write_all(asks.as_bytes()).await.unwrap();
        // The first reply goes out while the second line is half read.
        let replied = async {
            while let Some(line) = sent.recv().await {
                if line.contains(r#""id":"s1""#) {
                    return;
                }
            }
        };
        let replied = tokio::time::timeout(Duration::from_secs(10), replied).await;
        replied.unwrap_or_else(|_| panic!("run {run}: s1 got no reply"));
        peer.write_all(format!("{tail}\n").as_bytes())
            .await
            .unwrap();
        drop(peer);
        served.await.unwrap();

        let rest = drain(&mut sent);
        let unanswered = unanswered(&[second], &rest);
        assert!(
            unanswered.is_empty(),
            "run {run}: no reply to {unanswered:?}, only {rest:#?}"
        );
        s.close().await;
    }
}

#[tokio::test]
async fn a_line_read_in_pieces_survives_a_reply_going_out_meanwhile_on_one_thread() {
    a_line_read_in_pieces_survives_a_reply_going_out_meanwhile(1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_read_in_pieces_survives_a_reply_going_out_meanwhile_on_many_threads() {
    a_line_read_in_pieces_survives_a_reply_going_out_meanwhile(20).await;
}

/// A source that holds `serve` to what [`LineSource::next_line`] promises:
/// each call comes once the future of the call before it resolved, and none
/// after it returned None. Each read is pending once before it resolves, so
/// `serve` has a pass in which it could drop it.
struct Checked {
    asks: std::vec::IntoIter<String>,
    reading: bool,
    ended: bool,
}

impl LineSource for Checked {
    fn next_line(
        &mut self,
    ) -> impl Future<Output = Option<Result<String, crate::TransportError>>> + Send {
        assert!(!self.ended, "next_line was called after it returned None");
        assert!(
            !self.reading,
            "next_line was called before the future of the call before it resolved"
        );
        self.reading = true;
        let line = self.asks.next();
        self.ended = line.is_none();
        async move {
            tokio::task::yield_now().await;
            self.reading = false;
            line.map(Ok)
        }
    }
}

/// `serve` asks for a line only once the read before it resolved, keeps
/// every read until it does, and asks no more once the peer is gone.
async fn a_source_is_asked_for_a_line_only_once_the_read_before_it_resolved() {
    let (s, _) = session(&answers(&[]), hub());
    let asks = histories(20);
    let (tx, mut sent) = mpsc::unbounded_channel();
    let source = Checked {
        asks: asks.clone().into_iter(),
        reading: false,
        ended: false,
    };
    let grant = Grant::watch(Origin::Agent);
    let ended = serve(s.clone(), grant, Start::default(), source, Collect(tx)).await;

    assert!(matches!(ended, Ended::PeerLeft), "{ended:?}");
    let unanswered = unanswered(&asks, &drain(&mut sent));
    assert!(unanswered.is_empty(), "no reply to {unanswered:?}");
    s.close().await;
}

#[tokio::test]
async fn a_source_is_asked_for_a_line_only_once_the_read_before_it_resolved_on_one_thread() {
    a_source_is_asked_for_a_line_only_once_the_read_before_it_resolved().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_source_is_asked_for_a_line_only_once_the_read_before_it_resolved_on_many_threads() {
    a_source_is_asked_for_a_line_only_once_the_read_before_it_resolved().await;
}

/// A source that tells, through its flag, when it is dropped.
struct Told(
    std::vec::IntoIter<String>,
    Arc<std::sync::atomic::AtomicBool>,
);

impl Drop for Told {
    fn drop(&mut self) {
        self.1.store(true, Ordering::SeqCst);
    }
}

impl LineSource for Told {
    fn next_line(
        &mut self,
    ) -> impl Future<Output = Option<Result<String, crate::TransportError>>> + Send {
        std::future::ready(self.0.next().map(Ok))
    }
}

/// Into a channel, each line with whether the source was dropped by then.
struct Noting(
    mpsc::UnboundedSender<(String, bool)>,
    Arc<std::sync::atomic::AtomicBool>,
);

impl LineSink for Noting {
    fn send_line(
        &mut self,
        line: String,
    ) -> impl Future<Output = Result<(), crate::TransportError>> + Send {
        let _ = self.0.send((line, self.1.load(Ordering::SeqCst)));
        std::future::ready(Ok(()))
    }
}

/// The peer's input ends while its turn runs: the source is kept until the
/// connection ends, so the turn's reply goes out before the source drops.
#[tokio::test]
async fn the_source_is_kept_until_the_connection_ends() {
    let model = ScriptedProvider::new([
        tool_answer(&[("c1", "Wait", json!({}))]),
        text_answer("done"),
    ]);
    let (s, go) = session(&model, hub());
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let source = Told(
        vec![turn("t1", "wait").to_string()].into_iter(),
        dropped.clone(),
    );
    let (tx, mut sent) = mpsc::unbounded_channel();
    let grant = Grant::command(Origin::Human);
    let served = tokio::spawn(serve(
        s.clone(),
        grant,
        Start::default(),
        source,
        Noting(tx, dropped.clone()),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    go.notify_one();
    let ended = served.await.unwrap();

    assert!(matches!(ended, Ended::PeerLeft), "{ended:?}");
    let (_, gone) = std::iter::from_fn(|| sent.try_recv().ok())
        .find(|(line, _)| line.contains(r#""id":"t1""#) && line.contains(r#""type":"reply""#))
        .expect("t1 answered");
    assert!(!gone, "the source was dropped before t1's reply went out");
    assert!(
        dropped.load(Ordering::SeqCst),
        "dropped with the connection"
    );
    s.close().await;
}
