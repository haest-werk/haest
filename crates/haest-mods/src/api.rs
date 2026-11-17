//! What a mod is ([`Mod`]), what it can reach ([`ModEnv`]), and the host's half
//! ([`ModUi`]): the human's Allow / Deny and the lines they see.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use haest_core::{
    Tools,
    error::BoxError,
    tool::{JsonMap, ToolContext, ToolOutput},
    workspace::WorkspaceMode,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// One tool call as a mod sees it, in `tool.before` and `tool.after`.
///
/// A mod file's handler gets the same value as its `call` argument, in
/// camelCase: `{ name, input, readOnly }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModToolCall {
    /// The tool's name, as the model called it (`Bash`, `Write`, `mcp__github__search`).
    pub name: String,
    /// The input the tool will run with: the model's, or an earlier mod's replacement.
    pub input: JsonMap,
    /// The tool declares it changes nothing ([`Tool::read_only`](haest_core::Tool::read_only)).
    pub read_only: bool,
}

/// What a [`Mod::tool_before`] hook decided about one tool call.
#[derive(Debug, Clone, PartialEq)]
pub enum Before {
    /// Let the call through as it is.
    Pass,
    /// Refuse it: the model reads `Refused: <reason>` as the call's error
    /// result, and the tool does not run. The first deny wins; later mods are
    /// not asked.
    Deny(String),
    /// Replace its input, for the hooks after this one and for the tool.
    Input(JsonMap),
}

/// An Allow / Deny prompt for the human, raised by a mod through [`ModUi::ask`].
///
/// A mod file builds one as the `req` of `haest.ask(req, signal)`, in
/// camelCase: `{ toolName, description, inputPreview, toolUseId?, key? }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    /// The tool the ask is about (`Bash`); [`ApprovalGate`](crate::ApprovalGate) scopes ALWAYS to it.
    pub tool_name: String,
    /// The question, in one sentence: "haest wants to run a command that deletes files recursively".
    pub description: String,
    /// What the human reads to decide: the command, the start of the input.
    pub input_preview: String,
    /// The tool call this ask holds up, when the mod knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// What the human's ALWAYS on this ask covers (asks with the same tool and key
    /// answer themselves); None = the description. A Bash ask uses the exact command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

haest_core::word_enum! {
    /// The human's answer to one ask: what [`ModUi::ask`] returns. Its word
    /// (`"allow"`, `"deny"`) is what a mod file's `haest.ask` promise
    /// resolves to.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Decision ("decision") {
        /// The human allowed it.
        Allow = "allow",
        /// The human refused it, did not answer in time, or the call was interrupted.
        Deny = "deny",
    }
}

/// PLUG POINT: how mods reach the human, implemented by the host (a CLI, a
/// control panel, a chat app).
///
/// `ask` and the lines reach the human; how is the host's business. Most hosts
/// do not implement this: an [`ApprovalGate`](crate::ApprovalGate) over their
/// own [`Prompter`](crate::Prompter) keeps the rules every ask must keep, and
/// [`ApprovalGate::with_lines`](crate::ApprovalGate::with_lines) makes it a
/// `ModUi` that sends every other line where the host says.
/// [`ApprovalGate`](crate::ApprovalGate#examples)'s example is a whole host.
#[async_trait]
pub trait ModUi: Send + Sync {
    /// The human's Allow / Deny. Deny when `cancel` fires (an interrupt) or the
    /// human never answers -- a tool call must never hang on someone who walked away.
    async fn ask(&self, req: ApprovalRequest, cancel: &CancellationToken) -> Decision;
    /// A line the human sees: what a mod has to say (plan mode is on).
    fn info(&self, text: &str);
    /// A mod failed to load or in an event; the human must see it. Default:
    /// an [`info`](Self::info) line.
    fn error(&self, text: &str) {
        self.info(text)
    }
    /// A line for haest's diagnostic log, which the human need not see (each
    /// mod as it loads). Default: dropped.
    fn debug(&self, _message: &str) {}
}

/// Where haest runs, and the human: what every mod event can reach.
///
/// One per conversation, shared by every mod in [`Mods`](crate::Mods); the
/// events get it as their `haest` argument.
#[derive(Clone)]
pub struct ModEnv {
    /// The conversation's working directory.
    pub cwd: PathBuf,
    /// The workspace the agent's tools run in: [`ApprovalMod`](crate::ApprovalMod) asks only in `Real`.
    pub workspace: WorkspaceMode,
    /// The host's ask, and its info, error and debug lines.
    pub ui: Arc<dyn ModUi>,
}

impl ModEnv {
    /// The environment for one conversation's mods.
    pub fn new(cwd: impl Into<PathBuf>, workspace: WorkspaceMode, ui: Arc<dyn ModUi>) -> Self {
        Self {
            cwd: cwd.into(),
            workspace,
            ui,
        }
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
}

/// A NATIVE mod: plain Rust with the same events a mod file has. Every method
/// has a default that does nothing, so a mod implements only what it hooks.
/// Built-ins ([`ApprovalMod`](crate::ApprovalMod), [`PlanMod`](crate::PlanMod))
/// are mods like this; so is a host's own. The [crate docs](crate#a-native-mod)
/// have one.
///
/// A mod is put in a conversation with [`Mods::add`](crate::Mods::add); every
/// event runs across the mods in the order they were added. An `Err` from an
/// event (any error, or a line of text: `Err("why".into())`) is reported to
/// the human through [`ModUi::error`] with its causes, and never takes the
/// conversation down. What else a failure means depends on the event;
/// [`Mods`](crate::Mods) has the rules (a failed `tool.before` refuses the
/// call, a failed `tool.after` leaves the output as it was).
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
    ///
    /// `ctx.cancel()` fires when the human interrupts the call; an ask made here
    /// should pass it on. An `Err` refuses the call: a guard that broke must
    /// not wave anything through.
    async fn tool_before(
        &self,
        _call: &ModToolCall,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        Ok(Before::Pass)
    }

    /// `tool.after`: after the call ran; Some replaces the output the model
    /// reads. Every mod sees the output as the mods before it left it. An
    /// `Err` leaves the output as it was: the call happened, and the model
    /// must not retry it.
    async fn tool_after(
        &self,
        _call: &ModToolCall,
        _output: &ToolOutput,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Option<ToolOutput>, BoxError> {
        Ok(None)
    }

    /// `prompt.submit`: a human message before the model sees it; Some rewrites
    /// it. Runs when the host calls [`Mods::rewrite_prompt`](crate::Mods::rewrite_prompt).
    async fn prompt_submit(
        &self,
        _text: &str,
        _haest: &ModEnv,
    ) -> Result<Option<String>, BoxError> {
        Ok(None)
    }

    /// `system.prompt`: text appended to the system prompt, asked before every
    /// model call (through [`ModSystemPrompt`](crate::ModSystemPrompt)). None
    /// or empty adds nothing.
    async fn system_prompt(&self, _haest: &ModEnv) -> Result<Option<String>, BoxError> {
        Ok(None)
    }

    /// Tools this mod adds (asked again before every model call).
    /// [`ModTools`](crate::ModTools) runs them inside every mod's hooks, the
    /// same as the host's own.
    fn tools(&self) -> Tools {
        Tools::new()
    }

    /// The slash commands this mod answers, without the `/`.
    fn commands(&self) -> Vec<String> {
        Vec::new()
    }

    /// Run one of [`Mod::commands`]: Some text goes to the model; None = the mod
    /// handled the line itself. `name` comes without its `/`; `args` is the
    /// rest of the line.
    async fn command(
        &self,
        _name: &str,
        _args: &str,
        _haest: &ModEnv,
    ) -> Result<Option<String>, BoxError> {
        Ok(None)
    }
}
