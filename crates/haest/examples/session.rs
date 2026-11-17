//! A conversation another program watches and drives while it runs, in one
//! process: a watcher prints every frame, a second input arrives while a tool
//! runs and joins that turn, a peer that connects late catches up from the
//! ring, and closing the session ends every stream.
//!
//! `cargo run -p haest --example session --features session`
//!
//! The model is scripted, so nothing here calls a provider. A real host
//! starts the session over haest-runtime's `Runtime` the same way (the
//! `runtime` and `session` features together), and a transport serves the
//! handle to another process.

use std::{sync::Arc, time::Duration};

use haest::{
    Agent, AgentDeps, AgentOptions, BoxError, JsonMap, LoopEvent, Message, ShutWorkspace, Tool,
    ToolContext, ToolError, ToolOutput, Tools, async_trait,
    core::testing::{ScriptedProvider, text_answer, tool_answer},
    serde_json::{Value, json},
    session::{Backend, Body, Hub, HubOptions, Input, start},
    wire::{Outbound, encode_line},
};
use tokio::sync::Notify;

/// A tool that answers once it is let go, so input can arrive while it runs.
struct Wait(Arc<Notify>);

#[async_trait]
impl Tool for Wait {
    fn name(&self) -> &str {
        "Wait"
    }
    fn description(&self) -> String {
        "Waits until it is let go.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "wait"
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        self.0.notified().await;
        Ok(ToolOutput::text("waited"))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), BoxError> {
    let go = Arc::new(Notify::new());
    let model = ScriptedProvider::new([
        tool_answer(&[("c1", "Wait", json!({}))]),
        text_answer("Done, and I saw your note."),
    ]);
    let tools = Tools::new().with(Wait(go.clone()));
    let deps = AgentDeps::new(model.resolver(), tools, "", "/", ShutWorkspace);
    let agent = Agent::new(deps, AgentOptions::new("scripted:m"));

    // The hub first: one stream, numbered, kept for catch-up.
    let hub = Hub::new("example-stream", HubOptions::default());
    let mut watcher = hub.subscribe(0);
    let session = start(agent, hub);

    // The watcher prints each frame as the line a peer would read.
    let watching = tokio::spawn(async move {
        while let Some(frame) = watcher.recv().await {
            print!("{}", encode_line(&*frame));
        }
    });

    // A turn whose tool waits; a second input arrives meanwhile.
    let first = tokio::spawn({
        let session = session.clone();
        async move { session.turn(Input::human("wait, then answer")).await }
    });
    while model.requests().is_empty() {
        tokio::task::yield_now().await;
    }
    let note = tokio::spawn({
        let session = session.clone();
        async move { session.turn(Input::human("also: the build is green")).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    go.notify_one();
    first.await??;
    note.await??;

    // One turn took both inputs: the note is a user message after the tool's result.
    let history = session.history().await?.messages;
    assert!(matches!(&history[3], Message::User(u) if u.content == "also: the build is green"));
    assert_eq!(model.requests().len(), 2);

    // A peer that connects now catches up: the finished answer, its deltas
    // folded into it.
    let mut late = session.subscribe(0);
    let mut answer = String::new();
    while let Some(frame) = late.recv().await {
        match &frame.body {
            Body::Loop(LoopEvent::Assistant { message }) => answer = message.text(),
            Body::Loop(LoopEvent::TurnEnd { .. }) => break,
            _ => {}
        }
    }
    assert_eq!(answer, "Done, and I saw your note.");

    // A snapshot says where the conversation stands.
    let now = session.snapshot().await?;
    eprintln!("{} messages, last seq {}", now.messages, now.last_seq);

    // Closing ends every stream once it has handed over what it held.
    session.close().await;
    watching.await?;
    while let Some(frame) = late.recv().await {
        assert!(
            matches!(frame.body, Body::Wire(Outbound::Status(_))),
            "only the closing status is left"
        );
    }
    Ok(())
}

/// `cargo test` runs the example, so a change that breaks it fails the suite.
#[test]
fn the_example_runs() -> Result<(), BoxError> {
    main()
}
