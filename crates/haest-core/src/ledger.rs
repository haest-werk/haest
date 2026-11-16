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
//! Only the human raises it ([`Ledger::set_cap`], through the host). Snapshots
//! are absolute -- the latest wins -- so replaying the session log restores it,
//! and a cleared conversation carries it forward.
//!
//! A host makes a [`Ledger`] and shares it as `Arc<Mutex<Ledger>>` on
//! [`AgentDeps::ledger`](crate::AgentDeps::ledger). The turn guard charges
//! every model call and tool spend to it, and stops a turn before its next
//! model call once [`Ledger::exhausted`] says so. A host's way to raise a cap
//! ([`Ledger::with_remedies`]) ends the message that says so.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    caps::billed_tokens,
    messages::Usage,
    remedy::{Limit, Remedies},
};

/// A conversation's two caps: the defaults a host configures, or the caps in
/// force ([`Ledger::caps`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationCaps {
    /// USD across the conversation; 0 = no cap.
    pub max_usd: f64,
    /// Tokens billed by calls that reported no price; 0 = no cap.
    pub max_unpriced_tokens: u64,
}

/// Caps the human set; a None field = the configured default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapOverrides {
    /// The USD cap the human set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
    /// The unpriced-token cap the human set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_unpriced_tokens: Option<u64>,
}

impl CapOverrides {
    fn is_empty(&self) -> bool {
        self.max_usd.is_none() && self.max_unpriced_tokens.is_none()
    }
}

/// A ledger's whole state, as the session log keeps it.
///
/// Absolute: the latest wins, so replaying the session log restores it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerSnapshot {
    /// USD spent by calls that reported a price.
    pub usd: f64,
    /// Tokens billed by calls that reported no price.
    pub unpriced_tokens: u64,
    /// The caps the human set ([`Ledger::set_cap`]); absent = the configured
    /// defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caps: Option<CapOverrides>,
}

/// Where a [`Ledger`] reports: snapshots to persist, warnings for the human.
///
/// Called with the ledger locked: neither method may lock it again.
pub trait LedgerHooks: Send + Sync {
    /// Save this snapshot (to the session log). Called after every charge
    /// that billed something and after every cap change.
    fn persist(&self, snapshot: &LedgerSnapshot);
    /// The human must hear this (once per crossing).
    ///
    /// A crossing is the spend reaching 80% of a cap; setting a new cap
    /// re-arms it.
    fn warn(&self, text: &str);
}

/// One cap the human sets ([`Ledger::set_cap`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConversationCap {
    /// USD across the conversation; 0 = no cap.
    Usd(f64),
    /// Tokens billed by calls that reported no price; 0 = no cap.
    UnpricedTokens(u64),
}

/// Share of a cap at which the human is warned.
const WARN_SHARE: f64 = 0.8;
const STAYS: &str = "Posting again will not reset it.";

#[derive(Clone, Copy)]
enum Measure {
    Usd,
    Tokens,
}

/// What one conversation has spent across all its turns, and its caps.
///
/// # Examples
///
/// ```
/// use std::sync::{Arc, Mutex};
///
/// use haest_core::{
///     ledger::{Ledger, ConversationCap, ConversationCaps, LedgerHooks, LedgerSnapshot},
///     messages::Usage,
/// };
///
/// #[derive(Default)]
/// struct Warnings(Mutex<Vec<String>>);
///
/// impl LedgerHooks for Warnings {
///     fn persist(&self, _snapshot: &LedgerSnapshot) {}
///     fn warn(&self, text: &str) {
///         self.0.lock().unwrap().push(text.to_string());
///     }
/// }
///
/// let warnings = Arc::new(Warnings::default());
/// let caps = ConversationCaps { max_usd: 1.0, max_unpriced_tokens: 0 };
/// let mut ledger = Ledger::new(caps, warnings.clone());
///
/// ledger.charge(&Usage { cost_usd: Some(0.85), ..Usage::default() });
/// assert_eq!(warnings.0.lock().unwrap().len(), 1); // past 80% of the cap: warned once
/// assert!(ledger.exhausted().is_none());
///
/// ledger.charge(&Usage { cost_usd: Some(0.20), ..Usage::default() });
/// assert!(ledger.exhausted().is_some()); // no further model call in this conversation
///
/// ledger.set_cap(ConversationCap::Usd(2.0)); // the human raises it
/// assert!(ledger.exhausted().is_none());
/// ```
pub struct Ledger {
    /// USD spent by calls that reported a price. [`Ledger::charge`] persists
    /// and warns; writing the field directly does neither.
    pub usd: f64,
    /// Tokens billed by calls that reported no price; the same goes as for `usd`.
    pub unpriced_tokens: u64,
    set: CapOverrides,
    warned_usd: bool,
    warned_tokens: bool,
    defaults: ConversationCaps,
    hooks: Arc<dyn LedgerHooks>,
    remedies: Remedies,
}

impl Ledger {
    /// An empty ledger with `defaults` as its caps; `hooks` hear every charge
    /// and every new cap.
    pub fn new(defaults: ConversationCaps, hooks: Arc<dyn LedgerHooks>) -> Self {
        Self {
            usd: 0.0,
            unpriced_tokens: 0,
            set: CapOverrides::default(),
            warned_usd: false,
            warned_tokens: false,
            defaults,
            hooks,
            remedies: Remedies::default(),
        }
    }

    /// The host's way to raise a cap, added to the message that the cap is
    /// reached ([`exhausted`](Self::exhausted)) or near (the warning).
    pub fn with_remedies(mut self, remedies: Remedies) -> Self {
        self.remedies = remedies;
        self
    }

    /// The caps in force: each one the human set, else the default.
    pub fn caps(&self) -> ConversationCaps {
        ConversationCaps {
            max_usd: self.set.max_usd.unwrap_or(self.defaults.max_usd),
            max_unpriced_tokens: self
                .set
                .max_unpriced_tokens
                .unwrap_or(self.defaults.max_unpriced_tokens),
        }
    }

    /// The state to persist; `caps` names only what the human set.
    pub fn snapshot(&self) -> LedgerSnapshot {
        LedgerSnapshot {
            usd: self.usd,
            unpriced_tokens: self.unpriced_tokens,
            caps: (!self.set.is_empty()).then_some(self.set),
        }
    }

    /// Take a snapshot's state (replaying a session log); persists nothing.
    /// A spend already past 80% of a cap does not warn again.
    pub fn restore(&mut self, s: &LedgerSnapshot) {
        self.usd = s.usd;
        self.unpriced_tokens = s.unpriced_tokens;
        self.set = s.caps.unwrap_or_default();
        self.rearm();
    }

    /// One model call's bill, or a tool's own spend.
    ///
    /// A priced call adds its USD; an unpriced one its billed tokens. Then
    /// the snapshot is persisted and the human warned at a crossing; a call
    /// with neither a price nor tokens changes nothing.
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

    /// A new cap, from the human. The warning re-arms, so the next crossing is heard too.
    pub fn set_cap(&mut self, cap: ConversationCap) {
        match cap {
            ConversationCap::Usd(v) => self.set.max_usd = Some(v),
            ConversationCap::UnpricedTokens(v) => self.set.max_unpriced_tokens = Some(v),
        }
        self.rearm();
        self.hooks.persist(&self.snapshot());
    }

    /// Why no further model call may go out in this conversation, or None.
    /// The text ends with the host's remedy, if it has one.
    pub fn exhausted(&self) -> Option<String> {
        let ConversationCaps {
            max_usd,
            max_unpriced_tokens,
        } = self.caps();
        let (m, text) = if self.over(Measure::Usd, 1.0) {
            let spent = self.usd;
            (
                Measure::Usd,
                format!(
                    "This conversation has spent ${spent:.2}; its budget is ${max_usd}. {STAYS}"
                ),
            )
        } else if self.over(Measure::Tokens, 1.0) {
            let billed = self.unpriced_tokens;
            (
                Measure::Tokens,
                format!(
                    "This conversation has billed {billed} tokens on models that report no price; the cap for those is {max_unpriced_tokens}. {STAYS}"
                ),
            )
        } else {
            return None;
        };
        Some(self.remedies.add_to(text, self.limit(m)))
    }

    /// One line for the human: what was spent of each cap.
    pub fn summary(&self) -> String {
        let ConversationCaps {
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

    /// The cap in force for `m`, as the host's remedy gets it.
    fn limit(&self, m: Measure) -> Limit {
        let caps = self.caps();
        Limit::Conversation(match m {
            Measure::Usd => ConversationCap::Usd(caps.max_usd),
            Measure::Tokens => ConversationCap::UnpricedTokens(caps.max_unpriced_tokens),
        })
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
            let text = format!("{} haest stops at the cap.", self.summary());
            self.hooks.warn(&self.remedies.add_to(text, self.limit(m)));
        }
    }
}

#[cfg(test)]
mod tests;
