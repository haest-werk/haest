//! What the loader reads beside the settings, for the runtime to be handed:
//! the environment a child process gets, OpenRouter's routing, the project's
//! MCP servers. The runtime reads none of it itself.

use std::{collections::HashMap, path::Path};

use kern_core::{ChildEnv, fs::Fs};
use kern_mcp::{McpServerConfig, load_project_mcp_config, project_mcp_path};
use kern_providers::{RoutingEnv, routing::parse_routing};

use crate::{
    fields::{CORE_VARS, EnvPassthrough},
    source::Source,
};

/// What a process kern starts gets of `env`, by `env_passthrough`.
pub(crate) fn child_env(policy: EnvPassthrough, env: &HashMap<String, String>) -> ChildEnv {
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

/// `provider_routing`, parsed. Malformed: a note naming where it came from,
/// and no routing.
pub(crate) fn routing(
    raw: Option<&str>,
    from: &Source,
    notes: &mut Vec<String>,
) -> Option<RoutingEnv> {
    parse_routing(raw?).unwrap_or_else(|e| {
        notes.push(format!("provider_routing ({from}) is {e}"));
        None
    })
}

/// The note for a project's `path`, skipped: `root` is not in trusted_dirs.
pub(crate) fn untrusted(path: &Path, root: &Path) -> String {
    format!(
        "Skipped {}: {} is not in trusted_dirs.",
        path.display(),
        root.display()
    )
}

/// [`untrusted`]'s note, when there is a `path` to skip.
async fn note_untrusted(fs: &dyn Fs, path: &Path, root: &Path, notes: &mut Vec<String>) {
    if fs.exists(path).await {
        notes.push(untrusted(path, root));
    }
}

/// The project's `.mcp.json` servers: with `mcp` on, in a trusted dir.
pub(crate) async fn mcp_servers(
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
        note_untrusted(fs, &project_mcp_path(cwd), cwd, notes).await;
        return Vec::new();
    }
    load_project_mcp_config(fs, cwd).await.unwrap_or_else(|e| {
        notes.push(format!(".mcp.json not loaded: {e}"));
        Vec::new()
    })
}

/// `project_mods` off outside a trusted dir (they are the repo's code).
pub(crate) async fn gate_project_mods(
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
    note_untrusted(fs, &cwd.join(".kern/mods"), cwd, notes).await;
}
