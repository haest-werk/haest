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
//! [`WebSearchConfig`]) and [`parse_model_spec`], which adapters share.

use std::{fmt, sync::Arc};

use futures::{StreamExt, stream::BoxStream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

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

impl WebSearchConfig {
    /// `engine` with haest's result caps: 5 a search, 15 across one model call.
    pub fn new(engine: WebSearchEngine) -> Self {
        Self {
            engine,
            max_results: 5,
            max_total_results: 15,
        }
    }
}

/// A provider-side web search for every model call ([`WebSearchConfig::new`]
/// sets haest's caps). Only providers that run one (OpenRouter) send it;
/// the rest ignore it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchConfig {
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
    pub web_search: Option<WebSearchConfig>,
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

/// A spec resolved to the provider that serves it and the provider's model id.
#[derive(Clone)]
pub struct ResolvedModel {
    /// The provider that serves the spec.
    pub provider: Arc<dyn Provider>,
    /// The id that provider knows the model by (what [`ProviderRequest::model`] gets).
    pub model: String,
}

impl fmt::Debug for ResolvedModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedModel")
            .field("provider", &self.provider.id())
            .field("model", &self.model)
            .finish()
    }
}

/// Why a model spec cannot be served: what [`ModelResolver::resolve`] and
/// [`parse_model_spec`] refuse. The loop shows its text to the human.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// The spec is not `<provider>:<model>`.
    #[error(
        "model \"{spec}\" must be \"<provider>:<model>\", e.g. openrouter:anthropic/claude-sonnet-5"
    )]
    BadSpec {
        /// The spec, as written.
        spec: String,
    },
    /// No provider goes by the spec's provider part.
    #[error("unknown provider \"{provider}\". Known: {}", known.join(", "))]
    UnknownProvider {
        /// The provider part, as written.
        provider: String,
        /// The providers there are.
        known: Vec<String>,
    },
    /// The provider is known but cannot be built: a key it needs is not set,
    /// its configuration is bad. Its text is the resolver's own.
    #[error(transparent)]
    Unavailable(Box<dyn std::error::Error + Send + Sync>),
}

/// Turns a `provider:model` spec into a live provider. The loop asks it before
/// every model call, so a model switch mid-conversation takes effect at once.
///
/// Any `Fn(&str) -> Result<ResolvedModel, ModelError>` is one; `haest-providers`'
/// registry is the full one.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use haest_core::{ModelError, ModelResolver, Provider, ResolvedModel, provider::parse_model_spec};
/// # use haest_core::{ProviderRequest, provider::ProviderStream};
/// # struct Echo;
/// # impl Provider for Echo {
/// #     fn id(&self) -> &str { "echo" }
/// #     fn stream(&self, _: ProviderRequest) -> ProviderStream { unimplemented!() }
/// # }
///
/// let echo: Arc<dyn Provider> = Arc::new(Echo);
/// let resolver = move |spec: &str| match parse_model_spec(spec)? {
///     ("echo", model) => Ok(ResolvedModel { provider: echo.clone(), model: model.into() }),
///     (other, _) => Err(ModelError::UnknownProvider {
///         provider: other.into(),
///         known: vec!["echo".into()],
///     }),
/// };
///
/// assert_eq!(resolver.resolve("echo:echo-1").unwrap().model, "echo-1");
/// assert!(matches!(
///     resolver.resolve("acme:big"),
///     Err(ModelError::UnknownProvider { provider, .. }) if provider == "acme"
/// ));
/// ```
pub trait ModelResolver: Send + Sync {
    /// The provider and model id for `spec`.
    ///
    /// # Errors
    ///
    /// [`ModelError`]: a bad spec, an unknown provider, one that cannot be
    /// built (a missing key).
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, ModelError>;
}

impl<F> ModelResolver for F
where
    F: Fn(&str) -> Result<ResolvedModel, ModelError> + Send + Sync,
{
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, ModelError> {
        self(spec)
    }
}

/// Splits a model spec into provider and model id at the FIRST colon:
/// `openrouter:anthropic/claude-sonnet-5` -> ("openrouter", "anthropic/claude-sonnet-5"),
/// `openrouter:x/y:free` -> ("openrouter", "x/y:free").
///
/// # Errors
///
/// [`ModelError::BadSpec`] when there is no colon or nothing before it.
pub fn parse_model_spec(spec: &str) -> Result<(&str, &str), ModelError> {
    match spec.find(':') {
        Some(i) if i > 0 => Ok((&spec[..i], &spec[i + 1..])),
        _ => Err(ModelError::BadSpec { spec: spec.into() }),
    }
}

pub(crate) mod adapter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_splits_on_the_first_colon_only() {
        assert_eq!(
            parse_model_spec("openrouter:x/y:free").unwrap(),
            ("openrouter", "x/y:free")
        );
        for bad in ["no-provider", ":model"] {
            assert!(matches!(
                parse_model_spec(bad),
                Err(ModelError::BadSpec { spec }) if spec == bad
            ));
        }
    }
}
