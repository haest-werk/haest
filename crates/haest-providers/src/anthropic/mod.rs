//! Anthropic Messages API adapter (raw SSE event stream). Request shaping
//! lives in `convert`; `stream` folds the events.
//!
//! [`AnthropicProvider`] is the provider; build it from [`AnthropicOptions`],
//! or let [`ProviderRegistry`](crate::ProviderRegistry) build it for an
//! `anthropic:` spec or a providers.json entry of `"type": "anthropic"`. The
//! request body comes from [`build_anthropic_params`] and the pure functions
//! it calls.

mod convert;
mod stream;
#[cfg(test)]
mod tests;

use std::{fmt, time::Duration};

use haest_core::provider::{Provider, ProviderRequest, ProviderStream};
use serde_json::{Value, json};

pub(crate) use convert::{thinking_params, to_anthropic_messages, to_anthropic_tools};

use haest_http::{HttpClientRef, Request};

use crate::{
    driver::drive,
    retry::RetryPolicy,
    send::{HeaderNames, REDACTED, Sender, UrlDebug, configured, endpoint},
};

const DEFAULT_MAX_TOKENS: u32 = 32_000;
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Where an [`AnthropicProvider`] sends its requests, and with which key.
///
/// [`AnthropicOptions::new`] takes the two fields every provider needs; set
/// the others with the `with_*` setters (or on the fields). Its Debug output
/// shows neither the key, nor a header's value, nor a `user:password@` in
/// the base URL.
///
/// ```
/// use haest_providers::AnthropicOptions;
///
/// let opts = AnthropicOptions::new("gateway", "sk-ant-...")
///     .with_base_url("https://llm-gateway.example.com")
///     .with_header("X-Team", "platform");
/// assert_eq!(opts.base_url.as_deref(), Some("https://llm-gateway.example.com"));
/// assert!(!format!("{opts:?}").contains("sk-ant"));
/// ```
#[derive(Clone)]
pub struct AnthropicOptions {
    /// The provider's name: its [`Provider::id`], stored on every message it
    /// answers. Thinking replays, with its signature, only to the provider
    /// whose id a message carries: each endpoint signs its own.
    pub id: String,
    /// Sent as `x-api-key`.
    pub api_key: String,
    /// None = `https://api.anthropic.com`; requests go to `<base_url>/v1/messages`.
    pub base_url: Option<String>,
    /// Sent on every request (a proxy that authenticates by header); one
    /// named like a default (`x-api-key`, in any case) replaces it.
    pub headers: Vec<(String, String)>,
}

impl AnthropicOptions {
    /// The provider `id`, sending `api_key` to Anthropic's own API with no
    /// extra headers.
    pub fn new(id: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            api_key: api_key.into(),
            base_url: None,
            headers: Vec::new(),
        }
    }

    /// These options sending to `url` (`<url>/v1/messages`) instead of
    /// Anthropic's own API.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// These options sending `name: value` on every request, after the ones
    /// before it.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

impl fmt::Debug for AnthropicOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicOptions")
            .field("id", &self.id)
            .field("api_key", &REDACTED)
            .field("base_url", &self.base_url.as_deref().map(UrlDebug))
            .field("headers", &HeaderNames(&self.headers))
            .finish()
    }
}

/// Anthropic's Messages API as a [`Provider`].
///
/// Each request streams (`stream: true`) with a prompt-cache breakpoint on
/// the system prompt, the last tool and the newest message block, so the next
/// request reads everything before it from the cache. `max_tokens` defaults to
/// 32,000. Effort becomes adaptive thinking with the effort level, or for
/// models from before it (Claude 4.5 and older) a thinking budget of 2,048
/// tokens for low up to 31,999 for max. Thinking is replayed, with its
/// signature, only to this provider.
///
/// Its [`Provider::id`] is [`AnthropicOptions::id`]: `anthropic` for the
/// built-in, the entry's name for a providers.json entry of
/// `"type": "anthropic"`. Anthropic signs thinking per endpoint, so a gateway's
/// thinking never replays to Anthropic itself, nor the other way round.
///
/// ```
/// use haest_core::Provider;
/// use haest_providers::{AnthropicOptions, AnthropicProvider, RetryPolicy, http::ReqwestClient};
///
/// let provider = AnthropicProvider::new(
///     AnthropicOptions::new("anthropic", "sk-ant-..."),
///     ReqwestClient::new(),
/// )
/// .with_retry(RetryPolicy::none());
/// assert_eq!(provider.id(), "anthropic");
/// ```
///
/// Cloning is cheap: a clone shares the client.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    opts: AnthropicOptions,
    sender: Sender,
}

impl AnthropicProvider {
    /// A provider sending over `http` (the client itself, or an `Arc` of one
    /// the host shares) with the default [`RetryPolicy`]. Nothing is sent
    /// until [`Provider::stream`].
    pub fn new(opts: AnthropicOptions, http: impl Into<HttpClientRef>) -> Self {
        Self {
            opts,
            sender: Sender::new(http.into()),
        }
    }

    /// The options it was built from.
    pub fn options(&self) -> &AnthropicOptions {
        &self.opts
    }

    /// The same provider with `retry` instead of the default policy.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.sender.retry = retry;
        self
    }

    /// The same provider waiting at most `wait` for each try's status and
    /// headers (default ten minutes, as long as the SDKs allow; a long prompt
    /// can sit in a queue before the first byte). A try with no answer by
    /// then counts as unanswered, and the [`RetryPolicy`] may try again; the
    /// body then takes as long as it takes.
    pub fn with_headers_timeout(mut self, wait: Duration) -> Self {
        self.sender.headers_timeout = wait;
        self
    }

    fn http_request(&self, req: &ProviderRequest) -> Request {
        let url = endpoint(&self.opts.base_url, DEFAULT_BASE_URL, "v1/messages");
        let post = Request::post(url)
            .with_header("x-api-key", &self.opts.api_key)
            .with_header("anthropic-version", API_VERSION)
            .with_json(&build_anthropic_params(&self.opts.id, req))
            .with_header("accept", "application/json");
        configured(post, &self.opts.headers)
    }
}

/// The streaming request body for `req`, as the [`AnthropicProvider`] named
/// `provider_id` sends it.
///
/// `model`, `max_tokens` (`req.max_tokens`, else 32,000), the system prompt
/// as one cached text block, the messages ([`to_anthropic_messages`]), the
/// tools when there are any ([`to_anthropic_tools`]) and the thinking fields
/// for `req.effort` ([`thinking_params`]).
pub(crate) fn build_anthropic_params(provider_id: &str, req: &ProviderRequest) -> Value {
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "system": [{"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}],
        "messages": to_anthropic_messages(&req.messages, provider_id),
        "stream": true,
    });
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(to_anthropic_tools(&req.tools));
    }
    if let Value::Object(map) = &mut body {
        map.extend(thinking_params(&req.model, req.effort));
    }
    body
}

impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.opts.id
    }

    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let fold = stream::AnthropicFold::new(&self.opts.id, &req.model);
        drive(&self.sender, self.http_request(&req), req.cancel, fold)
    }
}
