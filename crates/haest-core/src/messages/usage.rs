//! [`Usage`]: what one model call, or a sum of several, took and cost.

use serde::{Deserialize, Serialize};

/// Token counts and cost of one model call, or a sum of several.
///
/// The four token counts do not overlap: `input` excludes what was read from
/// or written to the prompt cache. [`Usage::context_tokens`] is their sum.
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

impl Usage {
    /// Add `u` to this sum. A cost is added when `u` has one, so the sum's
    /// `cost_usd` stays `None` until some call reported a price.
    pub fn add(&mut self, u: &Usage) {
        self.input += u.input;
        self.output += u.output;
        self.cache_read += u.cache_read;
        self.cache_write += u.cache_write;
        self.reasoning += u.reasoning;
        if let Some(c) = u.cost_usd {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + c);
        }
    }

    /// Tokens the last request actually put in the context window.
    pub fn context_tokens(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.output
    }
}
