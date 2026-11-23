use haest_core::{
    cache::CacheTtl,
    messages::{
        AssistantMessage, AssistantPart, Message, StopReason, ToolCall, ToolResultMessage, Usage,
    },
    provider::Effort,
};
use serde_json::{Value, json};

use super::{convert::uses_budget_thinking, *};
use crate::testing::{FakeEnd, FakeHttp, FakeReply, collect, kinds, request, typed_sse};

fn provider(t: &FakeHttp) -> AnthropicProvider {
    AnthropicProvider::new(AnthropicOptions::new("anthropic", "k"), t.clone())
}

fn start() -> Value {
    json!({
        "type": "message_start",
        "message": {
            "model": "claude-sonnet-5",
            "usage": {"input_tokens": 10, "cache_read_input_tokens": 90, "cache_creation_input_tokens": 5},
        },
    })
}

fn block_start(index: u64, block: Value) -> Value {
    json!({"type": "content_block_start", "index": index, "content_block": block})
}

fn delta(index: u64, delta: Value) -> Value {
    json!({"type": "content_block_delta", "index": index, "delta": delta})
}

#[tokio::test]
async fn thinking_with_signature_text_and_a_streamed_tool_call_fold_into_one_message() {
    let body = typed_sse(&[
        start(),
        json!({"type": "ping"}),
        block_start(
            0,
            json!({"type": "thinking", "thinking": "", "signature": ""}),
        ),
        delta(0, json!({"type": "thinking_delta", "thinking": "hmm"})),
        delta(0, json!({"type": "signature_delta", "signature": "SIG"})),
        block_start(1, json!({"type": "text", "text": ""})),
        delta(1, json!({"type": "text_delta", "text": "ok"})),
        block_start(
            2,
            json!({"type": "tool_use", "id": "tu_1", "name": "Read", "input": {}}),
        ),
        delta(
            2,
            json!({"type": "input_json_delta", "partial_json": "{\"file_path\":"}),
        ),
        delta(
            2,
            json!({"type": "input_json_delta", "partial_json": "\"a\"}"}),
        ),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 7}}),
        json!({"type": "message_stop"}),
    ]);
    let t = FakeHttp::new([FakeReply::sse(body)]);
    let (events, msg) = collect(provider(&t).stream(request("claude-sonnet-5"))).await;
    assert_eq!(
        kinds(&events),
        ["thinking_delta", "text_delta", "tool_call_start", "done"]
    );
    assert_eq!(
        msg.content,
        vec![
            AssistantPart::Thinking {
                thinking: "hmm".into(),
                signature: Some("SIG".into()),
                redacted: None
            },
            AssistantPart::Text { text: "ok".into() },
            AssistantPart::ToolCall(ToolCall {
                id: "tu_1".into(),
                name: "Read".into(),
                input: json!({"file_path": "a"}).as_object().unwrap().clone(),
            }),
        ]
    );
    assert_eq!(msg.stop_reason, StopReason::ToolUse);
    assert_eq!(
        (
            msg.usage.input,
            msg.usage.cache_read,
            msg.usage.cache_write,
            msg.usage.output
        ),
        (10, 90, 5, 7)
    );
    let req = &t.requests()[0];
    assert_eq!(req.url, "https://api.anthropic.com/v1/messages");
    assert_eq!(req.header("x-api-key"), Some("k"));
    assert_eq!(req.json_body()["stream"], json!(true));
}

#[tokio::test]
async fn a_stream_that_dies_keeps_text_but_not_the_half_built_tool_call() {
    let body = typed_sse(&[
        start(),
        block_start(0, json!({"type": "text", "text": "part"})),
        block_start(
            1,
            json!({"type": "tool_use", "id": "t", "name": "Bash", "input": {}}),
        ),
    ]);
    let broken = FakeReply::sse(body.clone()).then(FakeEnd::Fail("connection reset".into()));
    let error_event = typed_sse(&[json!({
        "type": "error",
        "error": {"type": "overloaded_error", "message": "Overloaded"},
    })]);
    let erroring = FakeReply::sse(body + &error_event);
    for (reply, why) in [(broken, "connection reset"), (erroring, "Overloaded")] {
        let t = FakeHttp::new([reply]);
        let (_, msg) = collect(provider(&t).stream(request("claude-sonnet-5"))).await;
        assert_eq!(msg.stop_reason, StopReason::Error);
        assert!(msg.error_message.as_ref().unwrap().contains(why));
        assert_eq!(
            msg.content,
            vec![AssistantPart::Text {
                text: "part".into()
            }]
        );
    }
}

/// A finished answer with a tool call, with what a proxy may add: a non-JSON
/// keep-alive between events, an event haest has no use for, a trailing `[DONE]`.
#[tokio::test]
async fn data_haest_does_not_use_is_skipped_and_the_answer_keeps_its_tool_call() {
    let head = typed_sse(&[
        start(),
        block_start(
            0,
            json!({"type": "tool_use", "id": "tu_1", "name": "Read", "input": {"file_path": "a"}}),
        ),
    ]);
    let tail = typed_sse(&[
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ]);
    let body = format!(
        "{head}data: keep-alive\n\nevent: proxy_status\ndata: <ok>\n\n{tail}data: [DONE]\n\n"
    );
    let t = FakeHttp::new([FakeReply::sse(body)]);
    let (_, msg) = collect(provider(&t).stream(request("claude-sonnet-5"))).await;
    assert_eq!(msg.error_message, None);
    assert_eq!(msg.stop_reason, StopReason::ToolUse);
    assert_eq!(msg.tool_calls().count(), 1);
}

/// An event haest folds whose data is not JSON still fails the stream; an
/// `error` event's text is the failure, as the server said it.
#[tokio::test]
async fn a_malformed_event_haest_uses_still_fails_the_stream() {
    let head = typed_sse(&[
        start(),
        block_start(0, json!({"type": "text", "text": "part"})),
    ]);
    let broken = format!("{head}event: content_block_delta\ndata: {{\"index\": 0,\n\n");
    let erroring = format!("{head}event: error\ndata: upstream overloaded\n\n");
    for (body, said) in [(broken, None), (erroring, Some("upstream overloaded"))] {
        let t = FakeHttp::new([FakeReply::sse(body.clone())]);
        let (_, msg) = collect(provider(&t).stream(request("claude-sonnet-5"))).await;
        assert_eq!(msg.stop_reason, StopReason::Error, "{body}");
        if let Some(said) = said {
            assert_eq!(msg.error_message.as_deref(), Some(said));
        }
    }
}

fn assistant(provider: &str) -> Message {
    let mut m = AssistantMessage::start(provider, "m");
    m.content = vec![
        AssistantPart::Thinking {
            thinking: "t".into(),
            signature: Some("S".into()),
            redacted: None,
        },
        AssistantPart::ToolCall(ToolCall {
            id: "call.1".into(),
            name: "Read".into(),
            input: Default::default(),
        }),
    ];
    m.usage = Usage::default();
    m.stop_reason = StopReason::ToolUse;
    Message::Assistant(m)
}

fn tool_result(id: &str, content: &str) -> Message {
    Message::Tool(ToolResultMessage {
        tool_call_id: id.into(),
        tool_name: "Read".into(),
        content: content.into(),
        is_error: false,
        media: Vec::new(),
    })
}

fn types(content: &Value) -> Vec<&str> {
    content
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect()
}

#[test]
fn tool_results_and_queued_input_merge_into_one_user_turn_ids_sanitised_tail_cached() {
    let msgs = to_anthropic_messages(
        &[
            Message::user("go"),
            assistant("anthropic"),
            tool_result("call.1", "x"),
            Message::user("also this"),
        ],
        "anthropic",
        CacheTtl::FiveMinutes,
    );
    let roles: Vec<_> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["user", "assistant", "user"]);
    let last = &msgs[2]["content"];
    assert_eq!(types(last), ["tool_result", "text"]);
    assert_eq!(last[0]["tool_use_id"], json!("call_1"));
    assert_eq!(last[1]["cache_control"], json!({"type": "ephemeral"}));
    assert!(last[0].get("cache_control").is_none());
}

#[test]
fn thinking_is_replayed_only_to_the_provider_that_signed_it() {
    let replayed = |from: &str, to: &str| {
        let msgs = to_anthropic_messages(
            &[Message::user("a"), assistant(from)],
            to,
            CacheTtl::FiveMinutes,
        );
        types(&msgs[1]["content"]) == ["thinking", "tool_use"]
    };
    assert!(replayed("anthropic", "anthropic"));
    assert!(replayed("gateway", "gateway"));
    assert!(!replayed("openrouter", "anthropic"));
    assert!(!replayed("gateway", "anthropic"));
    assert!(!replayed("anthropic", "gateway"));
}

#[test]
fn effort_maps_to_adaptive_thinking_on_current_models_and_a_budget_on_legacy_ones() {
    let max = DEFAULT_MAX_TOKENS;
    let adaptive = Value::Object(thinking_params("claude-sonnet-5", Some(Effort::High), max));
    assert_eq!(
        adaptive,
        json!({"thinking": {"type": "adaptive"}, "output_config": {"effort": "high"}})
    );
    assert!(uses_budget_thinking("claude-sonnet-4-5-20250929"));
    assert!(!uses_budget_thinking("claude-opus-4-6"));
    assert!(uses_budget_thinking("claude-3-7-sonnet"));
    assert!(uses_budget_thinking("claude-opus-4-20250514"));
    assert!(!uses_budget_thinking("claude-opus-4-10"));
    assert_eq!(
        Value::Object(thinking_params("claude-haiku-4-5", Some(Effort::Low), max)),
        json!({"thinking": {"type": "enabled", "budget_tokens": 2048}})
    );
    assert!(thinking_params("claude-sonnet-5", None, max).is_empty());
    assert!(thinking_params("claude-sonnet-5", Some(Effort::Off), max).is_empty());
    let params = build_anthropic_params("anthropic", &request("claude-sonnet-5"));
    assert_eq!(
        params["system"],
        json!([{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}])
    );
    assert_eq!(params["max_tokens"], json!(32_000));
}

/// Anthropic takes a thinking budget of at least 1,024 tokens and below
/// `max_tokens`, and a `max_tokens` above 0: any other request is a 400.
#[test]
fn a_thinking_budget_fits_under_the_requests_token_cap() {
    let sent = |model: &str, effort: Effort, max: Option<u32>| {
        let mut req = request(model).with_effort(effort);
        if let Some(max) = max {
            req = req.with_max_tokens(max);
        }
        build_anthropic_params("anthropic", &req)
    };
    for (effort, max) in [
        (Effort::High, Some(4096)),
        (Effort::Max, Some(16_000)),
        (Effort::Low, Some(2048)),
        (Effort::Max, None),
        (Effort::Low, Some(0)),
    ] {
        let body = sent("claude-sonnet-4-5", effort, max);
        let cap = body["max_tokens"].as_u64().unwrap();
        assert!(cap > 0, "{effort:?} {max:?}: {body}");
        let budget = body["thinking"]["budget_tokens"].as_u64().unwrap();
        assert!((1024..cap).contains(&budget), "{effort:?} {max:?}: {body}");
    }
    // A cap too small to think in sends no thinking at all.
    let body = sent("claude-sonnet-4-5", Effort::High, Some(1024));
    assert_eq!(body["max_tokens"], json!(1024));
    assert!(body.get("thinking").is_none(), "{body}");
    // Adaptive thinking has no budget: the cap goes out as given.
    let body = sent("claude-sonnet-5", Effort::High, Some(4096));
    assert_eq!(body["max_tokens"], json!(4096));
    assert_eq!(body["thinking"], json!({"type": "adaptive"}));
}

#[test]
fn the_last_tool_carries_the_cache_breakpoint() {
    let spec = |n: &str| haest_core::provider::ToolSpec {
        name: n.into(),
        description: "d".into(),
        parameters: json!({"type": "object"}),
    };
    let tools = to_anthropic_tools(&[spec("A"), spec("B")], CacheTtl::FiveMinutes);
    assert!(tools[0].get("cache_control").is_none());
    assert_eq!(tools[1]["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(tools[1]["input_schema"], json!({"type": "object"}));
}

#[test]
fn the_key_and_header_values_never_show_in_debug_output() {
    let mut o = AnthropicOptions::new("anthropic", "sk-ant-SECRET");
    o.headers = vec![("x-proxy-key".into(), "HEADER-SECRET".into())];
    o.base_url = Some("https://user:URL-SECRET@gw.test/v1".into());
    let shown = format!("{o:?}");
    assert!(!shown.contains("SECRET"), "{shown}");
    assert!(shown.contains("gw.test/v1"), "{shown}");
    assert!(
        shown.contains("anthropic") && shown.contains("x-proxy-key"),
        "{shown}"
    );
}
