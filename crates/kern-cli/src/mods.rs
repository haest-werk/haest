//! Mods in the CLI: the built-in approval mod and plan mode, the human's mods
//! (`<config dir>/mods`) and, with KERN_PROJECT_MODS=1, the project's
//! (`<cwd>/.kern/mods`). An approval ask goes to the terminal; with no terminal
//! to ask on, it is a deny that says why.

use std::sync::Arc;

use async_trait::async_trait;
use kern::{
    fs::Fs,
    mods::{
        Answer, ApprovalGate, ApprovalRequest, Decision, ModConfig, ModHost, Mods, Prompter,
        load_mods,
    },
    tool::ToolHost,
};
use tokio_util::sync::CancellationToken;

use crate::{ask, host::Host};

struct TermPrompter {
    host: Arc<Host>,
    interactive: bool,
}

fn question(req: &ApprovalRequest) -> String {
    let mut q = format!("Allow {}? {}", req.tool_name, req.description);
    let preview = req.input_preview.trim();
    if !preview.is_empty() && preview != req.description.trim() {
        q.push_str(&format!("\n  {preview}"));
    }
    q + "\n[y]es / [a]lways / [N]o: "
}

/// y / yes, a / always; anything else (an empty line included) is no.
fn answer(line: Option<&str>) -> Answer {
    match line.map(|l| l.trim().to_ascii_lowercase()).as_deref() {
        Some("y" | "yes") => Answer::Allow,
        Some("a" | "always") => Answer::AllowAlways,
        _ => Answer::Deny,
    }
}

#[async_trait]
impl Prompter for TermPrompter {
    async fn prompt(&self, req: &ApprovalRequest) -> Answer {
        if !self.interactive {
            self.host.warn(&format!(
                "Denied {}: {} -- approval needed and there is no terminal to ask on (KERN_APPROVALS=off turns approvals off).",
                req.tool_name, req.description
            ));
            return Answer::Deny;
        }
        self.host.ask(&question(req));
        answer(ask::read_line().await.as_deref())
    }

    fn auto_approved(&self, req: &ApprovalRequest) {
        self.host.info(&format!(
            "Allowed {} (always): {}",
            req.tool_name, req.description
        ));
    }
}

pub struct CliModHost {
    host: Arc<Host>,
    gate: ApprovalGate<TermPrompter>,
}

impl CliModHost {
    pub fn new(host: Arc<Host>, interactive: bool) -> Self {
        let prompter = TermPrompter {
            host: host.clone(),
            interactive,
        };
        Self {
            host,
            gate: ApprovalGate::new(prompter),
        }
    }
}

#[async_trait]
impl ModHost for CliModHost {
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

/// Every mod this conversation runs, the human's ask on the terminal when there
/// is one. Plan mode announces its own switches (through `notice`).
pub async fn load(host: &Arc<Host>, fs: &dyn Fs) -> Arc<Mods> {
    let cfg = &host.cfg;
    let mut mods = ModConfig::new(&cfg.cwd, cfg.workspace, &cfg.config_dir);
    mods.approvals = cfg.features.approvals;
    mods.project_mods = cfg.features.project_mods;
    let mod_host = Arc::new(CliModHost::new(host.clone(), ask::interactive()));
    Arc::new(load_mods(mods, fs, mod_host).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_yes_or_always_allow_and_silence_is_no() {
        assert_eq!(answer(Some("y\n")), Answer::Allow);
        assert_eq!(answer(Some(" YES ")), Answer::Allow);
        assert_eq!(answer(Some("a\n")), Answer::AllowAlways);
        assert_eq!(answer(Some("\n")), Answer::Deny);
        assert_eq!(answer(Some("sure")), Answer::Deny);
        assert_eq!(answer(None), Answer::Deny);
    }
}
