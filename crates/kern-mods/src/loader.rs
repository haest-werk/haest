//! Loading the mods of one conversation, in this order:
//!   1. built-in: the approval mod (`approvals`, with auto mode's judge when the
//!      host has one; a gate, so it judges a call after every mod's rewrite)
//!      and plan mode (`/plan`);
//!   2. the human's own: `<config_dir>/mods/*.ts|js|mjs` -- with `user_mods`;
//!   3. the project's: `<cwd>/.kern/mods/*.ts|js|mjs` -- ONLY with `project_mods`.
//!      A repo's mod is code from that repo running inside the host with full
//!      trust; cloning a repo must never be enough to run it.
//!
//! A mod that fails to load is reported to the human and skipped; the rest
//! still load. A host's own native mods go in with [`Mods::add`] afterwards.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use kern_core::{fs::Fs, workspace::WorkspaceMode};

use crate::{
    api::{ModEnv, ModHost},
    approval::{ApprovalMod, AutoJudge},
    js::{JsMod, Limits},
    plan::{PlanListener, PlanMod},
    registry::Mods,
};

pub struct ModConfig {
    pub cwd: PathBuf,
    pub workspace: WorkspaceMode,
    /// The host's own config directory: the human's mods are in `<config_dir>/mods`.
    pub config_dir: PathBuf,
    /// Load `<config_dir>/mods`. Off by default: a host asks for them.
    pub user_mods: bool,
    /// Also load `<cwd>/.kern/mods`. Off by default (see the module doc).
    pub project_mods: bool,
    /// The built-in approval mod. On by default.
    pub approvals: bool,
    /// Auto mode's judge for the approval mod; None = the irreversible list alone.
    pub auto: Option<Arc<dyn AutoJudge>>,
    /// Told whenever plan mode turns on or off.
    pub on_plan_change: Option<PlanListener>,
}

impl ModConfig {
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
            on_plan_change: None,
        }
    }

    /// Where mod files are looked for, in load order.
    pub fn mod_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if self.user_mods {
            dirs.push(self.config_dir.join("mods"));
        }
        if self.project_mods {
            dirs.push(self.cwd.join(".kern").join("mods"));
        }
        dirs
    }
}

fn is_mod_file(name: &str) -> bool {
    let code = [".ts", ".js", ".mjs"].iter().any(|ext| name.ends_with(ext));
    code && !name.ends_with(".d.ts")
}

/// The mod files in `dir`, by name; none when it cannot be read (it does not exist).
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

/// Every mod for one conversation. `fs` is the HOST's own files (the real disk),
/// never the agent's workspace.
pub async fn load_mods(cfg: ModConfig, fs: &dyn Fs, host: Arc<dyn ModHost>) -> Mods {
    load_mods_with(cfg, fs, host, Limits::default()).await
}

pub(crate) async fn load_mods_with(
    cfg: ModConfig,
    fs: &dyn Fs,
    host: Arc<dyn ModHost>,
    limits: Limits,
) -> Mods {
    let env = ModEnv::new(cfg.cwd.clone(), cfg.workspace, host.clone());
    let mut mods = Mods::new(env.clone());
    if cfg.approvals {
        mods.add(Arc::new(ApprovalMod::new(cfg.auto.clone())));
    }
    mods.add(Arc::new(PlanMod::new(cfg.on_plan_change.clone())));
    for dir in cfg.mod_dirs() {
        for path in mod_files(fs, &dir).await {
            let loaded = match fs.read_to_string(&path).await {
                Ok(source) => JsMod::load_with(&path, source, &env, limits).await,
                Err(e) => Err(e.to_string()),
            };
            match loaded {
                Ok(m) => mods.add(Arc::new(m)),
                Err(e) => host.error(&format!(
                    "Mod {} failed to load and was skipped: {e}",
                    path.display()
                )),
            }
        }
    }
    mods
}

#[cfg(test)]
mod tests;
