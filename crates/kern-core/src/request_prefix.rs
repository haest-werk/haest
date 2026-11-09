//! Everything a model call sends ahead of the history: system prompt, tool specs,
//! fitted effort, provider-side web search. Built in one place because two
//! callers must send the SAME prefix -- the loop, and compaction's warm summary
//! request, which only hits the prompt cache when its prefix matches byte for byte.

use crate::{
    deps::AgentDeps,
    effort::fit_effort,
    provider::{Effort, ToolSpec, WebSearchConfig},
};

#[derive(Debug, Clone, PartialEq)]
pub struct RequestPrefix {
    pub system: String,
    pub tools: Vec<ToolSpec>,
    pub effort: Option<Effort>,
    pub web_search: Option<WebSearchConfig>,
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
