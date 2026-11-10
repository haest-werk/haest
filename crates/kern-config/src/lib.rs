//! kern's settings loader, for a host that wants kern's files, variables and
//! flags. The runtime never reads any of them: it is handed what [`load`]
//! returns (or what a host writes out itself, `kern_settings::Settings`).
//!
//! One name per setting, three spellings: `skill_catalog` in a TOML file,
//! `KERN_SKILL_CATALOG` in the environment, `--skill-catalog` on the command
//! line. The layers, each over the one before: bare, your
//! `<config dir>/config.toml`, a trusted project's `.kern/config.toml`, the
//! profile, `KERN_*` variables, flags ([`resolve`]). `profile = "claude"` turns
//! on everything Claude Code has that kern reads.
//!
//! Beside the settings, [`load`] reads what the runtime is handed with them:
//! the provider keys (the environment), `providers.json`, the OpenRouter
//! routing variable, a trusted project's `.mcp.json`, and the environment a
//! child process gets (`env_passthrough`).
//!
//! [`registry::DEFS`] is the one list: a host builds its flags from it.

mod fields;
mod file;
pub mod handed;
pub mod profiles;
pub mod registry;
mod resolve;
#[cfg(test)]
mod tests;
pub mod value;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use kern_core::{
    ChildEnv,
    fs::{Fs, normalize},
};
use kern_mcp::McpServerConfig;
use kern_providers::{CustomProviders, RoutingEnv, load_custom_providers};

pub use fields::{CORE_VARS, Config, EnvPassthrough, get, set};
pub use registry::{DEFAULT_MODEL, DEFS, Def, Kind, Scope};
pub use resolve::{Inputs, ProjectFile, Resolved, Source, resolve};

use crate::resolve::{absolute, env_var, xdg};

/// What a host hands [`load`].
pub struct LoadInputs<'a> {
    pub cwd: PathBuf,
    /// The process's environment: `KERN_*`, `HOME`, the XDG dirs, the keys.
    pub env: &'a HashMap<String, String>,
    /// `(key, raw)` per setting flag given.
    pub flags: &'a [(String, String)],
}

/// The settings, where each came from, and what the runtime is handed with them.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub resolved: Resolved,
    /// What a process kern starts gets of the environment (`env_passthrough`).
    pub child_env: ChildEnv,
    /// The environment, as the source of the provider keys.
    pub keys: HashMap<String, String>,
    /// `KERN_PROVIDER_ROUTING`, parsed.
    pub routing: Option<RoutingEnv>,
    /// `<config dir>/providers.json`.
    pub providers: CustomProviders,
    /// A trusted project's `.mcp.json`, with `mcp` on.
    pub mcp_servers: Vec<McpServerConfig>,
    /// What the host should tell the human: a setting asked for and not honoured.
    pub notes: Vec<String>,
}

/// Find and read the files, resolve every layer, decide trust, read what the
/// runtime is handed beside the settings.
pub async fn load(fs: &dyn Fs, inputs: LoadInputs<'_>) -> Result<Loaded, String> {
    let env = inputs.env;
    let home = env_var(env, "HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let cwd = inputs.cwd;
    let dir = config_dir_before_files(env, inputs.flags, &cwd, &home);
    let user_file = read(fs, &dir.join("config.toml")).await?;
    let project_file = match project_config(fs, &cwd, &home).await {
        Some(path) => read(fs, &path).await?,
        None => None,
    };
    let (mut config, resolved) = resolve(&Inputs {
        cwd: &cwd,
        home: &home,
        env,
        flags: inputs.flags,
        user_file,
        project_file,
    })?;
    let mut notes = Vec::new();
    if let ProjectFile::Untrusted(path) = &resolved.project_file {
        let root = path.parent().and_then(Path::parent);
        notes.push(format!(
            "Skipped {}: {} is not in trusted_dirs.",
            path.display(),
            root.unwrap_or(Path::new("/")).display()
        ));
    }
    let trusted = resolved.trusted;
    let s = &mut config.settings;
    handed::gate_project_mods(fs, &cwd, &mut s.project_mods, trusted, &mut notes).await;
    let mcp_servers = handed::mcp_servers(fs, &cwd, config.mcp, trusted, &mut notes).await;
    let providers = load_custom_providers(fs, &config.settings.config_dir)
        .await
        .map_err(|e| e.0)?;
    let routing = handed::routing(env, &mut notes);
    Ok(Loaded {
        child_env: handed::child_env(config.env_passthrough, env),
        keys: env.clone(),
        config,
        resolved,
        routing,
        providers,
        mcp_servers,
        notes,
    })
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
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty());
    match flag.or_else(|| env_var(env, "KERN_CONFIG_DIR")) {
        Some(set) => absolute(set, cwd, home),
        None => xdg(env, "XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config/kern")),
    }
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
