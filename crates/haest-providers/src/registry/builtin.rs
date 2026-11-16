//! The built-in providers' options: each asks the host's [`KeySource`] for its
//! key and its base URL, under the standard variable names.

use haest_core::{
    __private::openrouter_base_url,
    KeySource, ModelError,
    catalog::{OpenRouterEffort, Reasoning},
};

use super::RegistryOptions;
use crate::{
    anthropic::AnthropicOptions,
    catalog::Catalog,
    compat::{CompatFlavor, CompatOptions},
    responses::ResponsesOptions,
};

/// The key `key`, or the error that names it.
fn need(keys: &dyn KeySource, key: &str, provider: &str) -> Result<String, ModelError> {
    keys.key(key).ok_or_else(|| ModelError::MissingKey {
        provider: provider.into(),
        key: key.into(),
    })
}

/// `anthropic`: the Messages API at `ANTHROPIC_BASE_URL` (else Anthropic's),
/// with the key `ANTHROPIC_API_KEY`.
pub(super) fn anthropic_options(keys: &dyn KeySource) -> Result<AnthropicOptions, ModelError> {
    let mut opts =
        AnthropicOptions::new("anthropic", need(keys, "ANTHROPIC_API_KEY", "anthropic")?);
    opts.base_url = keys.key("ANTHROPIC_BASE_URL");
    Ok(opts)
}

/// `openai`: the Responses API at `OPENAI_BASE_URL` (else OpenAI's), with the
/// key `OPENAI_API_KEY`. Responses, not Chat Completions: OpenAI refuses
/// function tools + reasoning effort there.
pub(super) fn openai_options(keys: &dyn KeySource) -> Result<ResponsesOptions, ModelError> {
    let mut opts = ResponsesOptions::new("openai", need(keys, "OPENAI_API_KEY", "openai")?);
    opts.base_url = keys.key("OPENAI_BASE_URL");
    Ok(opts)
}

/// `openrouter`: OpenRouter's Chat Completions with the key
/// `OPENROUTER_API_KEY`, and the registry's routing and app attribution.
pub(super) fn openrouter_options(
    keys: &dyn KeySource,
    registry: &RegistryOptions,
) -> Result<CompatOptions, ModelError> {
    let key = need(keys, "OPENROUTER_API_KEY", "openrouter")?;
    let mut opts = CompatOptions::new("openrouter", key);
    opts.flavor = CompatFlavor::OpenRouter;
    opts.base_url = Some(openrouter_base_url(keys));
    if let Some(url) = &registry.app_url {
        opts.headers.push(("HTTP-Referer".into(), url.clone()));
    }
    if let Some(title) = &registry.app_title {
        opts.headers.push(("X-Title".into(), title.clone()));
    }
    opts.routing = registry.routing.clone();
    Ok(opts)
}

/// `paperinstruments`: Chat Completions at `PAPERINSTRUMENTS_BASE_URL` (else
/// `https://api.paperinstruments.com/v1`), with the key
/// `PAPERINSTRUMENTS_API_KEY`, its effort levels and the catalog's prices.
///
/// docs.paperinstruments.com/docs/ultramarine/chat-completions (2026-10-17): top-level
/// `reasoning_effort` low/high/max only, reasoning cannot be disabled, no cost in the reply.
pub(crate) fn paper_instruments_options(keys: &dyn KeySource) -> Result<CompatOptions, ModelError> {
    let key = need(keys, "PAPERINSTRUMENTS_API_KEY", "paperinstruments")?;
    let mut opts = CompatOptions::new("paperinstruments", key);
    opts.base_url = Some(
        keys.key("PAPERINSTRUMENTS_BASE_URL")
            .unwrap_or_else(|| "https://api.paperinstruments.com/v1".into()),
    );
    opts.reasoning_effort = Some(Reasoning {
        mandatory: true,
        efforts: vec![
            OpenRouterEffort::Low,
            OpenRouterEffort::High,
            OpenRouterEffort::Max,
        ],
    });
    opts.pricing = Catalog::builtin().provider_pricing("paperinstruments");
    Ok(opts)
}
