//! The feature switches, named and defaulted as the TypeScript kern's
//! (`host/feature-config.ts`). Garbage is refused, never read as a default.
//!   KERN_WEB_SEARCH    off | exa (default) | auto | native | parallel | firecrawl | perplexity
//!   KERN_CODE          on (default) | off -- the Code tool
//!   KERN_CODE_TOOLS    comma list of extra tool names Code may call (exact, or `prefix*`)
//!   KERN_APPROVALS     on (default) | off -- the built-in approval mod
//!   KERN_PROJECT_MODS  1 = also load <cwd>/.kern/mods (repo code; off by default)

use kern::provider::{WebSearchConfig, WebSearchEngine};

use super::{Env, get};

#[derive(Debug, Clone, PartialEq)]
pub struct Features {
    /// None = off.
    pub web_search: Option<WebSearchConfig>,
    /// None = no Code tool; Some = the extra names it may call.
    pub code: Option<Vec<String>>,
    pub approvals: bool,
    pub project_mods: bool,
}

const ENGINES: &str = "auto, native, exa, parallel, firecrawl, perplexity";

/// Unset = exa: measured 2026-10-24, every answer right on both engines and
/// native search cost 1.8x Exa on Sonnet. Results capped at 5 a search, 15 a call.
fn web_search(env: &Env) -> Result<Option<WebSearchConfig>, String> {
    let raw = get(env, "KERN_WEB_SEARCH").unwrap_or("exa");
    if raw == "off" {
        return Ok(None);
    }
    let engine: WebSearchEngine = serde_json::from_value(serde_json::Value::String(raw.into()))
        .map_err(|_| format!("KERN_WEB_SEARCH \"{raw}\" must be off or one of {ENGINES}"))?;
    Ok(Some(WebSearchConfig {
        engine,
        max_results: 5,
        max_total_results: 15,
    }))
}

fn flag(env: &Env, key: &str, on: &str, off: &str, default: bool) -> Result<bool, String> {
    match get(env, key) {
        None => Ok(default),
        Some(raw) if raw == on => Ok(true),
        Some(raw) if raw == off => Ok(false),
        Some(raw) => Err(format!("{key} \"{raw}\" must be {on} or {off}")),
    }
}

impl Features {
    pub fn from(env: &Env) -> Result<Self, String> {
        let extra = get(env, "KERN_CODE_TOOLS")
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        Ok(Self {
            web_search: web_search(env)?,
            code: flag(env, "KERN_CODE", "on", "off", true)?.then_some(extra),
            approvals: flag(env, "KERN_APPROVALS", "on", "off", true)?,
            project_mods: flag(env, "KERN_PROJECT_MODS", "1", "0", false)?,
        })
    }
}
