//! The OpenRouter request kern actually sends carries the `provider` object the
//! routing config resolves to for THAT request's model -- read from the routing
//! env var, never from anywhere else.

use std::collections::HashMap;

use kern_core::provider::Provider;
use serde_json::{Value, json};

use super::*;
use crate::{
    routing::{ROUTING_ENV, routing_from_env},
    testing::{FakeReply, FakeTransport, chat_sse, collect, request},
};

fn routing_json() -> Value {
    json!({
        "global": {"banned": ["novita"], "favorites": ["deepinfra"]},
        "overrides": [{"match": "z-ai/glm-*", "banned": ["chutes"], "favorites": ["z-ai", "novita"]}],
    })
}

fn env(value: &str) -> HashMap<String, String> {
    HashMap::from([(ROUTING_ENV.to_string(), value.to_string())])
}

fn openrouter(env: &HashMap<String, String>) -> CompatOptions {
    let mut opts = CompatOptions::new("openrouter", CompatFlavor::OpenRouter, "k");
    opts.routing = routing_from_env(env).unwrap_or(None);
    opts
}

/// Drive the real request path and capture the body handed to the transport.
async fn wire_body(opts: CompatOptions, model: &str) -> Value {
    let t = FakeTransport::new([FakeReply::sse(chat_sse(&[]))]);
    collect(CompatProvider::new(opts, t.clone()).stream(request(model))).await;
    t.last_body()
}

/// `ignore` is a set on OpenRouter's side; compare it as one.
fn with_sorted_ignore(body: &Value) -> Value {
    let mut provider = body["provider"].clone();
    let mut ignore: Vec<String> = serde_json::from_value(provider["ignore"].clone()).unwrap();
    ignore.sort();
    provider["ignore"] = json!(ignore);
    provider
}

#[tokio::test]
async fn a_family_override_unions_bans_orders_its_favorites_and_drops_a_banned_one() {
    let body = wire_body(
        openrouter(&env(&routing_json().to_string())),
        "z-ai/glm-4.6",
    )
    .await;
    assert_eq!(
        with_sorted_ignore(&body),
        json!({"ignore": ["chutes", "novita"], "order": ["z-ai"], "allow_fallbacks": true})
    );
}

#[tokio::test]
async fn the_project_scope_from_the_env_wins_favorites_and_adds_its_priority() {
    let mut scoped = routing_json();
    scoped["project"] = json!({
        "global": {"banned": ["groq"], "favorites": ["cerebras"]}, "overrides": [], "priority": "speed",
    });
    let body = wire_body(openrouter(&env(&scoped.to_string())), "z-ai/glm-4.6").await;
    assert_eq!(
        with_sorted_ignore(&body),
        json!({
            "ignore": ["chutes", "groq", "novita"], "order": ["cerebras"],
            "allow_fallbacks": true, "sort": "throughput",
        })
    );
}

#[tokio::test]
async fn a_model_no_override_matches_gets_the_global_rule() {
    let body = wire_body(
        openrouter(&env(&routing_json().to_string())),
        "anthropic/claude-sonnet-5",
    )
    .await;
    assert_eq!(
        body["provider"],
        json!({"ignore": ["novita"], "order": ["deepinfra"], "allow_fallbacks": true})
    );
}

#[tokio::test]
async fn no_routing_env_or_a_malformed_one_sends_no_provider_object() {
    let none = wire_body(openrouter(&HashMap::new()), "z-ai/glm-4.6").await;
    assert!(none.get("provider").is_none());
    assert!(routing_from_env(&env("{nope")).is_err());
    let malformed = wire_body(openrouter(&env("{nope")), "z-ai/glm-4.6").await;
    assert!(malformed.get("provider").is_none());
}

#[tokio::test]
async fn a_generic_non_openrouter_server_never_gets_a_provider_object() {
    let mut generic = openrouter(&env(&routing_json().to_string()));
    generic.id = "local".into();
    generic.flavor = CompatFlavor::Generic;
    assert!(
        wire_body(generic, "z-ai/glm-4.6")
            .await
            .get("provider")
            .is_none()
    );
}
