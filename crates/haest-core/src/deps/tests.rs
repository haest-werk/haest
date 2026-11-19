use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use serde_json::json;

use super::*;
use crate::{
    agent::AgentOptions,
    events::LoopEvent,
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, ToolServices, Tools},
    workspace::ShutWorkspace,
};

fn deps(prompt: impl Into<SystemPromptRef>) -> AgentDeps {
    let provider = ScriptedProvider::new([]);
    AgentDeps::new(on(&provider), Tools::new(), prompt)
}

/// What every deps here run on: `provider`, at `/`, with no workspace.
fn on(provider: &ScriptedProvider) -> SharedDeps {
    SharedDeps::new(provider.resolver(), "/", ShutWorkspace)
}

#[tokio::test]
async fn a_str_a_string_and_a_closure_are_system_prompts() {
    assert_eq!(deps("fixed").system_prompt().system_prompt().await, "fixed");
    let owned = String::from("owned");
    assert_eq!(deps(owned).system_prompt().system_prompt().await, "owned");

    let calls = Arc::new(AtomicU64::new(0));
    let counted = calls.clone();
    let built = deps(move || format!("call {}", counted.fetch_add(1, Ordering::SeqCst)));
    assert_eq!(built.system_prompt().system_prompt().await, "call 0");
    assert_eq!(built.system_prompt().system_prompt().await, "call 1");
}

/// Publishes what it is called with.
struct Publish;

#[async_trait]
impl Tool for Publish {
    fn name(&self) -> &str {
        "Publish"
    }
    fn description(&self) -> String {
        "Publish bytes.".into()
    }
    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "test.publish"
    }
    async fn execute(&self, _: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let url = ctx.services.publish(b"bytes".to_vec(), "text/plain").await;
        Ok(ToolOutput::text(url.unwrap_or_else(|| "none".into())))
    }
}

/// Answers every publish with a URL, and counts them.
#[derive(Default)]
struct Blobs(AtomicU64);

#[async_trait]
impl ToolServices for Blobs {
    async fn publish(&self, _: Vec<u8>, _: &str) -> Option<String> {
        Some(format!("blob://{}", self.0.fetch_add(1, Ordering::SeqCst)))
    }
}

async fn published(deps: AgentDeps) -> String {
    let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    let mut events: Vec<LoopEvent> = Vec::new();
    let _ = agent.run("publish", &mut events).await;
    let result = events.iter().find_map(|e| match e {
        LoopEvent::ToolResult { output, .. } => Some(output.content.clone()),
        _ => None,
    });
    result.expect("the tool ran")
}

#[tokio::test]
async fn tools_reach_the_services_slot_not_the_hooks() {
    let script = || {
        [
            tool_answer(&[("c1", "Publish", json!({}))]),
            text_answer("done"),
        ]
    };
    let provider = ScriptedProvider::new(script());
    let tools = Tools::new().with(Publish);
    let plain = AgentDeps::new(on(&provider), tools.clone(), "");
    assert_eq!(published(plain).await, "none");

    let provider = ScriptedProvider::new(script());
    let blobs = Arc::new(Blobs::default());
    let served = AgentDeps::new(on(&provider), tools, "").with_services(blobs.clone());
    assert_eq!(published(served).await, "blob://0");
    assert_eq!(blobs.0.load(Ordering::SeqCst), 1);
}

/// One object behind every host slot, each as its default.
struct Host;

impl AgentHooks for Host {}
impl Clock for Host {}
impl ToolServices for Host {}
impl crate::catalog::ModelFacts for Host {}
impl crate::caps::TurnCapsSource for Host {}

/// Where a slot's value lives: one host behind every slot has one address.
fn at<T: ?Sized>(slot: &T) -> *const () {
    (slot as *const T).cast()
}

#[test]
fn one_host_fills_every_host_slot_with_the_one_value() {
    let provider = ScriptedProvider::new([]);
    let deps = AgentDeps::new(on(&provider), Tools::new(), "").with_host(Host);
    let s = deps.shared();
    let slots = [
        at(&**s.hooks()),
        at(&**s.services()),
        at(&**s.facts()),
        at(&**s.turn_caps()),
        at(&**s.clock()),
    ];
    assert!(slots.iter().all(|p| *p == slots[0]), "{slots:?}");
    // A setter after it changes its own slot only.
    let deps = deps.with_clock(|| 7);
    assert_eq!(deps.shared().clock().now_ms(), 7);
    assert_eq!(at(&**deps.shared().facts()), slots[0]);
}
