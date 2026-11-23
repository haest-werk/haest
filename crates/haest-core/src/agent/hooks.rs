//! Where the loop asks the host's [`AgentHooks`](crate::AgentHooks) about a
//! step: before its model call, once its answer landed, and before the turn
//! ends on its own. Each answer works the same way: what the hook spent
//! counts toward the turn, a turn cancelled by then is aborted, and an `Err`
//! ends the turn as an error.

use crate::{
    agent::{Agent, turn::Turn},
    error::{BoxError, report},
    events::TurnOutcome,
    messages::{AssistantMessage, StopReason, Usage},
};

impl Agent {
    /// [`before_model_call`](crate::AgentHooks::before_model_call): how the
    /// turn ends before its model call, or None to make it.
    pub(super) async fn ask_before_model_call(&mut self, t: &mut Turn<'_>) -> Option<TurnOutcome> {
        let cancel = t.cancel.clone();
        let hooks = self.deps.shared().hooks().clone();
        let ran = hooks.before_model_call(self, &cancel).await;
        let spent = self.take_side_spend();
        if let Err(end) = settle(ran, &spent, t) {
            return Some(end);
        }
        // The model call comes next: a summary the hook paid for may stop it.
        if spent != Usage::default()
            && let Some(over) = t.guard.stop(&t.usage, self.turn_caps().as_ref())
        {
            return Some(over.into());
        }
        None
    }

    /// [`after_model_call`](crate::AgentHooks::after_model_call) about
    /// `answer`, the last message in history: how the turn ends before the
    /// answer's tool calls run, or None to go on. A turn already cancelled
    /// does not ask. What the hook spent stops the turn, if at all, before its
    /// next model call, as the answer's own spend does.
    pub(super) async fn ask_after_model_call(
        &mut self,
        answer: &AssistantMessage,
        t: &mut Turn<'_>,
    ) -> Option<TurnOutcome> {
        if t.cancel.is_cancelled() {
            return Some(TurnOutcome::new(StopReason::Aborted));
        }
        let cancel = t.cancel.clone();
        let hooks = self.deps.shared().hooks().clone();
        let ran = hooks.after_model_call(self, answer, &cancel).await;
        let spent = self.take_side_spend();
        settle(ran, &spent, t).err()
    }

    /// An answer without tool calls would end the turn with `outcome`. Queued
    /// steering keeps it going; else
    /// [`before_turn_end`](crate::AgentHooks::before_turn_end) may, its text
    /// added as steering is. How the turn ends, or None to go on: the next
    /// step is under the turn's caps and guard as any other.
    pub(super) async fn at_turn_end(
        &mut self,
        outcome: TurnOutcome,
        t: &mut Turn<'_>,
    ) -> Option<TurnOutcome> {
        if self.inject_queued(t).await {
            return None;
        }
        let cancel = t.cancel.clone();
        let hooks = self.deps.shared().hooks().clone();
        let ran = hooks.before_turn_end(self, &outcome, &cancel).await;
        let spent = self.take_side_spend();
        match settle(ran, &spent, t) {
            Err(end) => Some(end),
            Ok(None) => Some(outcome),
            Ok(Some(text)) => {
                self.add_user(&text, t).await;
                None
            }
        }
    }
}

/// What a hook's answer means for the turn. `spent` (a summary it paid for)
/// counts toward the turn; a turn cancelled while it ran was aborted, whatever
/// it said; an `Err` ends the turn as an error, its causes said.
fn settle<T>(ran: Result<T, BoxError>, spent: &Usage, t: &mut Turn<'_>) -> Result<T, TurnOutcome> {
    t.usage += spent;
    if t.cancel.is_cancelled() {
        return Err(TurnOutcome::new(StopReason::Aborted));
    }
    ran.map_err(|why| TurnOutcome::error(StopReason::Error, report(&*why)))
}
