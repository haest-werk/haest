//! What OpenRouter's web search server tool leaves in a Chat Completions stream:
//! `url_citation` annotations on the deltas (the pages it cited) and
//! `usage.server_tool_use_details.web_search_requests` (how many searches ran).
//! There is no tool-call event and no query -- the search happens inside the call.

use haest_core::{
    messages::{WebSearchUse, WebSource},
    provider::WebSearchOptions,
};
use serde_json::{Value, json};

use crate::driver::text_of;

#[derive(Debug, Default)]
pub(crate) struct SearchAcc {
    /// Cited pages by URL, in first-cited order.
    sources: Vec<WebSource>,
    searches: u32,
}

impl SearchAcc {
    pub(crate) fn apply_annotations(&mut self, annotations: Option<&Value>) {
        let Some(list) = annotations.and_then(Value::as_array) else {
            return;
        };
        for a in list {
            if a.get("type").and_then(Value::as_str) != Some("url_citation") {
                continue;
            }
            let cite = a.get("url_citation").unwrap_or(&Value::Null);
            let Some(url) = text_of(cite.get("url")) else {
                continue;
            };
            if self.sources.iter().any(|s| s.url == url) {
                continue;
            }
            self.sources.push(WebSource {
                url: url.to_string(),
                title: text_of(cite.get("title")).map(str::to_string),
            });
        }
    }

    pub(crate) fn apply_usage(&mut self, usage: &Value) {
        let n = usage
            .pointer("/server_tool_use_details/web_search_requests")
            .and_then(Value::as_f64);
        if let Some(n) = n {
            self.searches = n.max(0.0) as u32;
        }
    }

    /// The message's record of the search, or None when nothing was searched or cited.
    pub(crate) fn into_use(self) -> Option<WebSearchUse> {
        if self.searches == 0 && self.sources.is_empty() {
            return None;
        }
        Some(WebSearchUse {
            searches: self.searches,
            sources: self.sources,
        })
    }
}

/// OpenRouter's web search SERVER tool (openrouter.ai/docs/guides/features/server-tools/web-search):
/// the model decides when to search, OpenRouter runs it inside the same call and
/// bills it into `usage.cost`. Last in the list, after the function tools, so a
/// config change never shifts the cached prefix of the ones before it.
pub(crate) fn web_search_tool(cfg: &WebSearchOptions) -> Value {
    json!({
        "type": "openrouter:web_search",
        "parameters": {
            "engine": cfg.engine,
            "max_results": cfg.max_results,
            "max_total_results": cfg.max_total_results,
        },
    })
}
