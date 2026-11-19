//! providers.json: reading the file and checking each entry.

use std::path::Path;

use haest_core::{fs::MemFs, testing::assert_causes_said_once};
use serde_json::json;

use super::{
    tests::{bad_entry, entries, keys, over, registry},
    *,
};
use crate::testing::{FakeHttp, FakeReply, collect, request};

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
            matches!(&*problem, EntryProblem::UnknownKeys { keys }
                if *keys == [UnknownKey::new(unknown, close)]),
            "{name}: {problem:?}"
        );
        assert_eq!(provider, name);
        let said = problem.to_string();
        assert!(said.contains(unknown), "{said}");
    }
    assert!(t.requests().is_empty(), "nothing sent");
}

/// An entry with more than one key no entry has was refused one key per
/// run, the first only: every one is named at once.
#[test]
fn every_key_no_entry_has_is_named_at_once() {
    let entry = json!({
        "_comment": "a note",
        "apiKey": "k",
        "baseUrl": "https://gw.example",
        "models": ["m"],
    });
    let problem = CustomProviderConfig::from_json(&entry).unwrap_err();
    let EntryProblem::UnknownKeys { keys } = &problem else {
        panic!("{problem:?}");
    };
    let named: Vec<(&str, Option<&str>)> = keys.iter().map(|k| (k.key.as_str(), k.meant)).collect();
    assert_eq!(
        named,
        [
            ("_comment", None),
            ("baseUrl", Some("baseURL")),
            ("models", None)
        ]
    );
}

/// A `baseURL`, `apiKey` or `apiKeyEnv` that is there but not a string
/// (`null`, which a generator writes for an unset value, a number, a list)
/// read as absent. A `baseURL` read so made the entry one for the dialect's
/// own API, which got the gateway's key and the conversation. Each is
/// refused, naming the key and never the value, and nothing is sent.
#[tokio::test]
async fn a_url_or_key_that_is_not_a_string_is_refused_and_nothing_is_sent() {
    let t = FakeHttp::new([FakeReply::status(400, "")]);
    let custom = entries(json!({
        "null": {"type": "anthropic", "baseURL": null, "apiKeyEnv": "GW"},
        "number": {"type": "anthropic", "baseURL": 42, "apiKeyEnv": "GW"},
        "list": {"baseURL": ["https://gw.example"], "apiKeyEnv": "GW"},
        "object": {"baseURL": {"url": "https://gw.example"}, "apiKeyEnv": "GW"},
        "key": {"baseURL": "http://x/v1", "apiKey": 987_654_321},
        "env": {"baseURL": "http://x/v1", "apiKeyEnv": false},
    }));
    let reg = over(
        ProviderRegistry::new(keys(&[("GW", "gw-secret")])).with_custom(custom),
        &t,
    );
    for (name, key) in [
        ("null", "baseURL"),
        ("number", "baseURL"),
        ("list", "baseURL"),
        ("object", "baseURL"),
        ("key", "apiKey"),
        ("env", "apiKeyEnv"),
    ] {
        let resolved = reg.resolve(&format!("{name}:m"));
        if let Ok(r) = &resolved {
            collect(r.provider().stream(request(r.model()))).await;
        }
        let sent: Vec<String> = t.requests().into_iter().map(|r| r.url).collect();
        let Err(e @ ModelError::Unavailable { .. }) = resolved else {
            panic!("{name}: resolved, and sent to {sent:?}");
        };
        let said = haest_core::error::report(&e);
        assert!(!said.contains("987654321"), "{said}");
        let problem = bad_entry(&reg, &format!("{name}:m")).map(|(_, p)| p);
        assert!(
            matches!(&problem, Some(EntryProblem::NotAString { key: k, .. }) if *k == key),
            "{name}: {problem:?}"
        );
    }
    assert!(t.requests().is_empty(), "nothing sent");
    // No key at all still means the default.
    let plain = CustomProviderConfig::from_json(&json!({"type": "anthropic"})).unwrap();
    assert_eq!(
        (plain.base_url, plain.api_key, plain.api_key_env),
        (None, None, None)
    );
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
