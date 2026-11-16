//! Resolve a `provider:model` spec to a live provider.
//!
//! Built in: `anthropic`, `openai`, `openrouter`, `paperinstruments` (keys
//! under the standard variable names, asked of the host's [`KeySource`]). Any
//! other name comes from `providers.json` in the haest config dir
//! ([`CustomProviderConfig`]). `type` is the wire dialect ("openai" = Chat
//! Completions, "anthropic" = Messages); the built-in `openai` provider alone
//! speaks the Responses API.
//!
//! Everything is handed in: keys come from the host's [`KeySource`],
//! providers.json's entries arrive parsed ([`load_custom_providers`] is the
//! loader's helper), routing arrives parsed. The registry reads no environment
//! and no file.

mod builtin;
mod custom;
mod error;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_custom;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use haest_core::{
    KeySource,
    provider::{ModelError, ModelResolver, Provider, ResolvedModel, parse_model_spec},
};

pub(crate) use builtin::paper_instruments_options;
use builtin::{anthropic_options, openai_options, openrouter_options};
pub use custom::{CustomProviderConfig, CustomProviders, Dialect, load_custom_providers};
pub use error::{EntryProblem, ProvidersFileError};

use haest_http::{HttpClient, ReqwestClient};

use crate::{
    anthropic::{AnthropicOptions, AnthropicProvider},
    compat::{CompatOptions, CompatProvider},
    responses::{ResponsesOptions, ResponsesProvider},
    retry::RetryPolicy,
    routing::RoutingScopes,
};

const BUILTIN: [&str; 4] = ["anthropic", "openai", "openrouter", "paperinstruments"];

/// What every provider a [`ProviderRegistry`] builds shares: the HTTP client,
/// the retry policy and OpenRouter's extras.
///
/// The default sends over the process's shared [`ReqwestClient`], retries as
/// the SDKs do, sends no routing, and tells OpenRouter the app is `haest`
/// (`X-Title`) with no URL. Change a field with struct update syntax:
/// `RegistryOptions { http, ..RegistryOptions::default() }`. The default
/// builds nothing: the shared client is built on the first request, so a host
/// that brings its own never pays for it.
pub struct RegistryOptions {
    /// How every provider's requests travel.
    pub http: Arc<dyn HttpClient>,
    /// How every provider retries a refused or unanswered request.
    pub retry: RetryPolicy,
    /// OpenRouter provider routing to send ([`crate::routing::parse_routing`]
    /// reads the host's JSON: haest's `provider_routing` setting). None = none:
    /// a host whose gateway routes leaves it off.
    pub routing: Option<RoutingScopes>,
    /// OpenRouter's app attribution: `X-Title` (None = not sent).
    pub app_title: Option<String>,
    /// OpenRouter's app attribution: `HTTP-Referer` (None = not sent).
    pub app_url: Option<String>,
}

impl Default for RegistryOptions {
    fn default() -> Self {
        Self {
            http: Arc::new(ReqwestClient::new()),
            retry: RetryPolicy::default(),
            routing: None,
            app_title: Some("haest".into()),
            app_url: None,
        }
    }
}

/// Turns a `provider:model` spec into a live provider: the built-ins and the
/// providers.json entries it was handed.
///
/// It is haest-core's [`ModelResolver`], what the agent loop asks for a model:
/// [`resolve`](ModelResolver::resolve) turns a spec into the provider and its
/// model id. A provider is built on its name's first resolve and kept: later
/// specs with that name share it, and the key it read then stays its key. A
/// name that fails is tried again on the next resolve. It is `Send + Sync`
/// and may be shared.
///
/// Resolving sends nothing. The provider name is everything before the first
/// `:`; the model id is the rest, colons and slashes included. It fails with
/// [`ModelError::BadSpec`] or [`ModelError::UnknownProvider`] for a spec it
/// cannot place, [`ModelError::MissingKey`] when a built-in's key is missing
/// from the [`KeySource`], and [`ModelError::Unavailable`] (the
/// [`EntryProblem`] its source) when the providers.json entry cannot be used.
///
/// # Examples
///
/// ```
/// use std::{collections::HashMap, sync::Arc};
///
/// use haest_core::{ModelError, ModelResolver};
/// use haest_providers::{CustomProviders, ProviderRegistry, RegistryOptions};
///
/// let keys = HashMap::from([("ANTHROPIC_API_KEY".to_string(), "sk-ant-...".to_string())]);
/// let custom: CustomProviders = serde_json::from_str(
///     r#"{"local": {"type": "openai", "baseURL": "http://localhost:1234/v1"}}"#,
/// )
/// .unwrap();
/// let registry = ProviderRegistry::new(Arc::new(keys), custom, RegistryOptions::default());
///
/// let opus = registry.resolve("anthropic:claude-opus-5-5").unwrap();
/// assert_eq!((opus.provider.id(), opus.model.as_str()), ("anthropic", "claude-opus-5-5"));
/// assert_eq!(registry.resolve("local:qwen3-coder").unwrap().provider.id(), "local");
///
/// // No OPENROUTER_API_KEY: the error names it.
/// let err = registry.resolve("openrouter:z-ai/glm-5.3").unwrap_err();
/// assert!(matches!(err, ModelError::MissingKey { key, .. } if key == "OPENROUTER_API_KEY"));
/// ```
pub struct ProviderRegistry {
    keys: Arc<dyn KeySource>,
    custom: CustomProviders,
    opts: RegistryOptions,
    cache: Mutex<HashMap<String, Arc<dyn Provider>>>,
}

impl ProviderRegistry {
    /// The built-ins and `custom` (providers.json's entries; empty = none),
    /// their keys from `keys`. Nothing is built, and no key asked for, until
    /// a spec is resolved.
    pub fn new(keys: Arc<dyn KeySource>, custom: CustomProviders, opts: RegistryOptions) -> Self {
        Self {
            keys,
            custom,
            opts,
            cache: Mutex::default(),
        }
    }

    /// Every provider name a spec may use, each once: the built-ins, then the
    /// other providers.json entries in name order. An entry named like a
    /// built-in replaces it in its place.
    pub fn names(&self) -> Vec<String> {
        let added = self
            .custom
            .keys()
            .filter(|k| !BUILTIN.contains(&k.as_str()));
        BUILTIN
            .iter()
            .map(|s| s.to_string())
            .chain(added.cloned())
            .collect()
    }

    fn build(&self, name: &str) -> Result<Arc<dyn Provider>, ModelError> {
        let (http, retry) = (self.opts.http.clone(), self.opts.retry.clone());
        Ok(match self.options(name)? {
            Options::Anthropic(o) => Arc::new(AnthropicProvider::new(o, http).with_retry(retry)),
            Options::Compat(o) => Arc::new(CompatProvider::new(*o, http).with_retry(retry)),
            Options::Responses(o) => Arc::new(ResponsesProvider::new(o, http).with_retry(retry)),
        })
    }

    /// `name`'s options: its providers.json entry, else the built-in.
    fn options(&self, name: &str) -> Result<Options, ModelError> {
        let keys = self.keys.as_ref();
        if let Some(raw) = self.custom.get(name) {
            let cfg = CustomProviderConfig::from_json(raw).map_err(|problem| {
                ModelError::Unavailable {
                    provider: name.to_string(),
                    source: Box::new(problem),
                }
            })?;
            return Ok(match cfg.dialect {
                Dialect::Anthropic => Options::Anthropic(cfg.anthropic_options(name, keys)),
                Dialect::OpenAi => Options::Compat(Box::new(cfg.compat_options(name, keys))),
            });
        }
        Ok(match name {
            "anthropic" => Options::Anthropic(anthropic_options(keys)?),
            "openai" => Options::Responses(openai_options(keys)?),
            "openrouter" => Options::Compat(Box::new(openrouter_options(keys, &self.opts)?)),
            "paperinstruments" => Options::Compat(Box::new(paper_instruments_options(keys)?)),
            _ => {
                return Err(ModelError::UnknownProvider {
                    provider: name.into(),
                    known: self.names(),
                });
            }
        })
    }
}

/// A provider's options, by the wire dialect it speaks, before it is built.
enum Options {
    Anthropic(AnthropicOptions),
    Compat(Box<CompatOptions>),
    Responses(ResponsesOptions),
}

/// `openrouter:z-ai/glm-5` -> (the openrouter provider, `z-ai/glm-5`), the
/// provider built once per name ([`ProviderRegistry`] has the rules).
impl ModelResolver for ProviderRegistry {
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, ModelError> {
        let (name, model) = parse_model_spec(spec)?;
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
}
