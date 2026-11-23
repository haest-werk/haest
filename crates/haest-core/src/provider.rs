//! The seam between the loop and a model API. An adapter turns a neutral
//! request ([`ProviderRequest`]) into its wire format and yields neutral stream
//! events ([`ProviderEvent`]), ending with exactly one
//! [`Done`](ProviderEvent::Done) carrying the complete assistant message.
//! Adapters never fail mid-stream: an API error ends with `Done` and
//! [`StopReason::Error`](crate::StopReason::Error), so the loop has one code path for every failure.
//!
//! PLUG POINT: implement [`Provider`] for a new model API, or [`ModelResolver`]
//! to decide which provider serves a `provider:model` spec. `haest-providers`
//! has the adapters (Anthropic Messages, OpenAI Chat Completions for
//! OpenRouter and any compatible server, OpenAI Responses) and a registry
//! that resolves specs; `testing::ScriptedProvider` (the `testing` feature)
//! answers from a script, for tests.
//!
//! Also here: the request's parts ([`ToolSpec`], [`Effort`],
//! [`WebSearchOptions`]) and [`parse_model_spec`], which adapters share.
//!
//! # Writing a provider
//!
//! What every adapter in `haest-providers` uses, for one of your own:
//!
//! - [`mark_stream_failed`]: the `Done` of a call that failed or was aborted.
//! - [`nearest_accepted_effort`]: the reasoning level the model's catalog row
//!   accepts, for an API that refuses any level it does not list.
//! - [`book_priced_cost`]: a cost from the configured price, for an API that
//!   returns none.
//! - [`Provider::check_prewarm`]: whether a request can go out as a cache
//!   pre-warm ([`ProviderRequest::prewarm`]); the default says it cannot.
//! - [`data_url`](crate::media::data_url) and
//!   [`dropped_note`](crate::media::dropped_note): media the wire format
//!   carries as a `data:` URL, or cannot carry at all.

use futures::{StreamExt, stream::BoxStream};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use adapter::mark_stream_failed;
pub use answer::{Deltas, answer_stream};
pub use request::ProviderRequest;
pub use resolve::{ModelError, ModelResolver, ModelResolverRef, ResolvedModel, parse_model_spec};

pub use crate::{effort::nearest_accepted_effort, pricing::book_priced_cost};

use crate::{cache::PrewarmRefused, messages::AssistantMessage};

crate::word_enum! {
    /// haest's reasoning levels, cheapest first. `Off` turns reasoning off
    /// where the model allows it.
    ///
    /// A request carries one as asked; the loop fits it to what the model
    /// accepts, per request. Written as its word (`"high"`) everywhere.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
    #[non_exhaustive]
    pub enum Effort ("effort") {
        /// No reasoning, where the model can stop.
        Off = "off",
        /// The least reasoning.
        Low = "low",
        /// More than `low`.
        Medium = "medium",
        /// More than `medium`.
        High = "high",
        /// More than `high`.
        Xhigh = "xhigh",
        /// The most the model does.
        Max = "max",
    }
}

/// A tool as the model sees it: what a provider puts in the request's tool list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// The name the model calls it by.
    pub name: String,
    /// What it does and when to use it, for the model.
    pub description: String,
    /// JSON Schema of the input object.
    pub parameters: Value,
}

crate::word_enum! {
    /// A provider-side web search (OpenRouter's `openrouter:web_search` server tool).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum WebSearchEngine ("web search engine") {
        /// The model's native search where it has one, else Exa.
        Auto = "auto",
        /// The model vendor's own search.
        Native = "native",
        /// Exa.
        Exa = "exa",
        /// Parallel.
        Parallel = "parallel",
        /// Firecrawl.
        Firecrawl = "firecrawl",
        /// Perplexity.
        Perplexity = "perplexity",
    }
}

impl WebSearchOptions {
    /// `engine` with haest's result caps: 5 a search, 15 across one model call.
    pub fn new(engine: WebSearchEngine) -> Self {
        Self {
            engine,
            max_results: 5,
            max_total_results: 15,
        }
    }
}

/// [`WebSearchEngine::Auto`] with haest's result caps, as
/// [`new`](WebSearchOptions::new) sets them.
impl Default for WebSearchOptions {
    fn default() -> Self {
        Self::new(WebSearchEngine::Auto)
    }
}

/// A provider-side web search for every model call ([`WebSearchOptions::new`]
/// sets haest's caps). Only providers that run one (OpenRouter) send it;
/// the rest ignore it.
///
/// Plain data: set the fields a host cares about and take the rest from
/// [`Default`], so a setting added later breaks no host.
///
/// ```
/// use haest_core::provider::{WebSearchEngine, WebSearchOptions};
///
/// let exa = WebSearchOptions {
///     engine: WebSearchEngine::Exa,
///     max_total_results: 10,
///     ..WebSearchOptions::default()
/// };
/// assert_eq!(exa.max_results, 5);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchOptions {
    /// Which search engine runs it.
    pub engine: WebSearchEngine,
    /// Results per search.
    pub max_results: u32,
    /// Results across every search of one model call: the cost and context bound.
    pub max_total_results: u32,
}

/// One event of a model call's stream. The deltas are for showing progress;
/// only [`ProviderEvent::Done`]'s message goes into history.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ProviderEvent {
    /// Answer text as it arrives.
    TextDelta(String),
    /// Reasoning text as it arrives, where the provider shows any.
    ThinkingDelta(String),
    /// A tool call began in the stream: the text before it was narration.
    ToolCallStart {
        /// The call's id, as in the final [`ToolCall`](crate::ToolCall).
        id: String,
        /// The tool's name.
        name: String,
    },
    /// Always last, exactly once: the whole assistant message, its usage and
    /// its stop reason, also when the call failed.
    Done(AssistantMessage),
}

/// What [`Provider::stream`] returns: a boxed, `'static`, `Send` stream of
/// [`ProviderEvent`]s.
pub type ProviderStream = BoxStream<'static, ProviderEvent>;

/// Why a model call has no answer when its stream ended without a
/// [`ProviderEvent::Done`].
pub(crate) const NO_RESPONSE: &str = "provider ended without a response";

/// The answer `stream` ends with, every event before it handed to `delta`;
/// None when the stream ended without a [`ProviderEvent::Done`] ([`NO_RESPONSE`]).
pub(crate) async fn final_message(
    mut stream: ProviderStream,
    mut delta: impl FnMut(ProviderEvent),
) -> Option<AssistantMessage> {
    let mut done = None;
    while let Some(ev) = stream.next().await {
        match ev {
            ProviderEvent::Done(m) => done = Some(m),
            other => delta(other),
        }
    }
    done
}

/// A model API: turns a [`ProviderRequest`] into a stream of
/// [`ProviderEvent`]s.
///
/// The contract:
/// - [`Provider::stream`] returns at once; the work runs as the stream is
///   polled.
/// - The stream ends with exactly one [`ProviderEvent::Done`], also on
///   failure: an API or network error is a `Done` with
///   [`StopReason::Error`](crate::StopReason::Error) and the reason in
///   `error_message`, never a panic or a stream that stops short. A stream
///   that ends without `Done` reaches history as an error message.
/// - The loop reads the stream to its end and does not watch `req.cancel`
///   itself: when it fires, the adapter ends the stream with a `Done` whose
///   stop reason is [`StopReason::Aborted`](crate::StopReason::Aborted).
///
/// # Examples
///
/// A provider that answers every request with the last user message, built
/// with [`answer_stream`], which ends the stream with the answer the async
/// block returns:
///
/// ```
/// use haest_core::{
///     AssistantMessage, AssistantPart, Message, Provider, ProviderEvent, ProviderRequest,
///     ProviderStream, answer_stream,
/// };
///
/// struct Echo;
///
/// impl Provider for Echo {
///     fn id(&self) -> &str {
///         "echo"
///     }
///
///     fn stream(&self, req: ProviderRequest) -> ProviderStream {
///         let said = match req.messages.last() {
///             Some(Message::User(u)) => u.content.clone(),
///             _ => String::new(),
///         };
///         let mut answer = AssistantMessage::start(self.id(), &req.model);
///         answer_stream(|deltas| async move {
///             // A real provider sends each piece as its API streams it.
///             deltas.text(said.as_str());
///             answer.content.push(AssistantPart::Text { text: said });
///             answer
///         })
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// use futures::StreamExt;
///
/// let req = ProviderRequest::new("echo-1", vec![Message::user("hello")])
///     .with_system("Repeat the user.");
/// let events: Vec<ProviderEvent> = Echo.stream(req).collect().await;
/// assert_eq!(events[0], ProviderEvent::TextDelta("hello".into()));
/// let Some(ProviderEvent::Done(answer)) = events.last() else {
///     return Err("Done comes last".into());
/// };
/// assert_eq!((answer.text(), answer.provider.as_str()), ("hello".into(), "echo"));
/// # Ok(())
/// # }
/// ```
pub trait Provider: Send + Sync {
    /// Stable id stored on every assistant message ("anthropic", "openrouter", ...).
    fn id(&self) -> &str;
    /// Start the call. Must end with exactly one [`ProviderEvent::Done`], also on failure.
    fn stream(&self, req: ProviderRequest) -> ProviderStream;

    /// Whether [`stream`](Self::stream) would send `req` as the cache
    /// pre-warm it asks for ([`ProviderRequest::prewarm`]): `Ok` when it
    /// would, why not when it would refuse (and send nothing).
    ///
    /// The default refuses every request ([`PrewarmRefused::Unsupported`]):
    /// a provider that sends pre-warms says so here, and its `stream` ends a
    /// pre-warm it refuses with that refusal as its error.
    ///
    /// # Errors
    ///
    /// The [`PrewarmRefused`] that says why `req` cannot be a pre-warm here.
    fn check_prewarm(&self, _req: &ProviderRequest) -> Result<(), PrewarmRefused> {
        Err(PrewarmRefused::Unsupported {
            provider: self.id().to_string(),
        })
    }
}

crate::plug_ref! {
    /// A [`Provider`], shared: what a [`ResolvedModel`] holds. Built from the
    /// provider itself, or from an `Arc` of one a resolver keeps.
    pub struct ProviderRef(dyn Provider);
    debug(provider, f) { f.debug_tuple("ProviderRef").field(&provider.id()).finish() }
}

mod adapter;
mod answer;
mod request;
mod resolve;
#[cfg(test)]
mod tests;
