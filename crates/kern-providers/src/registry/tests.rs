use std::{collections::HashMap, path::Path, sync::Arc};

use kern_core::{fs::MemFs, provider::ModelResolver};
use serde_json::json;

use super::*;
use crate::{
    routing::ROUTING_ENV,
    testing::{FakeReply, FakeTransport, collect, request},
};

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn opts(t: &Arc<FakeTransport>) -> RegistryOptions {
    RegistryOptions {
        transport: t.clone(),
        retry: RetryPolicy::none(),
        ..RegistryOptions::default()
    }
}

fn registry(env: HashMap<String, String>, custom: BTreeMap<String, Value>) -> ProviderRegistry {
    ProviderRegistry::new(env, custom, opts(&FakeTransport::new([])))
}

fn err(reg: &ProviderRegistry, spec: &str) -> String {
    reg.resolve(spec).expect_err(spec)
}

#[test]
fn built_ins_need_their_key_and_the_error_names_the_env_var() {
    let reg = registry(HashMap::new(), BTreeMap::new());
    assert!(err(&reg, "openrouter:x/y").contains("OPENROUTER_API_KEY"));
    assert!(err(&reg, "nope:m").contains("nope"));
    assert!(err(&reg, "no-colon").contains("<provider>:<model>"));
}

#[test]
fn a_spec_resolves_keeping_slashes_and_colons_in_the_model_id_and_the_provider_is_reused() {
    let reg = registry(env(&[("OPENROUTER_API_KEY", "k")]), BTreeMap::new());
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
    let t = FakeTransport::new([]);
    let reg = ProviderRegistry::load(&fs, Path::new("/cfg"), HashMap::new(), opts(&t))
        .await
        .unwrap();
    assert_eq!(reg.resolve("local:qwen").unwrap().provider.id(), "local");
    assert_eq!(
        reg.resolve("proxy:claude").unwrap().provider.id(),
        "anthropic"
    );
    assert!(reg.names().contains(&"local".to_string()));
}

#[tokio::test]
async fn openai_speaks_responses_while_openrouter_and_custom_servers_speak_chat_completions() {
    let t = FakeTransport::new(
        (0..3).map(|_| FakeReply::status(400, r#"{"error":{"message":"stop here"}}"#)),
    );
    let base = "http://localhost:1/v1";
    let custom = BTreeMap::from([(
        "local".to_string(),
        json!({"type": "openai", "baseURL": base}),
    )]);
    let e = env(&[
        ("OPENAI_API_KEY", "k"),
        ("OPENAI_BASE_URL", base),
        ("OPENROUTER_API_KEY", "k"),
        ("OPENROUTER_BASE_URL", base),
    ]);
    let reg = ProviderRegistry::new(e, custom, opts(&t));
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
async fn openrouter_routing_is_off_unless_asked_and_attribution_is_configurable() {
    let routing = json!({"global": {"banned": ["novita"], "favorites": []}}).to_string();
    let e = env(&[("OPENROUTER_API_KEY", "k"), (ROUTING_ENV, routing.as_str())]);
    for asked in [false, true] {
        let t = FakeTransport::new([FakeReply::status(400, "")]);
        let mut o = opts(&t);
        o.openrouter_routing = asked;
        o.app_url = Some("https://example.test".into());
        let reg = ProviderRegistry::new(e.clone(), BTreeMap::new(), o);
        let r = reg.resolve("openrouter:z-ai/glm-5").unwrap();
        collect(r.provider.stream(request(&r.model))).await;
        let sent = &t.requests()[0];
        assert_eq!(sent.body.get("provider").is_some(), asked);
        assert_eq!(sent.header("http-referer"), Some("https://example.test"));
        assert_eq!(sent.header("x-title"), Some("kern"));
    }
}

#[test]
fn a_malformed_routing_env_is_a_warning_not_a_failure() {
    let e = env(&[("OPENROUTER_API_KEY", "k"), (ROUTING_ENV, "{nope")]);
    let mut o = opts(&FakeTransport::new([]));
    o.openrouter_routing = true;
    let reg = ProviderRegistry::new(e, BTreeMap::new(), o);
    assert_eq!(reg.warnings().len(), 1);
    assert!(reg.resolve("openrouter:x/y").is_ok());
}

#[test]
fn one_bad_custom_entry_blocks_only_itself() {
    let custom = BTreeMap::from([
        (
            "bad".to_string(),
            json!({"type": "openai", "reasoningEffort": {"efforts": "low"}}),
        ),
        (
            "good".to_string(),
            json!({"type": "openai", "baseURL": "http://x/v1"}),
        ),
    ]);
    let reg = registry(HashMap::new(), custom);
    assert!(err(&reg, "bad:m").contains("reasoningEffort"));
    assert!(reg.resolve("good:m").is_ok());
}
