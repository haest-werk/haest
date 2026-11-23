//! What a model's prompt cache can do, as the provider documents it.

use serde::{Deserialize, Serialize};

/// What a model's prompt cache can do, as its provider documents it: what
/// haest checks the cache breakpoints of a request against, and what a
/// keeper of an idle conversation reads.
///
/// Built with [`new`](Self::new) and the `with_*` setters, so a fact it
/// gains later breaks no host; its fields read as they are. In a file (the
/// catalog, a providers.json entry's `cache`) it is `{minCacheableTokens?,
/// ttl1h?, prewarmMaxTokens?, toolChanges?}`; a key it does not know is
/// refused, so a misspelt fact never reads as "no".
///
/// ```
/// use haest_core::catalog::CacheFacts;
///
/// // Claude Opus 5.5 on Anthropic's own API.
/// let opus = CacheFacts::new()
///     .with_min_cacheable_tokens(512)
///     .with_ttl_1h(true)
///     .with_prewarm_max_tokens(0)
///     .with_tool_changes(true);
/// let read: CacheFacts = serde_json::from_str(
///     r#"{"minCacheableTokens": 512, "ttl1h": true, "prewarmMaxTokens": 0, "toolChanges": true}"#,
/// )?;
/// assert_eq!(read, opus);
/// assert!(serde_json::from_str::<CacheFacts>(r#"{"ttl1H": true}"#).is_err());
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct CacheFacts {
    /// The shortest prefix the model caches, in tokens: a breakpoint that
    /// ends earlier caches nothing. Absent = unknown, nothing is checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_cacheable_tokens: Option<u64>,
    /// Whether a breakpoint may live an hour
    /// ([`CacheTtl::OneHour`](crate::cache::CacheTtl::OneHour)). False: a
    /// plan's hour is sent as five minutes.
    #[serde(default)]
    pub ttl_1h: bool,
    /// The output cap a cache pre-warm goes out with
    /// ([`ProviderRequest::prewarm`](crate::ProviderRequest::prewarm)): 0 on
    /// Anthropic's own API, 1 on OpenRouter, which does not honour 0.
    /// Absent = haest sends the model no pre-warm, so it is never kept warm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prewarm_max_tokens: Option<u32>,
    /// Whether the model takes tools added or removed mid-conversation
    /// without rewriting the cached prefix (Anthropic's mid-conversation
    /// tool changes).
    #[serde(default)]
    pub tool_changes: bool,
}

impl CacheFacts {
    /// Nothing known: no minimum, no hour, no pre-warm, no tool changes.
    pub fn new() -> Self {
        Self::default()
    }

    /// The shortest prefix the model caches, in tokens.
    #[must_use]
    pub fn with_min_cacheable_tokens(mut self, tokens: u64) -> Self {
        self.min_cacheable_tokens = Some(tokens);
        self
    }

    /// Whether a breakpoint may live an hour.
    #[must_use]
    pub fn with_ttl_1h(mut self, takes: bool) -> Self {
        self.ttl_1h = takes;
        self
    }

    /// The output cap of the model's cache pre-warm (0 or 1).
    #[must_use]
    pub fn with_prewarm_max_tokens(mut self, max_tokens: u32) -> Self {
        self.prewarm_max_tokens = Some(max_tokens);
        self
    }

    /// Whether the model takes mid-conversation tool changes.
    #[must_use]
    pub fn with_tool_changes(mut self, takes: bool) -> Self {
        self.tool_changes = takes;
        self
    }
}
