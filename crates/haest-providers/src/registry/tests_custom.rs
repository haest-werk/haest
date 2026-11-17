//! providers.json: reading the file, checking an entry, and what a custom
//! provider sends.

use std::path::Path;

use haest_core::{fs::MemFs, messages::Message, testing::assert_causes_said_once};
use serde_json::json;

use super::{
    tests::{bad_entry, keys, over, registry},
    *,
};
use crate::testing::{FakeHttp, FakeReply, collect, request, request_with, typed_sse};

#[tokio::test]
async fn custom_providers_come_from_providers_json_by_dialect() {
    let file = json!({
        "local": {"type": "openai", "baseURL": "http://localhost:1234/v1", "apiKeyEnv": "LOCAL_KEY"},
        "proxy": {"type": "anthropic", "baseURL": "http://proxy", "apiKey": "x"},
    });
    let fs = MemFs::from_files([("/cfg/providers.json", file.to_string())]);
    let custom = load_custom_providers(&fs, Path::new("/cfg")).await.unwrap();
    let reg = registry(keys(&[]), custom);
    assert_eq!(reg.resolve("local:qwen").unwrap().provider().id(), "local");
    assert_eq!(
        reg.resolve("proxy:claude").unwrap().provider().id(),
        "proxy"
    );
    assert!(reg.names().contains(&"local".to_string()));
}

/// Anthropic signs thinking per endpoint: a gateway's signature sent to
/// Anthropic itself (or the other way round) is a refused request.
#[tokio::test]
async fn a_custom_anthropic_entry_is_its_own_provider_and_its_thinking_replays_only_to_itself() {
    let signed = typed_sse(&[
        json!({"type": "message_start", "message": {"model": "claude", "usage": {}}}),
        json!({"type": "content_block_start", "index": 0,
               "content_block": {"type": "thinking", "thinking": "hmm", "signature": "gateway-sig"}}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": "hi"}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
    ]);
    let t = FakeHttp::new([
        FakeReply::sse(signed),
        FakeReply::status(400, ""),
        FakeReply::status(400, ""),
    ]);
    let custom = CustomProviders::from([(
        "gateway".to_string(),
        json!({"type": "anthropic", "baseURL": "http://gateway"}),
    )]);
    let reg = over(
        ProviderRegistry::new(keys(&[("ANTHROPIC_API_KEY", "k")])).with_custom(custom),
        &t,
    );
    let gateway = reg.resolve("gateway:claude").unwrap();
    assert_eq!(gateway.provider().id(), "gateway");
    let (_, answer) = collect(gateway.provider().stream(request(gateway.model()))).await;
    assert_eq!(answer.provider, "gateway");

    let history = vec![
        Message::user("hi"),
        Message::Assistant(answer),
        Message::user("again"),
    ];
    for spec in ["gateway:claude", "anthropic:claude"] {
        let r = reg.resolve(spec).unwrap();
        collect(
            r.provider()
                .stream(request_with(r.model(), history.clone())),
        )
        .await;
    }
    let replays_thinking = |i: usize| {
        let body = t.requests()[i].json_body();
        body["messages"][1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["type"] == "thinking")
    };
    assert!(replays_thinking(1), "to the gateway that signed it");
    assert!(!replays_thinking(2), "never to Anthropic itself");
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
    let reg = over(ProviderRegistry::new(keys(&[])).with_custom(custom), &t);
    let r = reg.resolve("proxy:claude").unwrap();
    collect(r.provider().stream(request(r.model()))).await;
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
    let reg = over(ProviderRegistry::new(keys(&[])).with_custom(custom), &t);
    let r = reg.resolve("proxy:claude").unwrap();
    collect(r.provider().stream(request(r.model()))).await;
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
async fn a_broken_providers_json_is_a_clear_error_and_a_missing_one_is_empty() {
    let fs = MemFs::from_files([("/cfg/providers.json", "{nope")]);
    assert!(
        load_custom_providers(&fs, Path::new("/none"))
            .await
            .unwrap()
            .is_empty()
    );
    let e = load_custom_providers(&fs, Path::new("/cfg"))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, ProvidersFileError::Parse { path, .. } if path == Path::new("/cfg/providers.json")),
        "{e}"
    );
}

#[test]
fn a_type_haest_does_not_speak_is_refused_and_no_type_is_chat_completions() {
    let custom = CustomProviders::from([
        ("typo".to_string(), json!({"type": "anthropc"})),
        ("number".to_string(), json!({"type": 3})),
        ("plain".to_string(), json!({"baseURL": "http://x/v1"})),
    ]);
    let reg = registry(keys(&[]), custom);
    for name in ["typo", "number"] {
        let e = bad_entry(&reg, &format!("{name}:m"));
        assert!(
            matches!(&e, Some((provider, EntryProblem::UnknownType(_))) if provider == name),
            "{e:?}"
        );
    }
    let plain = CustomProviderConfig::from_json(&json!({})).unwrap();
    assert_eq!(plain.dialect, Dialect::OpenAi);
    assert!(reg.resolve("plain:m").is_ok());
}

#[test]
fn an_entry_that_is_not_an_object_is_refused() {
    let custom = CustomProviders::from([("local".to_string(), json!("http://localhost:1234/v1"))]);
    let e = bad_entry(&registry(keys(&[]), custom), "local:m");
    assert!(matches!(&e, Some((_, EntryProblem::NotAnObject))), "{e:?}");
}

/// A providers.json that is not JSON, or an entry with a bad price, says
/// each cause once.
#[tokio::test]
async fn a_broken_providers_json_says_each_cause_once() {
    let fs = MemFs::from_files([("/cfg/providers.json", "{not json")]);
    let e = load_custom_providers(&fs, Path::new("/cfg"))
        .await
        .unwrap_err();
    assert_causes_said_once(&e);
    let custom = CustomProviders::from([(
        "p".to_string(),
        json!({"pricing": {"m": {"input": "free"}}}),
    )]);
    let e = registry(keys(&[]), custom).resolve("p:m").unwrap_err();
    assert_causes_said_once(&e);
}

#[tokio::test]
async fn a_providers_json_that_is_not_an_object_is_refused() {
    let fs = MemFs::from_files([("/cfg/providers.json", r#"[{"local": {}}]"#)]);
    let e = load_custom_providers(&fs, Path::new("/cfg"))
        .await
        .unwrap_err();
    assert!(matches!(&e, ProvidersFileError::NotAnObject { .. }), "{e}");
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
    assert!(matches!(
        bad_entry(&reg, "bad:m"),
        Some((provider, EntryProblem::ReasoningEffort)) if provider == "bad"
    ));
    assert!(reg.resolve("good:m").is_ok());
}

#[test]
fn the_key_and_header_values_never_show_in_debug_output() {
    let entry = json!({
        "baseURL": "http://user:URL-SECRET@localhost:1234/v1",
        "apiKey": "sk-SECRET",
        "apiKeyEnv": "LOCAL_KEY",
        "headers": {"x-proxy-key": "HEADER-SECRET"}
    });
    let cfg = CustomProviderConfig::from_json(&entry).unwrap();
    let shown = format!("{cfg:?}");
    assert!(!shown.contains("SECRET"), "{shown}");
    assert!(
        shown.contains("LOCAL_KEY") && shown.contains("x-proxy-key"),
        "{shown}"
    );
}
