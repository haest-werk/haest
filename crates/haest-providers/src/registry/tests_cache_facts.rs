//! A model's cache facts: the catalog's agree with what its provider sends,
//! and a providers.json entry's stand in place of them.

use haest_core::{
    cache::CacheTtl,
    catalog::{CacheFacts, ModelFacts},
};
use serde_json::{Value, json};

use super::{
    tests::{entries, keys, over},
    *,
};
use crate::{
    CatalogFacts,
    catalog::Catalog,
    testing::{FakeHttp, FakeReply, request},
};

/// The bodies `spec` sends for a pre-warm, when its provider takes one, and
/// for a request asking for an hour-long cache.
async fn sent(spec: &str) -> (Option<Value>, Value) {
    let t = FakeHttp::new([FakeReply::status(400, "no"), FakeReply::status(400, "no")]);
    let k = keys(&[
        ("ANTHROPIC_API_KEY", "k"),
        ("OPENAI_API_KEY", "k"),
        ("OPENROUTER_API_KEY", "k"),
    ]);
    let resolved = over(ProviderRegistry::new(k), &t).resolve(spec).unwrap();
    let provider = resolved.provider();
    let warm = request(resolved.model()).with_prewarm(true);
    let prewarm = match provider.check_prewarm(&warm) {
        Ok(()) => {
            let _ = crate::testing::collect(provider.stream(warm)).await;
            Some(t.last_body())
        }
        Err(_) => None,
    };
    let hour = request(resolved.model()).with_cache_ttl(CacheTtl::OneHour);
    let _ = crate::testing::collect(provider.stream(hour)).await;
    (prewarm, t.last_body())
}

/// Whether `body` holds a breakpoint that lives an hour, anywhere.
fn has_hour(body: &Value) -> bool {
    match body {
        Value::Object(map) => map.get("ttl") == Some(&json!("1h")) || map.values().any(has_hour),
        Value::Array(items) => items.iter().any(has_hour),
        _ => false,
    }
}

#[tokio::test]
async fn every_catalog_cache_fact_matches_what_its_provider_sends() {
    let rows: Vec<_> = Catalog::builtin()
        .models()
        .filter_map(|m| Some((m.spec.clone(), m.cache.clone()?)))
        .collect();
    assert!(rows.len() >= 10, "the Anthropic, OpenAI and Google rows");
    for (spec, facts) in rows {
        let (prewarm, hour) = sent(&spec).await;
        let cap = prewarm.map(|body| body["max_tokens"].as_u64().unwrap());
        assert_eq!(
            cap,
            facts.prewarm_max_tokens.map(u64::from),
            "{spec}: pre-warm"
        );
        assert_eq!(has_hour(&hour), facts.ttl_1h, "{spec}: an hour's TTL");
    }
}

#[test]
fn every_anthropic_row_knows_its_minimum_and_only_the_direct_api_takes_tool_changes() {
    for m in Catalog::builtin().models() {
        let Some(facts) = &m.cache else { continue };
        let anthropic = m.family.as_deref() == Some("anthropic");
        if anthropic {
            assert!(facts.min_cacheable_tokens.is_some(), "{}", m.spec);
        }
        let direct = m.spec.starts_with("anthropic:");
        assert_eq!(facts.tool_changes, direct, "{}", m.spec);
    }
}

/// Keep-warm pings only a priced model ([`ping_pays`](haest_core::cache::ping_pays)),
/// so a row that takes a pre-warm without a price could never be kept warm:
/// the CLI's default model, an OpenRouter row, was such a row.
#[test]
fn every_row_that_takes_a_pre_warm_has_the_prices_keep_warm_needs() {
    use std::time::Duration;

    use haest_core::cache::ping_pays;

    let warmable: Vec<_> = Catalog::builtin()
        .models()
        .filter(|m| {
            m.cache
                .as_ref()
                .is_some_and(|c| c.prewarm_max_tokens.is_some())
        })
        .collect();
    assert!(warmable.iter().any(|m| m.spec.starts_with("openrouter:")));
    for m in warmable {
        let pricing = m.pricing.as_ref();
        for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour] {
            assert!(
                ping_pays(pricing, ttl, Duration::from_secs(5 * 60)),
                "{}: a ping at {ttl:?} never pays: {pricing:?}",
                m.spec
            );
        }
    }
}

#[test]
fn a_providers_json_entry_states_cache_facts_in_place_of_the_catalogs() {
    let file = entries(json!({
        "anthropic": {"type": "anthropic", "cache": {"claude-opus-5-5": {"ttl1h": false}},
            "pricing": {"claude-opus-5-5": {"input": 1, "cachedInput": 0.1, "output": 2}}},
        "gateway": {"type": "anthropic", "baseURL": "https://gw.example",
            "cache": {"house": {"minCacheableTokens": 2048, "prewarmMaxTokens": 0}}},
    }));
    let facts = CatalogFacts::new(file);
    let opus = "anthropic:claude-opus-5-5";
    assert_eq!(facts.cache_facts(opus), Some(CacheFacts::new()));
    assert_eq!(facts.pricing(opus).map(|p| p.input), Some(1.0));
    let row = facts.model_info(opus).unwrap();
    assert_eq!(
        (row.cache, row.pricing.map(|p| p.output)),
        (Some(CacheFacts::new()), Some(2.0))
    );
    // A model the entry says nothing of keeps the catalog's.
    let sonnet = "anthropic:claude-sonnet-5-5";
    assert_eq!(
        facts.cache_facts(sonnet),
        Catalog::builtin().model(sonnet).unwrap().cache
    );
    // A model only the entry knows: facts, no row.
    let house = facts.cache_facts("gateway:house").unwrap();
    assert_eq!(
        (house.min_cacheable_tokens, house.prewarm_max_tokens),
        (Some(2048), Some(0))
    );
    assert!(facts.model_info("gateway:house").is_none());
    assert_eq!(facts.cache_facts("gateway:other"), None);
}

#[test]
fn a_cache_row_with_a_misspelt_or_mistyped_fact_is_refused() {
    for row in [json!({"ttl1H": true}), json!({"minCacheableTokens": "512"})] {
        let entry = json!({"baseURL": "http://localhost:1/v1", "cache": {"m": row}});
        let problem = CustomProviderConfig::from_json(&entry).unwrap_err();
        assert!(
            matches!(&problem, EntryProblem::Cache { model, .. } if model == "m"),
            "{row}: {problem:?}"
        );
        // Nothing read from it: the catalog's facts stand.
        let facts = CatalogFacts::new(entries(json!({"anthropic": entry})));
        assert_eq!(
            facts.cache_facts("anthropic:claude-opus-5-5"),
            Catalog::builtin()
                .model("anthropic:claude-opus-5-5")
                .unwrap()
                .cache
        );
    }
}
