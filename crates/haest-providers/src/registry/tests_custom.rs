//! providers.json: reading the file, checking an entry, and what a custom
//! provider sends.

use std::path::Path;

use haest_core::{fs::MemFs, messages::Message, testing::assert_causes_said_once};
use serde_json::json;

use super::{
    tests::{bad_entry, entries, keys, over, registry},
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
    let custom = entries(json!({"gateway": {"type": "anthropic", "baseURL": "http://gateway"}}));
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
    let custom = entries(
        json!({"proxy": {"type": "anthropic", "baseURL": "http://proxy",
        "apiKey": "x", "headers": {"x-proxy-auth": "tok"}}}),
    );
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
    let custom = entries(
        json!({"proxy": {"type": "anthropic", "baseURL": "http://proxy",
        "headers": {"X-Api-Key": "tok", "Accept": "text/event-stream"}}}),
    );
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
    let custom = entries(json!({
        "typo": {"type": "anthropc"},
        "number": {"type": 3},
        "plain": {"baseURL": "http://x/v1"},
    }));
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

/// A key no entry has is refused, naming it and the key meant when one is
/// close. Read past, a misspelt `baseURL` made the entry one for the
/// dialect's own API, which then got the entry's key and the conversation.
#[tokio::test]
async fn a_key_no_entry_has_is_refused_and_nothing_is_sent() {
    let t = FakeHttp::new([FakeReply::status(400, "")]);
    let custom = entries(json!({
        "gateway": {"type": "anthropic", "baseUrl": "https://gw.example", "apiKey": "gw-key"},
        "local": {"base_url": "http://localhost:1234/v1", "apiKeyEnv": "LOCAL_KEY"},
        "keyed": {"baseURL": "http://x/v1", "api_key_env": "LOCAL_KEY"},
        "listed": {"baseURL": "http://x/v1", "models": ["m"]},
        "team": {"baseURL": "http://x/v1", "header": {"X-Team": "platform"}},
    }));
    let reg = over(
        ProviderRegistry::new(keys(&[("LOCAL_KEY", "local-key")])).with_custom(custom),
        &t,
    );
    for (name, unknown, close) in [
        ("gateway", "baseUrl", Some("baseURL")),
        ("local", "base_url", Some("baseURL")),
        ("keyed", "api_key_env", Some("apiKeyEnv")),
        ("listed", "models", None),
        ("team", "header", Some("headers")),
    ] {
        let resolved = reg.resolve(&format!("{name}:m"));
        if let Ok(r) = &resolved {
            collect(r.provider().stream(request(r.model()))).await;
        }
        let sent: Vec<String> = t.requests().into_iter().map(|r| r.url).collect();
        let Err(ModelError::Unavailable { provider, source }) = resolved else {
            panic!("{name}: resolved, and sent to {sent:?}");
        };
        let problem = source.downcast::<EntryProblem>().expect("an entry problem");
        assert!(
            matches!(&*problem, EntryProblem::UnknownKey { key, meant }
                if key == unknown && *meant == close),
            "{name}: {problem:?}"
        );
        assert_eq!(provider, name);
        let said = problem.to_string();
        assert!(
            said.contains(unknown) && said.contains("apiKeyEnv"),
            "{said}"
        );
    }
    assert!(t.requests().is_empty(), "nothing sent");
}

#[test]
fn an_entry_that_is_not_an_object_is_refused() {
    let custom = entries(json!({"local": "http://localhost:1234/v1"}));
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
    let custom = entries(json!({"p": {"pricing": {"m": {"input": "free"}}}}));
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
    let custom = entries(json!({
        "bad": {"type": "openai", "reasoningEffort": {"efforts": "low"}},
        "good": {"type": "openai", "baseURL": "http://x/v1"},
    }));
    assert_eq!(custom.len(), 2, "both are kept: checked when resolved");
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
    let entries = entries(json!({"local": entry}));
    assert_eq!(format!("{entries:?}"), r#"CustomProviders(["local"])"#);
}

/// The key an entry sends: its own, else the host's under `apiKeyEnv`, else
/// none, and then the provider sends a placeholder a local server takes.
#[tokio::test]
async fn an_entry_without_a_key_has_none_and_its_provider_sends_a_placeholder() {
    let host = keys(&[("LOCAL_KEY", "from-host")]);
    let own = CustomProviderConfig {
        api_key: Some("own".into()),
        api_key_env: Some("LOCAL_KEY".into()),
        ..CustomProviderConfig::default()
    };
    assert_eq!(own.key(host.as_ref()).as_deref(), Some("own"));
    let named = CustomProviderConfig {
        api_key_env: Some("LOCAL_KEY".into()),
        ..CustomProviderConfig::default()
    };
    assert_eq!(named.key(host.as_ref()).as_deref(), Some("from-host"));
    // The registry's own key source, borrowed from its handle.
    let registry = ProviderRegistry::new(keys(&[("LOCAL_KEY", "from-host")]));
    let held = registry.keys().as_ref();
    assert_eq!(named.key(held).as_deref(), Some("from-host"));
    let bare = CustomProviderConfig::openai("http://localhost:1/v1");
    assert_eq!(bare.key(host.as_ref()), None);

    let t = FakeHttp::new([FakeReply::status(400, "")]);
    let reg = over(
        ProviderRegistry::new(host).with_custom_provider("local", bare),
        &t,
    );
    let r = reg.resolve("local:m").unwrap();
    collect(r.provider().stream(request(r.model()))).await;
    let sent = &t.requests()[0].headers;
    assert!(
        sent.contains(&("authorization".to_string(), "Bearer none".to_string())),
        "{sent:?}"
    );
}

/// An entry deserializes as `from_json` reads it, and a host's typed entry
/// is the same entry as the file's.
#[test]
fn an_entry_deserializes_as_from_json_reads_it() {
    let raw = json!({"type": "anthropic", "baseURL": "https://gw", "apiKeyEnv": "GW"});
    let read: CustomProviderConfig = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(read, CustomProviderConfig::from_json(&raw).unwrap());
    assert_eq!(
        read,
        CustomProviderConfig {
            api_key_env: Some("GW".into()),
            ..CustomProviderConfig::anthropic("https://gw")
        }
    );
    let typo = serde_json::from_value::<CustomProviderConfig>(json!({"type": "anthropc"}));
    assert!(typo.unwrap_err().to_string().contains("anthropc"));
    let file = entries(json!({"gw": raw}));
    assert_eq!(file.get("gw").unwrap().unwrap(), read);
}
