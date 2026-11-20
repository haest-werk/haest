//! Resolve a `provider:model` spec to a live provider.
//!
//! Built in: `anthropic`, `openai`, `openrouter`, `paperinstruments` (keys
//! under the standard variable names, asked of the host's [`KeySource`]). Any
//! other name comes from `providers.json` in the haest config dir
//! ([`CustomProviderConfig`]), or is a provider the host built itself
//! ([`ProviderRegistry::with_provider`]). `type` is the wire dialect ("openai" = Chat
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
#[cfg(test)]
mod tests_file;

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use haest_core::{
    KeySourceRef,
    provider::{ModelError, ModelResolver, ProviderRef, ResolvedModel, parse_model_spec},
};

pub(crate) use builtin::paper_instruments_options;
use builtin::{anthropic_options, openai_options, openrouter_options};
pub use custom::{CustomProviderConfig, CustomProviders, Dialect, load_custom_providers};
pub use error::{EntryProblem, ProvidersFileError, UnknownKey};

use haest_http::{HttpClientRef, check_endpoint_url};

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
/// the retry policy, the headers timeout and OpenRouter's extras. Set and
/// read on the registry.
#[derive(Debug, Clone)]
pub(crate) struct Shared {
    http: HttpClientRef,
    retry: RetryPolicy,
    headers_timeout: Duration,
    pub(crate) routing: Option<RoutingScopes>,
    pub(crate) app_title: Option<String>,
    pub(crate) app_url: Option<String>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            http: HttpClientRef::default(),
            retry: RetryPolicy::default(),
            headers_timeout: HEADERS_TIMEOUT,
            routing: None,
            app_title: Some("haest".into()),
            app_url: None,
        }
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
/// tried again on the next resolve. The key source is asked with nothing
/// locked, so a slow one holds up only the name it is asked for.
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
    /// The host's own providers, built already, by name.
    providers: BTreeMap<String, ProviderRef>,
    shared: Shared,
}

impl ProviderRegistry {
    /// The built-ins, their keys asked of `keys` (a closure, a `HashMap`, a
    /// host's own [`KeySource`](haest_core::KeySource), or an `Arc` of one),
    /// and no providers.json entries. Every provider it builds sends over the
    /// default client ([`HttpClientRef::default`]: the process's shared
    /// `ReqwestClient`, or without the `reqwest` feature none), retries as the
    /// SDKs do, waits ten minutes for each try's status, sends no routing,
    /// and tells OpenRouter the app is `haest` (`X-Title`) with no URL;
    /// change a part with its `with_*` setter.
    ///
    /// Nothing is built, no key asked for and no client built until a spec is
    /// resolved: a host that brings its own client never pays for the shared one.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use haest_providers::{ProviderRegistry, RetryPolicy, http::HttpClientRef};
    ///
    /// let keys = |name: &str| (name == "OPENROUTER_API_KEY").then(|| "sk-or-...".to_string());
    /// let registry = ProviderRegistry::new(keys)
    ///     .with_http(HttpClientRef::default())
    ///     .with_retry(RetryPolicy::none())
    ///     .with_headers_timeout(Duration::from_secs(60))
    ///     .without_app_title();
    /// ```
    pub fn new(keys: impl Into<KeySourceRef>) -> Self {
        Self {
            setup: Arc::new(Setup {
                keys: keys.into(),
                custom: CustomProviders::new(),
                providers: BTreeMap::new(),
                shared: Shared::default(),
            }),
            cache: Arc::default(),
        }
    }

    /// This registry with providers.json's entries beside the built-ins (an
    /// entry named like a built-in replaces it), in place of the entries it
    /// had.
    #[must_use]
    pub fn with_custom(mut self, custom: CustomProviders) -> Self {
        self.setup_mut().custom = custom;
        self
    }

    /// This registry with `config` as the custom provider `name`, beside the
    /// entries it has (in place of one of that name): a providers.json entry
    /// written in Rust.
    #[must_use]
    pub fn with_custom_provider(
        mut self,
        name: impl Into<String>,
        config: CustomProviderConfig,
    ) -> Self {
        self.setup_mut().custom.insert(name, config);
        self
    }

    /// This registry resolving `name:<model>` to `provider`, the host's own
    /// [`Provider`](haest_core::Provider) (or a handle to one): it wins over
    /// a providers.json entry and a built-in of that name. The registry
    /// builds nothing for it, so its client, retries and keys are its own.
    ///
    /// ```
    /// use haest_core::testing::ScriptedProvider;
    /// use haest_providers::ProviderRegistry;
    ///
    /// let registry = ProviderRegistry::new(|_: &str| None)
    ///     .with_provider("bedrock", ScriptedProvider::new([]));
    /// let resolved = registry.resolve("bedrock:claude-opus-5-5")?;
    /// assert_eq!(resolved.model(), "claude-opus-5-5");
    /// assert!(registry.names().contains(&"bedrock".to_string()));
    /// # Ok::<(), haest_core::ModelError>(())
    /// ```
    #[must_use]
    pub fn with_provider(
        mut self,
        name: impl Into<String>,
        provider: impl Into<ProviderRef>,
    ) -> Self {
        self.setup_mut()
            .providers
            .insert(name.into(), provider.into());
        self
    }

    /// This registry sending every provider's requests through `http` (the
    /// client itself, or an `Arc` of one the host shares) instead of the
    /// default client.
    #[must_use]
    pub fn with_http(mut self, http: impl Into<HttpClientRef>) -> Self {
        self.shared_mut().http = http.into();
        self
    }

    /// This registry's providers retrying a refused or unanswered request by
    /// `retry`.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.shared_mut().retry = retry;
        self
    }

    /// This registry's providers waiting at most `wait` for each try's status
    /// and headers before it counts as unanswered
    /// ([`AnthropicProvider::with_headers_timeout`]).
    #[must_use]
    pub fn with_headers_timeout(mut self, wait: Duration) -> Self {
        self.shared_mut().headers_timeout = wait;
        self
    }

    /// OpenRouter provider routing to send
    /// ([`parse_routing`](crate::parse_routing) reads the host's JSON: haest's
    /// `provider_routing` setting). None is sent until this is called; a host
    /// whose gateway routes leaves it off.
    #[must_use]
    pub fn with_routing(mut self, routing: RoutingScopes) -> Self {
        self.shared_mut().routing = Some(routing);
        self
    }

    /// OpenRouter's app attribution `X-Title` (`haest` by default).
    #[must_use]
    pub fn with_app_title(mut self, title: impl Into<String>) -> Self {
        self.shared_mut().app_title = Some(title.into());
        self
    }

    /// No `X-Title` sent to OpenRouter.
    #[must_use]
    pub fn without_app_title(mut self) -> Self {
        self.shared_mut().app_title = None;
        self
    }

    /// OpenRouter's app attribution `HTTP-Referer` (none by default).
    #[must_use]
    pub fn with_app_url(mut self, url: impl Into<String>) -> Self {
        self.shared_mut().app_url = Some(url.into());
        self
    }

    /// The setup to change, and an empty cache: what was built from the old
    /// setup is not this registry's.
    fn setup_mut(&mut self) -> &mut Setup {
        self.cache = Arc::default();
        Arc::make_mut(&mut self.setup)
    }

    fn shared_mut(&mut self) -> &mut Shared {
        &mut self.setup_mut().shared
    }

    /// The provider and model id for `spec` (`openrouter:z-ai/glm-5` -> the
    /// openrouter provider, `z-ai/glm-5`), the provider built on its name's
    /// first resolve and kept.
    ///
    /// # Errors
    ///
    /// [`ModelError::BadSpec`] or [`ModelError::UnknownProvider`] for a spec
    /// it cannot place, [`ModelError::MissingKey`] when a built-in's key is
    /// missing from the key source, and [`ModelError::Unavailable`] when the
    /// providers.json entry cannot be used (the [`EntryProblem`] its source)
    /// or the provider's base URL (a built-in's `*_BASE_URL`, an entry's
    /// `baseURL`) has an `@` past its host or no host: a password with an
    /// unescaped `/`, `?` or `#` reads to a URL parser as another host, and
    /// so does the first part of the path of a URL with no host. That
    /// error's source names where the URL was set (`ANTHROPIC_BASE_URL`,
    /// `providers.json baseURL`, or the `base_url` of an entry the host
    /// built in Rust), and its source is haest-http's
    /// [`EndpointUrlError`](haest_http::EndpointUrlError).
    pub fn resolve(&self, spec: &str) -> Result<ResolvedModel, ModelError> {
        let (name, model) = parse_model_spec(spec)?;
        if let Some(p) = self.setup.providers.get(name) {
            return Ok(ResolvedModel::new(p.clone(), model));
        }
        if let Some(p) = self.built().get(name) {
            return Ok(ResolvedModel::new(p.clone(), model));
        }
        // Built with the lock released: a key source may take its time, and
        // only this name waits for it. Two first resolves of one name both
        // build; the first kept is the one every spec shares.
        let built = self.build(name)?;
        let provider = self
            .built()
            .entry(name.to_string())
            .or_insert(built)
            .clone();
        Ok(ResolvedModel::new(provider, model))
    }

    /// Every provider name a spec may use, each once: the built-ins, then the
    /// other providers.json entries in name order, then the host's other
    /// providers in name order. A name that replaces another keeps its place.
    pub fn names(&self) -> Vec<String> {
        let Setup {
            custom, providers, ..
        } = &*self.setup;
        let custom = custom.names().filter(|k| !BUILTIN.contains(k));
        let own = providers
            .keys()
            .map(String::as_str)
            .filter(|k| !BUILTIN.contains(k) && !self.setup.custom.contains(k));
        BUILTIN
            .into_iter()
            .chain(custom)
            .chain(own)
            .map(str::to_string)
            .collect()
    }

    /// What has been built. Nothing panics while holding it, so a poisoned
    /// lock still holds a whole map.
    fn built(&self) -> MutexGuard<'_, HashMap<String, ProviderRef>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn build(&self, name: &str) -> Result<ProviderRef, ModelError> {
        let o = &self.setup.shared;
        let (http, retry, wait) = (o.http.clone(), o.retry, o.headers_timeout);
        let (options, from) = self.provider_options(name)?;
        if let Some(url) = options.base_url() {
            check_endpoint_url(url).map_err(|source| ModelError::Unavailable {
                provider: name.to_string(),
                source: Box::new(BaseUrlRefused { from, source }),
            })?;
        }
        Ok(match options {
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

    /// `name`'s options, its providers.json entry else the built-in, and
    /// where its base URL is set.
    fn provider_options(&self, name: &str) -> Result<(Options, &'static str), ModelError> {
        let Setup {
            keys,
            custom,
            shared,
            ..
        } = &*self.setup;
        let keys = keys.as_ref();
        if let Some(entry) = custom.get(name) {
            let cfg = entry.map_err(|problem| ModelError::Unavailable {
                provider: name.to_string(),
                source: Box::new(problem),
            })?;
            let options = match cfg.dialect {
                Dialect::Anthropic => Options::Anthropic(cfg.anthropic_options(name, keys)),
                Dialect::OpenAi => Options::Compat(Box::new(cfg.compat_options(name, keys))),
            };
            let from = if custom.is_json(name) {
                "providers.json baseURL"
            } else {
                HOST_BASE_URL
            };
            return Ok((options, from));
        }
        Ok(match name {
            "anthropic" => (
                Options::Anthropic(anthropic_options(keys)?),
                "ANTHROPIC_BASE_URL",
            ),
            "openai" => (Options::Responses(openai_options(keys)?), "OPENAI_BASE_URL"),
            "openrouter" => (
                Options::Compat(Box::new(openrouter_options(keys, shared)?)),
                "OPENROUTER_BASE_URL",
            ),
            "paperinstruments" => (
                Options::Compat(Box::new(paper_instruments_options(keys)?)),
                "PAPERINSTRUMENTS_BASE_URL",
            ),
            _ => {
                return Err(ModelError::UnknownProvider {
                    provider: name.into(),
                    known: self.names(),
                });
            }
        })
    }
}

/// Where a custom entry the host built in Rust set its base URL: it came
/// from no providers.json.
const HOST_BASE_URL: &str = "the base_url the host gave it";

/// A provider's base URL [`check_endpoint_url`] refused, and where it was
/// set (`ANTHROPIC_BASE_URL`, an entry's `baseURL`, the `base_url` of an
/// entry the host built): the human then knows which one to fix.
#[derive(Debug, thiserror::Error)]
#[error("{from}")]
struct BaseUrlRefused {
    /// The variable or the providers.json key the URL came from.
    from: &'static str,
    /// What is wrong with it.
    source: haest_http::EndpointUrlError,
}

/// The names it resolves, what it builds with and what it has built; never
/// an entry's JSON (an `apiKey` may be in it).
impl fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut built: Vec<String> = self.built().keys().cloned().collect();
        built.sort();
        f.debug_struct("ProviderRegistry")
            .field("names", &self.names())
            .field("shared", &self.setup.shared)
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

impl Options {
    /// Where its requests go, when not the dialect's own API.
    fn base_url(&self) -> Option<&str> {
        match self {
            Self::Anthropic(o) => o.base_url(),
            Self::Compat(o) => o.base_url(),
            Self::Responses(o) => o.base_url(),
        }
    }
}

/// [`ProviderRegistry::resolve`], for the agent loop.
impl ModelResolver for ProviderRegistry {
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, ModelError> {
        ProviderRegistry::resolve(self, spec)
    }
}
