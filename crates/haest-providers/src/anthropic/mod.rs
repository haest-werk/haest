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

use haest_core::provider::{ProviderRequest, ProviderStream};
use serde_json::{Value, json};

pub(crate) use convert::{thinking_params, to_anthropic_messages, to_anthropic_tools};

use haest_http::Request;

use crate::{
    driver::drive,
    send::{Sender, configured, endpoint, endpoint_options, sender_provider},
};

const DEFAULT_MAX_TOKENS: u32 = 32_000;
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

endpoint_options! {
    /// Where an [`AnthropicProvider`] sends its requests, and with which key
    /// (sent as `x-api-key`).
    ///
    /// [`AnthropicOptions::new`] takes the two every provider needs; set the
    /// others with the `with_*` setters. Its Debug output shows neither the key,
    /// nor a header's value, nor a `user:password@` in the base URL.
    ///
    /// ```
    /// use haest_providers::AnthropicOptions;
    ///
    /// let opts = AnthropicOptions::new("gateway", "sk-ant-...")
    ///     .with_base_url("https://llm-gateway.example.com")
    ///     .with_header("X-Team", "platform");
    /// assert_eq!(opts.base_url(), Some("https://llm-gateway.example.com"));
    /// assert!(!format!("{opts:?}").contains("sk-ant"));
    /// ```
    AnthropicOptions {
        api: "Anthropic's own API",
        url: "https://api.anthropic.com",
        path: "v1/messages",
        key: "x-api-key",
    }
}

/// Anthropic's Messages API as a [`Provider`](haest_core::Provider).
///
/// Each request streams (`stream: true`) with a prompt-cache breakpoint on
/// the system prompt, the last tool and the newest message block, so the next
/// request reads everything before it from the cache. `max_tokens` defaults to
/// 32,000 (also for a cap of 0). Effort becomes adaptive thinking with the
/// effort level, or for models from before it (Claude 4.5 and older) a
/// thinking budget of 2,048 tokens for low up to 31,999 for max, kept below
/// `max_tokens` (a cap of 1,024 or less sends no thinking). Thinking is
/// replayed, with its signature, only to this provider.
///
/// Its [`Provider::id`](haest_core::Provider::id) is [`AnthropicOptions::id`]: `anthropic` for the
/// built-in, the entry's name for a providers.json entry of
/// `"type": "anthropic"`. Anthropic signs thinking per endpoint, so a gateway's
/// thinking never replays to Anthropic itself, nor the other way round.
///
/// ```
/// use haest_core::Provider;
/// use haest_providers::{AnthropicOptions, AnthropicProvider, RetryPolicy, http::HttpClientRef};
///
/// let provider = AnthropicProvider::new(
///     AnthropicOptions::new("anthropic", "sk-ant-..."),
///     HttpClientRef::default(),
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

sender_provider!(AnthropicProvider, AnthropicOptions);

impl AnthropicProvider {
    /// Sends `req` and streams the answer (the
    /// [`Provider`](haest_core::Provider) method, callable without importing
    /// the trait).
    pub fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let fold = stream::AnthropicFold::new(&self.opts.id, &req.model);
        drive(&self.sender, self.http_request(&req), req.cancel, fold)
    }

    fn http_request(&self, req: &ProviderRequest) -> Request {
        let url = endpoint(self.opts.base_url(), DEFAULT_BASE_URL, "v1/messages");
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
/// `model`, `max_tokens` (`req.max_tokens` when above 0, else 32,000), the
/// system prompt as one cached text block, the messages
/// ([`to_anthropic_messages`]), the tools when there are any
/// ([`to_anthropic_tools`]) and the thinking fields for `req.effort` under
/// that cap ([`thinking_params`]).
pub(crate) fn build_anthropic_params(provider_id: &str, req: &ProviderRequest) -> Value {
    let max_tokens = req
        .max_tokens
        .filter(|m| *m > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    let mut body = json!({
        "model": req.model,
        "max_tokens": max_tokens,
        "system": [{"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}],
        "messages": to_anthropic_messages(&req.messages, provider_id),
        "stream": true,
    });
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(to_anthropic_tools(&req.tools));
    }
    if let Value::Object(map) = &mut body {
        map.extend(thinking_params(&req.model, req.effort, max_tokens));
    }
    body
}
