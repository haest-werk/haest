//! How an agent asks the provider to cache its prompt: the TTL and the key,
//! set on [`AgentOptions`] and changed on [`Agent`].

use super::CacheTtl;
use crate::agent::{Agent, AgentOptions};

/// The cache TTL and the cache key every model call of one agent sends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CachePrefs {
    pub(crate) ttl: CacheTtl,
    pub(crate) key: Option<String>,
}

impl AgentOptions {
    /// The same, asking the provider to keep what each model call caches for
    /// `ttl` ([`ProviderRequest::cache_ttl`](crate::ProviderRequest::cache_ttl)).
    /// Without it, the default: five minutes.
    ///
    /// ```
    /// use haest_core::{AgentOptions, cache::CacheTtl};
    ///
    /// let opts = AgentOptions::new("anthropic:claude-opus-5-5").with_cache_ttl(CacheTtl::OneHour);
    /// assert_eq!(opts.cache_ttl(), CacheTtl::OneHour);
    /// assert_eq!(AgentOptions::new("anthropic:claude-opus-5-5").cache_ttl(), CacheTtl::FiveMinutes);
    /// ```
    #[must_use]
    pub fn with_cache_ttl(mut self, ttl: CacheTtl) -> Self {
        self.cache.ttl = ttl;
        self
    }

    /// The same, sending `key` as the provider's prompt-cache routing key
    /// ([`ProviderRequest::cache_key`](crate::ProviderRequest::cache_key)):
    /// requests with the same key land where their prefix is cached. A host
    /// passes something stable for the conversation, such as its session id.
    /// Without it none is sent.
    ///
    /// ```
    /// use haest_core::AgentOptions;
    ///
    /// let opts = AgentOptions::new("openai:gpt-6.1-sol").with_cache_key("session-42");
    /// assert_eq!(opts.cache_key(), Some("session-42"));
    /// ```
    #[must_use]
    pub fn with_cache_key(mut self, key: impl Into<String>) -> Self {
        self.cache.key = Some(key.into());
        self
    }

    /// How long the provider keeps what each model call caches. Becomes
    /// [`Agent::cache_ttl`].
    pub fn cache_ttl(&self) -> CacheTtl {
        self.cache.ttl
    }

    /// The prompt-cache routing key each model call sends; None sends none.
    /// Becomes [`Agent::cache_key`].
    pub fn cache_key(&self) -> Option<&str> {
        self.cache.key.as_deref()
    }
}

/// The cache TTL and key between turns, as [`Agent::set_effort`] changes
/// the effort.
///
/// ```
/// # #[cfg(feature = "testing")]
/// # fn main() {
/// use haest_core::{
///     Agent, AgentDeps, AgentOptions, SharedDeps, ShutWorkspace, Tools, cache::CacheTtl,
///     testing::ScriptedProvider,
/// };
///
/// let shared = SharedDeps::new(ScriptedProvider::new([]).resolver(), "/", ShutWorkspace);
/// let deps = AgentDeps::new(shared, Tools::new(), "Be brief.");
/// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m").with_cache_key("s-1"));
/// assert_eq!((agent.cache_ttl(), agent.cache_key()), (CacheTtl::FiveMinutes, Some("s-1")));
///
/// // The human steps away often: an hour from the next model call on.
/// agent.set_cache_ttl(CacheTtl::OneHour);
/// agent.set_cache_key("s-2");
/// assert_eq!((agent.cache_ttl(), agent.cache_key()), (CacheTtl::OneHour, Some("s-2")));
/// # }
/// # #[cfg(not(feature = "testing"))]
/// # fn main() {}
/// ```
impl Agent {
    /// How long the provider keeps what the next model call caches.
    pub fn cache_ttl(&self) -> CacheTtl {
        self.cache.ttl
    }

    /// The TTL from the next model call on. The entries already cached keep
    /// the TTL they were written with.
    pub fn set_cache_ttl(&mut self, ttl: CacheTtl) {
        self.cache.ttl = ttl;
    }

    /// The prompt-cache routing key the next model call sends; None sends none.
    pub fn cache_key(&self) -> Option<&str> {
        self.cache.key.as_deref()
    }

    /// `key` as the routing key from the next model call on.
    pub fn set_cache_key(&mut self, key: impl Into<String>) {
        self.cache.key = Some(key.into());
    }
}
