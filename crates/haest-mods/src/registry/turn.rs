//! The loop events across every mod, for
//! [`ModLoopHooks`](crate::ModLoopHooks): `model.before`, `model.after` and
//! `turn.end`. Load order is the order asked; the first stop, or the first
//! mod that keeps a turn going, wins. A mod that fails is reported and the
//! next one is asked; nothing is asked
//! once the turn is cancelled (it ends as aborted whatever a mod says).

use haest_core::{Agent, AssistantMessage, TurnOutcome, error::BoxError};
use tokio_util::sync::CancellationToken;

use super::Mods;
use crate::{
    api::{Mod, Step, TurnEnd},
    env::ModEnv,
    events::ModEvent,
};

/// A mod stopped the turn: the turn's error, naming the mod.
#[derive(Debug, thiserror::Error)]
#[error("the mod {name} stopped the turn: {reason}")]
struct Stopped {
    name: String,
    reason: String,
}

impl Mods {
    /// A mod's loop event failed: reported, unless the turn was cancelled
    /// meanwhile (an interrupt is not the mod's failure).
    fn failed(&self, m: &dyn Mod, event: ModEvent, e: &BoxError, cancel: &CancellationToken) {
        if !cancel.is_cancelled() {
            self.report(m, event.as_str(), e);
        }
    }

    /// What one `model.before` or `model.after` answer means: Err ends the
    /// turn. A [`Mods`] inside this one answers its own mod's stop as that
    /// [`Stopped`], which ends the turn as it is.
    fn step(
        &self,
        m: &dyn Mod,
        event: ModEvent,
        ran: Result<Step, BoxError>,
        cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        match ran {
            Ok(Step::Pass) => Ok(()),
            Ok(Step::Stop(reason)) => Err(Stopped {
                name: m.name().to_string(),
                reason,
            }
            .into()),
            Err(e) if e.is::<Stopped>() => Err(e),
            Err(e) => {
                self.failed(m, event, &e, cancel);
                Ok(())
            }
        }
    }

    /// `model.before` across every mod; Err = a mod stopped the turn.
    pub(crate) async fn model_before(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        self.model_before_for(agent, cancel, self.env()).await
    }

    /// [`model_before`](Self::model_before), in the event `came_with`
    /// brought ([`ModEnv::for_event`]).
    pub(super) async fn model_before_for(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
        came_with: &ModEnv,
    ) -> Result<(), BoxError> {
        let haest = self.env().for_event(came_with);
        for m in self.list() {
            if cancel.is_cancelled() {
                break;
            }
            let ran = m.model_before(agent, cancel, &haest).await;
            self.step(m.as_ref(), ModEvent::ModelBefore, ran, cancel)?;
        }
        Ok(())
    }

    /// `model.after` across every mod about `answer`; Err = a mod stopped the turn.
    pub(crate) async fn model_after(
        &self,
        agent: &mut Agent,
        answer: &AssistantMessage,
        cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        self.model_after_for(agent, answer, cancel, self.env())
            .await
    }

    /// [`model_after`](Self::model_after), in the event `came_with` brought
    /// ([`ModEnv::for_event`]).
    pub(super) async fn model_after_for(
        &self,
        agent: &mut Agent,
        answer: &AssistantMessage,
        cancel: &CancellationToken,
        came_with: &ModEnv,
    ) -> Result<(), BoxError> {
        let haest = self.env().for_event(came_with);
        for m in self.list() {
            if cancel.is_cancelled() {
                break;
            }
            let ran = m.model_after(agent, answer, cancel, &haest).await;
            self.step(m.as_ref(), ModEvent::ModelAfter, ran, cancel)?;
        }
        Ok(())
    }

    /// `turn.end` across every mod: the first text that keeps the turn
    /// going, or None to let it end. The turn (`cancel`, its own token) is
    /// noted as kept going, for the next `turn.end` payload in it.
    pub(crate) async fn turn_end(
        &self,
        agent: &mut Agent,
        outcome: &TurnOutcome,
        cancel: &CancellationToken,
    ) -> Option<String> {
        self.turn_end_for(agent, outcome, cancel, self.env()).await
    }

    /// [`turn_end`](Self::turn_end), in the event `came_with` brought
    /// ([`ModEnv::for_event`]).
    pub(super) async fn turn_end_for(
        &self,
        agent: &mut Agent,
        outcome: &TurnOutcome,
        cancel: &CancellationToken,
        came_with: &ModEnv,
    ) -> Option<String> {
        let haest = self.env().for_event(came_with);
        for m in self.list() {
            if cancel.is_cancelled() {
                break;
            }
            match m.turn_end(agent, outcome, cancel, &haest).await {
                Ok(TurnEnd::Pass) => {}
                Ok(TurnEnd::Continue(text)) => {
                    haest.kept_going(cancel);
                    return Some(text);
                }
                Err(e) => self.failed(m.as_ref(), ModEvent::TurnEnd, &e, cancel),
            }
        }
        None
    }
}
