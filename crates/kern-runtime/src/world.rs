//! What the agent works in, as the host hands it ([`World`]): the workspace
//! (the sandbox by default, with `js` in its shell), background jobs (Bash in
//! the real workspace only) and the MCP servers the host listed.

use std::{
    path::Path,
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
        skills::{SkillOpts, skill_roots},
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

    /// The environment of a process kern starts; None = it starts none.
    pub fn child_env(&self) -> Option<&ChildEnv> {
        match self {
            World::Disk { child_env } => Some(child_env),
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
/// mounts the skill dirs the `skills` setting reads (read-only) and the spill
/// dir (read-write); a host's Fs is the sandbox's whole world.
pub async fn workspace_for(
    s: &Settings,
    world: &World,
    skill_opts: &SkillOpts,
    spill: &Path,
) -> anyhow::Result<Arc<dyn Workspace>> {
    let fs = world.fs();
    let ws = match (s.workspace, world) {
        (WorkspaceMode::Shut, _) => return Ok(Arc::new(ShutWorkspace)),
        (WorkspaceMode::Real, World::Disk { child_env }) => {
            return Ok(Arc::new(RealWorkspace::new(child_env.clone())));
        }
        (WorkspaceMode::Real, World::Fs(_)) => {
            bail!("workspace = real needs the real disk; this host hands kern its own files")
        }
        (WorkspaceMode::Sandbox, World::Fs(fs)) => SandboxWorkspace::over(fs.clone()),
        (WorkspaceMode::Sandbox, World::Disk { .. }) => {
            // The spill dir is mounted read-write, so the path in a spill notice is real.
            ensure_dir(fs.as_ref(), spill).await?;
            let mut mounts = SandboxMounts::new(&s.cwd).writable(spill);
            for dir in skill_roots(fs.as_ref(), skill_opts).await {
                if fs.exists(&dir).await {
                    mounts = mounts.readable(dir);
                }
            }
            SandboxWorkspace::open(mounts)
                .await
                .context("opening the sandbox")?
        }
    };
    let ws = sandbox_commands()
        .into_iter()
        .fold(ws, SandboxWorkspace::with_command);
    Ok(Arc::new(ws))
}

/// The MCP servers the host listed, started. A stdio server is a real
/// process: in a world without them it is skipped, and the host is told.
pub async fn connect_mcp(host: &Host, world: &World, servers: Vec<McpServerConfig>) -> Arc<McpHub> {
    let Some(child_env) = world.child_env() else {
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
    start(host, McpHub::with_env(child_env.clone()), servers).await
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
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        kern::{fs::MemFs, tools::inbox::NoInbox},
        testkit::{host, settings},
    };

    use super::*;

    #[tokio::test]
    async fn a_world_of_the_hosts_files_starts_no_stdio_server_and_says_which() {
        let (host, ui) = host(settings());
        let server = McpServerConfig {
            name: "gh".into(),
            transport: McpTransport::Stdio {
                command: "gh-mcp".into(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
            },
        };
        let world = World::Fs(Arc::new(MemFs::new()));
        let hub = connect_mcp(&host, &world, vec![server]).await;
        assert!(hub.tools().is_empty());
        assert_eq!(ui.warnings().len(), 1, "{:?}", ui.warnings());
    }

    #[tokio::test]
    async fn the_real_workspace_needs_the_real_disk() {
        let s = Settings {
            workspace: WorkspaceMode::Real,
            ..settings()
        };
        let opts = crate::prompt::skill_opts(&s);
        let world = World::Fs(Arc::new(MemFs::new()));
        let err = workspace_for(&s, &world, &opts, Path::new("/d/spill"))
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("real disk"), "{err}");
        let sandbox = workspace_for(&settings(), &world, &opts, Path::new("/d/spill")).await;
        assert!(sandbox.is_ok());
    }

    #[tokio::test]
    async fn a_shut_workspace_is_shut_in_any_world() {
        let s = Settings {
            workspace: WorkspaceMode::Shut,
            ..settings()
        };
        let opts = crate::prompt::skill_opts(&s);
        let worlds = [
            World::Fs(Arc::new(MemFs::new())),
            World::Disk {
                child_env: ChildEnv::default(),
            },
        ];
        for world in worlds {
            let ws = workspace_for(&s, &world, &opts, Path::new("/d/spill"))
                .await
                .unwrap();
            assert_eq!(ws.mode(), WorkspaceMode::Shut);
        }
    }

    #[test]
    fn background_jobs_exist_only_for_bash_in_the_real_workspace() {
        let jobs = |workspace, bash| {
            let mut s = Settings {
                workspace,
                ..settings()
            };
            s.tools.bash = bash;
            jobs_for(&s, Arc::new(NoInbox)).is_some()
        };
        assert!(!jobs(WorkspaceMode::Sandbox, true));
        assert!(!jobs(WorkspaceMode::Real, false));
        assert!(jobs(WorkspaceMode::Real, true));
    }

    #[tokio::test]
    async fn killing_at_exit_counts_the_jobs_and_waits_until_they_are_gone() {
        let jobs = JobHub::new(Arc::new(NoInbox));
        let env: ChildEnv = std::env::vars().collect();
        let ws: Arc<dyn Workspace> = Arc::new(RealWorkspace::new(env));
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
