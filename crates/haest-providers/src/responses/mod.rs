//! OpenAI Responses API adapter (raw SSE event stream). Request shaping lives
//! in `convert`; `stream` folds the events.
//!
//! [`ResponsesProvider`] is the provider, configured by [`ResponsesOptions`];
//! [`ProviderRegistry`](crate::ProviderRegistry) builds it for an `openai:`
//! spec. The request body comes from [`build_responses_body`] and the pure
//! functions it calls.

pub(crate) mod convert;
mod stream;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_convert;

use std::{fmt, time::Duration};

use haest_core::provider::{Provider, ProviderRequest, ProviderStream};

use convert::build_responses_body;

use haest_http::{HttpClientRef, Request};

use crate::{
    driver::drive,
    retry::RetryPolicy,
    send::{HeaderNames, OPENAI_URL, REDACTED, Sender, UrlDebug, bearer_post, endpoint},
};

/// Where a [`ResponsesProvider`] sends its requests, and with which key.
///
/// [`ResponsesOptions::new`] takes the two fields every provider needs; set
/// the others with the `with_*` setters (or on the fields). Its Debug output
/// shows neither the key, nor a header's value, nor a `user:password@` in
/// the base URL.
#[derive(Clone)]
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

impl ResponsesOptions {
    /// The provider `id`, sending `api_key` to OpenAI's own API with no extra
    /// headers.
    pub fn new(id: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            api_key: api_key.into(),
            base_url: None,
            headers: Vec::new(),
        }
    }

    /// These options sending to `url` (`<url>/responses`) instead of
    /// OpenAI's own API.
    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// These options sending `name: value` on every request, after the ones
    /// before it.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

impl fmt::Debug for ResponsesOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponsesOptions")
            .field("id", &self.id)
            .field("api_key", &REDACTED)
            .field("base_url", &self.base_url.as_deref().map(UrlDebug))
            .field("headers", &HeaderNames(&self.headers))
            .finish()
    }
}

/// OpenAI's Responses API as a [`Provider`]: the built-in `openai` provider.
///
/// It runs stateless (`store: false`): each request carries the whole
/// history, and the model's reasoning survives between turns only as its
/// encrypted content, kept in the thinking part's `signature` and replayed to
/// the same provider. haest's effort maps onto OpenAI's levels (`off` is
/// `none`, `max` is `xhigh`), with an automatic reasoning summary; no effort
/// sends no `reasoning` field. A refusal or a `content_filter` stop ends the message
/// with [`StopReason::Refusal`](haest_core::StopReason::Refusal), running out of
/// output tokens with `MaxTokens`.
///
/// ```
/// use haest_core::Provider;
/// use haest_providers::{ResponsesOptions, ResponsesProvider, http::HttpClientRef};
///
/// let opts = ResponsesOptions::new("openai", "sk-...");
/// let provider = ResponsesProvider::new(opts, HttpClientRef::default());
/// assert_eq!(provider.id(), "openai");
/// ```
///
/// Cloning is cheap: a clone shares the client.
#[derive(Debug, Clone)]
pub struct ResponsesProvider {
    opts: ResponsesOptions,
    sender: Sender,
}

impl ResponsesProvider {
    /// A provider sending over `http` (the client itself, or an `Arc` of one
    /// the host shares) with the default [`RetryPolicy`]. Nothing is sent
    /// until [`stream`](Self::stream).
    pub fn new(opts: ResponsesOptions, http: impl Into<HttpClientRef>) -> Self {
        Self {
            opts,
            sender: Sender::new(http.into()),
        }
    }

    /// The options it was built from.
    pub fn options(&self) -> &ResponsesOptions {
        &self.opts
    }

    /// The same provider with `retry` instead of the default policy.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.sender.retry = retry;
        self
    }

    /// The same provider waiting at most `wait` for each try's status and
    /// headers (default ten minutes); past it the try counts as unanswered,
    /// as [`AnthropicProvider::with_headers_timeout`](crate::AnthropicProvider::with_headers_timeout) says.
    #[must_use]
    pub fn with_headers_timeout(mut self, wait: Duration) -> Self {
        self.sender.headers_timeout = wait;
        self
    }

    /// How it retries ([`with_retry`](Self::with_retry)).
    pub fn retry(&self) -> RetryPolicy {
        self.sender.retry
    }

    /// How long each try waits for its status and headers
    /// ([`with_headers_timeout`](Self::with_headers_timeout)).
    pub fn headers_timeout(&self) -> Duration {
        self.sender.headers_timeout
    }

    /// The client its requests go through.
    pub fn http(&self) -> &HttpClientRef {
        &self.sender.http
    }

    /// Its id, the options' `id`: what its answers carry as their provider.
    pub fn id(&self) -> &str {
        &self.opts.id
    }

    /// Sends `req` and streams the answer (the [`Provider`] method, callable
    /// without importing the trait).
    pub fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let fold = stream::ResponsesFold::new(&self.opts.id, &req.model);
        drive(&self.sender, self.http_request(&req), req.cancel, fold)
    }

    fn http_request(&self, req: &ProviderRequest) -> Request {
        bearer_post(
            endpoint(&self.opts.base_url, OPENAI_URL, "responses"),
            &self.opts.api_key,
            &build_responses_body(&self.opts.id, req),
            &self.opts.headers,
        )
    }
}

/// [`ResponsesProvider::id`] and [`ResponsesProvider::stream`].
impl Provider for ResponsesProvider {
    fn id(&self) -> &str {
        ResponsesProvider::id(self)
    }

    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        ResponsesProvider::stream(self, req)
    }
}
