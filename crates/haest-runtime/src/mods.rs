//! [`mods_for`]: the conversation's mods.

use crate::{
    RuntimeHooks,
    haest::{
        core::fs::Fs,
        core::tool::ToolServices,
        mods::{ApprovalGate, ModLine, ModOptions, Mods, load_mods},
    },
};

/// Every mod the settings turn on, read through `fs`, the human's answers
/// through `gate`.
///
/// `approvals` turns on the built-in approval mod and plan mode, `mods` the
/// human's mods in `<config dir>/mods`, `project_mods` the project's in
/// `.haest/mods` (the repo's code, so a loader turns it on only in a trusted
/// dir; whoever loaded the settings decided). A mod's lines, and one that does
/// not load, go to the host's [`Ui`](crate::Ui). An approval ask goes through
/// `gate` ([`ApprovalGate`]) to the host's prompter, which decides how a human
/// answers; an ask nobody answers is a deny after its timeout. Never fails:
/// the mods that loaded are the ones returned.
pub(crate) async fn mods_for(hooks: &RuntimeHooks, fs: &dyn Fs, gate: &ApprovalGate) -> Mods {
    let s = hooks.settings();
    let mut cfg = ModOptions::new(&s.cwd, s.workspace, &s.config_dir);
    if !s.approvals {
        cfg = cfg.without_approvals();
    }
    if s.mods {
        cfg = cfg.with_user_mods();
    }
    if s.project_mods {
        cfg = cfg.with_project_mods();
    }
    let to = hooks.clone();
    let mod_ui = gate.mod_ui(move |line| match line {
        ModLine::Info(text) => to.info(text),
        ModLine::Error(text) => to.error(text),
        ModLine::Debug(message) => ToolServices::debug(&to, message),
    });
    load_mods(cfg, fs, mod_ui).await
}
