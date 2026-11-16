//! P2: a run killed mid-tool resumes. Calls that finished stay finished, the
//! call that may have run follows its tool's OnResume, a call that cannot have
//! started runs once, and the turn caps go on from the restored state.

use std::time::Duration;

use serde_json::json;

use super::*;
use crate::{
    messages::StopReason,
    run::Served,
    session::INTERRUPTED,
    testing::{text_answer, tool_answer},
};

fn three_calls(names: [&str; 3]) -> crate::messages::AssistantMessage {
    tool_answer(&[
        ("c1", names[0], json!({})),
        ("c2", names[1], json!({})),
        ("c3", names[2], json!({})),
    ])
}

#[tokio::test]
async fn a_run_killed_after_tool_two_of_three_resumes_and_runs_tool_three_once() {
    let (store, key, log) = (MemoryRunStore::new(), RunKey::new("job"), Log::default());
    let hung = Arc::new(Notify::new());
    let tools = [
        Probe::new("Write", false, &log).arc(),
        // A payment that passes its effect key on: on resume it looks up.
        (Probe::new("Pay", false, &log).on_resume(OnResume::Lookup))
            .hangs(&hung)
            .arc(),
        Probe::new("Mail", false, &log).arc(),
    ];
    let p = ScriptedProvider::new([three_calls(["Write", "Pay", "Mail"])]);
    let (mut agent, _) = durable(&p, &store, &tools, None);
    let mut events = no_events();
    kill_when(&hung, agent.run_durable(&key, "go", &mut events)).await;
    drop(agent);

    let p2 = ScriptedProvider::new([text_answer("all done")]);
    let (mut agent, _) = durable(&p2, &store, &tools, None);
    let resumed = agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(resumed.served, Served::Resumed);
    assert_eq!(
        (resumed.stop_reason, resumed.text.as_str()),
        (StopReason::EndTurn, "all done")
    );
    // Tool 1 finished before the kill and does not run again; tool 2 looks
    // up what it did; tool 3 never started, and runs once.
    assert_eq!(
        runs(&log),
        [
            run("Write", "c1", false),
            run("Pay", "c2", false),
            run("Pay", "c2", true),
            run("Mail", "c3", false),
        ]
    );
    let keys: Vec<Option<String>> = (log.lock().unwrap().iter())
        .filter(|s| s.tool == "Pay")
        .map(|s| s.effect_key.clone())
        .collect();
    assert!(
        keys[0].is_some() && keys[0] == keys[1],
        "the same effect key: {keys:?}"
    );
    // The first model call is not paid again: one request, after all three results.
    assert_eq!(p2.requests().len(), 1);
    let sent: Vec<String> = results_sent(&p2, 0)
        .into_iter()
        .map(|r| r.tool_call_id)
        .collect();
    assert_eq!(sent, ["c1", "c2", "c3"]);

    // A third start: the stored result.
    let p3 = ScriptedProvider::new([]);
    let (mut agent, _) = durable(&p3, &store, &tools, None);
    let stored = agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();
    assert_eq!((stored.served, p3.requests().len()), (Served::Stored, 0));
}

#[tokio::test]
async fn a_read_only_call_that_may_have_run_runs_again() {
    let (store, key, log) = (MemoryRunStore::new(), RunKey::new("job"), Log::default());
    let hung = Arc::new(Notify::new());
    let tools = [
        Probe::new("Look", true, &log).arc(),
        Probe::new("Peek", true, &log).hangs(&hung).arc(),
        Probe::new("Mail", false, &log).arc(),
    ];
    let p = ScriptedProvider::new([three_calls(["Look", "Peek", "Mail"])]);
    let (mut agent, _) = durable(&p, &store, &tools, None);
    kill_when(&hung, agent.run_durable(&key, "go", &mut no_events())).await;
    drop(agent);

    let p2 = ScriptedProvider::new([text_answer("ok")]);
    let (mut agent, _) = durable(&p2, &store, &tools, None);
    agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(
        runs(&log),
        [
            run("Look", "c1", false),
            run("Peek", "c2", false),
            run("Peek", "c2", true),
            run("Mail", "c3", false),
        ]
    );
}

#[tokio::test]
async fn a_call_with_side_effects_that_may_have_run_is_reported_not_run_again() {
    let (store, key, log) = (MemoryRunStore::new(), RunKey::new("job"), Log::default());
    let hung = Arc::new(Notify::new());
    let tools = [
        Probe::new("Write", false, &log).arc(),
        Probe::new("Send", false, &log).hangs(&hung).arc(),
        Probe::new("Look", true, &log).arc(),
    ];
    let p = ScriptedProvider::new([three_calls(["Write", "Send", "Look"])]);
    let (mut agent, _) = durable(&p, &store, &tools, None);
    kill_when(&hung, agent.run_durable(&key, "go", &mut no_events())).await;
    drop(agent);

    let p2 = ScriptedProvider::new([text_answer("ok")]);
    let (mut agent, _) = durable(&p2, &store, &tools, None);
    agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(
        runs(&log),
        [
            run("Write", "c1", false),
            run("Send", "c2", false),
            run("Look", "c3", false),
        ]
    );
    let sent = results_sent(&p2, 0);
    assert!(sent[1].is_error && sent[1].content == INTERRUPTED);
    assert!(!sent[2].is_error);
}

#[tokio::test]
async fn the_turn_caps_continue_from_the_restored_spend() {
    let (store, key, log) = (MemoryRunStore::new(), RunKey::new("job"), Log::default());
    let hung = Arc::new(Notify::new());
    let tools = [
        Probe::new("Slow", false, &log).hangs(&hung).arc(),
        echo_tool(),
    ];
    let two_steps = Some(turn_caps(|c| c.max_turn_steps = 2));
    let p = ScriptedProvider::new([tool_answer(&[("c1", "Slow", json!({}))])]);
    let (mut agent, _) = durable(&p, &store, &tools, two_steps.clone());
    kill_when(&hung, agent.run_durable(&key, "go", &mut no_events())).await;
    drop(agent);

    // A fresh turn would get two model calls; the resumed one made one already.
    let p2 = ScriptedProvider::new([echo("c2", json!({"v": 1})), text_answer("never asked")]);
    let (mut agent, _) = durable(&p2, &store, &tools, two_steps);
    let resumed = agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(resumed.stop_reason, StopReason::MaxTokens);
    assert_eq!(p2.requests().len(), 1);
    // Both model calls' usage: the one before the kill, the one after.
    assert_eq!((resumed.usage.input, resumed.usage.output), (20, 10));
    // A cap ends the run: the next start is the stored result.
    let p3 = ScriptedProvider::new([]);
    let (mut agent, _) = durable(&p3, &store, &tools, None);
    let stored = agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();
    assert_eq!(
        (stored.served, stored.stop_reason),
        (Served::Stored, StopReason::MaxTokens)
    );
}

#[tokio::test]
async fn the_time_the_run_was_stopped_does_not_count_toward_the_time_cap() {
    let (store, key, log) = (MemoryRunStore::new(), RunKey::new("job"), Log::default());
    let hung = Arc::new(Notify::new());
    let tools = [Probe::new("Slow", false, &log).hangs(&hung).arc()];
    let half_hour = Some(turn_caps(|c| {
        c.max_turn_time = Duration::from_secs(30 * 60)
    }));
    let p = ScriptedProvider::new([tool_answer(&[("c1", "Slow", json!({}))])]);
    let (mut agent, hooks) = durable(&p, &store, &tools, half_hour.clone());
    hooks.set_now(1_000_000);
    kill_when(&hung, agent.run_durable(&key, "go", &mut no_events())).await;
    drop(agent);

    // Back ten hours later.
    let p2 = ScriptedProvider::new([text_answer("ok")]);
    let (mut agent, hooks) = durable(&p2, &store, &tools, half_hour);
    hooks.set_now(1_000_000 + 10 * 3_600_000);
    let resumed = agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(resumed.stop_reason, StopReason::EndTurn);
    assert_eq!(p2.requests().len(), 1);
}
