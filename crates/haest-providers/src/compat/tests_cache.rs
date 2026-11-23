//! The prompt cache over Chat Completions: the TTL and the tools breakpoint
//! for Anthropic models on OpenRouter, the cache key on the servers that
//! document one, and the pre-warm only for Anthropic models on OpenRouter.

use haest_core::{
    cache::{CacheTtl, PrewarmRefused},
    messages::StopReason,
    provider::{Effort, ProviderRequest, ToolSpec, WebSearchEngine, WebSearchOptions},
};
use serde_json::{Value, json};

use super::{
    tests::{chunk, openrouter, usage_chunk},
    *,
};
use crate::testing::{FakeHttp, FakeReply, breakpoints, chat_sse, collect, request};

fn tooled(model: &str, ttl: CacheTtl) -> ProviderRequest {
    let tool = |name: &str| ToolSpec {
        name: name.into(),
        description: "d".into(),
        parameters: json!({"type": "object"}),
    };
    let mut req = request(model).with_cache_ttl(ttl);
    req.tools = vec![tool("A"), tool("B")];
    req.web_search = Some(WebSearchOptions::new(WebSearchEngine::Exa));
    req
}

#[test]
fn an_anthropic_model_on_openrouter_caches_its_last_function_tool_with_the_ttl() {
    for (ttl, marker) in [
        (CacheTtl::FiveMinutes, json!({"type": "ephemeral"})),
        (CacheTtl::OneHour, json!({"type": "ephemeral", "ttl": "1h"})),
    ] {
        let body = build_compat_body(&openrouter(), &tooled("anthropic/claude-sonnet-5", ttl));
        let tools = body["tools"].as_array().unwrap();
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"], marker, "the last function tool");
        assert!(tools[2].get("cache_control").is_none(), "the server tool");
        // System prompt, last tool, newest user message: within Anthropic's four.
        assert_eq!(breakpoints(&body), [marker.clone(), marker.clone(), marker]);
    }
}

#[test]
fn a_google_model_keeps_its_two_default_breakpoints_and_others_get_none() {
    let body = build_compat_body(
        &openrouter(),
        &tooled("google/gemini-3-pro", CacheTtl::OneHour),
    );
    let plain = json!({"type": "ephemeral"});
    assert_eq!(breakpoints(&body), [plain.clone(), plain]);
    let other = build_compat_body(&openrouter(), &tooled("openai/gpt-6", CacheTtl::OneHour));
    assert!(breakpoints(&other).is_empty());
    let local = CompatOptions::new("local", "k").with_base_url("http://localhost:1/v1");
    let direct = build_compat_body(
        &local,
        &tooled("anthropic/claude-sonnet-5", CacheTtl::OneHour),
    );
    assert!(breakpoints(&direct).is_empty());
}

fn key_fields(opts: &CompatOptions, req: &ProviderRequest) -> Vec<(String, Value)> {
    let body = build_compat_body(opts, req);
    ["session_id", "prompt_cache_key"]
        .into_iter()
        .filter_map(|f| body.get(f).map(|v| (f.to_string(), v.clone())))
        .collect()
}

#[test]
fn the_cache_key_routes_on_openrouter_and_never_partitions_openai() {
    let field = |f: &str, v: &str| vec![(f.to_string(), json!(v))];
    let keyed = |key: &str| request("m").with_cache_key(key);
    assert_eq!(
        key_fields(&openrouter(), &keyed("s-1")),
        field("session_id", "s-1")
    );
    assert_eq!(key_fields(&openrouter(), &keyed(&"x".repeat(257))), []);
    let openai = CompatOptions::new("direct", "k");
    let eu = CompatOptions::new("eu", "k").with_base_url("https://eu.api.openai.com/v1");
    let local = CompatOptions::new("local", "k").with_base_url("http://localhost:1234/v1");
    for opts in [&openai, &eu, &local] {
        assert_eq!(key_fields(opts, &keyed("s-1")), [], "{opts:?}");
    }
    let body = build_compat_body(&openrouter(), &request("m"));
    assert!(body.get("session_id").is_none() && body.get("prompt_cache_key").is_none());
}

#[test]
fn an_explicit_partition_key_goes_to_openais_own_api_only() {
    let field = |f: &str, v: &str| vec![(f.to_string(), json!(v))];
    let parted = request("m").with_partition_key("customer-7");
    let openai = CompatOptions::new("direct", "k");
    let eu = CompatOptions::new("eu", "k").with_base_url("https://eu.api.openai.com/v1");
    for opts in [&openai, &eu] {
        assert_eq!(
            key_fields(opts, &parted),
            field("prompt_cache_key", "customer-7")
        );
    }
    let local = CompatOptions::new("local", "k").with_base_url("http://localhost:1234/v1");
    assert_eq!(key_fields(&local, &parted), []);
    assert_eq!(key_fields(&openrouter(), &parted), []);
}

#[tokio::test]
async fn an_anthropic_pre_warm_on_openrouter_is_the_real_request_capped_at_one_token() {
    let usage = json!({"prompt_tokens": 8_420, "completion_tokens": 1,
        "prompt_tokens_details": {"cached_tokens": 8_412}});
    let reply = chat_sse(&[chunk(json!({"content": "Hi"})), usage_chunk(usage)]);
    let t = FakeHttp::new([FakeReply::sse(reply)]);
    let real = tooled("anthropic/claude-haiku-5.5", CacheTtl::OneHour)
        .with_effort(Effort::High)
        .with_max_tokens(8_000)
        .with_cache_key("s-1");
    let provider = CompatProvider::new(openrouter(), t.clone());
    assert_eq!(
        provider.check_prewarm(&real.clone().with_prewarm(true)),
        Ok(())
    );
    let (_, msg) = collect(provider.stream(real.clone().with_prewarm(true))).await;
    assert_eq!((msg.usage.cache_read, msg.usage.cache_write), (8_412, 0));

    let mut sent = t.last_body();
    assert_eq!(sent["max_tokens"], 1);
    sent["max_tokens"] = json!(8_000);
    assert_eq!(
        sent,
        build_compat_body(&openrouter(), &real),
        "otherwise the real request: messages, breakpoints, tools, effort, session"
    );
}

#[test]
fn a_pre_warm_without_a_cap_of_its_own_still_sends_one_token() {
    let warm = request("anthropic/claude-opus-5.5").with_prewarm(true);
    assert_eq!(build_compat_body(&openrouter(), &warm)["max_tokens"], 1);
    let real = build_compat_body(&openrouter(), &request("anthropic/claude-opus-5.5"));
    assert!(real.get("max_tokens").is_none());
}

#[tokio::test]
async fn a_budget_thinking_model_warms_only_when_it_does_not_think() {
    for model in [
        "anthropic/claude-sonnet-4.5",
        "anthropic/claude-opus-4.1",
        "anthropic/claude-haiku-4.5:batch",
        "anthropic/claude-3.7-sonnet",
    ] {
        let t = FakeHttp::new([]);
        let provider = CompatProvider::new(openrouter(), t.clone());
        let thinking = request(model).with_effort(Effort::High).with_prewarm(true);
        assert!(
            matches!(
                provider.check_prewarm(&thinking),
                Err(PrewarmRefused::BudgetThinking { model: m }) if m == model
            ),
            "{model}"
        );
        let (_, msg) = collect(provider.stream(thinking)).await;
        assert_eq!(msg.stop_reason, StopReason::Error);
        assert!(t.requests().is_empty(), "{model}: nothing sent");
        for effort in [None, Some(Effort::Off)] {
            let mut plain = request(model).with_prewarm(true);
            plain.effort = effort;
            assert_eq!(provider.check_prewarm(&plain), Ok(()), "{model} {effort:?}");
        }
    }
    let adaptive = request("anthropic/claude-sonnet-4.6")
        .with_effort(Effort::Max)
        .with_prewarm(true);
    let provider = CompatProvider::new(openrouter(), FakeHttp::new([]));
    assert_eq!(provider.check_prewarm(&adaptive), Ok(()));
}

#[tokio::test]
async fn a_pre_warm_elsewhere_is_refused_and_nothing_is_sent() {
    let local = CompatOptions::new("local", "k").with_base_url("http://localhost:1/v1");
    for (opts, model) in [
        (openrouter(), "openai/gpt-6.1-sol"),
        (openrouter(), "google/gemini-4-argon"),
        (local, "anthropic/claude-opus-5.5"),
    ] {
        let t = FakeHttp::new([]);
        let provider = CompatProvider::new(opts.clone(), t.clone());
        let req = request(model).with_prewarm(true);
        assert!(
            matches!(
                provider.check_prewarm(&req),
                Err(PrewarmRefused::Unsupported { provider }) if provider == opts.id()
            ),
            "{model} on {}",
            opts.id()
        );
        let (_, msg) = collect(provider.stream(req)).await;
        assert_eq!(msg.stop_reason, StopReason::Error);
        assert!(t.requests().is_empty(), "{model}: nothing sent");
    }
}
