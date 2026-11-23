//! Anthropic Messages API adapter (raw SSE event stream). Request shaping
//! lives in `convert`; `stream` folds the events.
//!
//! [`AnthropicProvider`] is the provider; build it from [`AnthropicOptions`],
//! or let [`ProviderRegistry`](crate::ProviderRegistry) build it for an
//! `anthropic:` spec or a providers.json entry of `"type": "anthropic"`. The
//! request body comes from [`build_anthropic_params`] and the pure functions
//! it calls. A cache pre-warm is the one call that does not stream: it goes
//! through [`drive_json`].

mod convert;
mod stream;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_cache;
#[cfg(test)]
mod tests_prewarm;
#[cfg(test)]
mod tests_tool_changes;

use haest_core::{
    cache::PrewarmRefused,
    provider::{ProviderRequest, ProviderStream},
};
use serde_json::{Value, json};

pub(crate) use convert::{
    sends_tool_changes, thinking_params, to_anthropic_messages, to_anthropic_tools,
    uses_budget_thinking,
};

use haest_http::Request;

use crate::{
    cache::cache_control,
    driver::{drive, drive_json, not_sent},
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
    /// use haest_core::catalog::ModelPricing;
    /// use haest_providers::AnthropicOptions;
    ///
    /// // A house model's price, so its answers book a cost.
    /// let price = ModelPricing::new(3.0, 0.3, 15.0).with_cache_write(3.75).with_cache_write_1h(6.0);
    /// let opts = AnthropicOptions::new("gateway", "sk-ant-...")
    ///     .with_base_url("https://llm-gateway.example.com")
    ///     .with_header("X-Team", "platform")
    ///     .with_pricing([("house-model", price)]);
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
/// Each request streams (`stream: true`) with the prompt-cache breakpoints
/// its [`cache`](haest_core::ProviderRequest::cache) plan places: by
/// default on the system prompt, the last tool and the newest message block,
/// so the next request reads everything before it from the cache. A message
/// the plan chose gets one on its last block that is not thinking. Each
/// carries its own TTL (`"ttl": "1h"` for an hour, nothing for the default
/// five minutes). A request's
/// [`routing_key`](haest_core::ProviderRequest::routing_key) is dropped:
/// the Messages API has no routing field. `max_tokens` defaults to
/// 32,000 (also for a cap of 0). Effort becomes adaptive thinking with the
/// effort level, or for models from before it (Claude 4.5 and older) a
/// thinking budget of 2,048 tokens for low up to 31,999 for max, kept below
/// `max_tokens` (a cap of 1,024 or less sends no thinking). Thinking is
/// replayed, with its signature, only to this provider.
///
/// A request with a [`tool_baseline`](haest_core::ProviderRequest::tool_baseline)
/// (a model whose catalog facts say it takes mid-conversation tool changes)
/// sends that baseline as its `tools`, and each
/// [`Message::ToolChange`](haest_core::messages::Message::ToolChange) in its
/// history as a `system` message of `tool_addition` / `tool_removal` blocks,
/// none with a breakpoint. A request that carries such a message gets the
/// beta `inline-tools-2026-09-15` in its `anthropic-beta` header, after any
/// the host configured. Without a baseline, the tool changes are left out
/// and the current tools sent.
///
/// A [`prewarm`](haest_core::ProviderRequest::prewarm) request goes out as
/// Anthropic documents a cache pre-warm: `max_tokens: 0` and not streamed,
/// with the thinking and effort the real request sends, and answers with no
/// content and the usage; [`check_prewarm`](Self::check_prewarm) says when
/// one is refused. The Messages API reports no cost: an answer is priced from
/// [`AnthropicOptions::with_pricing`] (the registry sets the catalog's).
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
        let price = self.opts.pricing.get(&req.model).cloned();
        let fold = stream::AnthropicFold::new(&self.opts.id, &req.model, price);
        if !req.prewarm {
            return drive(&self.sender, self.http_request(&req), req.cancel, fold);
        }
        match self.check_prewarm(&req) {
            Ok(()) => drive_json(&self.sender, self.http_request(&req), req.cancel, fold),
            Err(refused) => not_sent(fold, &refused.to_string(), &req.cancel),
        }
    }

    /// Whether [`stream`](Self::stream) sends `req` as the cache pre-warm it
    /// asks for: yes, unless its model would think on a token budget
    /// (`thinking.type: "enabled"`, the Claude models before adaptive
    /// thinking, at an effort above off), which Anthropic refuses with
    /// `max_tokens: 0`. A pre-warm without that thinking would write an
    /// entry the real requests never read.
    ///
    /// ```
    /// use haest_core::{ProviderRequest, cache::PrewarmRefused, provider::Effort};
    /// use haest_providers::{AnthropicOptions, AnthropicProvider, http::HttpClientRef};
    ///
    /// let provider = AnthropicProvider::new(AnthropicOptions::new("anthropic", "sk-ant-..."), HttpClientRef::default());
    /// let adaptive = ProviderRequest::new("claude-opus-5-5", vec![]).with_effort(Effort::High);
    /// assert_eq!(provider.check_prewarm(&adaptive), Ok(()));
    /// let budget = ProviderRequest::new("claude-sonnet-4-5", vec![]).with_effort(Effort::High);
    /// assert!(matches!(provider.check_prewarm(&budget), Err(PrewarmRefused::BudgetThinking { .. })));
    /// ```
    ///
    /// # Errors
    ///
    /// [`PrewarmRefused::BudgetThinking`] for a model on a thinking budget.
    pub fn check_prewarm(&self, req: &ProviderRequest) -> Result<(), PrewarmRefused> {
        let thinking = thinking_params(&req.model, req.effort, max_tokens(req));
        let kind = thinking.get("thinking").and_then(|t| t.get("type"));
        if kind.and_then(Value::as_str) == Some("enabled") {
            return Err(PrewarmRefused::BudgetThinking {
                model: req.model.clone(),
            });
        }
        Ok(())
    }

    fn http_request(&self, req: &ProviderRequest) -> Request {
        let url = endpoint(self.opts.base_url(), DEFAULT_BASE_URL, "v1/messages");
        let post = Request::post(url)
            .with_header("x-api-key", &self.opts.api_key)
            .with_header("anthropic-version", API_VERSION)
            .with_json(&build_anthropic_params(&self.opts.id, req))
            .with_header("accept", "application/json");
        let post = configured(post, &self.opts.headers);
        if sends_tool_changes(req) {
            with_beta(post, INLINE_TOOLS_BETA)
        } else {
            post
        }
    }
}

/// The beta a request with a tool change needs: it covers tools added by
/// reference and by definition (`mid-conversation-tool-changes-2026-07-01`
/// covers only the first).
const INLINE_TOOLS_BETA: &str = "inline-tools-2026-09-15";

/// `post` with `beta` in its `anthropic-beta` header, after any a host
/// configured (one header, the betas comma-separated).
fn with_beta(post: Request, beta: &str) -> Request {
    let betas = match post.header("anthropic-beta") {
        Some(had) if had.split(',').any(|b| b.trim() == beta) => return post,
        Some(had) => format!("{had},{beta}"),
        None => beta.to_string(),
    };
    post.with_header("anthropic-beta", betas)
}

/// The output cap a request of `req` sends: `req.max_tokens` when above 0,
/// else 32,000.
fn max_tokens(req: &ProviderRequest) -> u32 {
    req.max_tokens
        .filter(|m| *m > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS)
}

/// The request body for `req`, as the [`AnthropicProvider`] named
/// `provider_id` sends it.
///
/// `model`, `max_tokens` ([`max_tokens`]), the system prompt as one text
/// block, the messages ([`to_anthropic_messages`]), `stream: true`, the
/// tools when there are any ([`to_anthropic_tools`]) and the thinking fields
/// for `req.effort` under that cap ([`thinking_params`]); every breakpoint
/// of `req.cache` with its own TTL ([`cache_control`]). With a
/// [`tool_baseline`](ProviderRequest::tool_baseline) the tools are the
/// baseline and each tool change is a `system` message; without, the tools
/// are `req.tools` and the changes are left out. A pre-warm is the
/// same request with `max_tokens: 0` and no `stream`: its thinking is the
/// real cap's, so it renders into the prompt as the real request's does.
pub(crate) fn build_anthropic_params(provider_id: &str, req: &ProviderRequest) -> Value {
    let max_tokens = max_tokens(req);
    let cache = &req.cache;
    let mut system = json!({"type": "text", "text": req.system});
    if let Some(ttl) = cache.system {
        system["cache_control"] = cache_control(ttl);
    }
    let tool_changes = req.tool_baseline.is_some();
    let messages = to_anthropic_messages(&req.messages, provider_id, cache.clone(), tool_changes);
    let mut body = json!({
        "model": req.model,
        "max_tokens": if req.prewarm { 0 } else { max_tokens },
        "system": [system],
        "messages": messages,
    });
    if !req.prewarm {
        body["stream"] = Value::Bool(true);
    }
    let tools = req.sent_tools();
    if !tools.is_empty() {
        body["tools"] = Value::Array(to_anthropic_tools(tools, cache.tools));
    }
    if let Value::Object(map) = &mut body {
        map.extend(thinking_params(&req.model, req.effort, max_tokens));
    }
    body
}
