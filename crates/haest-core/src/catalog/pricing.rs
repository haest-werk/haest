//! What a model costs, for a provider whose API reports no cost.

use serde::{Deserialize, Serialize};

/// A model's price in USD per 1M tokens, for a provider whose API returns no
/// cost of its own ([`book_priced_cost`](crate::provider::book_priced_cost)).
///
/// Built with [`new`](Self::new), [`with_cache_write`](Self::with_cache_write)
/// and [`with_cache_write_1h`](Self::with_cache_write_1h), so a price it gains
/// later (for reasoning, for a search) breaks no host; its fields read as they
/// are. In a file (the catalog, providers.json) it is `{input, cachedInput,
/// output, cacheWrite?, cacheWrite1h?}`.
///
/// ```
/// use haest_core::catalog::ModelPricing;
///
/// // $3 in, $0.30 from the cache, $15 out; a cache write costs $3.75 for
/// // five minutes and $6 for an hour.
/// let price = ModelPricing::new(3.0, 0.3, 15.0).with_cache_write(3.75).with_cache_write_1h(6.0);
/// assert_eq!((price.cache_write, price.cache_write_1h), (Some(3.75), Some(6.0)));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ModelPricing {
    /// Uncached input tokens.
    pub input: f64,
    /// Input tokens read from the provider's prompt cache.
    pub cached_input: f64,
    /// Output tokens.
    pub output: f64,
    /// Input tokens written to the prompt cache for the provider's default
    /// time (Anthropic's five minutes, OpenAI's 30); absent = billed as
    /// `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    /// Input tokens written to the prompt cache to live an hour (Anthropic's
    /// `ttl: "1h"`, [`Usage::cache_write_1h`](crate::Usage::cache_write_1h));
    /// absent = billed as `cache_write`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<f64>,
}

impl ModelPricing {
    /// `input`, `cached_input` and `output` USD per 1M tokens, in that
    /// order; a cache write is billed as input.
    pub const fn new(input: f64, cached_input: f64, output: f64) -> Self {
        Self {
            input,
            cached_input,
            output,
            cache_write: None,
            cache_write_1h: None,
        }
    }

    /// Cache writes at `usd` per 1M tokens, not as input.
    #[must_use]
    pub const fn with_cache_write(mut self, usd: f64) -> Self {
        self.cache_write = Some(usd);
        self
    }

    /// Cache writes that live an hour at `usd` per 1M tokens, not as the
    /// default write.
    #[must_use]
    pub const fn with_cache_write_1h(mut self, usd: f64) -> Self {
        self.cache_write_1h = Some(usd);
        self
    }
}
