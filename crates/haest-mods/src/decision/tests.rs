//! Every decision reads exactly what a mod file's handler may return, and
//! writes the same words back.

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::{Compact, ModelAfterDecision, ModelBeforeDecision, TurnEndDecision};
use crate::{api::Before, testkit::input};

fn read<T: DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{v} does not read: {e}"))
}

fn refused<T: DeserializeOwned + std::fmt::Debug>(v: Value) {
    let got = serde_json::from_value::<T>(v.clone());
    assert!(got.is_err(), "{v} must not read: {got:?}");
}

/// `d` writes as `json`, and reads back as itself.
fn round_trip<T>(d: &T, json: Value)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(serde_json::to_value(d).unwrap(), json);
    assert_eq!(&read::<T>(json), d);
}

/// What any handler may return that is not an answer: it passes.
fn not_answers() -> [Value; 7] {
    [
        Value::Null,
        json!("ok"),
        json!(42),
        json!(true),
        json!(["deny"]),
        json!({}),
        json!({ "other": 1 }),
    ]
}

#[test]
fn tool_before_reads_deny_and_input_and_passes_anything_else() {
    assert_eq!(
        read::<Before>(json!({ "deny": "no" })),
        Before::Deny("no".into())
    );
    let both = json!({ "deny": "no", "input": { "v": 1 } });
    assert_eq!(read::<Before>(both), Before::Deny("no".into()), "deny wins");
    let rewritten = read::<Before>(json!({ "input": { "v": 1 } }));
    assert_eq!(rewritten, Before::Input(input(json!({ "v": 1 }))));
    for passes in not_answers() {
        assert_eq!(read::<Before>(passes), Before::Pass);
    }
    assert_eq!(read::<Before>(json!({ "deny": null })), Before::Pass);
    for bad in [
        json!({ "input": 5 }),
        json!({ "input": "v" }),
        json!({ "deny": 5 }),
    ] {
        refused::<Before>(bad);
    }
}

#[test]
fn tool_before_writes_what_it_reads() {
    round_trip(&Before::Pass, Value::Null);
    round_trip(&Before::Deny("no".into()), json!({ "deny": "no" }));
    let input = input(json!({ "v": "x" }));
    round_trip(&Before::Input(input), json!({ "input": { "v": "x" } }));
}

#[test]
fn model_before_reads_compact_steer_and_stop_and_ignores_what_it_does_not_take() {
    let all = json!({ "compact": { "instructions": "keep paths" }, "steer": "s", "stop": "x", "continue": "c" });
    let read_all: ModelBeforeDecision = read(all);
    assert_eq!(
        read_all,
        ModelBeforeDecision::default()
            .with_compact(Compact::new().with_instructions("keep paths"))
            .with_steer("s")
            .with_stop("x")
    );
    let bare = read::<ModelBeforeDecision>(json!({ "compact": true }));
    assert_eq!(bare.compact, Some(Compact::new()));
    for none in [json!({ "compact": false }), json!({ "compact": null })] {
        assert_eq!(
            read::<ModelBeforeDecision>(none),
            ModelBeforeDecision::default()
        );
    }
    let empty = read::<ModelBeforeDecision>(json!({ "compact": {} }));
    assert_eq!(empty.compact, Some(Compact::new()));
    for passes in not_answers() {
        assert_eq!(
            read::<ModelBeforeDecision>(passes),
            ModelBeforeDecision::default()
        );
    }
    for bad in [
        json!({ "stop": 5 }),
        json!({ "steer": {} }),
        json!({ "compact": 1 }),
    ] {
        refused::<ModelBeforeDecision>(bad);
    }
}

#[test]
fn model_after_takes_no_compaction() {
    let got = read::<ModelAfterDecision>(json!({ "compact": true, "steer": "s", "stop": "x" }));
    assert_eq!(
        got,
        ModelAfterDecision::default().with_steer("s").with_stop("x")
    );
    for passes in not_answers() {
        assert_eq!(
            read::<ModelAfterDecision>(passes),
            ModelAfterDecision::default()
        );
    }
}

#[test]
fn turn_end_reads_continue_and_compact_and_takes_no_stop() {
    let got = read::<TurnEndDecision>(json!({ "continue": "go on", "compact": true, "stop": "x" }));
    assert_eq!(
        got,
        TurnEndDecision::default()
            .with_continue("go on")
            .with_compact(Compact::new())
    );
    for passes in not_answers() {
        assert_eq!(read::<TurnEndDecision>(passes), TurnEndDecision::default());
    }
    refused::<TurnEndDecision>(json!({ "continue": 1 }));
}

#[test]
fn loop_decisions_write_what_they_read() {
    round_trip(&ModelBeforeDecision::default(), json!({}));
    round_trip(
        &ModelBeforeDecision::default()
            .with_compact(Compact::new())
            .with_steer("s")
            .with_stop("x"),
        json!({ "compact": true, "steer": "s", "stop": "x" }),
    );
    round_trip(
        &ModelAfterDecision::default().with_steer("s"),
        json!({ "steer": "s" }),
    );
    round_trip(
        &TurnEndDecision::default()
            .with_compact(Compact::new().with_instructions("k"))
            .with_continue("go"),
        json!({ "compact": { "instructions": "k" }, "continue": "go" }),
    );
}

#[test]
fn the_text_events_read_text_or_nothing() {
    assert_eq!(read::<Option<String>>(json!("new")), Some("new".into()));
    assert_eq!(read::<Option<String>>(Value::Null), None);
    refused::<Option<String>>(json!({ "text": "new" }));
    let output = read::<Option<haest_core::ToolOutput>>(json!({ "content": "x" }));
    assert_eq!(
        output.map(|o| (o.content, o.is_error)),
        Some(("x".into(), false))
    );
    refused::<Option<haest_core::ToolOutput>>(json!("x"));
}
