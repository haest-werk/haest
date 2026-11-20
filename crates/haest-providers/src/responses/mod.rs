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

use haest_core::provider::{ProviderRequest, ProviderStream};

use convert::build_responses_body;

use haest_http::Request;

use crate::{
    driver::drive,
    send::{OPENAI_URL, Sender, bearer_post, endpoint, endpoint_options, sender_provider},
};

endpoint_options! {
    /// Where a [`ResponsesProvider`] sends its requests, and with which key
    /// (sent as `Authorization: Bearer <api_key>`).
    ///
    /// [`ResponsesOptions::new`] takes the two every provider needs; set the
    /// others with the `with_*` setters. Its Debug output shows neither the key,
    /// nor a header's value, nor a `user:password@` in the base URL.
    ///
    /// ```
    /// use haest_providers::ResponsesOptions;
    ///
    /// let opts = ResponsesOptions::new("openai", "sk-...").with_header("OpenAI-Project", "proj_1");
    /// assert_eq!(opts.id(), "openai");
    /// assert_eq!(opts.base_url(), None);
    /// ```
    ResponsesOptions {
        api: "OpenAI's own API",
        url: "https://api.openai.com/v1",
        path: "responses",
        key: "authorization",
    }
}

/// OpenAI's Responses API as a [`Provider`](haest_core::Provider): the
/// built-in `openai` provider.
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

sender_provider!(ResponsesProvider, ResponsesOptions);

impl ResponsesProvider {
    /// Sends `req` and streams the answer (the
    /// [`Provider`](haest_core::Provider) method, callable without importing
    /// the trait).
    pub fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let fold = stream::ResponsesFold::new(&self.opts.id, &req.model);
        drive(&self.sender, self.http_request(&req), req.cancel, fold)
    }

    fn http_request(&self, req: &ProviderRequest) -> Request {
        bearer_post(
            endpoint(self.opts.base_url(), OPENAI_URL, "responses"),
            &self.opts.api_key,
            &build_responses_body(&self.opts.id, req),
            &self.opts.headers,
        )
    }
}
