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

/// A cost that is no real amount (a provider's NaN, a refund) made the sum a
/// NaN, or lowered it: the turn's USD cap read that sum.
#[test]
fn usage_adds_no_cost_that_is_negative_or_not_finite() {
    let mut u = Usage {
        cost_usd: Some(0.25),
        ..Default::default()
    };
    for bad in [f64::NAN, -1.0, f64::INFINITY, f64::NEG_INFINITY] {
        u += &Usage {
            input: 1,
            cost_usd: Some(bad),
            ..Default::default()
        };
    }
    assert_eq!((u.cost_usd, u.input), (Some(0.25), 4));
    let unpriced: Usage = [Some(f64::NAN), None]
        .map(|cost_usd| Usage {
            cost_usd,
            ..Default::default()
        })
        .iter()
        .sum();
    assert_eq!(unpriced.cost_usd, None);
}

/// Two real costs summed past the largest float made the sum `inf`, and a
/// usage whose own cost was a NaN stayed one whatever was added to it.
#[test]
fn a_cost_sum_stops_at_f64_max_and_a_bad_cost_on_the_left_is_no_price() {
    let cost = |c: f64| Usage {
        cost_usd: Some(c),
        ..Default::default()
    };
    let total: Usage = [cost(f64::MAX), cost(f64::MAX)].iter().sum();
    assert_eq!(total.cost_usd, Some(f64::MAX));
    for bad in [f64::NAN, -1.0, f64::INFINITY] {
        let mut left = cost(bad);
        left += &cost(1.0);
        assert_eq!(left.cost_usd, Some(1.0), "{bad} on the left");
    }
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
    sum += &max;
    assert_eq!(sum, max);
    assert_eq!(max.context_tokens(), u64::MAX);
}
