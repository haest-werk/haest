//! A parent process driving a conversation over a pipe pair, the way
//! `haest --stdio` is driven: request lines in, frame lines out. Here both
//! ends live in one process (in-memory pipes); a real child serves
//! `Stdio::process()`, its own stdin and stdout, the same way.
//!
//! The parent reads `hello`, sends a turn and reads the turn's frames up to
//! its reply, starts a second turn whose tool runs until stopped, stops it
//! with an `interrupt`, and ends the conversation with `close`.
//!
//! `cargo run -p haest --example stdio --features transport-stdio`
//!
//! The model is scripted, so nothing here calls a provider.

use std::sync::Arc;

use haest::{
    Agent, AgentDeps, AgentOptions, JsonMap, ShutWorkspace, Tool, ToolContext, ToolError,
    ToolOutput, Tools, async_trait,
    core::testing::{ScriptedProvider, text_answer, tool_answer},
    session::{Hub, HubOptions, start},
    transport::{Grant, LineSource, NdjsonSource, Stdio, Transport},
    wire::Origin,
};
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, DuplexStream};

/// A tool that runs until its call is stopped.
struct Forever;

#[async_trait]
impl Tool for Forever {
    fn name(&self) -> &str {
        "Forever"
    }
    fn description(&self) -> String {
        "Runs until it is stopped.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "wait"
    }
    async fn execute(&self, _: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        ctx.cancel.cancelled().await;
        Ok(ToolOutput::text("stopped"))
    }
}

/// The parent's end: writes requests, reads frames.
struct Parent {
    to_child: DuplexStream,
    from_child: NdjsonSource<DuplexStream>,
}

impl Parent {
    async fn send(&mut self, request: Value) {
        let line = format!("{request}\n");
        self.to_child.write_all(line.as_bytes()).await.unwrap();
    }

    /// Frames until `stop` matches one; that one is returned.
    async fn until(&mut self, stop: impl Fn(&Value) -> bool) -> Value {
        loop {
            let line = self.from_child.next_line().await.unwrap().unwrap();
            let frame: Value = serde_json::from_str(&line).unwrap();
            println!("{line}");
            if stop(&frame) {
                return frame;
            }
        }
    }

    async fn reply(&mut self, id: &str) -> Value {
        self.until(|f| f["type"] == "reply" && f["id"] == id).await
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let model = ScriptedProvider::new([
        text_answer("Hello from the child."),
        tool_answer(&[("c1", "Forever", json!({}))]),
    ]);
    let tools = Tools::new().with(Forever);
    let deps = AgentDeps::new(model.resolver(), tools, "", "/", ShutWorkspace);
    let agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    let session = start(agent, Hub::new("example-stream", HubOptions::default()));

    // The child's side: its session served over the pipe pair. The host
    // decides what the parent may do (command) and whose input it sends
    // (a person's, for the turn gate).
    let (to_child, child_stdin) = tokio::io::duplex(1 << 16);
    let (child_stdout, from_child) = tokio::io::duplex(1 << 16);
    let attached = Stdio::new(child_stdin, child_stdout)
        .attach(Arc::new(session), Grant::command(Origin::Human))
        .unwrap();

    let mut parent = Parent {
        to_child,
        from_child: NdjsonSource::new(from_child),
    };
    let hello = parent.until(|f| f["type"] == "hello").await;
    assert_eq!(hello["access"], "command");

    // A turn: its frames, then its reply with how it ended.
    parent
        .send(json!({"v": 1, "id": "t1", "type": "turn", "text": "say hello"}))
        .await;
    let reply = parent.reply("t1").await;
    assert_eq!(reply["result"]["stopReason"], "end_turn");

    // A turn whose tool runs until stopped; the parent stops it.
    parent
        .send(json!({"v": 1, "id": "t2", "type": "turn", "text": "run forever"}))
        .await;
    parent.until(|f| f["type"] == "assistant").await;
    parent
        .send(json!({"v": 1, "id": "c1", "type": "control", "verb": "interrupt"}))
        .await;
    let reply = parent.reply("t2").await;
    assert_eq!(reply["result"]["stopReason"], "aborted");

    // Close: the reply, then the connection ends with the conversation.
    parent
        .send(json!({"v": 1, "id": "x", "type": "control", "verb": "close"}))
        .await;
    parent.reply("x").await;
    eprintln!("ended: {:?}", attached.ended().await);
}

/// `cargo test` runs the example, so a change that breaks it fails the suite.
#[test]
fn the_example_runs() {
    main();
}
