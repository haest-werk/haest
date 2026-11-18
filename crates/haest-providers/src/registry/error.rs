//! What is wrong with a providers.json entry, and why providers.json cannot be read.

use std::{fmt, path::PathBuf};

use haest_core::words::UnknownWord;

use super::custom::ENTRY_KEYS;

/// What is wrong with one providers.json entry
/// ([`CustomProviderConfig::from_json`](super::CustomProviderConfig::from_json)).
/// Resolving a spec of that provider fails with
/// [`ModelError::Unavailable`](haest_core::ModelError::Unavailable), this as
/// its source; every other entry still resolves. Its message says it is
/// about providers.json, where haest reads entries. A host that reads an
/// entry from a file of its own (through `Deserialize` or `from_json`) names
/// that file around the message, or maps the problem to its own words.
///
/// More problems may come in a minor release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EntryProblem {
    /// The entry is not a JSON object.
    #[error("the providers.json entry is not a JSON object")]
    NotAnObject,
    /// Keys no entry has (`baseUrl` for `baseURL`), every one of them:
    /// refused, so a misspelt `baseURL` never sends the entry's key and the
    /// conversation to the dialect's own API.
    #[error(
        "the providers.json entry has {} {}; an entry takes {}",
        if keys.len() == 1 { "an unknown key" } else { "unknown keys" },
        keys.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
        ENTRY_KEYS.join(", ")
    )]
    UnknownKeys {
        /// Every such key the entry has.
        keys: Vec<UnknownKey>,
    },
    /// `baseURL`, `apiKey` or `apiKeyEnv` is there but is not a string
    /// (`null`, a number, a list): refused, so a `baseURL` a generator wrote
    /// as `null` never sends the entry's key and the conversation to the
    /// dialect's own API. An entry for that API has no `baseURL` at all, so
    /// the message for `baseURL` says to write the server's URL; for the
    /// other two, to delete the key.
    #[error(
        "providers.json \"{key}\" is {found}, not a string; {}",
        if *key == "baseURL" {
            "write the server's URL (an entry with no baseURL is one for the dialect's own API)"
        } else {
            "delete the key to leave it unset"
        }
    )]
    NotAString {
        /// The key.
        key: &'static str,
        /// What its value is (`null`, `a number`, `a list`, ...), never the
        /// value itself: an `apiKey` written as a number is a key.
        found: &'static str,
    },
    /// `type` names no wire dialect haest speaks: a typo is refused, never
    /// sent as Chat Completions. A `type` that is not a string is its JSON
    /// text here.
    #[error("providers.json \"type\": {0}")]
    UnknownType(UnknownWord),
    /// `reasoningEffort` lacks its `efforts` array or its `mandatory` boolean.
    #[error("providers.json reasoningEffort needs \"efforts\" (array) and \"mandatory\" (boolean)")]
    ReasoningEffort,
    /// A `pricing` row is not `{input, cachedInput, output, cacheWrite?}`, in numbers.
    #[error("providers.json pricing.{model} cannot be read")]
    Pricing {
        /// The model id the row is for.
        model: String,
        /// What reading the row failed with.
        source: serde_json::Error,
    },
}

/// A key a providers.json entry has that no entry takes
/// ([`EntryProblem::UnknownKeys`]), and the key it is close to. Shown as
/// the key, quoted, then `(did you mean "baseURL"?)` when one is close.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnknownKey {
    /// The key, as written.
    pub key: String,
    /// The key it is close to, when one is: the same letters in another case
    /// or with `_` or `-` between them, or at most two letters off.
    pub meant: Option<&'static str>,
}

impl UnknownKey {
    /// `key`, and the entry key it is close to.
    pub(super) fn new(key: impl Into<String>, meant: Option<&'static str>) -> Self {
        Self {
            key: key.into(),
            meant,
        }
    }
}

impl fmt::Display for UnknownKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", self.key)?;
        match self.meant {
            Some(meant) => write!(f, " (did you mean \"{meant}\"?)"),
            None => Ok(()),
        }
    }
}

/// Why `<config dir>/providers.json` could not be read
/// ([`load_custom_providers`](super::load_custom_providers)). Every variant's
/// message names the file; the io or JSON error is the source. More variants
/// may come in a minor release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProvidersFileError {
    /// The file is there but could not be read.
    #[error("cannot read {}", path.display())]
    Read {
        /// The providers.json it is about.
        path: PathBuf,
        /// What reading it failed with.
        source: std::io::Error,
    },
    /// The file is not JSON.
    #[error("{} is not valid JSON", path.display())]
    Parse {
        /// The providers.json it is about.
        path: PathBuf,
        /// Where the JSON broke.
        source: serde_json::Error,
    },
    /// The file is JSON but not an object of entries by provider name.
    #[error("{}: the top level is not a JSON object of providers by name", path.display())]
    NotAnObject {
        /// The providers.json it is about.
        path: PathBuf,
    },
}
