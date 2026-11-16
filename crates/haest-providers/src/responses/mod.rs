//! OpenAI Responses API adapter (raw SSE event stream). Request shaping lives
//! in `convert`; `stream` folds the events.
//!
//! [`ResponsesProvider`] is the provider, configured by [`ResponsesOptions`];
//! [`ProviderRegistry`](crate::ProviderRegistry) builds it for an `openai:`
//! spec. The request-shaping functions ([`build_responses_body`] and the ones
//! it calls) are pure and public, for a host that wants the exact body a
//! request would send.

mod convert;
mod stream;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_convert;

use std::sync::Arc;

use haest_core::provider::{Provider, ProviderRequest, ProviderStream};

pub use convert::{build_responses_body, reasoning_params, to_responses_input};

use crate::{
    driver::drive,
    http::{OPENAI_URL, RetryPolicy, bearer_headers, endpoint},
    transport::{HttpRequest, Transport},
};

/// Where a [`ResponsesProvider`] sends its requests, and with which key.
#[derive(Debug, Clone, Default)]
pub struct ResponsesOptions {
    /// The provider's name: its [`Provider::id`], stored on every message it
    /// answers. Reasoning replays only to the provider whose id a message carries.
    pub id: String,
    /// Sent as `Authorization: Bearer <api_key>`.
    pub api_key: String,
    /// None = `https://api.openai.com/v1`; requests go to `<base_url>/responses`.
    pub base_url: Option<String>,
    /// Sent on every request, after (and over) the defaults.
    pub headers: Vec<(String, String)>,
}

/// OpenAI's Responses API as a [`Provider`]: the built-in `openai` provider.
///
/// It runs stateless (`store: false`): each request carries the whole
/// history, and the model's reasoning survives between turns only as its
/// encrypted content, kept in the thinking part's `signature` and replayed to
/// the same provider. haest's effort maps onto OpenAI's levels
/// ([`reasoning_params`]). A refusal or a `content_filter` stop ends the message
/// with [`StopReason::Refusal`](haest_core::StopReason::Refusal), running out of
/// output tokens with `MaxTokens`.
///
/// ```
/// use std::sync::Arc;
///
/// use haest_core::Provider;
/// use haest_providers::{ReqwestTransport, ResponsesOptions, ResponsesProvider};
///
/// let opts = ResponsesOptions { id: "openai".into(), api_key: "sk-...".into(), ..Default::default() };
/// let provider = ResponsesProvider::new(opts, Arc::new(ReqwestTransport::new()));
/// assert_eq!(provider.id(), "openai");
/// ```
pub struct ResponsesProvider {
    opts: ResponsesOptions,
    transport: Arc<dyn Transport>,
    retry: RetryPolicy,
}

impl ResponsesProvider {
    /// A provider over `transport` with the default [`RetryPolicy`]. Nothing
    /// is sent until [`Provider::stream`].
    pub fn new(opts: ResponsesOptions, transport: Arc<dyn Transport>) -> Self {
        Self {
            opts,
            transport,
            retry: RetryPolicy::default(),
        }
    }

    /// The same provider with `retry` instead of the default policy.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    fn http_request(&self, req: &ProviderRequest) -> HttpRequest {
        HttpRequest {
            url: endpoint(&self.opts.base_url, OPENAI_URL, "responses"),
            headers: bearer_headers(&self.opts.api_key, &self.opts.headers),
            body: build_responses_body(&self.opts.id, req),
        }
    }
}

impl Provider for ResponsesProvider {
    fn id(&self) -> &str {
        &self.opts.id
    }

    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let fold = stream::ResponsesFold::new(&self.opts.id, &req.model);
        drive(
            &self.transport,
            &self.retry,
            self.http_request(&req),
            req.cancel,
            fold,
        )
    }
}
