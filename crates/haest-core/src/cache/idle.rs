//! An idle conversation's cache: when the next tick is due
//! ([`Agent::next_cache_tick`]), what one does ([`Agent::cache_tick`]), and
//! whether compacting before the cache expires pays ([`compact_pays`]).

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::{CacheTtl, IdleAction, IdleState, WarmCache, WarmError, WarmOutcome};
use crate::{
    agent::Agent,
    caps::estimate_request_tokens,
    catalog::ModelPricing,
    compaction::{
        CompactionError, CompactionOptions, CompactionResult, compact, cut_index,
        last_context_tokens,
    },
};

/// The summary [`compact_pays`] weighs when the host has no better guess:
/// what a compaction's summary is about as long as.
pub const SUMMARY_TOKENS: u64 = 4_096;

/// Whether compacting an idle conversation while its cache is still warm
/// costs less than letting the cache expire and writing the whole prefix
/// again on the next turn. False without a price (`pricing` None, or no
/// write price for `ttl`).
///
/// Compacting costs the summary call, which reads the `prefix` from the
/// cache (`cached_input` a token) and writes `summary` tokens (`output`),
/// then the next turn's write of the smaller prefix, `next_prefix` tokens
/// (the kept messages, the system prompt and the tools, and the summary).
/// Not compacting costs the next turn's write of the whole `prefix`. A write
/// is the TTL's price ([`ModelPricing::cache_write`], else the input price,
/// for `5m`; [`ModelPricing::cache_write_1h`] for `1h`).
///
/// ```
/// use haest_core::{cache::{CacheTtl, compact_pays}, catalog::ModelPricing};
///
/// // $3 in, $0.30 a read, $15 out, $3.75 a 5m write.
/// let price = ModelPricing::new(3.0, 0.3, 15.0).with_cache_write(3.75);
/// let five = CacheTtl::FiveMinutes;
/// // 150k tokens down to 20k, a 4k summary: 150k reads and 4k out (about
/// // $0.11), then a 20k write ($0.075), against a 150k write ($0.56).
/// assert!(compact_pays(Some(&price), five, 150_000, 20_000, 4_096));
/// // 30k down to 25k: the summary costs more than the rewrite saves.
/// assert!(!compact_pays(Some(&price), five, 30_000, 25_000, 4_096));
/// assert!(!compact_pays(None, five, 150_000, 20_000, 4_096));
/// ```
pub fn compact_pays(
    pricing: Option<&ModelPricing>,
    ttl: CacheTtl,
    prefix: u64,
    next_prefix: u64,
    summary: u64,
) -> bool {
    let Some(p) = pricing else { return false };
    let Some(write) = super::write_price(p, ttl) else {
        return false;
    };
    let (prefix, next, summary) = (prefix as f64, next_prefix as f64, summary as f64);
    prefix * p.cached_input + summary * p.output + next * write < prefix * write
}

impl IdleState<'_> {
    /// Whether compacting this conversation with `options` now pays
    /// ([`compact_pays`]): what it would cut
    /// ([`CompactionOptions::keep_recent`] decides) is estimated as the loop
    /// estimates a request, and the summary weighed is [`SUMMARY_TOKENS`].
    /// False when there is nothing to cut or no price. [`Agent::cache_tick`]
    /// asks the same before it runs a [`IdleAction::Compact`]; a policy asks
    /// first to ping instead when it does not pay.
    pub fn compaction_pays(&self, options: &CompactionOptions) -> bool {
        let cut = cut_index(self.messages, options.keep_recent);
        if cut == 0 {
            return false;
        }
        let head = estimate_request_tokens("", &self.messages[..cut], &[]);
        let next = (self.prefix_tokens.saturating_sub(head)).saturating_add(SUMMARY_TOKENS);
        compact_pays(
            self.pricing,
            self.ttl,
            self.prefix_tokens,
            next,
            SUMMARY_TOKENS,
        )
    }
}

/// What one [`Agent::cache_tick`] did. Only a ping that read the cache
/// leaves it warm and the ticks going ([`keeps_going`](Self::keeps_going));
/// after any other the ticks end until the next turn.
#[derive(Debug)]
#[non_exhaustive]
pub enum CacheTick {
    /// Nothing: the policy asked for nothing, or haest's own checks said no
    /// (the cache already expired, a compaction that would not pay or had
    /// nothing to cut).
    Nothing,
    /// A pre-warm went out ([`Agent::warm_cache`]); one that wrote instead
    /// of read was told to the human.
    Pinged(WarmOutcome),
    /// A pre-warm failed or was refused; the human was told.
    PingFailed(WarmError),
    /// The conversation was compacted; None when there was nothing to cut.
    Compacted(Option<CompactionResult>),
    /// A compaction failed; the human was told, and history is as it was.
    CompactFailed(CompactionError),
}

impl CacheTick {
    /// Whether another tick is due: after a ping that read the cache.
    pub fn keeps_going(&self) -> bool {
        matches!(self, CacheTick::Pinged(o) if o.cache == WarmCache::Read)
    }
}

/// `ms` as a duration, a clock that went back as none.
fn since(now: u64, then: u64) -> Duration {
    Duration::from_millis(now.saturating_sub(then))
}

impl Agent {
    /// When the next idle tick is due, in the agent's clock's milliseconds
    /// ([`Clock::now_ms`](crate::deps::Clock::now_ms)): a tenth of the
    /// cache's lifetime before it expires ([`CacheTtl::ping_every`] after
    /// the last request that read or wrote it), the lifetime being the TTL
    /// of the last breakpoint the last loop request sent
    /// ([`CachePlan::last_ttl`](super::CachePlan::last_ttl)). None when no
    /// tick is due: before the first answered model call, after a plan with
    /// no breakpoint, and after a tick that ended them until the next turn.
    ///
    /// A host that keeps an idle conversation's cache sleeps until then and
    /// calls [`cache_tick`](Self::cache_tick); haest-session's driver does.
    pub fn next_cache_tick(&self) -> Option<u64> {
        let state = &self.cache;
        if !state.ticking || self.history().is_empty() {
            return None;
        }
        let ttl = state.last_plan.as_ref()?.last_ttl()?;
        let every = u64::try_from(ttl.ping_every().as_millis()).unwrap_or(u64::MAX);
        Some(state.last_refresh_ms?.saturating_add(every))
    }

    /// One idle tick: asks the policy what to do
    /// ([`CachePolicy::on_idle`](super::CachePolicy::on_idle)) and does it
    /// if haest's own checks pass. A ping goes out only while the cache is
    /// still warm; a compaction only while it is warm and when it pays
    /// ([`compact_pays`], with a summary of [`SUMMARY_TOKENS`]). A ping that
    /// wrote, a failed ping or compaction, and a refused pre-warm (once per
    /// agent) are told to the human through
    /// [`AgentHooks::on_warning`](crate::AgentHooks::on_warning).
    ///
    /// Call it at [`next_cache_tick`](Self::next_cache_tick), between turns.
    pub async fn cache_tick(&mut self, cancel: &CancellationToken) -> CacheTick {
        let tick = self.tick(cancel).await;
        self.cache.ticking = tick.keeps_going();
        tick
    }

    async fn tick(&mut self, cancel: &CancellationToken) -> CacheTick {
        let shared = self.deps().shared().clone();
        let now = shared.clock().now_ms();
        let spec = self.model_spec().to_string();
        let (Some(ttl), Some(call), Some(refresh)) = (
            self.cache.last_plan.as_ref().and_then(|p| p.last_ttl()),
            self.cache.last_call_ms,
            self.cache.last_refresh_ms,
        ) else {
            return CacheTick::Nothing;
        };
        if since(now, refresh) >= ttl.lifetime() {
            return CacheTick::Nothing;
        }
        let pricing = shared.facts().pricing(&spec);
        let facts = shared.facts().cache_facts(&spec);
        let window = (shared.turn_caps().turn_caps(&spec))
            .map(|c| c.context_window)
            .filter(|w| *w > 0);
        let prefix = last_context_tokens(self.history());
        let mut idle =
            IdleState::new(&spec, since(now, call), ttl, prefix).with_messages(self.history());
        idle.context_window = window;
        idle.pricing = pricing.as_ref();
        idle.facts = facts.as_ref();
        match self.cache.policy.on_idle(&idle) {
            IdleAction::Ping => self.ping(&spec, cancel).await,
            IdleAction::Compact(options) if idle.compaction_pays(&options) => {
                self.compact_idle(&options, cancel).await
            }
            IdleAction::Compact(_) => {
                let text = format!(
                    "No compaction before {spec}'s prompt cache expires: nothing to cut, or it would not cost less than writing the cache again."
                );
                self.note(&text, false);
                CacheTick::Nothing
            }
            _ => CacheTick::Nothing,
        }
    }

    async fn ping(&mut self, spec: &str, cancel: &CancellationToken) -> CacheTick {
        match self.warm_cache(cancel).await {
            Ok(outcome) => {
                if outcome.cache == WarmCache::Wrote {
                    let text = format!(
                        "Keep-warm stopped: a ping of {spec}'s prompt cache wrote {} tokens instead of reading them (the cache had expired, or the prompt changed).",
                        outcome.usage.cache_write
                    );
                    self.say(&text, true);
                }
                CacheTick::Pinged(outcome)
            }
            Err(e) => {
                match &e {
                    // The same refusal comes back every turn: told once.
                    WarmError::Refused { source, .. } => {
                        self.note(&format!("Keep-warm is off for {spec}: {source}."), true);
                    }
                    // A host that stopped it knows; nothing to tell.
                    _ if cancel.is_cancelled() => {}
                    other => {
                        let report = crate::error::report(other);
                        self.say(&format!("Keep-warm stopped: {report}."), true);
                    }
                }
                CacheTick::PingFailed(e)
            }
        }
    }

    async fn compact_idle(
        &mut self,
        options: &CompactionOptions,
        cancel: &CancellationToken,
    ) -> CacheTick {
        match compact(self, cancel, options).await {
            Ok(done) => CacheTick::Compacted(done),
            Err(e) => {
                // A host that stopped it knows; nothing to tell.
                if !cancel.is_cancelled() {
                    let text = format!(
                        "Compaction before the prompt cache expired failed: {}.",
                        crate::error::report(&e)
                    );
                    self.say(&text, true);
                }
                CacheTick::CompactFailed(e)
            }
        }
    }
}
