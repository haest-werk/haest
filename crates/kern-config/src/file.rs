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

use kern_core::{context::project_chain, fs::Fs, fs::read_text_if_present};
use toml::{Table, Value};

use crate::{
    registry::{Kind, Scope, find},
    source::Source,
    stack::{Entries, Layer},
    value::{check, named_var},
};

/// A file the host named (it must be there), else `<config dir>/config.toml`
/// (None when there is none).
pub async fn read_user_file(
    fs: &dyn Fs,
    named: Option<PathBuf>,
    config_dir: &Path,
) -> Result<Option<Layer>, String> {
    let (path, required) = match named {
        Some(path) => (path, true),
        None => (config_dir.join("config.toml"), false),
    };
    match read(fs, &path).await? {
        Some(text) => parse_file(&path, &text, FileKind::User).map(Some),
        None if required => Err(format!("{}: no such file", path.display())),
        None => Ok(None),
    }
}

/// A trusted project's `.kern/config.toml`.
pub async fn read_project_file(fs: &dyn Fs, path: &Path) -> Result<Layer, String> {
    let text = read(fs, path).await?.unwrap_or_default();
    parse_file(path, &text, FileKind::Project)
}

/// The nearest `.kern/config.toml` from `cwd` up, not counting the home dir's own.
pub async fn find_project_file(fs: &dyn Fs, cwd: &Path, home: &Path) -> Option<PathBuf> {
    for dir in project_chain(cwd, home).iter().rev() {
        let path = dir.join(".kern/config.toml");
        if fs.exists(&path).await {
            return Some(path);
        }
    }
    None
}

/// `<root>/.kern/config.toml`: the root is what must be trusted.
pub fn project_root(path: &Path) -> &Path {
    path.parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("/"))
}

async fn read(fs: &dyn Fs, path: &Path) -> Result<Option<String>, String> {
    read_text_if_present(fs, path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Which file it is decides what it may set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    /// `<config dir>/config.toml`.
    User,
    /// A trusted project's `.kern/config.toml`.
    Project,
}

fn parse_file(path: &Path, text: &str, kind: FileKind) -> Result<Layer, String> {
    let at = |e: String| format!("{}: {e}", path.display());
    let table: Table = text
        .parse()
        .map_err(|e: toml::de::Error| at(e.to_string()))?;
    let source = Source::File(path.to_path_buf());
    let mut layer = Layer::default();
    for (key, value) in &table {
        if key != "profiles" {
            let entries = entries(key, value, kind, false).map_err(at)?;
            layer
                .values
                .extend(entries.into_iter().map(|(def, v)| (def, v, source.clone())));
            continue;
        }
        let Value::Table(profiles) = value else {
            return Err(at("profiles must be a table of [profiles.<name>]".into()));
        };
        for (name, body) in profiles {
            let Value::Table(body) = body else {
                return Err(at(format!("profiles.{name} must be a table")));
            };
            let mut table = Entries::new();
            for (key, value) in body {
                let entries = entries(key, value, kind, true)
                    .map_err(|e| at(format!("profiles.{name}: {e}")))?;
                table.extend(entries);
            }
            layer.profiles.insert(name.clone(), table);
        }
    }
    Ok(layer)
}

/// One `key = value` of a file, checked: a known setting, one this file (and
/// a profile in it) may set, a value of its kind. A table of variables
/// (`[env]`) is one value per variable.
fn entries(key: &str, value: &Value, kind: FileKind, in_profile: bool) -> Result<Entries, String> {
    let def =
        find(key).ok_or_else(|| format!("unknown setting \"{key}\" (kern settings lists them)"))?;
    match def.scope {
        Scope::Process => {
            return Err(format!(
                "{key} is set by {} or --{} only: it says where this file is",
                def.env(),
                def.flag()
            ));
        }
        Scope::User if kind == FileKind::Project => {
            return Err(format!(
                "{key} can't be set in a project's file: a project does not trust itself"
            ));
        }
        _ => {}
    }
    if in_profile && !def.profile_may_set() {
        return Err(format!("{key} can't be set in a profile"));
    }
    let checked = |raw: &str| check(def, raw).map_err(|e| format!("{key}: {e}"));
    if def.kind == Kind::Vars {
        let Value::Table(vars) = value else {
            return Err(format!("{key} must be a table: [{key}] NAME = \"value\""));
        };
        return vars
            .iter()
            .map(|(name, value)| {
                let text = raw(value)
                    .filter(|_| !value.is_array())
                    .ok_or_else(|| format!("{key}.{name}: a string, number or true/false"))?;
                Ok((
                    def,
                    named_var(name, &text).map_err(|e| format!("{key}: {e}"))?,
                ))
            })
            .collect();
    }
    let raw = raw(value)
        .ok_or_else(|| format!("{key}: a string, number, true/false or a list of strings"))?;
    Ok(vec![(def, checked(&raw)?)])
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
