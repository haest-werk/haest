//! P1: a run key, the run and run_end records, the finished-run shortcut;
//! and that nothing is durable unless the host asks.

use serde_json::json;

use super::*;
use crate::{
    messages::StopReason,
    run::{RunError, Served, input_hash},
    testing::{text_answer, tool_answer},
};

#[tokio::test]
async fn a_second_start_with_the_same_key_returns_the_stored_result_and_asks_no_model() {
    let store = MemoryRunStore::new();
    let key = RunKey::new("job");
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Echo", json!({"v": 1}))]),
        text_answer("done"),
    ]);
    let (mut agent, _) = durable(&p, &store, &[echo_tool()], None);
    let first = agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();
    assert_eq!((first.served, first.text.as_str()), (Served::Fresh, "done"));

    // Another process: a model that has nothing to say, the same store.
    let silent = ScriptedProvider::new([]);
    let (mut again, _) = durable(&silent, &store, &[echo_tool()], None);
    let mut events = no_events();
    let second = again.run_durable(&key, "go", &mut events).await.unwrap();

    assert_eq!(second.served, Served::Stored);
    assert_eq!(silent.requests().len(), 0);
    assert!(events.is_empty(), "a stored result emits nothing");
    assert_eq!(
        (second.stop_reason, second.text, second.usage),
        (first.stop_reason, first.text, first.usage)
    );
}

#[tokio::test]
async fn the_same_key_with_another_input_is_refused() {
    let store = MemoryRunStore::new();
    let key = RunKey::new("job");
    let p = ScriptedProvider::new([text_answer("done")]);
    let (mut agent, _) = durable(&p, &store, &[], None);
    agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();
    let written = store.lines(&key);

    let mut events = no_events();
    let refused = agent.run_durable(&key, "something else", &mut events).await;

    match refused {
        Err(RunError::InputMismatch {
            key: k,
            stored,
            given,
        }) => {
            assert_eq!(k, key);
            assert_eq!(
                (stored, given),
                (input_hash("go"), input_hash("something else"))
            );
        }
        other => panic!("not refused: {other:?}"),
    }
    assert_eq!(p.requests().len(), 1, "only the first run asked the model");
    assert!(events.is_empty());
    assert_eq!(store.lines(&key), written, "nothing was written");
}

#[tokio::test]
async fn a_run_writes_its_start_first_and_its_end_last() {
    let store = MemoryRunStore::new();
    let key = RunKey::new("job");
    let p = ScriptedProvider::new([text_answer("done")]);
    let (mut agent, _) = durable(&p, &store, &[], None);
    agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    let kinds: Vec<String> = store
        .lines(&key)
        .iter()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["type"]
                .as_str()
                .unwrap()
                .into()
        })
        .collect();
    assert_eq!(kinds.first().map(String::as_str), Some("run"));
    assert_eq!(kinds.last().map(String::as_str), Some("run_end"));
    assert!(kinds.iter().any(|k| k == "turn"));
    assert_eq!(kinds.iter().filter(|k| *k == "message").count(), 2);
}

#[tokio::test]
async fn a_run_that_ends_in_an_error_stays_resumable() {
    let store = MemoryRunStore::new();
    let key = RunKey::new("job");
    // Out of script: the model call fails.
    let failing = ScriptedProvider::new([]);
    let (mut agent, _) = durable(&failing, &store, &[], None);
    let failed = agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();
    assert_eq!(failed.stop_reason, StopReason::Error);

    let p = ScriptedProvider::new([text_answer("done")]);
    let (mut again, _) = durable(&p, &store, &[], None);
    let retried = again
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(retried.served, Served::Resumed);
    assert_eq!(
        (retried.stop_reason, retried.text.as_str()),
        (StopReason::EndTurn, "done")
    );
    assert_eq!(p.requests().len(), 1);
}

#[tokio::test]
async fn a_durable_run_without_a_store_is_refused_and_does_nothing() {
    let p = ScriptedProvider::new([text_answer("done")]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    let mut events = no_events();
    let refused = agent
        .run_durable(&RunKey::new("job"), "go", &mut events)
        .await;
    assert!(matches!(refused, Err(RunError::NoStore)));
    assert!(events.is_empty() && p.requests().is_empty() && agent.history().is_empty());
}

/// A host that hands no store and no key gets today's turn: tools see no
/// run, no effect key, no resume; nothing is written anywhere new.
#[tokio::test]
async fn a_host_that_hands_no_store_and_no_key_gets_a_plain_turn() {
    let log: Log = Arc::default();
    let tools = [Probe::new("Write", false, &log).arc()];
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Write", json!({}))]),
        text_answer("done"),
    ]);
    let opts = Opts {
        tools: Some(tools.to_vec()),
        ..Default::default()
    };
    let (mut agent, hooks) = agent_with(&p, opts);
    let end = agent.run("go", &mut no_events()).await;

    assert_eq!(end.stop_reason, StopReason::EndTurn);
    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].resumed, seen[0].effect_key.clone()), (false, None));
    assert_eq!(*hooks.persisted.lock().unwrap(), agent.history());
}

#[tokio::test]
async fn a_plain_turn_never_touches_the_store_it_was_handed() {
    let store = MemoryRunStore::new();
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Echo", json!({"v": 1}))]),
        text_answer("done"),
    ]);
    let (mut agent, _) = durable(&p, &store, &[echo_tool()], None);
    agent.run("go", &mut no_events()).await;
    assert!(store.keys().is_empty());
}
