//! The driver's idle ticks, on paused time: an agent's clock that follows
//! tokio's, so the ticks it asks for come when the test sleeps past them.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::stream;
use haest_core::{
    Agent, AgentDeps, AgentHooks, AgentOptions, AssistantMessage, Message, ModelError,
    ModelResolverRef, Provider, ProviderEvent, ProviderRequest, ResolvedModel, SharedDeps,
    ShutWorkspace, StopReason, Tools, Usage,
    cache::{
        CachePlan, CachePolicy, CachePolicyRef, IdleAction, IdleState, PrewarmRefused, RequestShape,
    },
    catalog::{ModelFacts, ModelPricing},
    compaction::CompactionOptions,
    deps::Clock,
    provider::ProviderStream,
    testing::{ScriptedProvider, text_answer},
};
use haest_settings::CacheSettings;
use haest_wire::Outbound;

use super::support::{until, within};
use crate::{Body, Hub, Input, SessionHandle, start};

/// tokio's clock, from a start in the past: paused time moves it.
struct TokioClock(tokio::time::Instant);

impl Clock for TokioClock {
    fn now_ms(&self) -> u64 {
        let since = u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX);
        1_000_000 + since
    }
}

/// What the human was warned of.
#[derive(Default)]
struct Warned(Mutex<Vec<String>>);

impl AgentHooks for Warned {
    fn on_warning(&self, text: &str) {
        self.0.lock().unwrap().push(text.to_string());
    }
}

impl Warned {
    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

/// $3 in, $0.30 a read, $15 out, $3.75 a 5m write, for every model.
struct Priced;

impl ModelFacts for Priced {
    fn pricing(&self, _spec: &str) -> Option<ModelPricing> {
        Some(ModelPricing::new(3.0, 0.3, 15.0).with_cache_write(3.75))
    }
}

/// A policy that answers the same at every tick.
struct Always(IdleAction);

impl CachePolicy for Always {
    fn plan(&self, _request: &RequestShape<'_>) -> CachePlan {
        CachePlan::default()
    }
    fn on_idle(&self, _idle: &IdleState<'_>) -> IdleAction {
        self.0.clone()
    }
}

/// A model whose pre-warms wait until they are stopped; the rest of its
/// calls are answered from the script.
struct StallsPings {
    then: ScriptedProvider,
    pings: AtomicUsize,
    stopped: Arc<AtomicBool>,
}

impl Provider for StallsPings {
    fn id(&self) -> &str {
        "scripted"
    }
    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        if !req.prewarm {
            return self.then.stream(req);
        }
        self.pings.fetch_add(1, Ordering::SeqCst);
        let stopped = self.stopped.clone();
        Box::pin(stream::once(async move {
            req.cancel.cancelled().await;
            stopped.store(true, Ordering::SeqCst);
            let mut m = AssistantMessage::start("scripted", &req.model);
            m.stop_reason = StopReason::Aborted;
            ProviderEvent::Done(m)
        }))
    }
    fn check_prewarm(&self, _req: &ProviderRequest) -> Result<(), PrewarmRefused> {
        Ok(())
    }
}

fn stalls_pings(then: ScriptedProvider) -> Arc<StallsPings> {
    Arc::new(StallsPings {
        then,
        pings: AtomicUsize::new(0),
        stopped: Arc::default(),
    })
}

fn resolves_to(p: &Arc<StallsPings>) -> ModelResolverRef {
    let p = p.clone();
    let resolve = move |_spec: &str| -> Result<ResolvedModel, ModelError> {
        Ok(ResolvedModel::new(p.clone(), "m"))
    };
    resolve.into()
}

/// An agent on `models` and `policy`, priced, on tokio's clock.
fn cached(
    models: impl Into<ModelResolverRef>,
    policy: impl Into<CachePolicyRef>,
    history: Vec<Message>,
) -> (Agent, Arc<Warned>) {
    let warned = Arc::new(Warned::default());
    let shared = SharedDeps::new(models, "/", ShutWorkspace)
        .with_clock(TokioClock(tokio::time::Instant::now()))
        .with_hooks(warned.clone())
        .with_facts(Priced);
    let opts = (AgentOptions::new("scripted:m"))
        .with_cache_policy(policy)
        .with_history(history);
    (
        Agent::new(AgentDeps::new(shared, Tools::new(), "sys"), opts),
        warned,
    )
}

/// A pre-warm's reply: what it read and wrote.
fn ping(read: u64, write: u64) -> AssistantMessage {
    let mut m = AssistantMessage::start("scripted", "m");
    m.usage.cache_read = read;
    m.usage.cache_write = write;
    m.stop_reason = StopReason::MaxTokens;
    m
}

fn prewarms(p: &ScriptedProvider) -> Vec<bool> {
    p.requests().iter().map(|r| r.prewarm).collect()
}

async fn sleep_s(s: u64) {
    tokio::time::sleep(Duration::from_secs(s)).await;
}

/// A person's turn on `text`, which ends answered.
async fn says(session: &SessionHandle<Agent>, text: &str) {
    let end = within(session.turn(Input::human(text))).await.unwrap();
    assert_eq!((end.stop_reason, end.error), (StopReason::EndTurn, None));
}

#[tokio::test(start_paused = true)]
async fn an_idle_session_pings_its_cache_until_a_ping_writes() {
    let p = ScriptedProvider::new([text_answer("hi"), ping(90, 0), ping(90, 0), ping(0, 90)]);
    let (agent, warned) = cached(p.resolver(), Always(IdleAction::Ping), Vec::new());
    let session = start(agent, Hub::new("s1"));
    says(&session, "go").await;
    sleep_s(3_600).await;
    assert_eq!(
        prewarms(&p),
        [false, true, true, true],
        "at 4.5, 9, 13.5 min"
    );
    assert_eq!(warned.count(), 1, "the ping that wrote");
    session.close().await;
}

/// A terminal holds the session the whole time its prompt waits.
#[tokio::test(start_paused = true)]
async fn a_hold_keeps_turns_out_but_not_the_ticks() {
    let p = ScriptedProvider::new([text_answer("hi"), ping(90, 0), ping(0, 90)]);
    let (agent, _warned) = cached(p.resolver(), Always(IdleAction::Ping), Vec::new());
    let session = start(agent, Hub::new("s1"));
    says(&session, "go").await;
    let hold = within(session.hold()).await.unwrap();
    sleep_s(3_600).await;
    assert_eq!(prewarms(&p), [false, true, true]);
    drop(hold);
    session.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_turn_starts_the_ticks_over_from_its_own_call() {
    let replies = [
        text_answer("hi"),
        ping(90, 0),
        text_answer("back"),
        ping(90, 0),
    ];
    let p = ScriptedProvider::new(replies);
    let (agent, _warned) = cached(p.resolver(), Always(IdleAction::Ping), Vec::new());
    let session = start(agent, Hub::new("s1"));
    says(&session, "go").await;
    sleep_s(280).await;
    says(&session, "back").await;
    // The ping at 270 s was due again at 540; the turn at 280 moved it to 550.
    sleep_s(265).await;
    assert_eq!(prewarms(&p), [false, true, false]);
    sleep_s(10).await;
    assert_eq!(prewarms(&p), [false, true, false, true]);
    session.close().await;
}

#[tokio::test(start_paused = true)]
async fn input_stops_a_running_tick_and_nobody_is_told() {
    let model = stalls_pings(ScriptedProvider::new([
        text_answer("hi"),
        text_answer("back"),
    ]));
    let (agent, warned) = cached(resolves_to(&model), Always(IdleAction::Ping), Vec::new());
    let session = start(agent, Hub::new("s1"));
    says(&session, "go").await;
    sleep_s(280).await;
    assert_eq!(model.pings.load(Ordering::SeqCst), 1, "a ping is out");
    // A peer's snapshot is answered at once and leaves the ping alone.
    within(session.snapshot()).await.unwrap();
    assert!(!model.stopped.load(Ordering::SeqCst));
    says(&session, "back").await;
    assert!(model.stopped.load(Ordering::SeqCst));
    assert_eq!(session.history().await.unwrap().messages.len(), 4);
    assert_eq!(warned.count(), 0);
    session.close().await;
}

#[tokio::test(start_paused = true)]
async fn closing_stops_a_running_tick() {
    let model = stalls_pings(ScriptedProvider::new([text_answer("hi")]));
    let (agent, warned) = cached(resolves_to(&model), Always(IdleAction::Ping), Vec::new());
    let session = start(agent, Hub::new("s1"));
    says(&session, "go").await;
    sleep_s(280).await;
    within(session.close()).await;
    assert!(model.stopped.load(Ordering::SeqCst));
    assert_eq!(warned.count(), 0);
}

#[tokio::test(start_paused = true)]
async fn keep_warm_ends_past_keep_warm_minutes() {
    let settings = CacheSettings {
        keep_warm: true,
        keep_warm_time: Duration::from_secs(10 * 60),
        ..CacheSettings::default()
    };
    let replies = [text_answer("hi"), ping(90, 0), ping(90, 0), ping(90, 0)];
    let p = ScriptedProvider::new(replies);
    let (agent, _warned) = cached(p.resolver(), settings, Vec::new());
    let session = start(agent, Hub::new("s1"));
    says(&session, "go").await;
    sleep_s(3_600).await;
    assert_eq!(
        prewarms(&p),
        [false, true, true],
        "at 4.5 and 9 min, not 13.5"
    );
    session.close().await;
}

#[tokio::test(start_paused = true)]
async fn what_a_tick_compacts_is_published() {
    let history = vec![
        Message::user("x".repeat(400_000)),
        Message::Assistant(text_answer("ok")),
    ];
    let mut done = text_answer("done");
    done.usage = Usage {
        input: 102_000,
        ..Usage::default()
    };
    let p = ScriptedProvider::new([done, text_answer("THE SUMMARY")]);
    let options = CompactionOptions {
        keep_recent: 2,
        ..CompactionOptions::new(200_000)
    };
    let (agent, _warned) = cached(p.resolver(), Always(IdleAction::Compact(options)), history);
    let hub = Hub::new("s1");
    let mut sub = hub.subscribe(0);
    let session = start(agent, hub);
    says(&session, "go").await;
    sleep_s(280).await;
    let compacted =
        |f: &crate::Frame| matches!(&f.body, Body::Wire(Outbound::Status(s)) if s.messages == 3);
    until(&mut sub, compacted).await;
    assert_eq!(p.requests().len(), 2, "the turn and the summary");
    session.close().await;
}
