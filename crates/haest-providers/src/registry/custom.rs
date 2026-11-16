//! `providers.json`: the custom providers a haest config dir declares, and the
//! built-in direct provider configured the same way.

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use haest_core::{
    KeySource,
    catalog::{ModelPricing, OpenRouterEffort, Reasoning},
    fs::{Fs, read_text_if_present},
};
use serde_json::Value;

use super::ProviderConfigError;
use crate::{
    catalog::Catalog,
    compat::{CompatFlavor, CompatOptions},
};

/// The wire dialect of a custom provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// Chat Completions (`"type": "openai"`, and anything that is not `"anthropic"`).
    OpenAi,
    /// The Messages API.
    Anthropic,
}

/// One entry of providers.json:
///
/// ```json
/// { "local": { "type": "openai", "baseURL": "http://localhost:1234/v1", "apiKeyEnv": "LOCAL_KEY",
///     "reasoningEffort": { "efforts": ["low", "high", "max"], "mandatory": true },
///     "pricing": { "my-model": { "input": 0.15, "cachedInput": 0.03, "output": 0.5 } } } }
/// ```
///
/// | Key | Field | Means |
/// |---|---|---|
/// | `type` | [`dialect`](Self::dialect) | `"anthropic"` for the Messages API; anything else, or none, is Chat Completions |
/// | `baseURL` | [`base_url`](Self::base_url) | where the API is |
/// | `apiKey` | [`api_key`](Self::api_key) | the key itself |
/// | `apiKeyEnv` | [`api_key_env`](Self::api_key_env) | the name to ask the host for the key |
/// | `headers` | [`headers`](Self::headers) | an object of headers sent on every request |
/// | `reasoningEffort` | [`reasoning_effort`](Self::reasoning_effort) | `{ "efforts": [...], "mandatory": bool }` |
/// | `pricing` | [`pricing`](Self::pricing) | `{ "<model id>": { "input", "cachedInput", "output", "cacheWrite"? } }` |
///
/// `reasoningEffort` and `pricing` apply to `type: "openai"` only.
/// `reasoningEffort`: send a top-level `reasoning_effort`, fitted to the listed
/// levels ([`CompatOptions::reasoning_effort`]). Absent = no effort is sent.
/// `pricing`: USD per 1M tokens per model id, for an API that returns no cost; a
/// catalog row's `pricing` fills in for a model not listed here.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomProviderConfig {
    /// `type`: the wire dialect.
    pub dialect: Dialect,
    /// `baseURL`; None = the dialect's own API (OpenAI's or Anthropic's).
    pub base_url: Option<String>,
    /// `apiKey`: the key itself, in the file.
    pub api_key: Option<String>,
    /// `apiKeyEnv`: the name the host's [`KeySource`] is asked for when
    /// `api_key` is absent (the haest CLI answers from its environment).
    pub api_key_env: Option<String>,
    /// `headers`; a value that is not a string is skipped. One named like a
    /// default header, in any case, replaces it.
    pub headers: Vec<(String, String)>,
    /// `reasoningEffort`: the effort levels the server takes; None sends no effort.
    pub reasoning_effort: Option<Reasoning>,
    /// `pricing`: USD per 1M tokens, by model id (the spec without `name:`).
    pub pricing: HashMap<String, ModelPricing>,
}

fn opt_string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn reasoning(name: &str, v: Option<&Value>) -> Result<Option<Reasoning>, ProviderConfigError> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let (Some(efforts), Some(mandatory)) = (
        v.get("efforts").and_then(Value::as_array),
        v.get("mandatory").and_then(Value::as_bool),
    ) else {
        return Err(ProviderConfigError(format!(
            "provider \"{name}\": reasoningEffort needs \"efforts\" (array) and \"mandatory\" (boolean)"
        )));
    };
    // A level haest has no name for can never be chosen; it is skipped, not refused.
    let efforts = efforts
        .iter()
        .filter_map(|e| serde_json::from_value::<OpenRouterEffort>(e.clone()).ok())
        .collect();
    Ok(Some(Reasoning { mandatory, efforts }))
}

fn pricing(
    name: &str,
    v: Option<&Value>,
) -> Result<HashMap<String, ModelPricing>, ProviderConfigError> {
    let mut out = HashMap::new();
    for (model, price) in v.and_then(Value::as_object).into_iter().flatten() {
        let num = |k: &str| price.get(k).and_then(Value::as_f64);
        let (Some(input), Some(cached_input), Some(output)) =
            (num("input"), num("cachedInput"), num("output"))
        else {
            return Err(ProviderConfigError(format!(
                "provider \"{name}\": pricing.{model} needs numeric input, cachedInput, output"
            )));
        };
        let cache_write = num("cacheWrite");
        out.insert(
            model.clone(),
            ModelPricing {
                input,
                cached_input,
                output,
                cache_write,
            },
        );
    }
    Ok(out)
}

impl CustomProviderConfig {
    /// Read the providers.json entry `name`; a malformed effort list or price
    /// is refused, never booked as garbage.
    ///
    /// An effort level haest has no name for is skipped. A `type` other than
    /// `"anthropic"` reads as Chat Completions, a typo included.
    ///
    /// # Errors
    ///
    /// A [`ProviderConfigError`] naming the entry when `reasoningEffort` lacks
    /// an `efforts` array or a `mandatory` boolean, or a `pricing` row lacks a
    /// numeric `input`, `cachedInput` or `output`.
    ///
    /// ```
    /// use haest_providers::{CustomProviderConfig, registry::Dialect};
    /// use serde_json::json;
    ///
    /// let entry = json!({"type": "anthropic", "baseURL": "https://llm-gateway.example.com",
    ///                    "apiKeyEnv": "GATEWAY_API_KEY", "headers": {"X-Team": "platform"}});
    /// let cfg = CustomProviderConfig::from_json("gateway", &entry).unwrap();
    /// assert_eq!(cfg.dialect, Dialect::Anthropic);
    /// assert_eq!(cfg.headers, [("X-Team".to_string(), "platform".to_string())]);
    ///
    /// let bad = json!({"pricing": {"m": {"input": 1}}});
    /// assert!(CustomProviderConfig::from_json("bad", &bad).is_err());
    /// ```
    pub fn from_json(name: &str, v: &Value) -> Result<Self, ProviderConfigError> {
        let dialect = match v.get("type").and_then(Value::as_str) {
            Some("anthropic") => Dialect::Anthropic,
            _ => Dialect::OpenAi,
        };
        let headers = v
            .get("headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect();
        Ok(Self {
            dialect,
            base_url: opt_string(v, "baseURL"),
            api_key: opt_string(v, "apiKey"),
            api_key_env: opt_string(v, "apiKeyEnv"),
            headers,
            reasoning_effort: reasoning(name, v.get("reasoningEffort"))?,
            pricing: pricing(name, v.get("pricing"))?,
        })
    }

    /// The key itself, else the one the host has under `apiKeyEnv`, else
    /// `"none"` (a local server takes any). A custom provider never fails for
    /// want of a key.
    pub fn api_key(&self, keys: &dyn KeySource) -> String {
        self.api_key
            .clone()
            .or_else(|| self.api_key_env.as_ref().and_then(|k| keys.key(k)))
            .unwrap_or_else(|| "none".into())
    }
}

/// A `type: "openai"` entry as compat options; the entry's prices over the catalog's.
///
/// The provider is `name`, [`CompatFlavor::Generic`], with the entry's URL,
/// key ([`CustomProviderConfig::api_key`]), headers and effort levels. Its
/// prices are the built-in catalog's for `name:` models, then the entry's own.
pub fn custom_compat_options(
    name: &str,
    cfg: &CustomProviderConfig,
    keys: &dyn KeySource,
) -> CompatOptions {
    let mut opts = CompatOptions::new(name, CompatFlavor::Generic, cfg.api_key(keys));
    opts.base_url = cfg.base_url.clone();
    opts.headers = cfg.headers.clone();
    opts.reasoning_effort = cfg.reasoning_effort.clone();
    opts.pricing = Catalog::builtin().provider_pricing(name);
    opts.pricing.extend(cfg.pricing.clone());
    opts
}

/// providers.json's entries by name, as JSON: each is checked when its
/// provider is first resolved, so one bad entry blocks only itself.
/// `serde_json::from_str` reads a providers.json text straight into one.
pub type CustomProviders = BTreeMap<String, Value>;

/// The raw entries of `<config_dir>/providers.json`, read through `fs`; empty
/// when there is none. A loader's helper: the registry is handed the result.
///
/// The entries are not checked here (see [`CustomProviders`]). A file whose
/// top level is not a JSON object reads as no entries.
///
/// # Errors
///
/// A [`ProviderConfigError`] naming the file when it exists but cannot be read,
/// or is not valid JSON.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use haest_core::fs::MemFs;
/// use haest_providers::load_custom_providers;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let fs = MemFs::with_files([(
///     "/home/me/.config/haest/providers.json",
///     r#"{"local": {"type": "openai", "baseURL": "http://localhost:1234/v1"}}"#,
/// )]);
/// let custom = load_custom_providers(&fs, Path::new("/home/me/.config/haest")).await.unwrap();
/// assert!(custom.contains_key("local"));
///
/// let none = load_custom_providers(&fs, Path::new("/elsewhere")).await.unwrap();
/// assert!(none.is_empty());
/// # }
/// ```
pub async fn load_custom_providers(
    fs: &dyn Fs,
    config_dir: &Path,
) -> Result<CustomProviders, ProviderConfigError> {
    let file = config_dir.join("providers.json");
    let Some(text) = read_text_if_present(fs, &file)
        .await
        .map_err(|e| ProviderConfigError(format!("{}: {e}", file.display())))?
    else {
        return Ok(CustomProviders::new());
    };
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|e| ProviderConfigError(format!("{} is not valid JSON: {e}", file.display())))?;
    Ok(match parsed {
        Value::Object(map) => map.into_iter().collect(),
        _ => CustomProviders::new(),
    })
}

/// The key `name`, or the error that names it.
pub(super) fn need(
    keys: &dyn KeySource,
    name: &str,
    provider: &str,
) -> Result<String, ProviderConfigError> {
    keys.key(name).ok_or_else(|| {
        ProviderConfigError(format!(
            "provider \"{provider}\" needs {name}, and the host has none"
        ))
    })
}

/// The built-in `paperinstruments` provider's options: Chat Completions at
/// `PAPERINSTRUMENTS_BASE_URL` (else `https://api.paperinstruments.com/v1`),
/// with the key `PAPERINSTRUMENTS_API_KEY`, its effort levels and the
/// catalog's prices.
///
/// docs.paperinstruments.com/docs/ultramarine/chat-completions (2026-10-17): top-level
/// `reasoning_effort` low/high/max only, reasoning cannot be disabled, no cost in the reply.
///
/// # Errors
///
/// A [`ProviderConfigError`] when `keys` has no `PAPERINSTRUMENTS_API_KEY`.
pub fn paper_instruments_options(
    keys: &dyn KeySource,
) -> Result<CompatOptions, ProviderConfigError> {
    let key = need(keys, "PAPERINSTRUMENTS_API_KEY", "paperinstruments")?;
    let mut opts = CompatOptions::new("paperinstruments", CompatFlavor::Generic, key);
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
