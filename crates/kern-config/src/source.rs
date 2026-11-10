//! Where each value came from, and what trust decided: what `kern settings`
//! shows beside every value.

use std::{collections::BTreeMap, fmt, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Default,
    File(PathBuf),
    Profile(String),
    Env(String),
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

/// The project's `.kern/config.toml`, if there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectFile {
    None,
    Read(PathBuf),
    /// Outside every trusted dir: not read.
    Untrusted(PathBuf),
}

/// Where each value came from, and what trust decided.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub(crate) sources: BTreeMap<String, Source>,
    pub project_file: ProjectFile,
    /// The working directory is inside a trusted dir.
    pub trusted: bool,
}

impl Resolved {
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
