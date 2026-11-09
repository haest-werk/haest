//! Fakes shared by the loop tests (agent, guard, limits, ledger, tool_*).

use std::{
    future::Future,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use futures::{FutureExt, future::BoxFuture};
use serde_json::Value;

use crate::{
    agent::{Agent, AgentOptions},
    deps::{AgentDeps, AgentHooks},
    events::LoopEvent,
    fs::{Fs, MemFs},
    ledger::Ledger,
    limits::TurnLimits,
    messages::{AssistantMessage, Message, StopReason, Usage},
    testing::{ScriptedProvider, tool_answer},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolHost, ToolOutput},
    workspace::{EXIT_NOT_FOUND, ExecOptions, ExecResult, Workspace, WorkspaceMode},
};

/// Every cap off; a test turns on the one it is about.
pub fn uncapped() -> TurnLimits {
    TurnLimits::default()
}

/// Records what the loop persists; serves fixed limits and a settable clock.
#[derive(Default)]
pub struct TestHooks {
    pub limits: Option<TurnLimits>,
    pub persisted: Mutex<Vec<Message>>,
    pub now: AtomicU64,
}

impl TestHooks {
    pub fn set_now(&self, ms: u64) {
        self.now.store(ms, Ordering::SeqCst);
    }
}

impl ToolHost for TestHooks {}

#[async_trait]
impl AgentHooks for TestHooks {
    fn on_message(&self, msg: &Message) {
        self.persisted.lock().unwrap().push(msg.clone());
    }
    fn limits(&self, _spec: &str) -> Option<TurnLimits> {
        self.limits.clone()
    }
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

/// Files in memory, no shell.
#[derive(Default)]
pub struct MemWorkspace {
    pub fs: Arc<MemFs>,
}

#[async_trait]
impl Workspace for MemWorkspace {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Sandbox
    }
    fn fs(&self) -> Arc<dyn Fs> {
        self.fs.clone()
    }
    async fn shell(&self, _: &str, _: ExecOptions) -> ExecResult {
        ExecResult {
            exit_code: EXIT_NOT_FOUND,
            ..Default::default()
        }
    }
    async fn run(&self, _: &[String], _: ExecOptions) -> ExecResult {
        ExecResult {
            exit_code: EXIT_NOT_FOUND,
            ..Default::default()
        }
    }
    fn prompt_note(&self) -> String {
        String::new()
    }
}

type Run =
    dyn Fn(JsonMap, ToolContext) -> BoxFuture<'static, Result<ToolOutput, ToolError>> + Send + Sync;

/// A tool made of a closure.
pub struct FnTool {
    name: String,
    read_only: bool,
    run: Box<Run>,
}

impl FnTool {
    pub fn arc<F, Fut>(name: &str, read_only: bool, f: F) -> Arc<dyn Tool>
    where
        F: Fn(JsonMap, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ToolOutput, ToolError>> + Send + 'static,
    {
        Arc::new(Self {
            name: name.into(),
            read_only,
            run: Box::new(move |i, c| f(i, c).boxed()),
        })
    }
}

#[async_trait]
impl Tool for FnTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> String {
        self.name.clone()
    }
    fn parameters(&self) -> Value {
        serde_json::json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        (self.run)(input, ctx.clone()).await
    }
}

/// `echo:<v>`, a write (not read-only), like the default tool of every loop test.
pub fn echo_tool() -> Arc<dyn Tool> {
    FnTool::arc("Echo", false, |input, _| async move {
        let v = input.get("v").map_or("undefined".into(), |v| {
            v.as_str().map_or_else(|| v.to_string(), str::to_string)
        });
        Ok(ToolOutput::text(format!("echo:{v}")))
    })
}

/// One call of Echo.
pub fn echo(id: &str, input: Value) -> AssistantMessage {
    tool_answer(&[(id, "Echo", input)])
}

pub fn with_usage(mut msg: AssistantMessage, usage: &Usage) -> AssistantMessage {
    msg.usage = usage.clone();
    msg
}

#[derive(Default)]
pub struct Opts {
    pub tools: Option<Vec<Arc<dyn Tool>>>,
    pub limits: Option<TurnLimits>,
    pub ledger: Option<Arc<Mutex<Ledger>>>,
    pub workspace: Option<Arc<dyn Workspace>>,
    pub spill_dir: Option<&'static str>,
}

/// An agent on `fake:m1` served by `p`, with Echo unless `opts.tools` says otherwise.
pub fn agent_with(p: &Arc<ScriptedProvider>, opts: Opts) -> (Agent, Arc<TestHooks>) {
    let hooks = Arc::new(TestHooks {
        limits: opts.limits,
        ..Default::default()
    });
    let tools: Vec<Arc<dyn Tool>> = opts.tools.unwrap_or_else(|| vec![echo_tool()]);
    let workspace = opts
        .workspace
        .unwrap_or_else(|| Arc::new(MemWorkspace::default()));
    let mut deps = AgentDeps::new(
        Arc::new(p.resolver()),
        Arc::new(tools),
        Arc::new("sys".to_string()),
        "/tmp",
        workspace,
    );
    deps.hooks = hooks.clone();
    deps.ledger = opts.ledger;
    deps.spill_dir = opts.spill_dir.map(|d| Path::new(d).to_path_buf());
    (Agent::new(deps, AgentOptions::new("fake:m1")), hooks)
}

/// Every event of one turn.
pub async fn collect(agent: &mut Agent, input: &str) -> Vec<LoopEvent> {
    let mut events = Vec::new();
    agent.run(input, &mut events).await;
    events
}

/// The `TurnEnd` event's fields; panics when the turn did not end with one.
pub fn end(events: &[LoopEvent]) -> (StopReason, Usage, Option<String>) {
    match events.last() {
        Some(LoopEvent::TurnEnd {
            stop_reason,
            usage,
            error,
            ..
        }) => (*stop_reason, usage.clone(), error.clone()),
        other => panic!("turn did not end: {other:?}"),
    }
}

pub fn count(events: &[LoopEvent], pred: impl Fn(&LoopEvent) -> bool) -> usize {
    events.iter().filter(|e| pred(e)).count()
}
