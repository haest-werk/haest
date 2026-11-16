//! THE CONVERSATION LEDGER: what one conversation has spent across ALL its
//! turns. Turn caps reset whenever something posts, and that something is not
//! always the human -- another agent, a schedule, a queue. Two agents trading
//! messages is a string of turns, each under its cap, with no ceiling. This is
//! the ceiling, and posting again does NOT reset it.
//!
//! Two measures, because not every provider reports a price: USD for every call
//! that did, billed TOKENS for every call that did not. Counting an unpriced
//! call as $0 would be the "spent nothing" vs "measured nothing" lie.
//!
//! Only the human raises it (`/budget`). Snapshots are absolute -- the latest
//! wins -- so replaying the session log restores it and `/clear` carries it forward.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{limits::billed_tokens, messages::Usage};

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

impl CapOverrides {
    fn is_empty(&self) -> bool {
        self.max_usd.is_none() && self.max_unpriced_tokens.is_none()
    }
}

/// Absolute: the latest wins, so replaying the session log restores it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerSnapshot {
    pub usd: f64,
    pub unpriced_tokens: u64,
    /// Raised (or lowered) by `/budget`; absent = the configured default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caps: Option<CapOverrides>,
}

/// Called with the ledger locked: neither method may lock it again.
pub trait LedgerHooks: Send + Sync {
    fn persist(&self, snapshot: &LedgerSnapshot);
    /// The human must hear this (once per crossing).
    fn warn(&self, text: &str);
}

/// One cap the human sets (`/budget`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LedgerCap {
    Usd(f64),
    UnpricedTokens(u64),
}

/// Share of a cap at which the human is warned.
const WARN_SHARE: f64 = 0.8;
const RAISE: &str = "Posting again will not reset it -- raise it with";

#[derive(Clone, Copy)]
enum Measure {
    Usd,
    Tokens,
}

pub struct Ledger {
    pub usd: f64,
    pub unpriced_tokens: u64,
    set: CapOverrides,
    warned_usd: bool,
    warned_tokens: bool,
    defaults: LedgerCaps,
    hooks: Arc<dyn LedgerHooks>,
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

    pub fn snapshot(&self) -> LedgerSnapshot {
        LedgerSnapshot {
            usd: self.usd,
            unpriced_tokens: self.unpriced_tokens,
            caps: (!self.set.is_empty()).then_some(self.set),
        }
    }

    pub fn restore(&mut self, s: &LedgerSnapshot) {
        self.usd = s.usd;
        self.unpriced_tokens = s.unpriced_tokens;
        self.set = s.caps.unwrap_or_default();
        self.rearm();
    }

    /// One model call's bill, or a tool's own spend.
    pub fn charge(&mut self, u: &Usage) {
        match u.cost_usd {
            Some(c) => self.usd += c,
            None => self.unpriced_tokens += billed_tokens(u),
        }
        if u.cost_usd.is_none() && billed_tokens(u) == 0 {
            return;
        }
        self.hooks.persist(&self.snapshot());
        self.maybe_warn();
    }

    /// `/budget`: a new cap. The warning re-arms, so the next crossing is heard too.
    pub fn set_cap(&mut self, cap: LedgerCap) {
        match cap {
            LedgerCap::Usd(v) => self.set.max_usd = Some(v),
            LedgerCap::UnpricedTokens(v) => self.set.max_unpriced_tokens = Some(v),
        }
        self.rearm();
        self.hooks.persist(&self.snapshot());
    }

    /// Why no further model call may go out in this conversation, or None.
    pub fn exhausted(&self) -> Option<String> {
        let LedgerCaps {
            max_usd,
            max_unpriced_tokens,
        } = self.caps();
        if self.over(Measure::Usd, 1.0) {
            return Some(format!(
                "This conversation has spent ${:.2}; its budget is ${max_usd} (KERN_MAX_CONVERSATION_USD). {RAISE} /budget {}.",
                self.usd,
                max_usd * 2.0
            ));
        }
        if self.over(Measure::Tokens, 1.0) {
            return Some(format!(
                "This conversation has billed {} tokens on models that report no price; the cap for those is {max_unpriced_tokens} (KERN_MAX_CONVERSATION_TOKENS). {RAISE} /budget tokens {}.",
                self.unpriced_tokens,
                max_unpriced_tokens * 2
            ));
        }
        None
    }

    pub fn summary(&self) -> String {
        let LedgerCaps {
            max_usd,
            max_unpriced_tokens,
        } = self.caps();
        let cap = |on: bool, text: String| if on { text } else { "no cap".into() };
        let usd_cap = cap(max_usd > 0.0, format!("${max_usd}"));
        let token_cap = cap(max_unpriced_tokens > 0, max_unpriced_tokens.to_string());
        format!(
            "Conversation budget: ${:.2} of {usd_cap} spent; {} unpriced tokens of {token_cap}.",
            self.usd, self.unpriced_tokens
        )
    }

    fn over(&self, m: Measure, share: f64) -> bool {
        let caps = self.caps();
        let (used, cap) = match m {
            Measure::Usd => (self.usd, caps.max_usd),
            Measure::Tokens => (self.unpriced_tokens as f64, caps.max_unpriced_tokens as f64),
        };
        cap > 0.0 && used >= cap * share
    }

    fn rearm(&mut self) {
        self.warned_usd = self.over(Measure::Usd, WARN_SHARE);
        self.warned_tokens = self.over(Measure::Tokens, WARN_SHARE);
    }

    fn warned(&mut self, m: Measure) -> &mut bool {
        match m {
            Measure::Usd => &mut self.warned_usd,
            Measure::Tokens => &mut self.warned_tokens,
        }
    }

    fn maybe_warn(&mut self) {
        for m in [Measure::Usd, Measure::Tokens] {
            if *self.warned(m) || !self.over(m, WARN_SHARE) {
                continue;
            }
            *self.warned(m) = true;
            self.hooks.warn(&format!(
                "{} kern stops at the cap; /budget raises it.",
                self.summary()
            ));
        }
    }
}

#[cfg(test)]
mod tests;
