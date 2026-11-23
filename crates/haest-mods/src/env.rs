//! What every mod event can reach ([`ModEnv`]), and the host's half
//! ([`ModUi`]): the human's Allow / Deny and the lines they see.

use std::{
    error::Error,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use haest_core::{error::report, workspace::WorkspaceMode};
use tokio_util::sync::CancellationToken;

use crate::ask::{ApprovalRequest, Decision};

/// PLUG POINT: how mods reach the human, implemented by the host (a CLI, a
/// control panel, a chat app).
///
/// `ask` and the lines reach the human; how is the host's business. Most hosts
/// do not implement this: an [`ApprovalGate`](crate::ApprovalGate) over their
/// own [`Prompter`](crate::Prompter) keeps the rules every ask must keep, and
/// [`ApprovalGate::mod_ui`](crate::ApprovalGate::mod_ui) makes it a
/// `ModUi` that sends every other line where the host says.
/// [`ApprovalGate`](crate::ApprovalGate#examples)'s example is a whole host.
///
/// Haest holds one in a [`ModUiRef`].
#[async_trait]
pub trait ModUi: Send + Sync {
    /// The human's Allow / Deny. Deny when `cancel` fires (an interrupt) or the
    /// human never answers -- a tool call must never hang on someone who walked away.
    async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision;
    /// A line the human sees: what a mod has to say (plan mode is on).
    fn info(&self, text: &str);
    /// Something was skipped or held and the conversation goes on; the
    /// human should see it. Default: an [`info`](Self::info) line.
    fn warn(&self, text: &str) {
        self.info(text)
    }
    /// A mod failed to load or in an event; the human must see it. Default:
    /// an [`info`](Self::info) line.
    fn error(&self, text: &str) {
        self.info(text)
    }
    /// A line for haest's diagnostic log, which the human need not see (each
    /// mod as it loads). Default: dropped.
    fn debug(&self, _message: &str) {}
}

haest_core::plug_ref! {
    /// A [`ModUi`], shared: how the host reaches the human. A parameter that
    /// takes `impl Into<ModUiRef>` takes the host's value itself, or an `Arc`
    /// of it.
    pub struct ModUiRef(dyn ModUi);
}

/// Where haest runs, and the human: what every mod event can reach.
///
/// One per conversation, shared by every mod in [`Mods`](crate::Mods); the
/// events get it as their `haest` argument. Cheap to clone.
#[derive(Debug, Clone)]
pub struct ModEnv {
    cwd: PathBuf,
    workspace: WorkspaceMode,
    ui: ModUiRef,
}

impl ModEnv {
    /// The environment for one conversation's mods: its working directory,
    /// the workspace its tools run in, and the host's [`ModUi`].
    pub fn new(cwd: impl Into<PathBuf>, workspace: WorkspaceMode, ui: impl Into<ModUiRef>) -> Self {
        Self {
            cwd: cwd.into(),
            workspace,
            ui: ui.into(),
        }
    }

    /// The conversation's working directory.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The workspace the agent's tools run in:
    /// [`ApprovalMod`](crate::ApprovalMod) asks only in `Real`.
    pub fn workspace(&self) -> WorkspaceMode {
        self.workspace
    }

    /// The host's ask, and its info, warn, error and debug lines.
    pub fn ui(&self) -> &ModUiRef {
        &self.ui
    }

    /// The human's Allow / Deny, through the host; Deny on interrupt or
    /// timeout. An ask under an already-interrupted call (`cancel` fired) is a
    /// Deny at once, never a prompt.
    pub async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision {
        if cancel.is_cancelled() {
            return Decision::Deny;
        }
        self.ui.ask(req, cancel).await
    }

    /// A line the human sees, through the host ([`ModUi::info`]).
    pub fn info(&self, text: &str) {
        self.ui.info(text)
    }

    /// Something skipped or held, through the host ([`ModUi::warn`]).
    pub fn warn(&self, text: &str) {
        self.ui.warn(text)
    }

    /// Something failed, through the host ([`ModUi::error`]).
    pub fn error(&self, text: &str) {
        self.ui.error(text)
    }

    /// A line for the host's diagnostic log ([`ModUi::debug`]).
    pub fn debug(&self, message: &str) {
        self.ui.debug(message)
    }

    /// The mod (or the hooks) `name` failed in `event`: the human sees it,
    /// with every cause ([`ModUi::error`]).
    pub(crate) fn report_failure(&self, name: &str, event: &str, e: &(dyn Error + 'static)) {
        let why = report(e);
        self.error(&format!("Mod {name} failed in {event}: {why}"));
    }
}
