//! [`ModLoopHooks`]: the host's [`AgentHooks`] with the mods' loop events after
//! them.

use async_trait::async_trait;
use haest_core::{
    Agent, AgentHooks, AgentHooksRef, AssistantMessage, Effort, TurnOutcome,
    compaction::CompactionResult,
    error::BoxError,
    messages::{MediaPart, Message},
};
use tokio_util::sync::CancellationToken;

use crate::registry::Mods;

/// The host's own [`AgentHooks`] with the mods' loop events in them:
/// `model.before`, `model.after` and `turn.end` ([`Mod::model_before`],
/// [`Mod::model_after`], [`Mod::turn_end`]).
///
/// Give it to the agent in place of the host's hooks
/// (`AgentDeps::new(..).with_hooks(ModLoopHooks::new(host, mods.clone()))`),
/// the way [`ModTools`](crate::ModTools) goes in place of its tools. At each
/// loop point the host's hook runs first, then the mods in load order:
///
/// - before a model call, the host's
///   [`before_model_call`](AgentHooks::before_model_call) (its compaction
///   included), then every `model.before`;
/// - once an answer lands, the host's
///   [`after_model_call`](AgentHooks::after_model_call), then every
///   `model.after`;
/// - before a turn ends on its own, the host's
///   [`before_turn_end`](AgentHooks::before_turn_end); when it keeps the turn
///   going the mods are not asked, else every `turn.end`.
///
/// An `Err` from the host's hook ends the turn there, as it would without
/// mods. A mod that stops the turn ([`Step::Stop`](crate::Step::Stop)) ends
/// it as an error naming the mod; one that fails is reported and changes
/// nothing ([`Mods`] has the rules). Every other hook is the host's alone.
///
/// Only the agent these hooks are given runs the mods' loop events: a
/// subagent built over the same shared parts runs its own hooks.
///
/// # Examples
///
/// A mod that keeps a turn going until the answer says it is done:
///
/// ```
/// use async_trait::async_trait;
/// use haest_core::{
///     Agent, AgentDeps, AgentOptions, LoopEvent, Message, NoHooks, SharedDeps, ShutWorkspace,
///     StopReason, Tools, TurnOutcome,
///     error::BoxError,
///     testing::{ScriptedProvider, text_answer},
///     workspace::WorkspaceMode,
/// };
/// use haest_mods::{Mod, ModEnv, ModLoopHooks, Mods, TurnEnd};
/// use tokio_util::sync::CancellationToken;
/// # use haest_mods::{ApprovalDecision, ApprovalRequest, ModUi};
/// # struct Terminal;
/// # #[async_trait]
/// # impl ModUi for Terminal {
/// #     async fn ask(&self, _: ApprovalRequest, _: &CancellationToken) -> ApprovalDecision { ApprovalDecision::Deny }
/// #     fn info(&self, _: &str) {}
/// # }
///
/// const GO_ON: &str = "Not done yet: go on, and say DONE when it is.";
///
/// /// Holds every turn until the answer says DONE.
/// struct UntilDone;
///
/// #[async_trait]
/// impl Mod for UntilDone {
///     fn name(&self) -> &str {
///         "until-done"
///     }
///
///     async fn turn_end(
///         &self,
///         agent: &mut Agent,
///         _outcome: &TurnOutcome,
///         _cancel: &CancellationToken,
///         _haest: &ModEnv,
///     ) -> Result<TurnEnd, BoxError> {
///         let done = matches!(
///             agent.history().last(),
///             Some(Message::Assistant(answer)) if answer.text().contains("DONE")
///         );
///         Ok(if done { TurnEnd::Pass } else { TurnEnd::Continue(GO_ON.into()) })
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let mods = Mods::new(ModEnv::new("/project", WorkspaceMode::Sandbox, Terminal), [UntilDone]);
/// let model = ScriptedProvider::new([text_answer("Half way."), text_answer("All DONE.")]);
/// let shared = SharedDeps::new(model.resolver(), "/project", ShutWorkspace);
/// // The host's own hooks (none here), with the mods after them.
/// let deps = AgentDeps::new(shared, Tools::new(), "").with_hooks(ModLoopHooks::new(NoHooks, mods));
/// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
///
/// let end = agent.run("tidy the notes", &mut Vec::<LoopEvent>::new()).await;
///
/// // One turn, two model calls: the mod's text went in between.
/// assert_eq!(end.stop_reason, StopReason::EndTurn);
/// assert_eq!(model.requests().len(), 2);
/// assert!(matches!(&agent.history()[2], Message::User(u) if u.content == GO_ON));
/// # }
/// ```
///
/// [`Mod::model_before`]: crate::Mod::model_before
/// [`Mod::model_after`]: crate::Mod::model_after
/// [`Mod::turn_end`]: crate::Mod::turn_end
#[derive(Debug, Clone)]
pub struct ModLoopHooks {
    inner: AgentHooksRef,
    mods: Mods,
}

impl ModLoopHooks {
    /// `inner`, the host's own hooks, with `mods`' loop events after them.
    pub fn new(inner: impl Into<AgentHooksRef>, mods: Mods) -> Self {
        Self {
            inner: inner.into(),
            mods,
        }
    }
}

#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl AgentHooks for ModLoopHooks {
    fn on_message(&self, msg: &Message) {
        self.inner.on_message(msg);
    }

    /// A mod's compaction too: the host records it as its own.
    fn on_compaction(&self, result: &CompactionResult) {
        self.inner.on_compaction(result);
    }

    async fn before_model_call(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        self.inner.before_model_call(agent, cancel).await?;
        self.mods.model_before(agent, cancel).await
    }

    async fn after_model_call(
        &self,
        agent: &mut Agent,
        answer: &AssistantMessage,
        cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        self.inner.after_model_call(agent, answer, cancel).await?;
        self.mods.model_after(agent, answer, cancel).await
    }

    /// The host keeps the turn going first; the mods are asked only when it
    /// lets the turn end.
    async fn before_turn_end(
        &self,
        agent: &mut Agent,
        outcome: &TurnOutcome,
        cancel: &CancellationToken,
    ) -> Result<Option<String>, BoxError> {
        if let Some(text) = self.inner.before_turn_end(agent, outcome, cancel).await? {
            return Ok(Some(text));
        }
        Ok(self.mods.turn_end(agent, outcome, cancel).await)
    }

    async fn attach_media(
        &self,
        text: &str,
        cancel: &CancellationToken,
    ) -> (Vec<MediaPart>, Vec<String>) {
        self.inner.attach_media(text, cancel).await
    }

    fn on_media_withheld(&self, parts: &[MediaPart], model: &str) {
        self.inner.on_media_withheld(parts, model);
    }

    fn on_effort_fitted(&self, model: &str, used: Effort) {
        self.inner.on_effort_fitted(model, used);
    }

    async fn before_effects(&self) {
        self.inner.before_effects().await;
    }

    fn on_warning(&self, text: &str) {
        self.inner.on_warning(text);
    }
}

#[cfg(test)]
mod tests;
