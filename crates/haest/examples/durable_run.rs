//! A durable run on the memory store, against a scripted model: no network,
//! no keys. `cargo run -p haest --example durable_run` exiting 0 is the
//! proof; `cargo test` runs the same (`[[example]] test`).
//!
//! 1. The shortcut: a job run twice under one key runs once. The second start
//!    returns the stored result and the model is not asked.
//! 2. A kill and a resume: the process dies right after a tool sent a report
//!    and before its result landed. A new process on the same store goes on:
//!    the model call that finished is not paid again, the read that finished
//!    is not run again, and the report tool looks its effect key up in the
//!    outbox instead of sending twice.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use haest::{
    Agent, AgentDeps, AgentOptions, LoopEvent, ShutWorkspace, Tool, ToolContext, ToolError,
    ToolOutput, async_trait,
    core::run::{MemoryRunStore, RunKey, Served},
    core::testing::{ScriptedProvider, text_answer, tool_answer},
    core::tool::{JsonMap, OnResume},
};
use serde_json::{Value, json};
use tokio::sync::Notify;

/// An outside service that honours idempotency keys: a report sent twice
/// under one key is kept once.
#[derive(Default)]
struct Outbox(Mutex<HashMap<String, String>>);

/// Sends the report through the outbox, keyed by the call's effect key. On
/// resume it asks the outbox what happened instead of sending.
struct SendReport {
    outbox: Arc<Outbox>,
    /// The first send "crashes the process" after the report went out.
    crash_once: Option<Arc<Notify>>,
    crashed: AtomicBool,
}

#[async_trait]
impl Tool for SendReport {
    fn name(&self) -> &str {
        "SendReport"
    }
    fn description(&self) -> String {
        "Send the daily report.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "report.send"
    }
    fn on_resume(&self) -> OnResume {
        OnResume::Lookup
    }
    async fn execute(&self, _input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let key = ctx
            .effect_key()
            .expect("a durable run hands every call an effect key");
        if ctx.resumed {
            let sent = self.outbox.0.lock().unwrap().get(key.as_str()).cloned();
            return Ok(ToolOutput::text(match sent {
                Some(id) => format!("already sent as {id}"),
                None => "not sent yet; send it again".into(),
            }));
        }
        let id = format!("report-{}", &key.as_str()[..8]);
        self.outbox
            .0
            .lock()
            .unwrap()
            .insert(key.to_string(), id.clone());
        if let Some(crash) = &self.crash_once
            && !self.crashed.swap(true, Ordering::SeqCst)
        {
            crash.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(ToolOutput::text(format!("sent as {id}")))
    }
}

/// Reads the numbers for the report: safe to run again.
struct Fetch;

#[async_trait]
impl Tool for Fetch {
    fn name(&self) -> &str {
        "Fetch"
    }
    fn description(&self) -> String {
        "Fetch today's numbers.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "report.fetch"
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn execute(&self, _input: JsonMap, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("42 orders"))
    }
}

/// An agent on `provider` with `tools`, keeping its runs in `store`: the one
/// line that makes runs durable is `run_store`.
fn agent(
    provider: &Arc<ScriptedProvider>,
    store: &MemoryRunStore,
    tools: Vec<Arc<dyn Tool>>,
) -> Agent {
    let mut deps = AgentDeps::new(
        Arc::new(provider.resolver()),
        Arc::new(tools),
        Arc::new("You send the daily report.".to_string()),
        "/",
        Arc::new(ShutWorkspace),
    );
    deps.shared.run_store = Some(Arc::new(store.clone()));
    Agent::new(deps, AgentOptions::new("scripted:model-1"))
}

const JOB: &str = "send today's report";

async fn the_shortcut(store: &MemoryRunStore) {
    let key = RunKey::new("report-2026-11-16");
    let provider = ScriptedProvider::new([text_answer("Report sent.")]);
    let mut agent = agent(&provider, store, Vec::new());
    let mut events = Vec::<LoopEvent>::new();

    let first = agent.run_durable(&key, JOB, &mut events).await.unwrap();
    let again = agent.run_durable(&key, JOB, &mut events).await.unwrap();

    assert_eq!(
        (first.served, again.served),
        (Served::Fresh, Served::Stored)
    );
    assert_eq!(again.text, "Report sent.");
    assert_eq!(
        provider.requests().len(),
        1,
        "the second start asked no model"
    );
    println!("shortcut: ran once, served twice ({:?})", again.served);
}

async fn a_kill_and_a_resume(store: &MemoryRunStore) {
    let key = RunKey::new("report-2026-11-17");
    let outbox = Arc::new(Outbox::default());
    let crash = Arc::new(Notify::new());
    let send = Arc::new(SendReport {
        outbox: outbox.clone(),
        crash_once: Some(crash.clone()),
        crashed: AtomicBool::new(false),
    });
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(Fetch), send];

    // Process one: the model asks for both tools; the process dies in SendReport.
    let calls = tool_answer(&[("c1", "Fetch", json!({})), ("c2", "SendReport", json!({}))]);
    let provider = ScriptedProvider::new([calls]);
    let mut first = agent(&provider, store, tools.clone());
    let mut events = Vec::<LoopEvent>::new();
    tokio::select! {
        _ = first.run_durable(&key, JOB, &mut events) => unreachable!(),
        () = crash.notified() => println!("killed: the report went out, its result did not land"),
    }
    drop(first);

    // Process two: the same store, the same key.
    let provider = ScriptedProvider::new([text_answer("The report was already out.")]);
    let mut second = agent(&provider, store, tools);
    let resumed = second
        .run_durable(&key, JOB, &mut Vec::<LoopEvent>::new())
        .await
        .unwrap();

    assert_eq!(resumed.served, Served::Resumed);
    assert_eq!(outbox.0.lock().unwrap().len(), 1, "sent once");
    assert_eq!(
        provider.requests().len(),
        1,
        "the first model call was not paid again"
    );
    let lookup = provider.requests()[0]
        .messages
        .iter()
        .any(|m| matches!(m, haest::Message::Tool(r) if r.content.starts_with("already sent")));
    assert!(lookup, "the report tool looked up instead of sending");
    println!("resumed: {} (served {:?})", resumed.text, resumed.served);
}

async fn every_scene() {
    let store = MemoryRunStore::new();
    the_shortcut(&store).await;
    a_kill_and_a_resume(&store).await;
}

#[tokio::main]
async fn main() {
    every_scene().await;
    println!("durable_run example: every scene held");
}

#[tokio::test]
async fn every_durable_scene_holds() {
    every_scene().await;
}
