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

/// The caps on the subagent tree. `max_depth` and `max_concurrent` are NOT
/// "0 = off": 0 means no subagents at all (no Agent tool). The three per-agent
/// caps replace the conversation's turn caps for each subagent's one turn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubagentCaps {
    /// How deep agents may nest (main = 0); HAEST_MAX_AGENT_DEPTH.
    pub max_depth: u32,
    /// Subagents running at once, across the tree; HAEST_MAX_SUBAGENTS.
    pub max_concurrent: usize,
    /// One subagent's spend; HAEST_SUBAGENT_USD.
    pub usd: f64,
    /// One subagent's model calls; HAEST_SUBAGENT_STEPS.
    pub steps: u32,
    /// One subagent's wall clock; HAEST_SUBAGENT_MINUTES.
    pub minutes: u64,
}

impl Default for SubagentCaps {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_concurrent: 4,
            usd: 1.0,
            steps: 50,
            minutes: 15,
        }
    }
}

/// Every token the provider bills for, cached or not.
pub fn billed_tokens(u: &Usage) -> u64 {
    u.context_tokens()
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
        "Model call refused: the request is ~{estimate} tokens, the cap is {} (HAEST_MAX_REQUEST_TOKENS). Run /compact or /clear, then continue.",
        limits.max_request_tokens
    ))
}

#[cfg(test)]
mod tests;
