//! The dialects without a transport: what exit 2 is per event, what a
//! haest answer that is no decision does, and each Claude Code answer
//! haest cannot take.

use std::path::Path;

use haest_core::{ToolOutput, testing::text_answer};
use haest_mods::{
    Before, ModelAfterDecision, ModelAfterPayload, ModelBeforeDecision, ModelBeforePayload,
    PromptSubmitPayload, SessionView, SystemPromptPayload, ToolAfterPayload, ToolBeforePayload,
    TurnEndDecision, TurnEndPayload,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::*;

fn payload<E: DeserializeOwned>(v: Value) -> E {
    serde_json::from_value(v).unwrap()
}

fn tool_before() -> ToolBeforePayload {
    payload(json!({
        "call": { "name": "Bash", "input": { "command": "ls" }, "readOnly": false },
        "ctx": { "cwd": "/p", "toolCallId": "t1" },
    }))
}

fn tool_after() -> ToolAfterPayload {
    payload(json!({
        "call": { "name": "Bash", "input": { "command": "ls" }, "readOnly": false },
        "output": { "content": "a.txt", "title": "ls", "isError": false },
        "ctx": { "cwd": "/p" },
    }))
}

fn turn_end(continued: bool) -> TurnEndPayload {
    payload(json!({
        "modelSpec": "scripted:m",
        "historyLength": 2,
        "stopReason": "end_turn",
        "answer": serde_json::to_value(text_answer("All done.")).unwrap(),
        "continued": continued,
    }))
}

fn model_view() -> Value {
    json!({ "modelSpec": "scripted:m", "historyLength": 2 })
}

fn answer(text: &str) -> Reply {
    Reply::Answer(text.into())
}

fn exit2(stderr: &str) -> Reply {
    Reply::Blocked(stderr.into())
}

/// The failure's words, or a panic when there is a decision.
fn failed<T: std::fmt::Debug>(got: Result<T, HookFailure>) -> String {
    match got {
        Ok(decision) => panic!("expected a failure, got {decision:?}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn exit_2_refuses_a_call_stops_a_model_turn_and_keeps_a_turn_going_with_stderr() {
    let d = Haest;
    assert_eq!(
        d.decide(tool_before(), exit2("no rm -rf\n")).unwrap(),
        Before::Deny("no rm -rf".into())
    );
    let before: ModelBeforePayload = payload(model_view());
    assert_eq!(
        d.decide(before, exit2("halt")).unwrap(),
        ModelBeforeDecision::default().with_stop("halt")
    );
    let mut after = model_view();
    after["answer"] = json!({ "content": [], "stopReason": "end_turn" });
    let after: ModelAfterPayload = payload(after);
    assert_eq!(
        d.decide(after, exit2("halt")).unwrap(),
        ModelAfterDecision::default().with_stop("halt")
    );
    assert_eq!(
        d.decide(turn_end(false), exit2("run the tests")).unwrap(),
        TurnEndDecision::default().with_continue("run the tests")
    );
    // Said nothing: a reason all the same.
    assert_eq!(
        d.decide(tool_before(), exit2("  ")).unwrap(),
        Before::Deny(NO_REASON.into())
    );
}

#[test]
fn exit_2_hands_the_model_no_more_of_stderr_than_of_a_tools_output() {
    // A turn kept going with a megabyte of stderr would be a megabyte-long message.
    let flood = "x".repeat(1_000_000);
    let kept = Haest.decide(turn_end(false), exit2(&flood)).unwrap();
    let Value::String(text) = serde_json::to_value(kept).unwrap()["continue"].take() else {
        panic!("a continue");
    };
    assert!(
        text.chars().count() <= haest_core::tool::TRUNCATE_CHARS + 200,
        "{}",
        text.len()
    );
}

#[test]
fn exit_2_on_an_event_with_no_blocking_answer_is_a_failure() {
    let d = Haest;
    let no = failed(d.decide(tool_after(), exit2("nope")));
    assert!(
        no.contains("tool.after has no blocking answer") && no.contains("nope"),
        "{no}"
    );
    failed(d.decide(PromptSubmitPayload::new("hi"), exit2("nope")));
    failed(d.decide(SystemPromptPayload::new(), exit2("nope")));
}

#[test]
fn a_haest_answer_that_is_empty_passes_and_one_that_is_no_decision_fails() {
    let d = Haest;
    assert_eq!(
        d.decide(tool_before(), answer(" \n")).unwrap(),
        Before::Pass
    );
    assert_eq!(
        d.decide(tool_before(), answer(r#"{"deny":"no"}"#)).unwrap(),
        Before::Deny("no".into())
    );
    let garbage = failed(d.decide(tool_before(), answer("ok!")));
    assert!(
        garbage.starts_with("tool.before: the hook's answer is not a decision"),
        "{garbage}"
    );
    failed(d.decide(tool_before(), answer(r#"{"input":"not an object"}"#)));
    failed(d.decide(PromptSubmitPayload::new("hi"), answer("unquoted text")));
}

#[test]
fn the_haest_input_is_the_event_name_and_the_payload_and_nothing_in_the_environment() {
    let out = outgoing(&Haest, &tool_before(), None).unwrap();
    // A tool event runs where the call does, unless the host said.
    assert_eq!(out.dir, Path::new("/p"));
    let input: Value = serde_json::from_slice(&out.body).unwrap();
    let mut expected = serde_json::to_value(tool_before()).unwrap();
    expected["event"] = json!("tool.before");
    assert_eq!(input, expected);
    assert!(Haest.env(&out.dir).vars().is_empty());

    let out = outgoing(
        &Haest,
        &PromptSubmitPayload::new("hi"),
        Some(Path::new("/c")),
    )
    .unwrap();
    assert_eq!(out.dir, Path::new("/c"));
}

#[test]
fn the_claude_input_leaves_out_what_the_host_did_not_give_and_names_the_project_dir() {
    let out = outgoing(&Claude, &tool_before(), Some(Path::new("/c"))).unwrap();
    let input: Value = serde_json::from_slice(&out.body).unwrap();
    assert_eq!(
        input,
        json!({
            "cwd": "/c",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": "ls" },
            "tool_use_id": "t1",
        })
    );
    assert_eq!(
        Claude.env(Path::new("/c")).get("CLAUDE_PROJECT_DIR"),
        Some("/c")
    );

    // The payload's session: its id, and no transcript path the host keeps none of.
    let mut named = tool_before();
    named.session = Some(SessionView::new("s-1"));
    let out = outgoing(&Claude, &named, Some(Path::new("/c"))).unwrap();
    let input: Value = serde_json::from_slice(&out.body).unwrap();
    assert_eq!(input["session_id"], "s-1");
    assert!(input.get("transcript_path").is_none(), "{input}");

    let stop = |continued| {
        let out = outgoing(&Claude, &turn_end(continued), Some(Path::new("/c"))).unwrap();
        serde_json::from_slice::<Value>(&out.body).unwrap()
    };
    assert_eq!(stop(false)["stop_hook_active"], false);
    assert_eq!(stop(true)["stop_hook_active"], true);
    assert_eq!(stop(true)["last_assistant_message"], "All done.");
}

#[test]
fn a_claude_answer_haest_cannot_take_is_a_failure() {
    let c = Claude;
    for (text, says) in [
        ("{not json", "not a decision"),
        (
            r#"{"hookSpecificOutput":"deny"}"#,
            "`hookSpecificOutput` must be an object",
        ),
        (
            r#"{"hookSpecificOutput":{"permissionDecision":"maybe"}}"#,
            "allow, deny, ask or defer",
        ),
        (
            r#"{"hookSpecificOutput":{"updatedInput":"rm"}}"#,
            "`updatedInput` must be an object",
        ),
        (r#"{"decision":"veto"}"#, "\"block\" or \"approve\""),
        (r#"{"decision":true}"#, "`decision` must be text"),
    ] {
        let why = failed(c.decide(tool_before(), answer(text)));
        assert!(why.contains(says), "{text}: {why}");
    }
    let why = failed(c.decide(turn_end(false), answer(r#"{"decision":"block"}"#)));
    assert!(why.contains("needs a `reason`"), "{why}");
    let why = failed(c.decide(tool_after(), exit2("too late")));
    assert!(why.contains("tool.after has no blocking answer"), "{why}");
}

#[test]
fn claude_answers_haest_has_no_decision_for_are_ignored() {
    let c = Claude;
    let ignored =
        r#"{"continue":false,"stopReason":"x","systemMessage":"y","suppressOutput":true}"#;
    assert_eq!(
        c.decide(tool_before(), answer(ignored)).unwrap(),
        Before::Pass
    );
    assert_eq!(c.decide(tool_after(), answer(ignored)).unwrap(), None);
    assert_eq!(
        c.decide(PromptSubmitPayload::new("hi"), answer(ignored))
            .unwrap(),
        None
    );
    assert_eq!(
        c.decide(turn_end(false), answer(ignored)).unwrap(),
        TurnEndDecision::default()
    );
    // PreToolUse's `additionalContext` has no place in a `tool.before` decision.
    let context =
        r#"{"hookSpecificOutput":{"permissionDecision":"allow","additionalContext":"x"}}"#;
    assert_eq!(
        c.decide(tool_before(), answer(context)).unwrap(),
        Before::Pass
    );
    // `approve` is the older `allow`.
    assert_eq!(
        c.decide(tool_before(), answer(r#"{"decision":"approve"}"#))
            .unwrap(),
        Before::Pass
    );
}

#[test]
fn a_claude_tool_output_replacement_is_a_new_output_and_an_object_is_its_json() {
    let c = Claude;
    let replaced = c
        .decide(
            tool_after(),
            answer(r#"{"hookSpecificOutput":{"updatedToolOutput":{"files":["b.txt"]}}}"#),
        )
        .unwrap()
        .unwrap();
    assert_eq!(replaced.content, r#"{"files":["b.txt"]}"#);
    // What described the old output went with it.
    assert_eq!(replaced.title, None);

    // A reason and context with a replacement: after the new output.
    let both = r#"{"decision":"block","reason":"r","hookSpecificOutput":{"updatedToolOutput":"new","additionalContext":"c"}}"#;
    let out: ToolOutput = c.decide(tool_after(), answer(both)).unwrap().unwrap();
    assert_eq!(out.content, "new\n\nr\n\nc");
}

#[test]
fn a_claude_deny_without_a_reason_still_gives_one() {
    let c = Claude;
    let reason = |text: &str| match c.decide(tool_before(), answer(text)).unwrap() {
        Before::Deny(reason) => reason,
        other => panic!("{text}: expected a deny, got {other:?}"),
    };
    assert_eq!(
        reason(r#"{"hookSpecificOutput":{"permissionDecision":"deny"}}"#),
        "a hook denied this call"
    );
    assert!(
        reason(r#"{"hookSpecificOutput":{"permissionDecision":"ask"}}"#).contains("cannot get")
    );
    // A deny wins over an input rewrite in the same answer.
    assert_eq!(
        reason(
            r#"{"hookSpecificOutput":{"permissionDecision":"deny","updatedInput":{"command":"ls"}}}"#
        ),
        "a hook denied this call"
    );
}
