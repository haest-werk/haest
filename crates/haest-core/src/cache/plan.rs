//! [`CachePlan`]: where one request's prompt-cache breakpoints go, and how
//! long each lives.

use super::CacheTtl;

/// Where one request's prompt-cache breakpoints go, and how long each lives:
/// what a [`CachePolicy`](super::CachePolicy) answers for every request, and
/// what [`ProviderRequest::cache`](crate::ProviderRequest::cache) carries to
/// the provider.
///
/// A breakpoint caches the request up to and including its block. The
/// provider reads a prefix in one order: the tools, the system prompt, then
/// the messages, oldest first. Four places take one:
///
/// - [`tools`](Self::tools): the last tool definition;
/// - [`system`](Self::system): the system prompt;
/// - [`messages`](Self::messages): a message the policy chose, by its index
///   in the request's messages (an anchor that stays while the tail moves);
/// - [`history`](Self::history): the newest message, the rolling breakpoint
///   the next request reads everything before from.
///
/// None of a place leaves it unmarked. haest checks every plan before it is
/// sent ([`checked`](Self::checked)): at most four breakpoints, a longer TTL
/// before a shorter one, none where the model would not cache. Each provider
/// sends what it takes: Anthropic every breakpoint with its TTL; OpenRouter
/// the same for its `anthropic/` models, and the system prompt and the newest
/// message, at five minutes, for its `google/` ones; OpenAI none, since it
/// caches by itself.
///
/// [`partition_key`](Self::partition_key) is the one part that is no
/// breakpoint: a key OpenAI's own API partitions its cache by.
///
/// ```
/// use haest_core::cache::{CachePlan, CacheTtl};
///
/// // What haest sends unless a policy says otherwise: every place at five minutes.
/// assert_eq!(CachePlan::default(), CachePlan::standard(CacheTtl::FiveMinutes));
///
/// // A system prompt kept for an hour, the conversation for five minutes.
/// let plan = CachePlan::none()
///     .with_system(CacheTtl::OneHour)
///     .with_history(CacheTtl::FiveMinutes);
/// assert_eq!(plan.breakpoints(), 2);
/// assert_eq!(plan.last_ttl(), Some(CacheTtl::FiveMinutes));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CachePlan {
    /// The system prompt's breakpoint, or none.
    pub system: Option<CacheTtl>,
    /// The last tool definition's breakpoint, or none.
    pub tools: Option<CacheTtl>,
    /// The newest message's breakpoint (the rolling one), or none.
    pub history: Option<CacheTtl>,
    /// Breakpoints on messages the policy chose, by index into the
    /// request's messages, in any order.
    pub messages: Vec<(usize, CacheTtl)>,
    /// A key that keeps this request's cache apart from requests under any
    /// other: OpenAI's own API takes it as `prompt_cache_key` (on GPT-5.6
    /// and later it partitions the cache), no other provider has one. None
    /// (the default) sends none, so conversations that start alike share
    /// what one of them cached. Set it only to keep apart what must not
    /// share a cache (one customer's conversations from another's).
    pub partition_key: Option<String>,
}

impl CachePlan {
    /// No breakpoint and no partition key: the provider caches nothing it
    /// is not told to (OpenAI's own caching still runs).
    pub const fn none() -> Self {
        Self {
            system: None,
            tools: None,
            history: None,
            messages: Vec::new(),
            partition_key: None,
        }
    }

    /// The tools, the system prompt and the newest message, each living
    /// `ttl`: what haest has always sent. [`Default`] is this at five minutes.
    pub const fn standard(ttl: CacheTtl) -> Self {
        Self {
            system: Some(ttl),
            tools: Some(ttl),
            history: Some(ttl),
            messages: Vec::new(),
            partition_key: None,
        }
    }

    /// The system prompt cached for `ttl`.
    #[must_use]
    pub fn with_system(mut self, ttl: CacheTtl) -> Self {
        self.system = Some(ttl);
        self
    }

    /// The tool definitions cached for `ttl`.
    #[must_use]
    pub fn with_tools(mut self, ttl: CacheTtl) -> Self {
        self.tools = Some(ttl);
        self
    }

    /// The newest message cached for `ttl`.
    #[must_use]
    pub fn with_history(mut self, ttl: CacheTtl) -> Self {
        self.history = Some(ttl);
        self
    }

    /// The message at `index` (into the request's messages) cached for `ttl`.
    #[must_use]
    pub fn with_message(mut self, index: usize, ttl: CacheTtl) -> Self {
        self.messages.push((index, ttl));
        self
    }

    /// The cache kept apart under `key` ([`partition_key`](Self::partition_key)).
    #[must_use]
    pub fn with_partition_key(mut self, key: impl Into<String>) -> Self {
        self.partition_key = Some(key.into());
        self
    }

    /// How many breakpoints it places.
    pub fn breakpoints(&self) -> usize {
        [self.tools, self.system, self.history]
            .iter()
            .flatten()
            .count()
            + self.messages.len()
    }

    /// The TTL of its last breakpoint in the provider's order (the newest
    /// message's, else the latest chosen message's, else the system
    /// prompt's, else the tools'): how long the whole prefix it caches
    /// lives after a read. None without a breakpoint.
    pub fn last_ttl(&self) -> Option<CacheTtl> {
        let chosen = self.messages.iter().max_by_key(|(i, _)| *i).map(|m| m.1);
        self.history.or(chosen).or(self.system).or(self.tools)
    }
}

impl Default for CachePlan {
    /// [`CachePlan::standard`] at five minutes.
    fn default() -> Self {
        Self::standard(CacheTtl::FiveMinutes)
    }
}

/// [`CachePlan::standard`] at that TTL.
impl From<CacheTtl> for CachePlan {
    fn from(ttl: CacheTtl) -> Self {
        Self::standard(ttl)
    }
}
