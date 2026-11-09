//! Resolve a `provider:model` spec to a live provider.
//!
//! Built in: `anthropic`, `openai`, `openrouter`, `paperinstruments` (keys from
//! the standard env vars). Any other name comes from `providers.json` in the kern
//! config dir ([`CustomProviderConfig`]). `type` is the wire dialect ("openai" =
//! Chat Completions, "anthropic" = Messages); the built-in `openai` provider
//! alone speaks the Responses API.
//!
//! The environment is a map the host passes in, and providers.json is read
//! through a kern [`Fs`]: the registry never reads the process environment or
//! the disk on its own.

mod custom;
#[cfg(test)]
mod tests;

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::{Arc, Mutex},
};

use kern_core::{
    fs::Fs,
    provider::{ModelResolver, Provider, ResolvedModel, parse_model_spec},
};
use serde_json::Value;

pub use custom::{
    CustomProviderConfig, Dialect, custom_compat_options, load_custom_providers,
    paper_instruments_options,
};
use custom::{env_opt, need};

use crate::{
    anthropic::{AnthropicOptions, AnthropicProvider},
    compat::{CompatFlavor, CompatOptions, CompatProvider},
    http::RetryPolicy,
    reqwest_transport::ReqwestTransport,
    responses::{ResponsesOptions, ResponsesProvider},
    routing::routing_from_env,
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
    /// Send OpenRouter provider routing read from `KERN_PROVIDER_ROUTING` in the env.
    /// Off unless asked: a host whose gateway routes leaves it off.
    pub openrouter_routing: bool,
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
            openrouter_routing: false,
            app_title: Some("kern".into()),
            app_url: None,
        }
    }
}

pub struct ProviderRegistry {
    env: HashMap<String, String>,
    custom: BTreeMap<String, Value>,
    opts: RegistryOptions,
    warnings: Vec<String>,
    cache: Mutex<HashMap<String, Arc<dyn Provider>>>,
}

impl ProviderRegistry {
    /// `custom` is providers.json's object ([`load_custom_providers`]); each entry is
    /// checked when its provider is first resolved, so one bad entry blocks only itself.
    pub fn new(
        env: HashMap<String, String>,
        custom: BTreeMap<String, Value>,
        opts: RegistryOptions,
    ) -> Self {
        let mut warnings = Vec::new();
        if opts.openrouter_routing
            && let Err(e) = routing_from_env(&env)
        {
            warnings.push(e);
        }
        Self {
            env,
            custom,
            opts,
            warnings,
            cache: Mutex::default(),
        }
    }

    /// [`ProviderRegistry::new`] with providers.json read from `config_dir` through `fs`.
    pub async fn load(
        fs: &dyn Fs,
        config_dir: &Path,
        env: HashMap<String, String>,
        opts: RegistryOptions,
    ) -> Result<Self, ProviderConfigError> {
        let custom = load_custom_providers(fs, config_dir).await?;
        Ok(Self::new(env, custom, opts))
    }

    /// Configuration problems that did not stop the registry (a malformed routing env var).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
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
        if let Some(raw) = self.custom.get(name) {
            let cfg = CustomProviderConfig::from_json(name, raw)?;
            return Ok(self.build_custom(name, &cfg));
        }
        let env = &self.env;
        let t = self.opts.transport.clone();
        let retry = self.opts.retry.clone();
        Ok(match name {
            "anthropic" => Arc::new(
                AnthropicProvider::new(
                    AnthropicOptions {
                        api_key: need(env, "ANTHROPIC_API_KEY", name)?,
                        base_url: env_opt(env, "ANTHROPIC_BASE_URL"),
                        headers: Vec::new(),
                    },
                    t,
                )
                .with_retry(retry),
            ),
            // Responses, not Chat Completions: OpenAI refuses function tools + reasoning effort there.
            "openai" => Arc::new(
                ResponsesProvider::new(
                    ResponsesOptions {
                        id: "openai".into(),
                        api_key: need(env, "OPENAI_API_KEY", name)?,
                        base_url: env_opt(env, "OPENAI_BASE_URL"),
                        headers: Vec::new(),
                    },
                    t,
                )
                .with_retry(retry),
            ),
            "openrouter" => {
                Arc::new(CompatProvider::new(self.openrouter_options()?, t).with_retry(retry))
            }
            "paperinstruments" => {
                Arc::new(CompatProvider::new(paper_instruments_options(env)?, t).with_retry(retry))
            }
            _ => {
                return Err(ProviderConfigError(format!(
                    "unknown provider \"{name}\". Known: {}",
                    self.names().join(", ")
                )));
            }
        })
    }

    fn openrouter_options(&self) -> Result<CompatOptions, ProviderConfigError> {
        let env = &self.env;
        let mut opts = CompatOptions::new(
            "openrouter",
            CompatFlavor::OpenRouter,
            need(env, "OPENROUTER_API_KEY", "openrouter")?,
        );
        opts.base_url = Some(
            env_opt(env, "OPENROUTER_BASE_URL")
                .unwrap_or_else(|| "https://openrouter.ai/api/v1".into()),
        );
        if let Some(url) = &self.opts.app_url {
            opts.headers.push(("HTTP-Referer".into(), url.clone()));
        }
        if let Some(title) = &self.opts.app_title {
            opts.headers.push(("X-Title".into(), title.clone()));
        }
        if self.opts.openrouter_routing {
            opts.routing = routing_from_env(env).unwrap_or(None);
        }
        Ok(opts)
    }

    fn build_custom(&self, name: &str, cfg: &CustomProviderConfig) -> Arc<dyn Provider> {
        let t = self.opts.transport.clone();
        let retry = self.opts.retry.clone();
        match cfg.dialect {
            Dialect::Anthropic => Arc::new(
                AnthropicProvider::new(
                    AnthropicOptions {
                        api_key: cfg.api_key(&self.env),
                        base_url: cfg.base_url.clone(),
                        headers: cfg.headers.clone(),
                    },
                    t,
                )
                .with_retry(retry),
            ),
            Dialect::OpenAi => Arc::new(
                CompatProvider::new(custom_compat_options(name, cfg, &self.env), t)
                    .with_retry(retry),
            ),
        }
    }
}

impl ModelResolver for ProviderRegistry {
    fn resolve(&self, spec: &str) -> Result<ResolvedModel, String> {
        self.resolve_spec(spec).map_err(|e| e.0)
    }
}
