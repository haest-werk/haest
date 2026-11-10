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

use toml::{Table, Value};

use crate::{
    registry::{Def, Scope, find},
    value::check,
};

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
pub enum FileKind {
    /// `<config dir>/config.toml`.
    User,
    /// A trusted project's `.kern/config.toml`.
    Project,
}

pub fn parse_file(path: &Path, text: &str, kind: FileKind) -> Result<FileLayer, String> {
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
