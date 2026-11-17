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

#![expect(
    clippy::print_stdout,
    reason = "an example shows on the terminal what it does"
)]

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use haest::{
    Agent, AgentDeps, AgentOptions, BoxError, JsonMap, LoopEvent, SharedDeps, ShutWorkspace, Tool,
    ToolContext, ToolError, ToolOutput, Tools, async_trait,
    core::run::{MemoryRunStore, RunKey, Served},
    core::testing::{ScriptedProvider, text_answer, tool_answer},
    core::tool::OnResume,
    serde_json::{Value, json},
};
use tokio::sync::Notify;

/// An outside service that honours idempotency keys: a report sent twice
/// under one key is kept once.
#[derive(Default)]
struct Outbox(Mutex<HashMap<String, String>>);

impl Outbox {
    /// The report sent under `key`, if one was.
    fn sent(&self, key: &str) -> Option<String> {
        self.reports().get(key).cloned()
    }

    /// Keep `id` as the report sent under `key`.
    fn send(&self, key: &str, id: &str) {
        self.reports().insert(key.into(), id.into());
    }

    fn reports(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.0.lock().expect("the outbox's lock poisoned")
    }
}

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
        // A durable run hands every call one; outside one there is nothing to key on.
        let key = ctx
            .effect_key()
            .ok_or_else(|| ToolError::Failed("SendReport runs only in a durable run".into()))?;
        if ctx.resumed() {
            let sent = self.outbox.sent(key.as_str());
            return Ok(ToolOutput::text(match sent {
                Some(id) => format!("already sent as {id}"),
                None => "not sent yet; send it again".into(),
            }));
        }
        let id = format!("report-{}", &key.as_str()[..8]);
        self.outbox.send(key.as_str(), &id);
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
fn agent(provider: &ScriptedProvider, store: &MemoryRunStore, tools: Tools) -> Agent {
    let shared = SharedDeps::new(provider.resolver(), "/", ShutWorkspace);
    let deps =
        AgentDeps::new(shared, tools, "You send the daily report.").with_run_store(store.clone());
    Agent::new(deps, AgentOptions::new("scripted:model-1"))
}

const JOB: &str = "send today's report";

async fn the_shortcut(store: &MemoryRunStore) -> Result<(), BoxError> {
    let key = RunKey::new("report-2026-11-16");
    let provider = ScriptedProvider::new([text_answer("Report sent.")]);
    let mut agent = agent(&provider, store, Tools::new());
    let mut events = Vec::<LoopEvent>::new();

    let first = agent.run_durable(&key, JOB, &mut events).await?;
    let again = agent.run_durable(&key, JOB, &mut events).await?;

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
    Ok(())
}

async fn a_kill_and_a_resume(store: &MemoryRunStore) -> Result<(), BoxError> {
    let key = RunKey::new("report-2026-11-17");
    let outbox = Arc::new(Outbox::default());
    let crash = Arc::new(Notify::new());
    let send = SendReport {
        outbox: outbox.clone(),
        crash_once: Some(crash.clone()),
        crashed: AtomicBool::new(false),
    };
    let tools = Tools::new().with(Fetch).with(send);

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
        .await?;

    assert_eq!(resumed.served, Served::Resumed);
    assert_eq!(outbox.reports().len(), 1, "sent once");
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
    Ok(())
}

async fn every_scene() -> Result<(), BoxError> {
    let store = MemoryRunStore::new();
    the_shortcut(&store).await?;
    a_kill_and_a_resume(&store).await
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    every_scene().await?;
    println!("durable_run example: every scene held");
    Ok(())
}

#[tokio::test]
async fn every_durable_scene_holds() -> Result<(), BoxError> {
    every_scene().await
}
