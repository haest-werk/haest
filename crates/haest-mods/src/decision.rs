//! What a hook answers: the decisions of the loop events, and the one place
//! a decision takes effect. A file mod and [`Hooks`](crate::Hooks) both
//! apply their answers here; how each reads and writes as JSON is json.rs.

use std::ops::ControlFlow;

use haest_core::{
    Agent,
    compaction::{CompactionOptions, compact},
    error::BoxError,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::api::{ModToolCall, Step, TurnEnd};

mod json;
#[cfg(test)]
mod tests;

/// A request to compact the conversation: haest's own compaction
/// ([`compact`](haest_core::compaction::compact)), with what the summary
/// should keep. JSON: `true`, or `{ instructions }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Compact {
    /// What the summary should keep or leave out, after haest's own
    /// instruction; `None` asks for that alone.
    pub instructions: Option<String>,
}

impl Compact {
    /// Compact, with haest's own summary instruction alone.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the summary to keep or leave out what `instructions` say.
    #[must_use]
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }
}

/// What a `model.before` hook decided. JSON: `{ compact, steer, stop }`, each
/// optional; the [`Default`] passes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ModelBeforeDecision {
    /// Compact the conversation before the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact: Option<Compact>,
    /// Steer the turn with this text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steer: Option<String>,
    /// End the turn before the call, for this reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
}

impl ModelBeforeDecision {
    /// Compact the conversation first.
    #[must_use]
    pub fn with_compact(mut self, compact: Compact) -> Self {
        self.compact = Some(compact);
        self
    }

    /// Steer the turn with `text`.
    #[must_use]
    pub fn with_steer(mut self, text: impl Into<String>) -> Self {
        self.steer = Some(text.into());
        self
    }

    /// End the turn, for `reason`.
    #[must_use]
    pub fn with_stop(mut self, reason: impl Into<String>) -> Self {
        self.stop = Some(reason.into());
        self
    }

    /// What one `model.before` run's decisions, in the order they were
    /// made, do to `agent`, up to the first that stops: the first
    /// compaction asked for runs (its summary counts toward the turn,
    /// `cancel` stops it), then every steer goes in, in order, then that
    /// stop ends the turn ([`Step::Stop`]). Without one it is [`Step::Pass`].
    ///
    /// # Errors
    ///
    /// The compaction failed; nothing else was applied.
    pub async fn apply(
        decided: Vec<Self>,
        agent: &mut Agent,
        cancel: &CancellationToken,
    ) -> Result<Step, BoxError> {
        let decided = up_to_first(decided, |d| d.stop.is_some());
        if let Some(asked) = decided.iter().find_map(|d| d.compact.clone()) {
            compact_now(agent, cancel, asked).await?;
        }
        Ok(steer_then_stop(
            agent,
            decided.into_iter().map(|d| (d.steer, d.stop)),
        ))
    }
}

/// What a `model.after` hook decided. JSON: `{ steer, stop }`, each
/// optional; the [`Default`] passes. There is no compaction here: the
/// answer's tool results are still to land.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ModelAfterDecision {
    /// Steer the turn with this text, after the answer's tool results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steer: Option<String>,
    /// End the turn for this reason; the answer's calls do not run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
}

impl ModelAfterDecision {
    /// Steer the turn with `text`.
    #[must_use]
    pub fn with_steer(mut self, text: impl Into<String>) -> Self {
        self.steer = Some(text.into());
        self
    }

    /// End the turn, for `reason`.
    #[must_use]
    pub fn with_stop(mut self, reason: impl Into<String>) -> Self {
        self.stop = Some(reason.into());
        self
    }

    /// What one `model.after` run's decisions, in the order they were
    /// made, do to `agent`, up to the first that stops: every steer goes
    /// in, in order, then that stop ends the turn ([`Step::Stop`]).
    pub fn apply(decided: Vec<Self>, agent: &Agent) -> Step {
        let decided = up_to_first(decided, |d| d.stop.is_some());
        steer_then_stop(agent, decided.into_iter().map(|d| (d.steer, d.stop)))
    }
}

/// What a `turn.end` hook decided. JSON: `{ compact, continue }`, each
/// optional; the [`Default`] lets the turn end.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TurnEndDecision {
    /// Compact the conversation as the turn ends (or before it goes on).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact: Option<Compact>,
    /// Keep the turn going: this text goes to the model as a user message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#continue: Option<String>,
}

impl TurnEndDecision {
    /// Compact the conversation as the turn ends.
    #[must_use]
    pub fn with_compact(mut self, compact: Compact) -> Self {
        self.compact = Some(compact);
        self
    }

    /// Keep the turn going with `text` as the next user message.
    #[must_use]
    pub fn with_continue(mut self, text: impl Into<String>) -> Self {
        self.r#continue = Some(text.into());
        self
    }

    /// What one `turn.end` run's decisions, in the order they were made,
    /// do to `agent`, up to the first that keeps the turn going: the first
    /// compaction asked for runs, then that text keeps it going
    /// ([`TurnEnd::Continue`]). Without one it is [`TurnEnd::Pass`].
    ///
    /// # Errors
    ///
    /// The compaction failed; the turn ends.
    pub async fn apply(
        decided: Vec<Self>,
        agent: &mut Agent,
        cancel: &CancellationToken,
    ) -> Result<TurnEnd, BoxError> {
        let decided = up_to_first(decided, |d| d.r#continue.is_some());
        if let Some(asked) = decided.iter().find_map(|d| d.compact.clone()) {
            compact_now(agent, cancel, asked).await?;
        }
        let go_on = decided.into_iter().find_map(|d| d.r#continue);
        Ok(go_on.map_or(TurnEnd::Pass, TurnEnd::Continue))
    }
}

impl crate::api::Before {
    /// This decision applied to `call`: an input rewrite replaces its input
    /// (what comes after sees it), a deny breaks with the reason to refuse
    /// it. [`Mods`](crate::Mods) folds every mod's answer with it, and
    /// [`Hooks`](crate::Hooks) every binding's.
    pub(crate) fn apply_to(self, call: &mut ModToolCall) -> ControlFlow<String> {
        match self {
            Self::Pass => {}
            Self::Input(input) => call.input = input,
            Self::Deny(reason) => return ControlFlow::Break(reason),
        }
        ControlFlow::Continue(())
    }
}

/// What `system.prompt` answers add up to: each non-empty text, in order,
/// a blank line between.
pub(crate) fn system_text(parts: impl IntoIterator<Item = String>) -> String {
    let parts: Vec<String> = parts.into_iter().filter(|p| !p.is_empty()).collect();
    parts.join("\n\n")
}

/// `decided` up to and with the first one that `ends` the run: what comes
/// after it was never asked, or never counts.
fn up_to_first<T>(mut decided: Vec<T>, ends: impl Fn(&T) -> bool) -> Vec<T> {
    if let Some(at) = decided.iter().position(ends) {
        decided.truncate(at + 1);
    }
    decided
}

/// Every steer into `agent`'s queue, in order, then the first stop.
fn steer_then_stop(
    agent: &Agent,
    decided: impl Iterator<Item = (Option<String>, Option<String>)>,
) -> Step {
    let mut stop = None;
    for (steer, stopped) in decided {
        if let Some(text) = steer {
            agent.control().steer(text);
        }
        stop = stop.or(stopped);
    }
    stop.map_or(Step::Pass, Step::Stop)
}

/// The compaction a decision asked for: haest-core's, keeping its usual
/// tail. The window is for deciding when to compact, which the hook did.
async fn compact_now(
    agent: &mut Agent,
    cancel: &CancellationToken,
    asked: Compact,
) -> Result<(), BoxError> {
    let facts = agent.deps().shared().facts().model_info(agent.model_spec());
    let cfg = CompactionOptions {
        instructions: asked.instructions,
        ..CompactionOptions::new(facts.map_or(0, |m| m.context_window))
    };
    compact(agent, cancel, &cfg).await?;
    Ok(())
}
