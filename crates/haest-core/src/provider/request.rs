//! [`ProviderRequest`]: one model call, provider-neutral.

use tokio_util::sync::CancellationToken;

use super::{Effort, ToolSpec, WebSearchOptions};
use crate::{cache::CacheTtl, messages::Message};

/// One model call, provider-neutral: everything an adapter turns into its wire
/// request.
///
/// Built with [`ProviderRequest::new`] and a `with_*` per optional part, so a
/// part added later breaks no caller; an adapter reads the fields.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProviderRequest {
    /// The provider's own model id (the spec without `provider:`).
    pub model: String,
    /// The system prompt.
    pub system: String,
    /// The history. The loop sends it fitted to what the model accepts
    /// (media it cannot take is withheld, with a note).
    pub messages: Vec<Message>,
    /// The tools the model may call; empty = none.
    pub tools: Vec<ToolSpec>,
    /// The turn's interrupt. When it fires, end the stream soon, with a
    /// [`ProviderEvent::Done`](super::ProviderEvent::Done) whose stop reason
    /// is [`StopReason::Aborted`](crate::StopReason::Aborted).
    pub cancel: CancellationToken,
    /// None = send nothing and let the model use its own default.
    pub effort: Option<Effort>,
    /// The output cap; None = the adapter's or the model's default.
    pub max_tokens: Option<u32>,
    /// Only providers that run one (OpenRouter) send it.
    pub web_search: Option<WebSearchOptions>,
    /// How long the provider keeps the prompt cache this request writes, on
    /// every breakpoint it places; a provider with no choice ignores it.
    pub cache_ttl: CacheTtl,
    /// What groups this request with others for the provider's prompt cache
    /// (a conversation's id, say); None = nothing. OpenAI's own API takes it
    /// as `prompt_cache_key`, OpenRouter as `session_id`; every other
    /// provider leaves it out.
    pub cache_key: Option<String>,
    /// A cache pre-warm: the provider reads the request into its prompt
    /// cache and generates nothing. A provider sends one only when
    /// [`Provider::check_prewarm`](super::Provider::check_prewarm) says it
    /// can; otherwise the call ends with that refusal as its error, and
    /// nothing is sent.
    pub prewarm: bool,
}

impl ProviderRequest {
    /// A call to `model` with `messages`: no system prompt, no tools, a fresh
    /// cancel token, the model's own effort, output cap and search, the
    /// five-minute cache TTL, no cache key, and not a pre-warm.
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            system: String::new(),
            messages,
            tools: Vec::new(),
            cancel: CancellationToken::new(),
            effort: None,
            max_tokens: None,
            web_search: None,
            cache_ttl: CacheTtl::default(),
            cache_key: None,
            prewarm: false,
        }
    }

    /// With `system` as the system prompt.
    #[must_use]
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = system.into();
        self
    }

    /// With `tools` offered to the model.
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }

    /// Interrupted by `cancel`.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// At reasoning `effort`. Without it nothing is sent: the model's default.
    #[must_use]
    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// With at most `max_tokens` of output. Without it, the adapter's or the
    /// model's default cap.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// With a provider-side web search. Without it, none is sent.
    #[must_use]
    pub fn with_web_search(mut self, web_search: WebSearchOptions) -> Self {
        self.web_search = Some(web_search);
        self
    }

    /// Cached for `ttl` instead of five minutes ([`cache_ttl`](Self::cache_ttl)).
    ///
    /// ```
    /// use haest_core::{ProviderRequest, cache::CacheTtl};
    ///
    /// let req = ProviderRequest::new("claude-opus-5-5", vec![]).with_cache_ttl(CacheTtl::OneHour);
    /// assert_eq!(req.cache_ttl, CacheTtl::OneHour);
    /// ```
    #[must_use]
    pub fn with_cache_ttl(mut self, ttl: CacheTtl) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Grouped under `key` for the provider's prompt cache
    /// ([`cache_key`](Self::cache_key)).
    ///
    /// ```
    /// use haest_core::ProviderRequest;
    ///
    /// let req = ProviderRequest::new("gpt-6.1-sol", vec![]).with_cache_key("session-42");
    /// assert_eq!(req.cache_key.as_deref(), Some("session-42"));
    /// ```
    #[must_use]
    pub fn with_cache_key(mut self, key: impl Into<String>) -> Self {
        self.cache_key = Some(key.into());
        self
    }

    /// Sent as a cache pre-warm, or not ([`prewarm`](Self::prewarm)).
    ///
    /// ```
    /// use haest_core::ProviderRequest;
    ///
    /// let req = ProviderRequest::new("claude-opus-5-5", vec![]).with_prewarm(true);
    /// assert!(req.prewarm);
    /// ```
    #[must_use]
    pub fn with_prewarm(mut self, prewarm: bool) -> Self {
        self.prewarm = prewarm;
        self
    }
}
