//! Mods: the built-in approval mod and plan mode (`approvals`), the human's
//! mods in `<config dir>/mods` (`mods`) and the project's in `.kern/mods`
//! (`project_mods`, a trusted dir only: they are the repo's code). An approval
//! ask goes to the host's [`Prompter`]; the host decides how a human answers.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    Host,
    kern::{
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

/// Every mod the settings turn on, the human's answers through `prompter`.
/// Project mods outside a trusted dir are skipped, and the host is told.
pub async fn load<P: Prompter + 'static>(host: &Arc<Host>, fs: &dyn Fs, prompter: P) -> Arc<Mods> {
    let s = &host.settings;
    let mut cfg = ModConfig::new(&s.cwd, s.workspace, &s.config_dir);
    cfg.approvals = s.approvals;
    cfg.user_mods = s.mods;
    cfg.project_mods = s.project_mods && s.trusted;
    if s.project_mods && !s.trusted && fs.exists(&s.cwd.join(".kern").join("mods")).await {
        host.warn(&format!(
            "project_mods is on, but {} is not in trusted_dirs: its .kern/mods were not loaded.",
            s.cwd.display()
        ));
    }
    let mod_host = Arc::new(RuntimeModHost {
        host: host.clone(),
        gate: ApprovalGate::new(prompter),
    });
    Arc::new(load_mods(cfg, fs, mod_host).await)
}

#[cfg(test)]
mod tests {
    use crate::{
        kern::fs::MemFs,
        testkit::{Deny, host},
    };

    use super::*;

    #[tokio::test]
    async fn an_untrusted_dir_is_warned_about_only_when_it_has_mods() {
        let (host, ui) = host(&[("project_mods", "true")]);
        load(&host, &MemFs::new(), Deny).await;
        assert_eq!(ui.warnings(), Vec::<String>::new());
        let fs = MemFs::with_files([("/w/.kern/mods/m.ts", "export default {}")]);
        load(&host, &fs, Deny).await;
        assert_eq!(ui.warnings().len(), 1);
    }
}
