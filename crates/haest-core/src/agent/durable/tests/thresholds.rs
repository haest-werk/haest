//! A resumed run's step ends at the same boundary as any step: the turn's
//! thresholds are checked there, and one that fired before the stop does not
//! fire again.

use serde_json::json;

use super::*;
use crate::{
    testing::{text_answer, tool_answer},
    threshold::{Action, Mark, Meter, Reading, Rearm, Threshold, Thresholds},
};

/// Reminds the model once a turn from its first model call.
struct FirstCall;

impl Threshold for FirstCall {
    fn meter(&self) -> Meter {
        Meter::Steps
    }
    fn mark(&self) -> Mark {
        Mark::at(1.0)
    }
    fn action(&self) -> Action {
        Action::Remind
    }
    fn rearm(&self) -> Rearm {
        Rearm::NextTurn
    }
    fn text(&self, _: &Reading) -> String {
        "FIRST-CALL".into()
    }
}

/// [`durable`], with the guard's nudges and [`FirstCall`].
fn reminded(p: &Arc<ScriptedProvider>, store: &MemoryRunStore, tools: &[Arc<dyn Tool>]) -> Agent {
    let (mut agent, _) = durable(p, store, tools, None);
    let thresholds = Thresholds::default().with(Arc::new(FirstCall)).unwrap();
    agent.deps_mut().shared.thresholds = thresholds;
    agent
}

fn reminders(events: &[LoopEvent]) -> usize {
    count(
        events,
        |e| matches!(e, LoopEvent::Guard { text } if text.contains("FIRST-CALL")),
    )
}

#[tokio::test]
async fn a_resumed_step_checks_the_turns_thresholds_at_its_boundary() {
    let (store, key, log) = (MemoryRunStore::new(), RunKey::new("job"), Log::default());
    let hung = Arc::new(Notify::new());
    let tools = [Probe::new("Slow", false, &log).hangs(&hung).arc()];
    // Killed in the first step's tool: its boundary never came.
    let p = ScriptedProvider::new([tool_answer(&[("c1", "Slow", json!({}))])]);
    let mut agent = reminded(&p, &store, &tools);
    kill_when(&hung, agent.run_durable(&key, "go", &mut no_events())).await;
    drop(agent);

    let p2 = ScriptedProvider::new([text_answer("ok")]);
    let mut agent = reminded(&p2, &store, &tools);
    let mut events = no_events();
    agent.run_durable(&key, "go", &mut events).await.unwrap();
    assert_eq!(reminders(&events), 1);
}

#[tokio::test]
async fn a_threshold_that_fired_before_the_stop_does_not_fire_again() {
    let (store, key, log) = (MemoryRunStore::new(), RunKey::new("job"), Log::default());
    let hung = Arc::new(Notify::new());
    let tools = [
        Probe::new("Slow", false, &log).hangs(&hung).arc(),
        echo_tool(),
    ];
    // The first step's boundary reminds; the second step's tool is killed.
    let p = ScriptedProvider::new([
        echo("c1", json!({"v": 1})),
        tool_answer(&[("c2", "Slow", json!({}))]),
    ]);
    let mut agent = reminded(&p, &store, &tools);
    let mut first = no_events();
    kill_when(&hung, agent.run_durable(&key, "go", &mut first)).await;
    drop(agent);
    assert_eq!(reminders(&first), 1);

    let p2 = ScriptedProvider::new([text_answer("ok")]);
    let mut agent = reminded(&p2, &store, &tools);
    let mut events = no_events();
    agent.run_durable(&key, "go", &mut events).await.unwrap();
    assert_eq!(reminders(&events), 0);
}
