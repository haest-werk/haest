use std::{collections::HashMap, sync::Arc};

use haest_core::{provider::ModelError, testing::ScriptedProvider};
use serde_json::json;

use super::*;
use crate::{
    driver_tests::Silent,
    routing::parse_routing,
    testing::{FakeHttp, FakeReply, collect, request},
};

/// What the host answers for these names (nothing else).
pub(super) fn keys(pairs: &[(&str, &str)]) -> KeySourceRef {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    map.into()
}

/// providers.json's entries, as read from its JSON.
pub(super) fn entries(file: serde_json::Value) -> CustomProviders {
    serde_json::from_value(file).expect("an object of entries")
}

/// `reg` sending through `t`, never retrying.
pub(super) fn over(reg: ProviderRegistry, t: &FakeHttp) -> ProviderRegistry {
    reg.with_http(t.clone()).with_retry(RetryPolicy::none())
}

pub(super) fn registry(keys: impl Into<KeySourceRef>, custom: CustomProviders) -> ProviderRegistry {
    over(
        ProviderRegistry::new(keys).with_custom(custom),
        &FakeHttp::new([]),
    )
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

/// A base URL whose password holds an unescaped `/` reads to a URL parser
/// as another host (`user`, port 1234): each built-in's variable, a
/// providers.json entry and an entry written in Rust are refused when the
/// provider is resolved, the reason saying how to write it and showing no
/// credential. Percent-encoded, each resolves.
#[test]
fn a_base_url_a_parser_misreads_is_refused_at_resolve() {
    let misread = "http://user:1234/s3cret@gw.example/v1";
    let encoded = "http://user:1234%2Fs3cret@gw.example/v1";
    for url in [misread, encoded] {
        let keys = keys(&[
            ("ANTHROPIC_API_KEY", "k"),
            ("ANTHROPIC_BASE_URL", url),
            ("OPENAI_API_KEY", "k"),
            ("OPENAI_BASE_URL", url),
            ("OPENROUTER_API_KEY", "k"),
            ("OPENROUTER_BASE_URL", url),
            ("PAPERINSTRUMENTS_API_KEY", "k"),
            ("PAPERINSTRUMENTS_BASE_URL", url),
        ]);
        let custom = entries(serde_json::json!({"json": {"baseURL": url}}))
            .with_entry("rust", CustomProviderConfig::anthropic(url));
        let reg = registry(keys, custom);
        for name in BUILTIN.into_iter().chain(["json", "rust"]) {
            let resolved = reg.resolve(&format!("{name}:m"));
            if url == encoded {
                assert!(resolved.is_ok(), "{name}: {:?}", resolved.err());
                continue;
            }
            let Err(e @ ModelError::Unavailable { .. }) = resolved else {
                panic!("{name}: {:?}", resolved.map(|r| r.model().to_string()));
            };
            let said = haest_core::error::report(&e);
            assert!(!said.contains("s3cret") && !said.contains("1234"), "{said}");
            assert!(said.contains(name) && said.contains("%2F"), "{said}");
        }
    }
}

/// A refused base URL says where it was set, as a refused `broker_url`
/// does: the built-in's variable, the entry's `baseURL` (an entry named
/// like a built-in included), or, for an entry the host built in Rust, the
/// `base_url` it gave (that entry is in no providers.json).
#[test]
fn a_refused_base_url_names_where_it_was_set() {
    let misread = "http://user:1234/s3cret@gw.example/v1";
    let keys = keys(&[
        ("ANTHROPIC_API_KEY", "k"),
        ("ANTHROPIC_BASE_URL", misread),
        ("OPENAI_API_KEY", "k"),
        ("OPENAI_BASE_URL", misread),
        ("OPENROUTER_API_KEY", "k"),
        ("OPENROUTER_BASE_URL", misread),
        ("PAPERINSTRUMENTS_API_KEY", "k"),
        ("PAPERINSTRUMENTS_BASE_URL", "https:///v1"),
    ]);
    let custom = entries(json!({"json": {"baseURL": misread}, "openai": {"baseURL": misread}}))
        .with_entry("rust", CustomProviderConfig::anthropic(misread));
    let reg = registry(keys, custom);
    for (name, from) in [
        ("anthropic", "ANTHROPIC_BASE_URL"),
        ("openai", "providers.json baseURL"),
        ("openrouter", "OPENROUTER_BASE_URL"),
        ("paperinstruments", "PAPERINSTRUMENTS_BASE_URL"),
        ("json", "providers.json baseURL"),
        ("rust", HOST_BASE_URL),
    ] {
        let e = reg.resolve(&format!("{name}:m")).expect_err(name);
        let ModelError::Unavailable { source, .. } = &e else {
            panic!("{name}: {e:?}");
        };
        let refused = source.downcast_ref::<BaseUrlRefused>();
        assert_eq!(refused.map(|r| r.from), Some(from), "{name}");
        haest_core::testing::assert_causes_said_once(&e);
    }
}

#[test]
fn a_key_comes_from_whatever_the_host_answers_with() {
    let vault = |name: &str| (name == "OPENROUTER_API_KEY").then(|| "from-vault".to_string());
    let reg = registry(vault, CustomProviders::new());
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
    assert_eq!(r.provider().id(), "openrouter");
    assert_eq!(r.model(), "anthropic/claude-sonnet-5:beta");
    assert!(ProviderRef::ptr_eq(
        reg.resolve("openrouter:other").unwrap().provider(),
        r.provider()
    ));
}

#[tokio::test]
async fn openai_speaks_responses_while_openrouter_and_custom_servers_speak_chat_completions() {
    let t = FakeHttp::new(
        (0..3).map(|_| FakeReply::status(400, r#"{"error":{"message":"stop here"}}"#)),
    );
    let base = "http://localhost:1/v1";
    let custom = entries(json!({"local": {"type": "openai", "baseURL": base}}));
    let k = keys(&[
        ("OPENAI_API_KEY", "k"),
        ("OPENAI_BASE_URL", base),
        ("OPENROUTER_API_KEY", "k"),
        ("OPENROUTER_BASE_URL", base),
    ]);
    let reg = over(ProviderRegistry::new(k).with_custom(custom), &t);
    for spec in ["openai:gpt-5.5", "openrouter:openai/gpt-5.5", "local:qwen"] {
        let r = reg.resolve(spec).unwrap();
        collect(r.provider().stream(request(r.model()))).await;
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
        let sends = handed.is_some();
        let reg = over(
            ProviderRegistry::new(keys(&[("OPENROUTER_API_KEY", "k")])),
            &t,
        )
        .with_app_url("https://example.test");
        let reg = match handed {
            Some(routing) => reg.with_routing(routing),
            None => reg,
        };
        let r = reg.resolve("openrouter:z-ai/glm-5").unwrap();
        collect(r.provider().stream(request(r.model()))).await;
        let sent = &t.requests()[0];
        assert_eq!(sent.json_body().get("provider").is_some(), sends);
        assert_eq!(sent.header("http-referer"), Some("https://example.test"));
        assert_eq!(sent.header("x-title"), Some("haest"));
    }
}

#[tokio::test]
async fn every_dialect_the_registry_builds_waits_only_the_headers_timeout_it_was_handed() {
    let silent = Arc::new(Silent::default());
    let keys = keys(&[
        ("ANTHROPIC_API_KEY", "k"),
        ("OPENAI_API_KEY", "k"),
        ("OPENROUTER_API_KEY", "k"),
    ]);
    let reg = ProviderRegistry::new(keys)
        .with_http(silent.clone())
        .with_retry(RetryPolicy::none())
        .with_headers_timeout(std::time::Duration::from_millis(20));
    for spec in ["anthropic:m", "openai:m", "openrouter:m"] {
        let r = reg.resolve(spec).unwrap();
        let ended = std::time::Duration::from_secs(5);
        let (_, msg) =
            tokio::time::timeout(ended, collect(r.provider().stream(request(r.model()))))
                .await
                .unwrap_or_else(|_| panic!("{spec} waited past its headers timeout"));
        assert_eq!(
            msg.stop_reason,
            haest_core::messages::StopReason::Error,
            "{spec}"
        );
    }
}

/// The client handed in stays the client whatever is set after it: the
/// requests go through it, never the shared default.
#[tokio::test]
async fn the_client_handed_in_survives_every_later_setter() {
    let t = FakeHttp::new([FakeReply::status(400, "")]);
    let reg = ProviderRegistry::new(keys(&[("OPENROUTER_API_KEY", "k")]))
        .with_http(t.clone())
        .with_retry(RetryPolicy::none())
        .with_headers_timeout(std::time::Duration::from_secs(5))
        .with_routing(RoutingScopes::default())
        .with_app_title("host")
        .with_app_url("https://host.test");
    let r = reg.resolve("openrouter:m").unwrap();
    collect(r.provider().stream(request(r.model()))).await;
    assert_eq!(t.requests().len(), 1, "sent through the fake");
    assert_eq!(t.requests()[0].header("x-title"), Some("host"));
}

/// Without reqwest there is no client to fall back on: a model call the host
/// handed no client ends as an error that says to hand one in, never a hang.
#[cfg(not(feature = "reqwest"))]
#[tokio::test]
async fn without_reqwest_a_registry_with_no_client_says_to_hand_one_in() {
    let reg = ProviderRegistry::new(keys(&[("OPENROUTER_API_KEY", "k")]));
    let r = reg.resolve("openrouter:m").unwrap();
    let (_, msg) = collect(r.provider().stream(request(r.model()))).await;
    assert_eq!(msg.stop_reason, haest_core::messages::StopReason::Error);
    let said = msg.error_message.unwrap_or_default();
    assert!(said.contains("hand one in"), "{said}");
}

#[test]
fn an_entry_that_replaces_a_built_in_is_listed_once() {
    let custom = entries(json!({
        "openrouter": {"type": "openai", "baseURL": "http://gateway/v1"},
        "local": {"type": "openai"},
    }));
    let names = registry(keys(&[]), custom).names();
    let listed = |name: &str| names.iter().filter(|n| *n == name).count();
    assert_eq!(listed("openrouter"), 1, "{names:?}");
    assert_eq!(listed("local"), 1, "{names:?}");
    assert_eq!(listed("anthropic"), 1, "{names:?}");
}

#[test]
fn a_clone_shares_what_was_built_and_a_with_setter_starts_its_own() {
    let reg = registry(keys(&[("OPENROUTER_API_KEY", "k")]), CustomProviders::new());
    let first = reg.resolve("openrouter:a").unwrap();
    let clone = reg.clone();
    assert!(ProviderRef::ptr_eq(
        clone.resolve("openrouter:b").unwrap().provider(),
        first.provider()
    ));
    // Other options are another registry: nothing built with the old ones is reused.
    let other = over(clone, &FakeHttp::new([]));
    assert!(!ProviderRef::ptr_eq(
        other.resolve("openrouter:c").unwrap().provider(),
        first.provider()
    ));
    assert!(ProviderRef::ptr_eq(
        reg.resolve("openrouter:d").unwrap().provider(),
        first.provider()
    ));
}

/// A key source that takes its time (a vault over the network) holds up only
/// the provider it is asked for: a spec whose provider is built resolves
/// meanwhile, and so does the registry's Debug.
#[test]
fn a_slow_key_lookup_holds_up_no_other_resolve() {
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    let (asked, was_asked) = mpsc::channel::<()>();
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let vault = move |name: &str| {
        if name == "ANTHROPIC_API_KEY" {
            asked.send(()).ok();
            released
                .lock()
                .expect("lock poisoned")
                .recv_timeout(Duration::from_secs(10))
                .ok();
        }
        Some("k".to_string())
    };
    let reg = registry(vault, CustomProviders::new());
    reg.resolve("openrouter:a").unwrap();

    let slow = {
        let reg = reg.clone();
        thread::spawn(move || reg.resolve("anthropic:claude").map(|_| ()))
    };
    was_asked
        .recv_timeout(Duration::from_secs(10))
        .expect("the anthropic key is asked for");
    let (done, finished) = mpsc::channel();
    let fast = {
        let reg = reg.clone();
        thread::spawn(move || {
            let start = Instant::now();
            let resolved = reg.resolve("openrouter:b").is_ok();
            let shown = format!("{reg:?}");
            done.send((resolved, start.elapsed(), shown)).ok();
        })
    };
    let waited = finished.recv_timeout(Duration::from_secs(2));
    release.send(()).ok();
    let (resolved, took, shown) =
        waited.expect("a built provider resolves while a key is looked up");
    assert!(resolved && took < Duration::from_secs(2), "{took:?}");
    assert!(shown.contains("openrouter"), "{shown}");
    fast.join().unwrap();
    slow.join().unwrap().unwrap();
    assert!(
        format!("{reg:?}").contains("anthropic"),
        "the slow one is kept too"
    );
}

#[test]
fn debug_names_the_providers_but_never_an_entry_key() {
    let custom = entries(json!({
        "local": {"type": "openai", "apiKey": "sk-SECRET", "baseURL": "http://u:URL-SECRET@h/v1"},
    }))
    .with_entry(
        "typed",
        CustomProviderConfig {
            api_key: Some("sk-TYPED-SECRET".into()),
            ..CustomProviderConfig::openai("http://u:TYPED-SECRET@h/v1")
        },
    );
    let reg = registry(keys(&[("OPENROUTER_API_KEY", "KEY-SECRET")]), custom)
        .with_provider("own", ScriptedProvider::new([]));
    reg.resolve("typed:m").unwrap();
    reg.resolve("local:m").unwrap();
    reg.resolve("openrouter:m").unwrap();
    let shown = format!("{reg:?}");
    assert!(!shown.contains("SECRET"), "{shown}");
    for name in ["local", "typed", "own", "built"] {
        assert!(shown.contains(name), "{name}: {shown}");
    }
}

/// A host's own provider wins over a providers.json entry and a built-in of
/// its name, needs no key, and is listed once; a typed entry resolves like
/// one read from the file.
#[test]
fn a_host_hands_in_its_own_provider_and_typed_entries() {
    let own = ProviderRef::new(ScriptedProvider::new([]));
    let reg = registry(
        keys(&[]),
        entries(json!({"gateway": {"type": "anthropic"}})),
    )
    .with_custom_provider(
        "local",
        CustomProviderConfig::openai("http://localhost:1/v1"),
    )
    .with_provider("openrouter", own.clone())
    .with_provider("gateway", own.clone())
    .with_provider("bedrock", own.clone());
    for spec in ["openrouter:x/y", "gateway:claude", "bedrock:claude"] {
        let resolved = reg.resolve(spec).unwrap();
        assert!(ProviderRef::ptr_eq(resolved.provider(), &own), "{spec}");
    }
    assert_eq!(reg.resolve("local:m").unwrap().provider().id(), "local");
    assert_eq!(
        reg.names(),
        [
            "anthropic",
            "openai",
            "openrouter",
            "paperinstruments",
            "gateway",
            "local",
            "bedrock"
        ]
    );
    let own_names: Vec<&str> = reg.providers().map(|(name, _)| name).collect();
    assert_eq!(own_names, ["bedrock", "gateway", "openrouter"]);
    assert_eq!(
        reg.custom().names().collect::<Vec<_>>(),
        ["gateway", "local"]
    );
}

#[test]
fn routing_is_none_until_scopes_are_handed_in() {
    let scopes = parse_routing(r#"{"global": {"banned": ["novita"]}}"#)
        .unwrap()
        .unwrap();
    let unset = ProviderRegistry::new(keys(&[]));
    assert_eq!(unset.routing(), None);
    assert_eq!(unset.with_routing(scopes.clone()).routing(), Some(&scopes));
}
