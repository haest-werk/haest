//! When a model request is tried again, and how long to wait first: the
//! official SDKs' policy.

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    time::Duration,
};

use haest_http::Response;

/// How a provider retries a request that got no answer or was refused, the way
/// the official SDKs do.
///
/// A try is repeated after no answer (a connection failure, or no status and
/// headers within `headers_timeout`: [`HttpError::is_retryable`]), or a 408,
/// 409, 429 or 5xx answer. The server has the last word: an
/// `x-should-retry: false` header stops the retries, `true` retries any
/// status. The wait is the server's `retry-after-ms` or `retry-after` when it
/// names one under a minute, else `initial_delay` doubled per retry up to
/// `max_delay`, minus up to 25% jitter. Cancelling the request ends the wait.
///
/// Only the request is retried. Once the answer streams, a failure ends the
/// message: a retry would replay text the human has already seen.
///
/// Every provider takes one through `with_retry`, and
/// [`RegistryOptions::retry`](crate::RegistryOptions::retry) sets it for all.
///
/// ```
/// use std::time::Duration;
///
/// use haest_providers::RetryPolicy;
///
/// let patient = RetryPolicy { max_retries: 6, max_delay: Duration::from_secs(30), ..RetryPolicy::default() };
/// assert_eq!(patient.initial_delay, Duration::from_millis(500));
/// assert_eq!(RetryPolicy::none().max_retries, 0);
/// ```
///
/// [`HttpError::is_retryable`]: haest_http::HttpError::is_retryable
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// Tries after the first one; 0 never retries.
    pub max_retries: u32,
    /// The first backoff; doubles per retry.
    pub initial_delay: Duration,
    /// The longest backoff. A server's own `retry-after` may ask for longer, up to a minute.
    pub max_delay: Duration,
    /// How long one try waits for the status and headers before it counts as
    /// unanswered; the body then takes as long as it takes. A long prompt can
    /// sit in a queue before the first byte.
    pub headers_timeout: Duration,
}

impl Default for RetryPolicy {
    /// The SDKs' policy: 3 retries, 0.5 s doubling to at most 8 s, and ten
    /// minutes for each try's status, as long as the SDKs allow.
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
            headers_timeout: Duration::from_secs(600),
        }
    }
}

impl RetryPolicy {
    /// Never retry: the first refusal or connection failure is the answer.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Backoff before retry number `retry` (0-based): doubling, capped, minus up to 25% jitter.
    pub(crate) fn backoff(&self, retry: u32) -> Duration {
        let doubled = self.initial_delay.saturating_mul(1u32 << retry.min(16));
        let capped = doubled.min(self.max_delay);
        capped.mul_f64(1.0 - random_unit() * 0.25)
    }
}

/// Whether a refusal is worth another try: the server's `x-should-retry`,
/// else 408, 409, 429 or 5xx.
pub(crate) fn should_retry(resp: &Response) -> bool {
    match resp.header("x-should-retry") {
        Some("true") => true,
        Some("false") => false,
        _ => matches!(resp.status, 408 | 409 | 429) || resp.status >= 500,
    }
}

/// The server's own wait, when it names one under a minute.
pub(crate) fn retry_after(resp: &Response) -> Option<Duration> {
    let ms = resp
        .header("retry-after-ms")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .or_else(|| {
            resp.header("retry-after")
                .and_then(|v| v.trim().parse::<f64>().ok())
                .map(|s| s * 1000.0)
        })?;
    (0.0..60_000.0)
        .contains(&ms)
        .then(|| Duration::from_secs_f64(ms / 1000.0))
}

/// A uniform-ish number in [0, 1) without a random-number dependency.
fn random_unit() -> f64 {
    let mut h = RandomState::new().build_hasher();
    h.write_u64(0);
    (h.finish() >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_the_cap_with_at_most_a_quarter_off() {
        let p = RetryPolicy::default();
        for (retry, full) in [(0, 500), (1, 1000), (2, 2000), (5, 8000), (30, 8000)] {
            let d = p.backoff(retry).as_millis() as u64;
            assert!(d <= full && d >= full * 3 / 4, "retry {retry}: {d}ms");
        }
    }
}
