//! Everything a model call sends ahead of the history: system prompt, tool specs,
//! fitted effort, provider-side web search. Built in one place because two
//! callers must send the SAME prefix -- the loop, and compaction's warm summary
//! request, which only hits the prompt cache when its prefix matches byte for byte.
//!
//! PORT (T3): fit the effort through effort.rs (`AgentHooks::reasoning`, `on_effort_fitted`).

use crate::{
    deps::AgentDeps,
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
    _spec: &str,
    _model: &str,
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
        effort,
        web_search: deps.web_search.clone(),
    }
}
