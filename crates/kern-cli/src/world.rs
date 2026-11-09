//! What the agent works in: the workspace (the sandbox by default, with `js`
//! in its shell) and the project's MCP servers.

use std::{path::Path, sync::Arc};

use anyhow::Context;
use kern::{
    Workspace, WorkspaceMode,
    code::sandbox_commands,
    fs::{Fs, ensure_dir},
    mcp::{McpHub, load_project_mcp_config},
    skills::{SkillOpts, skill_roots},
    workspace::{RealWorkspace, SandboxMounts, SandboxWorkspace},
};

use crate::{config::Config, host::Host};

pub async fn workspace_for(
    cfg: &Config,
    fs: &Arc<dyn Fs>,
    skill_opts: &SkillOpts,
    spill: &Path,
) -> anyhow::Result<Arc<dyn Workspace>> {
    if cfg.workspace == WorkspaceMode::Real {
        return Ok(Arc::new(RealWorkspace::new()));
    }
    // The spill dir is mounted read-write, so the path in a spill notice is real.
    ensure_dir(fs.as_ref(), spill).await?;
    let mut mounts = SandboxMounts::new(&cfg.cwd).writable(spill);
    for dir in skill_roots(fs.as_ref(), skill_opts).await {
        if fs.exists(&dir).await {
            mounts = mounts.readable(dir);
        }
    }
    let ws = SandboxWorkspace::open(mounts)
        .await
        .context("opening the sandbox")?;
    let ws = sandbox_commands()
        .into_iter()
        .fold(ws, SandboxWorkspace::with_command);
    Ok(Arc::new(ws))
}

pub async fn connect_mcp(host: &Host, fs: &dyn Fs, cwd: &Path) -> Arc<McpHub> {
    let hub = Arc::new(McpHub::new());
    match load_project_mcp_config(fs, cwd).await {
        Ok(configs) if !configs.is_empty() => {
            hub.connect(&configs, &|m: &str| kern::tool::ToolHost::log(host, m))
                .await;
            for f in hub.failures() {
                host.warn(&format!("MCP server {} did not start: {}", f.name, f.error));
            }
        }
        Ok(_) => {}
        Err(e) => host.warn(&format!(".mcp.json not loaded: {e}")),
    }
    hub
}
