//! The broker link against a broker in the test: dialed, acked, cut,
//! dialed again, refused.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use haest_core::testing::{text_answer, tool_answer};
use haest_session::{Hub, HubOptions, Input};
use haest_wire::{Classify, FrameKind, Level, Origin};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::server::{ErrorResponse, Request, Response},
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};

use super::{Backoff, Broker};
use crate::{
    Ended, Grant, TransportError,
    testing::{answers, hub, session},
};

/// Waits short enough for a test.
const FAST: Backoff = Backoff {
    base: Duration::from_millis(10),
    cap: Duration::from_millis(50),
};

/// A broker: every link that dials it is handed to the test.
struct TestBroker {
    url: String,
    links: mpsc::UnboundedReceiver<Link>,
    accepted: Arc<AtomicUsize>,
    /// The path and query each link asked for.
    paths: Arc<Mutex<Vec<String>>>,
}

/// The broker's end of one link.
struct Link(WebSocketStream<tokio::net::TcpStream>);

impl TestBroker {
    /// Accepting links; with `refuse`, each handshake is refused with it.
    #[expect(
        clippy::result_large_err,
        reason = "tungstenite's handshake callback returns its own ErrorResponse"
    )]
    async fn start(refuse: Option<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/agents", listener.local_addr().unwrap());
        let (tx, links) = mpsc::unbounded_channel();
        let accepted = Arc::new(AtomicUsize::new(0));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let (count, seen) = (accepted.clone(), paths.clone());
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let seen = seen.clone();
                let check =
                    move |req: &Request, res: Response| -> Result<Response, ErrorResponse> {
                        seen.lock().unwrap().push(req.uri().to_string());
                        match refuse {
                            None => Ok(res),
                            Some(status) => {
                                let mut no = ErrorResponse::new(None);
                                *no.status_mut() = status.try_into().unwrap();
                                Err(no)
                            }
                        }
                    };
                if let Ok(ws) = tokio_tungstenite::accept_hdr_async(tcp, check).await {
                    let _ = tx.send(Link(ws));
                }
            }
        });
        Self {
            url,
            links,
            accepted,
            paths,
        }
    }

    async fn link(&mut self) -> Link {
        tokio::time::timeout(Duration::from_secs(5), self.links.recv())
            .await
            .expect("a link within 5 s")
            .unwrap()
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Link {
    /// The next frame; None when the link closed.
    async fn next(&mut self) -> Option<Value> {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), self.0.next())
                .await
                .expect("a frame within 5 s")?;
            match msg {
                Ok(Message::Text(text)) => {
                    return Some(serde_json::from_str(text.as_str()).unwrap());
                }
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    /// Frames up to and including the first `stop` matches.
    async fn until(&mut self, stop: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let frame = self.next().await.expect("the link is open");
            let done = stop(&frame);
            seen.push(frame);
            if done {
                return seen;
            }
        }
    }

    async fn send(&mut self, request: Value) {
        self.0
            .send(Message::Text(request.to_string().into()))
            .await
            .unwrap();
    }

    /// The broker's ack: it holds everything up to `after`; the rest, and
    /// what comes, please.
    async fn ack(&mut self, after: u64) {
        self.send(json!({"v": 1, "id": "ack", "type": "events", "after": after, "follow": true}))
            .await;
    }

    async fn close(mut self, code: u16, reason: &str) {
        let frame = CloseFrame {
            code: CloseCode::from(code),
            reason: reason.into(),
        };
        let _ = self.0.close(Some(frame)).await;
    }
}

fn ty(frame: &Value) -> &str {
    frame["type"].as_str().unwrap_or_default()
}

fn seq(frame: &Value) -> u64 {
    frame["seq"].as_u64().unwrap()
}

/// Stream frames only: a connection's own (hello, reply, gap) are not.
fn stream(frames: &[Value]) -> Vec<&Value> {
    frames
        .iter()
        .filter(|f| !matches!(ty(f), "hello" | "reply" | "gap"))
        .collect()
}

#[tokio::test]
async fn the_hello_carries_the_token_and_nothing_else_goes_before_the_ack() {
    let mut broker = TestBroker::start(None).await;
    let (backend, _) = session(&answers(&["before"]), hub());
    let _ = backend.turn(Input::human("first")).await.unwrap();
    let link = Broker::new(&broker.url)
        .with_token("t0k")
        .with_backoff(FAST);
    let _attached = link.attach(backend, Grant::watch(Origin::Agent)).unwrap();

    let mut side = broker.link().await;
    let hello = side.next().await.unwrap();
    assert_eq!(ty(&hello), "hello");
    assert_eq!(hello["token"], "t0k");
    assert_eq!(hello["access"], "watch");
    assert!(
        broker
            .paths
            .lock()
            .unwrap()
            .iter()
            .all(|p| !p.contains("t0k")),
        "never in the URL"
    );
    let early = tokio::time::timeout(Duration::from_millis(200), side.0.next()).await;
    assert!(early.is_err(), "nothing before the ack: {early:?}");

    side.ack(0).await;
    let frames = side.until(|f| ty(f) == "reply").await;
    assert!(
        frames
            .iter()
            .any(|f| ty(f) == "assistant" && f["message"]["content"][0]["text"] == "before")
    );
}

#[tokio::test]
async fn a_cut_link_resumes_from_the_brokers_ack_and_loses_nothing() {
    let mut broker = TestBroker::start(None).await;
    let model = haest_core::testing::ScriptedProvider::new([
        tool_answer(&[("c1", "Wait", json!({}))]),
        text_answer("after the cut"),
    ]);
    let hub = hub();
    // Every frame, as published: what the broker must end up with.
    let mut everything = hub.subscribe(0);
    let (backend, go) = session(&model, hub);
    let _attached = Broker::new(&broker.url)
        .with_backoff(FAST)
        .attach(backend.clone(), Grant::watch(Origin::Agent))
        .unwrap();

    let mut first = broker.link().await;
    first.next().await.unwrap();
    first.ack(0).await;
    let turn = tokio::spawn({
        let backend = backend.clone();
        async move { backend.turn(Input::human("wait")).await }
    });
    let before = first.until(|f| ty(f) == "assistant").await;
    let held: Vec<u64> = stream(&before).iter().map(|f| seq(f)).collect();
    // The link is cut, and the last two frames sent never reached the
    // broker (they were in a socket that was closing).
    drop(first);
    let kept = held[held.len() - 3];
    go.notify_one();

    let mut second = broker.link().await;
    assert_eq!(ty(&second.next().await.unwrap()), "hello");
    second.ack(kept).await;
    let after = second.until(|f| ty(f) == "turn_end").await;
    let _ = turn.await.unwrap().unwrap();

    let resent: Vec<u64> = stream(&after).iter().map(|f| seq(f)).collect();
    assert!(
        resent[0] <= held[held.len() - 1],
        "from the ack, not from what was sent: {resent:?}"
    );
    let all: Vec<u64> = held
        .iter()
        .copied()
        .filter(|n| *n <= kept)
        .chain(resent)
        .collect();
    assert!(
        all.windows(2).all(|w| w[0] < w[1]),
        "nothing twice: {all:?}"
    );
    assert!(after.iter().all(|f| ty(f) != "gap"));
    // Nothing lost: every frame but a model call's pieces, which the ring
    // folds into its answer once that is in (the answer holds their text).
    let last = *all.last().unwrap();
    while let Some(frame) = everything.recv().await {
        if frame.seq > last {
            break;
        }
        let piece = matches!(frame.body.kind(), FrameKind::Delta);
        assert!(
            piece || all.contains(&frame.seq),
            "frame {} lost: {all:?}",
            frame.seq
        );
    }
    assert!(
        after
            .iter()
            .any(|f| ty(f) == "assistant" && f["message"]["content"][0]["text"] == "after the cut")
    );
}

#[tokio::test]
async fn a_gap_when_the_ring_no_longer_holds_what_the_broker_missed() {
    let mut broker = TestBroker::start(None).await;
    let hub = Hub::new_with_options(
        "s1",
        HubOptions {
            ring: 2,
            buffer: 64,
        },
    );
    for n in 0..10 {
        hub.notice(Level::Info, format!("line {n}"));
    }
    let (s, _) = session(&answers(&[]), hub);
    let _attached = Broker::new(&broker.url)
        .with_backoff(FAST)
        .attach(s, Grant::watch(Origin::Agent))
        .unwrap();

    let mut side = broker.link().await;
    side.next().await.unwrap();
    side.ack(3).await;
    let first = side.next().await.unwrap();
    assert_eq!(ty(&first), "gap", "{first}");
    assert_eq!(first["after"], 3);
}

#[tokio::test]
async fn a_refused_token_stops_the_dialing() {
    // Closed with 4401 after the hello.
    let mut broker = TestBroker::start(None).await;
    let (s, _) = session(&answers(&[]), hub());
    let attached = Broker::new(&broker.url)
        .with_token("wrong")
        .with_backoff(FAST)
        .attach(s, Grant::watch(Origin::Agent))
        .unwrap();
    let mut side = broker.link().await;
    side.next().await.unwrap();
    side.close(4401, "bad token").await;
    let ended = tokio::time::timeout(Duration::from_secs(5), attached.ended())
        .await
        .unwrap();
    assert!(
        matches!(&ended, Ended::Failed(TransportError::Auth { code: 4401, reason }) if reason == "bad token"),
        "{ended:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(broker.accepted(), 1, "not dialed again");

    // Refused at the handshake.
    let broker = TestBroker::start(Some(401)).await;
    let (s, _) = session(&answers(&[]), hub());
    let attached = Broker::new(&broker.url)
        .with_backoff(FAST)
        .attach(s, Grant::watch(Origin::Agent))
        .unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(5), attached.ended())
        .await
        .unwrap();
    assert!(
        matches!(ended, Ended::Failed(TransportError::Auth { code: 401, .. })),
        "{ended:?}"
    );
    assert_eq!(broker.accepted(), 1);
}

#[tokio::test]
async fn a_link_the_broker_closes_is_dialed_again() {
    let mut broker = TestBroker::start(None).await;
    let (s, _) = session(&answers(&[]), hub());
    let _attached = Broker::new(&broker.url)
        .with_backoff(FAST)
        .attach(s, Grant::watch(Origin::Agent))
        .unwrap();
    let mut side = broker.link().await;
    side.next().await.unwrap();
    side.close(1001, "going away").await;
    let mut again = broker.link().await;
    assert_eq!(ty(&again.next().await.unwrap()), "hello");
    assert_eq!(broker.accepted(), 2);
}

/// Silence limits short enough for a test.
const SHORT: Duration = Duration::from_millis(300);

/// A path that went silent sends no close and no reset: the link notices
/// the silence itself, while the conversation is idle, and dials again.
#[tokio::test]
async fn a_broker_that_goes_silent_is_dialed_again() {
    let mut broker = TestBroker::start(None).await;
    let (s, _) = session(&answers(&[]), hub());
    let _attached = Broker::new(&broker.url)
        .with_backoff(FAST)
        .with_silence_limit(SHORT)
        .attach(s, Grant::watch(Origin::Agent))
        .unwrap();
    let mut first = broker.link().await;
    first.next().await.unwrap();
    first.ack(0).await;
    // The broker's end of a dead path: held open, never read again, so no
    // ping is answered.
    let mut again = broker.link().await;
    assert_eq!(ty(&again.next().await.unwrap()), "hello");
    assert_eq!(broker.accepted(), 2);
    drop(first);
}

/// The broker's end of one link, read to its end: the pings it got.
fn count_pings(mut side: Link) -> Arc<AtomicUsize> {
    let pings = Arc::new(AtomicUsize::new(0));
    let counted = pings.clone();
    tokio::spawn(async move {
        // Reading is what sends the pong its WebSocket owes a ping.
        while let Some(Ok(msg)) = side.0.next().await {
            if matches!(msg, Message::Ping(_)) {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    pings
}

/// A live broker that only listens is pinged, answers, and keeps its link,
/// while the conversation sends it frames and while it is idle.
#[tokio::test]
async fn a_live_broker_that_never_writes_keeps_its_link() {
    let mut broker = TestBroker::start(None).await;
    let hub = hub();
    let (s, _) = session(&answers(&[]), hub.clone());
    let _attached = Broker::new(&broker.url)
        .with_backoff(FAST)
        .with_silence_limit(SHORT)
        .attach(s, Grant::watch(Origin::Agent))
        .unwrap();
    let mut side = broker.link().await;
    side.next().await.unwrap();
    side.ack(0).await;
    let pings = count_pings(side);
    for n in 0..30 {
        hub.notice(Level::Info, format!("line {n}"));
        tokio::time::sleep(SHORT / 15).await;
    }
    tokio::time::sleep(SHORT * 3).await;
    assert_eq!(broker.accepted(), 1, "never dialed again");
    assert!(pings.load(Ordering::SeqCst) >= 2, "it was pinged");
}

/// Without a limit the link sends no ping and waits on a silent broker as
/// long as it stays open.
#[tokio::test]
async fn without_a_silence_limit_the_link_never_pings() {
    let link = Broker::new("wss://broker.example/agents");
    assert_eq!(link.silence_limit(), Some(Duration::from_secs(60)));
    assert_eq!(link.silence_limit(), Some(Broker::SILENCE_LIMIT));

    let mut broker = TestBroker::start(None).await;
    let (s, _) = session(&answers(&[]), hub());
    let _attached = Broker::new(&broker.url)
        .with_backoff(FAST)
        .with_silence_limit(SHORT)
        .without_silence_limit()
        .attach(s, Grant::watch(Origin::Agent))
        .unwrap();
    let mut side = broker.link().await;
    side.next().await.unwrap();
    side.ack(0).await;
    let pings = count_pings(side);
    tokio::time::sleep(SHORT * 3).await;
    assert_eq!(broker.accepted(), 1);
    assert_eq!(pings.load(Ordering::SeqCst), 0);
}

/// A limit past the clock's range (`Duration::MAX`, a big number of seconds
/// from a host's config) is no limit: the link serves, pinging no one, and
/// ends with the conversation.
#[tokio::test]
async fn a_silence_limit_past_the_clocks_range_is_no_limit() {
    for limit in [Duration::MAX, Duration::from_secs(u64::MAX)] {
        let mut broker = TestBroker::start(None).await;
        let (s, _) = session(&answers(&[]), hub());
        let attached = Broker::new(&broker.url)
            .with_backoff(FAST)
            .with_silence_limit(limit)
            .attach(s.clone(), Grant::watch(Origin::Agent))
            .unwrap();
        let mut side = broker.link().await;
        assert_eq!(ty(&side.next().await.unwrap()), "hello", "{limit:?}");
        side.ack(0).await;
        side.until(|f| ty(f) == "reply").await;
        s.close().await;
        let ended = tokio::time::timeout(Duration::from_secs(5), attached.ended())
            .await
            .expect("the link ends with the conversation");
        assert!(matches!(ended, Ended::Closed), "{limit:?}: {ended:?}");
        assert_eq!(broker.accepted(), 1, "{limit:?}");
    }
}

#[tokio::test]
async fn the_brokers_commands_obey_the_access_the_host_gave() {
    for (grant, runs) in [
        (Grant::watch(Origin::Agent), false),
        (Grant::command(Origin::Agent), true),
    ] {
        let mut broker = TestBroker::start(None).await;
        let model = answers(&["from the broker"]);
        let (s, _) = session(&model, hub());
        let _attached = Broker::new(&broker.url)
            .with_backoff(FAST)
            .attach(s, grant)
            .unwrap();
        let mut side = broker.link().await;
        side.next().await.unwrap();
        side.send(json!({"v": 1, "id": "t1", "type": "turn", "text": "hi"}))
            .await;
        let reply = side.until(|f| ty(f) == "reply").await.pop().unwrap();
        assert_eq!(reply["ok"], runs, "{reply}");
        assert_eq!(model.requests().len(), usize::from(runs));
    }
}

#[tokio::test]
async fn the_link_ends_with_the_conversation_and_only_a_ws_url_is_dialed() {
    let mut broker = TestBroker::start(None).await;
    let (backend, _) = session(&answers(&[]), hub());
    let attached = Broker::new(&broker.url)
        .with_backoff(FAST)
        .attach(backend.clone(), Grant::watch(Origin::Agent))
        .unwrap();
    broker.link().await.next().await.unwrap();
    backend.close().await;
    let ended = tokio::time::timeout(Duration::from_secs(5), attached.ended())
        .await
        .unwrap();
    assert!(matches!(ended, Ended::Closed), "{ended:?}");

    let (s, _) = session(&answers(&[]), hub());
    let wrong = Broker::new("https://broker.example/agents").attach(s, Grant::watch(Origin::Agent));
    assert!(matches!(wrong, Err(TransportError::Url(_))), "{wrong:?}");
}

#[test]
fn the_wait_is_full_jitter_up_to_a_doubling_cap() {
    let b = Backoff::default();
    for attempt in 0..40 {
        let ceiling = b.base.saturating_mul(1 << attempt.min(31)).min(b.cap);
        let wait = b.delay(attempt, super::jitter());
        assert!(wait <= ceiling, "{attempt}: {wait:?} > {ceiling:?}");
    }
    let draws: std::collections::HashSet<u64> =
        (0..20).map(|_| (super::jitter() * 1e9) as u64).collect();
    assert!(draws.len() > 15, "the waits differ: {draws:?}");
}

#[test]
fn the_token_never_shows_in_debug_output() {
    let link = Broker::new("wss://broker.example/agents").with_token("TOKEN-SECRET");
    let shown = format!("{link:?}");
    assert!(!shown.contains("SECRET"), "{shown}");
    assert!(shown.contains("broker.example"), "{shown}");
}

/// A credential in the URL (userinfo, a query's value) shows nowhere: not
/// in the link's Debug, not in the error for a URL it does not dial.
#[tokio::test]
async fn a_credential_in_the_url_never_shows() {
    let link = Broker::new("wss://agent:s3cret@broker.example/agents?key=k3y");
    let shown = format!("{link:?}");
    assert!(
        !shown.contains("s3cret") && !shown.contains("k3y"),
        "{shown}"
    );
    assert!(shown.contains("broker.example/agents?key="), "{shown}");

    let (s, _) = session(&answers(&[]), hub());
    let refused = Broker::new("https://agent:s3cret@broker.example/?key=k3y")
        .attach(s.clone(), Grant::watch(Origin::Agent))
        .expect_err("not a ws:// URL");
    for shown in [refused.to_string(), format!("{refused:?}")] {
        assert!(
            !shown.contains("s3cret") && !shown.contains("k3y"),
            "{shown}"
        );
    }
    s.close().await;
}
