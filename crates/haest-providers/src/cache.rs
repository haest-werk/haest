//! The prompt-cache marker of the dialects that place breakpoints:
//! Anthropic's Messages API, and OpenRouter's Chat Completions for Anthropic
//! models, which passes it through.

use haest_core::cache::CacheTtl;
use serde_json::{Value, json};

/// A breakpoint's `cache_control` for `ttl`: `{"type": "ephemeral"}` for the
/// default five minutes, exactly as haest sent it before it had a TTL, and
/// `{"type": "ephemeral", "ttl": "1h"}` for an hour.
pub(crate) fn cache_control(ttl: CacheTtl) -> Value {
    match ttl {
        CacheTtl::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
        // Five minutes, and a TTL added later until it has its own word here.
        _ => json!({"type": "ephemeral"}),
    }
}
