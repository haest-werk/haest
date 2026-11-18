//! `providers.json`: the custom providers a haest config dir declares.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::Path,
};

use haest_core::{
    KeySource,
    catalog::{ModelPricing, OpenRouterEffort, Reasoning},
    fs::{Fs, read_text_if_present},
};
use serde::{Deserialize, Deserializer, de::Error as _};
use serde_json::Value;

use super::{EntryProblem, ProvidersFileError};
use crate::{
    anthropic::AnthropicOptions,
    catalog::Catalog,
    compat::CompatOptions,
    send::{HeaderNames, REDACTED, UrlDebug},
};

haest_core::word_enum! {
/// The wire dialect of a custom provider: its providers.json `type`. More
/// may come in a minor release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
/// A key this table does not list is refused
/// ([`EntryProblem::UnknownKey`]), naming the key meant when one is close:
/// read past, a misspelt `baseURL` (`baseUrl`) would send the entry's key
/// and the conversation to the dialect's own API.
///
/// `reasoningEffort` and `pricing` apply to `type: "openai"` only.
/// `reasoningEffort`: send a top-level `reasoning_effort`, fitted to the listed
/// levels ([`CompatOptions::reasoning_effort`]). Absent = no effort is sent.
/// `pricing`: USD per 1M tokens per model id, for an API that returns no cost; a
/// catalog row's `pricing` fills in for a model not listed here.
///
/// A host builds one in Rust as well ([`openai`](Self::openai),
/// [`anthropic`](Self::anthropic), or the fields over `Default`, which is an
/// entry with no keys: Chat Completions at OpenAI's own API) and hands it to
/// [`ProviderRegistry::with_custom_provider`](super::ProviderRegistry::with_custom_provider).
/// It deserializes from an entry's JSON as [`from_json`](Self::from_json)
/// reads it.
///
/// Its Debug output shows neither `apiKey` nor a header's value.
///
/// ```
/// use haest_providers::{CustomProviderConfig, ProviderRegistry};
///
/// let gateway = CustomProviderConfig {
///     api_key_env: Some("GATEWAY_API_KEY".into()),
///     ..CustomProviderConfig::anthropic("https://llm-gateway.example.com")
/// };
/// let keys = |name: &str| (name == "GATEWAY_API_KEY").then(|| "gw-key".to_string());
/// assert_eq!(gateway.key(&keys).as_deref(), Some("gw-key"));
///
/// let registry = ProviderRegistry::new(keys).with_custom_provider("gateway", gateway);
/// assert!(registry.resolve("gateway:claude-opus-5-5").is_ok());
/// ```
#[derive(Clone, PartialEq)]
pub struct CustomProviderConfig {
    /// `type`: the wire dialect.
    pub dialect: Dialect,
    /// `baseURL`; None = the dialect's own API (OpenAI's or Anthropic's).
    /// Its path shows in Debug output and errors: a key goes in `apiKey`,
    /// `apiKeyEnv` or `headers`, never in the path.
    ///
    /// One with an `@` past its host fails the provider's resolve
    /// ([`ProviderRegistry::resolve`](super::ProviderRegistry::resolve)): a
    /// password holding an unescaped `/`, `?` or `#` ends the host a URL
    /// parser reads there, so it goes in percent-encoded (`/` as `%2F`).
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

impl fmt::Debug for CustomProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomProviderConfig")
            .field("dialect", &self.dialect)
            .field("base_url", &self.base_url.as_deref().map(UrlDebug))
            .field("api_key", &self.api_key.as_ref().map(|_| REDACTED))
            .field("api_key_env", &self.api_key_env)
            .field("headers", &HeaderNames(&self.headers))
            .field("reasoning_effort", &self.reasoning_effort)
            .field("pricing", &self.pricing)
            .finish()
    }
}

/// An entry with no keys: Chat Completions at OpenAI's own API, no key of
/// its own, no headers, no effort, no prices.
impl Default for CustomProviderConfig {
    fn default() -> Self {
        Self {
            dialect: Dialect::OpenAi,
            base_url: None,
            api_key: None,
            api_key_env: None,
            headers: Vec::new(),
            reasoning_effort: None,
            pricing: HashMap::new(),
        }
    }
}

/// An entry's JSON, read and checked as [`CustomProviderConfig::from_json`]
/// does.
impl<'de> Deserialize<'de> for CustomProviderConfig {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        Self::from_json(&Value::deserialize(de)?).map_err(D::Error::custom)
    }
}

/// What a custom provider sends when it has no key: a local server takes any.
const NO_KEY: &str = "none";

/// The keys a providers.json entry has, in the order its docs list them.
pub(super) const ENTRY_KEYS: [&str; 7] = [
    "type",
    "baseURL",
    "apiKey",
    "apiKeyEnv",
    "headers",
    "reasoningEffort",
    "pricing",
];

/// The entry key `key` is a misspelling of, if one is close: the same
/// letters in another case or with `_` or `-` between them
/// (`baseUrl`, `base_url`), or at most two letters off (`header`).
fn meant(key: &str) -> Option<&'static str> {
    let plain = |k: &str| -> Vec<u8> {
        k.bytes()
            .filter(u8::is_ascii_alphanumeric)
            .map(|b| b.to_ascii_lowercase())
            .collect()
    };
    let written = plain(key);
    ENTRY_KEYS
        .iter()
        .map(|known| (edit_distance(&written, &plain(known)), *known))
        .filter(|(distance, _)| *distance <= 2)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, known)| known)
}

/// How many letters must be added, removed or changed to make `a` into `b`.
fn edit_distance(a: &[u8], b: &[u8]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let changed = diagonal + usize::from(ca != cb);
            diagonal = row[j + 1];
            row[j + 1] = changed.min(row[j] + 1).min(diagonal + 1);
        }
    }
    row[b.len()]
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
        .filter_map(|e| e.as_str()?.parse::<OpenRouterEffort>().ok());
    Ok(Some(Reasoning::new(efforts).with_mandatory(mandatory)))
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
    /// A Chat Completions server at `base_url` (`"type": "openai"`).
    pub fn openai(base_url: impl Into<String>) -> Self {
        Self {
            base_url: Some(base_url.into()),
            ..Self::default()
        }
    }

    /// A Messages API server at `base_url` (`"type": "anthropic"`).
    pub fn anthropic(base_url: impl Into<String>) -> Self {
        Self {
            dialect: Dialect::Anthropic,
            ..Self::openai(base_url)
        }
    }

    /// Read the providers.json entry `name`; a key the entry does not have, a
    /// mistyped dialect, a malformed effort list or price is refused, never
    /// sent or booked as garbage.
    ///
    /// No `type` is Chat Completions. An effort level haest has no name for is
    /// skipped.
    ///
    /// # Errors
    ///
    /// The [`EntryProblem`] when the entry is not a JSON object, has a key
    /// [`CustomProviderConfig`]'s table does not list (`baseUrl` for
    /// `baseURL`), its `type`
    /// is neither `"openai"` nor `"anthropic"`,
    /// `reasoningEffort` lacks an `efforts` array or a `mandatory` boolean, or
    /// a `pricing` row lacks a numeric `input`, `cachedInput` or `output`, or
    /// has a `cacheWrite` that is not a number.
    ///
    /// ```
    /// use haest_providers::{CustomProviderConfig, Dialect, EntryProblem};
    /// use serde_json::json;
    ///
    /// # fn main() -> Result<(), EntryProblem> {
    /// let entry = json!({"type": "anthropic", "baseURL": "https://llm-gateway.example.com",
    ///                    "apiKeyEnv": "GATEWAY_API_KEY", "headers": {"X-Team": "platform"}});
    /// let cfg = CustomProviderConfig::from_json(&entry)?;
    /// assert_eq!(cfg.dialect, Dialect::Anthropic);
    /// assert_eq!(cfg.headers, [("X-Team".to_string(), "platform".to_string())]);
    ///
    /// let bad = json!({"pricing": {"m": {"input": 1}}});
    /// assert!(CustomProviderConfig::from_json(&bad).is_err());
    /// # Ok(())
    /// # }
    /// ```
    pub fn from_json(v: &Value) -> Result<Self, EntryProblem> {
        let Some(entry) = v.as_object() else {
            return Err(EntryProblem::NotAnObject);
        };
        // Read past, a misspelt `baseURL` would make this an entry for the
        // dialect's own API, which would get the entry's key.
        if let Some(key) = entry.keys().find(|k| !ENTRY_KEYS.contains(&k.as_str())) {
            return Err(EntryProblem::UnknownKey {
                key: key.clone(),
                meant: meant(key),
            });
        }
        let headers = v
            .get("headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect();
        Ok(Self {
            dialect: dialect(v.get("type"))?,
            base_url: opt_string(v, "baseURL"),
            api_key: opt_string(v, "apiKey"),
            api_key_env: opt_string(v, "apiKeyEnv"),
            headers,
            reasoning_effort: reasoning(v.get("reasoningEffort"))?,
            pricing: pricing(v.get("pricing"))?,
        })
    }

    /// The key this entry sends: `apiKey` itself, else the one the host's
    /// `keys` has under `apiKeyEnv`; None when neither is there (the provider
    /// then sends a placeholder, as a local server takes any key: a custom
    /// provider never fails for want of one).
    ///
    /// `keys` is any key source; one a registry holds is borrowed from its
    /// handle with `as_ref` (`entry.key(registry.keys().as_ref())`: a
    /// `&KeySourceRef` itself is refused, as the compiler tries the
    /// closure impl of [`KeySource`] on it first).
    pub fn key(&self, keys: &dyn KeySource) -> Option<String> {
        self.api_key
            .clone()
            .or_else(|| self.api_key_env.as_ref().and_then(|k| keys.key(k)))
    }

    /// [`key`](Self::key), or the placeholder.
    fn sent_key(&self, keys: &dyn KeySource) -> String {
        self.key(keys).unwrap_or_else(|| NO_KEY.into())
    }

    /// A `type: "anthropic"` entry as the options of the provider `name`: the
    /// entry's URL, key ([`key`](Self::key)) and headers.
    pub(crate) fn anthropic_options(&self, name: &str, keys: &dyn KeySource) -> AnthropicOptions {
        AnthropicOptions {
            base_url: self.base_url.clone(),
            headers: self.headers.clone(),
            ..AnthropicOptions::new(name, self.sent_key(keys))
        }
    }

    /// A `type: "openai"` entry as the options of the provider `name`, the
    /// plain body: the entry's URL, key ([`key`](Self::key)), headers and
    /// effort levels. Its prices are the built-in catalog's for `name:`
    /// models, then the entry's own over them.
    pub(crate) fn compat_options(&self, name: &str, keys: &dyn KeySource) -> CompatOptions {
        let opts = CompatOptions {
            base_url: self.base_url.clone(),
            headers: self.headers.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            pricing: Catalog::builtin().provider_pricing(name),
            ..CompatOptions::new(name, self.sent_key(keys))
        };
        opts.with_pricing(self.pricing.clone())
    }
}

/// providers.json's entries by name.
///
/// An entry read from JSON (`serde_json::from_str` reads a providers.json
/// text straight into one; [`load_custom_providers`] reads the file) is
/// checked when its provider is first resolved, so one bad entry blocks only
/// itself. An entry a host builds in Rust ([`with_entry`](Self::with_entry),
/// [`insert`](Self::insert), `collect`) is a [`CustomProviderConfig`]
/// already.
///
/// Its Debug output names the entries and shows none of them: an entry may
/// hold its `apiKey`.
///
/// ```
/// use haest_providers::{CustomProviderConfig, CustomProviders, EntryProblem};
///
/// # fn main() -> Result<(), serde_json::Error> {
/// let file: CustomProviders = serde_json::from_str(
///     r#"{"local": {"baseURL": "http://localhost:1234/v1"}, "bad": {"type": "anthropc"}}"#,
/// )?;
/// let custom = file.with_entry("gateway", CustomProviderConfig::anthropic("https://gw"));
/// assert_eq!(custom.names().collect::<Vec<_>>(), ["bad", "gateway", "local"]);
/// assert!(matches!(custom.get("bad"), Some(Err(EntryProblem::UnknownType(_)))));
/// assert!(matches!(custom.get("local"), Some(Ok(_))));
/// assert!(custom.get("nope").is_none());
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Default, PartialEq)]
pub struct CustomProviders {
    entries: BTreeMap<String, Entry>,
}

/// One entry, as it was handed in.
#[derive(Clone, PartialEq)]
enum Entry {
    /// Read from JSON: checked on its first resolve.
    Json(Value),
    /// Built in Rust.
    Config(CustomProviderConfig),
}

impl CustomProviders {
    /// No entries.
    pub fn new() -> Self {
        Self::default()
    }

    /// These entries and `config` as the entry `name` (in place of one of
    /// that name).
    #[must_use]
    pub fn with_entry(mut self, name: impl Into<String>, config: CustomProviderConfig) -> Self {
        self.insert(name, config);
        self
    }

    /// `config` as the entry `name`, in place of one of that name.
    pub fn insert(&mut self, name: impl Into<String>, config: CustomProviderConfig) {
        self.entries.insert(name.into(), Entry::Config(config));
    }

    /// The entry `name`, checked; None when there is none.
    ///
    /// # Errors
    ///
    /// Inside the `Some`: the [`EntryProblem`] of an entry read from JSON
    /// that cannot be used ([`CustomProviderConfig::from_json`]).
    pub fn get(&self, name: &str) -> Option<Result<CustomProviderConfig, EntryProblem>> {
        Some(match self.entries.get(name)? {
            Entry::Json(raw) => CustomProviderConfig::from_json(raw),
            Entry::Config(config) => Ok(config.clone()),
        })
    }

    /// Whether there is an entry `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// The entries' names, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// How many entries there are.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries read from JSON, checked when each is first resolved.
    fn from_json_entries(entries: impl IntoIterator<Item = (String, Value)>) -> Self {
        let entries = entries
            .into_iter()
            .map(|(name, raw)| (name, Entry::Json(raw)))
            .collect();
        Self { entries }
    }
}

/// The names only: an entry may hold its `apiKey`.
impl fmt::Debug for CustomProviders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CustomProviders")
            .field(&self.names().collect::<Vec<_>>())
            .finish()
    }
}

/// A providers.json text: an object of entries by name, each checked when its
/// provider is first resolved.
impl<'de> Deserialize<'de> for CustomProviders {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let entries = BTreeMap::<String, Value>::deserialize(de)?;
        Ok(Self::from_json_entries(entries))
    }
}

impl<K: Into<String>> FromIterator<(K, CustomProviderConfig)> for CustomProviders {
    fn from_iter<I: IntoIterator<Item = (K, CustomProviderConfig)>>(entries: I) -> Self {
        let mut out = Self::new();
        out.extend(entries);
        out
    }
}

impl<K: Into<String>> Extend<(K, CustomProviderConfig)> for CustomProviders {
    fn extend<I: IntoIterator<Item = (K, CustomProviderConfig)>>(&mut self, entries: I) {
        for (name, config) in entries {
            self.insert(name, config);
        }
    }
}

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
/// # async fn main() -> Result<(), haest_providers::ProvidersFileError> {
/// let fs = MemFs::from_files([(
///     "/home/me/.config/haest/providers.json",
///     r#"{"local": {"type": "openai", "baseURL": "http://localhost:1234/v1"}}"#,
/// )]);
/// let custom = load_custom_providers(&fs, Path::new("/home/me/.config/haest")).await?;
/// assert!(custom.contains("local"));
///
/// let none = load_custom_providers(&fs, Path::new("/elsewhere")).await?;
/// assert!(none.is_empty());
/// # Ok(())
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
        Value::Object(map) => Ok(CustomProviders::from_json_entries(map)),
        _ => Err(ProvidersFileError::NotAnObject { path }),
    }
}
