//! What a mod is ([`Mod`]), what it can reach ([`ModEnv`]), and the host's half
//! ([`ModHost`]): the human's Allow / Deny and the lines they see.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use kern_core::{
    tool::{JsonMap, Tool, ToolContext, ToolOutput},
    workspace::WorkspaceMode,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// One tool call as a mod sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModToolCall {
    pub name: String,
    pub input: JsonMap,
    /// The tool declares it changes nothing ([`Tool::read_only`]).
    pub read_only: bool,
}

/// What a `tool.before` hook decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Before {
    /// Let the call through as it is.
    Pass,
    /// Refuse it: the model reads the reason. The first deny wins.
    Deny(String),
    /// Replace its input, for the hooks after this one and for the tool.
    Input(JsonMap),
}

/// An Allow / Deny prompt for the human.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub description: String,
    pub input_preview: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// What the human's ALWAYS on this ask covers (asks with the same tool and key
    /// answer themselves); None = the description. A Bash ask uses the exact command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// The human's answer to one ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
        }
    }
}

/// A mod failed in one of its events. The host reports it to the human; it never
/// takes the host down.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct ModError(pub String);

impl ModError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// PLUG POINT: what a host (a CLI, a control panel) gives mods. `ask` and
/// `notice` reach the human; how is the host's business.
#[async_trait]
pub trait ModHost: Send + Sync {
    /// The human's Allow / Deny. Deny when `cancel` fires (an interrupt) or the
    /// human never answers -- a tool call must never hang on someone who walked away.
    async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision;
    /// A line the human sees.
    fn notice(&self, text: &str);
    /// A mod failed to load or in an event; the human must see it. Default: a notice.
    fn error(&self, text: &str) {
        self.notice(text)
    }
    /// kern's diagnostic log.
    fn log(&self, _message: &str) {}
}

/// Where kern runs, and the human: what every mod event can reach.
#[derive(Clone)]
pub struct ModEnv {
    pub cwd: PathBuf,
    pub workspace: WorkspaceMode,
    pub host: Arc<dyn ModHost>,
}

impl ModEnv {
    pub fn new(cwd: impl Into<PathBuf>, workspace: WorkspaceMode, host: Arc<dyn ModHost>) -> Self {
        Self {
            cwd: cwd.into(),
            workspace,
            host,
        }
    }

    /// The human's Allow / Deny; Deny on interrupt or timeout. An ask under an
    /// already-interrupted call is a Deny at once, never a prompt.
    pub async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision {
        if cancel.is_cancelled() {
            return Decision::Deny;
        }
        self.host.ask(req, cancel).await
    }

    pub fn notice(&self, text: &str) {
        self.host.notice(text)
    }
}

/// A NATIVE mod: plain Rust with the same events a mod file has. Every method
/// has a default that does nothing, so a mod implements only what it hooks.
/// Built-ins (approval, plan) are mods like this; so is a host's own.
///
/// A mod runs with the HOST's full trust, sandbox or not.
#[async_trait]
pub trait Mod: Send + Sync {
    /// Who this mod is in a report: a built-in's name or a file's path.
    fn name(&self) -> &str;

    /// A gate (the approval mod) judges a call as it will run: its
    /// `tool.before` goes after every other mod's, on the input they rewrote,
    /// so no later rewrite slips past it. Gates run in load order.
    fn is_gate(&self) -> bool {
        false
    }

    /// `tool.before`: before every tool call. [`Before::Deny`] refuses it (the
    /// model reads the reason), [`Before::Input`] replaces its input.
    async fn tool_before(
        &self,
        _call: &ModToolCall,
        _ctx: &ToolContext,
        _kern: &ModEnv,
    ) -> Result<Before, ModError> {
        Ok(Before::Pass)
    }

    /// `tool.after`: after it ran; Some replaces the output.
    async fn tool_after(
        &self,
        _call: &ModToolCall,
        _output: &ToolOutput,
        _ctx: &ToolContext,
        _kern: &ModEnv,
    ) -> Result<Option<ToolOutput>, ModError> {
        Ok(None)
    }

    /// `prompt.submit`: a human message before the model sees it; Some rewrites it.
    async fn prompt_submit(&self, _text: &str, _kern: &ModEnv) -> Result<Option<String>, ModError> {
        Ok(None)
    }

    /// `system.prompt`: text appended to the system prompt, asked before every model call.
    async fn system_prompt(&self, _kern: &ModEnv) -> Result<Option<String>, ModError> {
        Ok(None)
    }

    /// Tools this mod adds (asked again before every model call).
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    /// The slash commands this mod answers, without the `/`.
    fn commands(&self) -> Vec<String> {
        Vec::new()
    }

    /// Run one of [`Mod::commands`]: Some text goes to the model; None = the mod
    /// handled the line itself.
    async fn command(
        &self,
        _name: &str,
        _args: &str,
        _kern: &ModEnv,
    ) -> Result<Option<String>, ModError> {
        Ok(None)
    }
}
