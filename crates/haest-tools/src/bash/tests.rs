//! Ported from tools/tools.test.ts (Bash). The workspace is scripted: killing a
//! command at its timeout and sharing one file system with the tools are the
//! WORKSPACE's job, and haest-workspace's tests defend them. These defend what
//! Bash asks of the workspace and what it makes of the answer.

use std::{path::PathBuf, sync::Arc, time::Duration};

use haest_core::{
    tool::{Tool, ToolError, ToolOutput},
    workspace::ExecResult,
};
use serde_json::{Value, json};

use super::{BashOptions, BashTool};
use crate::testkit::{Call, TestWorkspace, ctx, input};

fn answering(answer: impl Fn(&Call) -> ExecResult + Send + Sync + 'static) -> Arc<TestWorkspace> {
    TestWorkspace::answering([("/p/.keep", "")], answer)
}

async fn bash(ws: &Arc<TestWorkspace>, v: Value) -> Result<ToolOutput, ToolError> {
    BashTool::default()
        .execute(input(v), &ctx(ws.clone()))
        .await
}

#[tokio::test]
async fn reports_exit_code_and_output_as_a_shell_result() {
    let ws = answering(|_| ExecResult {
        stdout: "out\n".into(),
        stderr: "err\n".into(),
        exit_code: 3,
        timed_out: false,
    });
    let r = bash(&ws, json!({"command": "echo out; echo err >&2; exit 3"}))
        .await
        .unwrap();
    assert!(r.is_error);
    assert!(r.content.contains("out") && r.content.contains("err"));
    let res = r.result.unwrap();
    assert_eq!(res["kind"], "shell");
    assert_eq!(res["exitCode"], 3);
    assert_eq!(res["stdout"], "out\n");
    assert_eq!(res["stderr"], "err\n");
    let calls = ws.calls();
    assert!(calls[0].shell);
    assert_eq!(calls[0].argv, ["echo out; echo err >&2; exit 3"]);
    assert_eq!(calls[0].cwd, PathBuf::from("/p"));
}

#[tokio::test]
async fn a_clean_exit_is_not_an_error() {
    let ws = answering(|_| ExecResult::default());
    let r = bash(&ws, json!({"command": "true"})).await.unwrap();
    assert!(!r.is_error);
    assert!(
        !r.content.is_empty(),
        "the model is told there was no output"
    );
}

#[tokio::test]
async fn a_timeout_ends_the_command_and_reports_it_as_an_error() {
    let ws = answering(|c| ExecResult {
        timed_out: c.timeout <= Duration::from_secs(1),
        exit_code: 0,
        ..Default::default()
    });
    let r = bash(
        &ws,
        json!({"command": "while true; do sleep 1; done", "timeout": 1000}),
    )
    .await
    .unwrap();
    assert_eq!(ws.calls()[0].timeout, Duration::from_millis(1000));
    assert!(r.is_error);
}

#[tokio::test]
async fn the_timeout_is_clamped_and_the_default_is_the_hosts() {
    let ws = answering(|_| ExecResult::default());
    bash(&ws, json!({"command": "a", "timeout": 5}))
        .await
        .unwrap();
    bash(&ws, json!({"command": "b", "timeout": "99999999"}))
        .await
        .unwrap();
    bash(&ws, json!({"command": "c"})).await.unwrap();
    let tool = BashTool::new(BashOptions {
        default_timeout: Duration::from_secs(30),
        ..Default::default()
    });
    tool.execute(input(json!({"command": "d"})), &ctx(ws.clone()))
        .await
        .unwrap();
    let timeouts: Vec<_> = ws.calls().iter().map(|c| c.timeout.as_secs()).collect();
    assert_eq!(timeouts, [1, 600, 120, 30]);
    assert!(matches!(
        bash(&ws, json!({"command": "e", "timeout": "soon"})).await,
        Err(ToolError::Input(_))
    ));
    assert!(matches!(
        bash(&ws, json!({})).await,
        Err(ToolError::Input(_))
    ));
}

#[tokio::test]
async fn long_output_keeps_both_ends_and_says_it_was_cut() {
    let ws = answering(|_| ExecResult {
        stdout: format!("HEAD{}TAIL", "x".repeat(100_000)),
        ..Default::default()
    });
    let r = bash(&ws, json!({"command": "big"})).await.unwrap();
    assert!(r.content.starts_with("HEAD") && r.content.ends_with("TAIL"));
    assert!(r.content.len() < 40_000);
    assert_eq!(r.result.unwrap()["truncated"], true);
}

#[test]
fn without_a_job_hub_there_is_no_background_switch() {
    let p = BashTool::default().parameters();
    assert!(p["properties"].get("run_in_background").is_none());
    let c = BashTool::default().canonical_input(&input(json!({"command": "ls", "timeout": 5})));
    assert_eq!(json!(c), json!({"command": "ls", "timeoutMs": 5}));
}

#[tokio::test]
async fn without_a_job_hub_run_in_background_runs_the_command_now() {
    let ws = answering(|_| ExecResult {
        stdout: "now\n".into(),
        ..Default::default()
    });
    let r = bash(&ws, json!({"command": "x", "run_in_background": true}))
        .await
        .unwrap();
    assert_eq!(ws.calls().len(), 1);
    assert_eq!(r.result.unwrap()["stdout"], "now\n");
}
