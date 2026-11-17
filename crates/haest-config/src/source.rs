//! Where each value came from, and what trust decided: what `haest settings`
//! shows beside every value.

use std::{collections::BTreeMap, fmt, path::PathBuf};

/// The layer a setting's value came from.
///
/// Its [`Display`](fmt::Display) is what `haest settings` shows: `default`, a
/// file's path, `profile claude`, `HAEST_MODEL`, `--model`. More layers may
/// come in a minor release.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
    /// No layer set it: the defaults the loader started from.
    Default,
    /// A config file: yours, one the host named, or the project's.
    File(PathBuf),
    /// The profile of this name.
    Profile(String),
    /// The variable of this name (`HAEST_MODEL`, or `XDG_CONFIG_HOME` for a
    /// dir [`Loader::xdg_dirs`](crate::Loader::xdg_dirs) set).
    Env(String),
    /// The flag of this name, without the dashes in front (`model`).
    Flag(String),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Default => f.write_str("default"),
            Source::File(p) => write!(f, "{}", p.display()),
            Source::Profile(n) => write!(f, "profile {n}"),
            Source::Env(v) => f.write_str(v),
            Source::Flag(name) => write!(f, "--{name}"),
        }
    }
}

/// The project's `.haest/config.toml`, if there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectFile {
    /// There is none from the working dir up, or the loader reads none
    /// (no [`Loader::project_file`](crate::Loader::project_file)).
    None,
    /// Read: its project is inside a trusted dir.
    Read(PathBuf),
    /// Outside every trusted dir: not read.
    Untrusted(PathBuf),
}

/// Where each value came from, and what trust decided: [`Loaded::resolved`](crate::Loaded::resolved).
#[derive(Debug, Clone)]
pub struct Resolved {
    pub(crate) sources: BTreeMap<String, Source>,
    /// The project's `.haest/config.toml`: read, skipped, or none.
    pub project_file: ProjectFile,
    /// The working directory is inside a trusted dir.
    pub trusted: bool,
}

impl Resolved {
    /// Where `key`'s value came from: the last layer that set it, else
    /// [`Source::Default`] (also for a key that is no setting).
    ///
    /// For `env`, whose layers add variables, the last layer that passed one;
    /// each variable's own is [`var_source`](Self::var_source).
    pub fn source(&self, key: &str) -> Source {
        self.sources.get(key).cloned().unwrap_or(Source::Default)
    }

    /// Where variable `name` of a [`crate::Kind::Vars`] setting (`env`) came from.
    pub fn var_source(&self, key: &str, name: &str) -> Source {
        self.source(&var_key(key, name))
    }
}

/// The key a variable's source is kept under: `env.GH_TOKEN`.
pub(crate) fn var_key(key: &str, name: &str) -> String {
    format!("{key}.{name}")
}
