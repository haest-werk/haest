//! The decorators a host composes to put mods in the loop -- no haest-core change:
//!   - [`ModTools`]: a [`ToolSource`] whose every tool (the host's and the mods'
//!     own) runs inside `tool.before` / `tool.after`, less those a mod hides;
//!   - [`ModSystemPrompt`]: a [`SystemPrompt`] with what `system.prompt` appends;
//!   - [`ModHooks`](crate::ModHooks) (hooks.rs): the host's `AgentHooks` with
//!     `model.before`, `model.after` and `turn.end` after them;
//!   - `prompt.submit` is [`Mods::rewrite_prompt`], called on the human's text
//!     before it goes to `Agent::run`; commands are [`Mods::command`].

use async_trait::async_trait;
use haest_core::{
    ToolRef, Tools,
    deps::{SystemPrompt, SystemPromptRef},
    tool::{JsonMap, ToolContext, ToolError, ToolOutput, ToolSource, ToolSourceRef, ToolWrapper},
};

use crate::{api::ModToolCall, registry::Mods};

/// The agent's tool source with the mods in it: every tool runs inside every
/// mod's `tool.before` / `tool.after`, and the tools mods add are offered
/// beside the host's.
///
/// Give it to the agent in place of the host's own [`ToolSource`]
/// (`AgentDeps::new(shared, ModTools::new(tools, mods.clone()), ...)`). The
/// list is built again each time the loop asks, so a mod that registers a
/// tool later is offered from the next model call.
///
/// A tool a mod hides ([`Mod::disabled_tools`](crate::Mod::disabled_tools))
/// is neither advertised nor callable, whoever added it; once no mod hides
/// it, it is back as it was, where it was.
///
/// A wrapped call goes: every mod's `tool.before` (gates last), then the tool
/// with the input they left, then every mod's `tool.after`. A refusal comes
/// back as [`ToolError::Input`] (`Refused: <reason>`), which the model reads;
/// a mod that failed in `tool.before` as [`ToolError::Failed`], and the tool
/// does not run.
#[derive(Debug, Clone)]
pub struct ModTools {
    inner: ToolSourceRef,
    mods: Mods,
}

impl ModTools {
    /// `inner`'s tools, and every mod's, inside `mods`' hooks.
    pub fn new(inner: impl Into<ToolSourceRef>, mods: Mods) -> Self {
        Self {
            inner: inner.into(),
            mods,
        }
    }

    /// `tools` plus every mod's tool, each wrapped in the `tool.before` /
    /// `tool.after` hooks: the list [`ModTools`] offers, for a host that
    /// builds a tool list rather than a source. A tool that runs OTHER tools
    /// (Code) should be handed these wrapped siblings, so every call it makes
    /// passes the hooks too.
    ///
    /// The mods' tools come after `tools`, in load order, and every tool a
    /// mod hides ([`Mods::disabled_tools`]) is left out. A wrapped tool keeps
    /// its name, description, schema, kind and `read_only`.
    pub fn wrap(mods: &Mods, tools: Tools) -> Tools {
        let hidden = mods.disabled_tools();
        tools
            .into_iter()
            .chain(mods.tools())
            .filter(|tool| !hidden.iter().any(|name| name == tool.name()))
            .map(|tool| {
                ToolRef::new(ModTool {
                    tool,
                    mods: mods.clone(),
                })
            })
            .collect()
    }
}

#[warn(clippy::missing_trait_methods)]
impl ToolSource for ModTools {
    fn tools(&self) -> Tools {
        Self::wrap(&self.mods, self.inner.tools())
    }
    /// A tool `inner` keeps back (a deferred one) runs by name, inside the hooks too.
    fn callable(&self) -> Tools {
        Self::wrap(&self.mods, self.inner.callable())
    }
}

/// One tool inside the hooks.
struct ModTool {
    tool: ToolRef,
    mods: Mods,
}

#[async_trait]
impl ToolWrapper for ModTool {
    fn inner(&self) -> &ToolRef {
        &self.tool
    }

    async fn execute(&self, input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let call = ModToolCall {
            name: self.tool.name().to_string(),
            input,
            read_only: self.tool.read_only(),
        };
        let call = self.mods.before(call, ctx).await?;
        let output = self.tool.execute(call.input.clone(), ctx).await?;
        Ok(self.mods.after(&call, output, ctx).await)
    }
}

/// The agent's system prompt with the mods' additions: the host's prompt, a
/// blank line, then what every mod's `system.prompt` hook returns
/// ([`Mods::system_append`]).
///
/// Asked before every model call, so a mod's addition can change between
/// calls (plan mode's brief comes and goes with `/plan`). Give it to the agent
/// in place of the host's own [`SystemPrompt`].
#[derive(Debug, Clone)]
pub struct ModSystemPrompt {
    inner: SystemPromptRef,
    mods: Mods,
}

impl ModSystemPrompt {
    /// `inner`, with what `mods` append.
    pub fn new(inner: impl Into<SystemPromptRef>, mods: Mods) -> Self {
        Self {
            inner: inner.into(),
            mods,
        }
    }
}

#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl SystemPrompt for ModSystemPrompt {
    async fn system_prompt(&self) -> String {
        let base = self.inner.system_prompt().await;
        let added = self.mods.system_append().await;
        match (base.is_empty(), added.is_empty()) {
            (_, true) => base,
            (true, false) => added,
            (false, false) => format!("{base}\n\n{added}"),
        }
    }
}
