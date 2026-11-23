//! One event's handlers, asked one at a time in order, and their answers
//! folded by the rules every mod made of handlers follows. [`Hooks`]'
//! bindings and a mod file's `haest.on` handlers (haest-bridges' `JsMod`)
//! both fire through here, so an answer means the same from either:
//!
//! | Event | Several handlers |
//! |---|---|
//! | `tool.before` | each sees the call as the ones before it left it (input rewrites chain); the first deny refuses it and the ones after it are not asked |
//! | `tool.after` | each sees the output the one before it left |
//! | `prompt.submit` | each sees the text the one before it left |
//! | `system.prompt` | every text, in order, a blank line between |
//! | `model.before`, `model.after`, `turn.end` | each sees the same payload, up to the first stop (`turn.end`: continue); the ones after it are not asked. Every steer is kept, the first compaction counts |
//!
//! A handler's answer is its event's decision, read by the one decoder
//! (decision/json.rs) wherever it came from; what the decisions do is
//! decision.rs.
//!
//! [`Hooks`]: crate::Hooks

use std::ops::ControlFlow;

use async_trait::async_trait;
use haest_core::{
    Agent, AssistantMessage, TurnOutcome,
    error::BoxError,
    tool::{ToolContext, ToolOutput},
};
use tokio_util::sync::CancellationToken;

use crate::{
    api::{Before, ModToolCall, Step, TurnEnd},
    decision::{ModelAfterDecision, ModelBeforeDecision, TurnEndDecision, system_text},
    env::ModEnv,
    events::Event,
    payload::{
        ModelAfterPayload, ModelBeforePayload, PromptSubmitPayload, SystemPromptPayload,
        ToolAfterPayload, ToolBeforePayload, TurnEndPayload,
    },
};

/// One event's handlers, in order, as the folds here ask them: one at a
/// time, each on the payload the ones before it left.
#[async_trait]
pub trait Handlers<E: Event>: Sync {
    /// Why a handler gave no answer, ending the event there.
    type Error: Send;

    /// How many handlers there are.
    fn count(&self) -> usize;

    /// The `nth` handler (from 1) on `payload`: its decision, `None` to
    /// pass it over (it is not for this call, or it failed and the next one
    /// is asked), or the error that ends the event.
    async fn ask(&self, nth: usize, payload: E) -> Result<Option<E::Decision>, Self::Error>;
}

/// `tool.before`: [`Before::Input`] with the input the handlers left when
/// one rewrote it, the first deny, or a pass.
///
/// # Errors
///
/// A handler's error: the handlers after it are not asked.
pub async fn tool_before<H: Handlers<ToolBeforePayload>>(
    handlers: &H,
    call: &ModToolCall,
    ctx: &ToolContext,
) -> Result<Before, H::Error> {
    let mut call = call.clone();
    let mut rewritten = false;
    for nth in 1..=handlers.count() {
        let payload = ToolBeforePayload::new(&call, ctx);
        let Some(decision) = handlers.ask(nth, payload).await? else {
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

/// `tool.after`: the output the last handler that replaced it left, or
/// `None` to keep the call's.
///
/// # Errors
///
/// A handler's error: the handlers after it are not asked.
pub async fn tool_after<H: Handlers<ToolAfterPayload>>(
    handlers: &H,
    call: &ModToolCall,
    output: &ToolOutput,
    ctx: &ToolContext,
) -> Result<Option<ToolOutput>, H::Error> {
    let mut replaced = None;
    for nth in 1..=handlers.count() {
        let payload = ToolAfterPayload::new(call, replaced.as_ref().unwrap_or(output), ctx);
        if let Some(Some(new)) = handlers.ask(nth, payload).await? {
            replaced = Some(new);
        }
    }
    Ok(replaced)
}

/// `prompt.submit`: the text the last handler that rewrote it left, or
/// `None` to keep `text`.
///
/// # Errors
///
/// A handler's error: the handlers after it are not asked.
pub async fn prompt_submit<H: Handlers<PromptSubmitPayload>>(
    handlers: &H,
    text: &str,
) -> Result<Option<String>, H::Error> {
    let mut rewritten: Option<String> = None;
    for nth in 1..=handlers.count() {
        let payload = PromptSubmitPayload::new(rewritten.as_deref().unwrap_or(text));
        if let Some(Some(new)) = handlers.ask(nth, payload).await? {
            rewritten = Some(new);
        }
    }
    Ok(rewritten)
}

/// `system.prompt`: what the handlers append, or `None` when none adds anything.
///
/// # Errors
///
/// A handler's error: the handlers after it are not asked.
pub async fn system_prompt<H: Handlers<SystemPromptPayload>>(
    handlers: &H,
) -> Result<Option<String>, H::Error> {
    let mut parts = Vec::new();
    for nth in 1..=handlers.count() {
        if let Some(Some(text)) = handlers.ask(nth, SystemPromptPayload::new()).await? {
            parts.push(text);
        }
    }
    let added = system_text(parts);
    Ok((!added.is_empty()).then_some(added))
}

/// `model.before`: what the handlers' decisions do to `agent`
/// ([`ModelBeforeDecision`]'s rules).
///
/// # Errors
///
/// The outer `Err` is a handler's error, nothing applied: the caller says
/// what it does to the turn. The inner one is a compaction that failed.
pub async fn model_before<H: Handlers<ModelBeforePayload>>(
    handlers: &H,
    agent: &mut Agent,
    cancel: &CancellationToken,
) -> Result<Result<Step, BoxError>, H::Error> {
    if handlers.count() == 0 {
        return Ok(Ok(Step::Pass));
    }
    let payload = ModelBeforePayload::new(agent);
    let decided = decide(handlers, &payload, |d| d.stop.is_some()).await?;
    Ok(ModelBeforeDecision::apply(decided, agent, cancel).await)
}

/// `model.after`: what the handlers' decisions do to `agent`
/// ([`ModelAfterDecision`]'s rules).
///
/// # Errors
///
/// A handler's error, nothing applied: the caller says what it does to the turn.
pub async fn model_after<H: Handlers<ModelAfterPayload>>(
    handlers: &H,
    agent: &Agent,
    answer: &AssistantMessage,
) -> Result<Step, H::Error> {
    if handlers.count() == 0 {
        return Ok(Step::Pass);
    }
    let payload = ModelAfterPayload::new(agent, answer);
    let decided = decide(handlers, &payload, |d| d.stop.is_some()).await?;
    Ok(ModelAfterDecision::apply(decided, agent))
}

/// `turn.end`: what the handlers' decisions do to `agent`
/// ([`TurnEndDecision`]'s rules).
///
/// # Errors
///
/// The outer `Err` is a handler's error, nothing applied. The inner one is
/// a compaction that failed.
pub async fn turn_end<H: Handlers<TurnEndPayload>>(
    handlers: &H,
    agent: &mut Agent,
    outcome: &TurnOutcome,
    cancel: &CancellationToken,
    haest: &ModEnv,
) -> Result<Result<TurnEnd, BoxError>, H::Error> {
    if handlers.count() == 0 {
        return Ok(Ok(TurnEnd::Pass));
    }
    let payload = TurnEndPayload::new(agent, outcome, cancel, haest);
    let decided = decide(handlers, &payload, |d| d.r#continue.is_some()).await?;
    Ok(TurnEndDecision::apply(decided, agent, cancel).await)
}

/// A loop event's decisions on `payload`, in order, up to the first that
/// `ends` the run: the handlers after it are not asked.
async fn decide<E: Event, H: Handlers<E>>(
    handlers: &H,
    payload: &E,
    ends: impl Fn(&E::Decision) -> bool,
) -> Result<Vec<E::Decision>, H::Error> {
    let mut decided = Vec::new();
    for nth in 1..=handlers.count() {
        let Some(decision) = handlers.ask(nth, payload.clone()).await? else {
            continue;
        };
        let ended = ends(&decision);
        decided.push(decision);
        if ended {
            break;
        }
    }
    Ok(decided)
}
