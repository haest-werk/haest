//! A subagent's caps are its own: they wind it down on their own share, and a
//! stop or a refusal hands the host the subagent cap that applied.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::{
    LoopEvent, SharedDeps, ToolRef,
    caps::{ConversationCap, SubagentCaps},
    remedy::{Limit, Remedies},
    threshold::Thresholds,
    tool::ToolError,
    wind_down::{WindDown, WindDownWatch, WindingDown},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    caps,
    support::{CALL_USD, Rig, call, now, say, spawns},
};
use crate::{
    agents::{AgentDirectory, AgentError, AgentRef, SpawnInput, SpawnOutcome, agent_tools},
    testkit::{input, shut_ctx},
};

/// Counts the spawns it is asked for.
#[derive(Default)]
struct Counting(Mutex<u32>);

#[async_trait]
impl AgentDirectory for Counting {
    async fn spawn(
        &self,
        _: &AgentRef,
        _: SpawnInput,
        _: CancellationToken,
    ) -> Result<SpawnOutcome, AgentError> {
        *self.0.lock().unwrap() += 1;
        Ok(SpawnOutcome {
            text: "done".into(),
            cost_usd: 0.0,
        })
    }
    fn deliver(&self, _: &str, _: &str, _: &str) -> Result<String, AgentError> {
        Ok(String::new())
    }
    fn stop(&self, _: &str) -> bool {
        false
    }
}

/// A cap one call ($0.01) takes past 80% of, and not to the cap.
const PAST_80_IN_ONE_CALL: f64 = CALL_USD * 1.2;

fn agent_tool(dir: &Arc<Counting>) -> ToolRef {
    agent_tools(dir.clone(), AgentRef::main(), true)[0].clone()
}

/// The conversation winds down at `share`.
fn wind_down_at(share: f64) -> impl Fn(SharedDeps) -> SharedDeps + Send + Sync + 'static {
    move |d| d.with_thresholds(Thresholds::default().wind_down(WindDown::at(share).unwrap()))
}

fn guards(events: &[LoopEvent]) -> usize {
    let guard = |e: &&LoopEvent| matches!(e, LoopEvent::Guard { .. });
    events.iter().filter(guard).count()
}

#[tokio::test]
async fn the_agent_tool_starts_nothing_while_its_turn_winds_down_and_names_the_cap() {
    let dir = Arc::new(Counting::default());
    let tool = agent_tool(&dir);
    let brief = || input(json!({"description": "look", "prompt": "find x"}));
    tool.execute(brief(), &shut_ctx()).await.unwrap();
    assert_eq!(*dir.0.lock().unwrap(), 1, "no watch: spawns as before");

    let past = WindingDown {
        fact: "the budget is nearly spent: $8.10 of $10 (`max_conversation_usd`)".into(),
        limit: Some(Limit::Conversation(ConversationCap::Usd(10.0))),
        remedy: Some("REMEDY".into()),
    };
    let fact = past.clone();
    let ctx = shut_ctx().with_wind_down(WindDownWatch::new(move || Some(fact.clone())));
    let refused = tool.execute(brief(), &ctx).await;
    assert_eq!(*dir.0.lock().unwrap(), 1, "nothing started");
    let Err(ToolError::Input(text)) = refused else {
        panic!("{refused:?}")
    };
    assert_eq!(text, AgentError::WindingDown(past).to_string());
    assert!(text.contains("max_conversation_usd"), "{text}");
    assert!(text.ends_with(" REMEDY"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn crossing_the_conversations_share_refuses_mains_spawn_and_tells_main_once() {
    let mut r = Rig::new(
        now(|_, n| match n {
            0 => vec![call(
                "a",
                "Agent",
                json!({"description": "x", "prompt": "p"}),
            )],
            1 => vec![call(
                "b",
                "Agent",
                json!({"description": "y", "prompt": "p"}),
            )],
            _ => say("ok"),
        }),
        now(|_, _| say("done")),
        caps(),
    );
    r.adjust(wind_down_at(0.8));
    // Main's first call ($0.01) is past 80% of this budget.
    let budget = ConversationCap::Usd(PAST_80_IN_ONE_CALL);
    r.parent.ledger.lock().unwrap().set_cap(budget);
    r.turn("go").await;
    assert!(r.ids("started").is_empty(), "no subagent started");
    let results = r.tool_results();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|(_, refused)| *refused), "{results:?}");
    assert_eq!(guards(&r.events), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn below_the_share_main_spawns_and_hears_nothing() {
    let mut r = Rig::new(
        spawns(json!({"description": "x", "prompt": "p"}), "ok"),
        now(|_, _| say("done")),
        caps(),
    );
    r.adjust(wind_down_at(0.8));
    r.parent
        .ledger
        .lock()
        .unwrap()
        .set_cap(ConversationCap::Usd(100.0));
    r.turn("go").await;
    assert_eq!(r.ids("started").len(), 1);
    assert_eq!(r.ids("guard").len(), 0);
    assert_eq!(guards(&r.events), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_winds_down_on_its_own_caps_while_main_goes_on() {
    let mut r = Rig::new(
        spawns(json!({"description": "mid", "prompt": "p"}), "ok"),
        // Its first call takes it past 40% of its own cap (not yet to the
        // guard's nudge at half) and asks for a subagent of its own.
        now(|_, n| match n {
            0 => vec![call(
                "m",
                "Agent",
                json!({"description": "leaf", "prompt": "p"}),
            )],
            _ => say("mid done"),
        }),
        SubagentCaps {
            usd: CALL_USD * 2.2,
            ..caps()
        },
    );
    r.adjust(wind_down_at(0.4));
    r.turn("go").await;
    let started = r.ids("started");
    assert_eq!(started.len(), 1, "the leaf never started: {started:?}");
    assert_eq!(r.ids("guard"), started, "the subagent heard it, once");
    assert_eq!(guards(&r.events), 0, "main's caps are not the subagent's");
    assert!(r.texts().iter().any(|t| t.contains("mid done")));
}

/// Regression: the hub's children ran on caps that did not say they were a
/// subagent's, so a child stopped at its step cap told the parent to raise
/// `max_turn_steps` (the host's remedy for the TURN cap).
#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_stopped_at_its_own_cap_carries_the_hosts_remedy_for_that_cap() {
    let mut r = Rig::new(
        spawns(json!({"description": "looper", "prompt": "p"}), "ok"),
        now(|_, n| {
            vec![call(
                &format!("l{n}"),
                "Glob",
                json!({"pattern": format!("*.{n}")}),
            )]
        }),
        SubagentCaps { steps: 3, ..caps() },
    );
    r.adjust(|d| {
        d.with_remedies(Remedies::new(|limit| match limit {
            Limit::SubagentSteps(3) => Some("REMEDY-SUBAGENT".into()),
            Limit::TurnSteps(_) => Some("REMEDY-TURN".into()),
            _ => None,
        }))
    });
    r.turn("go").await;
    let answer = &r.tool_results()[0].0;
    assert!(answer.contains("REMEDY-SUBAGENT"), "{answer}");
    assert!(!answer.contains("REMEDY-TURN"), "{answer}");
}
