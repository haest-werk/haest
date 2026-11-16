//! Durable runs: a turn that a crash, a deploy or a kill stops part way goes
//! on where it stopped, and a turn started twice under the same [`RunKey`]
//! returns the first one's result.
//!
//! Opt-in, off by default: nothing is durable unless the host hands a
//! [`RunStore`] ([`SharedDeps::run_store`](crate::SharedDeps::run_store)) and
//! starts the turn with [`Agent::run_durable`](crate::Agent::run_durable)
//! and a key. [`Agent::run`](crate::Agent::run) never touches a store.
//!
//! | The store has for the key | `run_durable` does |
//! |---|---|
//! | nothing | runs the turn, writing its records to the store |
//! | a run with this input that never ended | resumes it ([`Served::Resumed`]) |
//! | a run with this input that ended | returns its result: no model call, no tool, no spend ([`Served::Stored`]) |
//! | a run with another input | refuses ([`RunError::InputMismatch`]) |
//!
//! # Records
//!
//! A durable run writes session records ([`SessionRecord`](crate::session::SessionRecord))
//! beside the ones a session file already has, so a reader that does not know
//! them skips them, as both haests do:
//!
//! | `type` | Written | Holds |
//! |---|---|---|
//! | `run` | when the run starts | the key, the input's hash, how long the history was |
//! | `message` | every message the turn appends | the message |
//! | `turn` | after every model call, every tool result and every step | [`TurnState`]: what the turn caps count |
//! | `run_end` | when the run ends on its own | [`RunEnd`]: the result a second start returns |
//!
//! A turn that ends with an error or an abort writes no `run_end`: it stays
//! resumable, so a second start retries it from where it stopped.
//!
//! # Resuming
//!
//! The model is asked again only for a step that never finished: a model call
//! whose answer landed is in the log. The calls of the last answer that have
//! no result are answered by each tool's [`OnResume`](crate::tool::OnResume):
//! a read-only call runs again, any other is answered as interrupted unless
//! its tool says it can run again or look up what happened. A call that
//! cannot have started (it comes after a call with side effects whose result
//! never landed) runs once, as it would have. The turn caps go on from the
//! last [`TurnState`]: the spend, the model calls, the time (not counting the
//! time the run was stopped) and the repeat counts.
//!
//! # Guarantees and limits
//!
//! - Side effects run at least once. Exactly once only where a tool passes
//!   its [`effect key`](crate::ToolContext::effect_key) to an API that honours it.
//! - A model call in flight at the stop is paid again: its answer never landed.
//! - The loop flushes the run's records before every batch of tool calls that
//!   holds a tool with side effects, so the call (the intent) lands before the
//!   effect. Read-only batches do not wait.
//! - The conversation ledger and the history before the run stay the host's,
//!   as for any turn: restore them as for a resumed session.
//!
//! # Examples
//!
//! See [`Agent::run_durable`](crate::Agent::run_durable).

mod journal;
mod outcome;
mod replay;
mod store;
#[cfg(test)]
mod tests;

use std::fmt;

use serde::{Deserialize, Serialize};

pub(crate) use journal::Journal;
pub(crate) use outcome::finished;
pub use outcome::{RunEnd, RunError, RunOutcome, Served};
pub use replay::{ReplayedRun, replay_run};
pub use store::{MemoryRunStore, RunStore};

use crate::{messages::Usage, stable_hash::hex128};

/// The name of one durable run, chosen by the host: a job id, a request id,
/// a message id. Starting a run under a key the store already has resumes it
/// or returns its result.
///
/// A key is any string. A subagent's or a step's key derives from its
/// parent's with [`RunKey::child`], so a replay finds it again.
///
/// # Examples
///
/// ```
/// use haest_core::run::RunKey;
///
/// let job = RunKey::new("nightly-report-2026-11-16");
/// let child = job.child("call-7");
/// assert_eq!(child.as_str(), "nightly-report-2026-11-16/call-7");
///
/// // The same call in the same run always has the same effect key.
/// assert_eq!(job.effect_key("call-7"), job.effect_key("call-7"));
/// assert_ne!(job.effect_key("call-7"), child.effect_key("call-7"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunKey(String);

impl RunKey {
    /// The key `key`, as given.
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The key as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A key under this one: `<this>/<part>`. A subagent's key is its
    /// parent's with the tool call id; a flow step's, the step's name.
    pub fn child(&self, part: &str) -> RunKey {
        RunKey(format!("{}/{part}", self.0))
    }

    /// The effect key of tool call `tool_call_id` in this run: a stable hash
    /// of the two, the same in every process and every build.
    pub fn effect_key(&self, tool_call_id: &str) -> EffectKey {
        EffectKey(hex128(&[
            b"effect",
            self.0.as_bytes(),
            tool_call_id.as_bytes(),
        ]))
    }
}

impl fmt::Display for RunKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for RunKey {
    fn from(key: &str) -> Self {
        Self::new(key)
    }
}

impl From<String> for RunKey {
    fn from(key: String) -> Self {
        Self(key)
    }
}

/// One tool call's effect key: 32 hex digits, a stable hash of the run key
/// and the call id ([`ToolContext::effect_key`](crate::ToolContext::effect_key)).
///
/// A tool hands it to an outside API as that API's idempotency key (a
/// payment, an image job): when a resumed run calls the tool again, the API
/// sees the same key and does not do the work twice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EffectKey(String);

impl EffectKey {
    /// The key as text: 32 lowercase hex digits.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EffectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The hash a `run` record keeps of the run's input: 32 hex digits, the same
/// in every process and every build. A second start whose input hashes
/// differently is refused.
pub fn input_hash(input: &str) -> String {
    hex128(&[b"input", input.as_bytes()])
}

/// What the turn caps count, as a `turn` record keeps it: absolute, the last
/// one wins. A resumed turn starts from it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnState {
    /// What the turn has spent: its model calls, a summary a hook paid for,
    /// and every tool's own spend.
    pub usage: Usage,
    /// Model calls made.
    pub steps: u64,
    /// How long the turn had run, on the host's clock. A resumed turn's time
    /// cap counts on from here: the time the run was stopped does not count.
    pub elapsed_ms: u64,
    /// Identical tool calls with identical results, in first-seen order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repeats: Vec<RepeatCount>,
    /// What the model was already told this turn, so a resumed turn does not
    /// tell it again: each repeat nudged (`repeat:<key>`), and each of the
    /// turn's thresholds that fired (`threshold <n>: <action> on <meter>`;
    /// the guard's nudges among them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nudged: Vec<String>,
    /// The least the next request will hold, in tokens: what a tool result may
    /// still add is worked out from it.
    #[serde(default)]
    pub request_tokens: u64,
}

/// How often one tool call (same input, same result) came back this turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepeatCount {
    /// The call and its result, hashed: 16 hex digits.
    pub key: String,
    /// The tool's name.
    pub tool: String,
    /// How many times.
    pub count: u64,
}
