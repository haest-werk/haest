//! R3, intent before effect: before a batch with side effects the run's
//! records land (and the host's own flush runs); read-only batches do not wait.

use std::io;

use serde_json::json;

use super::*;
use crate::{
    deps::AgentHooks,
    run::{ReplayedRun, RunStore},
    session::SessionSink,
    testing::{text_answer, tool_answer},
    tool::ToolServices,
};

/// What happened, in order: a record appended (its kind), a flush, a tool's effect.
#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Record(String),
    Flush,
    Effect(String),
}

type Events = Arc<Mutex<Vec<Ev>>>;

/// A store, and its sink, that only write down what they are asked to do.
struct Watching(Events);

#[async_trait]
impl SessionSink for Watching {
    fn append(&self, record: &SessionRecord) {
        let v = serde_json::to_value(record).unwrap();
        let kind = v["type"].as_str().unwrap().to_string();
        self.0.lock().unwrap().push(Ev::Record(kind));
    }
    async fn flush(&self) {
        self.0.lock().unwrap().push(Ev::Flush);
    }
}

#[async_trait]
impl RunStore for Watching {
    fn sink(&self, _key: &RunKey) -> Arc<dyn SessionSink> {
        Arc::new(Watching(self.0.clone()))
    }
    async fn load(&self, _key: &RunKey) -> io::Result<Option<ReplayedRun>> {
        Ok(None)
    }
}

/// A tool that writes its effect into the event list.
fn effect(name: &str, read_only: bool, events: &Events) -> Arc<dyn Tool> {
    let (label, events) = (name.to_string(), events.clone());
    FnTool::arc(name, read_only, move |_, _| {
        let (label, events) = (label.clone(), events.clone());
        async move {
            events.lock().unwrap().push(Ev::Effect(label));
            Ok(ToolOutput::text("ok"))
        }
    })
}

/// The events between the last record before `effect` and the effect itself.
fn just_before(events: &[Ev], effect: &str) -> Vec<Ev> {
    let at = events
        .iter()
        .position(|e| *e == Ev::Effect(effect.into()))
        .unwrap();
    let from = events[..at]
        .iter()
        .rposition(|e| matches!(e, Ev::Record(k) if k == "message"))
        .unwrap();
    events[from + 1..at].to_vec()
}

#[tokio::test]
async fn the_call_lands_before_a_tool_with_side_effects_runs_and_reads_do_not_wait() {
    let events: Events = Arc::default();
    let tools = [
        effect("Look", true, &events),
        effect("Write", false, &events),
    ];
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Look", json!({}))]),
        tool_answer(&[("c2", "Write", json!({}))]),
        text_answer("done"),
    ]);
    let opts = Opts {
        tools: Some(tools.to_vec()),
        ..Default::default()
    };
    let (mut agent, _) = agent_with(&p, opts);
    agent.deps_mut().shared.run_store = Some(Arc::new(Watching(events.clone())));
    agent
        .run_durable(&RunKey::new("job"), "go", &mut no_events())
        .await
        .unwrap();

    let events = events.lock().unwrap().clone();
    assert!(!just_before(&events, "Look").contains(&Ev::Flush));
    assert!(just_before(&events, "Write").contains(&Ev::Flush));
}

/// Counts the host's own flushes.
#[derive(Default)]
struct Flushing(Mutex<u32>);

impl ToolServices for Flushing {}

#[async_trait]
impl AgentHooks for Flushing {
    async fn before_effects(&self) {
        *self.0.lock().unwrap() += 1;
    }
}

#[tokio::test]
async fn a_plain_turn_awaits_the_hosts_flush_before_side_effects_only() {
    let events: Events = Arc::default();
    let tools = [
        effect("Look", true, &events),
        effect("Write", false, &events),
    ];
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Look", json!({})), ("c2", "Look", json!({}))]),
        tool_answer(&[("c3", "Write", json!({}))]),
        tool_answer(&[("c4", "Nope", json!({}))]),
        text_answer("done"),
    ]);
    let opts = Opts {
        tools: Some(tools.to_vec()),
        ..Default::default()
    };
    let (mut agent, _) = agent_with(&p, opts);
    let host = Arc::new(Flushing::default());
    agent.deps_mut().shared.hooks = host.clone();
    agent.run("go", &mut no_events()).await;

    // One batch with side effects; the reads and the unknown tool ran none.
    assert_eq!(*host.0.lock().unwrap(), 1);
}
