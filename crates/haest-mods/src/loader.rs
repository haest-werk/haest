//! Loading the mods of one conversation, in this order:
//!   1. built-in: the approval mod (`approvals`, with auto mode's judge when the
//!      host has one; a gate, so it judges a call after every mod's rewrite)
//!      and plan mode (`/plan`);
//!   2. the human's own: `<config_dir>/mods/*.ts|js|mjs` -- with `user_mods`;
//!   3. the project's: `<cwd>/.haest/mods/*.ts|js|mjs` -- ONLY with `project_mods`.
//!      A repo's mod is code from that repo running inside the host with full
//!      trust; cloning a repo must never be enough to run it.
//!
//!   4. the host's own native mods ([`ModOptions::with_mod`]), in the order given.
//!
//! A mod that fails to load is reported to the human and skipped; the rest
//! still load.

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use haest_core::{error::report, fs::Fs, workspace::WorkspaceMode};

use crate::{
    api::{ModEnv, ModRef, ModUiRef},
    approval::{ApprovalMod, AutoJudgeRef, SHELL_TOOLS},
    js::{JsMod, ModCaps},
    plan::{PLANNING_TOOLS, PlanListener, PlanMod},
    registry::Mods,
};

/// Which mods [`load_mods`] loads for one conversation, and where from.
///
/// [`ModOptions::new`] is the bare default: the approval mod and plan mode, no
/// mod files. The `with_*` setters change it.
///
/// ```
/// use std::path::PathBuf;
///
/// use haest_core::workspace::WorkspaceMode;
/// use haest_mods::ModOptions;
///
/// let opts = ModOptions::new("/project", WorkspaceMode::Real, "/home/me/.config/haest")
///     .with_user_mods(true)
///     .with_shell_tools(["Bash", "Shell"]);
/// assert_eq!(opts.mod_dirs(), [PathBuf::from("/home/me/.config/haest/mods")]);
/// assert!(opts.approvals());
/// ```
#[derive(Clone)]
pub struct ModOptions {
    cwd: PathBuf,
    workspace: WorkspaceMode,
    config_dir: PathBuf,
    user_mods: bool,
    project_mods: bool,
    approvals: bool,
    auto: Option<AutoJudgeRef>,
    shell_tools: Vec<String>,
    plan_listener: Option<PlanListener>,
    planning_tools: Vec<String>,
    caps: ModCaps,
    host_mods: Vec<ModRef>,
}

impl fmt::Debug for ModOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModOptions")
            .field("cwd", &self.cwd)
            .field("workspace", &self.workspace)
            .field("config_dir", &self.config_dir)
            .field("user_mods", &self.user_mods)
            .field("project_mods", &self.project_mods)
            .field("approvals", &self.approvals)
            .field("auto", &self.auto)
            .field("shell_tools", &self.shell_tools)
            .field("plan_listener", &self.plan_listener.is_some())
            .field("planning_tools", &self.planning_tools)
            .field("caps", &self.caps)
            .field("host_mods", &self.host_mods)
            .finish()
    }
}

impl ModOptions {
    /// The defaults for a conversation in `cwd`, whose agent's tools run in
    /// `workspace`, for a host whose own config directory is `config_dir`:
    /// approvals on over [`SHELL_TOOLS`], no mod files, no auto mode, no plan
    /// listener, the [`PLANNING_TOOLS`], the default [`ModCaps`], no mods of
    /// the host's own.
    pub fn new(
        cwd: impl Into<PathBuf>,
        workspace: WorkspaceMode,
        config_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            workspace,
            config_dir: config_dir.into(),
            user_mods: false,
            project_mods: false,
            approvals: true,
            auto: None,
            shell_tools: SHELL_TOOLS.iter().map(|t| t.to_string()).collect(),
            plan_listener: None,
            planning_tools: PLANNING_TOOLS.iter().map(|t| t.to_string()).collect(),
            caps: ModCaps::default(),
            host_mods: Vec::new(),
        }
    }

    /// Whether to load the human's own mods too, `<config_dir>/mods`. Off by
    /// default: a host asks for them.
    #[must_use]
    pub fn with_user_mods(mut self, on: bool) -> Self {
        self.user_mods = on;
        self
    }

    /// Whether to load the project's mods too, `<cwd>/.haest/mods`. Off by
    /// default: a repository's mod is that repository's code running in the
    /// host with full trust, and cloning a repository must never be enough
    /// to run it. Turn it on only for a directory the human trusts.
    #[must_use]
    pub fn with_project_mods(mut self, on: bool) -> Self {
        self.project_mods = on;
        self
    }

    /// Whether to load the built-in approval mod ([`ApprovalMod`]). On by
    /// default.
    #[must_use]
    pub fn with_approvals(mut self, on: bool) -> Self {
        self.approvals = on;
        self
    }

    /// Auto mode's judge for the approval mod; without one, the irreversible
    /// list alone decides.
    #[must_use]
    pub fn with_auto(mut self, judge: impl Into<AutoJudgeRef>) -> Self {
        self.auto = Some(judge.into());
        self
    }

    /// The shell tools the approval mod judges, by name, in place of
    /// [`SHELL_TOOLS`] (`Bash`).
    #[must_use]
    pub fn with_shell_tools(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.shell_tools = names.into_iter().map(Into::into).collect();
        self
    }

    /// Told whenever plan mode turns on (`true`) or off (`false`), however it
    /// was switched ([`PlanMod::with_listener`]).
    #[must_use]
    pub fn with_plan_listener(mut self, f: impl Fn(bool) + Send + Sync + 'static) -> Self {
        self.plan_listener = Some(Arc::new(f));
        self
    }

    /// The tools plan mode lets run though they are not read-only, by name,
    /// in place of [`PLANNING_TOOLS`].
    #[must_use]
    pub fn with_planning_tools(
        mut self,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.planning_tools = names.into_iter().map(Into::into).collect();
        self
    }

    /// The bounds every mod file runs in, in place of [`ModCaps::default`].
    #[must_use]
    pub fn with_caps(mut self, caps: ModCaps) -> Self {
        self.caps = caps;
        self
    }

    /// Add a native mod of the host's own (the mod itself, or an `Arc` of it
    /// the host keeps): it loads after the built-ins and every mod file,
    /// whatever else is on, and the host's mods in the order they were added.
    #[must_use]
    pub fn with_mod(mut self, m: impl Into<ModRef>) -> Self {
        self.host_mods.push(m.into());
        self
    }

    /// The conversation's working directory: the project's mods are in
    /// `<cwd>/.haest/mods`, and every mod sees it as [`ModEnv::cwd`].
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The workspace the agent's tools run in; the approval mod asks only in
    /// [`WorkspaceMode::Real`].
    pub fn workspace(&self) -> WorkspaceMode {
        self.workspace
    }

    /// The host's own config directory: the human's mods are in `<config_dir>/mods`.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Whether `<config_dir>/mods` loads ([`with_user_mods`](Self::with_user_mods)).
    pub fn user_mods(&self) -> bool {
        self.user_mods
    }

    /// Whether `<cwd>/.haest/mods` loads ([`with_project_mods`](Self::with_project_mods)).
    pub fn project_mods(&self) -> bool {
        self.project_mods
    }

    /// Whether the approval mod loads ([`with_approvals`](Self::with_approvals)).
    pub fn approvals(&self) -> bool {
        self.approvals
    }

    /// The shell tools the approval mod judges.
    pub fn shell_tools(&self) -> &[String] {
        &self.shell_tools
    }

    /// The tools plan mode lets run though they are not read-only.
    pub fn planning_tools(&self) -> &[String] {
        &self.planning_tools
    }

    /// The bounds every mod file runs in.
    pub fn caps(&self) -> ModCaps {
        self.caps
    }

    /// The host's own native mods, in order ([`with_mod`](Self::with_mod)).
    pub fn host_mods(&self) -> &[ModRef] {
        &self.host_mods
    }

    /// Where mod files are looked for, in load order: `<config_dir>/mods`
    /// with [`with_user_mods`](Self::with_user_mods), then `<cwd>/.haest/mods`
    /// with [`with_project_mods`](Self::with_project_mods). Empty by default.
    pub fn mod_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if self.user_mods {
            dirs.push(self.config_dir.join("mods"));
        }
        if self.project_mods {
            dirs.push(self.cwd.join(".haest").join("mods"));
        }
        dirs
    }
}

fn is_mod_file(name: &str) -> bool {
    let code = [".ts", ".js", ".mjs"].iter().any(|ext| name.ends_with(ext));
    code && !name.ends_with(".d.ts")
}

/// The mod files in `dir`, sorted by name: every `.ts`, `.js` and `.mjs`
/// file, but not a `.d.ts` declaration file. None when `dir` cannot be read
/// (it does not exist); subdirectories are not searched.
pub(crate) async fn mod_files(fs: &dyn Fs, dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs.read_dir(dir).await else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .into_iter()
        .map(|e| e.name)
        .filter(|n| is_mod_file(n))
        .collect();
    names.sort();
    names.into_iter().map(|n| dir.join(n)).collect()
}

/// Every mod for one conversation, in the order their events run.
///
/// 1. Built in: the approval mod (unless [`ModOptions::with_approvals`] is off,
///    with auto mode's judge when [`ModOptions::with_auto`] gave one), then
///    plan mode, always.
/// 2. The human's own mod files, `<config_dir>/mods/*`, with
///    [`ModOptions::with_user_mods`].
/// 3. The project's, `<cwd>/.haest/mods/*`, with
///    [`ModOptions::with_project_mods`] only.
/// 4. The host's own native mods ([`ModOptions::with_mod`]), in order.
///
/// Each directory's `.ts`, `.js` and `.mjs` files (not `.d.ts`) load in name
/// order (subdirectories are not searched); each one runs its default export
/// now ([`JsMod::load_with`], within [`ModOptions::caps`]). A mod that fails
/// to load is reported through [`ModUi::error`](crate::ModUi::error) and
/// skipped; the rest still load, so this never fails. The [`Mods`] returned
/// never changes.
///
/// `fs` is the HOST's own files (the real disk), never the agent's
/// workspace: a mod file the agent wrote in its sandbox must not load. `ui`
/// is how the mods reach the human.
///
/// Must be called inside a tokio runtime, or every mod file fails to load.
///
/// # Examples
///
/// The [crate docs](crate#putting-mods-in-the-loop) load a mod file from a
/// [`MemFs`](haest_core::MemFs) and put it around an agent.
pub async fn load_mods(cfg: ModOptions, fs: &dyn Fs, ui: impl Into<ModUiRef>) -> Mods {
    let ui = ui.into();
    let env = ModEnv::new(cfg.cwd.clone(), cfg.workspace, ui.clone());
    let mut mods = Mods::builder(env.clone());
    if cfg.approvals {
        let mut approval = ApprovalMod::new().with_shell_tools(cfg.shell_tools.clone());
        if let Some(auto) = cfg.auto.clone() {
            approval = approval.with_auto(auto);
        }
        mods = mods.with(approval);
    }
    let plan = PlanMod::new()
        .listened_by(cfg.plan_listener.clone())
        .with_planning_tools(cfg.planning_tools.clone());
    mods = mods.with(plan);
    for dir in cfg.mod_dirs() {
        for path in mod_files(fs, &dir).await {
            let loaded = match fs.read_to_string(&path).await {
                Ok(source) => JsMod::load_with(&path, source, &env, cfg.caps)
                    .await
                    .map_err(|e| report(&e)),
                Err(e) => Err(e.to_string()),
            };
            match loaded {
                Ok(m) => mods = mods.with(m),
                Err(why) => ui.error(&format!(
                    "Mod {} failed to load and was skipped: {why}",
                    path.display()
                )),
            }
        }
    }
    mods.with_all(cfg.host_mods).build()
}

#[cfg(test)]
mod tests;
