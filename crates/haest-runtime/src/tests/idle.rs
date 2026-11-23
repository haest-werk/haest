//! An idle session keeps main's prompt cache warm as the `[cache]` setting
//! says, over the provider's own wire, and tells the human when it stops.

use std::{sync::Arc, time::Duration};

use haest_core::fs::MemFs;
use haest_providers::testing::{FakeHttp, FakeReply, typed_sse};
use haest_session::{Body, Hub, Input, start};
use haest_settings::{CacheSettings, Settings};
use haest_wire::{Level, Outbound};
use serde_json::{Value, json};

use super::hardcoded;
use crate::{BuildOptions, HubUi, RuntimeInputs, World, build, testkit::Deny};

/// Anthropic's stream of the answer "ok", which wrote the prompt to the cache.
fn answered() -> String {
    typed_sse(&[
        json!({"type": "message_start", "message": {"model": "claude-sonnet-5-5",
            "usage": {"input_tokens": 10, "cache_creation_input_tokens": 900, "cache_read_input_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "ok"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
        json!({"type": "message_stop"}),
    ])
}

/// Anthropic's answer to a pre-warm that read and wrote these many tokens.
fn pinged(read: u64, wrote: u64) -> Value {
    json!({
        "id": "msg_01", "type": "message", "role": "assistant", "content": [],
        "model": "claude-sonnet-5-5", "stop_reason": "max_tokens", "stop_sequence": null,
        "usage": {"input_tokens": 10, "cache_creation_input_tokens": wrote,
            "cache_read_input_tokens": read, "output_tokens": 0},
    })
}

#[tokio::test(start_paused = true)]
async fn an_idle_session_keeps_mains_cache_warm_until_a_ping_writes() {
    let http = FakeHttp::new([
        FakeReply::sse(answered()),
        FakeReply::json(&pinged(900, 0)),
        FakeReply::json(&pinged(0, 900)),
    ]);
    let settings = Settings {
        model_spec: "anthropic:claude-sonnet-5-5".into(),
        cache: CacheSettings {
            keep_warm: true,
            ..Default::default()
        },
        ..hardcoded()
    };
    let key = |name: &str| (name == "ANTHROPIC_API_KEY").then(|| "k".to_string());
    let inputs = RuntimeInputs::new(settings, World::files(Arc::new(MemFs::new())))
        .with_keys(key)
        .with_http(http.clone());
    let hub = Hub::new("s1");
    let mut sub = hub.subscribe(0);
    let rt = build(
        inputs,
        HubUi::new(hub.clone()),
        Deny,
        BuildOptions::default(),
    )
    .await
    .unwrap();
    let session = start(rt, hub);
    let end = session.turn(Input::human("hi")).await.unwrap();
    assert_eq!(end.error, None);
    tokio::time::sleep(Duration::from_secs(3_600)).await;
    session.close().await;

    let caps: Vec<Value> = (http.requests().iter())
        .map(|r| r.json_body()["max_tokens"].clone())
        .collect();
    assert_eq!(caps.len(), 3, "the turn, then two pings: {caps:?}");
    assert_eq!(caps[1..], [json!(0), json!(0)]);
    let mut warnings = Vec::new();
    while let Some(frame) = sub.recv().await {
        if let Body::Wire(Outbound::Notice {
            level: Level::Warning,
            text,
        }) = &frame.body
        {
            warnings.push(text.clone());
        }
    }
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].starts_with("Keep-warm stopped"), "{warnings:?}");
}
