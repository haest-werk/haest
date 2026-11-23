//! The hooks around an answer in a resumed run: asked again about the answer
//! the run stopped after when the records cannot say they returned, not when
//! they can.

use std::sync::Mutex;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{
    deps::AgentHooks,
    error::BoxError,
    messages::{AssistantMessage, StopReason},
    run::{Served, replay_run},
    testing::{text_answer, tool_answer},
};

/// A host that, asked about an answer, says so and never returns: the
/// process dies inside the hook.
struct DiesInTheHook(Arc<Notify>);

#[async_trait]
impl AgentHooks for DiesInTheHook {
    async fn after_model_call(
        &self,
        _: &mut Agent,
        _: &AssistantMessage,
        _: &CancellationToken,
    ) -> Result<(), BoxError> {
        self.0.notify_one();
        std::future::pending().await
    }
}

fn answer(calls: &[&str]) -> Asked {
    Asked::Answer {
        calls: calls.iter().map(ToString::to_string).collect(),
        last: true,
    }
}

/// A run on `store` killed while the host looked at its answer, one call of
/// the read-only `Log`.
async fn killed_in_the_hook(store: &MemoryRunStore, key: &RunKey) {
    let p = ScriptedProvider::new([tool_answer(&[("c1", "Log", json!({}))])]);
    let tools = [logging_tool(&Arc::default(), true)];
    let (mut agent, _) = durable(&p, store, &tools, None);
    let died = Arc::new(Notify::new());
    agent.map_deps(|d| d.with_hooks(DiesInTheHook(died.clone())));
    kill_when(&died, agent.run_durable(key, "go", &mut no_events())).await;
}

/// The second process: an agent on `p` asking `host`, its `Log` writing to
/// the host's log.
fn resumer(p: &ScriptedProvider, store: &MemoryRunStore, host: &Arc<Asking>) -> Agent {
    let tools = [logging_tool(&host.seen, true)];
    let (mut agent, _) = durable(p, store, &tools, None);
    agent.map_deps(|d| d.with_hooks(host.clone()));
    agent
}

#[tokio::test]
async fn a_run_killed_in_the_hook_after_a_model_call_asks_again_before_its_calls_run() {
    let (store, key) = (MemoryRunStore::new(), RunKey::new("job"));
    killed_in_the_hook(&store, &key).await;

    let p2 = ScriptedProvider::new([text_answer("done")]);
    let host = Arc::new(Asking::default());
    let resumed = resumer(&p2, &store, &host)
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(
        (resumed.served, resumed.stop_reason),
        (Served::Resumed, StopReason::EndTurn)
    );
    let seen = host.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        [
            answer(&["c1"]),
            Asked::Tool("c1".into()),
            answer(&[]),
            Asked::TurnEnd
        ]
    );
}

#[tokio::test]
async fn a_hook_that_refuses_on_resume_leaves_no_call_without_a_result() {
    let (store, key) = (MemoryRunStore::new(), RunKey::new("job"));
    killed_in_the_hook(&store, &key).await;

    let p2 = ScriptedProvider::new([]);
    let host = Arc::new(Asking {
        refuse: Some("not now"),
        ..Default::default()
    });
    let resumed = resumer(&p2, &store, &host)
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(resumed.stop_reason, StopReason::Error);
    assert_eq!(*host.seen.lock().unwrap(), [answer(&["c1"])], "no tool ran");
    assert!(p2.requests().is_empty());
    let run = replay_run(&store.lines(&key).join("\n")).unwrap();
    assert!(run.pending.is_empty(), "{:?}", run.pending);
}

#[tokio::test]
async fn a_run_killed_after_a_call_answered_does_not_ask_about_its_answer_again() {
    let (store, key, log) = (MemoryRunStore::new(), RunKey::new("job"), Log::default());
    let hung = Arc::new(Notify::new());
    let tools = [
        Probe::new("Write", false, &log).arc(),
        Probe::new("Pay", false, &log).hangs(&hung).arc(),
    ];
    let p = ScriptedProvider::new([tool_answer(&[
        ("c1", "Write", json!({})),
        ("c2", "Pay", json!({})),
    ])]);
    let (mut agent, _) = durable(&p, &store, &tools, None);
    kill_when(&hung, agent.run_durable(&key, "go", &mut no_events())).await;
    drop(agent);

    let p2 = ScriptedProvider::new([text_answer("done")]);
    let (mut agent, _) = durable(&p2, &store, &tools, None);
    let host = Arc::new(Asking::default());
    agent.map_deps(|d| d.with_hooks(host.clone()));
    agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    // Only the answer the resumed turn got: the hook had returned about the
    // first one before its calls ran.
    assert_eq!(*host.seen.lock().unwrap(), [answer(&[]), Asked::TurnEnd]);
}

#[tokio::test]
async fn a_run_whose_answer_landed_without_its_end_asks_before_the_turn_ends_again() {
    let (store, key) = (MemoryRunStore::new(), RunKey::new("job"));
    let p = ScriptedProvider::new([text_answer("done")]);
    let (mut agent, _) = durable(&p, &store, &[], None);
    let first = Arc::new(Asking::default());
    agent.map_deps(|d| d.with_hooks(first.clone()));
    agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();
    // A fresh run asks as a plain turn does.
    assert_eq!(*first.seen.lock().unwrap(), [answer(&[]), Asked::TurnEnd]);
    // The run's end never landed.
    let store = torn(&store, &key, 1);

    let p2 = ScriptedProvider::new([text_answer("checked")]);
    let (mut agent, _) = durable(&p2, &store, &[], None);
    let host = Arc::new(Asking {
        go_on: Mutex::new(vec!["check it again"]),
        ..Default::default()
    });
    agent.map_deps(|d| d.with_hooks(host.clone()));
    let resumed = agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(
        (resumed.served, resumed.text.as_str()),
        (Served::Resumed, "checked")
    );
    let sent = &p2.requests()[0].messages;
    assert!(matches!(sent.last(), Some(Message::User(u)) if u.content == "check it again"));
    assert_eq!(
        *host.seen.lock().unwrap(),
        [answer(&[]), Asked::TurnEnd, answer(&[]), Asked::TurnEnd]
    );
}
