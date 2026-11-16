//! Why a provider cannot be built, and why providers.json cannot be read.

use std::path::PathBuf;

use haest_core::{provider::ModelError, words::UnknownWord};

/// Why [`ProviderRegistry::resolve_spec`](super::ProviderRegistry::resolve_spec)
/// cannot give a provider. Its text is said to the human as is.
///
/// The variant says what to do: [`Spec`](Self::Spec), pick another model;
/// [`MissingKey`](Self::MissingKey), hand in that key;
/// [`BadEntry`](Self::BadEntry), fix that providers.json entry.
#[derive(Debug, thiserror::Error)]
pub enum ProviderConfigError {
    /// The spec is not `provider:model`, or no provider goes by its name
    /// ([`ModelError::BadSpec`], [`ModelError::UnknownProvider`]).
    #[error(transparent)]
    Spec(#[from] ModelError),
    /// A built-in provider's key: the host's
    /// [`KeySource`](haest_core::KeySource) has none by that name.
    #[error("provider \"{provider}\" needs {key}, and the host has none")]
    MissingKey {
        /// The built-in provider.
        provider: String,
        /// The name the host was asked for (`OPENROUTER_API_KEY`).
        key: String,
    },
    /// A providers.json entry that cannot be used. Only specs of that
    /// provider fail; every other entry still resolves.
    #[error("provider \"{provider}\": {problem}")]
    BadEntry {
        /// The entry's name.
        provider: String,
        /// What is wrong with it.
        problem: EntryProblem,
    },
}

/// What is wrong with one providers.json entry
/// ([`CustomProviderConfig::from_json`](super::CustomProviderConfig::from_json)).
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
    #[error("pricing.{model}: {source}")]
    Pricing {
        /// The model id the row is for.
        model: String,
        /// What reading the row failed with.
        source: serde_json::Error,
    },
}

/// Why `<config dir>/providers.json` could not be read
/// ([`load_custom_providers`](super::load_custom_providers)). Every variant's
/// message starts with the file's path.
#[derive(Debug, thiserror::Error)]
pub enum ProvidersFileError {
    /// The file is there but could not be read.
    #[error("{}: {source}", path.display())]
    Read {
        /// The providers.json it is about.
        path: PathBuf,
        /// What reading it failed with.
        source: std::io::Error,
    },
    /// The file is not JSON.
    #[error("{} is not valid JSON: {source}", path.display())]
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
