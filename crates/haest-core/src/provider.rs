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

use futures::{StreamExt, stream::BoxStream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub use resolve::{ModelError, ModelResolver, ResolvedModel, parse_model_spec};

use crate::messages::{AssistantMessage, Message};

crate::word_enum! {
    /// haest's reasoning levels, cheapest first. `Off` turns reasoning off
    /// where the model allows it.
    ///
    /// A request carries one as asked; the loop fits it to what the model
    /// accepts, per request. Written as its word (`"high"`) everywhere.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

/// A provider-side web search for every model call ([`WebSearchOptions::new`]
/// sets haest's caps). Only providers that run one (OpenRouter) send it;
/// the rest ignore it.
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

/// One model call, provider-neutral: everything an adapter turns into its wire
/// request.
#[derive(Debug, Clone)]
pub struct ProviderRequest {
    /// The provider's own model id (the spec without `provider:`).
    pub model: String,
    /// The system prompt.
    pub system: String,
    /// The history. The loop sends it fitted to what the model accepts
    /// (media it cannot take is withheld, with a note).
    pub messages: Vec<Message>,
    /// The tools the model may call; empty = none.
    pub tools: Vec<ToolSpec>,
    /// The turn's interrupt. When it fires, end the stream soon, with a
    /// [`ProviderEvent::Done`] whose stop reason is [`StopReason::Aborted`](crate::StopReason::Aborted).
    pub cancel: CancellationToken,
    /// None = send nothing and let the model use its own default.
    pub effort: Option<Effort>,
    /// The output cap; None = the adapter's or the model's default.
    pub max_tokens: Option<u32>,
    /// Only providers that run one (OpenRouter) send it.
    pub web_search: Option<WebSearchOptions>,
}

/// One event of a model call's stream. The deltas are for showing progress;
/// only [`ProviderEvent::Done`]'s message goes into history.
#[derive(Debug, Clone, PartialEq)]
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
/// A provider that answers every request with the last user message:
///
/// ```
/// use futures::{StreamExt, stream};
/// use haest_core::{
///     AssistantMessage, AssistantPart, Message, Provider, ProviderEvent, ProviderRequest,
///     provider::ProviderStream,
/// };
/// use tokio_util::sync::CancellationToken;
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
///         let mut msg = AssistantMessage::start(self.id(), &req.model);
///         msg.content.push(AssistantPart::Text { text: said.clone() });
///         Box::pin(stream::iter([ProviderEvent::TextDelta(said), ProviderEvent::Done(msg)]))
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let req = ProviderRequest {
///     model: "echo-1".into(),
///     system: "Repeat the user.".into(),
///     messages: vec![Message::user("hello")],
///     tools: vec![],
///     cancel: CancellationToken::new(),
///     effort: None,
///     max_tokens: None,
///     web_search: None,
/// };
/// let events: Vec<ProviderEvent> = Echo.stream(req).collect().await;
/// let Some(ProviderEvent::Done(answer)) = events.last() else { panic!("Done comes last") };
/// assert_eq!(answer.text(), "hello");
/// assert_eq!(answer.provider, "echo");
/// # }
/// ```
pub trait Provider: Send + Sync {
    /// Stable id stored on every assistant message ("anthropic", "openrouter", ...).
    fn id(&self) -> &str;
    /// Start the call. Must end with exactly one [`ProviderEvent::Done`], also on failure.
    fn stream(&self, req: ProviderRequest) -> ProviderStream;
}

pub(crate) mod adapter;
mod resolve;
#[cfg(test)]
mod tests;
