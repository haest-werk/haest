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
mod tests_cache;
#[cfg(test)]
mod tests_convert;

use haest_core::{
    cache::PrewarmRefused,
    provider::{ProviderRequest, ProviderStream},
};

use convert::build_responses_body;

use haest_http::Request;

use crate::{
    driver::{drive, not_sent},
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
/// OpenAI caches the prompt by itself, for its own time: a request's
/// [`cache_ttl`](haest_core::ProviderRequest::cache_ttl) is not sent. Its
/// [`cache_key`](haest_core::ProviderRequest::cache_key) goes out as
/// `prompt_cache_key` when the base URL is OpenAI's own API, and not to any
/// other server. The Responses API reports no cost: an answer is priced from
/// [`ResponsesOptions::with_pricing`] (the registry sets the catalog's). It
/// sends no cache pre-warm ([`check_prewarm`](Self::check_prewarm)).
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
        let price = self.opts.pricing.get(&req.model).cloned();
        let fold = stream::ResponsesFold::new(&self.opts.id, &req.model, price);
        if let Some(Err(refused)) = req.prewarm.then(|| self.check_prewarm(&req)) {
            return not_sent(fold, &refused.to_string(), &req.cancel);
        }
        drive(&self.sender, self.http_request(&req), req.cancel, fold)
    }

    /// Whether [`stream`](Self::stream) sends `req` as a cache pre-warm:
    /// never yet. OpenAI documents `prompt_cache_options.prewarm` for
    /// GPT-5.6 and later, but not what such a response streams nor whether
    /// a hit restarts an entry's lifetime, and a model id does not say its
    /// generation. A pre-warm request ends with this refusal as its error,
    /// and nothing is sent.
    ///
    /// ```
    /// use haest_core::ProviderRequest;
    /// use haest_providers::{ResponsesOptions, ResponsesProvider, http::HttpClientRef};
    ///
    /// let provider = ResponsesProvider::new(ResponsesOptions::new("openai", "sk-..."), HttpClientRef::default());
    /// let warm = ProviderRequest::new("gpt-6.1-sol", vec![]).with_prewarm(true);
    /// assert!(provider.check_prewarm(&warm).is_err());
    /// ```
    ///
    /// # Errors
    ///
    /// Always [`PrewarmRefused::Unsupported`].
    pub fn check_prewarm(&self, _req: &ProviderRequest) -> Result<(), PrewarmRefused> {
        Err(PrewarmRefused::Unsupported {
            provider: self.opts.id.clone(),
        })
    }

    fn http_request(&self, req: &ProviderRequest) -> Request {
        bearer_post(
            endpoint(self.opts.base_url(), OPENAI_URL, "responses"),
            &self.opts.api_key,
            &build_responses_body(&self.opts, req),
            &self.opts.headers,
        )
    }
}
