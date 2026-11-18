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
/// bounded work: the session holds no more than the cap of them, the ones
/// past the cap that would wait are refused, and every request gets its
/// reply in the end.
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
    tokio::time::sleep(Duration::from_millis(100)).await;
    let busy = waiting.load(Ordering::SeqCst);
    assert!(
        busy < MAX_IN_FLIGHT,
        "{busy} history calls wait at once, the turn among the cap's {MAX_IN_FLIGHT}"
    );

    go.notify_one();
    let (mut served, mut refused) = (0, 0);
    while served + refused < asked + 1 {
        let frame = peer.next().await.expect("the connection is still open");
        if ty(&frame) == "reply" {
            *(if frame["ok"] == true {
                &mut served
            } else {
                &mut refused
            }) += 1;
        }
    }
    // Refused only once the turn and as many history calls as fill the cap
    // were held.
    assert!(served >= MAX_IN_FLIGHT, "{served} served");
    assert!(refused > 0, "none past the cap was refused");
    assert!(most.load(Ordering::SeqCst) < MAX_IN_FLIGHT);
}

/// A turn that runs, and history asked during it until the cap is reached
/// (a dashboard that polls once a second, half a minute into a long turn).
async fn fill_the_cap(peer: &mut Peer, waiting: &AtomicUsize) {
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
}

/// At the cap, an approval answer is still read, an `interrupt` still stops
/// the turn, and one more request that would wait is refused at once, with
/// its id.
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
    fill_the_cap(&mut peer, &waiting).await;

    peer.send(history("over")).await;
    let over = within("the request past the cap refused", peer.reply("over")).await;
    assert_eq!(over.last().unwrap()["ok"], false, "{over:#?}");

    let req = ApprovalRequest::new("Bash", "delete the build dir")
        .with_input_preview("rm -rf build")
        .with_key("rm -rf build");
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

/// At the cap, an `events` request (a dashboard that catches up again) is
/// still served: the stream's ring answers it, never the session.
#[tokio::test]
async fn at_the_cap_an_events_request_is_still_served() {
    let model = ScriptedProvider::new([
        tool_answer(&[("c1", "Wait", json!({}))]),
        text_answer("never"),
    ]);
    let (inner, _) = session(&model, hub());
    let waiting = Arc::new(AtomicUsize::new(0));
    let mut peer = Peer::parent(counted(inner.clone(), &waiting, &Arc::default()));
    fill_the_cap(&mut peer, &waiting).await;

    let events = json!({"v": 1, "id": "e", "type": "events", "after": 0, "follow": true});
    peer.send(events).await;
    let frames = within("the events reply", peer.reply("e")).await;
    let reply = frames.last().unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    inner.close().await;
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

    // The out buffer is full of replies, then the connection stops reading
    // (one that never stops reads every line in far less than the wait).
    let filled = async {
        while read.load(Ordering::SeqCst) <= OUT_BUFFER {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let filled = tokio::time::timeout(Duration::from_secs(10), filled).await;
    filled.expect("the replies fill the out buffer");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let read = read.load(Ordering::SeqCst);
    let most = OUT_BUFFER + 2 * MAX_IN_FLIGHT;
    assert!(
        read <= most,
        "{read} lines read from a peer that reads nothing, at most {most}"
    );
    served.abort();
    s.close().await;
}

/// Requests the session answers at once (no turn runs), written in one go
/// by a parent that queues its work, are all served, the turn after them
/// too: the cap counts what the session holds, never a request that has not
/// reached it yet or was already answered.
#[tokio::test]
async fn a_burst_the_session_answers_at_once_is_never_refused() {
    let (s, _) = session(&answers(&["one"]), hub());
    let mut peer = Peer::attach(s.clone(), Grant::command(Origin::Human), Start::default());
    let mut lines: Vec<String> = (0..MAX_IN_FLIGHT * 3)
        .map(|n| history(&format!("h{n}")).to_string())
        .collect();
    lines.push(turn("t", "hi").to_string());
    peer.send_raw(&lines.join("\n")).await;
    peer.end_input().await;

    let (rest, _) = peer.rest().await;
    let replies: Vec<&Value> = rest.iter().filter(|f| ty(f) == "reply").collect();
    let refused: Vec<&&Value> = replies.iter().filter(|f| f["ok"] == false).collect();
    assert!(refused.is_empty(), "{refused:#?}");
    assert_eq!(replies.len(), lines.len());
    s.close().await;
}

/// A burst of requests that wait, written in one go while a turn runs (a
/// parent that queues its work), meets the cap as requests sent one at a
/// time do: the session holds no more than the cap of them, the rest are
/// refused, and an `interrupt` written after them still stops the turn.
async fn a_burst_during_a_turn(runs: usize) {
    for run in 0..runs {
        let model = ScriptedProvider::new([
            tool_answer(&[("c1", "Wait", json!({}))]),
            text_answer("never"),
        ]);
        let (inner, _) = session(&model, hub());
        let (waiting, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let mut peer = Peer::parent(counted(inner.clone(), &waiting, &most));
        peer.send(turn("t1", "wait")).await;
        peer.until(|f| ty(f) == "assistant").await;

        let mut lines: Vec<String> = (0..2 * MAX_IN_FLIGHT)
            .map(|n| history(&format!("h{n}")).to_string())
            .collect();
        lines.push(control("stop", "interrupt").to_string());
        peer.send_raw(&lines.join("\n")).await;

        let frames = within("the turn stopped", peer.reply("t1")).await;
        let reply = frames.last().unwrap();
        assert_eq!(
            reply["result"]["stopReason"], "aborted",
            "run {run}: {reply}"
        );
        // The turn is the cap's last place.
        let most = most.load(Ordering::SeqCst);
        assert!(
            most < MAX_IN_FLIGHT,
            "run {run}: {most} history calls held at once"
        );
        inner.close().await;
    }
}

#[tokio::test]
async fn a_burst_during_a_turn_meets_the_cap_and_keeps_the_stop_button() {
    a_burst_during_a_turn(1).await;
}

/// Where the newest task often runs first, as in the `haest` command.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_during_a_turn_meets_the_cap_and_keeps_the_stop_button_on_many_threads() {
    a_burst_during_a_turn(10).await;
}
