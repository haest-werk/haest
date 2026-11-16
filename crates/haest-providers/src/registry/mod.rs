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

mod custom;
mod error;
#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use haest_core::{
    KeySource,
    handed::openrouter_base_url,
    provider::{ModelError, ModelResolver, Provider, ResolvedModel, parse_model_spec},
};

use custom::need;
pub(crate) use custom::paper_instruments_options;
pub use custom::{CustomProviderConfig, CustomProviders, Dialect, load_custom_providers};
pub use error::{EntryProblem, ProviderConfigError, ProvidersFileError};

use haest_http::{HttpClient, ReqwestClient};

use crate::{
    anthropic::{AnthropicOptions, AnthropicProvider},
    compat::{CompatFlavor, CompatOptions, CompatProvider},
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
/// A provider is built on its name's first [`resolve_spec`](Self::resolve_spec)
/// and kept: later specs with that name share it, and the key it read then
/// stays its key. A name that fails is tried again on the next resolve. As
/// haest-core's [`ModelResolver`] it is what the agent loop asks for a model;
/// it is `Send + Sync` and may be shared.
///
/// # Examples
///
/// ```
/// use std::{collections::HashMap, sync::Arc};
///
/// use haest_providers::{CustomProviders, ProviderRegistry, RegistryOptions};
///
/// let keys = HashMap::from([("ANTHROPIC_API_KEY".to_string(), "sk-ant-...".to_string())]);
/// let custom: CustomProviders = serde_json::from_str(
///     r#"{"local": {"type": "openai", "baseURL": "http://localhost:1234/v1"}}"#,
/// )
/// .unwrap();
/// let registry = ProviderRegistry::new(Arc::new(keys), custom, RegistryOptions::default());
///
/// let opus = registry.resolve_spec("anthropic:claude-opus-5-5").unwrap();
/// assert_eq!((opus.provider.id(), opus.model.as_str()), ("anthropic", "claude-opus-5-5"));
/// assert_eq!(registry.resolve_spec("local:qwen3-coder").unwrap().provider.id(), "local");
///
/// // No OPENROUTER_API_KEY: the error says so.
/// let err = registry.resolve_spec("openrouter:z-ai/glm-5.3").unwrap_err();
/// assert!(err.to_string().contains("OPENROUTER_API_KEY"));
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

    /// `openrouter:z-ai/glm-5` -> (the openrouter provider, `z-ai/glm-5`). Built once per name.
    ///
    /// The provider name is everything before the first `:`; the model id is
    /// the rest, colons and slashes included. Resolving sends nothing.
    ///
    /// # Errors
    ///
    /// [`ProviderConfigError::Spec`] when the spec has no `provider:` part or
    /// the name is neither a built-in nor a providers.json entry,
    /// [`MissingKey`](ProviderConfigError::MissingKey) when a built-in's key
    /// is missing from the [`KeySource`], and
    /// [`BadEntry`](ProviderConfigError::BadEntry) when the providers.json
    /// entry cannot be used.
    pub fn resolve_spec(&self, spec: &str) -> Result<ResolvedModel, ProviderConfigError> {
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

    fn build(&self, name: &str) -> Result<Arc<dyn Provider>, ProviderConfigError> {
        let (http, retry) = (self.opts.http.clone(), self.opts.retry.clone());
        Ok(match self.options(name)? {
            Options::Anthropic(o) => Arc::new(AnthropicProvider::new(o, http).with_retry(retry)),
            Options::Compat(o) => Arc::new(CompatProvider::new(*o, http).with_retry(retry)),
            Options::Responses(o) => Arc::new(ResponsesProvider::new(o, http).with_retry(retry)),
        })
    }

    /// `name`'s options: its providers.json entry, else the built-in.
    fn options(&self, name: &str) -> Result<Options, ProviderConfigError> {
        let keys = self.keys.as_ref();
        if let Some(raw) = self.custom.get(name) {
            let cfg = CustomProviderConfig::from_json(name, raw)?;
            return Ok(match cfg.dialect {
                Dialect::Anthropic => Options::Anthropic(cfg.anthropic_options(name, keys)),
                Dialect::OpenAi => Options::Compat(Box::new(cfg.compat_options(name, keys))),
            });
        }
        Ok(match name {
            "anthropic" => {
                let mut opts = AnthropicOptions::new(name, need(keys, "ANTHROPIC_API_KEY", name)?);
                opts.base_url = keys.key("ANTHROPIC_BASE_URL");
                Options::Anthropic(opts)
            }
            // Responses, not Chat Completions: OpenAI refuses function tools + reasoning effort there.
            "openai" => {
                let mut opts = ResponsesOptions::new(name, need(keys, "OPENAI_API_KEY", name)?);
                opts.base_url = keys.key("OPENAI_BASE_URL");
                Options::Responses(opts)
            }
            "openrouter" => Options::Compat(Box::new(self.openrouter_options()?)),
            "paperinstruments" => Options::Compat(Box::new(paper_instruments_options(keys)?)),
            _ => {
                return Err(ModelError::UnknownProvider {
                    provider: name.into(),
                    known: self.names(),
                }
                .into());
            }
        })
    }

    fn openrouter_options(&self) -> Result<CompatOptions, ProviderConfigError> {
        let keys = self.keys.as_ref();
        let key = need(keys, "OPENROUTER_API_KEY", "openrouter")?;
        let mut opts = CompatOptions::new("openrouter", key);
        opts.flavor = CompatFlavor::OpenRouter;
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

/// A provider's options, by the wire dialect it speaks, before it is built.
enum Options {
    Anthropic(AnthropicOptions),
    Compat(Box<CompatOptions>),
    Responses(ResponsesOptions),
}

/// [`resolve_spec`](ProviderRegistry::resolve_spec): a bad spec or an unknown
/// provider is that [`ModelError`]; a provider it cannot build is
/// [`ModelError::Unavailable`] holding the [`ProviderConfigError`].
impl ModelResolver for ProviderRegistry {
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, ModelError> {
        self.resolve_spec(spec).map_err(|e| match e {
            ProviderConfigError::Spec(e) => e,
            e => ModelError::Unavailable(Box::new(e)),
        })
    }
}
