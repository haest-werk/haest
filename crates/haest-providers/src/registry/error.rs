//! What is wrong with a providers.json entry, and why providers.json cannot be read.

use std::path::PathBuf;

use haest_core::words::UnknownWord;

/// What is wrong with one providers.json entry
/// ([`CustomProviderConfig::from_json`](super::CustomProviderConfig::from_json)).
/// Resolving a spec of that provider fails with
/// [`ModelError::Unavailable`](haest_core::ModelError::Unavailable), this as
/// its source; every other entry still resolves.
#[derive(Debug, thiserror::Error)]
pub enum EntryProblem {
    /// The entry is not a JSON object.
    #[error("the entry is not a JSON object")]
    NotAnObject,
    /// `type` names no wire dialect haest speaks: a typo is refused, never
    /// sent as Chat Completions. A `type` that is not a string is its JSON
    /// text here.
    #[error("\"type\": {0}")]
    UnknownType(UnknownWord),
    /// `reasoningEffort` lacks its `efforts` array or its `mandatory` boolean.
    #[error("reasoningEffort needs \"efforts\" (array) and \"mandatory\" (boolean)")]
    ReasoningEffort,
    /// A `pricing` row is not `{input, cachedInput, output, cacheWrite?}`, in numbers.
    #[error("pricing.{model} cannot be read")]
    Pricing {
        /// The model id the row is for.
        model: String,
        /// What reading the row failed with.
        source: serde_json::Error,
    },
}

/// Why `<config dir>/providers.json` could not be read
/// ([`load_custom_providers`](super::load_custom_providers)). Every variant's
/// message starts with the file's path; the io or JSON error is the source.
#[derive(Debug, thiserror::Error)]
pub enum ProvidersFileError {
    /// The file is there but could not be read.
    #[error("{}", path.display())]
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
