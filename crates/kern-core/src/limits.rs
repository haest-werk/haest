//! Hard turn limits, checked by the loop BEFORE every model call -- the only
//! point where stopping keeps history valid (every tool call already has its
//! result). Every cap is per TURN (one human message); 0 disables a cap. The
//! cap that SPANS turns is the ledger (ledger.rs); the guard that reads all of
//! them, and nudges at half, is guard.rs.
//!
//! PORT (T3): estimate_request_tokens, request_too_big from core/limits.ts.

use serde::{Deserialize, Serialize};

use crate::messages::Usage;

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
