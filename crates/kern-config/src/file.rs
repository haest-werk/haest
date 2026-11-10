//! One config file: its settings, and the profiles it defines.
//!
//! ```toml
//! tools = ["read", "edit", "bash"]
//! context = ["agents"]
//! trusted_dirs = ["~/projects"]
//!
//! [profiles.review]
//! tools = ["read", "grep", "glob"]
//! ```
//!
//! An unknown key is an error naming the file: a typo must not silently do nothing.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use kern_core::fs::{Fs, normalize};
use toml::{Table, Value};

use crate::{
    registry::{Def, Scope, find},
    value::check,
};

/// A file the host named (it must be there), else `<config dir>/config.toml`
/// (None when there is none).
pub async fn read_user_file(
    fs: &dyn Fs,
    named: Option<PathBuf>,
    config_dir: &Path,
) -> Result<Option<FileLayer>, String> {
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
pub async fn read_project_file(fs: &dyn Fs, path: &Path) -> Result<FileLayer, String> {
    let text = read(fs, path).await?.unwrap_or_default();
    parse_file(path, &text, FileKind::Project)
}

/// The nearest `.kern/config.toml` from `cwd` up, not counting the home dir's own.
pub async fn find_project_file(fs: &dyn Fs, cwd: &Path, home: &Path) -> Option<PathBuf> {
    let home = normalize(home);
    let mut dir = normalize(cwd);
    loop {
        if dir == home {
            return None;
        }
        let path = dir.join(".kern/config.toml");
        if fs.exists(&path).await {
            return Some(path);
        }
        dir = dir.parent()?.to_path_buf();
    }
}

/// `<root>/.kern/config.toml`: the root is what must be trusted.
pub fn project_root(path: &Path) -> &Path {
    path.parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("/"))
}

/// The file's text; None when there is no file.
async fn read(fs: &dyn Fs, path: &Path) -> Result<Option<String>, String> {
    if !fs.exists(path).await {
        return Ok(None);
    }
    let bytes = fs
        .read(path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// A setting's checked value, by its definition.
pub type Entries = Vec<(&'static Def, String)>;

#[derive(Debug, Default)]
pub struct FileLayer {
    pub path: PathBuf,
    pub base: Entries,
    pub profiles: BTreeMap<String, Entries>,
}

/// Which file it is decides what it may set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    /// `<config dir>/config.toml`.
    User,
    /// A trusted project's `.kern/config.toml`.
    Project,
}

fn parse_file(path: &Path, text: &str, kind: FileKind) -> Result<FileLayer, String> {
    let at = |e: String| format!("{}: {e}", path.display());
    let table: Table = text
        .parse()
        .map_err(|e: toml::de::Error| at(e.to_string()))?;
    let mut layer = FileLayer {
        path: path.to_path_buf(),
        ..Default::default()
    };
    for (key, value) in &table {
        if key == "profiles" {
            let Value::Table(profiles) = value else {
                return Err(at("profiles must be a table of [profiles.<name>]".into()));
            };
            for (name, body) in profiles {
                let Value::Table(body) = body else {
                    return Err(at(format!("profiles.{name} must be a table")));
                };
                let entries =
                    entries(body, kind, true).map_err(|e| at(format!("profiles.{name}: {e}")))?;
                layer.profiles.insert(name.clone(), entries);
            }
            continue;
        }
        let mut one = Table::new();
        one.insert(key.clone(), value.clone());
        layer.base.extend(entries(&one, kind, false).map_err(at)?);
    }
    Ok(layer)
}

fn entries(table: &Table, kind: FileKind, in_profile: bool) -> Result<Entries, String> {
    let mut out = Vec::new();
    for (key, value) in table {
        let def = find(key)
            .ok_or_else(|| format!("unknown setting \"{key}\" (kern settings lists them)"))?;
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
        if in_profile && (key == "profile" || def.scope == Scope::User) {
            return Err(format!("{key} can't be set in a profile"));
        }
        let raw = raw(value)
            .ok_or_else(|| format!("{key}: a string, number, true/false or a list of strings"))?;
        out.push((def, check(def, &raw).map_err(|e| format!("{key}: {e}"))?));
    }
    Ok(out)
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
