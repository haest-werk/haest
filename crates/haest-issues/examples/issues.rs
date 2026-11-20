//! The issue tools on a haest agent: a host hands the agent a tracker's tools
//! beside its others, and the model plans work on a board of card files.
//!
//! The model here is scripted (no network, no keys), so the run is the same
//! every time and `cargo test` runs it too; swap the resolver for a real
//! provider and the same tools serve a real model.
//!
//! ```text
//! cargo run -p haest-issues --example issues                  # a board in a temp dir
//! cargo run -p haest-issues --example issues -- --github o/r  # also lists o/r's open issues (read only, no token)
//! ```

#![expect(
    clippy::print_stdout,
    reason = "an example shows on the terminal what it does"
)]

use std::path::Path;

use haest_core::{
    Agent, AgentDeps, AgentOptions, LoopEvent, SharedDeps, ShutWorkspace, StopReason, Tools,
    error::BoxError,
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::{self, ToolContext},
};
use haest_issues::{CardTracker, GithubTracker, Tracker, issue_tools};
use haest_workspace::RealFs;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let tmp = tempfile::tempdir()?;
    plan_on_a_board(tmp.path()).await?;
    let args: Vec<String> = std::env::args().collect();
    if let Some(repo) = args
        .iter()
        .position(|a| a == "--github")
        .and_then(|i| args.get(i + 1))
    {
        list_github(repo).await?;
    }
    Ok(())
}

#[tokio::test]
async fn the_agent_plans_on_the_board() -> Result<(), BoxError> {
    let tmp = tempfile::tempdir()?;
    plan_on_a_board(tmp.path()).await
}

/// One turn: the model files an epic and a card under it, starts the card, and
/// lists the board; every change lands in a card file.
async fn plan_on_a_board(root: &Path) -> Result<(), BoxError> {
    let dir = root.join("cards");
    // The host picks the directory and the file system; the tracker reads nothing else.
    let tracker = CardTracker::new(&dir, RealFs);
    let tools: Tools = issue_tools(tracker.clone());

    let model = ScriptedProvider::new([
        tool_answer(&[
            (
                "c1",
                "IssueCreate",
                json!({"title": "Search v2", "labels": ["epic"]}),
            ),
            (
                "c2",
                "IssueCreate",
                json!({"title": "Index the titles", "priority": "high", "body": "Titles first, bodies next."}),
            ),
        ]),
        tool_answer(&[
            (
                "c3",
                "IssueLink",
                json!({"id": "index-the-titles", "relation": "parent", "target": "search-v2"}),
            ),
            (
                "c4",
                "IssueUpdate",
                json!({"id": "index-the-titles", "status": "in-progress"}),
            ),
        ]),
        tool_answer(&[("c5", "IssueList", json!({}))]),
        text_answer("Filed the epic and its first card, and started it."),
    ]);
    let shared = SharedDeps::new(model.resolver(), root, ShutWorkspace);
    let deps = AgentDeps::new(shared, tools, "You plan work on the team's board.");
    let mut agent = Agent::new(deps, AgentOptions::new("scripted:planner"));
    let mut events: Vec<LoopEvent> = Vec::new();
    let end = agent
        .run("Plan search v2 and start on it.", &mut events)
        .await;
    assert_eq!(end.stop_reason, StopReason::EndTurn, "{end:?}");

    for e in &events {
        if let LoopEvent::ToolResult { call, output, .. } = e {
            assert!(!output.is_error, "{}: {}", call.name, output.content);
            println!("{} -> {}", call.name, output.content);
        }
    }
    // The model was offered all seven tools.
    let offered: Vec<String> = model.requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(offered.len(), 7, "{offered:?}");

    let card = tracker.get("index-the-titles").await?;
    assert_eq!(card.status, "in-progress");
    let text = std::fs::read_to_string(dir.join("index-the-titles.md"))?;
    assert!(text.contains("epic: search-v2"), "{text}");
    println!("\n{}:\n{text}", dir.join("index-the-titles.md").display());
    Ok(())
}

/// The same tools over GitHub, called directly by a host: read only, no token
/// (a public repository), so nothing on GitHub changes.
async fn list_github(repo: &str) -> Result<(), BoxError> {
    let (owner, name) = repo.split_once('/').ok_or("--github takes owner/repo")?;
    let tools = issue_tools(GithubTracker::new(owner, name));
    let list = tools.get("IssueList").ok_or("no IssueList tool")?;
    let ctx = ToolContext::new("/", ShutWorkspace);
    match list.execute(tool::input(json!({"limit": 10}))?, &ctx).await {
        Ok(out) => println!("\n{repo}, open issues:\n{}", out.content),
        Err(e) => println!("\n{repo}: {e}"),
    }
    Ok(())
}
