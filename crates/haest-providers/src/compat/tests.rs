use haest_core::{
    messages::{AssistantMessage, AssistantPart, Message, StopReason, ToolCall, ToolResultMessage},
    provider::{Effort, Provider, ProviderRequest},
};
use serde_json::{Value, json};

use super::*;
use crate::testing::{FakeReply, FakeTransport, chat_sse, collect, request, request_with};

pub(super) fn chunk(delta: Value) -> Value {
    finished(delta, None)
}

/// A chunk that ends the choice with `reason`.
pub(super) fn finished(delta: Value, reason: Option<&str>) -> Value {
    json!({
        "id": "c", "object": "chat.completion.chunk", "created": 0, "model": "m",
        "choices": [{"index": 0, "delta": delta, "finish_reason": reason}],
    })
}

/// The last chunk of an `include_usage` stream: no choices, the usage.
pub(super) fn usage_chunk(usage: Value) -> Value {
    json!({"id": "c", "object": "chat.completion.chunk", "created": 0, "model": "m", "choices": [], "usage": usage})
}

pub(super) fn openrouter() -> CompatOptions {
    CompatOptions::new("openrouter", CompatFlavor::OpenRouter, "k")
}

pub(super) fn or_request() -> ProviderRequest {
    request("anthropic/claude-sonnet-5")
}

pub(super) async fn run_body(
    opts: CompatOptions,
    req: ProviderRequest,
    body: String,
) -> (Vec<ProviderEvent>, AssistantMessage) {
    let t = FakeTransport::new([FakeReply::sse(body)]);
    collect(CompatProvider::new(opts, t).stream(req)).await
}

pub(super) async fn run(chunks: &[Value]) -> (Vec<ProviderEvent>, AssistantMessage) {
    run_body(openrouter(), or_request(), chat_sse(chunks)).await
}

fn input(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[tokio::test]
async fn text_reasoning_fragmented_tool_calls_usage_and_cost_fold_into_one_message() {
    let (events, msg) = run(&[
        chunk(json!({"reasoning": "think"})),
        chunk(json!({"content": "Hel"})),
        chunk(json!({"content": "lo"})),
        chunk(json!({"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "Read", "arguments": "{\"file_"}}]})),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "path\":\"a\"}"}}]})),
        finished(json!({}), Some("tool_calls")),
        usage_chunk(json!({
            "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120,
            "prompt_tokens_details": {"cached_tokens": 60}, "cost": 0.01,
        })),
    ])
    .await;
    let texts = events
        .iter()
        .filter(|e| matches!(e, ProviderEvent::TextDelta(_)))
        .count();
    assert_eq!(texts, 2);
    assert!(events.contains(&ProviderEvent::ToolCallStart {
        id: "call_1".into(),
        name: "Read".into()
    }));
    assert_eq!(
        msg.content,
        vec![
            AssistantPart::Thinking {
                thinking: "think".into(),
                signature: None,
                redacted: None
            },
            AssistantPart::Text {
                text: "Hello".into()
            },
            AssistantPart::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "Read".into(),
                input: input(json!({"file_path": "a"})),
            }),
        ]
    );
    assert_eq!(msg.stop_reason, StopReason::ToolUse);
    let u = &msg.usage;
    assert_eq!(
        (u.input, u.cache_read, u.output, u.cost_usd),
        (40, 60, 20, Some(0.01))
    );
}

#[tokio::test]
async fn a_mid_stream_error_keeps_the_text_drops_half_built_tool_calls_and_ends_as_error() {
    let mut err = chunk(json!({}));
    err["error"] = json!({"message": "upstream overloaded"});
    let (_, msg) = run(&[
        chunk(json!({"content": "partial"})),
        chunk(json!({"tool_calls": [{"index": 0, "id": "x", "function": {"name": "Bash", "arguments": "{\"comm"}}]})),
        err,
    ])
    .await;
    assert_eq!(msg.stop_reason, StopReason::Error);
    assert_eq!(msg.error_message.as_deref(), Some("upstream overloaded"));
    assert_eq!(
        msg.content,
        vec![AssistantPart::Text {
            text: "partial".into()
        }]
    );
}

#[tokio::test]
async fn unparseable_tool_arguments_are_surfaced_not_thrown() {
    let (_, msg) = run(&[chunk(
        json!({"tool_calls": [{"index": 0, "id": "a", "function": {"name": "X", "arguments": "{bad"}}]}),
    )])
    .await;
    let AssistantPart::ToolCall(call) = &msg.content[0] else {
        panic!("{:?}", msg.content)
    };
    assert_eq!(
        Value::Object(call.input.clone()),
        json!({"_unparsed": "{bad"})
    );
}

#[tokio::test]
async fn a_tool_call_streamed_without_an_id_gets_one() {
    let (_, msg) = run(&[chunk(
        json!({"tool_calls": [{"index": 0, "function": {"name": "X", "arguments": "{}"}}]}),
    )])
    .await;
    let call = msg.tool_calls().next().unwrap();
    assert!(call.id.starts_with("call_") && call.id.len() > 5);
}

pub(super) fn body_with(opts: &CompatOptions, model: &str, effort: Option<Effort>) -> Value {
    let mut req = request(model);
    req.effort = effort;
    build_compat_body(opts, &req)
}

#[test]
fn openrouter_and_an_anthropic_model_get_cache_breakpoints_usage_accounting_and_effort() {
    let body = body_with(
        &openrouter(),
        "anthropic/claude-sonnet-5",
        Some(Effort::Max),
    );
    assert_eq!(body["usage"], json!({"include": true}));
    assert_eq!(body["reasoning"], json!({"effort": "max"}));
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(
        messages[0]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    let last = messages.last().unwrap();
    assert_eq!(
        last["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
}

#[test]
fn openrouter_gets_every_haest_effort_level_unchanged_and_off_as_reasoning_disabled() {
    let reasoning = |e| {
        body_with(&openrouter(), "anthropic/claude-sonnet-5", e)
            .get("reasoning")
            .cloned()
    };
    let levels = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];
    for level in levels {
        assert_eq!(
            reasoning(Some(level)),
            Some(json!({"effort": level.as_str()}))
        );
    }
    assert_eq!(
        reasoning(Some(Effort::Off)),
        Some(json!({"enabled": false}))
    );
    assert_eq!(reasoning(None), None);
}

#[test]
fn generic_servers_get_plain_strings_and_no_vendor_fields() {
    let local = CompatOptions::new("local", CompatFlavor::Generic, "k");
    let body = body_with(&local, "anthropic/claude-sonnet-5", Some(Effort::High));
    assert!(body.get("reasoning").is_none());
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("usage").is_none());
    assert_eq!(body["messages"][0]["content"], json!("sys"));
}

#[test]
fn assistant_tool_calls_and_tool_results_round_trip_into_chat_format() {
    let mut assistant = AssistantMessage::start("openrouter", "m");
    assistant.content = vec![
        AssistantPart::Text { text: "ok".into() },
        AssistantPart::ToolCall(ToolCall {
            id: "t1".into(),
            name: "Read".into(),
            input: input(json!({"file_path": "a"})),
        }),
    ];
    assistant.stop_reason = StopReason::ToolUse;
    assistant.provider_data = Some(json!({"reasoning_details": [{"type": "reasoning.text"}]}));
    let history = vec![
        Message::user("go"),
        Message::Assistant(assistant),
        Message::Tool(ToolResultMessage {
            tool_call_id: "t1".into(),
            tool_name: "Read".into(),
            content: "data".into(),
            is_error: false,
            media: Vec::new(),
        }),
    ];
    let body = build_compat_body(&openrouter(), &request_with("z-ai/glm-5.3", history));
    let messages = body["messages"].as_array().unwrap();
    let (assistant, tool) = (&messages[2], &messages[3]);
    assert_eq!(
        assistant["tool_calls"],
        json!([{"id": "t1", "type": "function", "function": {"name": "Read", "arguments": "{\"file_path\":\"a\"}"}}])
    );
    assert_eq!(
        assistant["reasoning_details"],
        json!([{"type": "reasoning.text"}])
    );
    assert_eq!(tool["role"], json!("tool"));
    assert_eq!(tool["tool_call_id"], json!("t1"));
    assert_eq!(tool["content"], json!("data"));
}

#[test]
fn the_wire_request_goes_to_chat_completions_with_a_bearer_key_and_custom_headers_win() {
    let mut opts = openrouter();
    opts.base_url = Some("https://example.test/api/v1/".into());
    opts.headers = vec![
        ("X-Title".into(), "haest".into()),
        ("Accept".into(), "text/event-stream".into()),
    ];
    let t = FakeTransport::new([]);
    let req = CompatProvider::new(opts, t).http_request(&or_request());
    assert_eq!(req.url, "https://example.test/api/v1/chat/completions");
    assert_eq!(req.header("authorization"), Some("Bearer k"));
    assert_eq!(req.header("x-title"), Some("haest"));
    assert_eq!(req.header("accept"), Some("text/event-stream"));
    assert_eq!(
        req.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("accept"))
            .count(),
        1
    );
}
