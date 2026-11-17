//! Booking a cost for a provider whose API returns none (every direct
//! OpenAI-compatible server; OpenRouter alone sends `usage.cost`). Pure.
//!
//! Only from a configured price: a model with none stays unpriced, because an
//! invented number in the ledger is worse than a missing one.
//!
//! A helper for provider adapters (`haest-providers` calls it once per model
//! call); the ledger ([`crate::ledger`]) then charges the cost the message
//! carries, when it is a real amount.

use crate::{
    catalog::ModelPricing,
    messages::{Usage, real_cost},
};

const PER: f64 = 1_000_000.0;

/// Sets `usage.cost_usd` from `pricing` when the provider gave no cost and
/// tokens were billed. A cost already there wins, unless it is negative or
/// not a finite number, which is no price; no `pricing` changes nothing.
///
/// Prices are USD per 1M tokens ([`ModelPricing`]); a cache write with no
/// price of its own is billed as input.
///
/// # Examples
///
/// ```
/// use haest_core::{Usage, catalog::ModelPricing, provider::book_priced_cost};
///
/// let price = ModelPricing::new(2.0, 0.2, 8.0);
/// let mut usage = Usage { input: 500_000, output: 100_000, ..Default::default() };
/// book_priced_cost(&mut usage, Some(&price));
/// assert_eq!(usage.cost_usd, Some(1.8)); // 0.5 * $2 + 0.1 * $8
/// ```
pub fn book_priced_cost(usage: &mut Usage, pricing: Option<&ModelPricing>) {
    let Some(p) = pricing else { return };
    if real_cost(usage.cost_usd).is_some() || usage.context_tokens() == 0 {
        return;
    }
    let cost = usage.input as f64 * p.input
        + usage.cache_read as f64 * p.cached_input
        + usage.cache_write as f64 * p.cache_write.unwrap_or(p.input)
        + usage.output as f64 * p.output;
    usage.cost_usd = Some(cost / PER);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_priced_model_books_its_cost_and_a_reported_cost_wins() {
        let p = ModelPricing::new(1.0, 0.1, 2.0);
        let mut u = Usage {
            input: 1_000_000,
            cache_write: 1_000_000,
            output: 500_000,
            ..Default::default()
        };
        book_priced_cost(&mut u, Some(&p));
        assert_eq!(u.cost_usd, Some(3.0));
        let mut reported = Usage {
            input: 10,
            cost_usd: Some(0.5),
            ..Default::default()
        };
        book_priced_cost(&mut reported, Some(&p));
        assert_eq!(reported.cost_usd, Some(0.5));
        let mut unpriced = Usage {
            input: 10,
            ..Default::default()
        };
        book_priced_cost(&mut unpriced, None);
        assert_eq!(unpriced.cost_usd, None);
    }

    /// A server's `usage.cost` below 0 is no price: it kept the configured
    /// one out, and the call went unpriced.
    #[test]
    fn a_reported_cost_that_is_no_real_amount_gives_way_to_the_configured_price() {
        let p = ModelPricing::new(1.0, 0.1, 2.0);
        for bad in [-50.0, f64::NAN, f64::INFINITY] {
            let mut u = Usage {
                input: 1_000_000,
                cost_usd: Some(bad),
                ..Default::default()
            };
            book_priced_cost(&mut u, Some(&p));
            assert_eq!(u.cost_usd, Some(1.0), "a reported {bad}");
        }
    }
}
