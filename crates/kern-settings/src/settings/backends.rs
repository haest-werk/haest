//! The backends a setting points a tool at: the provider's web search engine,
//! GenerateImage's OpenRouter config.

use std::{collections::HashMap, fmt};

use kern_core::provider::{WebSearchConfig, WebSearchEngine};
use kern_tools::image::{DEFAULT_IMAGE_BASE_URL, DEFAULT_IMAGE_MODELS};

use super::{Read, env_var};
use crate::value::split_list;

/// What GenerateImage runs on: OpenRouter's Image API with the host's key.
#[derive(Clone, PartialEq)]
pub struct ImageConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// Every model a call may pick, the default first.
    pub models: Vec<String>,
}

impl fmt::Debug for ImageConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageConfig")
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("models", &self.models)
            .finish()
    }
}

/// `web_search`: None = off.
pub(super) fn web_search(s: &Read) -> Result<Option<WebSearchConfig>, String> {
    let engine = match s.text("web_search").as_deref() {
        None | Some("off") => return Ok(None),
        Some("auto") => WebSearchEngine::Auto,
        Some("native") => WebSearchEngine::Native,
        Some("exa") => WebSearchEngine::Exa,
        Some("parallel") => WebSearchEngine::Parallel,
        Some("firecrawl") => WebSearchEngine::Firecrawl,
        Some("perplexity") => WebSearchEngine::Perplexity,
        Some(other) => return Err(format!("unknown web search engine \"{other}\"")),
    };
    Ok(Some(WebSearchConfig {
        engine,
        max_results: 5,
        max_total_results: 15,
    }))
}

/// GenerateImage's config; None without an OpenRouter key.
pub(super) fn image(s: &Read, env: &HashMap<String, String>) -> Option<ImageConfig> {
    let api_key = env_var(env, "OPENROUTER_API_KEY")?.to_string();
    let model = s.text("image_model")?;
    let others: Vec<String> = match s.0.get("image_models") {
        Some(raw) => split_list(raw),
        None => DEFAULT_IMAGE_MODELS.map(String::from).to_vec(),
    };
    let models = std::iter::once(model.clone())
        .chain(others.into_iter().filter(|m| *m != model))
        .collect();
    Some(ImageConfig {
        api_key,
        base_url: env_var(env, "OPENROUTER_BASE_URL")
            .unwrap_or(DEFAULT_IMAGE_BASE_URL)
            .to_string(),
        model,
        models,
    })
}
