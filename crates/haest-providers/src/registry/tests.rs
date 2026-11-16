use std::{collections::HashMap, path::Path, sync::Arc};

use haest_core::{
    fs::MemFs,
    provider::{ModelError, ModelResolver},
};
use serde_json::json;

use super::*;
use crate::{
    routing::parse_routing,
    testing::{FakeHttp, FakeReply, collect, request},
};

/// What the host answers for these names (nothing else).
fn keys(pairs: &[(&str, &str)]) -> Arc<dyn KeySource> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Arc::new(map)
}

fn opts(t: &Arc<FakeHttp>) -> RegistryOptions {
    RegistryOptions {
        http: t.clone(),
        retry: RetryPolicy::none(),
        ..RegistryOptions::default()
    }
}

fn registry(keys: Arc<dyn KeySource>, custom: CustomProviders) -> ProviderRegistry {
    ProviderRegistry::new(keys, custom, opts(&FakeHttp::new([])))
}

fn err(reg: &ProviderRegistry, spec: &str) -> String {
    reg.resolve(spec).expect_err(spec).to_string()
}

#[test]
fn built_ins_need_their_key_and_the_error_names_it() {
    let reg = registry(keys(&[]), CustomProviders::new());
    let missing = reg.resolve("openrouter:x/y").unwrap_err();
    assert!(matches!(missing, ModelError::Unavailable(_)));
    assert!(missing.to_string().contains("OPENROUTER_API_KEY"));
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
    assert!(err(&reg, "anthropic:claude").contains("ANTHROPIC_API_KEY"));
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
async fn custom_providers_come_from_providers_json_by_dialect() {
    let file = json!({
        "local": {"type": "openai", "baseURL": "http://localhost:1234/v1", "apiKeyEnv": "LOCAL_KEY"},
        "proxy": {"type": "anthropic", "baseURL": "http://proxy", "apiKey": "x"},
    });
    let fs = MemFs::with_files([("/cfg/providers.json", file.to_string())]);
    let custom = load_custom_providers(&fs, Path::new("/cfg")).await.unwrap();
    let reg = registry(keys(&[]), custom);
    assert_eq!(reg.resolve("local:qwen").unwrap().provider.id(), "local");
    assert_eq!(
        reg.resolve("proxy:claude").unwrap().provider.id(),
        "anthropic"
    );
    assert!(reg.names().contains(&"local".to_string()));
}

#[tokio::test]
async fn a_custom_anthropic_provider_sends_its_configured_headers() {
    let t = FakeHttp::new([FakeReply::status(
        400,
        r#"{"error":{"message":"stop here"}}"#,
    )]);
    let custom = CustomProviders::from([(
        "proxy".to_string(),
        json!({"type": "anthropic", "baseURL": "http://proxy", "apiKey": "x",
               "headers": {"x-proxy-auth": "tok"}}),
    )]);
    let reg = ProviderRegistry::new(keys(&[]), custom, opts(&t));
    let r = reg.resolve("proxy:claude").unwrap();
    collect(r.provider.stream(request(&r.model))).await;
    let sent = &t.requests()[0].headers;
    assert!(sent.contains(&("x-proxy-auth".to_string(), "tok".to_string())));
}

/// A configured header replaces a default of the same name, whatever its case:
/// a proxy that takes its key in `x-api-key` gets that one only.
#[tokio::test]
async fn a_configured_header_replaces_the_anthropic_default_of_that_name() {
    let t = FakeHttp::new([FakeReply::status(400, "")]);
    let custom = CustomProviders::from([(
        "proxy".to_string(),
        json!({"type": "anthropic", "baseURL": "http://proxy",
               "headers": {"X-Api-Key": "tok", "Accept": "text/event-stream"}}),
    )]);
    let reg = ProviderRegistry::new(keys(&[]), custom, opts(&t));
    let r = reg.resolve("proxy:claude").unwrap();
    collect(r.provider.stream(request(&r.model))).await;
    let sent = &t.requests()[0].headers;
    let values = |name: &str| -> Vec<&str> {
        sent.iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    };
    assert_eq!(values("x-api-key"), ["tok"]);
    assert_eq!(values("accept"), ["text/event-stream"]);
    assert_eq!(values("anthropic-version").len(), 1);
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
async fn a_broken_providers_json_is_a_clear_error_and_a_missing_one_is_empty() {
    let fs = MemFs::with_files([("/cfg/providers.json", "{nope")]);
    assert!(
        load_custom_providers(&fs, Path::new("/none"))
            .await
            .unwrap()
            .is_empty()
    );
    let e = load_custom_providers(&fs, Path::new("/cfg"))
        .await
        .unwrap_err();
    assert!(e.0.contains("providers.json"), "{e}");
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
        assert_eq!(sent.json().get("provider").is_some(), sends);
        assert_eq!(sent.header("http-referer"), Some("https://example.test"));
        assert_eq!(sent.header("x-title"), Some("haest"));
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

#[test]
fn one_bad_custom_entry_blocks_only_itself() {
    let custom = CustomProviders::from([
        (
            "bad".to_string(),
            json!({"type": "openai", "reasoningEffort": {"efforts": "low"}}),
        ),
        (
            "good".to_string(),
            json!({"type": "openai", "baseURL": "http://x/v1"}),
        ),
    ]);
    let reg = registry(keys(&[]), custom);
    assert!(err(&reg, "bad:m").contains("reasoningEffort"));
    assert!(reg.resolve("good:m").is_ok());
}
