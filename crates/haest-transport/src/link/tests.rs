//! One connection, end to end over in-memory pipes: a peer writes request
//! lines and reads frame lines, as a parent process or a socket client does.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use haest_core::{
    Agent, TurnOutcome,
    testing::{ScriptedProvider, text_answer, tool_answer},
};
use haest_mods::{ApprovalAnswer, ApprovalRequest, Prompter};
use haest_session::{
    Backend, BackendRef, Control, History, Hub, HubOptions, Input, Meta, SessionError,
    SessionHandle, SessionPrompter, Subscription,
};
use haest_wire::{Answer, Level, Origin, Snapshot};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncWriteExt, DuplexStream, WriteHalf},
    task::JoinHandle,
};

use super::{Ended, Start, serve};
use crate::{
    Grant, LineSink, LineSource,
    ndjson::{NdjsonSink, NdjsonSource},
    testing::{answers, hub, session},
};

/// The other end of a connection.
struct Peer {
    to: WriteHalf<DuplexStream>,
    from: NdjsonSource<DuplexStream>,
    served: JoinHandle<Ended>,
}

impl Peer {
    fn attach(backend: impl Into<BackendRef>, grant: Grant, start: Start) -> Self {
        let backend: BackendRef = backend.into();
        let (ours, theirs) = tokio::io::duplex(1 << 20);
        let (read, write) = tokio::io::split(theirs);
        let (out, from) = tokio::io::duplex(1 << 20);
        let served = tokio::spawn(serve(
            backend,
            grant,
            start,
            NdjsonSource::new(read),
            NdjsonSink::new(out),
        ));
        let (_, to) = tokio::io::split(ours);
        drop(write);
        Self {
            to,
            from: NdjsonSource::new(from),
            served,
        }
    }

    /// A command peer that follows the stream from the start, as a parent on stdio does.
    fn parent(backend: impl Into<BackendRef>) -> Self {
        let start = Start {
            follow_from: Some(0),
            token: None,
        };
        Self::attach(backend, Grant::command(Origin::Human), start)
    }

    async fn send(&mut self, line: Value) {
        self.send_raw(&line.to_string()).await;
    }

    async fn send_raw(&mut self, line: &str) {
        self.to
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
    }

    /// The next frame; None once the connection ended.
    async fn next(&mut self) -> Option<Value> {
        let line = tokio::time::timeout(Duration::from_secs(10), self.from.next_line())
            .await
            .expect("a frame within 10 s")?;
        Some(serde_json::from_str(&line.unwrap()).unwrap())
    }

    /// Frames up to and including the first `stop` matches.
    async fn until(&mut self, stop: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let frame = self.next().await.expect("the connection is still open");
            let done = stop(&frame);
            seen.push(frame);
            if done {
                return seen;
            }
        }
    }

    /// Frames up to the reply to `id`; the reply is the last.
    async fn reply(&mut self, id: &str) -> Vec<Value> {
        self.until(|f| is_reply(f, id)).await
    }

    /// Stop writing: the connection's input ends.
    async fn end_input(&mut self) {
        self.to.shutdown().await.unwrap();
    }

    /// Every frame until the connection ends, and why it ended.
    async fn rest(mut self) -> (Vec<Value>, Ended) {
        let mut rest = Vec::new();
        while let Some(frame) = self.next().await {
            rest.push(frame);
        }
        (rest, self.served.await.unwrap())
    }
}

fn ty(frame: &Value) -> &str {
    frame["type"].as_str().unwrap_or_default()
}

fn is_reply(frame: &Value, id: &str) -> bool {
    ty(frame) == "reply" && frame["id"] == id
}

fn turn(id: &str, text: &str) -> Value {
    json!({"v": 1, "id": id, "type": "turn", "text": text})
}

fn control(id: &str, verb: &str) -> Value {
    json!({"v": 1, "id": id, "type": "control", "verb": verb})
}

fn position(frames: &[Value], pred: impl Fn(&Value) -> bool) -> usize {
    frames
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("not in {frames:#?}"))
}

#[tokio::test]
async fn hello_comes_first_and_says_what_the_connection_may_do() {
    let (backend, _) = session(&answers(&[]), hub());

    let mut watcher = Peer::attach(
        backend.clone(),
        Grant::watch(Origin::Agent),
        Start::default(),
    );
    let hello = watcher.next().await.unwrap();
    assert_eq!(ty(&hello), "hello");
    assert_eq!(hello["access"], "watch");
    assert_eq!(hello["model"], "scripted:m");
    assert_eq!(hello["pid"], std::process::id());
    assert_eq!(hello["sid"], "s1");
    // Nothing of the stream was sent yet: the connection's own frames say 0.
    assert_eq!(hello["seq"], 0);

    let mut driver = Peer::attach(backend, Grant::command(Origin::Human), Start::default());
    assert_eq!(driver.next().await.unwrap()["access"], "command");
}

#[tokio::test]
async fn two_turns_reply_at_their_end_after_every_frame_of_the_turn() {
    let model = answers(&["one", "two"]);
    let (s, _) = session(&model, hub());
    let mut peer = Peer::parent(s);

    for (id, answer) in [("t1", "one"), ("t2", "two")] {
        peer.send(turn(id, "go")).await;
        let frames = peer.reply(id).await;
        let reply = frames.last().unwrap();
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["result"]["stopReason"], "end_turn");

        let said = position(&frames, |f| ty(f) == "assistant");
        assert_eq!(frames[said]["message"]["content"][0]["text"], answer);
        let ended = position(&frames, |f| ty(f) == "turn_end");
        assert!(said < ended, "{frames:#?}");
        assert_turn_over_before(&frames);
    }
}

/// The frames before the reply (the last) end with the turn's end and the
/// idle status, and the reply carries that status's seq.
fn assert_turn_over_before(frames: &[Value]) {
    let n = frames.len();
    assert!(n >= 3, "{frames:#?}");
    let (ended, idle, reply) = (&frames[n - 3], &frames[n - 2], &frames[n - 1]);
    assert_eq!(ty(ended), "turn_end", "{frames:#?}");
    assert!(
        ty(idle) == "status" && idle["state"] == "idle",
        "{frames:#?}"
    );
    assert_eq!(reply["seq"], idle["seq"]);
}

/// Where the session's tasks run on several threads, as a host's do, the
/// reply still never overtakes the turn's frames.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turns_reply_never_overtakes_its_frames_on_many_threads() {
    let texts: Vec<String> = (0..60).map(|n| format!("answer {n}")).collect();
    let model = ScriptedProvider::new(texts.iter().map(|t| text_answer(t)));
    let (s, _) = session(&model, hub());
    let mut peer = Peer::parent(s);

    for n in 0..texts.len() {
        let id = format!("t{n}");
        peer.send(turn(&id, "go")).await;
        let frames = peer.reply(&id).await;
        assert_turn_over_before(&frames);
    }
}

#[tokio::test]
async fn an_interrupt_stops_the_running_tool_and_its_turn() {
    let model = ScriptedProvider::new([
        tool_answer(&[("c1", "Wait", json!({}))]),
        text_answer("never"),
    ]);
    let (s, _) = session(&model, hub());
    let mut peer = Peer::parent(s);

    peer.send(turn("t1", "wait")).await;
    peer.until(|f| ty(f) == "assistant").await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    peer.send(control("c1", "interrupt")).await;

    let frames = peer.reply("t1").await;
    let reply = frames.last().unwrap();
    assert_eq!(reply["result"]["stopReason"], "aborted", "{frames:#?}");
    let ended = position(&frames, |f| ty(f) == "turn_end");
    assert_eq!(frames[ended]["stopReason"], "aborted");
    assert!(frames.iter().any(|f| is_reply(f, "c1") && f["ok"] == true));
    // The model was not called again.
    assert_eq!(model.requests().len(), 1);
}

#[tokio::test]
async fn a_request_with_another_version_is_refused_with_its_id() {
    let (s, _) = session(&answers(&["unused"]), hub());
    let mut peer = Peer::parent(s);

    peer.send(json!({"v": 2, "id": "r9", "type": "turn", "text": "hi", "shape": "new"}))
        .await;
    let reply = peer.reply("r9").await.pop().unwrap();
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap().contains('2'), "{reply}");

    // Not a request at all: refused, with no id to echo, and why (the
    // parser's words after the wire's).
    peer.send_raw("not json").await;
    let reply = peer.until(|f| ty(f) == "reply").await.pop().unwrap();
    assert_eq!((&reply["id"], &reply["ok"]), (&json!(""), &json!(false)));
    let why = reply["error"].as_str().unwrap();
    assert!(why.starts_with("the frame is not JSON: expected"), "{why}");

    // The connection goes on.
    peer.send(json!({"v": 1, "id": "s1", "type": "snapshot"}))
        .await;
    assert_eq!(peer.reply("s1").await.pop().unwrap()["ok"], true);
}

#[tokio::test]
async fn a_watch_connection_may_read_but_not_drive() {
    let model = answers(&["unused"]);
    let (s, _) = session(&model, hub());
    let mut peer = Peer::attach(s, Grant::watch(Origin::Agent), Start::default());
    peer.next().await.unwrap();

    let drives = [
        turn("t1", "hi"),
        control("c1", "interrupt"),
        json!({"v": 1, "id": "a1", "type": "approval_answer", "ask": 1, "answer": "allow"}),
    ];
    for request in drives {
        let id = request["id"].as_str().unwrap().to_string();
        peer.send(request).await;
        let reply = peer.reply(&id).await.pop().unwrap();
        assert_eq!(reply["ok"], false);
        assert!(
            reply["error"].as_str().unwrap().contains("may only watch"),
            "{reply}"
        );
    }
    assert!(model.requests().is_empty(), "no turn ran");

    for (id, ty) in [("s", "snapshot"), ("h", "history"), ("e", "events")] {
        peer.send(json!({"v": 1, "id": id, "type": ty})).await;
        assert_eq!(peer.reply(id).await.pop().unwrap()["ok"], true);
    }
}

#[tokio::test]
async fn events_replays_then_replies_then_follows() {
    let model = answers(&["before", "after"]);
    let (backend, _) = session(&model, hub());
    let _ = backend
        .turn(haest_session::Input::human("first"))
        .await
        .unwrap();

    let mut peer = Peer::attach(
        backend.clone(),
        Grant::watch(Origin::Agent),
        Start::default(),
    );
    peer.next().await.unwrap();
    peer.send(json!({"v": 1, "id": "e1", "type": "events", "after": 0, "follow": true}))
        .await;
    let replayed = peer.reply("e1").await;
    assert!(
        replayed
            .iter()
            .any(|f| ty(f) == "assistant" && f["message"]["content"][0]["text"] == "before")
    );
    let seqs: Vec<u64> = replayed[..replayed.len() - 1]
        .iter()
        .map(|f| f["seq"].as_u64().unwrap())
        .collect();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "in order, none twice: {seqs:?}"
    );
    assert_eq!(replayed.last().unwrap()["seq"], *seqs.last().unwrap());

    // Then live.
    backend
        .submit(haest_session::Input::human("second"))
        .unwrap();
    let live = peer.until(|f| ty(f) == "turn_end").await;
    assert!(
        live.iter()
            .any(|f| ty(f) == "assistant" && f["message"]["content"][0]["text"] == "after")
    );
    assert!(live[0]["seq"].as_u64().unwrap() > *seqs.last().unwrap());

    // Without follow: the replay and the reply, nothing live after it.
    peer.send(json!({"v": 1, "id": "e2", "type": "events", "after": seqs[0]}))
        .await;
    let again = peer.reply("e2").await;
    assert!(again.len() > 1);
    peer.send(json!({"v": 1, "id": "s", "type": "snapshot"}))
        .await;
    let mut tail = peer.reply("s").await;
    tail.pop();
    assert!(tail.is_empty(), "nothing between: {tail:#?}");
}

/// A catch-up is a request like any other: a peer that asks for one and
/// closes its input still gets it and its reply, and an `events` a later one
/// replaced before its reply is answered too.
#[tokio::test]
async fn every_events_request_is_answered_when_replaced_or_when_the_input_ends() {
    let hub = hub();
    hub.notice(Level::Info, "line 1");
    let (s, _) = session(&answers(&[]), hub);
    let mut peer = Peer::attach(s, Grant::watch(Origin::Agent), Start::default());
    peer.send(json!({"v": 1, "id": "e1", "type": "events", "after": 0, "follow": true}))
        .await;
    peer.send(json!({"v": 1, "id": "e2", "type": "events", "after": 0, "follow": false}))
        .await;
    peer.end_input().await;

    let (rest, ended) = peer.rest().await;

    assert!(rest.iter().any(|f| is_reply(f, "e1")), "{rest:#?}");
    let e2 = position(&rest, |f| is_reply(f, "e2"));
    assert!(
        rest[..e2].iter().any(|f| f["text"] == "line 1"),
        "{rest:#?}"
    );
    assert!(matches!(ended, Ended::PeerLeft), "{ended:?}");
}

#[tokio::test]
async fn a_peer_later_than_the_ring_gets_a_gap_first() {
    let hub = Hub::new_with_options(
        "s1",
        HubOptions {
            ring: 4,
            buffer: 64,
        },
    );
    for n in 0..10 {
        hub.notice(Level::Info, format!("line {n}"));
    }
    let (s, _) = session(&answers(&[]), hub);
    let mut peer = Peer::attach(s, Grant::watch(Origin::Agent), Start::default());
    peer.next().await.unwrap();

    peer.send(json!({"v": 1, "id": "e1", "type": "events", "after": 2}))
        .await;
    let frames = peer.reply("e1").await;
    assert_eq!(ty(&frames[0]), "gap");
    assert_eq!(frames[0]["after"], 2);
    assert!(frames[1]["seq"].as_u64().unwrap() > 3, "{frames:#?}");
    // A gap moves nothing: the reply says the last real frame.
    let last = &frames[frames.len() - 2];
    assert_eq!(frames.last().unwrap()["seq"], last["seq"]);
}

#[tokio::test]
async fn a_peer_that_does_not_read_falls_behind_and_the_conversation_does_not_wait() {
    let hub = Hub::new_with_options("s1", HubOptions { ring: 8, buffer: 8 });
    let (s, _) = session(&answers(&[]), hub.clone());
    // A tiny pipe the peer does not read.
    let (ours, theirs) = tokio::io::duplex(1 << 20);
    let (read, _keep) = tokio::io::split(theirs);
    let (out, from) = tokio::io::duplex(256);
    let start = Start {
        follow_from: Some(0),
        token: None,
    };
    let served = tokio::spawn(serve(
        s,
        Grant::watch(Origin::Agent),
        start,
        NdjsonSource::new(read),
        NdjsonSink::new(out),
    ));

    // Far more than the pipe, the link's buffer and the hub's buffer hold.
    let total = super::OUT_BUFFER as u64 * 4;
    for n in 0..total {
        hub.notice(Level::Info, format!("line {n}"));
    }
    assert!(hub.last_seq() >= total, "publishing never waited");

    let mut from = NdjsonSource::new(from);
    let mut gaps = 0;
    let mut last = 0;
    while last < hub.last_seq() {
        let line = tokio::time::timeout(Duration::from_secs(10), from.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame: Value = serde_json::from_str(&line).unwrap();
        if ty(&frame) == "gap" {
            gaps += 1;
        } else {
            last = frame["seq"].as_u64().unwrap();
        }
    }
    assert!(gaps >= 1, "it was told it missed frames");
    drop(ours);
    served.abort();
}

/// A session whose `history` calls are counted while they wait.
struct Counted {
    inner: SessionHandle<Agent>,
    waiting: Arc<AtomicUsize>,
    most: Arc<AtomicUsize>,
}

#[async_trait]
impl Backend for Counted {
    fn meta(&self) -> Meta {
        self.inner.meta().clone()
    }
    async fn submit(&self, input: Input) -> Result<(), SessionError> {
        self.inner.submit(input)
    }
    async fn turn(&self, input: Input) -> Result<TurnOutcome, SessionError> {
        self.inner.turn(input).await
    }
    async fn control(&self, control: Control) -> Result<(), SessionError> {
        self.inner.control(control).await
    }
    async fn answer(&self, ask: u64, answer: Answer) -> Result<(), SessionError> {
        self.inner.answer(ask, answer)
    }
    fn subscribe(&self, after: u64) -> Subscription {
        self.inner.subscribe(after)
    }
    async fn snapshot(&self) -> Result<Snapshot, SessionError> {
        self.inner.snapshot().await
    }
    async fn history(&self) -> Result<History, SessionError> {
        let now = self.waiting.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        let history = self.inner.history().await;
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        history
    }
    async fn close(&self) {
        self.inner.close().await;
    }
}

/// A peer that sends far more requests than it waits for replies to makes
/// no more work than the cap: the rest wait unread in its pipe.
#[tokio::test]
async fn one_connection_has_at_most_its_cap_of_requests_in_flight() {
    let model = ScriptedProvider::new([
        tool_answer(&[("c1", "Wait", json!({}))]),
        text_answer("done"),
    ]);
    let (inner, go) = session(&model, hub());
    let (waiting, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let counted = Counted {
        inner,
        waiting: waiting.clone(),
        most: most.clone(),
    };
    let mut peer = Peer::parent(counted);
    peer.send(turn("t1", "wait")).await;
    peer.until(|f| ty(f) == "assistant").await;

    // History asked during a turn answers when it ends: each one waits.
    let asked = super::MAX_IN_FLIGHT * 4;
    for n in 0..asked {
        peer.send(json!({"v": 1, "id": format!("h{n}"), "type": "history"}))
            .await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let busy = waiting.load(Ordering::SeqCst);
    assert!(
        busy < super::MAX_IN_FLIGHT,
        "{busy} history calls wait at once, the turn among the cap's {}",
        super::MAX_IN_FLIGHT
    );

    go.notify_one();
    let frames = peer.reply(&format!("h{}", asked - 1)).await;
    let replies = frames.iter().filter(|f| ty(f) == "reply").count();
    assert_eq!(replies, asked + 1, "every request was served in the end");
    assert!(most.load(Ordering::SeqCst) <= super::MAX_IN_FLIGHT);
}

#[tokio::test]
async fn snapshot_and_history_say_where_the_conversation_is() {
    let (s, _) = session(&answers(&["hi there"]), hub());
    let mut peer = Peer::parent(s);
    peer.send(turn("t1", "hello")).await;
    peer.reply("t1").await;

    peer.send(json!({"v": 1, "id": "s", "type": "snapshot"}))
        .await;
    let now = peer.reply("s").await.pop().unwrap();
    assert_eq!(now["result"]["state"], "idle");
    assert_eq!(now["result"]["messages"], 2);
    assert_eq!(now["result"]["model"], "scripted:m");

    peer.send(json!({"v": 1, "id": "h", "type": "history"}))
        .await;
    let history = peer.reply("h").await.pop().unwrap();
    let messages = history["result"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(history["result"]["seq"], now["result"]["lastSeq"]);
}

#[tokio::test]
async fn a_change_replies_after_the_status_that_shows_it() {
    let (s, _) = session(&answers(&[]), hub());
    let mut peer = Peer::parent(s);

    peer.send(
        json!({"v": 1, "id": "m", "type": "control", "verb": "set_effort", "effort": "high"}),
    )
    .await;
    let frames = peer.reply("m").await;
    assert_eq!(frames.last().unwrap()["ok"], true);
    assert!(
        frames
            .iter()
            .any(|f| ty(f) == "status" && f["effort"] == "high"),
        "{frames:#?}"
    );

    peer.send(
        json!({"v": 1, "id": "b", "type": "control", "verb": "set_effort", "effort": "loud"}),
    )
    .await;
    let reply = peer.reply("b").await.pop().unwrap();
    assert_eq!(reply["ok"], false);
    // A blank model is refused here; no model at all is not a request.
    let blank = json!({"v": 1, "id": "n", "type": "control", "verb": "set_model", "model": " "});
    peer.send(blank).await;
    let error = |reply: &Value| reply["error"].as_str().unwrap_or_default().to_string();
    let reply = peer.reply("n").await.pop().unwrap();
    assert!(error(&reply).contains("needs a model"), "{reply}");
    peer.send(json!({"v": 1, "id": "o", "type": "control", "verb": "set_model"}))
        .await;
    let reply = peer.reply("o").await.pop().unwrap();
    assert!(error(&reply).contains("missing field `model`"), "{reply}");
}

#[tokio::test]
async fn close_replies_then_ends_the_connection_and_the_conversation() {
    let (backend, _) = session(&answers(&[]), hub());
    let mut peer = Peer::parent(backend.clone());

    peer.send(control("x", "close")).await;
    let frames = peer.reply("x").await;
    assert_eq!(frames.last().unwrap()["ok"], true);
    let (rest, ended) = peer.rest().await;
    assert!(rest.iter().all(|f| ty(f) != "reply"), "{rest:#?}");
    assert!(matches!(ended, Ended::Closed), "{ended:?}");
    assert!(backend.snapshot().await.is_err(), "the conversation ended");
}

#[tokio::test]
async fn when_the_input_ends_a_running_turn_still_finishes_and_replies() {
    let model = ScriptedProvider::new([
        tool_answer(&[("c1", "Wait", json!({}))]),
        text_answer("done"),
    ]);
    let (s, go) = session(&model, hub());
    let mut peer = Peer::parent(s);

    peer.send(turn("t1", "wait")).await;
    peer.until(|f| ty(f) == "assistant").await;
    peer.end_input().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    go.notify_one();

    let (rest, ended) = peer.rest().await;
    let reply = rest.last().unwrap();
    assert!(is_reply(reply, "t1"), "{rest:#?}");
    assert_eq!(reply["result"]["stopReason"], "end_turn");
    assert!(matches!(ended, Ended::PeerLeft), "{ended:?}");
}

#[tokio::test]
async fn an_approval_ask_is_answered_by_a_command_peer() {
    let hub = hub();
    let prompter = SessionPrompter::new(hub.clone());
    let (s, _) = session(&answers(&[]), hub);
    let mut peer = Peer::parent(s);

    let req = ApprovalRequest {
        tool_name: "Bash".into(),
        description: "delete the build dir".into(),
        input_preview: "rm -rf build".into(),
        tool_use_id: None,
        key: Some("rm -rf build".into()),
    };
    let asked = tokio::spawn(async move { prompter.prompt(&req).await });

    let ask = peer.until(|f| ty(f) == "approval_ask").await.pop().unwrap();
    assert_eq!(ask["toolName"], "Bash");
    assert_eq!(ask["preview"], "rm -rf build");
    let n = ask["ask"].clone();
    peer.send(json!({"v": 1, "id": "a1", "type": "approval_answer", "ask": n, "answer": "deny"}))
        .await;
    assert_eq!(peer.reply("a1").await.pop().unwrap()["ok"], true);
    assert_eq!(asked.await.unwrap(), ApprovalAnswer::Deny);

    // Answered already: a second answer goes nowhere.
    peer.send(json!({"v": 1, "id": "a2", "type": "approval_answer", "ask": n, "answer": "allow"}))
        .await;
    let late = peer.reply("a2").await.pop().unwrap();
    assert_eq!(late["ok"], false);
    assert!(
        late["error"].as_str().unwrap().contains("no approval ask"),
        "{late}"
    );
}

#[tokio::test]
async fn a_connection_that_follows_nothing_ends_when_the_conversation_does() {
    let (backend, _) = session(&answers(&[]), hub());
    let mut peer = Peer::attach(
        backend.clone(),
        Grant::watch(Origin::Agent),
        Start::default(),
    );
    peer.next().await.unwrap();

    backend.close().await;
    let (rest, ended) = tokio::time::timeout(Duration::from_secs(2), peer.rest())
        .await
        .expect("it ended at once");
    assert!(rest.is_empty(), "{rest:#?}");
    assert!(matches!(ended, Ended::Closed), "{ended:?}");
}

/// A peer's lines, then nothing more: it is still there, and sends no more.
struct Asks(std::vec::IntoIter<String>);

impl LineSource for Asks {
    async fn next_line(&mut self) -> Option<Result<String, crate::TransportError>> {
        match self.0.next() {
            Some(line) => Some(Ok(line)),
            None => std::future::pending().await,
        }
    }
}

/// Takes `room` lines, then never another: the peer stopped reading, and
/// its socket's buffer is full.
struct Stalls {
    room: usize,
}

impl LineSink for Stalls {
    async fn send_line(&mut self, _line: String) -> Result<(), crate::TransportError> {
        if self.room == 0 {
            std::future::pending::<()>().await;
        }
        self.room -= 1;
        Ok(())
    }
}

/// A busy conversation (more frames than the connection's out buffer) and a
/// peer that follows it, sends `asks`, and stops reading: once the
/// conversation ends, `serve` returns all the same, within its grace.
async fn a_stalled_peer_lets_go_when_the_conversation_ends(asks: Vec<String>, turn: bool) {
    let hub = Hub::new_with_options(
        "s1",
        HubOptions {
            ring: 5000,
            ..HubOptions::default()
        },
    );
    for n in 0..super::OUT_BUFFER * 3 {
        hub.notice(Level::Info, format!("line {n}"));
    }
    let (s, _) = session(&answers(&["done"]), hub);
    let start = Start {
        follow_from: Some(0),
        token: None,
    };
    let served = tokio::spawn(serve(
        s.clone(),
        Grant::command(Origin::Human),
        start,
        Asks(asks.into_iter()),
        Stalls { room: 8 },
    ));
    if turn {
        // The turn has run: its reply waits for frames the peer will not take.
        let ran = async {
            while s.snapshot().await.unwrap().messages < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), ran)
            .await
            .expect("the turn ran");
    } else {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    s.close().await;
    let ended = tokio::time::timeout(Duration::from_secs(10), served).await;
    let ended = ended.expect("serve returned after the conversation ended");
    assert!(matches!(ended.unwrap(), Ended::Closed));
}

/// A turn's reply waits for the turn's frames to go out first; a peer that
/// stopped reading never takes them.
#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_peer_with_a_turn_in_flight_lets_go_when_the_conversation_ends() {
    let asks = vec![turn("t1", "hi").to_string()];
    a_stalled_peer_lets_go_when_the_conversation_ends(asks, true).await;
}

/// A refusal goes into the out buffer, which a peer that stopped reading has
/// filled.
#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_peer_refused_again_and_again_lets_go_when_the_conversation_ends() {
    let asks = vec!["not a request".to_string(); super::OUT_BUFFER * 2];
    a_stalled_peer_lets_go_when_the_conversation_ends(asks, false).await;
}

/// A sink that fails from the start: its peer went away right after it
/// connected.
struct Gone;

impl LineSink for Gone {
    async fn send_line(&mut self, _line: String) -> Result<(), crate::TransportError> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into())
    }

    async fn keepalive(&mut self) -> Result<(), crate::TransportError> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into())
    }
}

/// A peer gone before its `hello` went out has left: the conversation goes
/// on, so a transport that dials again after a peer left (the broker link)
/// dials again, where one that read "the conversation ended" stopped.
#[tokio::test]
async fn a_peer_gone_before_its_hello_has_left_and_the_conversation_goes_on() {
    let (s, _) = session(&answers(&[]), hub());
    for n in 0..20 {
        let asks = Asks(Vec::new().into_iter());
        let grant = Grant::watch(Origin::Agent);
        let ended = serve(s.clone(), grant, Start::default(), asks, Gone).await;
        assert!(
            matches!(ended, Ended::PeerLeft),
            "connection {n}: {ended:?}"
        );
    }
    assert!(s.snapshot().await.is_ok(), "the conversation still runs");
    s.close().await;
}

mod keepalive;

#[test]
fn the_token_never_shows_in_debug_output() {
    let start = Start {
        follow_from: Some(7),
        token: Some("TOKEN-SECRET".into()),
    };
    let shown = format!("{start:?}");
    assert!(!shown.contains("SECRET"), "{shown}");
    assert!(shown.contains('7'), "{shown}");
}
