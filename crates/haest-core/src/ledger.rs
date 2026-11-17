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
//! A host makes a [`Ledger`], a cheap `Clone` whose clones are the one
//! ledger, and hands a clone to [`SharedDeps::with_ledger`](crate::SharedDeps::with_ledger)
//! (a subagent shares its parent's). The turn guard charges
//! every model call and tool spend to it, and stops a turn before its next
//! model call once [`Ledger::exhausted`] says so. A host's way to raise a cap
//! ([`Ledger::set_remedies`]) ends the message that says so.
//!
//! A spender that runs alongside others and pays as it goes (a subagent)
//! holds its cap first ([`Ledger::reserve`]), so those running together fit
//! the budget together.
//!
//! The ledger is a threshold site ([`LedgerSite`]): at every charge it checks
//! its thresholds, by default the warning at 80% of each cap
//! ([`Ledger::set_thresholds`] changes them).

mod hold;

use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};

pub use hold::{Hold, NotAvailable};

use crate::{
    caps::{ConversationCap, ConversationCapOverrides, ConversationCaps},
    messages::Usage,
    remedy::{Limit, Remedies},
    threshold::{Action, Crossing, LedgerSite, Mark, Meter, Reading, Scope, Thresholds, Watch},
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
/// Both default to doing nothing; [`NoLedgerHooks`] is a ledger that
/// reports nowhere.
///
/// Called after the ledger has done its sums and let go of them: a hook may
/// read the ledger it hears from (its [`summary`](Ledger::summary), say).
pub trait LedgerHooks: Send + Sync {
    /// Save this snapshot (to the session log). Called after every charge
    /// that billed something and after every cap change, one call at a time
    /// and never an older snapshot after a newer one, so the last one saved
    /// is the ledger's state. It may read the ledger, but must not charge it
    /// or set its cap. Default: not saved.
    fn persist(&self, _snapshot: &LedgerSnapshot) {}
    /// The human must hear this (once per crossing). Default: dropped.
    ///
    /// A crossing is the spend reaching a warning threshold, 80% of a cap by
    /// default; setting a new cap re-arms it.
    fn warn(&self, _text: &str) {}
}

/// Ledger hooks that save nothing and tell no one: a ledger a host only
/// reads (`Ledger::new(caps, NoLedgerHooks)`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoLedgerHooks;

impl LedgerHooks for NoLedgerHooks {}

crate::plug_ref! {
    /// [`LedgerHooks`], shared: what a [`Ledger`] reports to. Built from the
    /// hooks themselves, or from an `Arc` of the host's object that fills
    /// other slots too.
    pub struct LedgerHooksRef(dyn LedgerHooks);
}

const STAYS: &str = "Posting again will not reset it.";

/// The conversation's USD.
const USD: Meter = Meter::Usd(Scope::Conversation);
/// The conversation's unpriced tokens.
const TOKENS: Meter = Meter::Tokens(Scope::Conversation);

/// What one conversation has spent across all its turns, and its caps.
///
/// A cheap `Clone`: every clone is the same ledger, so the host keeps one to
/// read and raise while the agent and its subagents charge theirs. Every
/// method takes `&self`.
///
/// # Examples
///
/// ```
/// use std::sync::mpsc;
///
/// use haest_core::{
///     Usage,
///     caps::{ConversationCap, ConversationCaps},
///     ledger::{Ledger, LedgerHooks, LedgerSnapshot},
/// };
///
/// /// Hands every warning to the host's UI.
/// struct Warnings(mpsc::Sender<String>);
///
/// impl LedgerHooks for Warnings {
///     fn persist(&self, _snapshot: &LedgerSnapshot) {}
///     fn warn(&self, text: &str) {
///         let _ = self.0.send(text.to_string());
///     }
/// }
///
/// let (warn, warned) = mpsc::channel();
/// let caps = ConversationCaps { max_usd: 1.0, max_unpriced_tokens: 0 };
/// let ledger = Ledger::new(caps, Warnings(warn));
///
/// ledger.charge(&Usage { cost_usd: Some(0.85), ..Usage::default() });
/// assert_eq!(warned.try_iter().count(), 1); // past 80% of the cap: warned once
/// assert!(ledger.exhausted().is_none());
///
/// ledger.charge(&Usage { cost_usd: Some(0.20), ..Usage::default() });
/// assert!(ledger.exhausted().is_some()); // no further model call in this conversation
///
/// ledger.set_cap(ConversationCap::Usd(2.0)); // the human raises it
/// assert!(ledger.exhausted().is_none());
/// ```
#[derive(Clone)]
pub struct Ledger {
    state: Arc<Mutex<State>>,
    hooks: LedgerHooksRef,
    /// Held while a snapshot is persisted: one at a time, in order.
    persisted: Arc<Mutex<u64>>,
}

/// A ledger's sums, behind its lock.
struct State {
    usd: f64,
    unpriced_tokens: u64,
    set: ConversationCapOverrides,
    warnings: Watch<LedgerSite>,
    defaults: ConversationCaps,
    remedies: Remedies,
    /// USD held for spenders running now ([`Ledger::reserve`]), and how many holds.
    held: f64,
    holds: usize,
    /// Bumped at every snapshot taken to persist, so an older one is never
    /// saved after a newer one.
    version: u64,
}

/// What it has spent and its caps, as [`Ledger::snapshot`] and
/// [`Ledger::caps`] say; `<locked>` while another thread holds it.
impl fmt::Debug for Ledger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("Ledger");
        match self.state.try_lock() {
            Ok(state) => out
                .field("usd", &state.usd)
                .field("unpriced_tokens", &state.unpriced_tokens)
                .field("held", &state.held)
                .field("caps", &state.caps()),
            Err(_) => out.field("state", &format_args!("<locked>")),
        };
        out.finish_non_exhaustive()
    }
}

/// What a change hands the hooks once the ledger is unlocked: the snapshot
/// to persist (with its version), the warnings due, and the host's remedies
/// to end them with. The remedies run unlocked too: one may read the ledger.
#[must_use]
struct Report {
    snapshot: Option<(u64, LedgerSnapshot)>,
    warnings: Vec<Crossing>,
    remedies: Remedies,
}

impl Ledger {
    /// An empty ledger with `defaults` as its caps; `hooks` hear every charge
    /// and every new cap.
    pub fn new(defaults: ConversationCaps, hooks: impl Into<LedgerHooksRef>) -> Self {
        let state = State {
            usd: 0.0,
            unpriced_tokens: 0,
            set: ConversationCapOverrides::default(),
            warnings: Watch::default(),
            defaults,
            remedies: Remedies::default(),
            held: 0.0,
            holds: 0,
            version: 0,
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            hooks: hooks.into(),
            persisted: Arc::default(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("ledger lock poisoned")
    }

    /// Hand the hooks what a change left them, the ledger unlocked.
    fn report(&self, report: Report) {
        if let Some((version, snapshot)) = report.snapshot {
            let mut last = self.persisted.lock().expect("ledger lock poisoned");
            if version > *last {
                *last = version;
                self.hooks.persist(&snapshot);
            }
        }
        for crossing in report.warnings {
            self.hooks.warn(&crossing.remedied_text(&report.remedies));
        }
    }

    /// The host's way to raise a cap, added to the message that the cap is
    /// reached ([`exhausted`](Self::exhausted)) or near (the warning). Every
    /// clone of this ledger says it from now on.
    ///
    /// They run with the ledger unlocked, so a remedy may read it (the spend
    /// so far, [`usd`](Self::usd)), but must not charge it or set its cap.
    pub fn set_remedies(&self, remedies: Remedies) {
        self.state().remedies = remedies;
    }

    /// What the ledger checks at every charge, in place of its default
    /// warnings ([`LedgerSite`]), for every clone of it. What is spent
    /// already counts as heard.
    ///
    /// A threshold's `meter`, `mark`, `action` and `rearm` run under the
    /// ledger's lock: they must not read the ledger. Its `text` runs once
    /// the ledger is unlocked, and may.
    ///
    /// ```
    /// use haest_core::{
    ///     caps::ConversationCaps,
    ///     ledger::{Ledger, NoLedgerHooks},
    ///     threshold::{LedgerSite, Thresholds},
    /// };
    ///
    /// // A ledger that never warns, only stops at its cap.
    /// let caps = ConversationCaps { max_usd: 10.0, max_unpriced_tokens: 0 };
    /// let ledger = Ledger::new(caps, NoLedgerHooks);
    /// ledger.set_thresholds(Thresholds::<LedgerSite>::none());
    /// ```
    pub fn set_thresholds(&self, thresholds: Thresholds<LedgerSite>) {
        let mut state = self.state();
        state.warnings = Watch::new(thresholds);
        state.rearm();
    }

    /// USD spent by calls that reported a price.
    pub fn usd(&self) -> f64 {
        self.state().usd
    }

    /// Tokens billed by calls that reported no price.
    pub fn unpriced_tokens(&self) -> u64 {
        self.state().unpriced_tokens
    }

    /// The caps in force: each one the human set, else the default.
    pub fn caps(&self) -> ConversationCaps {
        self.state().caps()
    }

    /// The state to persist; `caps` names only what the human set.
    pub fn snapshot(&self) -> LedgerSnapshot {
        self.state().snapshot()
    }

    /// Take a snapshot's state (replaying a session log); persists nothing.
    /// A spend already past 80% of a cap does not warn again. Holds stay:
    /// they are this process's spenders, never in a snapshot.
    pub fn restore(&self, s: &LedgerSnapshot) {
        let mut state = self.state();
        state.usd = s.usd;
        state.unpriced_tokens = s.unpriced_tokens;
        state.set = s.caps.unwrap_or_default();
        state.rearm();
    }

    /// One model call's bill, or a tool's own spend.
    ///
    /// A priced call adds its USD; an unpriced one its billed tokens. Then
    /// the snapshot is persisted and the human warned at a crossing; a call
    /// with neither a price nor tokens changes nothing.
    pub fn charge(&self, u: &Usage) {
        let report = {
            let mut state = self.state();
            match u.cost_usd {
                Some(c) => state.usd += c,
                None => {
                    state.unpriced_tokens = state.unpriced_tokens.saturating_add(u.context_tokens())
                }
            }
            if u.cost_usd.is_none() && u.context_tokens() == 0 {
                return;
            }
            Report {
                snapshot: Some(state.versioned_snapshot()),
                warnings: state.due_warnings(),
                remedies: state.remedies.clone(),
            }
        };
        self.report(report);
    }

    /// A new cap, from the human. The warning re-arms, so the next crossing is heard too.
    pub fn set_cap(&self, cap: ConversationCap) {
        let report = {
            let mut state = self.state();
            match cap {
                ConversationCap::Usd(v) => state.set.max_usd = Some(v),
                ConversationCap::UnpricedTokens(v) => state.set.max_unpriced_tokens = Some(v),
            }
            state.rearm();
            Report {
                snapshot: Some(state.versioned_snapshot()),
                warnings: Vec::new(),
                remedies: Remedies::default(),
            }
        };
        self.report(report);
    }

    /// Why no further model call may go out in this conversation, or None.
    /// The text ends with the host's remedy, if it has one.
    pub fn exhausted(&self) -> Option<String> {
        self.exhausted_at().map(|(text, _)| text)
    }

    /// [`exhausted`](Self::exhausted), and the cap that is spent.
    pub(crate) fn exhausted_at(&self) -> Option<(String, Option<Limit>)> {
        // The remedies run once the ledger is unlocked: one may read it.
        let ([usd, tokens], remedies) = {
            let state = self.state();
            (state.readings(), state.remedies.clone())
        };
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
        Some((r.remedied(text, &remedies), r.limit))
    }

    /// One line for the human: what was spent of each cap, "(no cap)" for
    /// one that is off.
    pub fn summary(&self) -> String {
        let [usd, tokens] = self.readings();
        format!("Conversation budget: {}; {}.", spent(&usd), spent(&tokens))
    }

    /// The conversation's USD and unpriced tokens against the caps in force.
    pub(crate) fn readings(&self) -> [Reading; 2] {
        self.state().readings()
    }
}

impl State {
    fn caps(&self) -> ConversationCaps {
        ConversationCaps {
            max_usd: self.set.max_usd.unwrap_or(self.defaults.max_usd),
            max_unpriced_tokens: self
                .set
                .max_unpriced_tokens
                .unwrap_or(self.defaults.max_unpriced_tokens),
        }
    }

    fn snapshot(&self) -> LedgerSnapshot {
        LedgerSnapshot {
            usd: self.usd,
            unpriced_tokens: self.unpriced_tokens,
            caps: (!self.set.is_empty()).then_some(self.set),
        }
    }

    /// The snapshot to persist, numbered after every one taken before it.
    fn versioned_snapshot(&mut self) -> (u64, LedgerSnapshot) {
        self.version += 1;
        (self.version, self.snapshot())
    }

    fn readings(&self) -> [Reading; 2] {
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

    /// The warnings a charge crossed. Their text (the threshold's and the
    /// host's remedy) is written once the ledger is unlocked.
    fn due_warnings(&mut self) -> Vec<Crossing> {
        let readings = self.readings();
        self.warnings.check(
            |m| readings.into_iter().find(|r| r.meter == m),
            Action::Warn,
        )
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
