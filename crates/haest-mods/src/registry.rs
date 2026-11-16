//! Every loaded mod, in load order, and running one event across all of them.
//! Load order is event order: the first mod's `tool.before` decides first,
//! except a gate's ([`Mod::is_gate`], the approval mod), which judges after
//! every other mod has had its say, on the call as it will run.

use std::sync::Arc;

use haest_core::tool::{Tool, ToolContext, ToolError, ToolOutput};

use crate::api::{Before, Mod, ModEnv, ModError, ModToolCall};

/// What the model reads for a call the human interrupted while a mod was deciding.
const INTERRUPTED: &str = "Interrupted by the user before this tool ran.";

/// What running a slash command found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// No mod has this command.
    Unknown,
    /// The mod handled the line itself; nothing goes to the model.
    Handled,
    /// The text to send to the model.
    Prompt(String),
}

/// Every mod one conversation loaded, and the environment they all see.
pub struct Mods {
    env: ModEnv,
    list: Vec<Arc<dyn Mod>>,
}

impl Mods {
    pub fn new(env: ModEnv) -> Self {
        Self {
            env,
            list: Vec::new(),
        }
    }

    /// Add a mod after every mod already here.
    pub fn add(&mut self, m: Arc<dyn Mod>) {
        self.env.host.log(&format!("mod loaded: {}", m.name()));
        self.list.push(m);
    }

    pub fn env(&self) -> &ModEnv {
        &self.env
    }

    /// Every mod's name, in load order.
    pub fn names(&self) -> Vec<String> {
        self.list.iter().map(|m| m.name().to_string()).collect()
    }

    /// Every tool the mods add, unwrapped ([`crate::ModTools`] wraps them).
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.list.iter().flat_map(|m| m.tools()).collect()
    }

    fn report(&self, m: &dyn Mod, event: &str, e: &ModError) {
        self.env
            .host
            .error(&format!("Mod {} failed in {event}: {e}", m.name()));
    }

    /// `tool.before` across every mod: the call to run, or why it must not run.
    /// A mod that fails refuses the call -- a guard that broke must not wave
    /// anything through.
    pub(crate) async fn before(
        &self,
        mut call: ModToolCall,
        ctx: &ToolContext,
    ) -> Result<ModToolCall, ToolError> {
        let (gates, rest): (Vec<_>, Vec<_>) = self.list.iter().partition(|m| m.is_gate());
        for m in rest.into_iter().chain(gates) {
            match m.tool_before(&call, ctx, &self.env).await {
                Ok(Before::Pass) => {}
                Ok(Before::Input(input)) => call.input = input,
                Ok(Before::Deny(reason)) => {
                    return Err(ToolError::Input(format!("Refused: {reason}")));
                }
                Err(_) if ctx.cancel.is_cancelled() => {
                    return Err(ToolError::Input(INTERRUPTED.into()));
                }
                Err(e) => {
                    self.report(m.as_ref(), "tool.before", &e);
                    return Err(ToolError::Failed(format!(
                        "the mod {} failed before this call, so it did not run: {e}",
                        m.name()
                    )));
                }
            }
        }
        Ok(call)
    }

    /// `tool.after` across every mod. The tool DID run: a mod that fails leaves
    /// the output as it was, so the model never retries a call that happened.
    pub(crate) async fn after(
        &self,
        call: &ModToolCall,
        mut output: ToolOutput,
        ctx: &ToolContext,
    ) -> ToolOutput {
        for m in &self.list {
            match m.tool_after(call, &output, ctx, &self.env).await {
                Ok(Some(replaced)) => output = replaced,
                Ok(None) => {}
                Err(_) if ctx.cancel.is_cancelled() => {}
                Err(e) => self.report(m.as_ref(), "tool.after", &e),
            }
        }
        output
    }

    /// A human message after every `prompt.submit` hook. A mod that fails leaves
    /// the text as it was.
    pub async fn rewrite_prompt(&self, text: &str) -> String {
        let mut text = text.to_string();
        for m in &self.list {
            match m.prompt_submit(&text, &self.env).await {
                Ok(Some(rewritten)) => text = rewritten,
                Ok(None) => {}
                Err(e) => self.report(m.as_ref(), "prompt.submit", &e),
            }
        }
        text
    }

    /// What every `system.prompt` hook appends, joined; empty for none.
    pub async fn system_append(&self) -> String {
        let mut parts = Vec::new();
        for m in &self.list {
            match m.system_prompt(&self.env).await {
                Ok(Some(text)) if !text.is_empty() => parts.push(text),
                Ok(_) => {}
                Err(e) => self.report(m.as_ref(), "system.prompt", &e),
            }
        }
        parts.join("\n\n")
    }

    /// Every slash command the mods answer, without the `/`, once each.
    pub fn commands(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for name in self.list.iter().flat_map(|m| m.commands()) {
            let name = bare(&name).to_string();
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    /// The mod that answers `name`: a later mod's command replaces an earlier one's.
    fn command_owner(&self, name: &str) -> Option<&Arc<dyn Mod>> {
        let name = bare(name);
        self.list
            .iter()
            .rev()
            .find(|m| m.commands().iter().any(|c| bare(c) == name))
    }

    pub fn has_command(&self, name: &str) -> bool {
        self.command_owner(name).is_some()
    }

    /// Run a mod's slash command (`name` with or without its `/`).
    pub async fn command(&self, name: &str, args: &str) -> CommandOutcome {
        let Some(m) = self.command_owner(name) else {
            return CommandOutcome::Unknown;
        };
        match m.command(bare(name), args, &self.env).await {
            Ok(Some(text)) => CommandOutcome::Prompt(text),
            Ok(None) => CommandOutcome::Handled,
            Err(e) => {
                self.report(m.as_ref(), &format!("/{}", bare(name)), &e);
                CommandOutcome::Handled
            }
        }
    }
}

fn bare(name: &str) -> &str {
    name.strip_prefix('/').unwrap_or(name)
}

#[cfg(test)]
mod tests;
