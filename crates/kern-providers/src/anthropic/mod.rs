//! Anthropic Messages API adapter (raw SSE event stream). Request shaping
//! lives in `convert`; `stream` folds the events.

mod convert;
mod stream;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use kern_core::provider::{Provider, ProviderRequest, ProviderStream};
use serde_json::{Value, json};

pub use convert::{
    thinking_params, to_anthropic_messages, to_anthropic_tools, uses_budget_thinking,
};

use crate::{
    driver::{Call, drive},
    http::RetryPolicy,
    transport::{HttpRequest, Transport},
};

const DEFAULT_MAX_TOKENS: u32 = 32_000;
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, Default)]
pub struct AnthropicOptions {
    pub api_key: String,
    /// None = `https://api.anthropic.com`.
    pub base_url: Option<String>,
    /// Sent on every request (a proxy that authenticates by header).
    pub headers: Vec<(String, String)>,
}

pub struct AnthropicProvider {
    opts: AnthropicOptions,
    transport: Arc<dyn Transport>,
    retry: RetryPolicy,
}

impl AnthropicProvider {
    pub fn new(opts: AnthropicOptions, transport: Arc<dyn Transport>) -> Self {
        Self {
            opts,
            transport,
            retry: RetryPolicy::default(),
        }
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    fn http_request(&self, req: &ProviderRequest) -> HttpRequest {
        let base = self.opts.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL);
        let mut headers = vec![
            ("x-api-key".into(), self.opts.api_key.clone()),
            ("anthropic-version".into(), API_VERSION.into()),
            ("content-type".into(), "application/json".into()),
            ("accept".into(), "application/json".into()),
        ];
        headers.extend(self.opts.headers.iter().cloned());
        HttpRequest {
            url: format!("{}/v1/messages", base.trim_end_matches('/')),
            headers,
            body: build_anthropic_params(req),
        }
    }
}

/// The streaming request body for `req`.
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
        let call = Call {
            transport: self.transport.clone(),
            request: self.http_request(&req),
            cancel: req.cancel.clone(),
            retry: self.retry.clone(),
        };
        drive(call, stream::AnthropicFold::new(&req.model))
    }
}
