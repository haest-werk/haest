//! What the loader reads beside the settings, for the runtime to be handed:
//! the environment a child process gets, OpenRouter's routing, the project's
//! MCP servers. The runtime reads none of it itself.

use std::{collections::HashMap, path::Path};

use kern_core::{ChildEnv, fs::Fs};
use kern_mcp::{McpServerConfig, load_project_mcp_config};
use kern_providers::{
    RoutingEnv,
    routing::{ROUTING_ENV, parse_routing},
};

use crate::{
    fields::{CORE_VARS, EnvPassthrough},
    value::env_var,
};

/// What a process kern starts gets of `env`, by `env_passthrough`.
pub fn child_env(policy: EnvPassthrough, env: &HashMap<String, String>) -> ChildEnv {
    let mut vars: Vec<(String, String)> = match policy {
        EnvPassthrough::None => Vec::new(),
        EnvPassthrough::Core => CORE_VARS
            .iter()
            .filter_map(|k| env.get(*k).map(|v| (k.to_string(), v.clone())))
            .collect(),
        EnvPassthrough::All => env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    };
    vars.sort();
    ChildEnv::new(vars)
}

/// `KERN_PROVIDER_ROUTING`, parsed. Malformed: a note, and no routing.
pub fn routing(env: &HashMap<String, String>, notes: &mut Vec<String>) -> Option<RoutingEnv> {
    let raw = env_var(env, ROUTING_ENV)?;
    parse_routing(raw).unwrap_or_else(|e| {
        notes.push(e);
        None
    })
}

/// The project's `.mcp.json` servers: with `mcp` on, in a trusted dir. A note
/// when `mcp` is on, the dir is not trusted and there is a file to skip.
pub async fn mcp_servers(
    fs: &dyn Fs,
    cwd: &Path,
    mcp: bool,
    trusted: bool,
    notes: &mut Vec<String>,
) -> Vec<McpServerConfig> {
    if !mcp {
        return Vec::new();
    }
    if !trusted {
        if fs.exists(&cwd.join(".mcp.json")).await {
            notes.push(format!(
                "mcp is on, but {} is not in trusted_dirs: its .mcp.json servers were not started.",
                cwd.display()
            ));
        }
        return Vec::new();
    }
    load_project_mcp_config(fs, cwd).await.unwrap_or_else(|e| {
        notes.push(format!(".mcp.json not loaded: {e}"));
        Vec::new()
    })
}

/// `project_mods` off outside a trusted dir (they are the repo's code); a note
/// when it was on and there are mods to skip.
pub async fn gate_project_mods(
    fs: &dyn Fs,
    cwd: &Path,
    project_mods: &mut bool,
    trusted: bool,
    notes: &mut Vec<String>,
) {
    if !*project_mods || trusted {
        return;
    }
    *project_mods = false;
    if fs.exists(&cwd.join(".kern").join("mods")).await {
        notes.push(format!(
            "project_mods is on, but {} is not in trusted_dirs: its .kern/mods were not loaded.",
            cwd.display()
        ));
    }
}
