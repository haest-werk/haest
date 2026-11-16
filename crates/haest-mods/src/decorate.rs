//! The decorators a host composes to put mods in the loop -- no haest-core change:
//!   - [`ModTools`]: a [`ToolSource`] whose every tool (the host's and the mods'
//!     own) runs inside `tool.before` / `tool.after`;
//!   - [`ModSystemPrompt`]: a [`SystemPrompt`] with what `system.prompt` appends;
//!   - `prompt.submit` is [`Mods::rewrite_prompt`], called on the human's text
//!     before it goes to `Agent::run`; commands are [`Mods::command`].

use std::sync::Arc;

use async_trait::async_trait;
use haest_core::{
    deps::SystemPrompt,
    tool::{JsonMap, OnResume, Tool, ToolContext, ToolError, ToolOutput, ToolSource},
};
use serde_json::Value;

use crate::{api::ModToolCall, registry::Mods};

/// The agent's tool source with the mods in it: every tool runs inside every
/// mod's `tool.before` / `tool.after`, and the tools mods add are offered
/// beside the host's.
///
/// Give it to the agent in place of the host's own [`ToolSource`]
/// (`AgentDeps::new(models, Arc::new(ModTools::new(tools, mods)), ...)`). The
/// list is built again each time the loop asks, so a mod that registers a
/// tool later is offered from the next model call.
///
/// A wrapped call goes: every mod's `tool.before` (gates last), then the tool
/// with the input they left, then every mod's `tool.after`. A refusal comes
/// back as [`ToolError::Input`] (`Refused: <reason>`), which the model reads;
/// a mod that failed in `tool.before` as [`ToolError::Failed`], and the tool
/// does not run.
pub struct ModTools {
    inner: Arc<dyn ToolSource>,
    mods: Arc<Mods>,
}

impl ModTools {
    /// `inner`'s tools, and every mod's, inside `mods`' hooks.
    pub fn new(inner: Arc<dyn ToolSource>, mods: Arc<Mods>) -> Self {
        Self { inner, mods }
    }

    /// `tools` plus every mod's tool, each wrapped in the `tool.before` /
    /// `tool.after` hooks: the list [`ModTools`] offers, for a host that
    /// builds a tool list rather than a source. A tool that runs OTHER tools
    /// (Code) should be handed these wrapped siblings, so every call it makes
    /// passes the hooks too.
    ///
    /// The mods' tools come after `tools`, in load order. A wrapped tool keeps
    /// its name, description, schema, kind and `read_only`.
    pub fn wrap(mods: &Arc<Mods>, tools: Vec<Arc<dyn Tool>>) -> Vec<Arc<dyn Tool>> {
        tools
            .into_iter()
            .chain(mods.tools())
            .map(|tool| {
                Arc::new(ModTool {
                    tool,
                    mods: mods.clone(),
                }) as Arc<dyn Tool>
            })
            .collect()
    }
}

impl ToolSource for ModTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Self::wrap(&self.mods, self.inner.tools())
    }
}

/// One tool inside the hooks.
struct ModTool {
    tool: Arc<dyn Tool>,
    mods: Arc<Mods>,
}

#[async_trait]
impl Tool for ModTool {
    fn name(&self) -> &str {
        self.tool.name()
    }
    fn description(&self) -> String {
        self.tool.description()
    }
    fn parameters(&self) -> Value {
        self.tool.parameters()
    }
    fn kind(&self) -> &str {
        self.tool.kind()
    }
    fn read_only(&self) -> bool {
        self.tool.read_only()
    }
    fn on_resume(&self) -> OnResume {
        self.tool.on_resume()
    }
    fn canonical_input(&self, input: &JsonMap) -> JsonMap {
        self.tool.canonical_input(input)
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
pub struct ModSystemPrompt {
    inner: Arc<dyn SystemPrompt>,
    mods: Arc<Mods>,
}

impl ModSystemPrompt {
    /// `inner`, with what `mods` append.
    pub fn new(inner: Arc<dyn SystemPrompt>, mods: Arc<Mods>) -> Self {
        Self { inner, mods }
    }
}

#[async_trait]
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
