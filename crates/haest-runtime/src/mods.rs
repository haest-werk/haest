//! Mods: the built-in approval mod and plan mode (`approvals`), the human's
//! mods in `<config dir>/mods` (`mods`) and the project's in `.haest/mods`
//! (`project_mods`: the repo's code, so a loader turns it on only in a trusted
//! dir). An approval ask goes to the host's [`Prompter`]; the host decides how
//! a human answers.

use std::sync::Arc;

use crate::{
    RuntimeHooks,
    haest::{
        core::fs::Fs,
        core::tool::ToolServices,
        mods::{ApprovalGate, ModLine, ModOptions, Mods, Prompter, load_mods},
    },
};

/// Every mod the settings turn on, read through `fs`, the human's answers
/// through `prompter`.
///
/// `approvals` turns on the built-in approval mod, `mods` the human's,
/// `project_mods` the project's (whoever loaded the settings decided whether
/// the project is trusted). A mod's notices and errors, and one that does not
/// load, go to the host's [`Ui`](crate::Ui); an approval ask goes to
/// `prompter`, and an ask nobody answers is a deny after 10 minutes. Never
/// fails: the mods that loaded are the ones returned.
pub async fn load<P: Prompter + 'static>(
    hooks: &Arc<RuntimeHooks>,
    fs: &dyn Fs,
    prompter: P,
) -> Arc<Mods> {
    let s = &hooks.settings;
    let mut cfg = ModOptions::new(&s.cwd, s.workspace, &s.config_dir);
    cfg.approvals = s.approvals;
    cfg.user_mods = s.mods;
    cfg.project_mods = s.project_mods;
    let to = hooks.clone();
    let mod_ui = ApprovalGate::new(prompter).with_lines(move |line| match line {
        ModLine::Info(text) => to.info(text),
        ModLine::Error(text) => to.error(text),
        ModLine::Debug(message) => ToolServices::debug(to.as_ref(), message),
    });
    Arc::new(load_mods(cfg, fs, Arc::new(mod_ui)).await)
}
