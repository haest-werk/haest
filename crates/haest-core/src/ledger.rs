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
//! [`SharedDeps::ledger`](crate::SharedDeps::ledger). The turn guard charges
//! every model call and tool spend to it, and stops a turn before its next
//! model call once [`Ledger::exhausted`] says so. A host's way to raise a cap
//! ([`Ledger::with_remedies`]) ends the message that says so.
//!
//! A spender that runs alongside others and pays as it goes (a subagent)
//! holds its cap first ([`Ledger::reserve`]), so those running together fit
//! the budget together.
//!
//! The ledger is a threshold site ([`LedgerSite`]): at every charge it checks
//! its thresholds, by default the warning at 80% of each cap
//! ([`Ledger::with_thresholds`] changes them).

mod hold;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use hold::{Hold, NotAvailable};

use crate::{
    caps::{ConversationCap, ConversationCapOverrides, ConversationCaps},
    messages::Usage,
    remedy::{Limit, Remedies},
    threshold::{Action, LedgerSite, Mark, Meter, Reading, Scope, Thresholds, Watch},
};

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
    pub caps: Option<ConversationCapOverrides>,
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
    /// A crossing is the spend reaching a warning threshold, 80% of a cap by
    /// default; setting a new cap re-arms it.
    fn warn(&self, text: &str);
}

const STAYS: &str = "Posting again will not reset it.";

/// The conversation's USD.
const USD: Meter = Meter::Usd(Scope::Conversation);
/// The conversation's unpriced tokens.
const TOKENS: Meter = Meter::Tokens(Scope::Conversation);

/// What one conversation has spent across all its turns, and its caps.
///
/// # Examples
///
/// ```
/// use std::sync::{Arc, Mutex};
///
/// use haest_core::{
///     caps::{ConversationCap, ConversationCaps},
///     ledger::{Ledger, LedgerHooks, LedgerSnapshot},
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
    usd: f64,
    unpriced_tokens: u64,
    set: ConversationCapOverrides,
    warnings: Watch<LedgerSite>,
    defaults: ConversationCaps,
    hooks: Arc<dyn LedgerHooks>,
    remedies: Remedies,
    /// USD held for spenders running now ([`Ledger::reserve`]), and how many holds.
    held: f64,
    holds: usize,
}

impl Ledger {
    /// An empty ledger with `defaults` as its caps; `hooks` hear every charge
    /// and every new cap.
    pub fn new(defaults: ConversationCaps, hooks: Arc<dyn LedgerHooks>) -> Self {
        Self {
            usd: 0.0,
            unpriced_tokens: 0,
            set: ConversationCapOverrides::default(),
            warnings: Watch::default(),
            defaults,
            hooks,
            remedies: Remedies::default(),
            held: 0.0,
            holds: 0,
        }
    }

    /// The host's way to raise a cap, added to the message that the cap is
    /// reached ([`exhausted`](Self::exhausted)) or near (the warning).
    pub fn with_remedies(mut self, remedies: Remedies) -> Self {
        self.remedies = remedies;
        self
    }

    /// What the ledger checks at every charge, in place of its default
    /// warnings ([`LedgerSite`]). What is spent already counts as heard.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use haest_core::{
    ///     caps::ConversationCaps,
    ///     ledger::{Ledger, LedgerHooks, LedgerSnapshot},
    ///     threshold::{LedgerSite, Thresholds},
    /// };
    ///
    /// struct Quiet;
    /// impl LedgerHooks for Quiet {
    ///     fn persist(&self, _: &LedgerSnapshot) {}
    ///     fn warn(&self, _: &str) {}
    /// }
    ///
    /// // A ledger that never warns, only stops at its cap.
    /// let caps = ConversationCaps { max_usd: 10.0, max_unpriced_tokens: 0 };
    /// let ledger = Ledger::new(caps, Arc::new(Quiet)).with_thresholds(Thresholds::<LedgerSite>::none());
    /// ```
    pub fn with_thresholds(mut self, thresholds: Thresholds<LedgerSite>) -> Self {
        self.warnings = Watch::new(thresholds);
        self.rearm();
        self
    }

    /// USD spent by calls that reported a price.
    pub fn usd(&self) -> f64 {
        self.usd
    }

    /// Tokens billed by calls that reported no price.
    pub fn unpriced_tokens(&self) -> u64 {
        self.unpriced_tokens
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
    /// A spend already past 80% of a cap does not warn again. Holds stay:
    /// they are this process's spenders, never in a snapshot.
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
            None => self.unpriced_tokens = self.unpriced_tokens.saturating_add(u.context_tokens()),
        }
        if u.cost_usd.is_none() && u.context_tokens() == 0 {
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
        let [usd, tokens] = self.readings();
        let at_cap = |r: &Reading| Mark::share(1.0).reached(r);
        let (r, text) = if at_cap(&usd) {
            let (spent, cap) = (usd.value, usd.cap.unwrap_or_default());
            let text =
                format!("This conversation has spent ${spent:.2}; its budget is ${cap}. {STAYS}");
            (usd, text)
        } else if at_cap(&tokens) {
            let (billed, cap) = (tokens.value as u64, tokens.cap.unwrap_or_default() as u64);
            let text = format!(
                "This conversation has billed {billed} tokens on models that report no price; the cap for those is {cap}. {STAYS}"
            );
            (tokens, text)
        } else {
            return None;
        };
        Some(r.remedied(text, &self.remedies))
    }

    /// One line for the human: what was spent of each cap, "(no cap)" for
    /// one that is off.
    pub fn summary(&self) -> String {
        let [usd, tokens] = self.readings();
        format!("Conversation budget: {}; {}.", spent(&usd), spent(&tokens))
    }

    /// The conversation's USD and unpriced tokens against the caps in force.
    pub(crate) fn readings(&self) -> [Reading; 2] {
        let caps = self.caps();
        let reading = |meter, value: f64, cap: f64, limit| Reading {
            meter,
            value,
            cap: (cap > 0.0).then_some(cap),
            limit: Some(Limit::Conversation(limit)),
        };
        [
            reading(
                USD,
                self.usd,
                caps.max_usd,
                ConversationCap::Usd(caps.max_usd),
            ),
            reading(
                TOKENS,
                self.unpriced_tokens as f64,
                caps.max_unpriced_tokens as f64,
                ConversationCap::UnpricedTokens(caps.max_unpriced_tokens),
            ),
        ]
    }

    /// What is spent counts as heard: a warning already past its mark stays
    /// quiet, one under it re-arms.
    fn rearm(&mut self) {
        let readings = self.readings();
        self.warnings
            .settle(|m| readings.into_iter().find(|r| r.meter == m));
    }

    fn maybe_warn(&mut self) {
        let readings = self.readings();
        let due = self.warnings.check(
            |m| readings.into_iter().find(|r| r.meter == m),
            Action::Warn,
        );
        for crossing in due {
            self.hooks.warn(&crossing.remedied_text(&self.remedies));
        }
    }
}

/// What a conversation meter stands at against its cap: "$8.00 spent of
/// $10", "900 unpriced tokens billed (no cap)".
pub(crate) fn spent(r: &Reading) -> String {
    let (used, cap) = match r.meter {
        Meter::Usd(_) => (
            format!("${:.2} spent", r.value),
            r.cap.map(|cap| format!("${cap}")),
        ),
        _ => (
            format!("{} unpriced tokens billed", r.value as u64),
            r.cap.map(|cap| (cap as u64).to_string()),
        ),
    };
    match cap {
        Some(cap) => format!("{used} of {cap}"),
        None => format!("{used} (no cap)"),
    }
}

#[cfg(test)]
mod tests;
