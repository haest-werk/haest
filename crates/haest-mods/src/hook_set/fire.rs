//! Where each event fires its bindings: [`Hooks`] as a [`Mod`]. Payloads
//! are built in payload.rs and decisions take effect in decision.rs, the
//! same code a mod file's events go through.

use std::ops::ControlFlow;

use async_trait::async_trait;
use haest_core::{
    Agent, AssistantMessage, Tools, TurnOutcome,
    error::{BoxError, report},
    tool::{ToolContext, ToolOutput},
};
use tokio_util::sync::CancellationToken;

use super::Hooks;
use crate::{
    api::{Before, Mod, ModToolCall, Step, TurnEnd},
    decision::{ModelAfterDecision, ModelBeforeDecision, TurnEndDecision, system_text},
    env::ModEnv,
    hook::HookError,
    payload::{
        ModelAfterPayload, ModelBeforePayload, PromptSubmitPayload, SystemPromptPayload,
        ToolAfterPayload, ToolBeforePayload, TurnEndPayload,
    },
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
/// `Hooks` fires its bindings (events.rs writes the binding).
#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl Mod for Hooks {
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
        let mut call = call.clone();
        let mut rewritten = false;
        for (nth, binding) in (1..).zip(&self.slots.on_tool_before) {
            let payload = ToolBeforePayload::new(&call, ctx);
            if !binding.takes(&payload) {
                continue;
            }
            let Some(decision) = self.ask(binding, nth, payload, ctx.cancel(), haest).await? else {
                continue;
            };
            rewritten |= matches!(decision, Before::Input(_));
            if let ControlFlow::Break(reason) = decision.apply_to(&mut call) {
                return Ok(Before::Deny(reason));
            }
        }
        Ok(if rewritten {
            Before::Input(call.input)
        } else {
            Before::Pass
        })
    }

    async fn tool_after(
        &self,
        call: &ModToolCall,
        output: &ToolOutput,
        ctx: &ToolContext,
        haest: &ModEnv,
    ) -> Result<Option<ToolOutput>, BoxError> {
        let mut replaced = None;
        for (nth, binding) in (1..).zip(&self.slots.on_tool_after) {
            let payload = ToolAfterPayload::new(call, replaced.as_ref().unwrap_or(output), ctx);
            if !binding.takes(&payload) {
                continue;
            }
            if let Some(Some(new)) = self.ask(binding, nth, payload, ctx.cancel(), haest).await? {
                replaced = Some(new);
            }
        }
        Ok(replaced)
    }

    /// `prompt.submit` cannot be interrupted: its hooks get a token that
    /// fires only when their time is up.
    async fn prompt_submit(&self, text: &str, haest: &ModEnv) -> Result<Option<String>, BoxError> {
        let never = CancellationToken::new();
        let mut rewritten: Option<String> = None;
        for (nth, binding) in (1..).zip(&self.slots.on_prompt_submit) {
            let payload = PromptSubmitPayload::new(rewritten.as_deref().unwrap_or(text));
            if let Some(Some(new)) = self.ask(binding, nth, payload, &never, haest).await? {
                rewritten = Some(new);
            }
        }
        Ok(rewritten)
    }

    /// `system.prompt` cannot be interrupted, as `prompt.submit`.
    async fn system_prompt(&self, haest: &ModEnv) -> Result<Option<String>, BoxError> {
        let never = CancellationToken::new();
        let mut parts = Vec::new();
        for (nth, binding) in (1..).zip(&self.slots.on_system_prompt) {
            let payload = SystemPromptPayload::new();
            if let Some(Some(text)) = self.ask(binding, nth, payload, &never, haest).await? {
                parts.push(text);
            }
        }
        let added = system_text(parts);
        Ok((!added.is_empty()).then_some(added))
    }

    async fn model_before(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        let bindings = &self.slots.on_model_before;
        if bindings.is_empty() {
            return Ok(Step::Pass);
        }
        let payload = ModelBeforePayload::new(agent);
        let stops = |d: &ModelBeforeDecision| d.stop.is_some();
        match self.decide(bindings, &payload, stops, cancel, haest).await {
            Ok(decided) => ModelBeforeDecision::apply(decided, agent, cancel).await,
            Err(e) => stopped(e),
        }
    }

    async fn model_after(
        &self,
        agent: &mut Agent,
        answer: &AssistantMessage,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        let bindings = &self.slots.on_model_after;
        if bindings.is_empty() {
            return Ok(Step::Pass);
        }
        let payload = ModelAfterPayload::new(agent, answer);
        let stops = |d: &ModelAfterDecision| d.stop.is_some();
        match self.decide(bindings, &payload, stops, cancel, haest).await {
            Ok(decided) => Ok(ModelAfterDecision::apply(decided, agent)),
            Err(e) => stopped(e),
        }
    }

    async fn turn_end(
        &self,
        agent: &mut Agent,
        outcome: &TurnOutcome,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<TurnEnd, BoxError> {
        let bindings = &self.slots.on_turn_end;
        if bindings.is_empty() {
            return Ok(TurnEnd::Pass);
        }
        let payload = TurnEndPayload::new(agent, outcome);
        let goes_on = |d: &TurnEndDecision| d.r#continue.is_some();
        let decided = self
            .decide(bindings, &payload, goes_on, cancel, haest)
            .await?;
        TurnEndDecision::apply(decided, agent, cancel).await
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
