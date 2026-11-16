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

use std::sync::Arc;

use haest_core::provider::{Provider, ProviderRequest, ProviderStream};
use serde_json::{Value, json};

pub(crate) use convert::{thinking_params, to_anthropic_messages, to_anthropic_tools};

use haest_http::{HttpClient, Request};

use crate::{
    driver::drive,
    retry::RetryPolicy,
    send::{Sender, endpoint, json_post, with_headers},
};

const DEFAULT_MAX_TOKENS: u32 = 32_000;
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Where an [`AnthropicProvider`] sends its requests, and with which key.
///
/// [`AnthropicOptions::new`] takes the two fields every provider needs; set
/// the others on the value it returns.
#[derive(Debug, Clone)]
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
/// use std::sync::Arc;
///
/// use haest_core::Provider;
/// use haest_providers::{AnthropicOptions, AnthropicProvider, RetryPolicy, http::ReqwestClient};
///
/// let provider = AnthropicProvider::new(
///     AnthropicOptions::new("anthropic", "sk-ant-..."),
///     Arc::new(ReqwestClient::new()),
/// )
/// .with_retry(RetryPolicy::none());
/// assert_eq!(provider.id(), "anthropic");
/// ```
pub struct AnthropicProvider {
    opts: AnthropicOptions,
    sender: Sender,
}

impl AnthropicProvider {
    /// A provider sending over `http` with the default [`RetryPolicy`].
    /// Nothing is sent until [`Provider::stream`].
    pub fn new(opts: AnthropicOptions, http: Arc<dyn HttpClient>) -> Self {
        Self {
            opts,
            sender: Sender::new(http),
        }
    }

    /// The same provider with `retry` instead of the default policy.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.sender.retry = retry;
        self
    }

    fn http_request(&self, req: &ProviderRequest) -> Request {
        let defaults = [
            ("x-api-key", self.opts.api_key.clone()),
            ("anthropic-version", API_VERSION.into()),
            ("content-type", "application/json".into()),
            ("accept", "application/json".into()),
        ];
        json_post(
            endpoint(&self.opts.base_url, DEFAULT_BASE_URL, "v1/messages"),
            with_headers(defaults, &self.opts.headers),
            &build_anthropic_params(&self.opts.id, req),
        )
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
