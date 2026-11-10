//! What the agent works in: the workspace (the sandbox by default, with `js`
//! in its shell), background jobs (Bash in the real workspace only) and the
//! project's MCP servers (`mcp`, a trusted dir only).

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use kern_settings::Settings;

use crate::{
    Host,
    kern::{
        Workspace, WorkspaceMode,
        code::sandbox_commands,
        fs::{Fs, ensure_dir},
        mcp::{McpHub, load_project_mcp_config},
        skills::{SkillOpts, skill_roots},
        tools::{Inbox, JobHub},
        workspace::{RealWorkspace, SandboxMounts, SandboxWorkspace},
    },
};

/// How long kern waits at exit for killed jobs to be gone.
const KILL_WAIT: Duration = Duration::from_secs(3);

/// Background jobs, as the TS kern (`host/jobs.ts`): Bash in the real
/// workspace only -- the sandbox has no real processes to leave running. A job
/// that exits on its own tells main through the inbox.
pub fn jobs_for(s: &Settings, inbox: Arc<dyn Inbox>) -> Option<Arc<JobHub>> {
    let real_bash = s.workspace == WorkspaceMode::Real && s.tools.has("bash");
    real_bash.then(|| Arc::new(JobHub::new(inbox)))
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

/// The `workspace` setting's workspace. The sandbox mounts the skill dirs the
/// `skills` setting reads (read-only) and the spill dir (read-write).
pub async fn workspace_for(
    s: &Settings,
    fs: &Arc<dyn Fs>,
    skill_opts: &SkillOpts,
    spill: &Path,
) -> anyhow::Result<Arc<dyn Workspace>> {
    if s.workspace == WorkspaceMode::Real {
        return Ok(Arc::new(RealWorkspace::new()));
    }
    // The spill dir is mounted read-write, so the path in a spill notice is real.
    ensure_dir(fs.as_ref(), spill).await?;
    let mut mounts = SandboxMounts::new(&s.cwd).writable(spill);
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

/// The project's MCP servers, started: with `mcp`, in a trusted dir. An empty
/// hub otherwise (its `.mcp.json` names commands from the repo).
pub async fn connect_mcp(host: &Host, fs: &dyn Fs) -> Arc<McpHub> {
    let hub = Arc::new(McpHub::new());
    let s = &host.settings;
    if !s.mcp {
        return hub;
    }
    if !s.trusted {
        if fs.exists(&s.cwd.join(".mcp.json")).await {
            host.warn(&format!(
                "mcp is on, but {} is not in trusted_dirs: its .mcp.json servers were not started.",
                s.cwd.display()
            ));
        }
        return hub;
    }
    match load_project_mcp_config(fs, &s.cwd).await {
        Ok(configs) if !configs.is_empty() => {
            hub.connect(&configs, &|m: &str| {
                crate::kern::tool::ToolHost::log(host, m)
            })
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
    use crate::{
        kern::{fs::MemFs, tools::inbox::NoInbox},
        testkit::{host, settings},
    };

    use super::*;

    #[tokio::test]
    async fn an_untrusted_dir_is_warned_about_only_when_it_has_mcp_servers() {
        let (host, ui) = host(&[("mcp", "true")]);
        connect_mcp(&host, &MemFs::new()).await;
        assert_eq!(ui.warnings(), Vec::<String>::new());
        let fs = MemFs::with_files([("/w/.mcp.json", "{\"mcpServers\":{}}")]);
        connect_mcp(&host, &fs).await;
        assert_eq!(ui.warnings().len(), 1);
    }

    #[test]
    fn background_jobs_exist_only_for_bash_in_the_real_workspace() {
        let jobs = |flags: &[(&str, &str)]| jobs_for(&settings(flags), Arc::new(NoInbox)).is_some();
        assert!(!jobs(&[("tools", "bash")]));
        assert!(!jobs(&[("workspace", "real"), ("tools", "read")]));
        assert!(jobs(&[("workspace", "real"), ("tools", "bash")]));
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
