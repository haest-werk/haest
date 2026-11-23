//! An idle conversation's ticks: when they are due, what each does, and
//! what ends them; on the hooks' clock, so no test waits.

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use super::{tests::Cold, *};
use crate::{
    Agent,
    agent::test_support::{Opts, TestHooks, agent_with, collect},
    catalog::{ModelFacts, ModelPricing},
    compaction::CompactionOptions,
    messages::{AssistantMessage, Message, StopReason, Usage},
    provider::ResolvedModel,
    testing::{ScriptedProvider, text_answer},
};

/// When the turn's model call starts.
const T0: u64 = 1_000_000;
/// 4.5 minutes: a 5m cache's tick.
const TICK: u64 = 270_000;

/// $3 in, $0.30 a read, $15 out, $3.75 a 5m write.
fn priced() -> ModelPricing {
    ModelPricing::new(3.0, 0.3, 15.0).with_cache_write(3.75)
}

/// Every model costs `0`'s price.
struct Priced(ModelPricing);

impl ModelFacts for Priced {
    fn pricing(&self, _spec: &str) -> Option<ModelPricing> {
        Some(self.0.clone())
    }
}

/// A policy that answers `0` at every tick and notes what it saw.
struct Asks(IdleAction, Mutex<Vec<(Duration, CacheTtl, u64)>>);

impl Asks {
    fn new(action: IdleAction) -> Arc<Self> {
        Arc::new(Self(action, Mutex::new(Vec::new())))
    }
}

impl CachePolicy for Asks {
    fn plan(&self, _request: &RequestShape<'_>) -> CachePlan {
        CachePlan::default()
    }
    fn on_idle(&self, idle: &IdleState<'_>) -> IdleAction {
        let seen = (idle.idle, idle.ttl, idle.prefix_tokens);
        self.1.lock().unwrap().push(seen);
        self.0.clone()
    }
}

fn answer(text: &str, context: u64) -> AssistantMessage {
    let mut m = text_answer(text);
    m.usage = Usage {
        input: context,
        ..Usage::default()
    };
    m
}

fn ping(read: u64, write: u64) -> AssistantMessage {
    let mut m = AssistantMessage::start("scripted", "m1");
    m.usage.cache_read = read;
    m.usage.cache_write = write;
    m.stop_reason = StopReason::MaxTokens;
    m
}

/// An agent whose policy is `policy`, after one turn answered at [`T0`].
async fn after_a_turn(
    replies: impl IntoIterator<Item = AssistantMessage>,
    policy: Arc<Asks>,
    opts: Opts,
) -> (Agent, ScriptedProvider, Arc<TestHooks>) {
    let p = ScriptedProvider::new(replies);
    let (mut agent, hooks) = agent_with(&p, opts);
    agent.set_cache_policy(policy);
    hooks.set_now(T0);
    collect(&mut agent, "go").await;
    (agent, p, hooks)
}

#[test]
fn compaction_pays_when_the_summary_and_the_smaller_write_cost_less() {
    let p = priced();
    let five = CacheTtl::FiveMinutes;
    assert!(compact_pays(
        Some(&p),
        five,
        150_000,
        20_000,
        SUMMARY_TOKENS
    ));
    assert!(!compact_pays(
        Some(&p),
        five,
        30_000,
        25_000,
        SUMMARY_TOKENS
    ));
    assert!(!compact_pays(None, five, 150_000, 20_000, SUMMARY_TOKENS));
    assert!(
        !compact_pays(Some(&p), CacheTtl::OneHour, 150_000, 20_000, SUMMARY_TOKENS),
        "no 1h write price"
    );
}

#[tokio::test]
async fn a_tick_is_due_a_tenth_before_expiry_and_only_after_an_answered_call() {
    let p = ScriptedProvider::new([text_answer("hi"), text_answer("again")]);
    let (mut agent, hooks) = agent_with(&p, Opts::default());
    assert_eq!(agent.next_cache_tick(), None, "nothing sent yet");
    hooks.set_now(T0);
    collect(&mut agent, "go").await;
    assert_eq!(agent.next_cache_tick(), Some(T0 + TICK));
    agent.set_cache_policy(CachePlan::none());
    collect(&mut agent, "again").await;
    assert_eq!(
        agent.next_cache_tick(),
        None,
        "no breakpoint, no cache to keep"
    );
}

#[tokio::test]
async fn the_default_policy_does_nothing_and_ends_the_ticks_until_the_next_turn() {
    let p = ScriptedProvider::new([text_answer("hi"), text_answer("again")]);
    let (mut agent, hooks) = agent_with(&p, Opts::default());
    hooks.set_now(T0);
    collect(&mut agent, "go").await;
    hooks.set_now(T0 + TICK);
    let tick = agent.cache_tick(&CancellationToken::new()).await;
    assert!(matches!(tick, CacheTick::Nothing), "{tick:?}");
    assert_eq!(agent.next_cache_tick(), None);
    hooks.set_now(T0 + 2 * TICK);
    collect(&mut agent, "back").await;
    assert_eq!(agent.next_cache_tick(), Some(T0 + 3 * TICK), "a new turn");
    assert_eq!(p.requests().len(), 2, "no ping");
}

#[tokio::test]
async fn a_ping_that_reads_keeps_the_ticks_going_from_its_own_start() {
    let policy = Asks::new(IdleAction::Ping);
    let replies = [answer("hi", 9_000), ping(9_000, 0)];
    let (mut agent, p, hooks) = after_a_turn(replies, policy.clone(), Opts::default()).await;
    hooks.set_now(T0 + TICK);
    let tick = agent.cache_tick(&CancellationToken::new()).await;
    assert!(tick.keeps_going(), "{tick:?}");
    assert_eq!(agent.next_cache_tick(), Some(T0 + 2 * TICK));
    assert!(p.requests()[1].prewarm);
    let seen = policy.1.lock().unwrap().clone();
    let away = Duration::from_millis(TICK);
    assert_eq!(seen, [(away, CacheTtl::FiveMinutes, 9_000)]);
    assert!(hooks.warnings.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_ping_that_wrote_ends_the_ticks_and_the_human_hears_it() {
    let policy = Asks::new(IdleAction::Ping);
    let replies = [
        answer("hi", 9_000),
        ping(0, 9_000),
        answer("again", 9_000),
        ping(0, 9_000),
    ];
    let (mut agent, _p, hooks) = after_a_turn(replies, policy, Opts::default()).await;
    hooks.set_now(T0 + TICK);
    let tick = agent.cache_tick(&CancellationToken::new()).await;
    assert!(matches!(&tick, CacheTick::Pinged(o) if o.cache == WarmCache::Wrote));
    assert!(!tick.keeps_going());
    assert_eq!(agent.next_cache_tick(), None);
    assert_eq!(hooks.warnings.lock().unwrap().len(), 1);
    // Each such ping is news, even one just like the last.
    hooks.set_now(T0 + 10 * TICK);
    collect(&mut agent, "back").await;
    hooks.set_now(T0 + 11 * TICK);
    agent.cache_tick(&CancellationToken::new()).await;
    assert_eq!(hooks.warnings.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn an_expired_cache_is_neither_pinged_nor_compacted() {
    for action in [
        IdleAction::Ping,
        IdleAction::Compact(CompactionOptions::new(200_000)),
    ] {
        let policy = Asks::new(action);
        let (mut agent, p, hooks) =
            after_a_turn([answer("hi", 9_000)], policy.clone(), Opts::default()).await;
        hooks.set_now(T0 + 300_000);
        let tick = agent.cache_tick(&CancellationToken::new()).await;
        assert!(matches!(tick, CacheTick::Nothing), "{tick:?}");
        assert_eq!(p.requests().len(), 1, "nothing sent");
        assert!(policy.1.lock().unwrap().is_empty(), "not even asked");
    }
}

#[tokio::test]
async fn a_refused_pre_warm_is_told_once() {
    let cold = Cold::default();
    let policy = Asks::new(IdleAction::Ping);
    let (mut agent, hooks) = agent_with(&ScriptedProvider::new([]), Opts::default());
    agent.map_deps(|d| d.with_models(ResolvedModel::new(cold.clone(), "m")));
    agent.set_cache_policy(policy);
    for turn in 0..2 {
        let at = T0 + turn * 10 * TICK;
        hooks.set_now(at);
        collect(&mut agent, "go").await;
        hooks.set_now(at + TICK);
        let tick = agent.cache_tick(&CancellationToken::new()).await;
        assert!(matches!(
            tick,
            CacheTick::PingFailed(WarmError::Refused { .. })
        ));
    }
    assert_eq!(hooks.warnings.lock().unwrap().len(), 1);
}

/// A long first message, its answer, then the turn: what compaction cuts.
fn long_history() -> Vec<Message> {
    vec![
        Message::user("x".repeat(400_000)),
        Message::Assistant(answer("read it", 101_000)),
    ]
}

#[tokio::test]
async fn compaction_runs_while_the_cache_is_warm_and_only_when_it_pays() {
    let options = CompactionOptions {
        keep_recent: 2,
        instructions: Some("Keep the TODOs.".into()),
        ..CompactionOptions::new(200_000)
    };
    let opts = || Opts {
        history: long_history(),
        ..Opts::default()
    };
    let replies = [answer("done", 102_000), text_answer("THE SUMMARY")];
    let policy = Asks::new(IdleAction::Compact(options.clone()));
    let (mut agent, p, hooks) = after_a_turn(replies.clone(), policy.clone(), opts()).await;
    agent.map_deps(|d| d.with_facts(Priced(priced())));
    hooks.set_now(T0 + TICK);
    let tick = agent.cache_tick(&CancellationToken::new()).await;
    assert!(matches!(tick, CacheTick::Compacted(Some(_))), "{tick:?}");
    assert_eq!(agent.history().len(), 3, "the summary, go, done");
    assert_eq!(agent.next_cache_tick(), None);
    let summary_request = &p.requests()[1];
    assert!(format!("{:?}", summary_request.messages).contains("Keep the TODOs."));
    // Unpriced: it cannot say compaction pays, so it does not compact.
    let (mut agent, p, hooks) = after_a_turn(replies, policy, opts()).await;
    hooks.set_now(T0 + TICK);
    let tick = agent.cache_tick(&CancellationToken::new()).await;
    assert!(matches!(tick, CacheTick::Nothing), "{tick:?}");
    assert_eq!((agent.history().len(), p.requests().len()), (4, 1));
}
