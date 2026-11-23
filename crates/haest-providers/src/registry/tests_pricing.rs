//! The registry prices what Anthropic's and OpenAI's own APIs answer from
//! the catalog, since neither reports a cost; a providers.json entry of
//! either type adds its own prices over the catalog's.

use serde_json::json;

use super::{
    tests::{entries, keys, over},
    *,
};
use crate::{
    catalog::Catalog,
    testing::{FakeHttp, FakeReply, collect, request, typed_sse},
};

fn anthropic_answer(model: &str) -> FakeReply {
    FakeReply::sse(typed_sse(&[
        json!({"type": "message_start", "message": {"model": model, "usage": {"input_tokens": 1_000_000}}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 0}}),
    ]))
}

fn responses_answer() -> FakeReply {
    let usage = json!({"input_tokens": 1_000_000, "output_tokens": 0});
    FakeReply::sse(typed_sse(&[
        json!({"type": "response.completed", "response": {"usage": usage}}),
    ]))
}

async fn cost(reg: &ProviderRegistry, spec: &str) -> Option<f64> {
    let resolved = reg.resolve(spec).unwrap();
    let (_, msg) = collect(resolved.provider().stream(request(resolved.model()))).await;
    msg.usage.cost_usd
}

#[tokio::test]
async fn direct_anthropic_and_openai_answers_are_priced_from_the_catalog() {
    let t = FakeHttp::new([
        anthropic_answer("claude-opus-5-5"),
        responses_answer(),
        anthropic_answer("claude-unknown"),
    ]);
    let k = keys(&[("ANTHROPIC_API_KEY", "k"), ("OPENAI_API_KEY", "k")]);
    let reg = over(ProviderRegistry::new(k), &t);
    let catalog = Catalog::builtin();
    for spec in ["anthropic:claude-opus-5-5", "openai:gpt-6.1-sol"] {
        let input = catalog.pricing(spec).unwrap().input;
        assert_eq!(
            cost(&reg, spec).await,
            Some(input),
            "1M input tokens of {spec}"
        );
    }
    assert_eq!(cost(&reg, "anthropic:claude-unknown").await, None);
}

#[tokio::test]
async fn an_anthropic_entry_prices_its_models_over_the_catalog() {
    let t = FakeHttp::new([anthropic_answer("house-model")]);
    let custom = entries(json!({"gateway": {
        "type": "anthropic",
        "baseURL": "https://gw.example",
        "pricing": {"house-model": {"input": 3, "cachedInput": 0.3, "output": 15, "cacheWrite1h": 6}},
    }}));
    let reg = over(ProviderRegistry::new(keys(&[])).with_custom(custom), &t);
    assert_eq!(cost(&reg, "gateway:house-model").await, Some(3.0));
}
