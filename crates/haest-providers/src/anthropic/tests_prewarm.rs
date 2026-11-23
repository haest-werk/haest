//! A cache pre-warm on Anthropic's own API: the request Anthropic documents
//! (`max_tokens: 0`, not streamed, the real request's thinking), its
//! non-streamed answer, and the refusal under a thinking budget.

use haest_core::{
    ProviderRequest,
    cache::{CacheTtl, PrewarmRefused},
    messages::{Message, StopReason},
    provider::Effort,
};
use serde_json::json;

use super::*;
use crate::testing::{FakeHttp, FakeReply, collect};

fn warm(model: &str) -> ProviderRequest {
    ProviderRequest::new(model, vec![Message::user("hi")])
        .with_system("sys")
        .with_effort(Effort::High)
        .with_cache_ttl(CacheTtl::OneHour)
        .with_prewarm(true)
}

/// The answer Anthropic's prompt-caching page shows for a pre-warm, with an
/// hour-long write.
fn prewarmed() -> serde_json::Value {
    json!({
        "id": "msg_01", "type": "message", "role": "assistant", "content": [],
        "model": "claude-opus-5-5", "stop_reason": "max_tokens", "stop_sequence": null,
        "usage": {
            "input_tokens": 8, "cache_creation_input_tokens": 5120, "cache_read_input_tokens": 0,
            "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 5120},
            "output_tokens": 0,
        },
    })
}

fn provider(t: &FakeHttp) -> AnthropicProvider {
    AnthropicProvider::new(AnthropicOptions::new("anthropic", "k"), t.clone())
}

#[test]
fn a_pre_warm_is_the_real_request_capped_at_zero_and_not_streamed() {
    let real = build_anthropic_params("anthropic", &warm("claude-opus-5-5").with_prewarm(false));
    let body = build_anthropic_params("anthropic", &warm("claude-opus-5-5"));
    assert_eq!(body["max_tokens"], 0);
    assert!(body.get("stream").is_none(), "{body}");
    for same in ["system", "messages", "thinking", "output_config", "model"] {
        assert_eq!(body[same], real[same], "{same}");
    }
    assert_eq!(body["thinking"], json!({"type": "adaptive"}));
}

#[tokio::test]
async fn a_pre_warm_reads_the_whole_answer_as_one_message() {
    let t = FakeHttp::new([FakeReply::json(&prewarmed())]);
    let (events, msg) = collect(provider(&t).stream(warm("claude-opus-5-5"))).await;
    assert_eq!(events.len(), 1, "only Done: {events:?}");
    assert_eq!(msg.stop_reason, StopReason::MaxTokens);
    assert!(msg.content.is_empty());
    let u = &msg.usage;
    assert_eq!(
        (u.input, u.cache_write, u.cache_write_1h, u.output),
        (8, 5120, 5120, 0)
    );
    assert_eq!(t.last_body()["max_tokens"], 0);
}

#[tokio::test]
async fn a_refused_or_unreadable_pre_warm_ends_as_an_error() {
    let t = FakeHttp::new([
        FakeReply::status(
            400,
            r#"{"type":"error","error":{"message":"no warm for you"}}"#,
        ),
        FakeReply::json(
            &json!({"type": "error", "error": {"type": "overloaded_error", "message": "busy"}}),
        ),
        FakeReply::sse("data: not json\n\n"),
    ]);
    let p = AnthropicProvider::new(AnthropicOptions::new("anthropic", "k"), t.clone())
        .with_retry(crate::RetryPolicy::none());
    let mut said = Vec::new();
    for _ in 0..3 {
        let (_, msg) = collect(p.stream(warm("claude-opus-5-5"))).await;
        assert_eq!(msg.stop_reason, StopReason::Error);
        said.push(msg.error_message.unwrap_or_default());
    }
    assert!(
        said[0].starts_with("400") && said[0].contains("no warm for you"),
        "{said:?}"
    );
    assert_eq!(said[1], "busy");
    assert!(!said[2].is_empty(), "{said:?}");
}

#[tokio::test]
async fn budget_thinking_is_refused_before_anything_is_sent() {
    let t = FakeHttp::new([]);
    let p = provider(&t);
    let req = warm("claude-sonnet-4-5");
    assert_eq!(
        p.check_prewarm(&req),
        Err(PrewarmRefused::BudgetThinking {
            model: "claude-sonnet-4-5".into()
        })
    );
    let (_, msg) = collect(p.stream(req)).await;
    assert_eq!(msg.stop_reason, StopReason::Error);
    assert!(msg.error_message.is_some());
    assert!(t.requests().is_empty());
    // The same model with no thinking asked for warms like any other.
    let mut no_effort = warm("claude-sonnet-4-5");
    no_effort.effort = None;
    assert_eq!(p.check_prewarm(&no_effort), Ok(()));
}
