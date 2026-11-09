//! Hard turn limits, checked by the loop BEFORE every model call -- the only
//! point where stopping keeps history valid (every tool call already has its
//! result). One tool-result flood once cost ~$50 in a single turn; nothing in the
//! harness assumed a turn could get that expensive, so now nothing may.
//!
//! Every cap here is per TURN (one human message): the human continues past a cap
//! by posting again, which is a decision, not an accident. 0 disables a cap. The
//! cap that SPANS turns is the conversation ledger (ledger.rs); the guard that
//! reads all of them, and nudges at half, is guard.rs.

use serde::{Deserialize, Serialize};

use crate::{
    media::messages_for_estimate,
    messages::{Message, Usage},
    provider::ToolSpec,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnLimits {
    /// Stop the turn once it has spent this much (only providers that report cost).
    pub max_turn_usd: f64,
    /// Stop the turn once it has billed this many tokens: the cap that works for EVERY provider.
    pub max_turn_tokens: u64,
    /// Refuse a single model call whose estimated input is bigger than this.
    pub max_request_tokens: u64,
    /// Stop the turn after this many model calls.
    pub max_turn_steps: u32,
    /// Stop the turn once it has run this long (wall clock, ms).
    pub max_turn_ms: u64,
    /// Stop the turn once one tool call (same input, same result) has repeated this often.
    pub max_repeats: u32,
}

/// Every token the provider bills for, cached or not.
pub fn billed_tokens(u: &Usage) -> u64 {
    u.input + u.cache_read + u.cache_write + u.output
}

/// Rough input size of one request: ~4 characters per token over the text sent,
/// plus each media part's own cost -- base64 counted as text would read a 1 MB
/// screenshot as ~350K tokens and refuse the call.
pub fn estimate_request_tokens(system: &str, messages: &[Message], tools: &[ToolSpec]) -> u64 {
    let (json, media_tokens) = messages_for_estimate(messages);
    let tools = serde_json::to_string(tools).unwrap_or_default();
    let chars = system.chars().count() + json.chars().count() + tools.chars().count();
    (chars as u64).div_ceil(4) + media_tokens
}

/// Why this request must not be sent, or None.
pub fn request_too_big(estimate: u64, limits: &TurnLimits) -> Option<String> {
    if limits.max_request_tokens == 0 || estimate <= limits.max_request_tokens {
        return None;
    }
    Some(format!(
        "Model call refused: the request is ~{estimate} tokens, the cap is {} (KERN_MAX_REQUEST_TOKENS). Run /compact or /clear, then continue.",
        limits.max_request_tokens
    ))
}

#[cfg(test)]
mod tests;
