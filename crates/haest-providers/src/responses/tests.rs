use std::sync::Arc;

use haest_core::{
    messages::{AssistantMessage, AssistantPart, StopReason, ToolCall, Usage},
    provider::{Provider, ProviderEvent},
};
use serde_json::{Value, json};

use super::*;
use crate::testing::{FakeEnd, FakeReply, FakeTransport, collect, kinds, request, typed_sse};

fn provider(t: &Arc<FakeTransport>) -> ResponsesProvider {
    let opts = ResponsesOptions {
        id: "openai".into(),
        api_key: "k".into(),
        ..Default::default()
    };
    ResponsesProvider::new(opts, t.clone())
}

async fn run(events: &[Value]) -> (Vec<ProviderEvent>, AssistantMessage) {
    let t = FakeTransport::new([FakeReply::sse(typed_sse(events))]);
    collect(provider(&t).stream(request("gpt-5.5"))).await
}

fn usage() -> Value {
    json!({
        "input_tokens": 100,
        "input_tokens_details": {"cached_tokens": 60, "cache_write_tokens": 0},
        "output_tokens": 20,
        "output_tokens_details": {"reasoning_tokens": 12},
        "total_tokens": 120,
    })
}

fn added(index: u64, item: Value) -> Value {
    json!({"type": "response.output_item.added", "output_index": index, "item": item})
}

fn item_done(index: u64, item: Value) -> Value {
    json!({"type": "response.output_item.done", "output_index": index, "item": item})
}

fn completed() -> Value {
    json!({"type": "response.completed", "response": {"model": "gpt-5.5-2026", "usage": usage()}})
}

fn reasoning_turn() -> Vec<Value> {
    vec![
        added(0, json!({"type": "reasoning", "id": "rs_1", "summary": []})),
        json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "summary_index": 0, "delta": "thin"}),
        json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "summary_index": 0, "delta": "k"}),
        item_done(
            0,
            json!({"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "think"}], "encrypted_content": "ENC"}),
        ),
        added(
            1,
            json!({"type": "message", "id": "msg_1", "role": "assistant", "content": []}),
        ),
        json!({"type": "response.output_text.delta", "output_index": 1, "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "output_index": 1, "delta": "lo"}),
        item_done(
            1,
            json!({"type": "message", "content": [{"type": "output_text", "text": "Hello"}]}),
        ),
        added(
            2,
            json!({"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "Read", "arguments": ""}),
        ),
        json!({"type": "response.function_call_arguments.delta", "output_index": 2, "delta": "{\"file_"}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 2, "delta": "path\":\"a\"}"}),
        item_done(
            2,
            json!({"type": "function_call", "call_id": "call_1", "name": "Read", "arguments": "{\"file_path\":\"a\"}"}),
        ),
    ]
}

fn text(t: &str) -> AssistantPart {
    AssistantPart::Text { text: t.into() }
}

#[tokio::test]
async fn reasoning_summary_text_a_streamed_function_call_and_usage_fold_into_one_message() {
    let mut events = reasoning_turn();
    events.push(completed());
    let (out, msg) = run(&events).await;
    assert_eq!(
        kinds(&out),
        [
            "thinking_delta",
            "thinking_delta",
            "text_delta",
            "text_delta",
            "tool_call_start",
            "done"
        ]
    );
    assert_eq!(
        msg.content,
        vec![
            AssistantPart::Thinking {
                thinking: "think".into(),
                signature: Some("ENC".into()),
                redacted: None
            },
            text("Hello"),
            AssistantPart::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "Read".into(),
                input: json!({"file_path": "a"}).as_object().unwrap().clone(),
            }),
        ]
    );
    assert_eq!(msg.stop_reason, StopReason::ToolUse);
    assert_eq!(msg.model, "gpt-5.5-2026");
    assert_eq!(msg.provider, "openai");
    assert_eq!(
        msg.usage,
        Usage {
            input: 40,
            output: 20,
            cache_read: 60,
            cache_write: 0,
            reasoning: 12,
            cost_usd: None
        }
    );
}

#[tokio::test]
async fn a_text_only_response_ends_the_turn() {
    let mut events = reasoning_turn()[4..8].to_vec();
    events.push(completed());
    let (_, msg) = run(&events).await;
    assert_eq!(msg.content, vec![text("Hello")]);
    assert_eq!(msg.stop_reason, StopReason::EndTurn);
}

#[tokio::test]
async fn incomplete_on_max_output_tokens_is_max_tokens_and_keeps_the_usage() {
    let mut events = reasoning_turn()[4..6].to_vec();
    events.push(json!({
        "type": "response.incomplete",
        "response": {"usage": usage(), "incomplete_details": {"reason": "max_output_tokens"}},
    }));
    let (_, msg) = run(&events).await;
    assert_eq!(msg.stop_reason, StopReason::MaxTokens);
    assert_eq!(msg.content, vec![text("Hel")]);
    assert_eq!(msg.usage.output, 20);
}

#[tokio::test]
async fn response_failed_and_error_events_end_as_error_keep_text_drop_half_built_calls() {
    let failures = [
        json!({"type": "response.failed", "response": {"error": {"code": "server_error", "message": "boom"}}}),
        json!({"type": "error", "code": "server_error", "message": "boom", "param": null}),
    ];
    for failure in failures {
        let mut events = reasoning_turn()[4..10].to_vec();
        events.push(failure);
        let (_, msg) = run(&events).await;
        assert_eq!(msg.stop_reason, StopReason::Error);
        assert_eq!(msg.error_message.as_deref(), Some("boom"));
        assert_eq!(msg.content, vec![text("Hello")]);
    }
}

#[tokio::test]
async fn an_aborted_stream_ends_as_aborted_with_no_error_message() {
    // Cancelled before the call, and cancelled mid-stream while the body hangs.
    let t = FakeTransport::new([]);
    let req = request("gpt-5.5");
    req.cancel.cancel();
    let (_, msg) = collect(provider(&t).stream(req)).await;
    assert_eq!(msg.stop_reason, StopReason::Aborted);
    assert_eq!(msg.error_message, None);

    let hanging = FakeReply::sse(typed_sse(&reasoning_turn()[4..7])).then(FakeEnd::Hang);
    let t = FakeTransport::new([hanging]);
    let req = request("gpt-5.5");
    let cancel = req.cancel.clone();
    let mut stream = provider(&t).stream(req);
    let mut seen = Vec::new();
    use futures::StreamExt;
    while let Some(ev) = stream.next().await {
        if matches!(&ev, ProviderEvent::TextDelta(t) if t == "lo") {
            cancel.cancel();
        }
        seen.push(ev);
    }
    let Some(ProviderEvent::Done(msg)) = seen.last() else {
        panic!("{seen:?}")
    };
    assert_eq!(msg.stop_reason, StopReason::Aborted);
    assert_eq!(msg.error_message, None);
    assert_eq!(msg.content, vec![text("Hello")]);
}

#[tokio::test]
async fn unparseable_function_arguments_are_surfaced_not_thrown() {
    let (_, msg) = run(&[
        added(
            0,
            json!({"type": "function_call", "call_id": "c", "name": "X", "arguments": ""}),
        ),
        item_done(
            0,
            json!({"type": "function_call", "call_id": "c", "name": "X", "arguments": "{bad"}),
        ),
        completed(),
    ])
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
async fn a_refusal_item_ends_as_refusal() {
    let (_, msg) = run(&[
        added(0, json!({"type": "message", "content": []})),
        item_done(
            0,
            json!({"type": "message", "content": [{"type": "refusal", "refusal": "no"}]}),
        ),
        completed(),
    ])
    .await;
    assert_eq!(msg.stop_reason, StopReason::Refusal);
    assert_eq!(msg.content, vec![text("no")]);
}

#[test]
fn the_wire_request_goes_to_responses() {
    let t = FakeTransport::new([]);
    let req = provider(&t).http_request(&request("gpt-5.5"));
    assert_eq!(req.url, "https://api.openai.com/v1/responses");
    assert_eq!(req.header("authorization"), Some("Bearer k"));
}
