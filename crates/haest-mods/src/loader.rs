//! Loading the mods of one conversation, in this order:
//!   1. built-in: the approval mod (`approvals`, with auto mode's judge when the
//!      host has one; a gate, so it judges a call after every mod's rewrite)
//!      and plan mode (`/plan`);
//!   2. the host's own mods ([`ModOptions::with_mod`]), in the order given.
//!      A conversation's `.js` / `.ts` mod files are one of them: haest-bridges'
//!      `ModFiles` (feature `js`), which the host adds first.

use std::path::PathBuf;

use haest_core::workspace::WorkspaceMode;

use crate::{
    api::ModRef,
    approval::ApprovalMod,
    env::{ModEnv, ModUiRef},
    plan::PlanMod,
    registry::Mods,
};

/// Which mods [`load_mods`] loads for one conversation.
///
/// [`ModOptions::new`] is the bare default: the approval mod and plan mode.
/// The `with_*` setters change it.
///
/// ```
/// use haest_core::workspace::WorkspaceMode;
/// use haest_mods::{ApprovalMod, ModOptions, load_mods};
/// # use haest_mods::{ApprovalDecision, ApprovalRequest, ModUi};
/// # struct Terminal;
/// # #[async_trait::async_trait]
/// # impl ModUi for Terminal {
/// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> ApprovalDecision { ApprovalDecision::Deny }
/// #     fn info(&self, _: &str) {}
/// # }
///
/// let opts = ModOptions::new("/project", WorkspaceMode::Real)
///     .with_approval(ApprovalMod::new().with_shell_tools(["Bash", "Shell"]));
/// assert_eq!(load_mods(opts, Terminal).names(), ["approval", "plan"]);
/// ```
#[derive(Debug)]
pub struct ModOptions {
    cwd: PathBuf,
    workspace: WorkspaceMode,
    /// None = approvals off.
    approval: Option<ApprovalMod>,
    plan: PlanMod,
    host_mods: Vec<ModRef>,
}

impl ModOptions {
    /// The defaults for a conversation in `cwd`, whose agent's tools run in
    /// `workspace`: the default [`ApprovalMod`] and [`PlanMod`], no mods of
    /// the host's own.
    pub fn new(cwd: impl Into<PathBuf>, workspace: WorkspaceMode) -> Self {
        Self {
            cwd: cwd.into(),
            workspace,
            approval: Some(ApprovalMod::new()),
            plan: PlanMod::new(),
            host_mods: Vec::new(),
        }
    }

    /// Whether to load the built-in approval mod ([`ApprovalMod`]). On by
    /// default.
    #[must_use]
    pub fn with_approvals(mut self, on: bool) -> Self {
        self.approval = if on {
            self.approval.or_else(|| Some(ApprovalMod::new()))
        } else {
            None
        };
        self
    }

    /// The approval mod as the host configured it (its auto mode's judge,
    /// its shell tools), in place of [`ApprovalMod::new`]; it turns
    /// approvals on.
    #[must_use]
    pub fn with_approval(mut self, approval: ApprovalMod) -> Self {
        self.approval = Some(approval);
        self
    }

    /// Plan mode as the host configured it (its listener, its planning
    /// tools), in place of [`PlanMod::new`].
    #[must_use]
    pub fn with_plan(mut self, plan: PlanMod) -> Self {
        self.plan = plan;
        self
    }

    /// Add a mod of the host's own (the mod itself, or an `Arc` of it the
    /// host keeps): it loads after the built-ins, whatever else is on, and
    /// the host's mods in the order they were added. A conversation's mod
    /// files (haest-bridges' `ModFiles`) go in here, first.
    #[must_use]
    pub fn with_mod(mut self, m: impl Into<ModRef>) -> Self {
        self.host_mods.push(m.into());
        self
    }
}

/// Every mod for one conversation, in the order their events run.
///
/// 1. Built in: the approval mod (unless [`ModOptions::with_approvals`] is off,
///    as [`ModOptions::with_approval`] configured it), then plan mode, always
///    ([`ModOptions::with_plan`]).
/// 2. The host's own mods ([`ModOptions::with_mod`]), in order: its mod
///    files first (haest-bridges' `ModFiles`), when it has any.
///
/// The [`Mods`] returned never changes. `ui` is how the mods reach the
/// human.
///
/// # Examples
///
/// [`PlanMod`'s example](PlanMod#examples) loads the built-ins and turns
/// plan mode on.
pub fn load_mods(cfg: ModOptions, ui: impl Into<ModUiRef>) -> Mods {
    let env = ModEnv::new(cfg.cwd, cfg.workspace, ui);
    let mut mods: Vec<ModRef> = cfg.approval.into_iter().map(ModRef::new).collect();
    mods.push(ModRef::new(cfg.plan));
    mods.extend(cfg.host_mods);
    Mods::new(env, mods)
}

#[cfg(test)]
mod tests;
