//! What the agent works in: the workspace (the sandbox by default, with `js`
//! in its shell), background jobs (the real workspace only) and the project's
//! MCP servers.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use kern::{
    Workspace, WorkspaceMode,
    code::sandbox_commands,
    fs::{Fs, ensure_dir},
    mcp::{McpHub, load_project_mcp_config},
    skills::{SkillOpts, skill_roots},
    tools::{Inbox, JobHub},
    workspace::{RealWorkspace, SandboxMounts, SandboxWorkspace},
};

use crate::{config::Config, host::Host};

/// How long kern waits at exit for killed jobs to be gone.
const KILL_WAIT: Duration = Duration::from_secs(3);

/// Background jobs, as the TS kern (`host/jobs.ts`): only in the real
/// workspace -- the sandbox has no real processes to leave running. A job that
/// exits on its own tells main through the inbox.
pub fn jobs_for(mode: WorkspaceMode, inbox: Arc<dyn Inbox>) -> Option<Arc<JobHub>> {
    (mode == WorkspaceMode::Real).then(|| Arc::new(JobHub::new(inbox)))
}

/// Kill every running job and wait until each one's process group is gone
/// (a job's task does the kill; the runtime must not shut down first).
/// Returns how many were killed.
pub async fn kill_jobs(jobs: &JobHub) -> usize {
    let ids: Vec<String> = jobs.running().into_iter().map(|j| j.id).collect();
    jobs.kill_all();
    let alive = |id: &String| jobs.get(id).is_some_and(|j| j.exit_code.is_none());
    let deadline = Instant::now() + KILL_WAIT;
    while ids.iter().any(alive) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    ids.len()
}

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

#[cfg(test)]
mod tests {
    use kern::tools::inbox::NoInbox;

    use super::*;

    #[test]
    fn background_jobs_exist_only_in_the_real_workspace() {
        assert!(jobs_for(WorkspaceMode::Sandbox, Arc::new(NoInbox)).is_none());
        assert!(jobs_for(WorkspaceMode::Real, Arc::new(NoInbox)).is_some());
    }

    #[tokio::test]
    async fn killing_at_exit_counts_the_jobs_and_waits_until_they_are_gone() {
        let jobs = JobHub::new(Arc::new(NoInbox));
        let ws: Arc<dyn Workspace> = Arc::new(RealWorkspace::new());
        let cwd = std::env::temp_dir();
        for _ in 0..2 {
            jobs.start(ws.clone(), "sleep 30", "s", cwd.clone())
                .unwrap();
        }
        assert_eq!(kill_jobs(&jobs).await, 2);
        assert!(jobs.running().is_empty());
        let ended = ["b1", "b2"].map(|id| jobs.get(id).and_then(|j| j.exit_code));
        assert!(ended.iter().all(Option::is_some), "{ended:?}");
        assert_eq!(kill_jobs(&jobs).await, 0);
    }
}
