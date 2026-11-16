//! Loading the mods of one conversation, in this order:
//!   1. built-in: the approval mod (`approvals`, with auto mode's judge when the
//!      host has one; a gate, so it judges a call after every mod's rewrite)
//!      and plan mode (`/plan`);
//!   2. the human's own: `<config_dir>/mods/*.ts|js|mjs` -- with `user_mods`;
//!   3. the project's: `<cwd>/.haest/mods/*.ts|js|mjs` -- ONLY with `project_mods`.
//!      A repo's mod is code from that repo running inside the host with full
//!      trust; cloning a repo must never be enough to run it.
//!
//! A mod that fails to load is reported to the human and skipped; the rest
//! still load. A host's own native mods go in with [`Mods::add`] afterwards.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use haest_core::{error::report, fs::Fs, workspace::WorkspaceMode};

use crate::{
    api::{ModEnv, ModUi},
    approval::{ApprovalMod, AutoJudge, SHELL_TOOLS},
    js::{JsMod, ModCaps},
    plan::{PLANNING_TOOLS, PlanListener, PlanMod},
    registry::Mods,
};

/// Which mods [`load_mods`] loads for one conversation, and where from.
///
/// [`ModConfig::new`] is the bare default: the approval mod and plan mode, no
/// mod files. Set the fields to change it.
pub struct ModConfig {
    /// The conversation's working directory: the project's mods are in
    /// `<cwd>/.haest/mods`, and every mod sees it as [`ModEnv::cwd`].
    pub cwd: PathBuf,
    /// The workspace the agent's tools run in; the approval mod asks only in
    /// [`WorkspaceMode::Real`].
    pub workspace: WorkspaceMode,
    /// The host's own config directory: the human's mods are in `<config_dir>/mods`.
    pub config_dir: PathBuf,
    /// Load `<config_dir>/mods`. Off by default: a host asks for them.
    pub user_mods: bool,
    /// Also load `<cwd>/.haest/mods`. Off by default: a repository's mod is
    /// that repository's code running in the host with full trust, and
    /// cloning a repository must never be enough to run it. Turn it on only
    /// for a directory the human trusts.
    pub project_mods: bool,
    /// The built-in approval mod ([`ApprovalMod`]). On by default.
    pub approvals: bool,
    /// Auto mode's judge for the approval mod; None = the irreversible list alone.
    pub auto: Option<Arc<dyn AutoJudge>>,
    /// The shell tools the approval mod judges, by name. [`SHELL_TOOLS`]
    /// (`Bash`) by default.
    pub shell_tools: Vec<String>,
    /// Told whenever plan mode turns on or off.
    pub on_plan_change: Option<PlanListener>,
    /// The tools plan mode lets run though they are not read-only, by name.
    /// [`PLANNING_TOOLS`] by default.
    pub planning_tools: Vec<String>,
    /// The bounds every mod file runs in. [`ModCaps::default`] by default.
    pub caps: ModCaps,
}

impl ModConfig {
    /// The defaults for a conversation in `cwd`: approvals on over
    /// [`SHELL_TOOLS`], no mod files, no auto mode, no plan listener, the
    /// [`PLANNING_TOOLS`], the default [`ModCaps`].
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
            on_plan_change: None,
            planning_tools: PLANNING_TOOLS.iter().map(|t| t.to_string()).collect(),
            caps: ModCaps::default(),
        }
    }

    /// Where mod files are looked for, in load order: `<config_dir>/mods`
    /// with [`user_mods`](Self::user_mods), then `<cwd>/.haest/mods` with
    /// [`project_mods`](Self::project_mods). Empty by default.
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
pub async fn mod_files(fs: &dyn Fs, dir: &Path) -> Vec<PathBuf> {
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
/// 1. Built in: the approval mod (with [`ModConfig::approvals`], and auto
///    mode's judge when [`ModConfig::auto`] has one), then plan mode, always.
/// 2. The human's own mod files, `<config_dir>/mods/*`, with [`ModConfig::user_mods`].
/// 3. The project's, `<cwd>/.haest/mods/*`, with [`ModConfig::project_mods`] only.
///
/// Each directory's files load in name order ([`mod_files`]); each one runs
/// its default export now ([`JsMod::load_with`], within
/// [`ModConfig::caps`]). A mod that fails to load is
/// reported through [`ModUi::error`] and skipped; the rest still load, so
/// this never fails. A host's own native mods go in afterwards with
/// [`Mods::add`].
///
/// `fs` is the HOST's own files (the real disk), never the agent's
/// workspace: a mod file the agent wrote in its sandbox must not load.
///
/// Must be called inside a tokio runtime, or every mod file fails to load.
///
/// # Examples
///
/// The [crate docs](crate#putting-mods-in-the-loop) load a mod file from a
/// [`MemFs`](haest_core::MemFs) and put it around an agent.
pub async fn load_mods(cfg: ModConfig, fs: &dyn Fs, ui: Arc<dyn ModUi>) -> Mods {
    let env = ModEnv::new(cfg.cwd.clone(), cfg.workspace, ui.clone());
    let mut mods = Mods::new(env.clone());
    if cfg.approvals {
        let approval = ApprovalMod::new(cfg.auto.clone()).with_shell_tools(cfg.shell_tools.clone());
        mods.add(Arc::new(approval));
    }
    let plan =
        PlanMod::new(cfg.on_plan_change.clone()).with_planning_tools(cfg.planning_tools.clone());
    mods.add(Arc::new(plan));
    for dir in cfg.mod_dirs() {
        for path in mod_files(fs, &dir).await {
            let loaded = match fs.read_to_string(&path).await {
                Ok(source) => JsMod::load_with(&path, source, &env, cfg.caps)
                    .await
                    .map_err(|e| report(&e)),
                Err(e) => Err(e.to_string()),
            };
            match loaded {
                Ok(m) => mods.add(Arc::new(m)),
                Err(why) => ui.error(&format!(
                    "Mod {} failed to load and was skipped: {why}",
                    path.display()
                )),
            }
        }
    }
    mods
}

#[cfg(test)]
mod tests;
