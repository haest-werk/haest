//! The cap on requests waiting for their replies bounds the work one peer
//! makes, and never takes away its stop button: at the cap a request that
//! would wait too is refused, and one answered at once is still served.

use super::*;
use crate::{
    link::{MAX_IN_FLIGHT, OUT_BUFFER},
    testing::Hooked,
};

fn history(id: &str) -> Value {
    json!({"v": 1, "id": id, "type": "history"})
}

/// What `fut` gives, or a failed test when that takes over 2 s.
async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    let waited = tokio::time::timeout(Duration::from_secs(2), fut).await;
    waited.unwrap_or_else(|_| panic!("not within 2 s: {what}"))
}

/// A session whose `history` calls are counted while they wait (`waiting`),
/// and the most that ever waited at once (`most`).
fn counted(
    inner: SessionHandle<Agent>,
    waiting: &Arc<AtomicUsize>,
    most: &Arc<AtomicUsize>,
) -> Hooked {
    let (waiting, most) = (waiting.clone(), most.clone());
    Hooked::new(inner, move |inner| {
        let (waiting, most) = (waiting.clone(), most.clone());
        Box::pin(async move {
            let now = waiting.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            let history = inner.history().await;
            waiting.fetch_sub(1, Ordering::SeqCst);
            history
        })
    })
}

/// A peer that sends far more requests than it waits for replies to makes
/// no more work than the cap: the ones past it that would wait are refused
/// at once, the rest answered when the turn ends.
#[tokio::test]
async fn one_connection_has_at_most_its_cap_of_requests_in_flight() {
    let model = ScriptedProvider::new([
        tool_answer(&[("c1", "Wait", json!({}))]),
        text_answer("done"),
    ]);
    let (inner, go) = session(&model, hub());
    let (waiting, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut peer = Peer::parent(counted(inner, &waiting, &most));
    peer.send(turn("t1", "wait")).await;
    peer.until(|f| ty(f) == "assistant").await;

    // History asked during a turn answers when it ends: each one waits.
    let asked = MAX_IN_FLIGHT * 4;
    for n in 0..asked {
        peer.send(history(&format!("h{n}"))).await;
    }
    // With the turn, the history asked first fills the cap.
    replies(&mut peer, asked - (MAX_IN_FLIGHT - 1), false).await;
    let busy = waiting.load(Ordering::SeqCst);
    assert!(
        busy < MAX_IN_FLIGHT,
        "{busy} history calls wait at once, the turn among the cap's {MAX_IN_FLIGHT}"
    );

    go.notify_one();
    replies(&mut peer, MAX_IN_FLIGHT, true).await;
    assert!(most.load(Ordering::SeqCst) <= MAX_IN_FLIGHT);
}

/// Reads frames up to the `n`th reply; each reply's `ok` is `ok`.
async fn replies(peer: &mut Peer, n: usize, ok: bool) {
    let mut seen = 0;
    while seen < n {
        let frame = peer.next().await.expect("the connection is still open");
        if ty(&frame) == "reply" {
            assert_eq!(frame["ok"], ok, "{frame}");
            seen += 1;
        }
    }
}

/// A turn that runs, and history asked during it until the cap is reached
/// (a dashboard that polls once a second, half a minute into a long turn):
/// an approval answer is still read, an `interrupt` still stops the turn,
/// and one more request that would wait is refused at once, with its id.
#[tokio::test]
async fn at_the_cap_an_interrupt_and_an_approval_answer_are_still_served() {
    let model = ScriptedProvider::new([
        tool_answer(&[("c1", "Wait", json!({}))]),
        text_answer("never"),
    ]);
    let hub = hub();
    let prompter = SessionPrompter::new(hub.clone());
    let (inner, _) = session(&model, hub);
    let waiting = Arc::new(AtomicUsize::new(0));
    let mut peer = Peer::parent(counted(inner, &waiting, &Arc::default()));
    peer.send(turn("t1", "wait")).await;
    peer.until(|f| ty(f) == "assistant").await;
    // History asked during a turn waits for its end: with the turn, the cap.
    for n in 1..MAX_IN_FLIGHT {
        peer.send(history(&format!("h{n}"))).await;
    }
    let full = async {
        while waiting.load(Ordering::SeqCst) < MAX_IN_FLIGHT - 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    within("the history requests wait for the turn", full).await;

    peer.send(history("over")).await;
    let over = within("the request past the cap refused", peer.reply("over")).await;
    assert_eq!(over.last().unwrap()["ok"], false, "{over:#?}");

    let req = ApprovalRequest {
        tool_name: "Bash".into(),
        description: "delete the build dir".into(),
        input_preview: "rm -rf build".into(),
        tool_use_id: None,
        key: Some("rm -rf build".into()),
    };
    let asked = tokio::spawn(async move { prompter.prompt(&req).await });
    let ask = peer.until(|f| ty(f) == "approval_ask").await.pop().unwrap();
    let answer =
        json!({"v": 1, "id": "a1", "type": "approval_answer", "ask": ask["ask"], "answer": "deny"});
    peer.send(answer).await;
    let asked = within("the approval answer read", asked).await;
    assert_eq!(asked.unwrap(), ApprovalAnswer::Deny);

    peer.send(control("stop", "interrupt")).await;
    let frames = within("the turn stopped", peer.reply("t1")).await;
    let reply = frames.last().unwrap();
    assert_eq!(reply["result"]["stopReason"], "aborted", "{frames:#?}");
}

/// `left` copies of one line, each counted as it is read; then a peer that
/// stays and sends nothing more.
struct Flood {
    line: String,
    left: usize,
    read: Arc<AtomicUsize>,
}

impl LineSource for Flood {
    async fn next_line(&mut self) -> Option<Result<String, crate::TransportError>> {
        if self.left == 0 {
            std::future::pending::<()>().await;
        }
        self.left -= 1;
        self.read.fetch_add(1, Ordering::SeqCst);
        Some(Ok(self.line.clone()))
    }
}

/// Requests answered at once are served at the cap, yet a peer that sends
/// them and never reads a reply still makes bounded work: once its replies
/// fill the out buffer, the connection reads at most twice the cap more.
#[tokio::test]
async fn a_peer_that_never_reads_its_replies_makes_bounded_work() {
    let (s, _) = session(&answers(&[]), hub());
    let read = Arc::new(AtomicUsize::new(0));
    let flood = Flood {
        line: json!({"v": 1, "id": "s", "type": "snapshot"}).to_string(),
        left: OUT_BUFFER * 4,
        read: read.clone(),
    };
    let grant = Grant::watch(Origin::Agent);
    let sink = Stalls { room: 0 };
    let served = tokio::spawn(serve(s.clone(), grant, Start::default(), flood, sink));

    // Until the connection stops reading.
    let mut last = usize::MAX;
    let still = async {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let now = read.load(Ordering::SeqCst);
            if now == last {
                return now;
            }
            last = now;
        }
    };
    let read = within("the connection stops reading", still).await;
    let most = OUT_BUFFER + 2 * MAX_IN_FLIGHT;
    assert!(
        (OUT_BUFFER..=most).contains(&read),
        "{read} lines read from a peer that reads nothing, at most {most}"
    );
    served.abort();
    s.close().await;
}
