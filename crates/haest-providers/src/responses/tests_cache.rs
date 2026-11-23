//! The prompt cache on OpenAI's Responses API: no routing key, the partition
//! key only to OpenAI's own API, a price for an API that reports none, and no
//! pre-warm.

use haest_core::{
    cache::{CacheTtl, PrewarmRefused},
    catalog::ModelPricing,
    messages::StopReason,
};
use serde_json::json;

use super::{convert::build_responses_body, *};
use crate::testing::{FakeHttp, FakeReply, collect, request, typed_sse};

#[test]
fn a_conversations_routing_key_is_not_sent_to_openai() {
    let keyed = request("gpt-6.1-sol").with_routing_key("session-1");
    let own = build_responses_body(&ResponsesOptions::new("openai", "k"), &keyed);
    assert!(own.get("prompt_cache_key").is_none());
    assert_eq!(
        own,
        build_responses_body(
            &ResponsesOptions::new("openai", "k"),
            &request("gpt-6.1-sol")
        )
    );
}

#[test]
fn an_explicit_partition_key_goes_out_only_to_openais_own_api() {
    let parted = request("gpt-6.1-sol").with_partition_key("customer-7");
    let own = build_responses_body(&ResponsesOptions::new("openai", "k"), &parted);
    assert_eq!(own["prompt_cache_key"], "customer-7");
    let proxy = ResponsesOptions::new("openai", "k").with_base_url("https://proxy.example/v1");
    assert!(
        build_responses_body(&proxy, &parted)
            .get("prompt_cache_key")
            .is_none()
    );
}

#[test]
fn openai_caches_for_its_own_time_so_the_ttl_is_not_sent() {
    let opts = ResponsesOptions::new("openai", "k");
    let hour = request("gpt-6.1-sol").with_cache_ttl(CacheTtl::OneHour);
    assert_eq!(
        build_responses_body(&opts, &hour),
        build_responses_body(&opts, &request("gpt-6.1-sol"))
    );
}

#[tokio::test]
async fn an_answer_is_priced_from_the_configured_price() {
    let usage = json!({
        "input_tokens": 4_000_000,
        "input_tokens_details": {"cached_tokens": 1_000_000, "cache_write_tokens": 2_000_000},
        "output_tokens": 1_000_000,
    });
    let done = json!({"type": "response.completed", "response": {"usage": usage}});
    let t = FakeHttp::new([FakeReply::sse(typed_sse(&[done]))]);
    let price = ModelPricing::new(2.0, 0.1, 10.0).with_cache_write(2.5);
    let opts = ResponsesOptions::new("openai", "k").with_pricing([("gpt-6.1-sol", price)]);
    let (_, msg) = collect(ResponsesProvider::new(opts, t).stream(request("gpt-6.1-sol"))).await;
    let u = &msg.usage;
    assert_eq!(
        (u.input, u.cache_read, u.cache_write),
        (1_000_000, 1_000_000, 2_000_000)
    );
    // 1M at $2 + 1M at $0.10 + 2M at $2.50 + 1M at $10.
    let cost = u.cost_usd.unwrap();
    assert!((cost - 17.1).abs() < 1e-9, "{cost}");
}

#[tokio::test]
async fn a_pre_warm_is_refused_and_nothing_is_sent() {
    let t = FakeHttp::new([]);
    let provider = ResponsesProvider::new(ResponsesOptions::new("openai", "k"), t.clone());
    let req = request("gpt-6.1-sol").with_prewarm(true);
    assert!(matches!(
        provider.check_prewarm(&req),
        Err(PrewarmRefused::Unsupported { provider }) if provider == "openai"
    ));
    let (_, msg) = collect(provider.stream(req)).await;
    assert_eq!(msg.stop_reason, StopReason::Error);
    assert!(t.requests().is_empty());
}
