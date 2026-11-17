//! Ported from code.test.ts: which tools Code may call, and what a run does with them.
//! The output format, promises, inner-call rows and see() are in [`output`].

mod output;
mod wind_down;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use haest_core::tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput};
use haest_tools::{
    BashOptions, BashTool, EditTool, FILE_TOOLS, GlobTool, GrepTool, ReadTool, WriteTool,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{CodeTool, MAX_CALLS, OUTPUT_END, code_callable, code_tool_names};
use crate::testkit::{MemWorkspace, Probe, ctx, input};
use haest_core::{ToolRef, Tools};

fn builtins() -> Tools {
    Tools::from_iter([
        ToolRef::new(ReadTool::default()),
        ToolRef::new(WriteTool::new()),
        ToolRef::new(EditTool::new()),
        ToolRef::new(GrepTool::new()),
        ToolRef::new(GlobTool::new()),
        ToolRef::new(BashTool::new(BashOptions::default())),
    ])
}

/// What haest-runtime allows: the file tools, then the host's `extra` names.
fn allowed(extra: &[&str]) -> Vec<String> {
    FILE_TOOLS
        .iter()
        .chain(extra)
        .map(|s| s.to_string())
        .collect()
}

/// A Code tool over `tools`, the way a host builds it: names from the policy, calls into the list.
fn code_over(tools: impl Into<Tools>, extra: &[&str]) -> CodeTool {
    let tools: Tools = tools.into();
    CodeTool::new(code_tool_names(&tools, &allowed(extra)), tools)
}

async fn run_in(tool: &CodeTool, code: &str, extra: Value, ctx: &ToolContext) -> ToolOutput {
    let mut i = input(extra);
    i.insert("code".into(), json!(code));
    tool.execute(i, ctx).await.expect("Code answers every run")
}

async fn run(tool: &CodeTool, code: &str) -> ToolOutput {
    run_in(
        tool,
        code,
        json!({}),
        &ctx(MemWorkspace::from_files::<&str, &str>([])),
    )
    .await
}

/// The returned value (`→ ...` up to the end marker): JSON parsed, a plain string as is.
fn returned(out: &ToolOutput) -> Option<Value> {
    let fenced = &out.content[..out.content.find(OUTPUT_END)?];
    let at = fenced.find("\n→ ")?;
    let text = fenced[at + "\n→ ".len()..].trim_end();
    Some(serde_json::from_str(text).unwrap_or_else(|_| json!(text)))
}

fn probe(name: &str, read_only: bool, ms: u64) -> Arc<Probe> {
    Arc::new(Probe::new(name, read_only, ms))
}

fn arc(p: &Arc<Probe>) -> ToolRef {
    p.clone().into()
}

/// label, tool, the host's extra names, callable?
type Case = (&'static str, ToolRef, &'static [&'static str], bool);

#[test]
fn code_callable_follows_the_policy() {
    let read = ToolRef::new(ReadTool::default());
    let bash = ToolRef::new(BashTool::new(BashOptions::default()));
    let cases: [Case; 8] = [
        ("Read", read, &[], true),
        ("Grep", ToolRef::new(GrepTool::new()), &[], true),
        ("Write", ToolRef::new(WriteTool::new()), &[], true),
        ("Bash", bash.clone(), &[], false),
        (
            "read-only MCP",
            probe("mcp__s__list", true, 0).into(),
            &[],
            true,
        ),
        (
            "writing MCP",
            probe("mcp__s__send", false, 0).into(),
            &[],
            false,
        ),
        (
            "writing MCP on the allowlist by prefix",
            probe("mcp__s__send", false, 0).into(),
            &["mcp__s__*"],
            true,
        ),
        ("Bash on the allowlist", bash, &["Bash"], true),
    ];
    for (label, tool, extra, want) in cases {
        assert_eq!(
            code_callable(tool.as_ref(), &allowed(extra)),
            want,
            "{label}"
        );
    }
    let read = ReadTool::default();
    assert!(
        !code_callable(&read, &[] as &[&str]),
        "a file tool the host leaves out"
    );
}

#[test]
fn code_never_calls_itself_even_when_allowlisted() {
    assert!(!code_callable(
        &code_over(Tools::new(), &[]),
        &["*".to_string()]
    ));
}

#[tokio::test]
async fn composes_several_tool_calls_and_returns_only_the_aggregate() {
    let ws = MemWorkspace::from_files([
        ("/p/a.ts", "export const TODO_A = 1\n"),
        ("/p/b.ts", "nothing here\n"),
        ("/p/c.ts", "TODO one\nTODO two\n"),
    ]);
    let code = "const files = (await tools.Grep({ pattern: 'TODO', path: '.', output_mode: 'files_with_matches' }))
         .split('\\n').filter(Boolean).map(f => f.split('/').pop()).sort()
       const counts = {}
       for (const f of files) counts[f] = (await tools.Read({ file_path: f })).split('TODO').length - 1
       return counts";
    let out = run_in(&code_over(builtins(), &[]), code, json!({}), &ctx(ws)).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(returned(&out), Some(json!({"a.ts": 1, "c.ts": 2})));
}

#[tokio::test]
async fn a_write_lands_through_the_workspace() {
    let ws = MemWorkspace::from_files([("/p/keep.txt", "")]);
    let code = "await tools.Write({ file_path: 'new.txt', content: 'hi' }); return 1";
    let out = run_in(
        &code_over(builtins(), &[]),
        code,
        json!({}),
        &ctx(ws.clone()),
    )
    .await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(ws.contents("/p/new.txt").await.as_deref(), Some("hi"));
}

#[tokio::test]
async fn a_tool_outside_the_policy_is_refused_into_the_guest_and_never_runs() {
    let bash = probe("Bash", false, 0);
    let tool = code_over(vec![ToolRef::new(ReadTool::default()), arc(&bash)], &[]);
    let code =
        "try { await tools.Bash({ command: 'ls' }); return 'ran' } catch (e) { return 'refused' }";
    assert_eq!(returned(&run(&tool, code).await), Some(json!("refused")));
    assert_eq!(bash.calls(), 0);
}

/// What wraps the model's tools (plan mode, approvals): here, a Write that is always denied.
struct Denied(ToolRef);

#[async_trait]
impl Tool for Denied {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn kind(&self) -> &str {
        self.0.kind()
    }
    fn description(&self) -> String {
        self.0.description()
    }
    fn parameters(&self) -> Value {
        self.0.parameters()
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Input("plan mode".into()))
    }
}

#[tokio::test]
async fn every_inner_call_goes_through_the_tools_the_model_is_given() {
    let ws = MemWorkspace::from_files([("/p/keep.txt", "")]);
    let wrapped: Tools = Tools::from_iter([
        ToolRef::new(ReadTool::default()),
        ToolRef::new(Denied(ToolRef::new(WriteTool::new()))),
    ]);
    let tool = CodeTool::new(["Read", "Write"], wrapped);
    let code = "try { await tools.Write({ file_path: 'x.txt', content: 'no' }); return 'wrote' } catch (e) { return 'denied' }";
    let out = run_in(&tool, code, json!({}), &ctx(ws.clone())).await;
    assert_eq!(returned(&out), Some(json!("denied")));
    assert_eq!(ws.contents("/p/x.txt").await, None);
}

#[tokio::test]
async fn the_guest_has_no_host_reach_of_its_own() {
    let out = run(
        &code_over(Tools::new(), &[]),
        "return [typeof require, typeof process, typeof fetch, typeof Bun]",
    )
    .await;
    assert_eq!(
        returned(&out),
        Some(json!(["undefined", "undefined", "undefined", "undefined"]))
    );
}

#[tokio::test]
async fn a_cpu_loop_is_stopped_at_the_deadline() {
    let started = Instant::now();
    let c = ctx(MemWorkspace::from_files::<&str, &str>([]));
    let out = run_in(
        &code_over(Tools::new(), &[]),
        "while (true) {}",
        json!({"timeout": 1000}),
        &c,
    )
    .await;
    assert!(out.is_error);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn the_caller_aborting_stops_a_run_parked_on_a_slow_tool() {
    let slow = probe("mcp__s__slow", true, 10_000);
    let cancel = CancellationToken::new();
    let c = ctx(MemWorkspace::from_files::<&str, &str>([])).with_cancel(cancel.clone());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let started = Instant::now();
    let out = run_in(
        &code_over(vec![arc(&slow)], &[]),
        "await tools.mcp__s__slow({})",
        json!({}),
        &c,
    )
    .await;
    assert!(out.is_error);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn tool_calls_stop_at_the_cap_per_run() {
    let ping = probe("mcp__s__ping", true, 0);
    let code =
        "let n = 0; try { for (;;) { await tools.mcp__s__ping({}); n++ } } catch (e) {} return n";
    let out = run(&code_over(vec![arc(&ping)], &[]), code).await;
    assert_eq!(returned(&out), Some(json!(MAX_CALLS)));
    assert_eq!(ping.calls(), MAX_CALLS);
}

#[tokio::test]
async fn writes_run_one_at_a_time_reads_in_parallel() {
    let (w, r) = (
        probe("mcp__s__write", false, 20),
        probe("mcp__s__read", true, 20),
    );
    let all = |name: &str| format!("await Promise.all([1,2,3,4].map(() => tools.{name}({{}})))");
    let code = format!("{}; {}", all("mcp__s__write"), all("mcp__s__read"));
    let out = run(
        &code_over(vec![arc(&w), arc(&r)], &["mcp__s__write"]),
        &code,
    )
    .await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(w.peak(), 1);
    assert_eq!(r.peak(), 4);
}

#[tokio::test]
async fn an_inner_tools_spend_is_folded_into_the_code_call() {
    let mut paid = Probe::new("mcp__s__paid", true, 0);
    paid.answer.cost_usd = Some(0.25);
    let out = run(
        &code_over(Tools::from_iter([ToolRef::new(paid)]), &[]),
        "await tools.mcp__s__paid({}); await tools.mcp__s__paid({})",
    )
    .await;
    assert_eq!(out.cost_usd, Some(0.5));
}

#[tokio::test]
async fn describe_returns_the_input_schema_of_a_callable_tool() {
    let out = run(
        &code_over(Tools::from_iter([ToolRef::new(ReadTool::default())]), &[]),
        "return describe('Read').required",
    )
    .await;
    assert_eq!(returned(&out), Some(json!(["file_path"])));
}

#[tokio::test]
async fn a_syntax_error_is_a_correctable_error_not_a_crash() {
    let out = run(&code_over(Tools::new(), &[]), "return (").await;
    assert!(out.is_error);
}
