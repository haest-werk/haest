//! The seam between the loop and a model API. An adapter turns a neutral
//! request into its wire format and yields neutral stream events, ending with
//! exactly one `Done` carrying the complete assistant message. Adapters never
//! fail mid-stream: an API error ends with `Done` and `StopReason::Error`, so the
//! loop has one code path for every failure.
//!
//! PLUG POINT: implement [`Provider`] for a new model API, or [`ModelResolver`]
//! to decide which provider serves a `provider:model` spec.

use std::{fmt, sync::Arc};

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::messages::{AssistantMessage, Message, StopReason};

crate::word_enum! {
    /// kern's reasoning levels. `Off` turns reasoning off where the model allows it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum Effort ("effort") {
        Off = "off",
        Low = "low",
        Medium = "medium",
        High = "high",
        Xhigh = "xhigh",
        Max = "max",
    }
}

/// A tool as the model sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the input object.
    pub parameters: Value,
}

crate::word_enum! {
    /// A provider-side web search (OpenRouter's `openrouter:web_search` server tool).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum WebSearchEngine ("web search engine") {
        /// The model's native search where it has one, else Exa.
        Auto = "auto",
        Native = "native",
        Exa = "exa",
        Parallel = "parallel",
        Firecrawl = "firecrawl",
        Perplexity = "perplexity",
    }
}

impl WebSearchConfig {
    /// `engine` with kern's result caps: 5 a search, 15 across one model call.
    pub fn new(engine: WebSearchEngine) -> Self {
        Self {
            engine,
            max_results: 5,
            max_total_results: 15,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchConfig {
    pub engine: WebSearchEngine,
    /// Results per search.
    pub max_results: u32,
    /// Results across every search of one model call: the cost and context bound.
    pub max_total_results: u32,
}

/// One model call.
#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub cancel: CancellationToken,
    /// None = send nothing and let the model use its own default.
    pub effort: Option<Effort>,
    pub max_tokens: Option<u32>,
    /// Only providers that run one (OpenRouter) send it.
    pub web_search: Option<WebSearchConfig>,
}

/// What a provider streams back.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    TextDelta(String),
    ThinkingDelta(String),
    /// A tool call began in the stream: the text before it was narration.
    ToolCallStart {
        id: String,
        name: String,
    },
    /// Always last, exactly once.
    Done(AssistantMessage),
}

pub type ProviderStream = BoxStream<'static, ProviderEvent>;

/// A model API.
pub trait Provider: Send + Sync {
    /// Stable id stored on every assistant message ("anthropic", "openrouter", ...).
    fn id(&self) -> &str;
    /// Must end with exactly one [`ProviderEvent::Done`], also on failure.
    fn stream(&self, req: ProviderRequest) -> ProviderStream;
}

/// A spec resolved to the provider that serves it and the provider's model id.
#[derive(Clone)]
pub struct ResolvedModel {
    pub provider: Arc<dyn Provider>,
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

/// `provider:model` -> a live provider. The error is shown to the human as is.
pub trait ModelResolver: Send + Sync {
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, String>;
}

impl<F> ModelResolver for F
where
    F: Fn(&str) -> Result<ResolvedModel, String> + Send + Sync,
{
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, String> {
        self(spec)
    }
}

/// `openrouter:anthropic/claude-sonnet-5` -> ("openrouter", "anthropic/claude-sonnet-5").
pub fn parse_model_spec(spec: &str) -> Result<(&str, &str), String> {
    match spec.find(':') {
        Some(i) if i > 0 => Ok((&spec[..i], &spec[i + 1..])),
        _ => Err(format!(
            "model \"{spec}\" must be \"<provider>:<model>\", e.g. openrouter:anthropic/claude-sonnet-5"
        )),
    }
}

/// A stream that failed: `Aborted` when the caller cancelled, else `Error` with the reason.
pub fn mark_stream_failed(
    msg: &mut AssistantMessage,
    cancel: &CancellationToken,
    err: impl fmt::Display,
) {
    if cancel.is_cancelled() {
        msg.stop_reason = StopReason::Aborted;
    } else {
        msg.stop_reason = StopReason::Error;
        msg.error_message = Some(err.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_splits_on_the_first_colon_only() {
        assert_eq!(
            parse_model_spec("openrouter:x/y:free").unwrap(),
            ("openrouter", "x/y:free")
        );
        assert!(parse_model_spec("no-provider").is_err());
        assert!(parse_model_spec(":model").is_err());
    }

    #[test]
    fn effort_parses_its_own_names() {
        for &e in Effort::ALL {
            assert_eq!(e.as_str().parse::<Effort>().unwrap(), e);
        }
        assert!("extreme".parse::<Effort>().is_err());
    }
}
