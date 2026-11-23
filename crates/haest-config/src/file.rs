//! One config file: its settings, and the profiles it defines.
//!
//! ```toml
//! tools = ["read", "edit", "bash"]
//! context = ["agents"]
//! trusted_dirs = ["~/projects"]
//!
//! [env]
//! GH_TOKEN = "${GITHUB_TOKEN}"
//!
//! [profiles.review]
//! tools = ["read", "grep", "glob"]
//! ```
//!
//! An unknown key is an error naming the file: a typo must not silently do nothing.

use std::path::{Path, PathBuf};

use haest_core::{context::project_chain, fs::Fs, fs::read_text_if_present};
use toml::{Table, Value};

use crate::{
    ConfigError,
    registry::{Kind, Scope, find, project_limit},
    source::Source,
    stack::{Entries, Layer},
    value::{check, named_var},
};

/// A file the host named (it must be there), else `<config dir>/config.toml`
/// (None when there is none).
pub(crate) async fn read_user_file(
    fs: &dyn Fs,
    named: Option<PathBuf>,
    config_dir: &Path,
) -> Result<Option<Layer>, ConfigError> {
    let (path, required) = match named {
        Some(path) => (path, true),
        None => (config_dir.join("config.toml"), false),
    };
    match read(fs, &path).await? {
        Some(text) => parse_file(&path, &text, FileKind::User).map(Some),
        None if required => Err(ConfigError::NoSuchFile { path }),
        None => Ok(None),
    }
}

/// A trusted project's `.haest/config.toml`.
pub(crate) async fn read_project_file(fs: &dyn Fs, path: &Path) -> Result<Layer, ConfigError> {
    let text = read(fs, path).await?.unwrap_or_default();
    parse_file(path, &text, FileKind::Project)
}

/// The nearest `.haest/config.toml` from `cwd` up, not counting the home dir's own.
pub(crate) async fn find_project_file(fs: &dyn Fs, cwd: &Path, home: &Path) -> Option<PathBuf> {
    for dir in project_chain(cwd, home).iter().rev() {
        let path = dir.join(".haest/config.toml");
        if fs.exists(&path).await {
            return Some(path);
        }
    }
    None
}

/// `<root>/.haest/config.toml`: the root is what must be trusted.
pub(crate) fn project_root(path: &Path) -> &Path {
    path.parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("/"))
}

async fn read(fs: &dyn Fs, path: &Path) -> Result<Option<String>, ConfigError> {
    read_text_if_present(fs, path)
        .await
        .map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })
}

/// Which file it is decides what it may set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    /// `<config dir>/config.toml`.
    User,
    /// A trusted project's `.haest/config.toml`.
    Project,
}

fn parse_file(path: &Path, text: &str, kind: FileKind) -> Result<Layer, ConfigError> {
    let at = Source::File(path.to_path_buf());
    let table: Table = text.parse().map_err(|source| ConfigError::Toml {
        path: path.to_path_buf(),
        source,
    })?;
    let mut layer = Layer::default();
    for (key, value) in &table {
        if key != "profiles" {
            let entries = Entry {
                at: &at,
                kind,
                profile: None,
            }
            .read(key, value)?;
            layer
                .values
                .extend(entries.into_iter().map(|(def, v)| (def, v, at.clone())));
            continue;
        }
        let Value::Table(profiles) = value else {
            return Err(invalid(
                &at,
                "profiles",
                "must be a table of [profiles.<name>]",
            ));
        };
        for (name, body) in profiles {
            let Value::Table(body) = body else {
                return Err(invalid(&at, &format!("profiles.{name}"), "must be a table"));
            };
            let mut table = Entries::new();
            let entry = Entry {
                at: &at,
                kind,
                profile: Some(name),
            };
            for (key, value) in body {
                table.extend(entry.read(key, value)?);
            }
            layer.profiles.insert(name.clone(), table);
        }
    }
    Ok(layer)
}

/// The setting at TOML path `key` in the file `at` is wrong: `why`.
fn invalid(at: &Source, key: &str, why: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        at: at.clone(),
        key: key.to_string(),
        why: why.into(),
    }
}

/// Where in a file an entry is: the file, which kind, and the
/// `[profiles.<name>]` table it is in, if any.
#[derive(Clone, Copy)]
struct Entry<'a> {
    at: &'a Source,
    kind: FileKind,
    profile: Option<&'a str>,
}

impl Entry<'_> {
    /// One `key = value`, checked: a known setting, one this file (and a
    /// profile in it) may set, a value of its kind, and in a project's file
    /// nothing its [`project_limit`] refuses. A table of variables
    /// (`[env]`) is one value per variable. An error names the entry by its
    /// TOML path.
    fn read(self, key: &str, value: &Value) -> Result<Entries, ConfigError> {
        let path = match self.profile {
            Some(name) => format!("profiles.{name}.{key}"),
            None => key.to_string(),
        };
        let Some(def) = find(key) else {
            return Err(ConfigError::UnknownSetting {
                at: self.at.clone(),
                key: path,
            });
        };
        let refused = |why: &str| invalid(self.at, &path, why);
        match def.scope {
            Scope::Process => {
                let (var, flag) = (def.env(), def.flag());
                let only = format!("set by {var} or --{flag} only: it says where this file is");
                return Err(refused(&only));
            }
            Scope::User if self.kind == FileKind::Project => {
                return Err(refused(
                    "can't be set in a project's file: a project does not trust itself",
                ));
            }
            _ => {}
        }
        if self.profile.is_some() && !def.profile_may_set() {
            return Err(refused("can't be set in a profile"));
        }
        if def.kind == Kind::Table {
            let Value::Table(table) = value else {
                return Err(refused(&format!("must be a table: [{key}]")));
            };
            let checked = crate::table::inline(table.clone());
            if let (FileKind::Project, Some(limit)) = (self.kind, project_limit(key)) {
                (limit.check)(&checked).map_err(|why| refused(&why))?;
            }
            return Ok(vec![(def, checked)]);
        }
        if def.kind == Kind::Vars {
            let Value::Table(vars) = value else {
                return Err(refused(&format!(
                    "must be a table: [{key}] NAME = \"value\""
                )));
            };
            return vars
                .iter()
                .map(|(name, value)| {
                    let var = format!("{path}.{name}");
                    let text = raw(value).filter(|_| !value.is_array()).ok_or_else(|| {
                        invalid(self.at, &var, "must be a string, number or true/false")
                    })?;
                    let checked =
                        named_var(name, &text).map_err(|why| invalid(self.at, &var, why))?;
                    Ok((def, checked))
                })
                .collect();
        }
        let raw = raw(value)
            .ok_or_else(|| refused("must be a string, number, true/false or a list of strings"))?;
        let checked = check(def, &raw).map_err(|why| refused(&why))?;
        Ok(vec![(def, checked)])
    }
}

/// A TOML value as the text a variable would hold; an empty list is `none`.
fn raw(value: &Value) -> Option<String> {
    Some(match value {
        Value::String(s) => s.clone(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Array(items) if items.is_empty() => "none".into(),
        Value::Array(items) => items
            .iter()
            .map(|v| v.as_str().map(String::from))
            .collect::<Option<Vec<_>>>()?
            .join(","),
        _ => return None,
    })
}
