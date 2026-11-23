//! Where each event fires its bindings: [`HookSet`] as a [`Mod`]. The
//! bindings are asked and their answers folded in fold.rs, the same code a
//! mod file's handlers go through; what a failure does here is the
//! binding's [`OnError`](crate::OnError).

use async_trait::async_trait;
use haest_core::{
    Agent, AssistantMessage, Tools, TurnOutcome,
    error::{BoxError, report},
    tool::{ToolContext, ToolOutput},
};
use tokio_util::sync::CancellationToken;

use super::HookSet;
use crate::{
    api::{Before, Mod, ModToolCall, Step, TurnEnd},
    env::ModEnv,
    fold,
    hook::HookError,
};

/// A blocking hook's failure in `model.before` or `model.after`: the turn
/// stops, its error the reason. An interrupt is passed on as one.
fn stopped(e: HookError) -> Result<Step, BoxError> {
    match e {
        HookError::Interrupted { .. } => Err(e.into()),
        e => Ok(Step::Stop(report(&e))),
    }
}

/// Every method is named: an event `Mod` gains fails the gates here until
/// `HookSet` fires its bindings (events.rs writes the binding).
#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl Mod for HookSet {
    fn name(&self) -> &str {
        &self.name
    }

    /// Hooks judge a call in load order, like any mod.
    fn is_gate(&self) -> bool {
        false
    }

    async fn tool_before(
        &self,
        call: &ModToolCall,
        ctx: &ToolContext,
        haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        let bound = self.bound(&self.slots.on_tool_before, ctx.cancel(), haest);
        Ok(fold::tool_before(&bound, call, ctx, haest).await?)
    }

    async fn tool_after(
        &self,
        call: &ModToolCall,
        output: &ToolOutput,
        ctx: &ToolContext,
        haest: &ModEnv,
    ) -> Result<Option<ToolOutput>, BoxError> {
        let bound = self.bound(&self.slots.on_tool_after, ctx.cancel(), haest);
        Ok(fold::tool_after(&bound, call, output, ctx, haest).await?)
    }

    /// `prompt.submit` cannot be interrupted: its hooks get a token that
    /// fires only when their time is up.
    async fn prompt_submit(&self, text: &str, haest: &ModEnv) -> Result<Option<String>, BoxError> {
        let never = CancellationToken::new();
        let bound = self.bound(&self.slots.on_prompt_submit, &never, haest);
        Ok(fold::prompt_submit(&bound, text, haest).await?)
    }

    /// `system.prompt` cannot be interrupted, as `prompt.submit`.
    async fn system_prompt(&self, haest: &ModEnv) -> Result<Option<String>, BoxError> {
        let never = CancellationToken::new();
        let bound = self.bound(&self.slots.on_system_prompt, &never, haest);
        Ok(fold::system_prompt(&bound, haest).await?)
    }

    async fn model_before(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        let bound = self.bound(&self.slots.on_model_before, cancel, haest);
        let folded = fold::model_before(&bound, agent, cancel, haest).await;
        folded.unwrap_or_else(stopped)
    }

    async fn model_after(
        &self,
        agent: &mut Agent,
        answer: &AssistantMessage,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        let bound = self.bound(&self.slots.on_model_after, cancel, haest);
        fold::model_after(&bound, agent, answer, haest)
            .await
            .or_else(stopped)
    }

    async fn turn_end(
        &self,
        agent: &mut Agent,
        outcome: &TurnOutcome,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<TurnEnd, BoxError> {
        let bound = self.bound(&self.slots.on_turn_end, cancel, haest);
        fold::turn_end(&bound, agent, outcome, cancel, haest).await?
    }

    /// Hooks add no tools.
    fn tools(&self) -> Tools {
        Tools::new()
    }

    /// Hooks hide no tools.
    fn disabled_tools(&self) -> Vec<String> {
        Vec::new()
    }

    /// Hooks answer no slash command.
    fn commands(&self) -> Vec<String> {
        Vec::new()
    }

    /// Hooks answer no slash command.
    async fn command(
        &self,
        _name: &str,
        _args: &str,
        _haest: &ModEnv,
    ) -> Result<Option<String>, BoxError> {
        Ok(None)
    }
}
