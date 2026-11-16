use std::{collections::HashMap, sync::Arc};

use haest_core::provider::{ModelError, ModelResolver};
use serde_json::json;

use super::*;
use crate::{
    driver_tests::Silent,
    routing::parse_routing,
    testing::{FakeHttp, FakeReply, collect, request},
};

/// What the host answers for these names (nothing else).
pub(super) fn keys(pairs: &[(&str, &str)]) -> Arc<dyn KeySource> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Arc::new(map)
}

pub(super) fn opts(t: &Arc<FakeHttp>) -> RegistryOptions {
    RegistryOptions {
        http: t.clone(),
        retry: RetryPolicy::none(),
        ..RegistryOptions::default()
    }
}

pub(super) fn registry(keys: Arc<dyn KeySource>, custom: CustomProviders) -> ProviderRegistry {
    ProviderRegistry::new(keys, custom, opts(&FakeHttp::new([])))
}

fn missing_key(reg: &ProviderRegistry, spec: &str) -> Option<String> {
    match reg.resolve(spec) {
        Err(ModelError::MissingKey { key, .. }) => Some(key),
        _ => None,
    }
}

/// The entry `spec`'s provider failed with, and the provider it names; None
/// when it did not fail as a bad providers.json entry.
pub(super) fn bad_entry(reg: &ProviderRegistry, spec: &str) -> Option<(String, EntryProblem)> {
    match reg.resolve(spec) {
        Err(ModelError::Unavailable { provider, source }) => {
            let problem = source.downcast::<EntryProblem>().ok()?;
            Some((provider, *problem))
        }
        _ => None,
    }
}

#[test]
fn built_ins_need_their_key_and_the_error_names_it() {
    let reg = registry(keys(&[]), CustomProviders::new());
    assert_eq!(
        missing_key(&reg, "openrouter:x/y").as_deref(),
        Some("OPENROUTER_API_KEY")
    );
    assert!(matches!(
        reg.resolve("nope:m"),
        Err(ModelError::UnknownProvider { provider, known }) if provider == "nope" && known.contains(&"openrouter".to_string())
    ));
    assert!(matches!(
        reg.resolve("no-colon"),
        Err(ModelError::BadSpec { .. })
    ));
}

#[test]
fn a_key_comes_from_whatever_the_host_answers_with() {
    let vault = |name: &str| (name == "OPENROUTER_API_KEY").then(|| "from-vault".to_string());
    let reg = registry(Arc::new(vault), CustomProviders::new());
    assert!(reg.resolve("openrouter:x/y").is_ok());
    assert_eq!(
        missing_key(&reg, "anthropic:claude").as_deref(),
        Some("ANTHROPIC_API_KEY")
    );
}

#[test]
fn a_spec_resolves_keeping_slashes_and_colons_in_the_model_id_and_the_provider_is_reused() {
    let reg = registry(keys(&[("OPENROUTER_API_KEY", "k")]), CustomProviders::new());
    let r = reg
        .resolve("openrouter:anthropic/claude-sonnet-5:beta")
        .unwrap();
    assert_eq!(r.provider.id(), "openrouter");
    assert_eq!(r.model, "anthropic/claude-sonnet-5:beta");
    assert!(Arc::ptr_eq(
        &reg.resolve("openrouter:other").unwrap().provider,
        &r.provider
    ));
}

#[tokio::test]
async fn openai_speaks_responses_while_openrouter_and_custom_servers_speak_chat_completions() {
    let t = FakeHttp::new(
        (0..3).map(|_| FakeReply::status(400, r#"{"error":{"message":"stop here"}}"#)),
    );
    let base = "http://localhost:1/v1";
    let custom = CustomProviders::from([(
        "local".to_string(),
        json!({"type": "openai", "baseURL": base}),
    )]);
    let k = keys(&[
        ("OPENAI_API_KEY", "k"),
        ("OPENAI_BASE_URL", base),
        ("OPENROUTER_API_KEY", "k"),
        ("OPENROUTER_BASE_URL", base),
    ]);
    let reg = ProviderRegistry::new(k, custom, opts(&t));
    for spec in ["openai:gpt-5.5", "openrouter:openai/gpt-5.5", "local:qwen"] {
        let r = reg.resolve(spec).unwrap();
        collect(r.provider.stream(request(&r.model))).await;
    }
    let paths: Vec<String> = t.requests().into_iter().map(|r| r.url).collect();
    assert_eq!(
        paths,
        [
            "http://localhost:1/v1/responses",
            "http://localhost:1/v1/chat/completions",
            "http://localhost:1/v1/chat/completions"
        ]
    );
}

#[tokio::test]
async fn openrouter_routing_goes_out_only_when_handed_in_and_attribution_is_configurable() {
    let routing = json!({"global": {"banned": ["novita"], "favorites": []}}).to_string();
    let routing = parse_routing(&routing).unwrap();
    for handed in [None, routing] {
        let t = FakeHttp::new([FakeReply::status(400, "")]);
        let mut o = opts(&t);
        let sends = handed.is_some();
        o.routing = handed;
        o.app_url = Some("https://example.test".into());
        let reg =
            ProviderRegistry::new(keys(&[("OPENROUTER_API_KEY", "k")]), Default::default(), o);
        let r = reg.resolve("openrouter:z-ai/glm-5").unwrap();
        collect(r.provider.stream(request(&r.model))).await;
        let sent = &t.requests()[0];
        assert_eq!(sent.json_body().get("provider").is_some(), sends);
        assert_eq!(sent.header("http-referer"), Some("https://example.test"));
        assert_eq!(sent.header("x-title"), Some("haest"));
    }
}

#[tokio::test]
async fn every_dialect_the_registry_builds_waits_only_the_headers_timeout_it_was_handed() {
    let silent = Arc::new(Silent::default());
    let o = RegistryOptions {
        http: silent.clone(),
        retry: RetryPolicy::none(),
        headers_timeout: std::time::Duration::from_millis(20),
        ..RegistryOptions::default()
    };
    let keys = keys(&[
        ("ANTHROPIC_API_KEY", "k"),
        ("OPENAI_API_KEY", "k"),
        ("OPENROUTER_API_KEY", "k"),
    ]);
    let reg = ProviderRegistry::new(keys, Default::default(), o);
    for spec in ["anthropic:m", "openai:m", "openrouter:m"] {
        let r = reg.resolve(spec).unwrap();
        let ended = std::time::Duration::from_secs(5);
        let (_, msg) = tokio::time::timeout(ended, collect(r.provider.stream(request(&r.model))))
            .await
            .unwrap_or_else(|_| panic!("{spec} waited past its headers timeout"));
        assert_eq!(
            msg.stop_reason,
            haest_core::messages::StopReason::Error,
            "{spec}"
        );
    }
}

#[test]
fn an_entry_that_replaces_a_built_in_is_listed_once() {
    let custom = CustomProviders::from([
        (
            "openrouter".to_string(),
            json!({"type": "openai", "baseURL": "http://gateway/v1"}),
        ),
        ("local".to_string(), json!({"type": "openai"})),
    ]);
    let names = registry(keys(&[]), custom).names();
    let listed = |name: &str| names.iter().filter(|n| *n == name).count();
    assert_eq!(listed("openrouter"), 1, "{names:?}");
    assert_eq!(listed("local"), 1, "{names:?}");
    assert_eq!(listed("anthropic"), 1, "{names:?}");
}
