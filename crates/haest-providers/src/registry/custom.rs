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

use super::{EntryProblem, ProviderConfigError, ProvidersFileError};
use crate::{
    catalog::Catalog,
    compat::{CompatFlavor, CompatOptions},
};

haest_core::word_enum! {
/// The wire dialect of a custom provider: its providers.json `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect ("provider type") {
    /// Chat Completions (`"type": "openai"`, or no `type`).
    OpenAi = "openai",
    /// The Messages API (`"type": "anthropic"`).
    Anthropic = "anthropic",
}
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
/// | `type` | [`dialect`](Self::dialect) | `"openai"` (or none) for Chat Completions, `"anthropic"` for the Messages API |
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

/// `type`: no value is Chat Completions; anything but a known word is refused.
fn dialect(v: Option<&Value>) -> Result<Dialect, EntryProblem> {
    let word = match v {
        None | Some(Value::Null) => return Ok(Dialect::OpenAi),
        Some(Value::String(word)) => word.clone(),
        Some(other) => other.to_string(),
    };
    word.parse().map_err(EntryProblem::UnknownType)
}

fn reasoning(v: Option<&Value>) -> Result<Option<Reasoning>, EntryProblem> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let (Some(efforts), Some(mandatory)) = (
        v.get("efforts").and_then(Value::as_array),
        v.get("mandatory").and_then(Value::as_bool),
    ) else {
        return Err(EntryProblem::ReasoningEffort);
    };
    // A level haest has no name for can never be chosen; it is skipped, not refused.
    let efforts = efforts
        .iter()
        .filter_map(|e| e.as_str()?.parse::<OpenRouterEffort>().ok())
        .collect();
    Ok(Some(Reasoning { mandatory, efforts }))
}

fn pricing(v: Option<&Value>) -> Result<HashMap<String, ModelPricing>, EntryProblem> {
    v.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(model, price)| {
            let row =
                serde_json::from_value(price.clone()).map_err(|source| EntryProblem::Pricing {
                    model: model.clone(),
                    source,
                })?;
            Ok((model.clone(), row))
        })
        .collect()
}

impl CustomProviderConfig {
    /// Read the providers.json entry `name`; a mistyped dialect, a malformed
    /// effort list or price is refused, never sent or booked as garbage.
    ///
    /// No `type` is Chat Completions. An effort level haest has no name for is
    /// skipped.
    ///
    /// # Errors
    ///
    /// [`ProviderConfigError::BadEntry`] naming the entry when it is not a
    /// JSON object, its `type` is neither `"openai"` nor `"anthropic"`,
    /// `reasoningEffort` lacks an `efforts` array or a `mandatory` boolean, or
    /// a `pricing` row lacks a numeric `input`, `cachedInput` or `output`, or
    /// has a `cacheWrite` that is not a number.
    ///
    /// ```
    /// use haest_providers::{CustomProviderConfig, Dialect};
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
        let bad = |problem| ProviderConfigError::BadEntry {
            provider: name.to_string(),
            problem,
        };
        if !v.is_object() {
            return Err(bad(EntryProblem::NotAnObject));
        }
        let headers = v
            .get("headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect();
        Ok(Self {
            dialect: dialect(v.get("type")).map_err(bad)?,
            base_url: opt_string(v, "baseURL"),
            api_key: opt_string(v, "apiKey"),
            api_key_env: opt_string(v, "apiKeyEnv"),
            headers,
            reasoning_effort: reasoning(v.get("reasoningEffort")).map_err(bad)?,
            pricing: pricing(v.get("pricing")).map_err(bad)?,
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
pub(crate) fn custom_compat_options(
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
/// The entries are not checked here (see [`CustomProviders`]).
///
/// # Errors
///
/// A [`ProvidersFileError`] naming the file when it exists but cannot be read,
/// is not valid JSON, or its top level is not a JSON object.
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
) -> Result<CustomProviders, ProvidersFileError> {
    let path = config_dir.join("providers.json");
    let text = match read_text_if_present(fs, &path).await {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(CustomProviders::new()),
        Err(source) => return Err(ProvidersFileError::Read { path, source }),
    };
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(parsed) => parsed,
        Err(source) => return Err(ProvidersFileError::Parse { path, source }),
    };
    match parsed {
        Value::Object(map) => Ok(map.into_iter().collect()),
        _ => Err(ProvidersFileError::NotAnObject { path }),
    }
}

/// The key `key`, or the error that names it.
pub(super) fn need(
    keys: &dyn KeySource,
    key: &str,
    provider: &str,
) -> Result<String, ProviderConfigError> {
    keys.key(key)
        .ok_or_else(|| ProviderConfigError::MissingKey {
            provider: provider.into(),
            key: key.into(),
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
pub(crate) fn paper_instruments_options(
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
