//! A call, a result, a task list and a stop reason, as the client is told
//! them.

use std::path::Path;

use agent_client_protocol::v1::schema::{
    PlanEntryStatus, StopReason as AcpStop, ToolCallContent, ToolCallStatus, ToolKind,
};
use haest_core::{JsonMap, StopReason, ToolCall, remedy::Limit, task_board::Task};
use serde_json::{Value, json};

use super::*;

fn input(v: Value) -> JsonMap {
    v.as_object().unwrap().clone()
}

fn call(name: &str, v: Value) -> ToolCall {
    ToolCall {
        id: "call-1".into(),
        name: name.into(),
        input: input(v),
    }
}

#[test]
fn each_kind_of_tool_is_drawn_as_its_protocol_kind() {
    let kinds = [
        ("file.read", ToolKind::Read),
        ("file.edit", ToolKind::Edit),
        ("file.write", ToolKind::Edit),
        ("file.glob", ToolKind::Search),
        ("text.search", ToolKind::Search),
        ("shell.exec", ToolKind::Execute),
        ("web.fetch", ToolKind::Fetch),
        ("task.spawn", ToolKind::Think),
        ("mcp.call", ToolKind::Other),
    ];
    for (kind, acp) in kinds {
        assert_eq!(tool_kind(kind), acp, "{kind}");
    }
}

#[test]
fn a_title_says_what_the_call_does_and_falls_back_to_the_tools_name() {
    let bash = input(json!({"command": "cargo test"}));
    assert_eq!(title("Bash", "shell.exec", &bash), "cargo test");
    let read = input(json!({"file_path": "src/lib.rs"}));
    assert_eq!(title("Read", "file.read", &read), "Read src/lib.rs");
    assert_eq!(title("Bash", "shell.exec", &input(json!({}))), "Bash");
    assert_eq!(title("mcp__x__y", "mcp.call", &bash), "mcp__x__y");
}

#[test]
fn an_edit_carries_its_file_and_the_text_it_replaces() {
    let edit = call(
        "Edit",
        json!({"file_path": "src/a.rs", "old_string": "x", "new_string": "y"}),
    );
    let entry = new_call(&edit, "file.edit", Path::new("/w"), ToolCallStatus::Pending);
    assert_eq!(entry.kind, ToolKind::Edit);
    assert_eq!(entry.title, "Edit src/a.rs");
    assert_eq!(entry.status, ToolCallStatus::Pending);
    assert_eq!(entry.locations[0].path, Path::new("/w/src/a.rs"));
    let [ToolCallContent::Diff(diff)] = entry.content.as_slice() else {
        panic!("one diff: {:?}", entry.content);
    };
    assert_eq!(diff.path, Path::new("/w/src/a.rs"));
    assert_eq!(
        (diff.old_text.as_deref(), diff.new_text.as_str()),
        (Some("x"), "y")
    );
}

#[test]
fn a_read_is_located_at_the_line_it_starts_from_and_a_shell_call_nowhere() {
    let read = input(json!({"file_path": "/abs/a.rs", "offset": 40}));
    let at = locations("file.read", &read, Path::new("/w"));
    assert_eq!(
        (at[0].path.as_path(), at[0].line),
        (Path::new("/abs/a.rs"), Some(40))
    );
    let bash = input(json!({"command": "ls"}));
    assert!(locations("shell.exec", &bash, Path::new("/w")).is_empty());
}

#[test]
fn a_result_is_completed_or_failed_and_an_edit_that_went_through_keeps_its_diff() {
    let done = result_fields("shell.exec", false, "ok", None, None);
    assert_eq!(done.status, Some(ToolCallStatus::Completed));
    assert_eq!(
        done.content,
        Some(vec![ToolCallContent::from("ok".to_string())])
    );

    let failed = result_fields("file.edit", true, "no match", None, None);
    assert_eq!(failed.status, Some(ToolCallStatus::Failed));
    assert!(failed.content.is_some(), "a failed edit says why");

    let edited = result_fields("file.edit", false, "edited", Some(&json!({"n": 1})), None);
    assert_eq!(edited.content, None, "the diff the call carried stays");
    assert_eq!(edited.raw_output, Some(json!({"n": 1})));
}

#[test]
fn the_task_list_is_a_plan_in_order() {
    let task = |id: &str, status: &str| -> Task {
        serde_json::from_value(json!({
            "id": id, "subject": format!("task {id}"), "status": status, "updatedAt": 0
        }))
        .unwrap()
    };
    let tasks = [
        task("1", "completed"),
        task("2", "in_progress"),
        task("3", "pending"),
    ];
    let plan = plan(&tasks);
    let shown: Vec<(&str, PlanEntryStatus)> = plan
        .entries
        .iter()
        .map(|e| (e.content.as_str(), e.status.clone()))
        .collect();
    assert_eq!(
        shown,
        [
            ("task 1", PlanEntryStatus::Completed),
            ("task 2", PlanEntryStatus::InProgress),
            ("task 3", PlanEntryStatus::Pending),
        ]
    );
}

#[test]
fn an_interrupted_turn_is_cancelled_the_steps_cap_is_max_turn_requests_and_an_error_is_none() {
    assert_eq!(
        stop_reason(StopReason::EndTurn, None),
        Some(AcpStop::EndTurn)
    );
    assert_eq!(
        stop_reason(StopReason::Aborted, None),
        Some(AcpStop::Cancelled)
    );
    assert_eq!(
        stop_reason(StopReason::Refusal, None),
        Some(AcpStop::Refusal)
    );
    assert_eq!(
        stop_reason(StopReason::MaxTokens, None),
        Some(AcpStop::MaxTokens)
    );
    let steps = Limit::TurnSteps(50);
    assert_eq!(
        stop_reason(StopReason::MaxTokens, Some(&steps)),
        Some(AcpStop::MaxTurnRequests)
    );
    assert_eq!(stop_reason(StopReason::Error, None), None);
}
