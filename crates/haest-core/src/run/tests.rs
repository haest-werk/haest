//! A durable run's records: their wire shape, what replay makes of them, and
//! that a reader that does not know them still reads the conversation.

use serde_json::{Value, json};

use super::*;
use crate::{
    agent::test_support::*,
    messages::{Message, StopReason, Usage},
    session::{SessionRecord, replay},
    testing::{ScriptedProvider, text_answer, tool_answer},
};

/// The six kinds every haest reader knew before durable runs.
const OLD_KINDS: [&str; 6] = [
    "header",
    "message",
    "compaction",
    "config",
    "ledger",
    "tasks",
];

fn kind(line: &str) -> String {
    let v: Value = serde_json::from_str(line).unwrap();
    v["type"].as_str().unwrap().to_string()
}

/// A finished durable run with one tool call, as its store keeps it.
async fn finished_run_lines() -> Vec<String> {
    let store = MemoryRunStore::new();
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Echo", json!({"v": 1}))]),
        text_answer("done"),
    ]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    agent.map_deps(|d| d.with_run_store(store.clone()));
    let key = RunKey::new("job");
    agent
        .run_durable(&key, "go", &mut Vec::new())
        .await
        .unwrap();
    store.lines(&key)
}

#[test]
fn durable_records_keep_the_typescript_shape() {
    let run = SessionRecord::Run {
        t: 1,
        key: RunKey::new("job"),
        input_hash: "ab".into(),
        base: 2,
    };
    let v = serde_json::to_value(&run).unwrap();
    assert_eq!(
        v,
        json!({"type": "run", "t": 1, "key": "job", "inputHash": "ab", "base": 2})
    );

    let turn = SessionRecord::Turn {
        t: 2,
        state: TurnState {
            steps: 3,
            elapsed_ms: 40,
            ..Default::default()
        },
    };
    let v = serde_json::to_value(&turn).unwrap();
    assert_eq!(
        (v["type"].clone(), v["steps"].clone()),
        (json!("turn"), json!(3))
    );
    assert_eq!(v["elapsedMs"], json!(40));

    let end = SessionRecord::RunEnd {
        t: 3,
        key: RunKey::new("job"),
        end: RunEnd {
            stop_reason: StopReason::EndTurn,
            error: None,
            text: "hi".into(),
            usage: Usage::default(),
        },
    };
    let v = serde_json::to_value(&end).unwrap();
    assert_eq!(
        (v["type"].clone(), v["stopReason"].clone()),
        (json!("run_end"), json!("end_turn"))
    );

    for r in [run, turn, end] {
        let line = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<SessionRecord>(&line).unwrap(), r);
    }
}

#[tokio::test]
async fn a_reader_that_knows_only_the_old_kinds_reads_a_durable_log_unchanged() {
    let lines = finished_run_lines().await;
    let new_kinds: Vec<String> = lines
        .iter()
        .map(|l| kind(l))
        .filter(|k| !OLD_KINDS.contains(&k.as_str()))
        .collect();
    // Durable runs add kinds; they change no old kind's record.
    assert!(
        new_kinds
            .iter()
            .all(|k| ["run", "turn", "run_end"].contains(&k.as_str()))
    );
    assert!(new_kinds.contains(&"run".to_string()) && new_kinds.contains(&"run_end".to_string()));

    let old_only: Vec<&String> = lines
        .iter()
        .filter(|l| OLD_KINDS.contains(&kind(l).as_str()))
        .collect();
    let old_text = old_only
        .iter()
        .map(|l| l.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let full = replay(&lines.join("\n"));
    let old = replay(&old_text);
    assert_eq!(full.messages, old.messages);
    assert_eq!(full.corrupt_lines, 0);
    assert_eq!(
        full.messages.len(),
        4,
        "the input, the call, its result, the answer"
    );
}

#[test]
fn a_record_kind_this_haest_does_not_know_is_skipped_by_replay_run() {
    let log = [
        r#"{"type":"run","t":1,"key":"job","inputHash":"x"}"#,
        r#"{"type":"run_step_v9","t":2,"anything":true}"#,
        r#"{"type":"message","t":3,"message":{"role":"user","content":"hi"}}"#,
    ]
    .join("\n");
    let run = replay_run(&log).unwrap();
    assert_eq!(run.messages, [Message::user("hi")]);
    assert_eq!((run.corrupt_lines, run.base), (0, 0));
}

fn log_of(records: &[SessionRecord]) -> String {
    let lines: Vec<String> = records
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect();
    lines.join("\n")
}

fn msg(message: Message) -> SessionRecord {
    SessionRecord::Message { t: 1, message }
}

fn start(key: &str) -> SessionRecord {
    SessionRecord::Run {
        t: 1,
        key: RunKey::new(key),
        input_hash: input_hash("go"),
        base: 0,
    }
}

fn result(id: &str) -> Message {
    Message::Tool(crate::messages::ToolResultMessage {
        tool_call_id: id.into(),
        tool_name: "Echo".into(),
        content: "ok".into(),
        is_error: false,
        media: Vec::new(),
    })
}

#[test]
fn the_last_answers_calls_without_a_result_stay_pending() {
    let calls = tool_answer(&[
        ("c1", "Echo", json!({})),
        ("c2", "Echo", json!({})),
        ("c3", "Echo", json!({})),
    ]);
    let log = log_of(&[
        start("job"),
        msg(Message::user("go")),
        msg(Message::Assistant(calls.clone())),
        msg(result("c1")),
    ]);
    let run = replay_run(&log).unwrap();
    let pending: Vec<&str> = run.pending.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(pending, ["c2", "c3"]);
    // Left unanswered for the resume to answer: no interrupted results.
    assert_eq!(run.messages.len(), 3);
    assert_eq!(run.messages.last(), Some(&result("c1")));
}

#[test]
fn an_earlier_call_without_a_result_is_answered_as_interrupted() {
    let log = log_of(&[
        start("job"),
        msg(Message::user("go")),
        msg(Message::Assistant(tool_answer(&[(
            "c1",
            "Echo",
            json!({}),
        )]))),
        msg(Message::user("steered")),
    ]);
    let run = replay_run(&log).unwrap();
    assert!(run.pending.is_empty());
    assert!(matches!(&run.messages[2], Message::Tool(r) if r.tool_call_id == "c1" && r.is_error));
}

#[test]
fn a_later_run_record_starts_the_run_over() {
    let log = log_of(&[
        start("job"),
        msg(Message::user("torn start")),
        start("job"),
        msg(Message::user("go")),
    ]);
    assert_eq!(replay_run(&log).unwrap().messages, [Message::user("go")]);
}

#[test]
fn a_log_without_a_run_record_is_no_run() {
    let log = log_of(&[msg(Message::user("go"))]);
    assert!(replay_run(&log).is_none());
}

/// Pinned: an effect key already handed to an outside API, or an input hash
/// already written, must read the same after any change to haest.
#[test]
fn effect_keys_and_input_hashes_are_the_same_in_every_build() {
    let key = RunKey::new("job");
    assert_eq!(
        key.effect_key("c1").as_str(),
        "61f4988879105959ad3a5337cc689a82"
    );
    assert_eq!(input_hash("go"), "9a5001411505e8620841e749cb0f15c7");
    assert_ne!(key.effect_key("c1"), key.effect_key("c2"));
    assert_ne!(key.effect_key("c1"), key.child("c1").effect_key("c1"));
}

#[tokio::test]
async fn the_memory_store_gives_back_what_its_sink_took() {
    let store = MemoryRunStore::new();
    let key = RunKey::new("job");
    assert!(store.load(&key).await.unwrap().is_none());
    let sink = store.sink(&key);
    sink.append(&start("job"));
    sink.append(&msg(Message::user("go")));
    let run = store.load(&key).await.unwrap().unwrap();
    assert_eq!(
        (run.key, run.messages),
        (key.clone(), vec![Message::user("go")])
    );
    assert_eq!(store.keys(), [key]);
}
