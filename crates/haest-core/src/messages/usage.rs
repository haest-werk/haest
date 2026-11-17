//! [`Usage`]: what one model call, or a sum of several, took and cost.

use std::{iter::Sum, ops::AddAssign};

use serde::{Deserialize, Serialize};

/// Token counts and cost of one model call, or a sum of several.
///
/// The four token counts do not overlap: `input` excludes what was read from
/// or written to the prompt cache. [`Usage::context_tokens`] is their sum.
///
/// Usages add up with `+=` and [`Sum`]; a count stops at `u64::MAX`. A cost
/// is added when the usage added has one, so a sum's `cost_usd` stays `None`
/// until some call reported a price. A cost that is negative or not a finite
/// number is no price: it is never added.
///
/// ```
/// use haest_core::Usage;
///
/// let calls = [
///     Usage { input: 100, output: 20, ..Usage::default() },
///     Usage { input: 50, output: 10, cost_usd: Some(0.01), ..Usage::default() },
/// ];
/// let mut total = Usage::default();
/// total += &calls[0];
/// assert_eq!(total.cost_usd, None);
/// total += &calls[1];
/// assert_eq!(calls.iter().sum::<Usage>(), total);
/// assert_eq!((total.input, total.output, total.cost_usd), (150, 30, Some(0.01)));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Input tokens billed at the full rate: neither read from nor written to the cache.
    pub input: u64,
    /// Tokens the model generated, reasoning included.
    pub output: u64,
    /// Input tokens served from the provider's prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the provider's prompt cache.
    pub cache_write: u64,
    /// Of `output`, the tokens spent reasoning, when the provider reports them.
    pub reasoning: u64,
    /// When the provider reports it (OpenRouter does), or from a configured price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl AddAssign<&Usage> for Usage {
    fn add_assign(&mut self, u: &Usage) {
        self.input = self.input.saturating_add(u.input);
        self.output = self.output.saturating_add(u.output);
        self.cache_read = self.cache_read.saturating_add(u.cache_read);
        self.cache_write = self.cache_write.saturating_add(u.cache_write);
        self.reasoning = self.reasoning.saturating_add(u.reasoning);
        if let Some(c) = real_cost(u.cost_usd) {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + c);
        }
    }
}

/// `cost` when it is an amount haest counts: finite, and not below 0. Any
/// other (a provider's or a tool's bad bill: a NaN, a refund) is no price,
/// so it can neither lower a spend nor switch a USD cap off (`NaN >= cap` is
/// false). Every sum of a cost (a `Usage`'s, a tool's, the ledger's) goes
/// through here.
pub(crate) fn real_cost(cost: Option<f64>) -> Option<f64> {
    cost.filter(|c| c.is_finite() && *c >= 0.0)
}

impl AddAssign for Usage {
    fn add_assign(&mut self, u: Usage) {
        *self += &u;
    }
}

impl<'a> Sum<&'a Usage> for Usage {
    fn sum<I: Iterator<Item = &'a Usage>>(iter: I) -> Self {
        iter.fold(Usage::default(), |mut total, u| {
            total += u;
            total
        })
    }
}

impl Sum for Usage {
    fn sum<I: Iterator<Item = Usage>>(iter: I) -> Self {
        iter.fold(Usage::default(), |mut total, u| {
            total += &u;
            total
        })
    }
}

impl Usage {
    /// Tokens the last request actually put in the context window; at most
    /// `u64::MAX`.
    pub fn context_tokens(&self) -> u64 {
        (self.input)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
            .saturating_add(self.output)
    }
}
