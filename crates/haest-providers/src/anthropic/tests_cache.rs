//! The prompt cache on Anthropic's own API: the TTL on every breakpoint, the
//! usage of each lifetime as the stream reports it, and the price it books.

use haest_core::{
    ProviderRequest,
    cache::CacheTtl,
    catalog::ModelPricing,
    messages::{Message, StopReason},
    provider::{Effort, ToolSpec},
};
use serde_json::json;

use super::*;
use crate::testing::{FakeHttp, FakeReply, breakpoints, collect, typed_sse};

fn tool(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: "d".into(),
        parameters: json!({"type": "object"}),
    }
}

fn turn(ttl: CacheTtl) -> ProviderRequest {
    ProviderRequest::new("claude-opus-5-5", vec![Message::user("hi")])
        .with_system("sys")
        .with_tools(vec![tool("A"), tool("B")])
        .with_effort(Effort::High)
        .with_cache_ttl(ttl)
}

/// The default TTL sends what haest sent before it had one, to the byte.
#[test]
fn a_five_minute_request_is_the_request_haest_always_sent() {
    let body = build_anthropic_params("anthropic", &turn(CacheTtl::FiveMinutes));
    let before = json!({
        "model": "claude-opus-5-5",
        "max_tokens": 32_000,
        "system": [{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}],
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}
        ]}],
        "stream": true,
        "tools": [
            {"name": "A", "description": "d", "input_schema": {"type": "object"}},
            {"name": "B", "description": "d", "input_schema": {"type": "object"},
             "cache_control": {"type": "ephemeral"}}
        ],
        "thinking": {"type": "adaptive"},
        "output_config": {"effort": "high"},
    });
    assert_eq!(body.to_string(), before.to_string());
}

#[test]
fn an_hour_long_ttl_rides_on_every_breakpoint() {
    let body = build_anthropic_params("anthropic", &turn(CacheTtl::OneHour));
    let hour = json!({"type": "ephemeral", "ttl": "1h"});
    assert_eq!(breakpoints(&body), [hour.clone(), hour.clone(), hour]);
    assert_eq!(body["system"][0]["cache_control"]["ttl"], "1h");
    assert_eq!(body["tools"][1]["cache_control"]["ttl"], "1h");
}

#[test]
fn the_messages_api_gets_no_cache_key() {
    let keyed = turn(CacheTtl::FiveMinutes).with_cache_key("session-1");
    assert_eq!(
        build_anthropic_params("anthropic", &keyed),
        build_anthropic_params("anthropic", &turn(CacheTtl::FiveMinutes))
    );
}

/// A stream as Anthropic sends it with an hour-long write: the split in
/// `message_start`, the cumulative counts again in `message_delta`.
fn hour_long_write() -> String {
    let usage = json!({
        "input_tokens": 8,
        "cache_read_input_tokens": 1_000_000,
        "cache_creation_input_tokens": 3_000_000,
        "cache_creation": {"ephemeral_5m_input_tokens": 1_000_000, "ephemeral_1h_input_tokens": 2_000_000},
        "output_tokens": 1,
    });
    typed_sse(&[
        json!({"type": "message_start", "message": {"model": "claude-opus-5-5", "usage": usage}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": "ok"}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {
            "input_tokens": 1_000_000, "cache_read_input_tokens": 1_000_000,
            "cache_creation_input_tokens": 3_000_000, "output_tokens": 500_000,
        }}),
    ])
}

#[tokio::test]
async fn each_lifetime_of_a_cache_write_is_counted_and_priced() {
    let t = FakeHttp::new([FakeReply::sse(hour_long_write())]);
    let price = ModelPricing::new(4.0, 0.2, 20.0)
        .with_cache_write(5.0)
        .with_cache_write_1h(8.0);
    let opts = AnthropicOptions::new("anthropic", "k").with_pricing([("claude-opus-5-5", price)]);
    let provider = AnthropicProvider::new(opts, t.clone());
    let (_, msg) = collect(provider.stream(turn(CacheTtl::OneHour))).await;
    assert_eq!(msg.stop_reason, StopReason::EndTurn);
    let u = &msg.usage;
    // The delta's input, grown past the start's (a server tool read more), wins.
    assert_eq!(
        (u.input, u.cache_read, u.output),
        (1_000_000, 1_000_000, 500_000)
    );
    assert_eq!((u.cache_write, u.cache_write_1h), (3_000_000, 2_000_000));
    // $4 + $0.20 + 1M at $5 + 2M at $8 + 0.5M at $20.
    let cost = u.cost_usd.unwrap();
    assert!((cost - 35.2).abs() < 1e-9, "{cost}");
}

#[tokio::test]
async fn a_model_with_no_price_books_no_cost() {
    let t = FakeHttp::new([FakeReply::sse(hour_long_write())]);
    let provider = AnthropicProvider::new(AnthropicOptions::new("anthropic", "k"), t.clone());
    let (_, msg) = collect(provider.stream(turn(CacheTtl::FiveMinutes))).await;
    assert_eq!(msg.usage.cache_write_1h, 2_000_000);
    assert_eq!(msg.usage.cost_usd, None);
}

/// A stream from before Anthropic split the write by lifetime reads as it did.
#[tokio::test]
async fn a_write_with_no_lifetime_split_is_all_default_lifetime() {
    let body = typed_sse(&[
        json!({"type": "message_start", "message": {"usage": {
            "input_tokens": 10, "cache_read_input_tokens": 90, "cache_creation_input_tokens": 5,
        }}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 7}}),
    ]);
    let t = FakeHttp::new([FakeReply::sse(body)]);
    let provider = AnthropicProvider::new(AnthropicOptions::new("anthropic", "k"), t.clone());
    let (_, msg) = collect(provider.stream(turn(CacheTtl::FiveMinutes))).await;
    let u = &msg.usage;
    assert_eq!(
        (
            u.input,
            u.cache_read,
            u.cache_write,
            u.cache_write_1h,
            u.output
        ),
        (10, 90, 5, 0, 7)
    );
}
