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
    driver::drive,
    http::{OPENAI_URL, RetryPolicy, bearer_headers, endpoint},
    transport::{HttpRequest, Transport},
};

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
