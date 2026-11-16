//! Anthropic Messages API adapter (raw SSE event stream). Request shaping
//! lives in `convert`; `stream` folds the events.
//!
//! [`AnthropicProvider`] is the provider; build it from [`AnthropicOptions`],
//! or let [`ProviderRegistry`](crate::ProviderRegistry) build it for an
//! `anthropic:` spec or a providers.json entry of `"type": "anthropic"`. The
//! request-shaping functions ([`build_anthropic_params`] and the ones it
//! calls) are pure and public, for a host that wants the exact body a request
//! would send.

mod convert;
mod stream;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use haest_core::provider::{Provider, ProviderRequest, ProviderStream};
use serde_json::{Value, json};

pub use convert::{
    thinking_params, to_anthropic_messages, to_anthropic_tools, uses_budget_thinking,
};

use haest_http::{HttpClient, Request};

use crate::{
    driver::drive,
    send::{RetryPolicy, endpoint, json_post, with_headers},
};

const DEFAULT_MAX_TOKENS: u32 = 32_000;
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Where an [`AnthropicProvider`] sends its requests, and with which key.
#[derive(Debug, Clone, Default)]
pub struct AnthropicOptions {
    /// Sent as `x-api-key`.
    pub api_key: String,
    /// None = `https://api.anthropic.com`; requests go to `<base_url>/v1/messages`.
    pub base_url: Option<String>,
    /// Sent on every request (a proxy that authenticates by header); one
    /// named like a default (`x-api-key`, in any case) replaces it.
    pub headers: Vec<(String, String)>,
}

/// Anthropic's Messages API as a [`Provider`].
///
/// Each request streams (`stream: true`) with a prompt-cache breakpoint on
/// the system prompt, the last tool and the newest message block, so the next
/// request reads everything before it from the cache. `max_tokens` defaults to
/// 32,000. Effort becomes adaptive thinking, or a thinking budget for models
/// from before it ([`thinking_params`]). Thinking is replayed, with its
/// signature, only to this provider.
///
/// Its [`Provider::id`] is always `anthropic`, also for a providers.json entry
/// of `"type": "anthropic"`: a conversation's thinking replays to any
/// Messages API endpoint.
///
/// ```
/// use std::sync::Arc;
///
/// use haest_core::Provider;
/// use haest_providers::{AnthropicOptions, AnthropicProvider, RetryPolicy, http::ReqwestClient};
///
/// let provider = AnthropicProvider::new(
///     AnthropicOptions { api_key: "sk-ant-...".into(), ..AnthropicOptions::default() },
///     Arc::new(ReqwestClient::new()),
/// )
/// .with_retry(RetryPolicy::none());
/// assert_eq!(provider.id(), "anthropic");
/// ```
pub struct AnthropicProvider {
    opts: AnthropicOptions,
    http: Arc<dyn HttpClient>,
    retry: RetryPolicy,
}

impl AnthropicProvider {
    /// A provider sending over `http` with the default [`RetryPolicy`].
    /// Nothing is sent until [`Provider::stream`].
    pub fn new(opts: AnthropicOptions, http: Arc<dyn HttpClient>) -> Self {
        Self {
            opts,
            http,
            retry: RetryPolicy::default(),
        }
    }

    /// The same provider with `retry` instead of the default policy.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
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
            &build_anthropic_params(req),
        )
    }
}

/// The streaming request body for `req`, as [`AnthropicProvider`] sends it.
///
/// `model`, `max_tokens` (`req.max_tokens`, else 32,000), the system prompt
/// as one cached text block, the messages ([`to_anthropic_messages`]), the
/// tools when there are any ([`to_anthropic_tools`]) and the thinking fields
/// for `req.effort` ([`thinking_params`]).
pub fn build_anthropic_params(req: &ProviderRequest) -> Value {
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "system": [{"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}],
        "messages": to_anthropic_messages(&req.messages),
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
        "anthropic"
    }

    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let fold = stream::AnthropicFold::new(&req.model);
        drive(
            &self.http,
            &self.retry,
            self.http_request(&req),
            req.cancel,
            fold,
        )
    }
}
