//! Resolve a `provider:model` spec to a live provider.
//!
//! Built in: `anthropic`, `openai`, `openrouter`, `paperinstruments` (keys from
//! the standard env vars). Any other name comes from `providers.json` in the kern
//! config dir ([`CustomProviderConfig`]). `type` is the wire dialect ("openai" =
//! Chat Completions, "anthropic" = Messages); the built-in `openai` provider
//! alone speaks the Responses API.
//!
//! Everything is handed in: keys come from the host's [`KeySource`],
//! providers.json's entries arrive parsed ([`load_custom_providers`] is the
//! loader's helper), routing arrives parsed. The registry reads no environment
//! and no file.

mod custom;
#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use kern_core::{
    KeySource,
    handed::openrouter_base_url,
    provider::{ModelResolver, Provider, ResolvedModel, parse_model_spec},
};

use custom::need;
pub use custom::{
    CustomProviderConfig, CustomProviders, Dialect, custom_compat_options, load_custom_providers,
    paper_instruments_options,
};

use crate::{
    anthropic::{AnthropicOptions, AnthropicProvider},
    compat::{CompatFlavor, CompatOptions, CompatProvider},
    http::RetryPolicy,
    reqwest_transport::ReqwestTransport,
    responses::{ResponsesOptions, ResponsesProvider},
    routing::RoutingEnv,
    transport::Transport,
};

/// A provider that cannot be built, said to the human as is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProviderConfigError(pub String);

const BUILTIN: [&str; 4] = ["anthropic", "openai", "openrouter", "paperinstruments"];

pub struct RegistryOptions {
    /// How every provider's requests travel.
    pub transport: Arc<dyn Transport>,
    pub retry: RetryPolicy,
    /// OpenRouter provider routing to send ([`crate::routing::parse_routing`]
    /// reads the host's JSON: kern's `provider_routing` setting). None = none:
    /// a host whose gateway routes leaves it off.
    pub routing: Option<RoutingEnv>,
    /// OpenRouter's app attribution: `X-Title` (None = not sent).
    pub app_title: Option<String>,
    /// OpenRouter's app attribution: `HTTP-Referer` (None = not sent).
    pub app_url: Option<String>,
}

impl Default for RegistryOptions {
    fn default() -> Self {
        Self {
            transport: Arc::new(ReqwestTransport::new()),
            retry: RetryPolicy::default(),
            routing: None,
            app_title: Some("kern".into()),
            app_url: None,
        }
    }
}

pub struct ProviderRegistry {
    keys: Arc<dyn KeySource>,
    custom: CustomProviders,
    opts: RegistryOptions,
    cache: Mutex<HashMap<String, Arc<dyn Provider>>>,
}

impl ProviderRegistry {
    /// The built-ins and `custom` (providers.json's entries; empty = none),
    /// their keys from `keys`.
    pub fn new(keys: Arc<dyn KeySource>, custom: CustomProviders, opts: RegistryOptions) -> Self {
        Self {
            keys,
            custom,
            opts,
            cache: Mutex::default(),
        }
    }

    pub fn names(&self) -> Vec<String> {
        BUILTIN
            .iter()
            .map(|s| s.to_string())
            .chain(self.custom.keys().cloned())
            .collect()
    }

    /// `openrouter:z-ai/glm-5` -> (the openrouter provider, `z-ai/glm-5`). Built once per name.
    pub fn resolve_spec(&self, spec: &str) -> Result<ResolvedModel, ProviderConfigError> {
        let (name, model) = parse_model_spec(spec).map_err(ProviderConfigError)?;
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let provider = match cache.get(name) {
            Some(p) => p.clone(),
            None => {
                let p = self.build(name)?;
                cache.insert(name.to_string(), p.clone());
                p
            }
        };
        Ok(ResolvedModel {
            provider,
            model: model.to_string(),
        })
    }

    fn build(&self, name: &str) -> Result<Arc<dyn Provider>, ProviderConfigError> {
        let keys = self.keys.as_ref();
        if let Some(raw) = self.custom.get(name) {
            let cfg = CustomProviderConfig::from_json(name, raw)?;
            return Ok(match cfg.dialect {
                Dialect::Anthropic => self.anthropic(AnthropicOptions {
                    api_key: cfg.api_key(keys),
                    base_url: cfg.base_url.clone(),
                    headers: cfg.headers.clone(),
                }),
                Dialect::OpenAi => self.compat(custom_compat_options(name, &cfg, keys)),
            });
        }
        Ok(match name {
            "anthropic" => self.anthropic(AnthropicOptions {
                api_key: need(keys, "ANTHROPIC_API_KEY", name)?,
                base_url: keys.key("ANTHROPIC_BASE_URL"),
                headers: Vec::new(),
            }),
            // Responses, not Chat Completions: OpenAI refuses function tools + reasoning effort there.
            "openai" => {
                let opts = ResponsesOptions {
                    id: "openai".into(),
                    api_key: need(keys, "OPENAI_API_KEY", name)?,
                    base_url: keys.key("OPENAI_BASE_URL"),
                    headers: Vec::new(),
                };
                let (t, retry) = (self.opts.transport.clone(), self.opts.retry.clone());
                Arc::new(ResponsesProvider::new(opts, t).with_retry(retry))
            }
            "openrouter" => self.compat(self.openrouter_options()?),
            "paperinstruments" => self.compat(paper_instruments_options(keys)?),
            _ => {
                return Err(ProviderConfigError(format!(
                    "unknown provider \"{name}\". Known: {}",
                    self.names().join(", ")
                )));
            }
        })
    }

    fn anthropic(&self, opts: AnthropicOptions) -> Arc<dyn Provider> {
        let (t, retry) = (self.opts.transport.clone(), self.opts.retry.clone());
        Arc::new(AnthropicProvider::new(opts, t).with_retry(retry))
    }

    fn compat(&self, opts: CompatOptions) -> Arc<dyn Provider> {
        let (t, retry) = (self.opts.transport.clone(), self.opts.retry.clone());
        Arc::new(CompatProvider::new(opts, t).with_retry(retry))
    }

    fn openrouter_options(&self) -> Result<CompatOptions, ProviderConfigError> {
        let keys = self.keys.as_ref();
        let mut opts = CompatOptions::new(
            "openrouter",
            CompatFlavor::OpenRouter,
            need(keys, "OPENROUTER_API_KEY", "openrouter")?,
        );
        opts.base_url = Some(openrouter_base_url(keys));
        if let Some(url) = &self.opts.app_url {
            opts.headers.push(("HTTP-Referer".into(), url.clone()));
        }
        if let Some(title) = &self.opts.app_title {
            opts.headers.push(("X-Title".into(), title.clone()));
        }
        opts.routing = self.opts.routing.clone();
        Ok(opts)
    }
}

impl ModelResolver for ProviderRegistry {
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, String> {
        self.resolve_spec(spec).map_err(|e| e.0)
    }
}
