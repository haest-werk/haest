//! What the agent works in, as the host hands it ([`World`]): the workspace
//! (the sandbox by default, with `js` in its shell), background jobs (Bash in
//! the real workspace only) and the MCP servers the host listed.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use kern_settings::Settings;

use crate::{
    Host,
    kern::{
        ChildEnv, ShutWorkspace, Workspace, WorkspaceMode,
        code::sandbox_commands,
        fs::{Fs, ensure_dir},
        mcp::{McpHub, McpServerConfig, McpTransport},
        tools::{Inbox, JobHub},
        workspace::{RealFs, RealWorkspace, SandboxMounts, SandboxWorkspace},
    },
};

/// Where the agent's files are and whether it may start real processes.
#[derive(Clone)]
pub enum World {
    /// The real disk: sessions, skills and instruction files are read and
    /// written there, `workspace = real` edits it, the sandbox overlays it.
    /// Every process kern starts (real bash, a background job, a stdio MCP
    /// server) gets `child_env` and nothing else of the host's environment.
    Disk { child_env: ChildEnv },
    /// The host's own files: everything kern reads and writes goes through
    /// this [`Fs`], the sandbox runs over it, and no real process starts
    /// (no real workspace, no background jobs, no stdio MCP server).
    Fs(Arc<dyn Fs>),
}

impl World {
    /// The [`Fs`] everything is read from and written to.
    pub fn fs(&self) -> Arc<dyn Fs> {
        match self {
            World::Disk { .. } => Arc::new(RealFs::new()),
            World::Fs(fs) => fs.clone(),
        }
    }

    /// The environment of a process kern starts: `child_env` with the
    /// variables the host passes on purpose on top. None = it starts none.
    pub fn child_env(&self, passed: &ChildEnv) -> Option<ChildEnv> {
        match self {
            World::Disk { child_env } => Some(child_env.with(passed)),
            World::Fs(_) => None,
        }
    }
}

/// How long kern waits at exit for killed jobs to be gone.
const KILL_WAIT: Duration = Duration::from_secs(3);

/// Background jobs, as the TS kern (`host/jobs.ts`): Bash in the real
/// workspace only -- the sandbox has no real processes to leave running. A job
/// that exits on its own tells main through the inbox.
pub fn jobs_for(s: &Settings, inbox: Arc<dyn Inbox>) -> Option<Arc<JobHub>> {
    let real_bash = s.workspace == WorkspaceMode::Real && s.tools.bash;
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

/// The `workspace` setting's workspace in this world. The disk's sandbox
/// mounts the skill dirs (read-only; `scan_roots` of the `skills` setting's
/// roots) and the spill dir (read-write); a host's Fs is the sandbox's whole
/// world. `passed` (the variables the host passes on purpose) reaches the
/// shell either way: the sandbox's alone, real bash's over `child_env`.
pub async fn workspace_for(
    s: &Settings,
    world: &World,
    passed: &ChildEnv,
    skill_dirs: &[PathBuf],
    spill: &Path,
) -> anyhow::Result<Arc<dyn Workspace>> {
    let fs = world.fs();
    let ws = match (s.workspace, world.child_env(passed)) {
        (WorkspaceMode::Shut, _) => return Ok(Arc::new(ShutWorkspace)),
        (WorkspaceMode::Real, Some(env)) => return Ok(Arc::new(RealWorkspace::new(env))),
        (WorkspaceMode::Real, None) => {
            bail!("workspace = real needs the real disk; this host hands kern its own files")
        }
        (WorkspaceMode::Sandbox, None) => SandboxWorkspace::over(fs),
        (WorkspaceMode::Sandbox, Some(_)) => {
            // The spill dir is mounted read-write, so the path in a spill notice is real.
            ensure_dir(fs.as_ref(), spill)
                .await
                .with_context(|| format!("creating the spill dir {}", spill.display()))?;
            let mut mounts = SandboxMounts::new(&s.cwd).writable(spill);
            for dir in skill_dirs {
                if fs.exists(dir).await {
                    mounts = mounts.readable(dir.clone());
                }
            }
            SandboxWorkspace::open(mounts)
                .await
                .context("opening the sandbox")?
        }
    };
    let ws = sandbox_commands()
        .into_iter()
        .fold(ws.with_env(passed.clone()), SandboxWorkspace::with_command);
    Ok(Arc::new(ws))
}

/// The MCP servers the host listed, started. A stdio server is a real
/// process: in a world without them it is skipped, and the host is told.
pub async fn connect_mcp(
    host: &Host,
    world: &World,
    passed: &ChildEnv,
    servers: Vec<McpServerConfig>,
) -> Arc<McpHub> {
    let Some(child_env) = world.child_env(passed) else {
        let (stdio, http): (Vec<_>, Vec<_>) = servers
            .into_iter()
            .partition(|s| matches!(s.transport, McpTransport::Stdio { .. }));
        for s in &stdio {
            host.warn(&format!(
                "MCP server {} not started: a stdio server is a real process, and this host hands kern its own files.",
                s.name
            ));
        }
        return start(host, McpHub::new(), http).await;
    };
    start(host, McpHub::with_env(child_env), servers).await
}

async fn start(host: &Host, hub: McpHub, servers: Vec<McpServerConfig>) -> Arc<McpHub> {
    let hub = Arc::new(hub);
    if servers.is_empty() {
        return hub;
    }
    hub.connect(&servers, &|m: &str| {
        crate::kern::tool::ToolHost::log(host, m)
    })
    .await;
    for f in hub.failures() {
        host.warn(&format!("MCP server {} did not start: {}", f.name, f.error));
    }
    hub
}

#[cfg(test)]
mod tests;
