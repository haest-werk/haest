//! Every model call compaction makes is counted: in what it reports, on the
//! conversation's ledger, and in the running turn's caps, whether the
//! summary came out or not.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use super::*;
use crate::{
    AgentDeps, AgentOptions, ResolvedModel, SharedDeps, Tools,
    agent::test_support::{MemWorkspace, collect, end, turn_caps},
    caps::{ConversationCaps, TurnCaps, TurnCapsSource},
    deps::AgentHooks,
    error::BoxError,
    ledger::{Ledger, NoLedgerHooks},
    provider::{Provider, ProviderRef, ProviderRequest, ProviderStream},
};

/// A host that compacts before every model call, as the runtime's does.
/// `esc`: the human cancels the turn as the summary is asked for.
struct CompactsFirst {
    caps: Option<TurnCaps>,
    esc: bool,
}
#[async_trait]
impl AgentHooks for CompactsFirst {
    async fn before_model_call(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        if self.esc {
            agent.control().abort();
        }
        compact(agent, cancel, &CFG).await?;
        Ok(())
    }
}
impl TurnCapsSource for CompactsFirst {
    fn turn_caps(&self, _: &str) -> Option<TurnCaps> {
        self.caps.clone()
    }
}

/// A reply that is no summary, at a price.
fn costly_empty(input: u64) -> AssistantMessage {
    let mut m = empty_answer();
    m.usage.input = input;
    m
}

fn compacting(p: &ScriptedProvider, caps: Option<TurnCaps>) -> Agent {
    let mut agent = agent_with(p);
    let host = Arc::new(CompactsFirst { caps, esc: false });
    agent.map_deps(|d| d.with_hooks(host.clone()));
    agent.map_deps(|d| d.with_turn_caps(host));
    agent
}

#[tokio::test]
async fn a_cold_summary_reports_the_warm_call_too() {
    let p = ScriptedProvider::new([costly_empty(700), text_answer("COLD")]);
    let mut agent = agent_with(&p);
    let res = compact(&mut agent, &CancellationToken::new(), &CFG).await;
    let res = res.unwrap().unwrap();
    assert_eq!(res.path, SummaryPath::Cold);
    assert_eq!(res.usage.input, 700 + 10);
}

/// Answers the first call from `first`; every call after it runs until it
/// is dropped (a cold summary given up on).
struct FirstThenStuck {
    first: ScriptedProvider,
    calls: AtomicUsize,
}

impl Provider for FirstThenStuck {
    fn id(&self) -> &str {
        "scripted"
    }

    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return self.first.stream(req);
        }
        Box::pin(futures::stream::pending())
    }
}

/// The compaction tests' history, on `provider`, charging `ledger`.
fn on_ledger(provider: impl Into<ProviderRef>, ledger: &Ledger) -> Agent {
    let provider = provider.into();
    let resolve = move |_: &str| Ok::<_, ModelError>(ResolvedModel::new(provider.clone(), "m"));
    let shared =
        SharedDeps::new(resolve, "/tmp", MemWorkspace::default()).with_ledger(ledger.clone());
    let mut agent = Agent::new(
        AgentDeps::new(shared, Tools::new(), "sys"),
        AgentOptions::new("fake:m1"),
    );
    agent.replace_history(history());
    agent
}

/// The caller charged the ledger once `compact` returned, so one dropped
/// while its cold call ran (the human gave up on a /compact) had booked
/// nothing, though the warm call it made was paid for. Each call goes on
/// the ledger as it comes back.
#[tokio::test]
async fn a_compaction_dropped_part_way_has_booked_the_call_that_came_back() {
    let mut warm = empty_answer();
    warm.usage.cost_usd = Some(0.25);
    let stuck = Arc::new(FirstThenStuck {
        first: ScriptedProvider::new([warm]),
        calls: AtomicUsize::new(0),
    });
    let ledger = Ledger::new(ConversationCaps::default(), NoLedgerHooks);
    let mut agent = on_ledger(stuck.clone(), &ledger);
    let cancel = CancellationToken::new();
    let compacting = compact(&mut agent, &cancel, &CFG);
    let cold_is_out = async {
        while stuck.calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        done = compacting => panic!("the cold call never answers: {done:?}"),
        () = cold_is_out => {}
    }
    assert_eq!(ledger.usd(), 0.25, "the warm call is booked");
}

/// What the summary calls cost is reported, and already on the ledger: a
/// caller that charged it again would count it twice.
#[tokio::test]
async fn a_compaction_books_each_call_on_the_ledger_once() {
    let mut warm = empty_answer();
    warm.usage.cost_usd = Some(0.25);
    let mut cold = text_answer("COLD");
    cold.usage.cost_usd = Some(0.5);
    let ledger = Ledger::new(ConversationCaps::default(), NoLedgerHooks);
    let mut agent = on_ledger(ScriptedProvider::new([warm, cold]), &ledger);
    let res = compact(&mut agent, &CancellationToken::new(), &CFG).await;
    let res = res.unwrap().unwrap();
    assert_eq!(res.usage.cost_usd, Some(0.75), "what it reports");
    assert_eq!(ledger.usd(), 0.75, "what the ledger holds");
}

/// What the caller is told the calls cost.
#[tokio::test]
async fn a_failed_compaction_says_what_its_calls_cost() {
    let p = ScriptedProvider::new([costly_empty(700), costly_empty(300)]);
    let mut agent = agent_with(&p);
    let err = compact(&mut agent, &CancellationToken::new(), &CFG).await;
    assert_eq!(err.unwrap_err().usage().input, 1000);
}

#[tokio::test]
async fn a_failed_compaction_still_counts_toward_the_turn() {
    let p = ScriptedProvider::new([costly_empty(700), costly_empty(300)]);
    let mut agent = compacting(&p, None);
    let events = collect(&mut agent, "go").await;
    let (reason, usage, _) = end(&events);
    assert_eq!(reason, StopReason::Error);
    assert_eq!(usage.input, 1000);
}

/// Esc while the summary is written: the turn was cancelled, not failed.
#[tokio::test]
async fn a_summary_cancelled_part_way_ends_the_turn_aborted_and_still_counts_toward_it() {
    let p = ScriptedProvider::new([text_answer("SUM"), text_answer("never")]);
    let mut agent = agent_with(&p);
    agent.map_deps(|d| {
        d.with_hooks(Arc::new(CompactsFirst {
            caps: None,
            esc: true,
        }))
    });
    let events = collect(&mut agent, "go").await;
    let (reason, usage, error) = end(&events);
    assert_eq!((reason, error), (StopReason::Aborted, None));
    assert_eq!(usage.input, 10);
    assert_eq!(p.requests().len(), 1, "the summary call only");
}

#[tokio::test]
async fn a_summary_counts_toward_the_turn_with_the_call_after_it() {
    let p = ScriptedProvider::new([text_answer("SUM"), text_answer("done")]);
    let mut agent = compacting(&p, None);
    let events = collect(&mut agent, "go").await;
    let (reason, usage, _) = end(&events);
    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(usage.input, 10 + 10);
}

#[tokio::test]
async fn a_summary_that_spends_past_the_turn_cap_stops_the_turn_before_the_model_call() {
    let p = ScriptedProvider::new([costly_empty(5000), text_answer("SUM"), text_answer("never")]);
    let caps = turn_caps(|l| l.max_turn_tokens = 1000);
    let mut agent = compacting(&p, Some(caps));
    let events = collect(&mut agent, "go").await;
    let (reason, _, error) = end(&events);
    assert_eq!(reason, StopReason::MaxTokens, "{error:?}");
    assert_eq!(p.requests().len(), 2, "the summary calls only");
}
