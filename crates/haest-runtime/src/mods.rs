//! [`mods_for`]: the conversation's mods.

use haest_bridges::{ModFileOptions, ModFiles};
use haest_core::{fs::FsRef, tool::ToolServices};
use haest_mods::{ApprovalGate, ModEnv, ModLine, ModOptions, ModRef, ModUiRef, Mods, load_mods};

use crate::RuntimeHooks;

/// Every mod the settings turn on, mod files read through `fs`, the human's
/// answers through `gate`.
///
/// `approvals` turns on the built-in approval mod and plan mode, `mods` the
/// human's mod files in `<config dir>/mods`, `project_mods` the project's in
/// `.haest/mods` (the repo's code, so a loader turns it on only in a trusted
/// dir; whoever loaded the settings decided). The mod files are one mod
/// ([`ModFiles`]), after the built-ins; the host's own mods (`host_mods`) run
/// after them, whatever the settings say. A mod's lines, and a file that
/// does not load, go to the host's [`Ui`](crate::Ui). An approval ask goes
/// through `gate` ([`ApprovalGate`]) to the host's prompter, which decides
/// how a human answers; an ask nobody answers is a deny after its timeout.
/// Never fails: the mods that loaded are the ones returned.
pub(crate) async fn mods_for(
    hooks: &RuntimeHooks,
    fs: FsRef,
    gate: &ApprovalGate,
    host_mods: Vec<ModRef>,
) -> Mods {
    let s = hooks.settings();
    let to = hooks.clone();
    let mod_ui: ModUiRef = ModUiRef::new(gate.mod_ui(move |line| match line {
        ModLine::Warn(text) => to.warn(text),
        ModLine::Error(text) => to.error(text),
        ModLine::Debug(message) => ToolServices::debug(&to, message),
        // Info, and a kind of line newer than this match: shown.
        other => to.info(other.text()),
    }));
    let mut cfg = ModOptions::new(&s.cwd, s.workspace).with_approvals(s.approvals);
    if s.mods || s.project_mods {
        let opts = ModFileOptions::new(&s.config_dir)
            .with_user_mods(s.mods)
            .with_project_mods(s.project_mods)
            .with_reload(s.mod_reload);
        let env = ModEnv::new(&s.cwd, s.workspace, mod_ui.clone());
        cfg = cfg.with_mod(ModFiles::load(opts, fs, env).await);
    }
    let cfg = host_mods.into_iter().fold(cfg, ModOptions::with_mod);
    load_mods(cfg, mod_ui)
}
