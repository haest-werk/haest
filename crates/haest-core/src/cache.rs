//! The provider's prompt cache: where a request's breakpoints go and how
//! long each lives ([`CachePolicy`], [`CachePlan`], [`CacheTtl`]), keeping it
//! warm while a conversation is idle
//! ([`Agent::warm_cache`](crate::Agent::warm_cache)), and whether that pays ([`ping_pays`]).
//!
//! **The policy.** Before every model call the agent's [`CachePolicy`]
//! ([`AgentOptions::with_cache_policy`](crate::AgentOptions::with_cache_policy))
//! plans the request's breakpoints: the tools, the system prompt, the
//! newest message, and messages it chooses, each with its own TTL. haest
//! checks the plan against the provider's rules and the model's
//! [`CacheFacts`](crate::catalog::CacheFacts) before it is sent
//! ([`CachePlan::checked`]), so a policy never makes a request fail. The
//! default places the three standard breakpoints at five minutes.
//!
//! **Choosing a TTL.** A read refreshes an entry for free, so a conversation
//! that sends a request at least every five minutes keeps `5m`, the cheaper
//! write (1.25x the input price on Anthropic, against 2x for `1h`). `1h`
//! pays when the human often pauses for longer than five minutes and less
//! than an hour: one 2x write instead of a 1.25x write after every pause. A
//! system prompt that rarely changes may live an hour while the
//! conversation after it lives five minutes.
//!
//! **Pre-warm.** [`Agent::warm_cache`](crate::Agent::warm_cache) sends the conversation's last request
//! again as a pre-warm ([`ProviderRequest::prewarm`](crate::ProviderRequest::prewarm):
//! `max_tokens: 0` on Anthropic, nothing generated; `max_tokens: 1` for
//! Anthropic's models on OpenRouter, which does not honour 0), which reads the cached
//! prefix and so restarts its lifetime. A host that pings an idle
//! conversation every [`CacheTtl::ping_every`] keeps its cache for the next
//! turn, and [`ping_pays`] says, from the model's prices, how long that chain
//! of reads stays cheaper than letting the entry expire and writing it again.
//! Only a provider whose [`Provider::check_prewarm`](crate::Provider::check_prewarm)
//! says yes takes one.
//!
//! **Idle ticks.** [`Agent::next_cache_tick`](crate::Agent::next_cache_tick)
//! says when an idle conversation's cache is about to expire, and
//! [`Agent::cache_tick`](crate::Agent::cache_tick) asks the policy what to
//! do then ([`CachePolicy::on_idle`]): nothing, a ping, or a compaction
//! while the cache is still warm, which haest runs only when it pays
//! ([`compact_pays`]). haest-session's driver ticks between turns.

mod check;
mod fix;
mod idle;
mod plan;
mod policy;
mod prefs;
mod warm;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_idle;
#[cfg(test)]
mod tests_plan;

pub use fix::{Breakpoint, MAX_BREAKPOINTS, PlanFix, TOOL_PROMPT_ALLOWANCE};
pub use idle::{CacheTick, SUMMARY_TOKENS, compact_pays};
pub use plan::CachePlan;
pub use policy::{CachePolicy, CachePolicyRef, IdleAction, IdleState, RequestShape};
pub(crate) use prefs::{CachePrefs, CacheState};

use std::time::Duration;

use crate::{catalog::ModelPricing, messages::Usage, provider::ModelError};

crate::word_enum! {
    /// How long a provider keeps the prompt cache a breakpoint writes.
    ///
    /// Anthropic's Messages API takes both, and so does OpenRouter for its
    /// `anthropic/` models: a write costs 1.25x the input price for five
    /// minutes and 2x for an hour, and a read refreshes either for free. A
    /// provider with lifetimes of its own (OpenAI's, Gemini's) ignores it.
    /// The module docs ([`cache`](crate::cache)) say when an hour pays.
    ///
    /// ```
    /// use haest_core::cache::CacheTtl;
    ///
    /// assert_eq!(CacheTtl::default(), CacheTtl::FiveMinutes);
    /// assert_eq!("1h".parse::<CacheTtl>(), Ok(CacheTtl::OneHour));
    /// assert_eq!(CacheTtl::OneHour.lifetime().as_secs(), 3_600);
    /// ```
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    #[non_exhaustive]
    pub enum CacheTtl ("cache TTL") {
        /// Five minutes: the provider's default, and what haest sends unless
        /// asked for more.
        #[default]
        FiveMinutes = "5m",
        /// One hour.
        OneHour = "1h",
    }
}

impl CacheTtl {
    /// How long an entry lives after the start of the request that wrote it
    /// or last read it.
    pub const fn lifetime(self) -> Duration {
        match self {
            CacheTtl::FiveMinutes => Duration::from_secs(5 * 60),
            CacheTtl::OneHour => Duration::from_secs(60 * 60),
        }
    }

    /// How often a host pings an idle conversation to keep its entry alive:
    /// the [`lifetime`](Self::lifetime) less a tenth, so a ping that waits in
    /// a queue or is retried still starts before the entry expires. 4.5
    /// minutes for `5m`, 54 for `1h`.
    pub const fn ping_every(self) -> Duration {
        let lifetime = self.lifetime().as_secs();
        Duration::from_secs(lifetime - lifetime / 10)
    }
}

/// Why a provider will not send a request as a cache pre-warm
/// ([`Provider::check_prewarm`](crate::Provider::check_prewarm)). Nothing was
/// sent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PrewarmRefused {
    /// The provider has no pre-warm request haest sends for this model:
    /// only Anthropic's Messages API does today, and OpenRouter for its
    /// `anthropic/` models.
    #[error("{provider} sends no cache pre-warm request")]
    Unsupported {
        /// The provider's id.
        provider: String,
    },
    /// The model thinks on a token budget (`thinking.type: "enabled"`, the
    /// Claude models before adaptive thinking), and a pre-warm cannot carry
    /// it: Anthropic refuses it at `max_tokens: 0`, and OpenRouter drops a
    /// budget its one-token cap cannot hold. One without the thinking writes
    /// an entry the real requests never read.
    #[error("{model} thinks on a token budget, which a pre-warm request cannot carry")]
    BudgetThinking {
        /// The provider's model id.
        model: String,
    },
}

/// What a pre-warm did to the provider's cache, read from its usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WarmCache {
    /// It read the cached prefix and wrote nothing: the entry lives on, its
    /// lifetime restarted.
    Read,
    /// It wrote to the cache: some of the prefix was not cached (it expired,
    /// or the system prompt, the tools or the effort changed), and that
    /// write was paid for. A keeper stops here.
    Wrote,
    /// It neither read nor wrote: the prefix is shorter than the model
    /// caches, or the provider does not say.
    Uncached,
}

/// What [`Agent::warm_cache`](crate::Agent::warm_cache) sent and what it cost. Read its fields; haest
/// may add one in a minor release.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct WarmOutcome {
    /// Whether the pre-warm read the cache or wrote it.
    pub cache: WarmCache,
    /// What it cost: already on the conversation's ledger.
    pub usage: Usage,
}

impl WarmOutcome {
    /// The outcome of a pre-warm that took `usage`: a write if it wrote
    /// anything, else a read if it read anything.
    pub(crate) fn from_usage(usage: Usage) -> Self {
        let cache = if usage.cache_write > 0 {
            WarmCache::Wrote
        } else if usage.cache_read > 0 {
            WarmCache::Read
        } else {
            WarmCache::Uncached
        };
        Self { cache, usage }
    }
}

/// Why [`Agent::warm_cache`](crate::Agent::warm_cache) warmed nothing. History is untouched either way.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WarmError {
    /// History holds no user or tool message, so there is no request to
    /// send again (nothing was sent).
    #[error("there is no request to keep warm: the history holds no user or tool message")]
    NoRequest,
    /// The agent's model spec resolves to no provider (nothing was sent).
    #[error(transparent)]
    Model(#[from] ModelError),
    /// The provider will not send this request as a pre-warm (nothing was
    /// sent): the source says why.
    #[error("cannot pre-warm the cache of {model_spec}")]
    Refused {
        /// The agent's model spec.
        model_spec: String,
        /// Why the provider refused.
        #[source]
        source: PrewarmRefused,
    },
    /// The pre-warm was sent and failed (an API or network error) or was
    /// cancelled.
    #[error("the cache pre-warm failed: {reason}")]
    Failed {
        /// Why: the provider's error, or "cancelled".
        reason: String,
        /// What it cost: already on the conversation's ledger.
        usage: Usage,
    },
}

/// Whether one more keep-warm ping, sent after `idle` of silence, keeps the
/// chain of pings cheaper than letting the cache expire and writing it again
/// on the next turn. False without a price (`pricing` None, or no write
/// price for `ttl`): a keeper never pays for what it cannot weigh.
///
/// A host pings every [`CacheTtl::ping_every`], so the ping at `idle` is the
/// `n`th of the chain (`idle` over that interval, rounded, at least 1). Each
/// ping reads the prefix (`cached_input` per token); a cache left to expire
/// is written once more by the next turn instead of read (`write - cached_input`
/// more per token, `write` the price of the TTL's write). The chain pays while
/// `n * cached_input < write - cached_input`: the prefix's length cancels out.
/// A conversation that never comes back has then cost at most the one
/// rewrite the pings were meant to save.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use haest_core::{cache::{CacheTtl, ping_pays}, catalog::ModelPricing};
///
/// // $2 in, $0.10 a cached read, $2.50 a 5m write, $4 a 1h write, $10 out.
/// let price = ModelPricing::new(2.0, 0.1, 10.0).with_cache_write(2.5).with_cache_write_1h(4.0);
/// let minutes = |m: u64| Duration::from_secs(m * 60);
///
/// // A 5m cache: 23 pings (about 1.7 hours) cost less than one rewrite.
/// assert!(ping_pays(Some(&price), CacheTtl::FiveMinutes, minutes(90)));
/// assert!(!ping_pays(Some(&price), CacheTtl::FiveMinutes, minutes(120)));
/// // A 1h cache pings 12 times as rarely and pays for a day.
/// assert!(ping_pays(Some(&price), CacheTtl::OneHour, minutes(24 * 60)));
/// // No price, no pings.
/// assert!(!ping_pays(None, CacheTtl::FiveMinutes, minutes(5)));
/// ```
pub fn ping_pays(pricing: Option<&ModelPricing>, ttl: CacheTtl, idle: Duration) -> bool {
    let Some(p) = pricing else { return false };
    let Some(write) = write_price(p, ttl) else {
        return false;
    };
    let read = p.cached_input;
    let nth = (idle.as_secs_f64() / ttl.ping_every().as_secs_f64())
        .round()
        .max(1.0);
    nth * read < write - read
}

/// What writing a token to the cache for `ttl` costs: the 5m write price,
/// else the input price, for `5m`; the 1h write price for `1h`. None when
/// the model has no price for it.
fn write_price(p: &ModelPricing, ttl: CacheTtl) -> Option<f64> {
    match ttl {
        CacheTtl::FiveMinutes => Some(p.cache_write.unwrap_or(p.input)),
        CacheTtl::OneHour => p.cache_write_1h,
    }
}
