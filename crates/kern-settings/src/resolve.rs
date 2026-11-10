//! The layers, each over the one before:
//!
//!   1. the bare defaults
//!   2. `<config dir>/config.toml` (yours)
//!   3. `.kern/config.toml` of the project (only inside `trusted_dirs`)
//!   4. the profile (`profile`), built in or from a file
//!   5. `KERN_*` variables
//!   6. flags
//!
//! Every value remembers where it came from (`kern settings` shows it).

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::{Path, PathBuf},
};

use kern_core::fs::normalize;

use crate::{
    file::{Entries, FileKind, FileLayer, parse_file},
    profiles,
    registry::{DEFS, Def, Scope, find},
    value::{check, split_list},
};

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

#[derive(Debug, Clone)]
pub struct Resolved {
    values: BTreeMap<&'static str, (String, Source)>,
    pub profile: Option<String>,
    pub project_file: ProjectFile,
    /// The working directory is inside a trusted dir.
    pub trusted: bool,
}

impl Resolved {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|(v, _)| v.as_str())
    }

    pub fn source(&self, key: &str) -> Option<&Source> {
        self.values.get(key).map(|(_, s)| s)
    }

    /// The value came from somewhere other than the defaults.
    pub fn is_set(&self, key: &str) -> bool {
        self.source(key).is_some_and(|s| *s != Source::Default)
    }

    /// Every setting in registry order: its definition, value (None = unset) and source.
    pub fn rows(&self) -> impl Iterator<Item = (&'static Def, Option<&str>, Option<&Source>)> {
        DEFS.iter()
            .map(|d| (d, self.get(d.key), self.source(d.key)))
    }
}

pub struct Inputs<'a> {
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub env: &'a HashMap<String, String>,
    /// `(key, raw)` per flag given, in order.
    pub flags: &'a [(String, String)],
    /// `<config dir>/config.toml` and its text.
    pub user_file: Option<(PathBuf, String)>,
    /// The nearest `.kern/config.toml` and its text.
    pub project_file: Option<(PathBuf, String)>,
}

fn env_layer(env: &HashMap<String, String>) -> Result<Entries, String> {
    let mut out = Vec::new();
    for def in DEFS {
        let var = def.env();
        let Some(raw) = env.get(&var).map(|v| v.trim()).filter(|v| !v.is_empty()) else {
            continue;
        };
        out.push((def, check(def, raw).map_err(|e| format!("{var} {e}"))?));
    }
    Ok(out)
}

fn flag_layer(flags: &[(String, String)]) -> Result<Entries, String> {
    let mut out = Vec::new();
    for (key, raw) in flags {
        let def = find(key).ok_or_else(|| format!("unknown setting \"{key}\""))?;
        out.push((
            def,
            check(def, raw).map_err(|e| format!("--{} {e}", def.flag()))?,
        ));
    }
    Ok(out)
}

/// The last value `key` has across these layers.
fn last<'a>(key: &str, layers: &[&'a Entries]) -> Option<&'a str> {
    layers
        .iter()
        .flat_map(|l| l.iter())
        .filter(|(d, _)| d.key == key)
        .map(|(_, v)| v.as_str())
        .next_back()
}

/// `~/x` -> home/x, `x` -> cwd/x, normalized: every path a setting names.
pub(crate) fn absolute(dir: &str, cwd: &Path, home: &Path) -> PathBuf {
    let path = match dir.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if dir == "~" => home.to_path_buf(),
        None => cwd.join(dir),
    };
    normalize(&path)
}

pub fn resolve(inputs: &Inputs) -> Result<Resolved, String> {
    let user = match &inputs.user_file {
        Some((path, text)) => parse_file(path, text, FileKind::User)?,
        None => FileLayer::default(),
    };
    let env = env_layer(inputs.env)?;
    let flags = flag_layer(inputs.flags)?;

    let trusted_dirs: Vec<PathBuf> = last("trusted_dirs", &[&user.base, &env, &flags])
        .map(split_list)
        .unwrap_or_default()
        .iter()
        .map(|d| absolute(d, inputs.cwd, inputs.home))
        .collect();
    let trusted = |dir: &Path| trusted_dirs.iter().any(|t| normalize(dir).starts_with(t));

    let (project, project_file) = match &inputs.project_file {
        None => (FileLayer::default(), ProjectFile::None),
        Some((path, text)) => {
            // <root>/.kern/config.toml: the root is what must be trusted.
            let root = path
                .parent()
                .and_then(Path::parent)
                .unwrap_or(Path::new("/"));
            if trusted(root) {
                let layer = parse_file(path, text, FileKind::Project)?;
                (layer, ProjectFile::Read(path.clone()))
            } else {
                (FileLayer::default(), ProjectFile::Untrusted(path.clone()))
            }
        }
    };

    let profile = last("profile", &[&user.base, &project.base, &env, &flags]).map(String::from);
    let profile_entries = match &profile {
        None => Vec::new(),
        Some(name) => profile_entries(name, &user, &project)?,
    };

    let mut values: BTreeMap<&'static str, (String, Source)> = BTreeMap::new();
    for def in DEFS {
        if let Some(d) = def.default {
            let v = check(def, d).expect("a registry default checks");
            values.insert(def.key, (v, Source::Default));
        }
    }
    let file = |l: &FileLayer| Source::File(l.path.clone());
    let layers: [(&Entries, Source); 3] = [
        (&user.base, file(&user)),
        (&project.base, file(&project)),
        (
            &profile_entries,
            Source::Profile(profile.clone().unwrap_or_default()),
        ),
    ];
    for (entries, source) in layers {
        for (def, v) in entries {
            values.insert(def.key, (v.clone(), source.clone()));
        }
    }
    for (def, v) in &env {
        values.insert(def.key, (v.clone(), Source::Env(def.env())));
    }
    for (def, v) in &flags {
        values.insert(def.key, (v.clone(), Source::Flag(def.flag())));
    }
    Ok(Resolved {
        values,
        profile,
        project_file,
        trusted: trusted(inputs.cwd),
    })
}

/// The built-in profile of that name, then each file's table of it on top.
fn profile_entries(name: &str, user: &FileLayer, project: &FileLayer) -> Result<Entries, String> {
    let builtin = profiles::builtin(name);
    let tables: Vec<&Entries> = [user, project]
        .iter()
        .filter_map(|l| l.profiles.get(name))
        .collect();
    if builtin.is_none() && tables.is_empty() {
        let mut known: Vec<&str> = profiles::BUILTIN.iter().map(|(n, _)| *n).collect();
        known.extend(
            user.profiles
                .keys()
                .chain(project.profiles.keys())
                .map(String::as_str),
        );
        return Err(format!(
            "unknown profile \"{name}\" (known: {})",
            known.join(", ")
        ));
    }
    let mut out: Entries = Vec::new();
    for (key, raw) in builtin.unwrap_or_default() {
        let def = find(key).expect("a built-in profile names real settings");
        debug_assert_eq!(def.scope, Scope::Any);
        out.push((def, check(def, raw).expect("a built-in profile checks")));
    }
    for table in tables {
        out.extend(table.iter().cloned());
    }
    Ok(out)
}
