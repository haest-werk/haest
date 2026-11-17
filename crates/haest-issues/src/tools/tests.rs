//! The tools over a board in memory (and a scripted GitHub where GitHub differs).

use haest_core::{ToolRef, Tools, fs::MemFs};
use serde_json::json;

use super::*;
use crate::{
    cards::CardTracker,
    testkit::{FakeHttp, ctx, github, input, issue, ok},
};

fn board() -> (CardTracker, Tools) {
    let tracker = CardTracker::new("/board", MemFs::new()).with_clock(|| 0);
    (tracker.clone(), issue_tools(tracker))
}

fn tool(tools: &[ToolRef], name: &str) -> ToolRef {
    tools.iter().find(|t| t.name() == name).cloned().unwrap()
}

async fn call(tools: &[ToolRef], name: &str, args: Value) -> Result<String, ToolError> {
    let out = tool(tools, name).execute(input(args), &ctx()).await?;
    assert!(!out.is_error);
    Ok(out.content)
}

#[test]
fn seven_tools_and_only_delete_has_the_destructive_kind() {
    let (_, tools) = board();
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(
        names,
        [
            "IssueList",
            "IssueSearch",
            "IssueGet",
            "IssueCreate",
            "IssueUpdate",
            "IssueDelete",
            "IssueLink"
        ]
    );
    let read_only: Vec<&str> = (tools.iter().filter(|t| t.read_only()))
        .map(|t| t.name())
        .collect();
    assert_eq!(read_only, ["IssueList", "IssueSearch", "IssueGet"]);
    let destructive: Vec<&str> = (tools.iter().filter(|t| t.kind() == DELETE_KIND))
        .map(|t| t.name())
        .collect();
    assert_eq!(destructive, ["IssueDelete"]);
    let cards = issue_tools_named("Card", CardTracker::new("/b", MemFs::new()));
    assert_eq!(cards[0].name(), "CardList");
    assert_eq!(cards[5].kind(), DELETE_KIND);
}

#[test]
fn the_schemas_offer_what_each_tracker_accepts() {
    let (_, tools) = board();
    let list = tool(&tools, "IssueList").parameters();
    let statuses = list["properties"]["status"]["enum"].as_array().unwrap();
    assert!(statuses.contains(&json!("in-review")) && statuses.contains(&json!("all")));
    assert_eq!(
        list["properties"]["priority"]["enum"],
        json!(["low", "medium", "high"])
    );
    let create = tool(&tools, "IssueCreate").parameters();
    assert!(
        !create["properties"]["status"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("all"))
    );

    let gh = issue_tools(github(FakeHttp::new([])));
    let create = tool(&gh, "IssueCreate").parameters();
    assert!(create["properties"].get("priority").is_none());
    assert_eq!(
        create["properties"]["status"]["enum"],
        json!(["open", "closed", "not_planned"])
    );
    let link = tool(&gh, "IssueLink").parameters();
    assert_eq!(
        link["properties"]["relation"]["enum"],
        json!(["parent", "child", "depends_on", "blocks"])
    );
}

#[tokio::test]
async fn a_board_worked_through_the_tools() {
    let (tracker, tools) = board();
    call(
        &tools,
        "IssueCreate",
        json!({"title": "Epic one", "labels": "plan, q4"}),
    )
    .await
    .unwrap();
    let made = call(
        &tools,
        "IssueCreate",
        json!({"title": "Write the parser", "priority": "high", "body": "Parse it."}),
    )
    .await
    .unwrap();
    assert!(made.contains("write-the-parser"));
    call(
        &tools,
        "IssueLink",
        json!({"id": "write-the-parser", "relation": "parent", "target": "epic-one"}),
    )
    .await
    .unwrap();
    call(
        &tools,
        "IssueUpdate",
        json!({"id": "write-the-parser", "status": "in-progress", "add_labels": ["parser"]}),
    )
    .await
    .unwrap();

    let got = tracker.get("write-the-parser").await.unwrap();
    assert_eq!(got.status, "in-progress");
    assert_eq!(got.labels, ["parser"]);
    assert_eq!(
        got.links,
        [crate::Link::new(crate::Relation::Parent, "epic-one")]
    );
    let epic = call(&tools, "IssueGet", json!({"id": "epic-one"}))
        .await
        .unwrap();
    assert!(
        epic.contains("write-the-parser"),
        "the epic shows its child: {epic}"
    );
    let listed = call(&tools, "IssueList", json!({"labels": ["q4"]}))
        .await
        .unwrap();
    assert!(listed.contains("epic-one") && !listed.contains("write-the-parser"));
    let found = call(&tools, "IssueSearch", json!({"query": "parse"}))
        .await
        .unwrap();
    assert!(found.contains("write-the-parser"));

    call(&tools, "IssueDelete", json!({"id": "epic-one"}))
        .await
        .unwrap();
    assert!(matches!(
        tracker.get("epic-one").await,
        Err(TrackerError::NotFound(_))
    ));
}

#[tokio::test]
async fn what_the_model_can_fix_comes_back_as_an_input_error() {
    let (_, tools) = board();
    call(&tools, "IssueCreate", json!({"title": "a"}))
        .await
        .unwrap();
    let wrong = [
        ("IssueCreate", json!({"title": "b", "status": "doing"})),
        ("IssueCreate", json!({"body": "no title"})),
        ("IssueUpdate", json!({"status": "done"})),
        ("IssueUpdate", json!({"id": "a"})),
        ("IssueGet", json!({"id": "missing"})),
        ("IssueList", json!({"labels": 5})),
        (
            "IssueLink",
            json!({"id": "a", "relation": "epic", "target": "a"}),
        ),
        ("IssueDelete", json!({"id": "../a"})),
    ];
    for (name, args) in wrong {
        let res = call(&tools, name, args.clone()).await;
        assert!(
            matches!(res, Err(ToolError::Input(_))),
            "{name} {args}: {res:?}"
        );
    }
    let gh = issue_tools(github(FakeHttp::new([])));
    let relates = call(
        &gh,
        "IssueLink",
        json!({"id": 1, "relation": "relates_to", "target": 2}),
    )
    .await;
    assert!(matches!(relates, Err(ToolError::Input(_))));
}

#[tokio::test]
async fn a_tracker_that_cannot_answer_is_a_failure_and_a_number_is_an_id() {
    let down = issue_tools(github(FakeHttp::down("connection refused")));
    let res = call(&down, "IssueGet", json!({"id": "1"})).await;
    assert!(matches!(res, Err(ToolError::Failed(_))), "{res:?}");

    let http = FakeHttp::new([ok(issue(42, "answer"))]);
    let gh = issue_tools(github(http.clone()));
    let got = call(&gh, "IssueGet", json!({"id": 42})).await.unwrap();
    assert!(got.contains("answer"));
    assert!(http.sent()[0].ends_with("/repos/o/r/issues/42"));
}

#[tokio::test]
async fn a_cancelled_turn_stops_a_call_that_hangs() {
    let gh = issue_tools(github(FakeHttp::hanging()));
    let ctx = ctx();
    ctx.cancel().cancel();
    let stopped = tokio::time::timeout(
        Duration::from_secs(5),
        tool(&gh, "IssueList").execute(JsonMap::new(), &ctx),
    )
    .await
    .expect("the cancelled call returns at once");
    assert!(matches!(stopped, Err(ToolError::Input(_))));
}
