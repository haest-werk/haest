//! Why the settings could not be loaded ([`ConfigError`]), and why a value
//! could not be set ([`SetError`]).

use std::{io, path::PathBuf};

use haest_providers::ProvidersFileError;

use crate::Source;

/// Why [`Loader::load`](crate::Loader::load) failed: what is wrong, and where.
///
/// The `Display` of [`Read`](Self::Read) and [`Toml`](Self::Toml) names the
/// file; what went wrong in it is the [`source`](std::error::Error::source).
/// More variants may come in a minor release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// A file the host named with [`Loader::file`](crate::Loader::file) is
    /// not there.
    #[error("{}: no such file", path.display())]
    NoSuchFile {
        /// The file, as the loader looked for it.
        path: PathBuf,
    },
    /// A config file is there but could not be read.
    #[error("cannot read {}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// What the read hit.
        #[source]
        source: io::Error,
    },
    /// A config file is not TOML.
    #[error("{} is not valid TOML", path.display())]
    Toml {
        /// The file.
        path: PathBuf,
        /// Where in it, and what.
        #[source]
        source: toml::de::Error,
    },
    /// No setting has this name: a typo, or a setting this haest does not
    /// have (`haest settings` lists them).
    #[error("{at}: {key}: no such setting (haest settings lists them)")]
    UnknownSetting {
        /// The layer it is in: a file, a variable, a flag, a profile.
        at: Source,
        /// The name as the layer gives it; in a file, its TOML path
        /// (`tool`, `profiles.review.tool`).
        key: String,
    },
    /// A setting is wrong where it is given: its value is not one it takes,
    /// or it is set where it may not be (a project's file or a profile
    /// setting what only you may, a file setting `config_dir`). `why` says
    /// which.
    #[error("{at}: {key}: {why}")]
    Invalid {
        /// The layer it is in: a file, a variable, a flag, a profile.
        at: Source,
        /// The setting as the layer gives it; in a file, its TOML path
        /// (`max_turn_steps`, `profiles.review.tools`, `env.GH_TOKEN`).
        key: String,
        /// What is wrong with it. Never the value of a variable in `env`:
        /// it may be a key.
        why: String,
    },
    /// `profile` names a profile there is none of.
    #[error("unknown profile \"{name}\" (known: {})", known.join(", "))]
    UnknownProfile {
        /// The name, as given.
        name: String,
        /// The profiles there are: the built-in ones, then the files'.
        known: Vec<String>,
    },
    /// `<config dir>/providers.json` is there but malformed.
    #[error(transparent)]
    Providers(#[from] ProvidersFileError),
}

impl ConfigError {
    /// A value [`Config::set`](crate::Config::set) refused, given `at`.
    pub(crate) fn not_set(at: Source, e: SetError) -> Self {
        match e {
            SetError::Unknown { key } => Self::UnknownSetting { at, key },
            SetError::Value { key, why } => Self::Invalid { at, key, why },
        }
    }
}

/// Why [`Config::set`](crate::Config::set) set nothing. More variants may
/// come in a minor release.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SetError {
    /// No setting has this name.
    #[error("unknown setting \"{key}\"")]
    Unknown {
        /// The name, as given.
        key: String,
    },
    /// The value is not one the setting takes.
    #[error("{key}: {why}")]
    Value {
        /// The setting.
        key: String,
        /// What it takes, and what it was given.
        why: String,
    },
}
