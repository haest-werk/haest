//! HOLDS: USD set aside for a spender that runs now and pays as it goes (a
//! subagent). Each one checks the ledger only before its own model calls, so
//! four started at once can each pass and overshoot together; a hold taken
//! up front for each one's cap makes the four fit the budget together.
//! Available = cap - spent - held, floored at 0. A hold stays until released,
//! however much the spender has already paid against it, so the ledger may
//! refuse what would still have fit, never admit what does not. Holds are not
//! saved: they belong to spenders of this process.

use super::Ledger;

/// Slack for float sums: $0.40 must fit in a cap of $1 with $0.60 spent.
const SLACK: f64 = 1e-9;

/// USD held on a [`Ledger`] ([`Ledger::reserve`]) until it is given back with
/// [`Ledger::release`].
#[derive(Debug)]
#[must_use = "a hold is held until Ledger::release takes it back"]
pub struct Hold {
    usd: f64,
}

impl Hold {
    /// The USD it holds.
    pub fn usd(&self) -> f64 {
        self.usd
    }
}

/// A hold that does not fit what is left of the conversation's budget.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[error(
    "${asked} does not fit: ${available:.2} is left of the conversation's budget (max_conversation_usd)"
)]
pub struct NotAvailable {
    /// The USD asked for.
    pub asked: f64,
    /// What was left to hold: cap - spent - held.
    pub available: f64,
}

impl Ledger {
    /// USD held now, for every spender still running.
    pub fn held(&self) -> f64 {
        self.held
    }

    /// What a new hold may take: the USD cap in force, less what was spent and
    /// what is held, never below 0. None when there is no USD cap.
    pub fn available(&self) -> Option<f64> {
        let cap = self.caps().max_usd;
        (cap > 0.0).then(|| (cap - self.usd - self.held).max(0.0))
    }

    /// Hold `usd` (a negative or NaN amount is 0) for a spender about to run;
    /// refused with what is left when it does not fit. With no USD cap every
    /// hold fits.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use haest_core::{
    ///     caps::ConversationCaps,
    ///     ledger::{Ledger, LedgerHooks, LedgerSnapshot},
    ///     messages::Usage,
    /// };
    ///
    /// struct Quiet;
    /// impl LedgerHooks for Quiet {
    ///     fn persist(&self, _: &LedgerSnapshot) {}
    ///     fn warn(&self, _: &str) {}
    /// }
    ///
    /// let caps = ConversationCaps { max_usd: 1.0, max_unpriced_tokens: 0 };
    /// let mut ledger = Ledger::new(caps, Arc::new(Quiet));
    /// let a = ledger.reserve(0.6).unwrap();
    /// let refused = ledger.reserve(0.6).unwrap_err();
    /// assert!((refused.available - 0.4).abs() < 1e-9);
    ///
    /// // The spender paid $0.10 as it ran; at its end the hold goes, the spend stays.
    /// ledger.charge(&Usage { cost_usd: Some(0.1), ..Usage::default() });
    /// ledger.release(a);
    /// assert!((ledger.available().unwrap() - 0.9).abs() < 1e-9);
    /// ```
    pub fn reserve(&mut self, usd: f64) -> Result<Hold, NotAvailable> {
        let usd = usd.max(0.0);
        if let Some(available) = self.available()
            && usd > available + SLACK
        {
            return Err(NotAvailable {
                asked: usd,
                available,
            });
        }
        self.held += usd;
        self.holds += 1;
        Ok(Hold { usd })
    }

    /// Give a hold back. Its spender's real spend stays charged.
    pub fn release(&mut self, hold: Hold) {
        self.holds = self.holds.saturating_sub(1);
        self.held = if self.holds == 0 {
            // No float dust left behind once nothing is held.
            0.0
        } else {
            (self.held - hold.usd).max(0.0)
        };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        caps::ConversationCaps,
        ledger::{LedgerHooks, LedgerSnapshot},
        messages::Usage,
    };

    struct Quiet;
    impl LedgerHooks for Quiet {
        fn persist(&self, _: &LedgerSnapshot) {}
        fn warn(&self, _: &str) {}
    }

    fn ledger(max_usd: f64) -> Ledger {
        let caps = ConversationCaps {
            max_usd,
            max_unpriced_tokens: 0,
        };
        Ledger::new(caps, Arc::new(Quiet))
    }

    fn spend(l: &mut Ledger, usd: f64) {
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
        let mut l = ledger(1.0);
        spend(&mut l, 0.2);
        let a = l.reserve(0.3).unwrap();
        let b = l.reserve(0.3).unwrap();
        assert!(close(l.available().unwrap(), 0.2));
        let refused = l.reserve(0.3).unwrap_err();
        assert!(close(refused.available, 0.2) && close(refused.asked, 0.3));
        // What is left exactly still fits.
        let c = l.reserve(0.2).unwrap();
        assert!(close(l.available().unwrap(), 0.0));
        for h in [a, b, c] {
            l.release(h);
        }
        assert_eq!(l.held(), 0.0);
    }

    #[test]
    fn a_release_gives_the_hold_back_and_the_real_spend_stays() {
        let mut l = ledger(1.0);
        let h = l.reserve(0.5).unwrap();
        spend(&mut l, 0.1);
        // Held and spent both count while it runs: refuse rather than overshoot.
        assert!(close(l.available().unwrap(), 0.4));
        l.release(h);
        assert!(close(l.usd(), 0.1));
        assert!(close(l.available().unwrap(), 0.9));
        assert!(l.exhausted().is_none());
    }

    #[test]
    fn with_no_usd_cap_every_hold_fits_and_a_bad_amount_holds_nothing() {
        let mut l = ledger(0.0);
        assert_eq!(l.available(), None);
        let big = l.reserve(1e9).unwrap();
        let nan = l.reserve(f64::NAN).unwrap();
        let negative = l.reserve(-5.0).unwrap();
        assert_eq!((nan.usd(), negative.usd()), (0.0, 0.0));
        for h in [big, nan, negative] {
            l.release(h);
        }
        assert_eq!(l.held(), 0.0);
    }

    #[test]
    fn holds_are_not_saved_and_a_restore_keeps_what_is_held() {
        let mut l = ledger(1.0);
        let h = l.reserve(0.5).unwrap();
        let snapshot = l.snapshot();
        l.restore(&snapshot);
        assert!(close(l.held(), 0.5), "a running spender keeps its hold");
        assert_eq!(ledger(1.0).held(), 0.0);
        l.release(h);
    }
}
