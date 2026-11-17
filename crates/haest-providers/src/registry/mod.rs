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
//!
//! [`KeySource`]: haest_core::KeySource

mod builtin;
mod custom;
mod error;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_custom;

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use haest_core::{
    KeySourceRef,
    provider::{ModelError, ModelResolver, ProviderRef, ResolvedModel, parse_model_spec},
};

pub(crate) use builtin::paper_instruments_options;
use builtin::{anthropic_options, openai_options, openrouter_options};
pub use custom::{CustomProviderConfig, CustomProviders, Dialect, load_custom_providers};
pub use error::{EntryProblem, ProvidersFileError};

use haest_http::{HttpClientRef, ReqwestClient};

use crate::{
    anthropic::{AnthropicOptions, AnthropicProvider},
    compat::{CompatOptions, CompatProvider},
    responses::{ResponsesOptions, ResponsesProvider},
    retry::RetryPolicy,
    routing::RoutingScopes,
    send::HEADERS_TIMEOUT,
};

const BUILTIN: [&str; 4] = ["anthropic", "openai", "openrouter", "paperinstruments"];

/// What every provider a [`ProviderRegistry`] builds shares: the HTTP client,
/// the retry policy, the headers timeout and OpenRouter's extras.
///
/// The default sends over the process's shared [`ReqwestClient`], retries as
/// the SDKs do, waits ten minutes for each try's status, sends no routing,
/// and tells OpenRouter the app is `haest` (`X-Title`) with no URL. Change a
/// part with its `with_*` setter; read it with the getter of its name. The
/// default builds nothing: the shared client is built on the first request,
/// so a host that brings its own never pays for it.
///
/// ```
/// use std::time::Duration;
///
/// use haest_providers::{RegistryOptions, RetryPolicy, http::ReqwestClient};
///
/// let opts = RegistryOptions::default()
///     .with_http(ReqwestClient::new())
///     .with_retry(RetryPolicy::none())
///     .with_headers_timeout(Duration::from_secs(60))
///     .without_app_title();
/// assert_eq!(opts.app_title(), None);
/// assert_eq!(opts.retry().max_retries, 0);
/// ```
#[derive(Debug, Clone)]
pub struct RegistryOptions {
    http: HttpClientRef,
    retry: RetryPolicy,
    headers_timeout: Duration,
    routing: Option<RoutingScopes>,
    app_title: Option<String>,
    app_url: Option<String>,
}

impl Default for RegistryOptions {
    fn default() -> Self {
        Self {
            http: ReqwestClient::new().into(),
            retry: RetryPolicy::default(),
            headers_timeout: HEADERS_TIMEOUT,
            routing: None,
            app_title: Some("haest".into()),
            app_url: None,
        }
    }
}

impl RegistryOptions {
    /// Every provider's requests going through `http` (the client itself, or
    /// an `Arc` of one the host shares) instead of the shared [`ReqwestClient`].
    pub fn with_http(mut self, http: impl Into<HttpClientRef>) -> Self {
        self.http = http.into();
        self
    }

    /// Every provider retrying a refused or unanswered request by `retry`.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Each try waiting at most `wait` for its status and headers before it
    /// counts as unanswered ([`AnthropicProvider::with_headers_timeout`]).
    pub fn with_headers_timeout(mut self, wait: Duration) -> Self {
        self.headers_timeout = wait;
        self
    }

    /// OpenRouter provider routing to send, or none ([`crate::routing::parse_routing`]
    /// reads the host's JSON: haest's `provider_routing` setting). Takes the
    /// scopes or an `Option` of them; a host whose gateway routes leaves it off.
    pub fn with_routing(mut self, routing: impl Into<Option<RoutingScopes>>) -> Self {
        self.routing = routing.into();
        self
    }

    /// OpenRouter's app attribution `X-Title` (`haest` by default).
    pub fn with_app_title(mut self, title: impl Into<String>) -> Self {
        self.app_title = Some(title.into());
        self
    }

    /// No `X-Title` sent to OpenRouter.
    pub fn without_app_title(mut self) -> Self {
        self.app_title = None;
        self
    }

    /// OpenRouter's app attribution `HTTP-Referer` (none by default).
    pub fn with_app_url(mut self, url: impl Into<String>) -> Self {
        self.app_url = Some(url.into());
        self
    }

    /// How every provider's requests travel.
    pub fn http(&self) -> &HttpClientRef {
        &self.http
    }

    /// How every provider retries a refused or unanswered request.
    pub fn retry(&self) -> &RetryPolicy {
        &self.retry
    }

    /// How long each try waits for its status and headers.
    pub fn headers_timeout(&self) -> Duration {
        self.headers_timeout
    }

    /// The OpenRouter provider routing sent; None = none.
    pub fn routing(&self) -> Option<&RoutingScopes> {
        self.routing.as_ref()
    }

    /// OpenRouter's `X-Title`; None = not sent.
    pub fn app_title(&self) -> Option<&str> {
        self.app_title.as_deref()
    }

    /// OpenRouter's `HTTP-Referer`; None = not sent.
    pub fn app_url(&self) -> Option<&str> {
        self.app_url.as_deref()
    }
}

/// Turns a `provider:model` spec into a live provider: the built-ins and the
/// providers.json entries it was handed.
///
/// [`resolve`](Self::resolve) turns a spec into the provider and its model
/// id; it is also haest-core's [`ModelResolver`], what the agent loop asks
/// for a model, so the registry goes into `AgentDeps` as it is. A provider is
/// built on its name's first resolve and kept: later specs with that name
/// share it, and the key it read then stays its key. A name that fails is
/// tried again on the next resolve.
///
/// Cloning is cheap, and the clones share what was built: one registry
/// serves every agent of a conversation. A `with_*` setter gives the registry
/// it returns a cache of its own.
///
/// Resolving sends nothing. The provider name is everything before the first
/// `:`; the model id is the rest, colons and slashes included.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
///
/// use haest_core::{ModelError, error::BoxError};
/// use haest_providers::{CustomProviders, ProviderRegistry};
///
/// # fn main() -> Result<(), BoxError> {
/// let keys = HashMap::from([("ANTHROPIC_API_KEY".to_string(), "sk-ant-...".to_string())]);
/// let custom: CustomProviders = serde_json::from_str(
///     r#"{"local": {"type": "openai", "baseURL": "http://localhost:1234/v1"}}"#,
/// )?;
/// let registry = ProviderRegistry::new(keys).with_custom(custom);
///
/// let opus = registry.resolve("anthropic:claude-opus-5-5")?;
/// assert_eq!((opus.provider().id(), opus.model()), ("anthropic", "claude-opus-5-5"));
/// assert_eq!(registry.resolve("local:qwen3-coder")?.provider().id(), "local");
///
/// // No OPENROUTER_API_KEY: the error names it.
/// assert!(matches!(
///     registry.resolve("openrouter:z-ai/glm-5.3"),
///     Err(ModelError::MissingKey { key, .. }) if key == "OPENROUTER_API_KEY"
/// ));
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ProviderRegistry {
    setup: Arc<Setup>,
    /// What has been built, by provider name; shared by the clones.
    cache: Arc<Mutex<HashMap<String, ProviderRef>>>,
}

/// What a [`ProviderRegistry`] builds from.
#[derive(Clone)]
struct Setup {
    keys: KeySourceRef,
    custom: CustomProviders,
    opts: RegistryOptions,
}

impl ProviderRegistry {
    /// The built-ins, their keys asked of `keys` (a closure, a `HashMap`, a
    /// host's own [`KeySource`](haest_core::KeySource), or an `Arc` of one);
    /// no providers.json entries and the default [`RegistryOptions`]. Nothing
    /// is built, and no key asked for, until a spec is resolved.
    pub fn new(keys: impl Into<KeySourceRef>) -> Self {
        Self {
            setup: Arc::new(Setup {
                keys: keys.into(),
                custom: CustomProviders::new(),
                opts: RegistryOptions::default(),
            }),
            cache: Arc::default(),
        }
    }

    /// This registry with providers.json's entries beside the built-ins (an
    /// entry named like a built-in replaces it).
    pub fn with_custom(mut self, custom: CustomProviders) -> Self {
        self.setup_mut().custom = custom;
        self
    }

    /// This registry building every provider with `opts`.
    pub fn with_options(mut self, opts: RegistryOptions) -> Self {
        self.setup_mut().opts = opts;
        self
    }

    /// This registry sending every request through `http`: the same as
    /// [`with_options`](Self::with_options) with
    /// [`RegistryOptions::with_http`].
    pub fn with_http(mut self, http: impl Into<HttpClientRef>) -> Self {
        let opts = &mut self.setup_mut().opts;
        opts.http = http.into();
        self
    }

    /// The setup to change, and an empty cache: what was built from the old
    /// setup is not this registry's.
    fn setup_mut(&mut self) -> &mut Setup {
        self.cache = Arc::default();
        Arc::make_mut(&mut self.setup)
    }

    /// Where the built-ins' keys come from.
    pub fn keys(&self) -> &KeySourceRef {
        &self.setup.keys
    }

    /// providers.json's entries.
    pub fn custom(&self) -> &CustomProviders {
        &self.setup.custom
    }

    /// What every provider is built with.
    pub fn options(&self) -> &RegistryOptions {
        &self.setup.opts
    }

    /// The provider and model id for `spec` (`openrouter:z-ai/glm-5` -> the
    /// openrouter provider, `z-ai/glm-5`), the provider built on its name's
    /// first resolve and kept.
    ///
    /// # Errors
    ///
    /// [`ModelError::BadSpec`] or [`ModelError::UnknownProvider`] for a spec
    /// it cannot place, [`ModelError::MissingKey`] when a built-in's key is
    /// missing from the key source, and [`ModelError::Unavailable`] (the
    /// [`EntryProblem`] its source) when the providers.json entry cannot be
    /// used.
    pub fn resolve(&self, spec: &str) -> Result<ResolvedModel, ModelError> {
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
        Ok(ResolvedModel::new(provider, model))
    }

    /// Every provider name a spec may use, each once: the built-ins, then the
    /// other providers.json entries in name order. An entry named like a
    /// built-in replaces it in its place.
    pub fn names(&self) -> Vec<String> {
        let added = self
            .setup
            .custom
            .keys()
            .filter(|k| !BUILTIN.contains(&k.as_str()));
        BUILTIN
            .iter()
            .map(|s| s.to_string())
            .chain(added.cloned())
            .collect()
    }

    fn build(&self, name: &str) -> Result<ProviderRef, ModelError> {
        let o = &self.setup.opts;
        let (http, retry, wait) = (o.http.clone(), o.retry.clone(), o.headers_timeout);
        Ok(match self.provider_options(name)? {
            Options::Anthropic(o) => ProviderRef::new(
                AnthropicProvider::new(o, http)
                    .with_retry(retry)
                    .with_headers_timeout(wait),
            ),
            Options::Compat(o) => ProviderRef::new(
                CompatProvider::new(*o, http)
                    .with_retry(retry)
                    .with_headers_timeout(wait),
            ),
            Options::Responses(o) => ProviderRef::new(
                ResponsesProvider::new(o, http)
                    .with_retry(retry)
                    .with_headers_timeout(wait),
            ),
        })
    }

    /// `name`'s options: its providers.json entry, else the built-in.
    fn provider_options(&self, name: &str) -> Result<Options, ModelError> {
        let Setup { keys, custom, opts } = &*self.setup;
        let keys = keys.as_ref();
        if let Some(raw) = custom.get(name) {
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
            "openrouter" => Options::Compat(Box::new(openrouter_options(keys, opts)?)),
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

/// The names it resolves, what it builds with and what it has built; never
/// an entry's JSON (an `apiKey` may be in it).
impl fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let built: Vec<String> = {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            let mut names: Vec<String> = cache.keys().cloned().collect();
            names.sort();
            names
        };
        f.debug_struct("ProviderRegistry")
            .field("names", &self.names())
            .field("options", &self.setup.opts)
            .field("built", &built)
            .finish_non_exhaustive()
    }
}

/// A provider's options, by the wire dialect it speaks, before it is built.
enum Options {
    Anthropic(AnthropicOptions),
    Compat(Box<CompatOptions>),
    Responses(ResponsesOptions),
}

/// [`ProviderRegistry::resolve`], for the agent loop.
impl ModelResolver for ProviderRegistry {
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, ModelError> {
        ProviderRegistry::resolve(self, spec)
    }
}
