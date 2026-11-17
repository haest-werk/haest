//! HOLDS: USD set aside for a spender that runs now and pays as it goes (a
//! subagent). Each one checks the ledger only before its own model calls, so
//! four started at once can each pass and overshoot together; a hold taken
//! up front for each one's cap makes the four fit the budget together.
//! Available = cap - spent - held, floored at 0. A hold stays until dropped,
//! however much the spender has already paid against it, so the ledger may
//! refuse what would still have fit, never admit what does not. Holds are not
//! saved: they belong to spenders of this process.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex, Weak},
};

use super::{Ledger, State};

/// Slack for float sums: $0.40 must fit in a cap of $1 with $0.60 spent.
const SLACK: f64 = 1e-9;

/// The holds running now, each by its id. What is held is their sum, taken
/// afresh: a running total would leave float dust behind, and once it
/// reached `inf` it could never come back down.
#[derive(Debug, Default)]
pub(super) struct Holds {
    live: BTreeMap<u64, f64>,
    next: u64,
}

impl Holds {
    /// `usd` held from now on; the id that gives it back.
    fn add(&mut self, usd: f64) -> u64 {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.live.insert(id, usd);
        id
    }

    /// The hold `id` given back.
    fn remove(&mut self, id: u64) {
        self.live.remove(&id);
    }

    /// The USD held, at most `f64::MAX`.
    pub(super) fn total(&self) -> f64 {
        // A fold from +0.0: an empty float `sum` is -0.0, which prints so.
        self.live.values().fold(0.0, |a, b| a + b).min(f64::MAX)
    }
}

/// USD held on a [`Ledger`] ([`Ledger::reserve`]) until this is dropped.
///
/// Dropping it gives the USD back to the ledger that made it, and to no
/// other, however its spender ends: at its end, or cancelled on the way (a
/// timeout, a `select!` branch that lost, an aborted task, a `?`). What the
/// spender paid meanwhile stays charged. A hold that outlives its ledger
/// gives back to nothing.
#[must_use = "a hold is given back when it is dropped"]
pub struct Hold {
    usd: f64,
    /// Which of the ledger's holds it is.
    id: u64,
    /// The sums of the ledger that made it; gone with that ledger.
    ledger: Weak<Mutex<State>>,
}

impl Hold {
    /// The USD it holds.
    pub fn usd(&self) -> f64 {
        self.usd
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let Some(ledger) = self.ledger.upgrade() else {
            return;
        };
        // The ledger's lock is held only for its own sums, never a host's
        // code, so it does not poison; were it poisoned, a destructor must
        // not panic, and the ledger's next call says so.
        if let Ok(mut state) = ledger.lock() {
            state.holds.remove(self.id);
        }
    }
}

/// The USD it holds.
impl fmt::Debug for Hold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hold")
            .field("usd", &self.usd)
            .finish_non_exhaustive()
    }
}

/// A hold that does not fit what is left of the conversation's budget.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[error(
    "${asked:.2} does not fit: ${available:.2} is left of the conversation's budget (max_conversation_usd)"
)]
pub struct NotAvailable {
    /// The USD asked for.
    pub asked: f64,
    /// What was left to hold: cap - spent - held.
    pub available: f64,
}

impl Ledger {
    /// USD held now, for every spender still running; at most `f64::MAX`.
    pub fn held(&self) -> f64 {
        self.state().holds.total()
    }

    /// What a new hold may take: the USD cap in force, less what was spent and
    /// what is held, never below 0. None when there is no USD cap.
    pub fn available(&self) -> Option<f64> {
        self.state().available()
    }

    /// Hold `usd` (a negative or NaN amount is 0, one past `f64::MAX` is
    /// `f64::MAX`) for a spender about to run, until the [`Hold`] is dropped;
    /// refused with what is left when it does not fit. With no USD cap every
    /// hold fits.
    ///
    /// # Errors
    ///
    /// [`NotAvailable`], saying what was asked and what is left, when `usd`
    /// does not fit what the cap leaves after what is spent and held.
    ///
    /// # Examples
    ///
    /// ```
    /// use haest_core::{
    ///     Usage,
    ///     caps::ConversationCaps,
    ///     ledger::{Ledger, NoLedgerHooks, NotAvailable},
    /// };
    ///
    /// # fn main() -> Result<(), haest_core::ledger::NotAvailable> {
    /// let caps = ConversationCaps { max_usd: 1.0, max_unpriced_tokens: 0 };
    /// let ledger = Ledger::new(caps, NoLedgerHooks);
    /// let a = ledger.reserve(0.6)?;
    /// // A second $0.60 does not fit: $0.40 is left.
    /// assert!(matches!(
    ///     ledger.reserve(0.6),
    ///     Err(NotAvailable { available, .. }) if (available - 0.4).abs() < 1e-9
    /// ));
    ///
    /// // The spender paid $0.10 as it ran; at its end the hold goes, the spend stays.
    /// ledger.charge(&Usage { cost_usd: Some(0.1), ..Usage::default() });
    /// drop(a);
    /// assert_eq!(ledger.available().map(|left| (left * 100.0).round()), Some(90.0));
    /// # Ok(())
    /// # }
    /// ```
    pub fn reserve(&self, usd: f64) -> Result<Hold, NotAvailable> {
        // Not `clamp`: it keeps a NaN.
        let usd = if usd > 0.0 { usd.min(f64::MAX) } else { 0.0 };
        let mut state = self.state();
        if let Some(available) = state.available()
            && usd > available + SLACK
        {
            return Err(NotAvailable {
                asked: usd,
                available,
            });
        }
        Ok(Hold {
            usd,
            id: state.holds.add(usd),
            ledger: Arc::downgrade(&self.state),
        })
    }
}

impl State {
    fn available(&self) -> Option<f64> {
        let cap = self.caps().max_usd;
        (cap > 0.0).then(|| (cap - self.usd - self.holds.total()).max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{caps::ConversationCaps, ledger::NoLedgerHooks, messages::Usage};

    fn ledger(max_usd: f64) -> Ledger {
        let caps = ConversationCaps {
            max_usd,
            max_unpriced_tokens: 0,
        };
        Ledger::new(caps, NoLedgerHooks)
    }

    fn spend(l: &Ledger, usd: f64) {
        l.charge(&Usage {
            cost_usd: Some(usd),
            ..Usage::default()
        });
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn holds_and_spend_together_never_pass_the_cap_and_a_refusal_says_what_is_left() {
        let l = ledger(1.0);
        spend(&l, 0.2);
        let a = l.reserve(0.3).unwrap();
        let b = l.reserve(0.3).unwrap();
        assert!(close(l.available().unwrap(), 0.2));
        let refused = l.reserve(0.3).unwrap_err();
        assert!(close(refused.available, 0.2) && close(refused.asked, 0.3));
        // What is left exactly still fits.
        let c = l.reserve(0.2).unwrap();
        assert!(close(l.available().unwrap(), 0.0));
        drop([a, b, c]);
        assert_eq!(l.held(), 0.0);
    }

    /// The amount asked was printed as the float it was: a hold of
    /// $0.10 + $0.20 read "$0.30000000000000004 does not fit".
    #[test]
    fn a_refusal_says_both_amounts_in_cents() {
        let refused = NotAvailable {
            asked: 0.1 + 0.2,
            available: 0.2,
        };
        assert_eq!(
            refused.to_string(),
            "$0.30 does not fit: $0.20 is left of the conversation's budget (max_conversation_usd)"
        );
    }

    #[test]
    fn a_dropped_hold_is_given_back_and_the_real_spend_stays() {
        let l = ledger(1.0);
        let h = l.reserve(0.5).unwrap();
        spend(&l, 0.1);
        // Held and spent both count while it runs: refuse rather than overshoot.
        assert!(close(l.available().unwrap(), 0.4));
        drop(h);
        assert!(close(l.usd(), 0.1));
        assert!(close(l.available().unwrap(), 0.9));
        assert!(l.exhausted().is_none());
    }

    #[test]
    fn with_no_usd_cap_every_hold_fits_and_a_bad_amount_holds_nothing() {
        let l = ledger(0.0);
        assert_eq!(l.available(), None);
        let big = l.reserve(1e9).unwrap();
        let nan = l.reserve(f64::NAN).unwrap();
        let negative = l.reserve(-5.0).unwrap();
        assert_eq!((nan.usd(), negative.usd()), (0.0, 0.0));
        drop([big, nan, negative]);
        assert_eq!(l.held(), 0.0);
    }

    /// A spender cancelled between its hold and its end (a timeout, a
    /// `select!` that lost, an aborted task) never got to give the hold
    /// back: it stayed on the ledger for the life of the conversation.
    #[tokio::test(start_paused = true)]
    async fn a_hold_whose_spender_is_cancelled_gives_itself_back() {
        let l = ledger(1.0);
        let spender = {
            let l = l.clone();
            async move {
                let _hold = l.reserve(0.6).unwrap();
                std::future::pending::<()>().await;
            }
        };
        let cancelled = tokio::time::timeout(Duration::from_millis(50), spender).await;
        assert!(cancelled.is_err());
        assert_eq!(l.held(), 0.0);
        assert!(close(l.available().unwrap(), 1.0));
        assert!(l.reserve(0.5).is_ok(), "the next spender fits");
    }

    /// A hold gives back to the ledger that made it and no other: one from
    /// conversation `a` reset `b`'s count, and `b` then admitted spend that
    /// did not fit.
    #[test]
    fn a_hold_gives_back_only_to_the_ledger_that_made_it() {
        let (a, b) = (ledger(1.0), ledger(1.0));
        let from_a = a.reserve(0.1).unwrap();
        let on_b = b.reserve(0.9).unwrap();
        drop(from_a);
        assert_eq!(a.held(), 0.0);
        assert!(close(b.held(), 0.9));
        assert!(b.reserve(0.9).is_err(), "b still holds its own");
        drop(on_b);
        assert_eq!(b.held(), 0.0);
    }

    /// A hold that outlives its ledger gives back to nothing, and says so
    /// nowhere: the ledger and its sums are gone.
    #[test]
    fn a_hold_that_outlives_its_ledger_drops_quietly() {
        let l = ledger(1.0);
        let h = l.reserve(0.5).unwrap();
        drop(l);
        assert!(close(h.usd(), 0.5));
        drop(h);
    }

    /// Taken while there was no USD cap, an infinite hold gave back `inf -
    /// inf` (NaN, floored to 0) and erased the holds still running, and two
    /// holds of `f64::MAX` summed to `inf`, which never came back down.
    #[test]
    fn a_huge_hold_gives_back_only_itself() {
        for big in [f64::INFINITY, f64::MAX] {
            let l = ledger(0.0);
            let a = l.reserve(big).unwrap();
            let b = l.reserve(big).unwrap();
            let small = l.reserve(0.5).unwrap();
            l.set_cap(crate::caps::ConversationCap::Usd(1.0));
            assert_eq!(l.available(), Some(0.0), "{big:e}");
            drop(a);
            assert_eq!(l.available(), Some(0.0), "{big:e}: the other big hold runs");
            drop(b);
            assert!(close(l.held(), 0.5), "{big:e}: held {}", l.held());
            assert!(close(l.available().unwrap(), 0.5), "{big:e}");
            assert!(
                l.reserve(0.9).is_err(),
                "{big:e}: the $0.50 hold still runs"
            );
            drop(small);
            assert_eq!(l.held(), 0.0);
        }
    }

    /// What is held stops at `f64::MAX`, as the ledger's spend does, and so
    /// does a hold of more.
    #[test]
    fn what_is_held_stops_at_f64_max() {
        let l = ledger(0.0);
        let a = l.reserve(f64::INFINITY).unwrap();
        assert_eq!(a.usd(), f64::MAX);
        let b = l.reserve(f64::MAX).unwrap();
        assert_eq!(l.held(), f64::MAX);
        drop([a, b]);
    }

    #[test]
    fn holds_are_not_saved_and_a_restore_keeps_what_is_held() {
        let l = ledger(1.0);
        let h = l.reserve(0.5).unwrap();
        let snapshot = l.snapshot();
        l.restore(&snapshot);
        assert!(close(l.held(), 0.5), "a running spender keeps its hold");
        assert_eq!(ledger(1.0).held(), 0.0);
        drop(h);
    }
}
