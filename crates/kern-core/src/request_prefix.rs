//! Everything a model call sends ahead of the history: system prompt, tool specs,
//! fitted effort, provider-side web search. Built in one place because two
//! callers must send the SAME prefix -- the loop, and compaction's warm summary
//! request, which only hits the prompt cache when its prefix matches byte for byte.

use tokio_util::sync::CancellationToken;

use crate::{
    deps::AgentDeps,
    effort::fit_effort,
    messages::Message,
    provider::{Effort, ProviderRequest, ToolSpec, WebSearchConfig},
};

#[derive(Debug, Clone, PartialEq)]
pub struct RequestPrefix {
    pub system: String,
    pub tools: Vec<ToolSpec>,
    pub effort: Option<Effort>,
    pub web_search: Option<WebSearchConfig>,
}

impl RequestPrefix {
    /// The model call: this prefix, then `messages`.
    pub fn request(
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
        }
    }
}

pub async fn build_request_prefix(
    deps: &AgentDeps,
    spec: &str,
    model: &str,
    effort: Option<Effort>,
) -> RequestPrefix {
    let tools = deps
        .tools
        .tools()
        .iter()
        .map(|t| ToolSpec {
            name: t.name().to_string(),
            description: t.description(),
            parameters: t.parameters(),
        })
        .collect();
    RequestPrefix {
        system: deps.system_prompt.system_prompt().await,
        tools,
        effort: fit_effort(effort, spec, model, deps.hooks.as_ref()),
        web_search: deps.web_search.clone(),
    }
}
