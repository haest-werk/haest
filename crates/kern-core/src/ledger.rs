//! THE CONVERSATION LEDGER: what one conversation has spent across ALL its
//! turns. Turn caps reset whenever something posts, and that something is not
//! always the human -- another agent, a schedule, a queue. This is the ceiling,
//! and posting again does NOT reset it.
//!
//! Two measures, because not every provider reports a price: USD for every call
//! that did, billed TOKENS for every call that did not. Counting an unpriced
//! call as $0 would be the "spent nothing" vs "measured nothing" lie.
//!
//! PORT (T3): the behaviour of core/ledger.ts (charge, caps, warn once per
//! crossing at 80%, over(), raise, snapshot/restore). Types below are the contract.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerCaps {
    /// USD across the conversation; 0 = no cap.
    pub max_usd: f64,
    /// Tokens billed by calls that reported no price; 0 = no cap.
    pub max_unpriced_tokens: u64,
}

/// Caps the human set; a None field = the configured default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_unpriced_tokens: Option<u64>,
}

/// Absolute: the latest wins, so replaying the session log restores it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerSnapshot {
    pub usd: f64,
    pub unpriced_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caps: Option<CapOverrides>,
}

pub trait LedgerHooks: Send + Sync {
    fn persist(&self, snapshot: &LedgerSnapshot);
    /// The human must hear this (once per crossing).
    fn warn(&self, text: &str);
}

#[allow(dead_code)] // T3: the ported behaviour reads every field; remove this allow then
pub struct Ledger {
    pub usd: f64,
    pub unpriced_tokens: u64,
    pub(crate) set: CapOverrides,
    pub(crate) warned_usd: bool,
    pub(crate) warned_tokens: bool,
    pub(crate) defaults: LedgerCaps,
    pub(crate) hooks: Arc<dyn LedgerHooks>,
}

impl Ledger {
    pub fn new(defaults: LedgerCaps, hooks: Arc<dyn LedgerHooks>) -> Self {
        Self {
            usd: 0.0,
            unpriced_tokens: 0,
            set: CapOverrides::default(),
            warned_usd: false,
            warned_tokens: false,
            defaults,
            hooks,
        }
    }

    pub fn caps(&self) -> LedgerCaps {
        LedgerCaps {
            max_usd: self.set.max_usd.unwrap_or(self.defaults.max_usd),
            max_unpriced_tokens: self
                .set
                .max_unpriced_tokens
                .unwrap_or(self.defaults.max_unpriced_tokens),
        }
    }
}
