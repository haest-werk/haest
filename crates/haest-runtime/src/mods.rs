//! Mods: the built-in approval mod and plan mode (`approvals`), the human's
//! mods in `<config dir>/mods` (`mods`) and the project's in `.haest/mods`
//! (`project_mods`: the repo's code, so a loader turns it on only in a trusted
//! dir). An approval ask goes to the host's [`Prompter`]; the host decides how
//! a human answers.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    Host,
    haest::{
        fs::Fs,
        mods::{
            ApprovalGate, ApprovalRequest, Decision, ModConfig, ModHost, Mods, Prompter, load_mods,
        },
        tool::ToolHost,
    },
};

struct RuntimeModHost<P> {
    host: Arc<Host>,
    gate: ApprovalGate<P>,
}

#[async_trait]
impl<P: Prompter + 'static> ModHost for RuntimeModHost<P> {
    async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision {
        self.gate.ask(req, cancel).await
    }

    fn notice(&self, text: &str) {
        self.host.info(text);
    }

    fn error(&self, text: &str) {
        self.host.error(text);
    }

    fn log(&self, message: &str) {
        ToolHost::log(self.host.as_ref(), message);
    }
}

/// Every mod the settings turn on, read through `fs`, the human's answers
/// through `prompter`.
///
/// `approvals` turns on the built-in approval mod, `mods` the human's,
/// `project_mods` the project's (whoever loaded the settings decided whether
/// the project is trusted). A mod's notices and errors, and one that does not
/// load, go to the host's [`Ui`](crate::Ui); an approval ask goes to
/// `prompter`, and an ask nobody answers is a deny after 10 minutes. Never
/// fails: the mods that loaded are the ones returned.
pub async fn load<P: Prompter + 'static>(host: &Arc<Host>, fs: &dyn Fs, prompter: P) -> Arc<Mods> {
    let s = &host.settings;
    let mut cfg = ModConfig::new(&s.cwd, s.workspace, &s.config_dir);
    cfg.approvals = s.approvals;
    cfg.user_mods = s.mods;
    cfg.project_mods = s.project_mods;
    let mod_host = Arc::new(RuntimeModHost {
        host: host.clone(),
        gate: ApprovalGate::new(prompter),
    });
    Arc::new(load_mods(cfg, fs, mod_host).await)
}
