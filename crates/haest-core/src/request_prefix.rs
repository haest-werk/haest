//! Everything a model call sends ahead of the history: system prompt, tool specs,
//! fitted effort, provider-side web search. Built in one place because two
//! callers must send the SAME prefix -- the loop, and compaction's warm summary
//! request, which only hits the prompt cache when its prefix matches byte for byte.

use tokio_util::sync::CancellationToken;

use crate::{
    cache::CachePlan,
    deps::AgentDeps,
    effort::fit_effort,
    messages::Message,
    provider::{Effort, ProviderRequest, ToolSpec, WebSearchOptions},
};

/// The part of a [`ProviderRequest`] that comes before the history.
/// [`Agent::request_prefix`](crate::Agent::request_prefix) builds it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RequestPrefix {
    /// The system prompt.
    pub(crate) system: String,
    /// The advertised tools, as the model sees them.
    pub(crate) tools: Vec<ToolSpec>,
    /// The effort fitted to the model; None sends none.
    pub(crate) effort: Option<Effort>,
    /// Provider-side web search; None = off.
    pub(crate) web_search: Option<WebSearchOptions>,
    /// The provider's prompt-cache routing key; None sends none.
    pub(crate) cache_key: Option<String>,
}

impl RequestPrefix {
    /// The model call: this prefix, then `messages`, with no `max_tokens`
    /// and the default cache plan, which the agent then plans
    /// (`Agent::plan_cache`).
    pub(crate) fn request(
        self,
        model: String,
        messages: Vec<Message>,
        cancel: CancellationToken,
    ) -> ProviderRequest {
        ProviderRequest {
            model,
            system: self.system,
            messages,
            tools: self.tools,
            cancel,
            effort: self.effort,
            max_tokens: None,
            web_search: self.web_search,
            cache: CachePlan::default(),
            cache_key: self.cache_key,
            prewarm: false,
        }
    }
}

/// The prefix for one call of `model` (the provider's id) under `spec`: the
/// system prompt built now, the tools [`ToolSource::tools`](crate::ToolSource::tools)
/// advertises now, `effort` fitted to what the model takes, and the deps'
/// web search; no cache key, which the agent sets.
pub(crate) async fn build_request_prefix(
    deps: &AgentDeps,
    spec: &str,
    model: &str,
    effort: Option<Effort>,
) -> RequestPrefix {
    let tools = deps
        .tools()
        .tools()
        .iter()
        .map(|t| ToolSpec {
            name: t.name().to_string(),
            description: t.description(),
            parameters: t.parameters(),
        })
        .collect();
    RequestPrefix {
        system: deps.system_prompt().system_prompt().await,
        tools,
        effort: fit_effort(
            effort,
            spec,
            model,
            deps.shared().facts().as_ref(),
            deps.shared().hooks().as_ref(),
        ),
        web_search: deps.shared().web_search().cloned(),
        cache_key: None,
    }
}
