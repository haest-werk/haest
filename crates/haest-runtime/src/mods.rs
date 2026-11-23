//! [`mods_for`]: the conversation's mods, [`config_hooks`]: the `hooks`
//! setting bound for it, and [`name_session`]: the session they are told.

use haest_bridges::{HookHost, ModFileOptions, ModFiles};
use haest_core::{ChildEnv, fs::FsRef, tool::ToolServices};
use haest_mods::{
    ApprovalGate, HookSettings, Hooks, ModEnv, ModLine, ModOptions, ModRef, ModUiRef, Mods,
    SessionView, load_mods,
};
use haest_providers::http::HttpClientRef;

use crate::{RuntimeHooks, world::World};

/// Every mod the settings turn on, mod files read through `fs`, the human's
/// answers through `gate`.
///
/// `approvals` turns on the built-in approval mod and plan mode, `mods` the
/// human's mod files in `<config dir>/mods`, `project_mods` the project's in
/// `.haest/mods` (the repo's code, so a loader turns it on only in a trusted
/// dir; whoever loaded the settings decided). The mod files are one mod
/// ([`ModFiles`]), after the built-ins; the `hooks` setting's entries
/// (`config_hooks`, [`config_hooks`]) are one more after them, as the
/// human's own settings; the host's own mods (`host_mods`) run last,
/// whatever the settings say. A mod's lines, and a file that does not load,
/// go to the host's [`Ui`](crate::Ui). An approval ask goes through `gate`
/// ([`ApprovalGate`]) to the host's prompter, which decides how a human
/// answers; an ask nobody answers is a deny after its timeout. The mods are
/// told the session the conversation starts on ([`name_session`]). Never
/// fails: the mods that loaded are the ones returned.
pub(crate) async fn mods_for(
    hooks: &RuntimeHooks,
    fs: FsRef,
    gate: &ApprovalGate,
    config_hooks: Option<Hooks>,
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
    let cfg = config_hooks.into_iter().fold(cfg, ModOptions::with_mod);
    let cfg = host_mods.into_iter().fold(cfg, ModOptions::with_mod);
    let mods = load_mods(cfg, mod_ui);
    name_session(&mods, hooks);
    mods
}

/// Tell `mods` the session the conversation is on now: the session file
/// records go to, its id and its path. Every hook's payload names it from
/// the next event on (a `claude` hook's `session_id` and
/// `transcript_path`); [`Runtime::clear`](crate::Runtime::clear) calls this
/// again for the new one.
pub(crate) fn name_session(mods: &Mods, hooks: &RuntimeHooks) {
    let store = hooks.store();
    let session = SessionView::new(store.session_id()).with_transcript_path(store.path());
    mods.env().set_session(session);
}

/// The `hooks` setting bound for this conversation: a command on the host
/// in the settings' `cwd` with the environment every process haest starts
/// gets (none bound in a world that starts no process), a URL through
/// `http`. The session a hook is told comes with each event
/// ([`name_session`]). An entry that can't bind here is a warning. None
/// when no entry binds.
pub(crate) fn config_hooks(
    settings: &HookSettings,
    hooks: &RuntimeHooks,
    world: &World,
    passed_env: &ChildEnv,
    http: &HttpClientRef,
) -> Option<Hooks> {
    if settings.is_empty() {
        return None;
    }
    let s = hooks.settings();
    let mut host = HookHost::new(&s.cwd).with_client(http.clone());
    if let Some(env) = world.child_env_with(passed_env) {
        host = host.with_env(env);
    }
    let (bound, notes) = host.bind(settings);
    for note in notes {
        hooks.warn(&format!("{note}."));
    }
    bound
}
