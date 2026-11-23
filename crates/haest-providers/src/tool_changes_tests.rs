//! Tool changes over the wire: an agent on the bundled catalog's facts,
//! whose tools change between turns, and its requests as each provider
//! sends them. Anthropic's own API keeps its `tools` as first sent and gets
//! each change as a `system` message; OpenRouter (its Anthropic models too)
//! and OpenAI get the current tools, as before.

use std::collections::HashMap;

use haest_core::{
    Agent, AgentDeps, AgentOptions, JsonMap, LiveTools, LoopEvent, SharedDeps, ShutWorkspace, Tool,
    ToolContext, ToolError, ToolOutput, Tools,
};
use haest_http::async_trait;
use serde_json::{Value, json};

use crate::{
    CatalogFacts, CustomProviders, ProviderRegistry, RetryPolicy,
    testing::{FakeHttp, FakeReply, chat_sse, typed_sse},
};

/// A tool that only has a name.
struct Named(&'static str);

#[async_trait]
impl Tool for Named {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> String {
        format!("The {} tool.", self.0)
    }
    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "test.named"
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("ok"))
    }
}

/// The answer "ok" in the dialect `spec`'s provider speaks.
fn ok(spec: &str) -> FakeReply {
    let body = match spec.split_once(':').map(|s| s.0) {
        Some("anthropic") => typed_sse(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 10}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": "ok"}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
        ]),
        Some("openai") => typed_sse(&[
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": "ok"}),
            json!({"type": "response.completed", "response": {"usage": {"input_tokens": 10, "output_tokens": 1}}}),
        ]),
        _ => chat_sse(&[
            json!({"choices": [{"index": 0, "delta": {"content": "ok"}, "finish_reason": "stop"}]}),
        ]),
    };
    FakeReply::sse(body)
}

/// An agent on `spec` whose tools are `live`, sending through `http`, every
/// key set, the catalog's facts.
fn agent(http: &FakeHttp, spec: &str, live: &LiveTools) -> Agent {
    let keys: HashMap<String, String> =
        ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY"]
            .into_iter()
            .map(|k| (k.to_string(), "k".to_string()))
            .collect();
    let models = ProviderRegistry::new(keys)
        .with_http(http.clone())
        .with_retry(RetryPolicy::none());
    let shared = SharedDeps::new(models, "/", ShutWorkspace)
        .with_facts(CatalogFacts::new(CustomProviders::new()));
    Agent::new(
        AgentDeps::new(shared, live.clone(), "sys"),
        AgentOptions::new(spec),
    )
}

async fn turn(agent: &mut Agent, input: &str) {
    let end = agent.run(input, &mut Vec::<LoopEvent>::new()).await;
    assert_eq!(end.error, None);
}

/// The tools a body sends, as the bytes of their JSON (OpenAI's Responses
/// API and Chat Completions name them differently, so the whole array).
fn tools_of(body: &Value) -> String {
    body["tools"].to_string()
}

/// The names of the tools a body sends, in each dialect.
fn names(body: &Value) -> Vec<String> {
    let tools = body["tools"].as_array().cloned().unwrap_or_default();
    let name = |t: &Value| {
        let name = t.get("name").or_else(|| t.pointer("/function/name"));
        name.and_then(Value::as_str).unwrap_or_default().to_string()
    };
    tools.iter().map(name).collect()
}

/// Three turns on `spec`: tools Clock and Lookup, then Lookup taken out and
/// Search put in between the first two. The bodies and the beta headers.
async fn three_turns(spec: &str) -> (Vec<Value>, Vec<Option<String>>) {
    let http = FakeHttp::new([ok(spec), ok(spec), ok(spec)]);
    let live = LiveTools::from(Tools::new().with(Named("Clock")).with(Named("Lookup")));
    let mut agent = agent(&http, spec, &live);
    turn(&mut agent, "one").await;
    live.remove_tool("Lookup");
    live.set_tool(Named("Search"));
    turn(&mut agent, "two").await;
    turn(&mut agent, "three").await;
    let sent = http.requests();
    let bodies = sent.iter().map(|r| r.json_body()).collect();
    let betas = (sent.iter())
        .map(|r| r.header("anthropic-beta").map(str::to_string))
        .collect();
    (bodies, betas)
}

#[tokio::test]
async fn anthropics_own_api_keeps_its_tools_byte_for_byte_and_hears_the_change() {
    let (bodies, betas) = three_turns("anthropic:claude-opus-5-5").await;
    assert_eq!(tools_of(&bodies[0]), tools_of(&bodies[1]));
    assert_eq!(tools_of(&bodies[0]), tools_of(&bodies[2]));
    assert_eq!(names(&bodies[2]), ["Clock", "Lookup"]);
    let change = json!({"role": "system", "content": [
        {"type": "tool_removal", "tool": {"type": "tool_reference", "name": "Lookup"}},
        {"type": "tool_addition", "tool": {"type": "tool_definition", "definition": {
            "name": "Search", "description": "The Search tool.", "input_schema": {"type": "object"}}}},
    ]});
    let roles = |body: &Value| -> Vec<Value> {
        let messages = body["messages"].as_array().unwrap();
        messages.iter().map(|m| m["role"].clone()).collect()
    };
    // The change came after the turn ended: it waited for "two" and sits right after it.
    assert_eq!(roles(&bodies[1]), ["user", "assistant", "user", "system"]);
    assert_eq!(bodies[1]["messages"][3], change);
    assert_eq!(
        roles(&bodies[2]),
        ["user", "assistant", "user", "system", "assistant", "user"]
    );
    assert_eq!(bodies[2]["messages"][3], change, "sent again as it was");
    let inline = Some("inline-tools-2026-09-15".to_string());
    assert_eq!(betas, [None, inline.clone(), inline]);
}

#[tokio::test]
async fn openrouter_and_openai_get_the_changed_tools_as_before() {
    for spec in ["openrouter:anthropic/claude-opus-5.5", "openai:gpt-6.1-sol"] {
        let (bodies, betas) = three_turns(spec).await;
        assert_eq!(names(&bodies[0]), ["Clock", "Lookup"], "{spec}");
        assert_eq!(names(&bodies[1]), ["Clock", "Search"], "{spec}");
        assert_eq!(tools_of(&bodies[1]), tools_of(&bodies[2]), "{spec}");
        let text = bodies[2].to_string();
        assert!(
            !text.contains("tool_addition") && !text.contains("tool_change"),
            "{spec}: {text}"
        );
        assert_eq!(betas, [None, None, None], "{spec}");
    }
}

#[tokio::test]
async fn a_switch_to_openrouter_and_back_sends_each_what_it_takes() {
    let anthropic = "anthropic:claude-opus-5-5";
    let openrouter = "openrouter:anthropic/claude-opus-5.5";
    let http = FakeHttp::new([ok(anthropic), ok(anthropic), ok(openrouter), ok(anthropic)]);
    let live = LiveTools::from(Tools::new().with(Named("Clock")));
    let mut agent = agent(&http, anthropic, &live);
    turn(&mut agent, "one").await;
    live.set_tool(Named("Search"));
    turn(&mut agent, "two").await;
    agent.set_model_spec(openrouter);
    live.set_tool(Named("Lookup"));
    turn(&mut agent, "three").await;
    agent.set_model_spec(anthropic);
    turn(&mut agent, "four").await;

    let bodies: Vec<Value> = http.requests().iter().map(|r| r.json_body()).collect();
    // OpenRouter: the current tools, the change left out.
    assert_eq!(names(&bodies[2]), ["Clock", "Search", "Lookup"]);
    let roles: Vec<_> = (bodies[2]["messages"].as_array().unwrap().iter())
        .map(|m| m["role"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        roles,
        ["system", "user", "assistant", "user", "assistant", "user"]
    );
    // Back on Anthropic: the tools as first sent, both changes after their user turns.
    assert_eq!(tools_of(&bodies[3]), tools_of(&bodies[0]));
    let systems: Vec<_> = (bodies[3]["messages"].as_array().unwrap().iter())
        .filter(|m| m["role"] == "system")
        .map(|m| m["content"][0]["tool"]["definition"]["name"].clone())
        .collect();
    assert_eq!(systems, ["Search", "Lookup"]);
    let before = uncached(&bodies[1]["messages"]);
    let after = uncached(&bodies[3]["messages"]);
    assert_eq!(
        after.as_array().unwrap()[..4],
        before.as_array().unwrap()[..]
    );
}

/// `v` without its breakpoints, which move to the newest message each request.
fn uncached(v: &Value) -> Value {
    match v {
        Value::Object(map) => (map.iter())
            .filter(|(k, _)| *k != "cache_control")
            .map(|(k, v)| (k.clone(), uncached(v)))
            .collect(),
        Value::Array(items) => items.iter().map(uncached).collect(),
        _ => v.clone(),
    }
}
