//! Tool changes on Anthropic's own API: the request keeps its `tools` as
//! first sent and says each change in a mid-conversation `system` message,
//! shaped as Anthropic documents it (the fact sheet's examples), with the
//! `inline-tools-2026-09-15` beta on every request that carries one.

use haest_core::{
    ProviderRequest,
    cache::CacheTtl,
    messages::{Message, ToolChange, ToolChangeMessage},
    provider::ToolSpec,
    testing::text_answer,
};
use serde_json::{Value, json};

use super::*;
use crate::testing::{FakeHttp, collect, ok_reply};

/// Anthropic's documented example tool, `get_weather`.
fn get_weather() -> ToolSpec {
    ToolSpec {
        name: "get_weather".into(),
        description: "Get the current weather in a given location.".into(),
        parameters: json!({"type": "object", "properties": {"location": {"type": "string"}}}),
    }
}

/// Anthropic's documented example of a tool defined in a message.
fn db_query() -> ToolSpec {
    ToolSpec {
        name: "db_query".into(),
        description: "Run a read-only SQL query against the analytics database.".into(),
        parameters: json!({
            "type": "object",
            "properties": {"sql": {"type": "string"}},
            "required": ["sql"]
        }),
    }
}

fn removed(name: &str) -> ToolChange {
    ToolChange::Removed { name: name.into() }
}

fn change(changes: Vec<ToolChange>, baseline: Option<Vec<ToolSpec>>) -> Message {
    Message::ToolChange(ToolChangeMessage { changes, baseline })
}

/// A request on `baseline` whose tools are now `tools`, of `messages`.
fn on(baseline: Vec<ToolSpec>, tools: Vec<ToolSpec>, messages: Vec<Message>) -> ProviderRequest {
    ProviderRequest::new("claude-opus-5-5", messages)
        .with_system("sys")
        .with_tools(tools)
        .with_tool_baseline(baseline)
        .with_cache_ttl(CacheTtl::FiveMinutes)
}

fn body(req: &ProviderRequest) -> Value {
    build_anthropic_params("anthropic", req)
}

/// A five-minute breakpoint.
fn cached() -> Value {
    json!({"type": "ephemeral"})
}

#[test]
fn a_tool_added_by_value_is_a_system_message_and_the_tools_stay_as_first_sent() {
    let req = on(
        vec![get_weather()],
        vec![get_weather(), db_query()],
        vec![
            Message::user("How many orders shipped yesterday?"),
            change(
                vec![ToolChange::Added { tool: db_query() }],
                Some(vec![get_weather()]),
            ),
        ],
    );
    let expected = json!({
        "model": "claude-opus-5-5",
        "max_tokens": 32_000,
        "system": [{"type": "text", "text": "sys", "cache_control": cached()}],
        "messages": [
            {"role": "user", "content": [
                {"type": "text", "text": "How many orders shipped yesterday?", "cache_control": cached()}
            ]},
            {"role": "system", "content": [
                {
                    "type": "tool_addition",
                    "tool": {
                        "type": "tool_definition",
                        "definition": {
                            "name": "db_query",
                            "description": "Run a read-only SQL query against the analytics database.",
                            "input_schema": {
                                "type": "object",
                                "properties": {"sql": {"type": "string"}},
                                "required": ["sql"]
                            }
                        }
                    }
                }
            ]}
        ],
        "stream": true,
        "tools": [{
            "name": "get_weather",
            "description": "Get the current weather in a given location.",
            "input_schema": {"type": "object", "properties": {"location": {"type": "string"}}},
            "cache_control": cached()
        }],
    });
    assert_eq!(body(&req).to_string(), expected.to_string());
}

#[test]
fn a_tool_removed_goes_by_reference() {
    let req = on(
        vec![get_weather(), db_query()],
        vec![db_query()],
        vec![
            Message::user("hi"),
            change(
                vec![removed("get_weather")],
                Some(vec![get_weather(), db_query()]),
            ),
        ],
    );
    let sent = body(&req);
    assert_eq!(
        sent["messages"][1],
        json!({
            "role": "system",
            "content": [
                {
                    "type": "tool_removal",
                    "tool": {"type": "tool_reference", "name": "get_weather"}
                }
            ]
        })
    );
    let names: Vec<_> = (sent["tools"].as_array().unwrap().iter())
        .map(|t| t["name"].clone())
        .collect();
    assert_eq!(
        names,
        ["get_weather", "db_query"],
        "the tools array never changes"
    );
}

#[test]
fn a_tool_hidden_then_shown_is_removed_then_added_back_by_reference() {
    let req = on(
        vec![get_weather()],
        vec![get_weather()],
        vec![
            Message::user("one"),
            change(vec![removed("get_weather")], Some(vec![get_weather()])),
            Message::Assistant(text_answer("ok")),
            Message::user("two"),
            change(
                vec![ToolChange::Restored {
                    name: "get_weather".into(),
                }],
                None,
            ),
        ],
    );
    let sent = body(&req);
    let roles: Vec<_> = (sent["messages"].as_array().unwrap().iter())
        .map(|m| m["role"].clone())
        .collect();
    assert_eq!(roles, ["user", "system", "assistant", "user", "system"]);
    assert_eq!(
        sent["messages"][1]["content"],
        json!([{"type": "tool_removal", "tool": {"type": "tool_reference", "name": "get_weather"}}])
    );
    assert_eq!(
        sent["messages"][4]["content"],
        json!([{"type": "tool_addition", "tool": {"type": "tool_reference", "name": "get_weather"}}])
    );
}

#[test]
fn a_replaced_definition_is_added_by_value_under_its_name() {
    let mut v2 = get_weather();
    v2.description = "Get the weather now, or for a given day.".into();
    let req = on(
        vec![get_weather()],
        vec![v2.clone()],
        vec![
            Message::user("hi"),
            change(
                vec![ToolChange::Added { tool: v2 }],
                Some(vec![get_weather()]),
            ),
        ],
    );
    let sent = body(&req);
    let block = &sent["messages"][1]["content"][0];
    assert_eq!(block["type"], "tool_addition");
    assert_eq!(block["tool"]["type"], "tool_definition");
    assert_eq!(
        block["tool"]["definition"],
        json!({
            "name": "get_weather",
            "description": "Get the weather now, or for a given day.",
            "input_schema": {"type": "object", "properties": {"location": {"type": "string"}}}
        })
    );
    assert_eq!(
        sent["tools"][0]["description"],
        "Get the current weather in a given location."
    );
}

#[test]
fn two_changes_go_out_as_one_system_message_in_order() {
    let req = on(
        vec![get_weather()],
        vec![db_query()],
        vec![
            Message::user("hi"),
            change(
                vec![
                    removed("get_weather"),
                    ToolChange::Added { tool: db_query() },
                ],
                Some(vec![get_weather()]),
            ),
        ],
    );
    let sent = body(&req);
    assert_eq!(sent["messages"].as_array().unwrap().len(), 2);
    let kinds: Vec<_> = (sent["messages"][1]["content"].as_array().unwrap().iter())
        .map(|b| b["type"].clone())
        .collect();
    assert_eq!(kinds, ["tool_removal", "tool_addition"]);
}

#[test]
fn a_breakpoint_never_lands_on_a_system_message() {
    let req = on(
        vec![get_weather()],
        vec![],
        vec![
            Message::user("one"),
            change(vec![removed("get_weather")], Some(vec![get_weather()])),
        ],
    )
    .with_cache(haest_core::cache::CachePlan::default().with_message(1, CacheTtl::OneHour));
    let sent = body(&req);
    assert!(
        sent["messages"][1]["content"][0]
            .get("cache_control")
            .is_none()
    );
    // The rolling one goes on the turn before it, as Anthropic advises.
    assert_eq!(sent["messages"][0]["content"][0]["cache_control"], cached());
    // Not in a definition either: a tool added by value carries none.
    let added = on(
        vec![get_weather()],
        vec![get_weather(), db_query()],
        vec![
            Message::user("one"),
            change(
                vec![ToolChange::Added { tool: db_query() }],
                Some(vec![get_weather()]),
            ),
        ],
    );
    let sent = body(&added);
    assert!(!sent["messages"][1].to_string().contains("cache_control"));
}

#[test]
fn without_a_baseline_the_changes_are_left_out_and_the_current_tools_sent() {
    let messages = vec![
        Message::user("hi"),
        change(vec![removed("get_weather")], Some(vec![get_weather()])),
    ];
    let req = ProviderRequest::new("claude-opus-5-5", messages)
        .with_system("sys")
        .with_tools(vec![db_query()]);
    let sent = body(&req);
    assert_eq!(sent["messages"].as_array().unwrap().len(), 1);
    assert_eq!(sent["tools"][0]["name"], "db_query");
}

#[tokio::test]
async fn the_beta_header_rides_only_on_a_request_that_carries_a_change() {
    let t = FakeHttp::new([0; 4].map(|_| ok_reply("anthropic:claude-opus-5-5")));
    let opts = AnthropicOptions::new("anthropic", "k");
    let provider = AnthropicProvider::new(opts.clone(), t.clone());
    let changed = vec![
        Message::user("hi"),
        change(vec![removed("get_weather")], Some(vec![get_weather()])),
    ];
    let plain = on(
        vec![get_weather()],
        vec![get_weather()],
        vec![Message::user("hi")],
    );
    let _ = collect(provider.stream(plain)).await;
    let _ = collect(provider.stream(on(vec![get_weather()], vec![], changed.clone()))).await;
    // Left out, the change needs no beta.
    let left_out = ProviderRequest::new("claude-opus-5-5", changed.clone()).with_system("sys");
    let _ = collect(provider.stream(left_out)).await;
    // A beta the host configured stays, the new one after it.
    let configured = opts.with_header("anthropic-beta", "files-api-2025-04-14");
    let provider = AnthropicProvider::new(configured, t.clone());
    let _ = collect(provider.stream(on(vec![get_weather()], vec![], changed))).await;

    let betas: Vec<_> = (t.requests().iter())
        .map(|r| r.header("anthropic-beta").map(str::to_string))
        .collect();
    assert_eq!(
        betas,
        [
            None,
            Some("inline-tools-2026-09-15".into()),
            None,
            Some("files-api-2025-04-14,inline-tools-2026-09-15".into()),
        ]
    );
}
