//! OpenAI Chat Completions adapter -- OpenRouter and any compatible server
//! (OpenAI itself goes through [`crate::responses`]).
//! Message conversion lives in `convert`, chunk accumulation in `stream`;
//! this file holds the provider, the request body ([`build_compat_body`]) and
//! the fold that runs each SSE event through `stream` into the answer.
//!
//! [`CompatProvider`] is the provider, configured by [`CompatOptions`]; its
//! [`CompatFlavor`] picks OpenRouter's extras or the plain body.

mod convert;
mod stream;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_configured;
#[cfg(test)]
mod tests_direct;
#[cfg(test)]
mod tests_routing;
#[cfg(test)]
mod tests_search;
#[cfg(test)]
mod tests_stream;

use std::{collections::HashMap, fmt, sync::Arc, time::Duration};

use haest_core::{
    catalog::{ModelPricing, Reasoning},
    messages::AssistantMessage,
    provider::{
        ProviderEvent, ProviderRequest, ProviderStream, book_priced_cost, mark_stream_failed,
    },
};
use haest_http::{HttpClient, Request};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use convert::reasoning_effort_extras;
pub(crate) use convert::{flavor_extras, to_chat_messages, to_chat_tools};
pub(crate) use stream::{StreamState, apply_chunk, finish_message};

use crate::{
    driver::{Step, StreamFold, drive, openai_error},
    retry::RetryPolicy,
    routing::RoutingScopes,
    send::{HeaderNames, OPENAI_URL, REDACTED, Sender, bearer_post, endpoint},
    sse::SseEvent,
    web_search::web_search_tool,
};

/// Which kind of Chat Completions server a [`CompatProvider`] talks to; the
/// extras it sends depend on it.
///
/// OpenAI itself is NOT a flavor here: it speaks Responses
/// ([`ResponsesProvider`](crate::ResponsesProvider)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatFlavor {
    /// OpenRouter: asks for usage with its cost, sends effort as OpenRouter's
    /// `reasoning` object, provider routing and the web search server tool,
    /// and puts cache breakpoints in for Anthropic and Google models.
    OpenRouter,
    /// Any other server: the plain body. Effort goes out only as a top-level
    /// `reasoning_effort`, when [`CompatOptions::reasoning_effort`] lists the
    /// levels the server takes.
    Generic,
}

/// How a [`CompatProvider`] reaches its server and what it sends.
///
/// [`CompatOptions::new`] takes the two fields every provider needs and
/// leaves the rest plain ([`CompatFlavor::Generic`], no extras); set the
/// others on the value it returns. Its Debug output shows neither the key
/// nor a header's value.
#[derive(Clone)]
pub struct CompatOptions {
    /// The provider's name: its [`Provider::id`](haest_core::Provider::id),
    /// stored on every message it answers. Reasoning replays only to the
    /// provider whose id a message carries.
    pub id: String,
    /// OpenRouter's extras, or the plain body ([`CompatFlavor::Generic`], the default).
    pub flavor: CompatFlavor,
    /// Sent as `Authorization: Bearer <api_key>`.
    pub api_key: String,
    /// None = `https://api.openai.com/v1`; requests go to `<base_url>/chat/completions`.
    pub base_url: Option<String>,
    /// Sent on every request, after (and over) the defaults.
    pub headers: Vec<(String, String)>,
    /// A provider that takes a top-level `reasoning_effort`: the levels it accepts
    /// (haest's effort is fitted to the nearest; off only when it lists `none`).
    /// None = no `reasoning_effort` is sent.
    pub reasoning_effort: Option<Reasoning>,
    /// Price per model id, for a provider whose API returns no cost (haest-core `pricing`).
    pub pricing: HashMap<String, ModelPricing>,
    /// OpenRouter only: provider routing, resolved per request against the request's model.
    /// None = off: send no `provider` object (a host gateway may decide routing itself).
    pub routing: Option<RoutingScopes>,
}

impl CompatOptions {
    /// The provider `id`, sending `api_key` to OpenAI's own URL with the plain
    /// body ([`CompatFlavor::Generic`]) and nothing else configured.
    pub fn new(id: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            flavor: CompatFlavor::Generic,
            api_key: api_key.into(),
            base_url: None,
            headers: Vec::new(),
            reasoning_effort: None,
            pricing: HashMap::new(),
            routing: None,
        }
    }
}

impl fmt::Debug for CompatOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompatOptions")
            .field("id", &self.id)
            .field("flavor", &self.flavor)
            .field("api_key", &REDACTED)
            .field("base_url", &self.base_url)
            .field("headers", &HeaderNames(&self.headers))
            .field("reasoning_effort", &self.reasoning_effort)
            .field("pricing", &self.pricing)
            .field("routing", &self.routing)
            .finish()
    }
}

/// OpenAI Chat Completions as a [`Provider`](haest_core::Provider): OpenRouter
/// and any OpenAI-compatible server (vLLM, LM Studio, Ollama, a gateway).
///
/// Each request streams with usage included. Reasoning is read from
/// `reasoning` (OpenRouter) or `reasoning_content` (DeepSeek-style servers);
/// what the server wants back (`reasoning_details`, `reasoning_content`) is
/// kept on the message and replayed to the same provider. A tool call the
/// server sends without an id gets one (`call_<hex>`). The message's cost is
/// the server's when it reports one, else the price in
/// [`CompatOptions::pricing`], else none.
///
/// # Examples
///
/// A local server, reached directly:
///
/// ```
/// use std::sync::Arc;
///
/// use haest_core::Provider;
/// use haest_providers::{CompatOptions, CompatProvider, http::ReqwestClient};
///
/// let mut opts = CompatOptions::new("local", "none");
/// opts.base_url = Some("http://localhost:1234/v1".into());
/// let provider = CompatProvider::new(opts, Arc::new(ReqwestClient::new()));
/// assert_eq!(provider.id(), "local");
/// ```
pub struct CompatProvider {
    opts: CompatOptions,
    sender: Sender,
}

impl CompatProvider {
    /// A provider sending over `http` with the default [`RetryPolicy`].
    /// Nothing is sent until [`Provider::stream`](haest_core::Provider::stream).
    pub fn new(opts: CompatOptions, http: Arc<dyn HttpClient>) -> Self {
        Self {
            opts,
            sender: Sender::new(http),
        }
    }

    /// The same provider with `retry` instead of the default policy.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.sender.retry = retry;
        self
    }

    /// The same provider waiting at most `wait` for each try's status and
    /// headers (default ten minutes); past it the try counts as unanswered,
    /// as [`AnthropicProvider::with_headers_timeout`](crate::AnthropicProvider::with_headers_timeout) says.
    pub fn with_headers_timeout(mut self, wait: Duration) -> Self {
        self.sender.headers_timeout = wait;
        self
    }

    fn http_request(&self, req: &ProviderRequest) -> Request {
        bearer_post(
            endpoint(&self.opts.base_url, OPENAI_URL, "chat/completions"),
            &self.opts.api_key,
            &build_compat_body(&self.opts, req),
            &self.opts.headers,
        )
    }
}

/// The streaming request body for `req` under `opts`, as [`CompatProvider`] sends it.
///
/// `model`, the messages ([`to_chat_messages`]), the tools when there are any
/// ([`to_chat_tools`], then the web search server tool when `req.web_search`
/// is set and the flavor is OpenRouter), `max_tokens` when above 0, the
/// flavor's extras ([`flavor_extras`])
/// and a `reasoning_effort` fitted to [`CompatOptions::reasoning_effort`].
pub(crate) fn build_compat_body(opts: &CompatOptions, req: &ProviderRequest) -> Value {
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
        // skipped, as the TS haest did; an `{error}` ends the stream.
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

impl haest_core::provider::Provider for CompatProvider {
    fn id(&self) -> &str {
        &self.opts.id
    }

    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let fold = CompatFold {
            msg: AssistantMessage::start(&self.opts.id, &req.model),
            st: StreamState::default(),
            price: self.opts.pricing.get(&req.model).cloned(),
        };
        drive(&self.sender, self.http_request(&req), req.cancel, fold)
    }
}
