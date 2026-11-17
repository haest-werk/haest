//! Fakes shared by the loop tests (agent, guard, limits, ledger, tool_*).

use std::{
    future::Future,
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
    caps::{TurnCaps, TurnCapsSource},
    deps::{AgentDeps, AgentHooks, Clock},
    events::LoopEvent,
    fs::{FsRef, MemFs},
    ledger::Ledger,
    messages::{AssistantMessage, Message, StopReason, Usage},
    testing::{ScriptedProvider, tool_answer},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, ToolRef, Tools, spelled},
    workspace::{EXIT_NOT_FOUND, ExecOptions, ExecResult, Workspace, WorkspaceMode, WorkspaceRef},
};

/// Every cap off; a test turns on the one it is about.
pub(crate) fn uncapped() -> TurnCaps {
    TurnCaps::default()
}

/// Records what the loop persists and warns; serves fixed limits and a
/// settable clock.
#[derive(Default)]
pub(crate) struct TestHooks {
    pub caps: Option<TurnCaps>,
    pub persisted: Mutex<Vec<Message>>,
    pub warnings: Mutex<Vec<String>>,
    pub now: AtomicU64,
}

impl TestHooks {
    pub(crate) fn set_now(&self, ms: u64) {
        self.now.store(ms, Ordering::SeqCst);
    }
}

#[async_trait]
impl AgentHooks for TestHooks {
    fn on_message(&self, msg: &Message) {
        self.persisted.lock().unwrap().push(msg.clone());
    }
    fn on_warning(&self, text: &str) {
        self.warnings.lock().unwrap().push(text.to_string());
    }
}

impl TurnCapsSource for TestHooks {
    fn turn_caps(&self, _spec: &str) -> Option<TurnCaps> {
        self.caps.clone()
    }
}

impl Clock for TestHooks {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

/// Files in memory, no shell.
#[derive(Default)]
pub(crate) struct MemWorkspace {
    pub fs: MemFs,
}

#[async_trait]
impl Workspace for MemWorkspace {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Sandbox
    }
    fn fs(&self) -> FsRef {
        self.fs.clone().into()
    }
    async fn shell(&self, _: &str, _: ExecOptions) -> ExecResult {
        ExecResult::failed("", EXIT_NOT_FOUND)
    }
    async fn run(&self, _: &[String], _: ExecOptions) -> ExecResult {
        ExecResult::failed("", EXIT_NOT_FOUND)
    }
    fn prompt_note(&self) -> String {
        String::new()
    }
}

type Run =
    dyn Fn(JsonMap, ToolContext) -> BoxFuture<'static, Result<ToolOutput, ToolError>> + Send + Sync;

/// A tool made of a closure.
pub(crate) struct FnTool {
    name: String,
    read_only: bool,
    run: Box<Run>,
}

impl FnTool {
    pub(crate) fn arc<F, Fut>(name: &str, read_only: bool, f: F) -> ToolRef
    where
        F: Fn(JsonMap, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ToolOutput, ToolError>> + Send + 'static,
    {
        ToolRef::new(Self {
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
pub(crate) fn echo_tool() -> ToolRef {
    FnTool::arc("Echo", false, |input, _| async move {
        let v = input.get("v").map_or("undefined".into(), spelled);
        Ok(ToolOutput::text(format!("echo:{v}")))
    })
}

/// One call of Echo.
pub(crate) fn echo(id: &str, input: Value) -> AssistantMessage {
    tool_answer(&[(id, "Echo", input)])
}

pub(crate) fn with_usage(mut msg: AssistantMessage, usage: &Usage) -> AssistantMessage {
    msg.usage = usage.clone();
    msg
}

#[derive(Default)]
pub(crate) struct Opts {
    pub tools: Option<Vec<ToolRef>>,
    pub caps: Option<TurnCaps>,
    pub ledger: Option<Ledger>,
    pub workspace: Option<WorkspaceRef>,
    pub spill_dir: Option<&'static str>,
}

/// Every cap off but what `f` turns on.
pub(crate) fn turn_caps(f: impl FnOnce(&mut TurnCaps)) -> TurnCaps {
    let mut l = uncapped();
    f(&mut l);
    l
}

/// The default agent with only the caps `f` turns on.
pub(crate) fn capped(f: impl FnOnce(&mut TurnCaps)) -> Opts {
    Opts {
        caps: Some(turn_caps(f)),
        ..Default::default()
    }
}

/// An agent on `fake:m1` served by `p`, with Echo unless `opts.tools` says otherwise.
pub(crate) fn agent_with(p: &ScriptedProvider, opts: Opts) -> (Agent, Arc<TestHooks>) {
    let hooks = Arc::new(TestHooks {
        caps: opts.caps,
        ..Default::default()
    });
    let tools = opts
        .tools
        .map_or_else(|| Tools::new().with(echo_tool()), Tools::from);
    let workspace = opts
        .workspace
        .unwrap_or_else(|| MemWorkspace::default().into());
    let shared = crate::SharedDeps::new(p.resolver(), "/tmp", workspace);
    let deps = AgentDeps::new(shared, tools, "sys")
        .with_hooks(hooks.clone())
        .with_turn_caps(hooks.clone())
        .with_clock(hooks.clone())
        .with_ledger(opts.ledger)
        .with_spill_dir(opts.spill_dir.map(std::path::PathBuf::from));
    (Agent::new(deps, AgentOptions::new("fake:m1")), hooks)
}

/// Every event of one turn; how it ended is the last one.
pub(crate) async fn collect(agent: &mut Agent, input: &str) -> Vec<LoopEvent> {
    let mut events = Vec::new();
    let _ = agent.run(input, &mut events).await;
    events
}

/// The `TurnEnd` event's fields; panics when the turn did not end with one.
pub(crate) fn end(events: &[LoopEvent]) -> (StopReason, Usage, Option<String>) {
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

pub(crate) fn count(events: &[LoopEvent], pred: impl Fn(&LoopEvent) -> bool) -> usize {
    events.iter().filter(|e| pred(e)).count()
}
