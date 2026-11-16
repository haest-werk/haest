//! OpenAI Chat Completions adapter -- OpenRouter and any compatible server
//! (OpenAI itself goes through [`crate::responses`]).
//! Request building lives in `convert`, stream folding in `stream`; this file
//! only wires the transport between them.

mod convert;
mod stream;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_direct;
#[cfg(test)]
mod tests_routing;
#[cfg(test)]
mod tests_search;
#[cfg(test)]
mod tests_stream;

use std::{collections::HashMap, sync::Arc};

use kern_core::{
    catalog::{ModelPricing, Reasoning},
    messages::AssistantMessage,
    pricing::book_priced_cost,
    provider::{ProviderEvent, ProviderRequest, ProviderStream, mark_stream_failed},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use convert::reasoning_effort_extras;
pub use convert::{flavor_extras, to_chat_messages, to_chat_tools};
pub(crate) use stream::{StreamState, apply_chunk, finish_message};

use crate::{
    driver::{Step, StreamFold, drive, openai_error},
    http::{OPENAI_URL, RetryPolicy, bearer_headers, endpoint},
    routing::RoutingEnv,
    sse::SseEvent,
    transport::{HttpRequest, Transport},
    web_search::web_search_tool,
};

/// OpenAI itself is NOT a flavor here: it speaks Responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatFlavor {
    OpenRouter,
    Generic,
}

#[derive(Debug, Clone)]
pub struct CompatOptions {
    pub id: String,
    pub flavor: CompatFlavor,
    pub api_key: String,
    /// None = `https://api.openai.com/v1`.
    pub base_url: Option<String>,
    /// Sent on every request, after (and over) the defaults.
    pub headers: Vec<(String, String)>,
    /// A provider that takes a top-level `reasoning_effort`: the levels it accepts
    /// (kern's effort is fitted to the nearest; off only when it lists `none`).
    pub reasoning_effort: Option<Reasoning>,
    /// Price per model id, for a provider whose API returns no cost (kern-core `pricing`).
    pub pricing: HashMap<String, ModelPricing>,
    /// OpenRouter only: provider routing, resolved per request against the request's model.
    /// None = off: send no `provider` object (a host gateway may decide routing itself).
    pub routing: Option<RoutingEnv>,
}

impl CompatOptions {
    /// A provider `id` of `flavor` with nothing else configured.
    pub fn new(id: impl Into<String>, flavor: CompatFlavor, api_key: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            flavor,
            api_key: api_key.into(),
            base_url: None,
            headers: Vec::new(),
            reasoning_effort: None,
            pricing: HashMap::new(),
            routing: None,
        }
    }
}

pub struct CompatProvider {
    opts: CompatOptions,
    transport: Arc<dyn Transport>,
    retry: RetryPolicy,
}

impl CompatProvider {
    pub fn new(opts: CompatOptions, transport: Arc<dyn Transport>) -> Self {
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

    pub fn options(&self) -> &CompatOptions {
        &self.opts
    }

    fn http_request(&self, req: &ProviderRequest) -> HttpRequest {
        HttpRequest {
            url: endpoint(&self.opts.base_url, OPENAI_URL, "chat/completions"),
            headers: bearer_headers(&self.opts.api_key, &self.opts.headers),
            body: build_compat_body(&self.opts, req),
        }
    }
}

/// The streaming request body for `req` under `opts`.
pub fn build_compat_body(opts: &CompatOptions, req: &ProviderRequest) -> Value {
    let mut tools = to_chat_tools(&req.tools);
    if opts.flavor == CompatFlavor::OpenRouter {
        tools.extend(req.web_search.as_ref().map(web_search_tool));
    }
    let messages = to_chat_messages(
        &req.system,
        &req.messages,
        opts.flavor,
        &req.model,
        &opts.id,
    );
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    let map = body.as_object_mut().expect("body is an object");
    if !tools.is_empty() {
        map.insert("tools".into(), Value::Array(tools));
    }
    if let Some(max) = req.max_tokens.filter(|m| *m > 0) {
        map.insert("max_tokens".into(), json!(max));
    }
    map.extend(flavor_extras(
        opts.flavor,
        req.effort,
        &req.model,
        opts.routing.as_ref(),
    ));
    map.extend(reasoning_effort_extras(
        opts.reasoning_effort.as_ref(),
        req.effort,
    ));
    body
}

struct CompatFold {
    msg: AssistantMessage,
    st: StreamState,
    /// The configured price of the model asked for (even if the server names it differently).
    price: Option<ModelPricing>,
}

impl StreamFold for CompatFold {
    fn on_event(&mut self, ev: &SseEvent, out: &mut Vec<ProviderEvent>) -> Result<Step, String> {
        if ev.data.starts_with("[DONE]") {
            return Ok(Step::End);
        }
        // A payload that is not JSON (a proxy's keep-alive, a stray line) is
        // skipped, as the TS kern did; an `{error}` ends the stream.
        let Ok(chunk) = serde_json::from_str::<serde_json::Value>(&ev.data) else {
            return Ok(Step::Continue);
        };
        let chunk = openai_error(chunk)?;
        apply_chunk(&chunk, &mut self.msg, &mut self.st, out);
        Ok(Step::Continue)
    }

    fn finish(mut self, failure: Option<&str>, cancel: &CancellationToken) -> AssistantMessage {
        finish_message(&mut self.msg, self.st, failure.is_some());
        if let Some(err) = failure {
            mark_stream_failed(&mut self.msg, cancel, err);
        }
        book_priced_cost(&mut self.msg.usage, self.price.as_ref());
        self.msg
    }
}

impl kern_core::provider::Provider for CompatProvider {
    fn id(&self) -> &str {
        &self.opts.id
    }

    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let fold = CompatFold {
            msg: AssistantMessage::start(&self.opts.id, &req.model),
            st: StreamState::default(),
            price: self.opts.pricing.get(&req.model).cloned(),
        };
        drive(
            &self.transport,
            &self.retry,
            self.http_request(&req),
            req.cancel,
            fold,
        )
    }
}
