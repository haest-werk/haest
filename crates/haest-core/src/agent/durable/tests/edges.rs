//! Resuming on the host's history, and logs whose last records never landed.

use serde_json::json;

use super::*;
use crate::{
    messages::StopReason,
    run::{RunError, Served},
    session::replay,
    testing::{text_answer, tool_answer},
};

fn prior() -> Vec<Message> {
    vec![
        Message::user("earlier"),
        Message::Assistant(text_answer("earlier answer")),
    ]
}

/// A run on `prior()` killed in its first tool; the store and the tools.
async fn killed_on_prior_history() -> (MemoryRunStore, RunKey, Tools, Vec<Message>) {
    let (store, key, log) = (MemoryRunStore::new(), RunKey::new("job"), Log::default());
    let hung = Arc::new(Notify::new());
    let tools = vec![Probe::new("Slow", false, &log).hangs(&hung).arc()];
    let p = ScriptedProvider::new([tool_answer(&[("c1", "Slow", json!({}))])]);
    let (mut agent, hooks) = durable(&p, &store, &tools, None);
    agent.replace_history(prior());
    kill_when(&hung, agent.run_durable(&key, "go", &mut no_events())).await;
    // What the host's own session file got: every message the loop appended.
    let hosts_file = hooks.persisted.lock().unwrap().clone();
    (store, key, tools.into(), hosts_file)
}

#[tokio::test]
async fn a_resume_goes_on_from_the_history_the_run_started_on() {
    let (store, key, tools, _) = killed_on_prior_history().await;
    let p2 = ScriptedProvider::new([text_answer("ok")]);
    let (mut agent, _) = durable(&p2, &store, &tools, None);
    agent.replace_history(prior());

    let resumed = agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(resumed.served, Served::Resumed);
    let sent = &p2.requests()[0].messages;
    // The prior two, the input, the call, its answer.
    assert_eq!(sent.len(), 5);
    assert_eq!(sent[..2], prior());
}

#[tokio::test]
async fn a_history_holding_the_runs_messages_is_cut_back_to_where_the_run_started() {
    let (store, key, tools, hosts_file) = killed_on_prior_history().await;
    // The host replays its own file: prior history, the run's messages, and
    // the dangling call answered as interrupted.
    let mut from_file = prior();
    from_file.extend(replay_messages(&hosts_file));
    let p2 = ScriptedProvider::new([text_answer("ok")]);
    let (mut agent, _) = durable(&p2, &store, &tools, None);
    agent.replace_history(from_file);

    agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(p2.requests()[0].messages.len(), 5, "nothing twice");
}

fn replay_messages(messages: &[Message]) -> Vec<Message> {
    let lines: Vec<String> = messages
        .iter()
        .map(|m| {
            serde_json::to_string(&SessionRecord::Message {
                t: 1,
                message: m.clone(),
            })
            .unwrap()
        })
        .collect();
    replay(&lines.join("\n")).messages
}

#[tokio::test]
async fn a_history_the_run_did_not_start_on_is_refused() {
    let (store, key, tools, _) = killed_on_prior_history().await;
    let p2 = ScriptedProvider::new([text_answer("ok")]);
    let (mut agent, _) = durable(&p2, &store, &tools, None);

    let refused = agent.run_durable(&key, "go", &mut no_events()).await;

    assert!(matches!(
        refused,
        Err(RunError::HistoryMismatch {
            base: 2,
            found: 0,
            ..
        })
    ));
    assert!(p2.requests().is_empty());
}

/// A store that cannot read back any run.
struct Unreadable;

#[async_trait]
impl crate::run::RunStore for Unreadable {
    fn sink(&self, key: &RunKey) -> crate::session::SessionSinkRef {
        crate::run::RunStore::sink(&MemoryRunStore::new(), key)
    }
    async fn load(&self, _key: &RunKey) -> std::io::Result<Option<crate::run::ReplayedRun>> {
        Err(std::io::Error::other("the disk is gone"))
    }
}

#[tokio::test]
async fn a_store_that_cannot_load_the_run_refuses_it_and_says_why_once() {
    let p = ScriptedProvider::new([text_answer("ok")]);
    let (mut agent, _) = durable(&p, &MemoryRunStore::new(), &[], None);
    agent.map_deps(|d| d.with_run_store(Unreadable));

    let refused = agent
        .run_durable(&RunKey::new("job"), "go", &mut no_events())
        .await
        .unwrap_err();

    assert!(matches!(refused, RunError::Store { .. }));
    assert!(crate::error::report(&refused).contains("the disk is gone"));
    crate::testing::assert_causes_said_once(&refused);
    assert!(p.requests().is_empty());
}

#[tokio::test]
async fn an_answer_that_landed_without_its_end_ends_the_run_without_a_model_call() {
    let store = MemoryRunStore::new();
    let key = RunKey::new("job");
    let p = ScriptedProvider::new([text_answer("done")]);
    let (mut agent, _) = durable(&p, &store, &[], None);
    agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();
    // The run_end never landed.
    let store = torn(&store, &key, 1);

    let p2 = ScriptedProvider::new([]);
    let (mut agent, _) = durable(&p2, &store, &[], None);
    let resumed = agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(resumed.served, Served::Resumed);
    assert_eq!(
        (resumed.stop_reason, resumed.text.as_str()),
        (StopReason::EndTurn, "done")
    );
    assert!(p2.requests().is_empty());
    assert!(
        crate::run::replay_run(&store.lines(&key).join("\n"))
            .unwrap()
            .end
            .is_some()
    );
}

#[tokio::test]
async fn a_start_whose_input_never_landed_runs_the_turn() {
    let store = MemoryRunStore::new();
    let key = RunKey::new("job");
    let p = ScriptedProvider::new([text_answer("done")]);
    let (mut agent, _) = durable(&p, &store, &[], None);
    agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();
    // Only the run record landed.
    let all = store.lines(&key).len();
    let store = torn(&store, &key, all - 1);

    let p2 = ScriptedProvider::new([text_answer("done again")]);
    let (mut agent, _) = durable(&p2, &store, &[], None);
    let resumed = agent
        .run_durable(&key, "go", &mut no_events())
        .await
        .unwrap();

    assert_eq!(resumed.text, "done again");
    assert_eq!(p2.requests().len(), 1);
    assert_eq!(p2.requests()[0].messages, [Message::user("go")]);
}

#[tokio::test]
async fn a_fresh_run_on_a_history_a_dropped_turn_left_answers_its_calls_first() {
    let (store, key, log) = (MemoryRunStore::new(), RunKey::new("job"), Log::default());
    let hung = Arc::new(Notify::new());
    let tools = vec![Probe::new("Slow", false, &log).hangs(&hung).arc()];
    let p = ScriptedProvider::new([tool_answer(&[("c1", "Slow", json!({}))]), text_answer("ok")]);
    let (mut agent, hooks) = durable(&p, &store, &tools, None);
    // A plain turn the host dropped mid-tool.
    kill_when(&hung, agent.run("go", &mut no_events())).await;

    let fresh = agent
        .run_durable(&key, "again", &mut no_events())
        .await
        .unwrap();

    assert_eq!(
        (fresh.served, fresh.stop_reason),
        (Served::Fresh, StopReason::EndTurn)
    );
    let sent = &p.requests()[1].messages;
    assert!(
        matches!(&sent[2], Message::Tool(r) if r.tool_call_id == "c1" && r.content == crate::session::INTERRUPTED),
        "{sent:?}"
    );
    // The answer is the history the run started on, not one of its messages.
    let run = crate::run::replay_run(&store.lines(&key).join("\n")).unwrap();
    assert_eq!(run.base, 3);
    assert_eq!(*hooks.persisted.lock().unwrap(), agent.history());
}
