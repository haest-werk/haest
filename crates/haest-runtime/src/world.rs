//! What the agent works in: [`World`] and the pieces built from it.

use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use haest_settings::Settings;

use crate::{
    RuntimeHooks,
    haest::{
        ChildEnv, ShutWorkspace, WorkspaceMode, WorkspaceRef,
        code::sandbox_commands,
        core::error::report,
        core::fs::{Fs, FsRef, ensure_dir},
        core::remedy::Remedies,
        mcp::{McpHub, McpServerConfig, McpTransport},
        tools::{InboxRef, JobHub},
        workspace::{RealFs, RealWorkspace, SandboxMounts, SandboxWorkspace},
    },
};

/// Where the agent's files are and whether it may start real processes.
///
/// [`build`](crate::build) makes what the agent works in from it: the
/// workspace (the sandbox by default, with `js` in its shell), background
/// jobs (Bash in the real workspace only; [`kill_jobs`] ends them) and the
/// MCP servers the host listed.
///
/// Two worlds: the real disk ([`World::disk`]) or the host's own files
/// ([`World::files`]).
///
/// ```
/// use haest_core::{ChildEnv, fs::MemFs};
/// use haest_runtime::World;
///
/// # fn main() -> Result<(), std::env::VarError> {
/// // The real disk; what haest starts gets PATH and nothing else.
/// let disk = World::disk(ChildEnv::new([("PATH", std::env::var("PATH")?)]));
/// assert!(disk.starts_processes());
/// assert!(disk.child_env().is_some_and(|env| env.get("PATH").is_some()));
///
/// // The host's own files; haest starts no process at all.
/// let own = World::files(MemFs::new());
/// assert!(!own.starts_processes());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct World {
    /// Everything is read from and written to it.
    fs: FsRef,
    /// The environment of every process haest starts; None = it starts none.
    child_env: Option<ChildEnv>,
}

impl World {
    /// The real disk: sessions, skills and instruction files are read and
    /// written there, `workspace = real` edits it, the sandbox overlays it.
    /// Every process haest starts (real bash, a background job, a stdio MCP
    /// server) gets `child_env` and nothing else of the host's environment,
    /// with the variables the host passes on purpose on top
    /// ([`RuntimeInputs::with_passed_env`](crate::RuntimeInputs::with_passed_env)).
    pub fn disk(child_env: ChildEnv) -> Self {
        Self {
            fs: FsRef::new(RealFs),
            child_env: Some(child_env),
        }
    }

    /// The host's own files: everything haest reads and writes goes through
    /// `fs` (the file system itself, or an `Arc` of one the host shares), the
    /// sandbox runs over it, and no real process starts (no real workspace,
    /// no background jobs, no stdio MCP server).
    pub fn files(fs: impl Into<FsRef>) -> Self {
        Self {
            fs: fs.into(),
            child_env: None,
        }
    }

    /// The [`Fs`] everything is read from and written to: the real disk's
    /// ([`RealFs`]) or the host's own.
    pub fn fs(&self) -> &FsRef {
        &self.fs
    }

    /// The environment the disk was made with ([`World::disk`]); None for
    /// the host's own files. A process haest starts gets it with the
    /// variables the host passes on purpose on top.
    pub fn child_env(&self) -> Option<&ChildEnv> {
        self.child_env.as_ref()
    }

    /// Whether haest may start real processes here (real bash, background
    /// jobs, a stdio MCP server): on the disk only.
    pub fn starts_processes(&self) -> bool {
        self.child_env.is_some()
    }

    /// The environment of a process haest starts: [`child_env`](Self::child_env)
    /// with `passed_env` on top. None = it starts none.
    pub(crate) fn child_env_with(&self, passed_env: &ChildEnv) -> Option<ChildEnv> {
        Some(self.child_env.as_ref()?.overlaid(passed_env))
    }
}

/// How long haest waits at exit for killed jobs to be gone.
const KILL_WAIT: Duration = Duration::from_secs(3);

/// The background-job hub, when the settings let Bash leave jobs running: the
/// `bash` tool on, in the real workspace (the sandbox has no real processes to
/// leave running). As the TS haest's `host/jobs.ts`. A job that exits on its
/// own tells main through `inbox`.
pub(crate) fn jobs_for(s: &Settings, inbox: impl Into<InboxRef>) -> Option<JobHub> {
    let real_bash = s.workspace == WorkspaceMode::Real && s.tools.bash;
    real_bash.then(|| JobHub::new(inbox))
}

/// Kill every running job and wait until each one's process group is gone
/// (a job's task does the kill; the runtime must not shut down first).
/// Returns how many were killed.
///
/// Waits at most 3 seconds; a job still alive by then is left. Call it before
/// the host exits, or a job outlives it.
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

/// Why the workspace the settings pick could not open
/// ([`BuildError::Workspace`](crate::BuildError::Workspace)).
///
/// A `match` on it has a `_` arm: haest may add a cause in a minor release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkspaceError {
    /// `workspace = real` in a [`World::files`]: the real workspace edits the
    /// real disk, and this host hands haest its own files.
    #[error("workspace = real needs the real disk; this host hands haest its own files")]
    RealNeedsDisk,
    /// The spill dir could not be made, or made private.
    #[error("creating the spill dir {}", path.display())]
    SpillDir {
        /// The spill dir.
        path: PathBuf,
        /// What making it hit.
        #[source]
        source: io::Error,
    },
    /// The sandbox did not open over the cwd and its mounts.
    #[error("opening the sandbox")]
    Sandbox(#[source] io::Error),
}

/// The spill dir on the disk, made private (0700) as the session dir is:
/// spilled output is file contents and command output.
async fn private_spill_dir(fs: &dyn Fs, spill: &Path) -> Result<(), WorkspaceError> {
    let made = async {
        ensure_dir(fs, spill).await?;
        fs.chmod(spill, 0o700).await
    };
    made.await.map_err(|source| WorkspaceError::SpillDir {
        path: spill.to_path_buf(),
        source,
    })
}

/// The `workspace` setting's workspace in this world. The disk's sandbox
/// mounts the skill dirs outside the project (read-only; `SkillScanRoots::dirs`
/// of the `skills` setting's roots) and the spill dir (read-write); a skill
/// dir inside the project is the overlay's like any project file. A host's Fs
/// is the sandbox's whole world. On the disk the spill dir is made first, private. `passed_env` (the
/// variables the host passes on purpose) reaches the shell either way: the
/// sandbox's alone, real bash's over `child_env`. The sandbox tells the model
/// how the human applies its edits with `remedies`. `workspace = shut` is a
/// workspace with no files and no shell, in any world.
///
/// # Errors
///
/// A [`WorkspaceError`]: `workspace = real` in a [`World::files`] (the real
/// workspace needs the real disk), a spill dir that cannot be made, a sandbox
/// that cannot open.
pub(crate) async fn workspace_for(
    s: &Settings,
    world: &World,
    passed_env: &ChildEnv,
    skill_dirs: &[PathBuf],
    spill: &Path,
    remedies: Remedies,
) -> Result<WorkspaceRef, WorkspaceError> {
    let fs = world.fs();
    let ws = match (s.workspace, world.child_env_with(passed_env)) {
        (WorkspaceMode::Shut, _) => return Ok(ShutWorkspace.into()),
        (WorkspaceMode::Real, Some(env)) => {
            private_spill_dir(fs.as_ref(), spill).await?;
            return Ok(RealWorkspace::new(env).into());
        }
        (WorkspaceMode::Real, None) => return Err(WorkspaceError::RealNeedsDisk),
        (WorkspaceMode::Sandbox, None) => SandboxWorkspace::over(fs.clone()),
        (WorkspaceMode::Sandbox, Some(_)) => {
            // The spill dir is mounted read-write, so the path in a spill notice is real.
            private_spill_dir(fs.as_ref(), spill).await?;
            let mut mounts = SandboxMounts::new(&s.cwd).with_writable(spill);
            // A skill dir in the project is the project's: the overlay holds
            // it, so the model can edit it and the edit is a change to review.
            for dir in skill_dirs.iter().filter(|d| !d.starts_with(&s.cwd)) {
                if fs.exists(dir).await {
                    mounts = mounts.with_readable(dir.clone());
                }
            }
            SandboxWorkspace::open(mounts)
                .await
                .map_err(WorkspaceError::Sandbox)?
        }
        // A mode added later runs shut until it is named here: no files, no shell.
        _ => return Ok(ShutWorkspace.into()),
    };
    let ws = sandbox_commands().into_iter().fold(
        ws.with_env(passed_env.clone()).with_remedies(remedies),
        SandboxWorkspace::with_command,
    );
    Ok(ws.into())
}

/// The MCP servers the host listed, started. A stdio server is a real
/// process: in a world without them it is skipped, and the host is told.
///
/// Every server starts at once; one that fails is a [`warn`](RuntimeHooks::warn), and
/// the hub holds the rest. A stdio server's process gets the world's
/// `child_env` with `passed_env` over it, then its own `env`. With `debug` on,
/// each server's line (how many tools, or why it failed) goes to the
/// [`Ui::debug`](crate::Ui::debug).
pub(crate) async fn connect_mcp(
    hooks: &RuntimeHooks,
    world: &World,
    passed_env: &ChildEnv,
    servers: Vec<McpServerConfig>,
) -> McpHub {
    let Some(child_env) = world.child_env_with(passed_env) else {
        let (stdio, http): (Vec<_>, Vec<_>) = servers
            .into_iter()
            .partition(|s| matches!(s.transport, McpTransport::Stdio(_)));
        for s in &stdio {
            hooks.warn(&format!(
                "MCP server {} not started: a stdio server is a real process, and this host hands haest its own files.",
                s.name
            ));
        }
        return start(hooks, McpHub::new(), http).await;
    };
    start(hooks, McpHub::new().with_env(child_env), servers).await
}

async fn start(hooks: &RuntimeHooks, hub: McpHub, servers: Vec<McpServerConfig>) -> McpHub {
    if servers.is_empty() {
        return hub;
    }
    let hub = if hooks.settings().debug {
        let ui = hooks.ui().clone();
        hub.with_log(move |m| ui.debug(m))
    } else {
        hub
    };
    hub.connect(&servers).await;
    for f in hub.failures() {
        let why = report(f.error());
        hooks.warn(&format!("MCP server {} did not start: {why}", f.name()));
    }
    hub
}

#[cfg(test)]
mod tests;
