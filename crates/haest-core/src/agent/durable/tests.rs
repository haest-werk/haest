//! Durable runs, driven whole: the shortcut (shortcut.rs), a kill and a
//! resume (resume.rs), the history and torn logs (edges.rs), the flush
//! before effects (effects.rs). Shared fakes here.

mod edges;
mod effects;
mod resume;
mod shortcut;
mod thresholds;

use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Notify;

use crate::{
    agent::{Agent, test_support::*},
    caps::TurnCaps,
    events::LoopEvent,
    messages::{Message, ToolResultMessage},
    run::{MemoryRunStore, RunKey},
    session::SessionRecord,
    testing::ScriptedProvider,
    tool::{JsonMap, OnResume, Tool, ToolContext, ToolError, ToolOutput, ToolRef, Tools},
};

/// One run of a [`Probe`]: which tool, which call, and what it was told.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    tool: String,
    call: String,
    resumed: bool,
    effect_key: Option<String>,
}

type Log = Arc<Mutex<Vec<Seen>>>;

/// A tool that writes down every run in a log shared by all probes (and by
/// both "processes" of a test). One that `hangs` does its effect on its first
/// run, says so, and never returns: the process dies there.
struct Probe {
    name: String,
    read_only: bool,
    on_resume: Option<OnResume>,
    hang: Option<Arc<Notify>>,
    hung: AtomicBool,
    log: Log,
}

impl Probe {
    fn new(name: &str, read_only: bool, log: &Log) -> Self {
        Self {
            name: name.into(),
            read_only,
            on_resume: None,
            hang: None,
            hung: AtomicBool::new(false),
            log: log.clone(),
        }
    }

    fn on_resume(mut self, how: OnResume) -> Self {
        self.on_resume = Some(how);
        self
    }

    fn hangs(mut self, signal: &Arc<Notify>) -> Self {
        self.hang = Some(signal.clone());
        self
    }

    fn arc(self) -> ToolRef {
        ToolRef::new(self)
    }
}

#[async_trait]
impl Tool for Probe {
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
        "test.probe"
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
    fn on_resume(&self) -> OnResume {
        self.on_resume.unwrap_or(if self.read_only {
            OnResume::Rerun
        } else {
            OnResume::Report
        })
    }
    async fn execute(&self, _input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        self.log.lock().unwrap().push(Seen {
            tool: self.name.clone(),
            call: ctx.tool_call_id.clone().unwrap_or_default(),
            resumed: ctx.resumed,
            effect_key: ctx.effect_key().map(|k| k.to_string()),
        });
        if let Some(signal) = &self.hang
            && !self.hung.swap(true, Ordering::SeqCst)
        {
            signal.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(ToolOutput::text(format!("{}:ok", self.name)))
    }
}

/// An agent on `p` with `tools`, keeping its runs in `store`.
fn durable(
    p: &Arc<ScriptedProvider>,
    store: &MemoryRunStore,
    tools: &[ToolRef],
    caps: Option<TurnCaps>,
) -> (Agent, Arc<TestHooks>) {
    let opts = Opts {
        tools: Some(tools.to_vec()),
        caps,
        ..Default::default()
    };
    let (mut agent, hooks) = agent_with(p, opts);
    agent.map_deps(|d| d.with_run_store(store.clone()));
    (agent, hooks)
}

/// Drive `run` until `signal` says a tool hung, then drop it: the process died.
async fn kill_when(signal: &Notify, run: impl Future) {
    tokio::select! {
        _ = run => panic!("the run finished; it was to be killed mid-tool"),
        () = signal.notified() => {}
    }
}

/// `(tool, call, resumed)` of every run so far.
fn runs(log: &Log) -> Vec<(String, String, bool)> {
    let log = log.lock().unwrap();
    log.iter()
        .map(|s| (s.tool.clone(), s.call.clone(), s.resumed))
        .collect()
}

fn run(tool: &str, call: &str, resumed: bool) -> (String, String, bool) {
    (tool.into(), call.into(), resumed)
}

/// The tool results `p`'s request number `n` sent, by call id.
fn results_sent(p: &ScriptedProvider, n: usize) -> Vec<ToolResultMessage> {
    p.requests()[n]
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::Tool(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

/// A copy of `store`'s run `key` without its last `drop` records: records
/// that never landed before the process died.
fn torn(store: &MemoryRunStore, key: &RunKey, drop: usize) -> MemoryRunStore {
    let lines = store.lines(key);
    let copy = MemoryRunStore::new();
    let sink = crate::run::RunStore::sink(&copy, key);
    for line in &lines[..lines.len() - drop] {
        sink.append(&serde_json::from_str::<SessionRecord>(line).unwrap());
    }
    copy
}

fn no_events() -> Vec<LoopEvent> {
    Vec::new()
}
