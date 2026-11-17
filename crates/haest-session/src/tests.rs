mod support;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::FutureExt as _;
use haest_core::{
    Agent, AgentDeps, AgentOptions, Effort, LoopEvent, Message, ModelError, ShutWorkspace,
    StopReason, ToolRef, Tools,
    testing::{ScriptedProvider, text_answer, tool_answer},
};
use haest_wire::{Level, Outbound, Path, State};
use serde_json::json;
use tokio::sync::{Notify, oneshot};

use self::support::*;
use crate::{Body, Control, Hub, HubOptions, Input, SessionError, start};

fn hub() -> Hub {
    Hub::new("s1")
}

/// The options a hub is made with hold for every clone: one stream, one
/// ring.
#[tokio::test]
async fn a_hubs_options_hold_for_every_clone() {
    let hub = Hub::new_with_options("s1", HubOptions { ring: 1, buffer: 1 });
    let early = hub.clone();
    for n in 0..3 {
        early.notice(Level::Info, format!("line {n}"));
    }
    let mut late = hub.subscribe(0);
    let first = late.recv().await.map(|f| f.body.clone());
    assert!(
        matches!(first, Some(Body::Wire(Outbound::Gap { after: 0, .. }))),
        "the ring kept one frame: {first:?}"
    );
    assert_eq!(late.recv().await.map(|f| f.seq), Some(3));
}

/// A host's own frame, whose conversion into a [`Body`] panics.
struct Unwritable;

impl From<Unwritable> for Body {
    fn from(_: Unwritable) -> Self {
        panic!("the host's frame cannot be written")
    }
}

/// `publish` ran the host's `From<_> for Body` under the hub's lock: one
/// that panicked poisoned it, and every publish after it panicked too,
/// haest's own included.
#[test]
fn a_host_conversion_that_panics_leaves_the_hub_working() {
    let hub = hub();
    let publish = std::panic::catch_unwind(|| hub.publish(Path::main(), Unwritable));
    assert!(publish.is_err(), "the host's conversion panicked");
    let after = std::panic::catch_unwind(|| hub.notice(Level::Info, "after"));
    assert_eq!(after.ok(), Some(1), "the next frame is the first");
    assert_eq!(hub.last_seq(), 1);
}

/// A model that calls `Wait` once, then answers each of `then`.
fn waits_then(then: &[&str]) -> ScriptedProvider {
    let first = tool_answer(&[("c1", "Wait", json!({}))]);
    ScriptedProvider::new(std::iter::once(first).chain(then.iter().map(|t| text_answer(t))))
}

#[tokio::test]
async fn input_mid_turn_lands_at_the_next_step() {
    let go = Arc::new(Notify::new());
    let model = waits_then(&["done, and noted"]);
    let session = start(
        agent(&model, Tools::from_iter([ToolRef::new(Wait(go.clone()))])),
        hub(),
    );

    let first = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("wait, then answer")).await }
    });
    eventually(|| model.requests().len() == 1).await;
    let second = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("also note this")).await }
    });
    // The input reaches the driver before the tool is let go.
    let mut sub = session.subscribe(session.hub().last_seq());
    tokio::time::sleep(Duration::from_millis(20)).await;
    go.notify_one();

    let (a, b) = (
        first.await.unwrap().unwrap(),
        second.await.unwrap().unwrap(),
    );
    assert_eq!(a.stop_reason, StopReason::EndTurn);
    assert_eq!(
        b.stop_reason,
        StopReason::EndTurn,
        "the steered input shares the turn's end"
    );
    assert_eq!(model.requests().len(), 2, "one turn, two model calls");
    let history = session.history().await.unwrap().messages;
    assert!(matches!(&history[3], Message::User(u) if u.content == "also note this"));
    // It entered as a user message after the tool's result, in the same turn.
    let frames = until(&mut sub, turn_end).await;
    let users = (frames.iter())
        .filter(|f| matches!(f.body, Body::Loop(LoopEvent::User { .. })))
        .count();
    assert_eq!(users, 1);
    session.close().await;
}

#[tokio::test]
async fn input_while_held_runs_next_as_its_own_turn_through_the_gate() {
    let model = answers(&["for the peer", "for the person"]);
    let recorded = Recorded::new(agent(&model, Tools::new()), 1);
    let log = recorded.log.clone();
    let session = start(recorded, hub());

    let hold = session.hold().await.unwrap();
    let peer = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::agent("from a peer")).await }
    });
    let person = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("from the person")).await }
    });
    eventually(|| session.hub().last_seq() >= 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(model.requests().is_empty(), "no turn starts under a hold");
    assert_eq!(session.snapshot().await.unwrap().queued, 2);
    drop(hold);

    assert_eq!(
        peer.await.unwrap().unwrap().stop_reason,
        StopReason::EndTurn
    );
    assert_eq!(
        person.await.unwrap().unwrap().stop_reason,
        StopReason::EndTurn
    );
    let log = log.lock().unwrap().clone();
    assert_eq!(
        log,
        vec![
            Asked::Gate("from a peer".into()),
            Asked::Run("from a peer".into()),
            Asked::Human("from the person".into()),
            Asked::Run("from the person".into()),
        ]
    );
    session.close().await;
}

#[tokio::test]
async fn input_the_gate_holds_is_refused_and_a_persons_still_runs() {
    let model = answers(&["for the person"]);
    let session = start(Recorded::new(agent(&model, Tools::new()), 0), hub());

    let hold = session.hold().await.unwrap();
    let peer = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::agent("from a peer")).await }
    });
    let person = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("from the person")).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    drop(hold);

    assert!(matches!(peer.await.unwrap(), Err(SessionError::Held)));
    assert_eq!(
        person.await.unwrap().unwrap().stop_reason,
        StopReason::EndTurn
    );
    assert_eq!(model.requests().len(), 1);
    session.close().await;
}

#[tokio::test]
async fn an_unread_human_steer_runs_next_as_a_human_turn() {
    let stall = Stall::new(answers(&["second answered"]));
    let recorded = Recorded::new(stalling_agent(&stall), 0);
    let log = recorded.log.clone();
    let session = start(recorded, hub());

    let first = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("first")).await }
    });
    eventually(|| stall.calls() == 1).await;
    let second = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("second")).await }
    });
    // The second reaches the driver while the model call stalls, then the
    // turn is stopped mid-call: the steering is never read.
    tokio::time::sleep(Duration::from_millis(20)).await;
    session.control(Control::Interrupt).await.unwrap();

    assert_eq!(
        first.await.unwrap().unwrap().stop_reason,
        StopReason::Aborted
    );
    let b = second.await.unwrap().unwrap();
    assert_eq!(
        b.stop_reason,
        StopReason::EndTurn,
        "it got a turn of its own"
    );
    let log = log.lock().unwrap().clone();
    assert!(log.contains(&Asked::Human("second".into())), "{log:?}");
    assert!(
        !log.iter().any(|a| matches!(a, Asked::Gate(_))),
        "a person's input skips the gate: {log:?}"
    );
    assert_eq!(log.iter().filter(|a| matches!(a, Asked::Run(_))).count(), 2);
    session.close().await;
}

/// The rewriter runs on a person's input once: the input that starts a
/// turn, input steered in, and never again when a steer left unread runs
/// next. Another program's input is not a person's: never rewritten.
#[tokio::test]
async fn a_persons_input_goes_through_the_rewriter_once_steered_or_not() {
    let go = Arc::new(Notify::new());
    let model = waits_then(&["noted"]);
    let wait = Tools::from_iter([ToolRef::new(Wait(go.clone()))]);
    let mut recorded = Recorded::new(agent(&model, wait), 0);
    recorded.rewriter = Some(Marks.into());
    let session = start(recorded, hub());

    let first = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("first")).await }
    });
    eventually(|| model.requests().len() == 1).await;
    let second = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("second")).await }
    });
    let peer = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::agent("from a peer")).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    go.notify_one();
    for turn in [first, second, peer] {
        let _ = turn.await.unwrap().unwrap();
    }

    let history = session.history().await.unwrap().messages;
    let said: Vec<&str> = (history.iter())
        .filter_map(|m| match m {
            Message::User(u) => Some(u.content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(said, ["[R] first", "[R] second\n\nfrom a peer"]);
    session.close().await;
}

#[tokio::test]
async fn an_unread_steer_is_not_rewritten_again_when_it_runs_next() {
    let stall = Stall::new(answers(&["second answered"]));
    let mut recorded = Recorded::new(stalling_agent(&stall), 0);
    recorded.rewriter = Some(Marks.into());
    let log = recorded.log.clone();
    let session = start(recorded, hub());

    let first = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("first")).await }
    });
    eventually(|| stall.calls() == 1).await;
    let second = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("second")).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    session.control(Control::Interrupt).await.unwrap();
    let _ = first.await.unwrap().unwrap();
    let _ = second.await.unwrap().unwrap();

    let humans: Vec<Asked> = (log.lock().unwrap().iter())
        .filter(|a| matches!(a, Asked::Human(_)))
        .cloned()
        .collect();
    assert_eq!(
        humans,
        [
            Asked::Human("[R] first".into()),
            Asked::Human("[R] second".into())
        ]
    );
    session.close().await;
}

#[tokio::test]
async fn an_interrupt_stops_the_running_tool_and_the_turn() {
    let go = Arc::new(Notify::new());
    let model = waits_then(&["never sent"]);
    let session = start(
        agent(&model, Tools::from_iter([ToolRef::new(Wait(go))])),
        hub(),
    );

    let turn = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("wait")).await }
    });
    eventually(|| model.requests().len() == 1).await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    session.control(Control::Interrupt).await.unwrap();

    let end = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .unwrap();
    assert_eq!(end.unwrap().unwrap().stop_reason, StopReason::Aborted);
    assert_eq!(model.requests().len(), 1, "no model call after the stop");
    session.close().await;
}

#[tokio::test]
async fn a_lagging_subscriber_gets_one_gap_and_the_turn_never_waits() {
    let model = answers(&["one", "two", "three"]);
    let opts = HubOptions {
        ring: 1000,
        buffer: 2,
    };
    let session = start(
        agent(&model, Tools::new()),
        Hub::new_with_options("s1", opts),
    );
    let mut slow = session.subscribe(0);

    // Three turns while it reads nothing: none waits for it.
    for text in ["a", "b", "c"] {
        let turn = session.turn(Input::human(text));
        let _ = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap();
    }
    session.idle().await;
    let last = session.hub().last_seq();

    let first = slow.recv().await.unwrap();
    let Body::Wire(Outbound::Gap { after, missed }) = first.body else {
        panic!("the first frame it reads is the gap: {first:?}")
    };
    assert_eq!(after, 0);
    assert!(missed.unwrap() > 0);
    let mut rest = Vec::new();
    while rest.last() != Some(&last) {
        let f = tokio::time::timeout(Duration::from_secs(1), slow.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !matches!(f.body, Body::Wire(Outbound::Gap { .. })),
            "one gap only"
        );
        rest.push(f.seq);
    }
    assert!(
        rest.windows(2).all(|w| w[0] < w[1]),
        "in order after the gap: {rest:?}"
    );
    session.close().await;
}

#[tokio::test]
async fn two_subscribers_see_the_same_frames_in_the_same_order() {
    let model = answers(&["shared"]);
    let session = start(agent(&model, Tools::new()), hub());
    let mut a = session.subscribe(0);
    let mut b = session.subscribe(0);

    let _ = session.turn(Input::human("hello")).await.unwrap();

    let a = until(&mut a, turn_end).await;
    let b = until(&mut b, turn_end).await;
    let json = |fs: &[Arc<crate::Frame>]| {
        fs.iter()
            .map(|f| serde_json::to_string(&**f).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(json(&a), json(&b));
    let seqs: Vec<u64> = a.iter().map(|f| f.seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    session.close().await;
}

#[tokio::test]
async fn a_late_subscriber_catches_up_on_the_running_turn() {
    let go = Arc::new(Notify::new());
    let model = waits_then(&["after the wait"]);
    let opts = HubOptions {
        ring: 2,
        buffer: 64,
    };
    let session = start(
        agent(&model, Tools::from_iter([ToolRef::new(Wait(go.clone()))])),
        Hub::new_with_options("s1", opts),
    );
    session.hub().notice(Level::Info, "before the turn");

    let turn = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("wait")).await }
    });
    eventually(|| model.requests().len() == 1).await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    // The ring holds two frames besides the running turn: the turn's start
    // is still there, the notice before it may not be.
    let mut late = session.subscribe(0);
    go.notify_one();
    let _ = turn.await.unwrap().unwrap();
    let frames = until(&mut late, turn_end).await;
    let bodies: Vec<_> = frames.iter().map(|f| &f.body).collect();
    assert!(
        bodies
            .iter()
            .any(|b| matches!(b, Body::Loop(LoopEvent::User { .. }))),
        "{bodies:?}"
    );
    assert!(
        bodies
            .iter()
            .any(|b| matches!(b, Body::Wire(Outbound::Status(s)) if s.state == State::Running)),
        "{bodies:?}"
    );
    session.close().await;
}

#[tokio::test]
async fn a_change_waits_for_the_running_turn() {
    let go = Arc::new(Notify::new());
    let model = waits_then(&["done"]);
    let session = start(
        agent(&model, Tools::from_iter([ToolRef::new(Wait(go.clone()))])),
        hub(),
    );

    let turn = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("wait")).await }
    });
    eventually(|| model.requests().len() == 1).await;
    let change = tokio::spawn({
        let s = session.clone();
        async move {
            s.control(Control::SetModelSpec("scripted:other".into()))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let now = session.snapshot().await.unwrap();
    assert_eq!(
        (now.state, now.model_spec.as_str()),
        (State::Running, "scripted:m")
    );
    assert!(!change.is_finished());

    go.notify_one();
    let _ = turn.await.unwrap().unwrap();
    change.await.unwrap().unwrap();
    assert_eq!(
        session.snapshot().await.unwrap().model_spec,
        "scripted:other"
    );
    assert_eq!(
        model.requests()[1].model,
        "m",
        "the running turn kept its model"
    );
    session.close().await;
}

#[tokio::test]
async fn with_runs_between_turns_and_hands_back_what_it_gives() {
    let go = Arc::new(Notify::new());
    let model = waits_then(&["done"]);
    let session = start(
        agent(&model, Tools::from_iter([ToolRef::new(Wait(go.clone()))])),
        hub(),
    );

    let turn = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("wait")).await }
    });
    eventually(|| model.requests().len() == 1).await;
    let seen = tokio::spawn({
        let s = session.clone();
        async move { s.with(|a| Box::pin(async move { a.history().len() })).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!seen.is_finished(), "it waits for the running turn");
    go.notify_one();
    let _ = turn.await.unwrap().unwrap();
    assert_eq!(seen.await.unwrap().unwrap(), 4, "the whole turn was in");
    session.close().await;
}

/// A job that waits for good, as a mod's slash command whose promise never
/// settles does; `started` is told once it runs.
fn stuck_job<D>(
    started: oneshot::Sender<()>,
) -> impl for<'a> FnOnce(&'a mut D) -> crate::BoxFuture<'a, ()> + Send + 'static {
    move |_| {
        Box::pin(async move {
            let _ = started.send(());
            std::future::pending::<()>().await;
        })
    }
}

/// A job ran on the driver to its end whether or not anyone still waited
/// for it: one that never ended held the session for good after its caller
/// gave up on it (Ctrl-C, a timeout), and no turn ran again.
#[tokio::test]
async fn a_job_whose_caller_gave_up_stops_and_the_next_turn_runs() {
    let model = answers(&["one"]);
    let session = start(agent(&model, Tools::new()), hub());
    let (started, running) = oneshot::channel();
    tokio::select! {
        job = session.with(stuck_job(started)) => panic!("the job never ends: {job:?}"),
        // The host gives up once it runs.
        _ = running => {}
    }
    let turn = tokio::time::timeout(Duration::from_secs(5), session.turn(Input::human("a"))).await;
    assert!(matches!(turn, Ok(Ok(_))), "the next turn runs: {turn:?}");
    session.close().await;
}

/// The same before the job started (a turn ran): it never starts.
#[tokio::test]
async fn a_job_whose_caller_gave_up_before_it_started_never_runs() {
    let go = Arc::new(Notify::new());
    let model = waits_then(&["done"]);
    let session = start(
        agent(&model, Tools::from_iter([ToolRef::new(Wait(go.clone()))])),
        hub(),
    );
    let turn = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("wait")).await }
    });
    eventually(|| model.requests().len() == 1).await;
    let ran = Arc::new(AtomicBool::new(false));
    let job = session.with({
        let ran = ran.clone();
        move |_| Box::pin(async move { ran.store(true, Ordering::SeqCst) })
    });
    assert!(job.now_or_never().is_none(), "sent, then given up on");
    go.notify_one();
    let _ = turn.await.unwrap().unwrap();

    // Asked after the job, so answered after the driver took it.
    let _ = session.history().await.unwrap();
    assert!(!ran.load(Ordering::SeqCst), "nobody waits for it");
    session.close().await;
}

/// `close` waited for a job that never ended, so it never returned and the
/// conversation was never closed.
#[tokio::test]
async fn close_stops_a_job_that_never_ends() {
    let model = answers(&[]);
    let recorded = Recorded::new(agent(&model, Tools::new()), 0);
    let log = recorded.log.clone();
    let session = start(recorded, hub());
    let (started, running) = oneshot::channel();
    let job = tokio::spawn({
        let s = session.clone();
        async move { s.with(stuck_job(started)).await }
    });
    running.await.unwrap();

    tokio::time::timeout(Duration::from_secs(5), session.close())
        .await
        .expect("close returns");
    assert_eq!(log.lock().unwrap().last(), Some(&Asked::Close));
    assert!(matches!(job.await.unwrap(), Err(SessionError::Closed)));
}

/// A job that waits behind the running turn when a close comes never
/// starts: the close comes first.
#[tokio::test]
async fn close_during_a_turn_drops_the_job_that_waits_behind_it() {
    let go = Arc::new(Notify::new());
    let model = waits_then(&["never sent"]);
    let session = start(
        agent(&model, Tools::from_iter([ToolRef::new(Wait(go))])),
        hub(),
    );
    let turn = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("wait")).await }
    });
    eventually(|| model.requests().len() == 1).await;
    let ran = Arc::new(AtomicBool::new(false));
    let mut job = Box::pin(session.with({
        let ran = ran.clone();
        move |_| {
            Box::pin(async move {
                ran.store(true, Ordering::SeqCst);
                std::future::pending::<()>().await;
            })
        }
    }));
    assert!((&mut job).now_or_never().is_none(), "sent");
    // Asked after the job and answered during the turn: the driver holds
    // the job until the turn ends.
    assert_eq!(session.snapshot().await.unwrap().state, State::Running);

    tokio::time::timeout(Duration::from_secs(5), session.close())
        .await
        .expect("close returns");
    assert!(!ran.load(Ordering::SeqCst), "the job never started");
    assert!(matches!(job.await, Err(SessionError::Closed)));
    assert!(turn.await.unwrap().is_ok());
}

/// What is asked while a job runs waits for its end, in order.
#[tokio::test]
async fn input_sent_while_a_job_runs_waits_for_it() {
    let model = answers(&["after the job"]);
    let session = start(agent(&model, Tools::new()), hub());
    let (started, running) = oneshot::channel();
    let (release, gate) = oneshot::channel::<()>();
    let mut job = Box::pin(session.with(move |a| {
        Box::pin(async move {
            let _ = started.send(());
            let _ = gate.await;
            a.history().len()
        })
    }));
    assert!((&mut job).now_or_never().is_none(), "sent");
    running.await.unwrap();
    let turn = session.send_input(Input::human("a")).unwrap();
    let _ = release.send(());

    assert_eq!(job.await.unwrap(), 0, "the turn waited for the job");
    assert_eq!(turn.await.unwrap().stop_reason, StopReason::EndTurn);
    session.close().await;
}

#[tokio::test]
async fn input_sent_under_a_hold_waits_and_runs_as_a_turn_of_its_own() {
    let model = answers(&["one", "two"]);
    let session = start(agent(&model, Tools::new()), hub());

    let hold = session.hold().await.unwrap();
    let first = session.send_input(Input::human("first")).unwrap();
    let second = session.send_input(Input::human("second")).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(model.requests().is_empty());
    drop(hold);

    let _ = first.await.unwrap();
    let _ = second.await.unwrap();
    assert_eq!(model.requests().len(), 2, "two turns, one input each");
    let users = (session.history().await.unwrap().messages.iter())
        .filter(|m| matches!(m, Message::User(_)))
        .count();
    assert_eq!(users, 2);
    session.close().await;
}

/// A host that gives up on `hold()` before it resolves: a `select!` with
/// Ctrl-C, a timeout, an aborted task.
fn give_up_on_a_hold(session: &crate::SessionHandle<impl crate::Driven>) {
    assert!(
        session.hold().now_or_never().is_none(),
        "the hold was not granted on its first poll"
    );
}

/// `hold()` built its `Hold`, whose drop releases it, only once the hold was
/// granted, but the driver counted the hold whether or not anyone still
/// waited for it: a `hold()` dropped in between held the session for good,
/// and no turn ran again.
#[tokio::test]
async fn a_hold_dropped_before_it_was_granted_holds_nothing() {
    let model = answers(&["one"]);
    let session = start(agent(&model, Tools::new()), hub());
    give_up_on_a_hold(&session);
    let turn = tokio::time::timeout(Duration::from_secs(5), session.turn(Input::human("a"))).await;
    assert!(matches!(turn, Ok(Ok(_))), "the next turn runs: {turn:?}");
    session.close().await;
}

/// The same while a turn runs: the driver puts the hold off until that
/// turn ends, and the release it gets from the dropped `hold()` after it.
#[tokio::test]
async fn a_hold_dropped_while_a_turn_runs_holds_nothing_after_it() {
    let go = Arc::new(Notify::new());
    let model = waits_then(&["first", "second"]);
    let session = start(
        agent(&model, Tools::from_iter([ToolRef::new(Wait(go.clone()))])),
        hub(),
    );
    let first = tokio::spawn({
        let s = session.clone();
        async move { s.turn(Input::human("wait, then answer")).await }
    });
    eventually(|| model.requests().len() == 1).await;
    give_up_on_a_hold(&session);
    // Asked after the hold and its release, and answered while the turn
    // runs: both reached the driver during the turn and wait for its end.
    assert_eq!(session.snapshot().await.unwrap().state, State::Running);
    go.notify_one();
    assert!(first.await.unwrap().is_ok());

    let next = session.turn(Input::human("again"));
    let next = tokio::time::timeout(Duration::from_secs(5), next).await;
    assert!(matches!(next, Ok(Ok(_))), "the next turn runs: {next:?}");
    assert_eq!(model.requests().len(), 3);
    session.close().await;
}

#[tokio::test]
async fn close_closes_the_conversation_and_ends_every_stream() {
    let model = answers(&["bye"]);
    let recorded = Recorded::new(agent(&model, Tools::new()), 0);
    let log = recorded.log.clone();
    let session = start(recorded, hub());
    let mut sub = session.subscribe(0);
    let _ = session.turn(Input::human("hi")).await.unwrap();

    session.close().await;
    assert_eq!(log.lock().unwrap().last(), Some(&Asked::Close));
    assert!(matches!(
        session.turn(Input::human("again")).await,
        Err(SessionError::Closed)
    ));
    let ended = async { while sub.recv().await.is_some() {} };
    tokio::time::timeout(Duration::from_secs(5), ended)
        .await
        .expect("the stream ends");
}

#[tokio::test]
async fn dropping_every_handle_closes_the_conversation() {
    let model = answers(&[]);
    let recorded = Recorded::new(agent(&model, Tools::new()), 0);
    let log = recorded.log.clone();
    let session = start(recorded, hub());
    drop(session);
    eventually(|| log.lock().unwrap().last() == Some(&Asked::Close)).await;
}

/// The stream `sub` reads ends within 5 s.
async fn stream_ends(sub: &mut crate::Subscription) {
    let ended = async { while sub.recv().await.is_some() {} };
    tokio::time::timeout(Duration::from_secs(5), ended)
        .await
        .expect("the stream ends");
}

/// A host's job that panics on the driver's task (a slash command with a
/// bug) killed the driver with nothing cleaned up: the stream never ended,
/// so every subscriber and transport waited on a dead session, and the
/// conversation was never closed (nothing it started stopped, nothing saved).
#[tokio::test]
async fn a_job_that_panics_ends_the_stream_and_closes_the_conversation() {
    let model = answers(&["one"]);
    let recorded = Recorded::new(agent(&model, Tools::new()), 0);
    let log = recorded.log.clone();
    let session = start(recorded, hub());
    let mut sub = session.subscribe(0);
    let _ = session.turn(Input::human("a")).await.unwrap();

    let job = session.with(|_| Box::pin(async { panic!("a slash command with a bug") }));
    let job = tokio::time::timeout(Duration::from_secs(5), job).await;
    assert!(matches!(job, Ok(Err(SessionError::Closed))), "{job:?}");
    stream_ends(&mut sub).await;
    assert_eq!(log.lock().unwrap().last(), Some(&Asked::Close));
    assert!(matches!(
        session.turn(Input::human("b")).await,
        Err(SessionError::Closed)
    ));
}

/// The same for code that panics inside a turn (the rewriter here; the
/// conversation's own `run` alike): the turn's caller reads Closed, the
/// stream ends, the conversation is closed.
#[tokio::test]
async fn a_turn_that_panics_ends_the_stream_and_closes_the_conversation() {
    let model = answers(&["never sent"]);
    let mut recorded = Recorded::new(agent(&model, Tools::new()), 0);
    recorded.rewriter = Some(Panics.into());
    let log = recorded.log.clone();
    let session = start(recorded, hub());
    let mut sub = session.subscribe(0);

    let turn = tokio::time::timeout(Duration::from_secs(5), session.turn(Input::human("a"))).await;
    assert!(matches!(turn, Ok(Err(SessionError::Closed))), "{turn:?}");
    stream_ends(&mut sub).await;
    assert_eq!(log.lock().unwrap().last(), Some(&Asked::Close));
    assert!(model.requests().is_empty());
}

/// `Driven::close` itself panics: `close()` returns and the stream ends all
/// the same.
#[tokio::test]
async fn a_close_that_panics_still_ends_the_stream() {
    let model = answers(&[]);
    let mut recorded = Recorded::new(agent(&model, Tools::new()), 0);
    recorded.close_panics = true;
    let session = start(recorded, hub());
    let mut sub = session.subscribe(0);

    tokio::time::timeout(Duration::from_secs(5), session.close())
        .await
        .expect("close returns");
    stream_ends(&mut sub).await;
}

#[tokio::test]
async fn a_failed_change_changes_nothing_and_says_why() {
    // A resolver that knows one provider only.
    let model = answers(&[]);
    let known = model.resolver();
    let resolve = move |spec: &str| match spec.split_once(':') {
        Some(("scripted", _)) => known(spec),
        _ => Err(ModelError::BadSpec { spec: spec.into() }),
    };
    let deps = AgentDeps::new(
        haest_core::SharedDeps::new(resolve, "/", ShutWorkspace),
        Tools::new(),
        "",
    );
    let session = start(Agent::new(deps, AgentOptions::new("scripted:m")), hub());
    let err = session
        .control(Control::SetModelSpec("nowhere:m".into()))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            SessionError::Control {
                what: "set the model",
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(session.snapshot().await.unwrap().model_spec, "scripted:m");
    session.close().await;
}

#[tokio::test]
async fn status_frames_say_when_a_turn_runs_and_what_changed() {
    let model = answers(&["ok"]);
    let session = start(agent(&model, Tools::new()), hub());
    let mut sub = session.subscribe(0);
    let _ = session.turn(Input::human("go")).await.unwrap();
    session
        .control(Control::SetEffort(Some(Effort::High)))
        .await
        .unwrap();

    let frames = until(&mut sub, |f| {
        matches!(&f.body, Body::Wire(Outbound::Status(s)) if s.effort.as_deref() == Some("high"))
    })
    .await;
    let states: Vec<State> = (frames.iter())
        .filter_map(|f| match &f.body {
            Body::Wire(Outbound::Status(s)) => Some(s.state),
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        vec![State::Idle, State::Running, State::Idle, State::Idle]
    );
    assert!(frames.iter().all(|f| f.path == Path::main()));
    session.close().await;
}

#[cfg(feature = "approvals")]
/// A change that fails says what failed; the chain says why, once.
#[test]
fn a_failed_change_says_its_cause_once() {
    let source = Box::new(std::io::Error::other("no such provider"));
    let e = SessionError::Control {
        what: "set the model",
        source,
    };
    haest_core::testing::assert_causes_said_once(&e);
}

#[cfg(feature = "approvals")]
mod approvals {
    use haest_mods::{ApprovalAnswer, ApprovalRequest, Prompter};
    use haest_wire::Answer;

    use super::*;
    use crate::SessionPrompter;

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            tool_name: "Bash".into(),
            description: "remove the build dir".into(),
            input_preview: "rm -rf build".into(),
            tool_use_id: None,
            key: Some("rm -rf build".into()),
        }
    }

    async fn asked(sub: &mut crate::Subscription) -> u64 {
        let frames = until(sub, |f| {
            matches!(f.body, Body::Wire(Outbound::ApprovalAsk { .. }))
        })
        .await;
        match &frames.last().unwrap().body {
            Body::Wire(Outbound::ApprovalAsk { ask, .. }) => *ask,
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn each_answer_maps_to_the_gates_answer() {
        let hub = hub();
        let prompter = Arc::new(SessionPrompter::new(hub.clone()));
        let mut sub = hub.subscribe(0);
        for (answer, want) in [
            (Answer::Allow, ApprovalAnswer::Allow),
            (Answer::Always, ApprovalAnswer::AllowAlways),
            (Answer::Deny, ApprovalAnswer::Deny),
        ] {
            let p = prompter.clone();
            let prompt = tokio::spawn(async move { p.prompt(&request()).await });
            let ask = asked(&mut sub).await;
            hub.answer(ask, answer).unwrap();
            assert_eq!(prompt.await.unwrap(), want);
        }
    }

    #[tokio::test]
    async fn an_ask_given_up_on_is_withdrawn_and_a_late_answer_refused() {
        let hub = hub();
        let prompter = SessionPrompter::new(hub.clone());
        let mut sub = hub.subscribe(0);
        let prompt = tokio::spawn(async move { prompter.prompt(&request()).await });
        let ask = asked(&mut sub).await;

        prompt.abort();
        let frames = until(&mut sub, |f| {
            matches!(f.body, Body::Wire(Outbound::ApprovalWithdrawn { .. }))
        })
        .await;
        assert!(
            matches!(frames.last().unwrap().body, Body::Wire(Outbound::ApprovalWithdrawn { ask: a }) if a == ask)
        );
        assert!(matches!(
            hub.answer(ask, Answer::Allow),
            Err(SessionError::NoSuchAsk { .. })
        ));
    }

    #[tokio::test]
    async fn an_answered_ask_is_not_withdrawn() {
        let hub = hub();
        let prompter = SessionPrompter::new(hub.clone());
        let mut sub = hub.subscribe(0);
        let prompt = tokio::spawn(async move { prompter.prompt(&request()).await });
        let ask = asked(&mut sub).await;
        hub.answer(ask, Answer::Allow).unwrap();
        prompt.await.unwrap();
        hub.notice(Level::Info, "after");
        let frames = until(&mut sub, |f| {
            matches!(f.body, Body::Wire(Outbound::Notice { .. }))
        })
        .await;
        assert!(
            !frames
                .iter()
                .any(|f| matches!(f.body, Body::Wire(Outbound::ApprovalWithdrawn { .. })))
        );
        assert!(matches!(
            hub.answer(ask, Answer::Allow),
            Err(SessionError::NoSuchAsk { .. })
        ));
    }

    #[tokio::test]
    async fn closing_the_hub_denies_an_open_ask() {
        let hub = hub();
        let prompter = SessionPrompter::new(hub.clone());
        let mut sub = hub.subscribe(0);
        let prompt = tokio::spawn(async move { prompter.prompt(&request()).await });
        asked(&mut sub).await;
        hub.close();
        assert_eq!(prompt.await.unwrap(), ApprovalAnswer::Deny);
    }

    #[tokio::test]
    async fn an_ask_after_the_hub_closed_is_denied_at_once() {
        let hub = hub();
        let prompter = SessionPrompter::new(hub.clone());
        hub.close();
        // Nobody can answer it: denied now, not after the gate's timeout.
        let req = request();
        let prompt = prompter.prompt(&req);
        let answer = tokio::time::timeout(std::time::Duration::from_secs(2), prompt).await;
        assert_eq!(answer.ok(), Some(ApprovalAnswer::Deny));
    }
}
