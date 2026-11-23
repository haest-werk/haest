//! [`CachePolicy`]: the plug point that decides, per request, where the
//! prompt-cache breakpoints go, and, while a conversation is idle, whether
//! to keep its cache warm or compact before it expires.

use std::time::Duration;

use super::{CachePlan, CacheTtl};
use crate::{
    catalog::{CacheFacts, ModelPricing},
    compaction::CompactionOptions,
    messages::Message,
    provider::ToolSpec,
};

/// PLUG POINT: how a conversation uses the provider's prompt cache, set with
/// [`AgentOptions::with_cache_policy`](crate::AgentOptions::with_cache_policy).
///
/// [`plan`](Self::plan) is asked before every model call (the loop's,
/// compaction's summary, a pre-warm) for that request's breakpoints;
/// [`on_idle`](Self::on_idle) at each idle tick, when the cache of an idle
/// conversation is about to expire. Choices live here, the provider's hard
/// rules in haest: every plan is checked before it is sent
/// ([`CachePlan::checked`]), and an idle answer runs only when haest's own
/// checks pass. So no policy can make a request fail.
///
/// A [`CachePlan`] is itself a policy: the same plan for every request, and
/// nothing at an idle tick. The default is [`CachePlan::default`].
///
/// ```
/// use haest_core::{
///     AgentOptions,
///     cache::{CachePlan, CachePolicy, CacheTtl, IdleAction, IdleState, RequestShape},
/// };
///
/// /// The system prompt, which rarely changes, for an hour; the conversation
/// /// for five minutes. Kept warm while a ping chain pays.
/// struct LongSystem;
///
/// impl CachePolicy for LongSystem {
///     fn plan(&self, _request: &RequestShape<'_>) -> CachePlan {
///         CachePlan::none()
///             .with_system(CacheTtl::OneHour)
///             .with_history(CacheTtl::FiveMinutes)
///     }
///
///     fn on_idle(&self, idle: &IdleState<'_>) -> IdleAction {
///         if haest_core::cache::ping_pays(idle.pricing, idle.ttl, idle.idle) {
///             IdleAction::Ping
///         } else {
///             IdleAction::Nothing
///         }
///     }
/// }
///
/// let opts = AgentOptions::new("anthropic:claude-opus-5-5").with_cache_policy(LongSystem);
/// # let _ = opts;
/// ```
pub trait CachePolicy: Send + Sync {
    /// The breakpoints of the request `request` describes. haest fixes what
    /// breaks a provider rule before it is sent, and says so
    /// ([`CachePlan::checked`]).
    fn plan(&self, request: &RequestShape<'_>) -> CachePlan;

    /// What to do at an idle tick of a conversation whose cache is about to
    /// expire. [`IdleAction::Nothing`] ends the ticks until the next turn.
    /// Default: nothing.
    fn on_idle(&self, _idle: &IdleState<'_>) -> IdleAction {
        IdleAction::Nothing
    }
}

crate::plug_ref! {
    /// A [`CachePolicy`], shared: what an agent's cache policy is held in.
    pub struct CachePolicyRef(dyn CachePolicy);
}

/// The same plan for every request, and nothing at an idle tick.
impl CachePolicy for CachePlan {
    fn plan(&self, _request: &RequestShape<'_>) -> CachePlan {
        self.clone()
    }
}

/// One request as a [`CachePolicy`] sees it: what it sends, in the order the
/// provider caches it, and what the model's cache can do.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct RequestShape<'a> {
    /// The conversation's model spec, `provider:model`.
    pub model_spec: &'a str,
    /// The tools offered, first in the cached prefix.
    pub tools: &'a [ToolSpec],
    /// The system prompt, after the tools.
    pub system: &'a str,
    /// The messages, oldest first; a plan's message indices point here.
    pub messages: &'a [Message],
    /// What the model's cache can do ([`ModelFacts::cache_facts`](crate::catalog::ModelFacts::cache_facts));
    /// None = unknown.
    pub facts: Option<&'a CacheFacts>,
}

impl<'a> RequestShape<'a> {
    /// A request to `model_spec` of `tools`, `system` and `messages`, its
    /// cache facts unknown.
    pub fn new(
        model_spec: &'a str,
        tools: &'a [ToolSpec],
        system: &'a str,
        messages: &'a [Message],
    ) -> Self {
        Self {
            model_spec,
            tools,
            system,
            messages,
            facts: None,
        }
    }

    /// With what the model's cache can do.
    #[must_use]
    pub fn with_facts(mut self, facts: &'a CacheFacts) -> Self {
        self.facts = Some(facts);
        self
    }
}

/// An idle conversation as a [`CachePolicy`] sees it at a tick, a tenth of
/// its cache's lifetime before that expires.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct IdleState<'a> {
    /// The conversation's model spec.
    pub model_spec: &'a str,
    /// How long since its last turn's last model call started: how long the
    /// human has been away.
    pub idle: Duration,
    /// How long its cache lives after each read: the TTL of the last
    /// breakpoint the last request sent ([`CachePlan::last_ttl`]).
    pub ttl: CacheTtl,
    /// The tokens the next turn would write again if the cache expired: the
    /// last request's prompt and its answer.
    pub prefix_tokens: u64,
    /// The model's context window, when known.
    pub context_window: Option<u64>,
    /// The model's price, when known: without it nothing that weighs a cost
    /// pays ([`ping_pays`](super::ping_pays)).
    pub pricing: Option<&'a ModelPricing>,
    /// What the model's cache can do, when known.
    pub facts: Option<&'a CacheFacts>,
}

/// What a [`CachePolicy`] asks for at an idle tick.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum IdleAction {
    /// Nothing: the ticks end until the next turn.
    Nothing,
    /// Keep the cache warm: the last request again as a pre-warm
    /// ([`Agent::warm_cache`](crate::Agent::warm_cache)). haest sends it only
    /// while the cache is still warm; a ping that wrote ends the ticks.
    Ping,
    /// Compact with these options while the cache is still warm, so the
    /// next turn writes a small prefix instead of the whole one
    /// ([`compact`](crate::compaction::compact)). haest compacts only when
    /// the cache is warm and that costs less than writing the prefix again;
    /// the ticks end either way.
    Compact(CompactionOptions),
}
