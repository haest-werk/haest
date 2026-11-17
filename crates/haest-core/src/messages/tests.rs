use super::*;
use serde_json::json;

#[test]
fn a_ts_session_message_round_trips() {
    let ts = json!({
        "role": "assistant",
        "content": [
            {"type": "thinking", "thinking": "hm", "signature": "sig"},
            {"type": "text", "text": "hi"},
            {"type": "tool_call", "id": "c1", "name": "Read", "input": {"path": "a.txt"}}
        ],
        "provider": "anthropic",
        "model": "claude-x",
        "usage": {"input": 1, "output": 2, "cacheRead": 3, "cacheWrite": 4, "reasoning": 0, "costUsd": 0.5},
        "stopReason": "tool_use"
    });
    let msg: Message = serde_json::from_value(ts.clone()).unwrap();
    let Message::Assistant(a) = &msg else {
        panic!("not assistant")
    };
    assert_eq!(a.tool_calls().count(), 1);
    assert_eq!(a.stop_reason, StopReason::ToolUse);
    assert_eq!(serde_json::to_value(&msg).unwrap(), ts);
}

#[test]
fn a_ts_user_turn_with_media_round_trips() {
    let ts = json!({
        "role": "user",
        "content": "see these",
        "media": [
            {"kind": "image", "mediaType": "image/png", "data": "AA==", "name": "a.png"},
            {"kind": "file", "mediaType": "application/pdf", "data": "AA==", "name": "b.pdf"},
            {"kind": "audio", "mediaType": "audio/wav", "data": "AA==", "name": "c.wav"},
            {"kind": "video", "mediaType": "video/mp4", "data": "AA==", "name": "d.mp4"}
        ]
    });
    let msg: Message = serde_json::from_value(ts.clone()).unwrap();
    let Message::User(u) = &msg else {
        panic!("not user")
    };
    let kinds: Vec<_> = u.media.iter().map(|p| p.kind).collect();
    assert_eq!(kinds, MediaKind::ALL);
    assert_eq!(serde_json::to_value(&msg).unwrap(), ts);
}

#[test]
fn usage_adds_cost_only_when_reported() {
    let mut u = Usage::default();
    u.add(&Usage {
        input: 5,
        ..Default::default()
    });
    assert_eq!(u.cost_usd, None);
    u.add(&Usage {
        cost_usd: Some(0.25),
        ..Default::default()
    });
    assert_eq!(u.cost_usd, Some(0.25));
    assert_eq!(u.context_tokens(), 5);
}

/// Token counts come from outside (a provider's usage, a session file): a
/// sum or a total past u64::MAX stops there instead of overflowing.
#[test]
fn usage_sums_and_totals_saturate_at_u64_max() {
    let max = Usage {
        input: u64::MAX,
        output: u64::MAX,
        cache_read: u64::MAX,
        cache_write: u64::MAX,
        reasoning: u64::MAX,
        cost_usd: None,
    };
    let mut sum = max.clone();
    sum.add(&max);
    assert_eq!(sum, max);
    assert_eq!(max.context_tokens(), u64::MAX);
}
