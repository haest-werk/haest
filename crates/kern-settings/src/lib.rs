//! kern's settings, for every host: the CLI, brain, anything that embeds kern.
//!
//! One name per setting, three spellings: `skill_catalog` in a TOML file,
//! `KERN_SKILL_CATALOG` in the environment, `--skill-catalog` on the command
//! line. The layers, each over the one before: the bare defaults, your
//! `<config dir>/config.toml`, a trusted project's `.kern/config.toml`, the
//! profile, `KERN_*` variables, flags ([`resolve`]).
//!
//! Bare means every capability off until asked for (no tools, no context
//! files, no skills, no MCP, no mods) and every brake on (approvals, the spend
//! and turn caps, the sandbox). `profile = "claude"` turns on everything Claude
//! Code has that kern reads.
//!
//! [`registry::DEFS`] is the one list: a host builds its flags from it.

mod file;
pub mod profiles;
pub mod registry;
mod resolve;
mod settings;
#[cfg(test)]
mod tests;
pub mod value;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use kern_core::fs::{Fs, normalize};

pub use registry::{DEFAULT_MODEL, DEFS, Def, Kind, Scope};
pub use resolve::{Inputs, ProjectFile, Resolved, Source, resolve};
pub use settings::{ImageConfig, Picked, Settings, config_dir};

/// What a host hands [`load`].
pub struct LoadInputs<'a> {
    pub cwd: PathBuf,
    pub env: &'a HashMap<String, String>,
    /// `(key, raw)` per setting flag given.
    pub flags: &'a [(String, String)],
}

/// The settings, and every value's source.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub settings: Settings,
    pub resolved: Resolved,
}

/// Find and read the files, resolve every layer, type the result.
pub async fn load(fs: &dyn Fs, inputs: LoadInputs<'_>) -> Result<Loaded, String> {
    let home = inputs
        .env
        .get("HOME")
        .map(|h| h.trim())
        .filter(|h| !h.is_empty())
        .map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let dir = config_dir_before_files(inputs.env, inputs.flags, &inputs.cwd, &home);
    let user_file = read(fs, &dir.join("config.toml")).await?;
    let project_file = match project_config(fs, &inputs.cwd, &home).await {
        Some(path) => read(fs, &path).await?,
        None => None,
    };
    settle(&Inputs {
        cwd: &inputs.cwd,
        home: &home,
        env: inputs.env,
        flags: inputs.flags,
        user_file,
        project_file,
    })
}

/// The settings from inputs already in hand (a host with its own files, a
/// test): resolve the layers, type the result. [`load`] reads the files first.
pub fn settle(inputs: &Inputs) -> Result<Loaded, String> {
    let resolved = resolve(inputs)?;
    let (cwd, home) = (inputs.cwd.to_path_buf(), inputs.home.to_path_buf());
    let mut settings = Settings::from(&resolved, cwd, home, inputs.env)?;
    if let ProjectFile::Untrusted(path) = &resolved.project_file {
        settings.notes.push(format!(
            "Skipped {}: {} is not in trusted_dirs.",
            path.display(),
            path.parent()
                .and_then(Path::parent)
                .unwrap_or(Path::new("/"))
                .display()
        ));
    }
    Ok(Loaded { settings, resolved })
}

/// Where config.toml is: only the environment and flags may say (`config_dir`
/// is [`Scope::Process`]), so it is known before any file is read.
fn config_dir_before_files(
    env: &HashMap<String, String>,
    flags: &[(String, String)],
    cwd: &Path,
    home: &Path,
) -> PathBuf {
    let flag = flags
        .iter()
        .rev()
        .find(|(k, _)| k == "config_dir")
        .map(|(_, v)| v.as_str());
    let set = flag.or_else(|| env.get("KERN_CONFIG_DIR").map(String::as_str));
    config_dir(set, env, cwd, home)
}

async fn read(fs: &dyn Fs, path: &Path) -> Result<Option<(PathBuf, String)>, String> {
    if !fs.exists(path).await {
        return Ok(None);
    }
    let bytes = fs
        .read(path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some((
        path.to_path_buf(),
        String::from_utf8_lossy(&bytes).into_owned(),
    )))
}

/// The nearest `.kern/config.toml` from `cwd` up, not counting the home dir's own.
async fn project_config(fs: &dyn Fs, cwd: &Path, home: &Path) -> Option<PathBuf> {
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
