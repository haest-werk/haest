//! OpenAI Responses API adapter (raw SSE event stream). Request shaping lives
//! in `convert`; `stream` folds the events.

mod convert;
mod stream;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_convert;

use std::sync::Arc;

use kern_core::provider::{Provider, ProviderRequest, ProviderStream};

pub use convert::{build_responses_body, reasoning_params, to_responses_input};

use crate::{
    driver::{Call, drive},
    http::{RetryPolicy, bearer_headers},
    transport::{HttpRequest, Transport},
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Debug, Clone, Default)]
pub struct ResponsesOptions {
    pub id: String,
    pub api_key: String,
    /// None = `https://api.openai.com/v1`.
    pub base_url: Option<String>,
    /// Sent on every request, after (and over) the defaults.
    pub headers: Vec<(String, String)>,
}

pub struct ResponsesProvider {
    opts: ResponsesOptions,
    transport: Arc<dyn Transport>,
    retry: RetryPolicy,
}

impl ResponsesProvider {
    pub fn new(opts: ResponsesOptions, transport: Arc<dyn Transport>) -> Self {
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
        HttpRequest {
            url: format!("{}/responses", base.trim_end_matches('/')),
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
        let call = Call {
            transport: self.transport.clone(),
            request: self.http_request(&req),
            cancel: req.cancel.clone(),
            retry: self.retry.clone(),
        };
        drive(call, stream::ResponsesFold::new(&self.opts.id, &req.model))
    }
}
