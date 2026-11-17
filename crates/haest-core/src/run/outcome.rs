//! How a durable run ended: [`RunEnd`] (its record), [`RunOutcome`] (what
//! [`Agent::run_durable`](crate::Agent::run_durable) returns), [`RunError`].

use std::io;

use serde::{Deserialize, Serialize};

use super::RunKey;
use crate::{
    messages::{StopReason, Usage},
    remedy::Limit,
};

/// How a durable run ended, as its `run_end` record keeps it: what a second
/// start under the same key returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEnd {
    /// Why the turn ended.
    pub stop_reason: StopReason,
    /// Which cap stopped it, or what went wrong; absent for a turn that ended normally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The text of the turn's last answer.
    #[serde(default)]
    pub text: String,
    /// What the whole run cost, a resumed run's spend before the stop included.
    #[serde(default)]
    pub usage: Usage,
    /// The cap that stopped it ([`TurnOutcome::limit`](crate::TurnOutcome::limit)).
    /// Not kept in the `run_end` record: a stored result has None.
    #[serde(skip)]
    pub limit: Option<Limit>,
}

/// Where a durable run's result came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Served {
    /// The store had nothing under the key: the turn ran.
    Fresh,
    /// The store had a run that never ended: it went on from where it stopped.
    Resumed,
    /// The store had a run that ended: its result, and nothing ran.
    Stored,
}

/// How a durable run ended: [`RunEnd`] and where it came from.
/// [`Agent::run_durable`](crate::Agent::run_durable) returns it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RunOutcome {
    /// Why the turn ended, as [`TurnOutcome::stop_reason`](crate::TurnOutcome::stop_reason)
    /// says: [`StopReason::MaxTokens`] is the model's own output limit, or a
    /// turn cap or the conversation budget, which `limit` names for a turn
    /// that ran now and `error` for a stored result.
    pub stop_reason: StopReason,
    /// Which cap stopped it, or what went wrong; None for a turn that ended normally.
    pub error: Option<String>,
    /// The text of the turn's last answer.
    pub text: String,
    /// What the whole run cost, a resumed run's spend before the stop included.
    /// A stored result's usage is what the run cost then: nothing was spent now.
    pub usage: Usage,
    /// The cap that stopped it, for a turn that ran now; None for a stored
    /// result ([`RunEnd::limit`]).
    pub limit: Option<Limit>,
    /// Whether the turn ran, went on, or came from the store.
    pub served: Served,
}

impl RunOutcome {
    pub(crate) fn served(end: RunEnd, served: Served) -> Self {
        Self {
            stop_reason: end.stop_reason,
            error: end.error,
            text: end.text,
            usage: end.usage,
            limit: end.limit,
            served,
        }
    }
}

/// Why [`Agent::run_durable`](crate::Agent::run_durable) did not run.
/// Nothing was written and no event emitted.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    /// The agent has no run store to keep the run in.
    #[error("a durable run needs a run store, and the agent has none")]
    NoStore,
    /// The key names a run that was started with another input.
    #[error("run \"{key}\" was started with another input (input hash {stored}, this one {given})")]
    InputMismatch {
        /// The run's key.
        key: RunKey,
        /// The hash the run's record keeps.
        stored: String,
        /// This input's hash.
        given: String,
    },
    /// The agent's history is not the one the run started on.
    #[error(
        "run \"{key}\" started on a history of {base} messages, and the agent's history does not begin with it ({found} messages)"
    )]
    HistoryMismatch {
        /// The run's key.
        key: RunKey,
        /// How many messages the history held when the run started.
        base: usize,
        /// How many the agent holds now.
        found: usize,
    },
    /// A turn of this agent is already running.
    #[error("a turn is already running")]
    TurnRunning,
    /// The store could not load the run.
    #[error("the run store could not load run \"{key}\"")]
    Store {
        /// The run's key.
        key: RunKey,
        /// The store's error.
        #[source]
        source: io::Error,
    },
}

/// Whether a turn that ended so is finished: it ended on its own. An error or
/// an abort leaves the run resumable.
pub(crate) fn finished(stop: StopReason) -> bool {
    !matches!(stop, StopReason::Error | StopReason::Aborted)
}
