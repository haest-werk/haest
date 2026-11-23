//! How an agent asks the provider to cache its prompt: the policy and the
//! routing key, set on [`AgentOptions`] and changed on [`Agent`], and the
//! plan each request gets from them.

use std::collections::HashSet;

use super::{CachePlan, CachePolicyRef, PlanFix, RequestShape};
use crate::{
    agent::{Agent, AgentOptions},
    provider::{ProviderRequest, ToolSpec},
};

/// The cache policy and the routing key a new agent starts with; None is
/// the default policy, [`CachePlan::default`].
#[derive(Debug, Clone, Default)]
pub(crate) struct CachePrefs {
    pub(crate) policy: Option<CachePolicyRef>,
    pub(crate) routing_key: Option<String>,
}

/// Two options are equal when they hold the same policy (one handle a clone
/// of the other, or both the default) and the same routing key.
impl PartialEq for CachePrefs {
    fn eq(&self, other: &Self) -> bool {
        let same_policy = match (&self.policy, &other.policy) {
            (None, None) => true,
            (Some(a), Some(b)) => CachePolicyRef::ptr_eq(a, b),
            _ => false,
        };
        same_policy && self.routing_key == other.routing_key
    }
}

/// An agent's prompt cache: its policy and routing key, the plan its last
/// loop request sent, when the cache was last used, whether idle ticks are
/// due, and what the human was already told.
#[derive(Debug)]
pub(crate) struct CacheState {
    pub(crate) policy: CachePolicyRef,
    pub(crate) routing_key: Option<String>,
    pub(crate) last_plan: Option<CachePlan>,
    /// When the loop's last answered model call started (ms since the
    /// epoch): where the human's idle time is counted from.
    pub(crate) last_call_ms: Option<u64>,
    /// When the last request that read or wrote the cache started: where
    /// its lifetime is counted from.
    pub(crate) last_refresh_ms: Option<u64>,
    /// Idle ticks are due: set by each answered loop call, cleared when a
    /// tick ends them.
    pub(crate) ticking: bool,
    /// The tool baseline the last answered loop call sent, and the model
    /// spec it went to: a model that takes tool changes keeps it until a
    /// change records it in history ([`Message::ToolChange`](crate::Message::ToolChange)).
    /// None after a call to any other model, and after a compaction, which
    /// starts the baseline over.
    pub(crate) tool_baseline: Option<(String, Vec<ToolSpec>)>,
    told: HashSet<String>,
}

impl CacheState {
    pub(crate) fn new(prefs: CachePrefs) -> Self {
        Self {
            policy: prefs.policy.unwrap_or_else(|| CachePlan::default().into()),
            routing_key: prefs.routing_key,
            last_plan: None,
            last_call_ms: None,
            last_refresh_ms: None,
            ticking: false,
            tool_baseline: None,
            told: HashSet::new(),
        }
    }

    /// A loop call that started at `ms` was answered: the cache was used
    /// then, and the idle ticks start over.
    pub(crate) fn answered(&mut self, ms: u64) {
        self.last_call_ms = Some(ms);
        self.last_refresh_ms = Some(ms);
        self.ticking = true;
    }

    /// Whether `text` is new to this agent: each note is told once.
    pub(crate) fn first_time(&mut self, text: &str) -> bool {
        self.told.insert(text.to_string())
    }
}

impl AgentOptions {
    /// The same, with `policy` deciding where each request's cache
    /// breakpoints go and what an idle conversation does
    /// ([`CachePolicy`](super::CachePolicy)). Without it, the default:
    /// [`CachePlan::default`], the tools, the system prompt and the newest
    /// message, each for five minutes.
    ///
    /// ```
    /// use haest_core::{AgentOptions, cache::{CachePlan, CacheTtl}};
    ///
    /// let hour = CachePlan::standard(CacheTtl::OneHour);
    /// let opts = AgentOptions::new("anthropic:claude-opus-5-5").with_cache_policy(hour);
    /// assert!(opts.cache_policy().is_some());
    /// assert!(AgentOptions::new("anthropic:claude-opus-5-5").cache_policy().is_none());
    /// ```
    #[must_use]
    pub fn with_cache_policy(mut self, policy: impl Into<CachePolicyRef>) -> Self {
        self.cache.policy = Some(policy.into());
        self
    }

    /// The same, with `key` as each model call's routing key
    /// ([`ProviderRequest::routing_key`](crate::ProviderRequest::routing_key)):
    /// OpenRouter keeps requests with the same key on the provider holding
    /// their cache. A host passes something stable for the conversation, such
    /// as its session id. Without it none is sent. It is never a cache key:
    /// what keeps one cache apart from another on OpenAI's own API is the
    /// policy's [`CachePlan::partition_key`].
    ///
    /// ```
    /// use haest_core::AgentOptions;
    ///
    /// let opts = AgentOptions::new("openrouter:anthropic/claude-sonnet-5.5").with_routing_key("session-42");
    /// assert_eq!(opts.routing_key(), Some("session-42"));
    /// ```
    #[must_use]
    pub fn with_routing_key(mut self, key: impl Into<String>) -> Self {
        self.cache.routing_key = Some(key.into());
        self
    }

    /// The cache policy set; None is the default. Becomes
    /// [`Agent::cache_policy`].
    pub fn cache_policy(&self) -> Option<&CachePolicyRef> {
        self.cache.policy.as_ref()
    }

    /// The routing key each model call sends; None sends none. Becomes
    /// [`Agent::routing_key`].
    pub fn routing_key(&self) -> Option<&str> {
        self.cache.routing_key.as_deref()
    }
}

/// The cache policy and routing key between turns, as [`Agent::set_effort`]
/// changes the effort, and the plan the last request sent.
///
/// ```
/// # #[cfg(feature = "testing")]
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// use haest_core::{
///     Agent, AgentDeps, AgentOptions, LoopEvent, SharedDeps, ShutWorkspace, Tools,
///     cache::{CachePlan, CacheTtl},
///     testing::{ScriptedProvider, text_answer},
/// };
///
/// let provider = ScriptedProvider::new([text_answer("hi")]);
/// let shared = SharedDeps::new(provider.resolver(), "/", ShutWorkspace);
/// let deps = AgentDeps::new(shared, Tools::new(), "Be brief.");
/// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m").with_routing_key("s-1"));
///
/// // The human steps away often: an hour from the next model call on.
/// agent.set_cache_policy(CachePlan::standard(CacheTtl::OneHour));
/// agent.run("hello", &mut Vec::<LoopEvent>::new()).await;
/// let sent = &provider.requests()[0];
/// assert_eq!(sent.cache.history, Some(CacheTtl::OneHour));
/// assert_eq!(sent.routing_key.as_deref(), Some("s-1"));
/// assert_eq!(agent.last_cache_plan(), Some(&sent.cache));
/// # }
/// # #[cfg(not(feature = "testing"))]
/// # fn main() {}
/// ```
impl Agent {
    /// The policy that plans each model call's cache breakpoints.
    pub fn cache_policy(&self) -> &CachePolicyRef {
        &self.cache.policy
    }

    /// `policy` from the next model call on. The entries already cached keep
    /// the TTL they were written with.
    pub fn set_cache_policy(&mut self, policy: impl Into<CachePolicyRef>) {
        self.cache.policy = policy.into();
    }

    /// The routing key the next model call sends; None sends none.
    pub fn routing_key(&self) -> Option<&str> {
        self.cache.routing_key.as_deref()
    }

    /// `key` as the routing key from the next model call on.
    pub fn set_routing_key(&mut self, key: impl Into<String>) {
        self.cache.routing_key = Some(key.into());
    }

    /// The cache plan the loop's last model call sent, as checked; None
    /// before the first.
    pub fn last_cache_plan(&self) -> Option<&CachePlan> {
        self.cache.last_plan.as_ref()
    }

    /// `req`'s cache plan: the policy's for its shape, checked against
    /// what the model's cache can do. A fix is told once per agent: to the
    /// human when it [`is_warning`](PlanFix::is_warning), else to the
    /// diagnostic log.
    pub(crate) fn plan_cache(&mut self, req: &mut ProviderRequest) {
        let spec = self.model_spec().to_string();
        let facts = self.deps().shared().facts().cache_facts(&spec);
        let shape = RequestShape::new(&spec, req.sent_tools(), &req.system, &req.messages);
        let shape = match &facts {
            Some(facts) => shape.with_facts(facts),
            None => shape,
        };
        let (plan, fixes) = self.cache.policy.plan(&shape).checked(&shape);
        req.cache = plan;
        for fix in fixes {
            self.tell(&spec, &fix);
        }
    }

    fn tell(&mut self, spec: &str, fix: &PlanFix) {
        self.note(&format!("Prompt cache of {spec}: {fix}."), fix.is_warning());
    }

    /// `text`, once per agent: to the human when `warn`, else to the
    /// diagnostic log.
    pub(crate) fn note(&mut self, text: &str, warn: bool) {
        if self.cache.first_time(text) {
            self.say(text, warn);
        }
    }

    /// `text`, every time: to the human when `warn`, else to the diagnostic
    /// log.
    pub(crate) fn say(&self, text: &str, warn: bool) {
        let shared = self.deps().shared();
        if warn {
            shared.hooks().on_warning(text);
        } else {
            shared.services().debug(text);
        }
    }
}
