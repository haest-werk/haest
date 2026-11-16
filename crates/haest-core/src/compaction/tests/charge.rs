//! Every model call compaction makes is counted: in what it reports, and in
//! the running turn's caps, whether the summary came out or not.

use async_trait::async_trait;

use super::*;
use crate::{
    agent::test_support::{collect, end, turn_caps},
    caps::{TurnCaps, TurnCapsSource},
    deps::AgentHooks,
    tool::ToolServices,
};

/// A host that compacts before every model call, as the runtime's does.
/// `esc`: the human cancels the turn as the summary is asked for.
struct CompactsFirst {
    caps: Option<TurnCaps>,
    esc: bool,
}
impl ToolServices for CompactsFirst {}
#[async_trait]
impl AgentHooks for CompactsFirst {
    async fn before_model_call(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

fn compacting(p: &Arc<ScriptedProvider>, caps: Option<TurnCaps>) -> Agent {
    let mut agent = agent_with(p);
    let host = Arc::new(CompactsFirst { caps, esc: false });
    agent.deps_mut().shared.hooks = host.clone();
    agent.deps_mut().shared.turn_caps = host;
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

/// What the caller charges to the conversation's ledger.
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
    agent.deps_mut().shared.hooks = Arc::new(CompactsFirst {
        caps: None,
        esc: true,
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
