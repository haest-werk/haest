//! OpenAI Chat Completions adapter -- OpenRouter and any compatible server
//! (OpenAI itself goes through [`crate::responses`]).
//! Message conversion lives in `convert`, chunk accumulation in `stream`;
//! this file holds the provider, the request body ([`build_compat_body`]) and
//! the fold that runs each SSE event through `stream` into the answer.
//!
//! [`CompatProvider`] is the provider, configured by [`CompatOptions`]; its
//! [`CompatFlavor`] picks OpenRouter's extras or the plain body.

mod convert;
mod options;
mod stream;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_cache;
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

use haest_core::{
    cache::PrewarmRefused,
    catalog::ModelPricing,
    messages::AssistantMessage,
    provider::{
        ProviderEvent, ProviderRequest, ProviderStream, book_priced_cost, mark_stream_failed,
    },
};
use haest_http::Request;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub(crate) use convert::{flavor_extras, to_chat_messages, to_chat_tools};
use convert::{
    key_extras, mark_last_tool, openrouter_anthropic, openrouter_budget_thinking,
    reasoning_effort_extras,
};
pub use options::CompatOptions;
pub(crate) use stream::{StreamState, apply_chunk, finish_message};

use crate::{
    driver::{Step, StreamFold, drive, not_sent, openai_error},
    send::{OPENAI_URL, Sender, bearer_post, endpoint, sender_provider},
    sse::SseEvent,
    web_search::web_search_tool,
};

/// The output cap of a cache pre-warm on OpenRouter: one token. OpenRouter
/// does not honour `max_tokens: 0` (the model answers in full); at 1 the
/// pre-warm reads the cached prefix and generates one token (measured on
/// `anthropic/claude-haiku-5.5`, an 8.4k prefix: 8,412 tokens read, none
/// written).
pub(crate) const PREWARM_MAX_TOKENS: u32 = 1;

/// Which kind of Chat Completions server a [`CompatProvider`] talks to; the
/// extras it sends depend on it.
///
/// OpenAI itself is NOT a flavor here: it speaks Responses
/// ([`ResponsesProvider`](crate::ResponsesProvider)). More flavors may come
/// in a minor release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompatFlavor {
    /// OpenRouter: asks for usage with its cost, sends effort as OpenRouter's
    /// `reasoning` object, provider routing and the web search server tool,
    /// and puts cache breakpoints in for Anthropic and Google models.
    OpenRouter,
    /// Any other server: the plain body. Effort goes out only as a top-level
    /// `reasoning_effort`, when [`CompatOptions::with_reasoning_effort`] lists
    /// the levels the server takes.
    Generic,
}

/// OpenAI Chat Completions as a [`Provider`](haest_core::Provider): OpenRouter
/// and any OpenAI-compatible server (vLLM, LM Studio, Ollama, a gateway).
///
/// Each request streams with usage included. Reasoning is read from
/// `reasoning` (OpenRouter) or `reasoning_content` (DeepSeek-style servers);
/// what the server wants back (`reasoning_details`, `reasoning_content`) is
/// kept on the message and replayed to the same provider. A tool call the
/// server sends without an id gets one (`call_<hex>`). The message's cost is
/// the server's when it reports one (at or above zero: a cost below it is
/// not taken), else the price set by [`CompatOptions::with_pricing`], else none.
///
/// The prompt cache: on OpenRouter an `anthropic/` model gets every
/// breakpoint of the request's [`cache`](haest_core::ProviderRequest::cache)
/// plan with its TTL (its last function tool too), a `google/` model the
/// system prompt's and the newest message's at five minutes, any other none.
/// A request's [`routing_key`](haest_core::ProviderRequest::routing_key) goes
/// to OpenRouter as `session_id`, so its sticky routing keeps the
/// conversation on the provider holding its cache (a key over 256 characters
/// is not sent), and its plan's
/// [`partition_key`](haest_core::cache::CachePlan::partition_key) to OpenAI's
/// own API as `prompt_cache_key`; any other server gets neither. A
/// [`prewarm`](haest_core::ProviderRequest::prewarm) request to an
/// `anthropic/` model on OpenRouter goes out as the real request capped at
/// `max_tokens: 1` (OpenRouter does not honour 0); every other is refused
/// ([`check_prewarm`](Self::check_prewarm)).
///
/// # Examples
///
/// A local server, reached directly:
///
/// ```
/// use haest_core::Provider;
/// use haest_providers::{CompatOptions, CompatProvider, http::HttpClientRef};
///
/// let opts = CompatOptions::new("local", "none").with_base_url("http://localhost:1234/v1");
/// let provider = CompatProvider::new(opts, HttpClientRef::default());
/// assert_eq!(provider.id(), "local");
/// ```
///
/// Cloning is cheap: a clone shares the client.
#[derive(Debug, Clone)]
pub struct CompatProvider {
    opts: CompatOptions,
    sender: Sender,
}

sender_provider!(CompatProvider, CompatOptions);

impl CompatProvider {
    /// Sends `req` and streams the answer (the
    /// [`Provider`](haest_core::Provider) method, callable without importing
    /// the trait).
    pub fn stream(&self, req: ProviderRequest) -> ProviderStream {
        let fold = CompatFold {
            msg: AssistantMessage::start(&self.opts.id, &req.model),
            st: StreamState::default(),
            price: self.opts.pricing.get(&req.model).cloned(),
        };
        if let Some(Err(refused)) = req.prewarm.then(|| self.check_prewarm(&req)) {
            return not_sent(fold, &refused.to_string(), &req.cancel);
        }
        drive(&self.sender, self.http_request(&req), req.cancel, fold)
    }

    /// Whether [`stream`](Self::stream) sends `req` as a cache pre-warm: only
    /// to an `anthropic/` model on OpenRouter, as the real request capped at
    /// one output token (`max_tokens: 1`). OpenRouter ignores
    /// `max_tokens: 0` and lets the model answer in full; at one token it
    /// reads the prefix and writes the breakpoints as Anthropic's own
    /// pre-warm does, for the price of that token. Not for a model that
    /// thinks on a token budget at the request's effort (the Claude models
    /// before adaptive thinking): OpenRouter drops a budget the one-token cap
    /// cannot hold, so the pre-warm would cache a prompt the real requests
    /// never send. No other server or model documents one. A refused
    /// pre-warm request ends with the refusal as its error, and nothing is
    /// sent.
    ///
    /// ```
    /// use haest_core::{ProviderRequest, cache::PrewarmRefused, provider::Effort};
    /// use haest_providers::{CompatFlavor, CompatOptions, CompatProvider, http::HttpClientRef};
    ///
    /// let openrouter = CompatOptions::new("openrouter", "k").with_flavor(CompatFlavor::OpenRouter);
    /// let provider = CompatProvider::new(openrouter, HttpClientRef::default());
    /// let warm = |model: &str| ProviderRequest::new(model, vec![]).with_prewarm(true);
    /// assert_eq!(provider.check_prewarm(&warm("anthropic/claude-opus-5.5").with_effort(Effort::High)), Ok(()));
    /// assert!(matches!(
    ///     provider.check_prewarm(&warm("anthropic/claude-sonnet-4.5").with_effort(Effort::High)),
    ///     Err(PrewarmRefused::BudgetThinking { .. })
    /// ));
    /// assert!(matches!(
    ///     provider.check_prewarm(&warm("openai/gpt-6.1-sol")),
    ///     Err(PrewarmRefused::Unsupported { .. })
    /// ));
    /// ```
    ///
    /// # Errors
    ///
    /// [`PrewarmRefused::Unsupported`] for any server but OpenRouter and any
    /// model but Anthropic's; [`PrewarmRefused::BudgetThinking`] for a model
    /// on a thinking budget.
    pub fn check_prewarm(&self, req: &ProviderRequest) -> Result<(), PrewarmRefused> {
        if !openrouter_anthropic(self.opts.flavor, &req.model) {
            return Err(PrewarmRefused::Unsupported {
                provider: self.opts.id.clone(),
            });
        }
        if openrouter_budget_thinking(&req.model, req.effort) {
            return Err(PrewarmRefused::BudgetThinking {
                model: req.model.clone(),
            });
        }
        Ok(())
    }

    fn http_request(&self, req: &ProviderRequest) -> Request {
        bearer_post(
            endpoint(self.opts.base_url(), OPENAI_URL, "chat/completions"),
            &self.opts.api_key,
            &build_compat_body(&self.opts, req),
            &self.opts.headers,
        )
    }
}

/// The streaming request body for `req` under `opts`, as [`CompatProvider`] sends it.
///
/// `model`, the messages ([`to_chat_messages`]), the tools when there are any
/// ([`to_chat_tools`], the last with a cache breakpoint where the model takes
/// one ([`mark_last_tool`]), then the web search server tool when
/// `req.web_search` is set and the flavor is OpenRouter), `max_tokens` when
/// above 0 ([`PREWARM_MAX_TOKENS`] for a pre-warm, which is otherwise the
/// same body), the flavor's extras ([`flavor_extras`]), a `reasoning_effort`
/// fitted to [`CompatOptions::with_reasoning_effort`], and the routing and
/// partition keys where the server takes them ([`key_extras`]).
pub(crate) fn build_compat_body(opts: &CompatOptions, req: &ProviderRequest) -> Value {
    let mut tools = to_chat_tools(&req.tools);
    mark_last_tool(&mut tools, opts.flavor, &req.model, req.cache.tools);
    if opts.flavor == CompatFlavor::OpenRouter {
        tools.extend(req.web_search.as_ref().map(web_search_tool));
    }
    let messages = to_chat_messages(
        &req.system,
        &req.messages,
        opts.flavor,
        &req.model,
        req.cache.clone(),
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
    let cap = if req.prewarm {
        Some(PREWARM_MAX_TOKENS)
    } else {
        req.max_tokens.filter(|m| *m > 0)
    };
    if let Some(max) = cap {
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
    map.extend(key_extras(opts, req));
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
        // skipped; an `{error}` ends the stream.
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
