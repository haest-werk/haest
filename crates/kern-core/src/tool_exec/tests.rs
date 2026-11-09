//! One tool result once put ~600k tokens into a conversation. Nothing a tool
//! returns may reach the model unbounded -- and nothing a tool does may turn
//! into anything but a result the loop can put in history.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde_json::json;

use super::*;
use crate::{
    agent::test_support::{FnTool, MemWorkspace},
    fs::{Fs, MemFs},
    tool::{opt_num, req_str},
    workspace::{ShutWorkspace, Workspace},
};

const SPILL: &str = "/spill";

fn returning(content: String) -> Arc<dyn Tool> {
    FnTool::arc("mcp__srv__list", false, move |_, _| {
        let content = content.clone();
        async move { Ok(ToolOutput::text(content)) }
    })
}

/// Reads a file through the workspace, the way the Read tool does.
fn read_tool() -> Arc<dyn Tool> {
    FnTool::arc("Read", true, |input, ctx| async move {
        let path = ctx.resolve(req_str(&input, "file_path")?);
        let text = ctx.workspace.fs().read_to_string(&path).await?;
        let limit = opt_num(&input, "limit")?.map_or(usize::MAX, |n| n as usize);
        Ok(ToolOutput::text(
            text.lines().take(limit).collect::<Vec<_>>().join("\n"),
        ))
    })
}

fn call(id: &str, name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        input: input.as_object().cloned().unwrap_or_default(),
    }
}

fn ctx_on(workspace: Arc<dyn Workspace>, spill: Option<&str>) -> ToolContext {
    let mut ctx = ToolContext::new("/work", workspace);
    ctx.spill_dir = spill.map(PathBuf::from);
    ctx.tool_call_id = Some("t1".into());
    ctx
}

async fn spilled(fs: &MemFs) -> Vec<PathBuf> {
    match fs.read_dir(Path::new(SPILL)).await {
        Ok(entries) => entries
            .into_iter()
            .map(|e| Path::new(SPILL).join(e.name))
            .collect(),
        Err(_) => Vec::new(),
    }
}

struct Run {
    out: ToolOutput,
    fs: Arc<MemFs>,
    tools: ToolIndex,
    ctx: ToolContext,
}

async fn run(content: String) -> Run {
    let ws = Arc::new(MemWorkspace::default());
    let fs = ws.fs.clone();
    let tools = ToolIndex::new(vec![returning(content), read_tool()]);
    let ctx = ctx_on(ws, Some(SPILL));
    let out = execute_tool(&tools, &call("t1", "mcp__srv__list", json!({})), &ctx).await;
    Run {
        out,
        fs,
        tools,
        ctx,
    }
}

#[tokio::test]
async fn a_huge_output_goes_to_a_file_and_the_model_gets_a_short_notice_naming_it() {
    let huge = "x".repeat(MAX_TOOL_OUTPUT * 20);
    let r = run(huge.clone()).await;
    assert!(r.out.content.len() < MAX_TOOL_OUTPUT);
    assert!(!r.out.is_error);
    let files = spilled(&r.fs).await;
    assert_eq!(files.len(), 1);
    assert_eq!(r.fs.read_to_string(&files[0]).await.unwrap(), huge);
    assert!(r.out.content.contains(&files[0].display().to_string()));
}

#[tokio::test]
async fn a_normal_output_passes_through_untouched_and_nothing_is_written() {
    let r = run("small result".into()).await;
    assert_eq!(r.out.content, "small result");
    assert!(spilled(&r.fs).await.is_empty());
}

#[tokio::test]
async fn the_spilled_file_is_in_the_workspace_and_readable_by_the_read_tool() {
    let r = run(format!("first-line\n{}", "y".repeat(MAX_TOOL_OUTPUT * 2))).await;
    let file = spilled(&r.fs).await.remove(0);
    assert!(r.out.content.contains(&file.display().to_string()));
    let read = call("t2", "Read", json!({"file_path": file, "limit": 1}));
    let got = execute_tool(&r.tools, &read, &r.ctx).await;
    assert!(got.content.contains("first-line"));
}

#[tokio::test]
async fn without_a_spill_dir_a_huge_output_is_cut_to_a_preview() {
    let tools = ToolIndex::new(vec![returning("z".repeat(MAX_TOOL_OUTPUT + 1))]);
    let ctx = ctx_on(Arc::new(MemWorkspace::default()), None);
    let out = execute_tool(&tools, &call("t1", "mcp__srv__list", json!({})), &ctx).await;
    assert!(out.content.len() < MAX_TOOL_OUTPUT && !out.is_error);
}

#[tokio::test]
async fn a_tool_that_sizes_its_own_output_is_never_spilled() {
    let big = "r".repeat(MAX_TOOL_OUTPUT * 2);
    let sized = FnTool::arc("Read", true, move |_, _| {
        let big = big.clone();
        async move {
            Ok(ToolOutput {
                sized: true,
                ..ToolOutput::text(big)
            })
        }
    });
    let ws = Arc::new(MemWorkspace::default());
    let fs = ws.fs.clone();
    let out = execute_tool(
        &ToolIndex::new(vec![sized]),
        &call("t1", "Read", json!({})),
        &ctx_on(ws, Some(SPILL)),
    )
    .await;
    assert_eq!(out.content.len(), MAX_TOOL_OUTPUT * 2);
    assert!(spilled(&fs).await.is_empty());
}

// Regression: a spill that cannot be written used to turn a tool that RAN into
// a failure ("X failed: ..."), and the model may run a side effect again.
#[tokio::test]
async fn a_spill_that_cannot_be_written_still_answers_with_the_preview() {
    let tools = ToolIndex::new(vec![returning("q".repeat(MAX_TOOL_OUTPUT + 1))]);
    let ctx = ctx_on(Arc::new(ShutWorkspace), Some(SPILL));
    let out = execute_tool(&tools, &call("t1", "mcp__srv__list", json!({})), &ctx).await;
    assert!(!out.is_error);
    assert!(out.content.contains("qqqq"));
    assert!(out.content.len() < MAX_TOOL_OUTPUT);
}

// Regression: two calls of one tool spilling in the same millisecond (a
// parallel batch) used to write the same file, the second over the first.
#[tokio::test]
async fn two_calls_spilling_at_once_keep_a_file_each() {
    let ws = Arc::new(MemWorkspace::default());
    let fs = ws.fs.clone();
    let tools = ToolIndex::new(vec![returning("w".repeat(MAX_TOOL_OUTPUT + 1))]);
    let ctx = ctx_on(ws, Some(SPILL));
    let (mut a, mut b) = (ctx.clone(), ctx);
    a.tool_call_id = Some("a".into());
    b.tool_call_id = Some("b".into());
    let (ca, cb) = (
        call("a", "mcp__srv__list", json!({})),
        call("b", "mcp__srv__list", json!({})),
    );
    futures::join!(execute_tool(&tools, &ca, &a), execute_tool(&tools, &cb, &b));
    assert_eq!(spilled(&fs).await.len(), 2);
}

#[tokio::test]
async fn an_unknown_tool_is_an_error_naming_the_tools_there_are() {
    let tools = ToolIndex::new(vec![returning("x".into()), read_tool()]);
    let ctx = ctx_on(Arc::new(MemWorkspace::default()), None);
    let out = execute_tool(&tools, &call("t1", "Nope", json!({})), &ctx).await;
    assert!(out.is_error);
    assert!(out.content.contains("Nope") && out.content.contains("Read"));
}

#[tokio::test]
async fn a_bad_input_goes_back_verbatim_and_a_failure_is_named() {
    let picky = FnTool::arc("Picky", false, |input, _| async move {
        match input.get("mode").and_then(|v| v.as_str()) {
            Some("input") => Err(ToolError::Input("say \"path\"".into())),
            _ => Err(ToolError::Failed("disk on fire".into())),
        }
    });
    let tools = ToolIndex::new(vec![picky]);
    let ctx = ctx_on(Arc::new(MemWorkspace::default()), None);
    let bad = execute_tool(&tools, &call("t1", "Picky", json!({"mode": "input"})), &ctx).await;
    assert!(bad.is_error);
    assert_eq!(bad.content, "say \"path\"");
    let failed = execute_tool(&tools, &call("t2", "Picky", json!({})), &ctx).await;
    assert!(failed.is_error);
    assert!(failed.content.starts_with("Picky") && failed.content.ends_with("disk on fire"));
    for out in [bad, failed] {
        assert_eq!(out.result.unwrap()["kind"], "error");
    }
}

#[tokio::test]
async fn arguments_that_were_not_json_are_answered_without_running_the_tool() {
    let ran = Arc::new(AtomicBool::new(false));
    let seen = ran.clone();
    let tool = FnTool::arc("T", false, move |_, _| {
        seen.store(true, Ordering::SeqCst);
        async { Ok(ToolOutput::text("ran")) }
    });
    let tools = ToolIndex::new(vec![tool]);
    let ctx = ctx_on(Arc::new(MemWorkspace::default()), None);
    let out = execute_tool(&tools, &call("t1", "T", json!({"_unparsed": "{bad"})), &ctx).await;
    assert!(out.is_error && out.content.contains("{bad"));
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_cancelled_turn_answers_a_call_as_interrupted_without_running_it() {
    let ran = Arc::new(AtomicBool::new(false));
    let seen = ran.clone();
    let tool = FnTool::arc("T", false, move |_, _| {
        seen.store(true, Ordering::SeqCst);
        async { Ok(ToolOutput::text("ran")) }
    });
    let tools = ToolIndex::new(vec![tool]);
    let ctx = ctx_on(Arc::new(MemWorkspace::default()), None);
    ctx.cancel.cancel();
    let out = run_call(&tools, &call("t1", "T", json!({})), &ctx).await;
    assert!(out.is_error);
    assert!(!ran.load(Ordering::SeqCst));
}

#[test]
fn a_tools_own_spend_and_its_childrens_fold_into_the_turn() {
    let mut usage = Usage::default();
    fold_spend(&mut usage, &ToolOutput::text("free"));
    assert_eq!(usage.cost_usd, None);
    let paid = ToolOutput {
        cost_usd: Some(1.0),
        child_cost_usd: Some(0.5),
        ..ToolOutput::text("paid")
    };
    fold_spend(&mut usage, &paid);
    assert_eq!(usage.cost_usd, Some(1.5));
}

#[test]
fn a_result_message_carries_the_call_and_the_output() {
    let c = call("t9", "Echo", json!({}));
    let m = tool_result_message(&c, &ToolOutput::error("no"));
    assert_eq!(
        (m.tool_call_id.as_str(), m.tool_name.as_str()),
        ("t9", "Echo")
    );
    assert!(m.is_error && m.media.is_empty());
    let e = error_assistant("m", "why");
    assert_eq!(e.stop_reason, StopReason::Error);
    assert_eq!(e.error_message.as_deref(), Some("why"));
}
