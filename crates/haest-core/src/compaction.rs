//! Context compaction: when the last request filled most of the window,
//! summarise everything except the recent tail with the same model, and
//! continue on [summary, ...tail]. The summary request is the last loop request
//! plus one instruction (see [`SummaryPath`]), so it costs a cache read, not a re-read.
//!
//! The cut lands before a user or an assistant message, never between a tool
//! call and its result, so the tail never starts with an orphaned tool result
//! and every tool call in it keeps its result.
//!
//! The loop never compacts on its own. A host checks [`needs_compaction`] and
//! calls [`compact`], usually from
//! [`AgentHooks::before_model_call`](crate::AgentHooks::before_model_call);
//! [`compact`]'s docs show the whole hook. That check is the compaction
//! threshold: [`CompactionOptions`] is one.

use std::collections::HashMap;

use tokio_util::sync::CancellationToken;

use crate::{
    agent::Agent,
    messages::{AssistantMessage, Message, StopReason, Usage},
    provider::ModelError,
    session::compaction_summary_message,
    threshold::{Action, Mark, Meter, Reading, Rearm, Threshold},
};

/// The share of the window at which haest-runtime compacts (its
/// [`CompactionOptions::threshold`]).
pub const COMPACT_SHARE: f64 = 0.8;

/// About how many messages haest-runtime keeps verbatim after a summary (its
/// [`CompactionOptions::keep_recent`]).
pub const KEEP_RECENT: usize = 12;

/// When to compact, and how much to keep.
///
/// It is the compaction threshold ([`threshold`](crate::threshold)): it reads
/// [`Meter::ContextTokens`] against the window, its mark is `threshold` of
/// the window or `ceiling` when that is lower, and it compacts at every check
/// it is past the mark ([`Rearm::EveryCheck`]: a check that found nothing to
/// cut tries again at the next). [`needs_compaction`] is its check.
///
/// [`new`](Self::new) is haest-runtime's choice for a window; change a field
/// with struct update syntax:
///
/// ```
/// use haest_core::compaction::CompactionOptions;
///
/// let cfg = CompactionOptions { ceiling: Some(150_000), ..CompactionOptions::new(1_000_000) };
/// assert_eq!((cfg.threshold, cfg.keep_recent), (0.8, 12));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionOptions {
    /// The model's context window, in tokens.
    pub context_window: u64,
    /// Fraction of the window that triggers compaction.
    pub threshold: f64,
    /// Absolute trigger in tokens when lower than the fraction (a 1M window at 80% is too late to be affordable).
    pub ceiling: Option<u64>,
    /// Roughly how many recent messages to keep verbatim.
    pub keep_recent: usize,
}

impl CompactionOptions {
    /// Compact at [`COMPACT_SHARE`] of `context_window` tokens, with no
    /// ceiling, keeping about [`KEEP_RECENT`] messages.
    #[must_use]
    pub const fn new(context_window: u64) -> Self {
        Self {
            context_window,
            threshold: COMPACT_SHARE,
            ceiling: None,
            keep_recent: KEEP_RECENT,
        }
    }
}

impl Threshold for CompactionOptions {
    fn meter(&self) -> Meter {
        Meter::ContextTokens
    }
    fn mark(&self) -> Mark {
        let share = Mark::share(self.threshold);
        match self.ceiling {
            Some(ceiling) => share.or_at(ceiling as f64),
            None => share,
        }
    }
    fn action(&self) -> Action {
        Action::Compact
    }
    fn rearm(&self) -> Rearm {
        Rearm::EveryCheck
    }
    fn text(&self, r: &Reading) -> String {
        format!("The context holds {} tokens.", r.value)
    }
}

fn last_assistant(messages: &[Message]) -> Option<&AssistantMessage> {
    messages.iter().rev().find_map(|m| match m {
        Message::Assistant(a) if a.stop_reason != StopReason::Error => Some(a),
        _ => None,
    })
}

/// What the last successful request put in the context window; 0 before any.
pub(crate) fn last_context_tokens(messages: &[Message]) -> u64 {
    last_assistant(messages).map_or(0, |a| a.usage.context_tokens())
}

/// The LAST successful request filled the trigger (not the sum of every request).
///
/// The trigger is `context_window * threshold`, or `ceiling` when that is
/// lower ([`CompactionOptions`]' mark); a request "filled" it when its
/// [`Usage::context_tokens`] reached it. A failed request never triggers, and
/// an empty history is never due.
pub fn needs_compaction(messages: &[Message], cfg: &CompactionOptions) -> bool {
    last_assistant(messages).is_some_and(|a| {
        let reading = Reading {
            meter: Meter::ContextTokens,
            value: a.usage.context_tokens() as f64,
            cap: Some(cfg.context_window as f64),
            limit: None,
        };
        cfg.mark().reached(&reading)
    })
}

/// Index of the first kept message: the latest at or before len-keep_recent
/// where a tail may start; 0 = nothing to cut.
///
/// A tail may start at a user or an assistant message, never at a tool
/// result, and never where a result after the cut answers a call before it.
/// At least the first message is always cut and the last always kept, so a
/// history of one message has nothing to cut.
pub(crate) fn cut_index(messages: &[Message], keep_recent: usize) -> usize {
    if messages.is_empty() {
        return 0;
    }
    let start = (messages.len() - 1).min(messages.len().saturating_sub(keep_recent).max(1));
    let starts = tail_starts(messages);
    (1..=start).rev().find(|&i| starts[i]).unwrap_or(0)
}

/// Per message: may the kept tail start there? At a user message or an
/// assistant message (the summary is a user message, so roles still
/// alternate), never at a tool result, and never where a result after the cut
/// answers a call before it. One long agentic turn has no user message to cut
/// at, and must still compact.
fn tail_starts(messages: &[Message]) -> Vec<bool> {
    // For each result, where its call is: the latest call of that id before it.
    let mut called_at: HashMap<&str, usize> = HashMap::new();
    let mut call_of = vec![usize::MAX; messages.len()];
    for (i, m) in messages.iter().enumerate() {
        match m {
            Message::Assistant(a) => called_at.extend(a.tool_calls().map(|c| (c.id.as_str(), i))),
            Message::Tool(r) => {
                call_of[i] = called_at
                    .get(r.tool_call_id.as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
            }
            Message::User(_) => {}
        }
    }
    let mut earliest_answered = usize::MAX;
    let mut starts = vec![false; messages.len()];
    for i in (0..messages.len()).rev() {
        earliest_answered = earliest_answered.min(call_of[i]);
        starts[i] = !matches!(messages[i], Message::Tool(_)) && earliest_answered >= i;
    }
    starts
}

crate::word_enum! {
    /// Which request wrote a compaction summary.
    ///
    /// Warm: the main loop's last request again (same system, tools, effort, history) plus the summary
    /// instruction, so the provider's prompt cache serves everything but the new rows.
    /// Cold: a standalone summary request; its prefix matches nothing, so the whole head is read at full price.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SummaryPath ("summary path") {
        /// The loop's last request plus the instruction: mostly a cache read.
        Warm = "warm",
        /// A standalone request over the cut head, after the warm one gave no summary.
        Cold = "cold",
    }
}

/// What [`compact`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    /// The summary text. History now opens with it, in a user message.
    pub summary: String,
    /// Messages kept verbatim after the summary.
    pub kept: usize,
    /// Which request wrote the summary.
    pub path: SummaryPath,
    /// What every summary call cost, the warm one too when the cold one wrote
    /// it. Already counted toward the running turn's caps; the caller charges
    /// it to the conversation's ledger.
    pub usage: Usage,
}

/// Why [`compact`] wrote no summary. History is untouched either way.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CompactionError {
    /// The agent's model spec resolves to no provider (no call was made).
    #[error(transparent)]
    Model(#[from] ModelError),
    /// Neither path produced a summary, or the run was cancelled; history is
    /// untouched. Why, and what the calls made cost (see [`Self::usage`]).
    #[error("compaction summary failed: {reason}")]
    Failed {
        /// Why no summary came out.
        reason: String,
        /// What the summary calls cost ([`CompactionError::usage`]).
        usage: Usage,
    },
}

impl CompactionError {
    /// What the summary calls cost before it failed: a failure is paid for too.
    /// Already counted toward the running turn's caps; the caller charges it to
    /// the conversation's ledger.
    pub fn usage(&self) -> Usage {
        match self {
            CompactionError::Model(_) => Usage::default(),
            CompactionError::Failed { usage, .. } => usage.clone(),
        }
    }
}

/// Compact in place. Returns the summary, how many messages were kept and how it
/// was written, or None if there is nothing to cut. On error history is untouched.
/// Every call it makes counts toward the running turn's caps, success or not.
///
/// The summary is asked of the agent's own model ([`Agent::model_spec`]),
/// warm first, then cold if the warm reply is not a whole summary (it failed,
/// called a tool, was cut off, or said nothing). The head of history is replaced by
/// one user message carrying the summary; [`on_message`](crate::AgentHooks::on_message)
/// is not called, so a host that persists history records the change itself.
/// The cost is not charged to the ledger: the caller does that with
/// [`CompactionResult::usage`] or [`CompactionError::usage`].
///
/// # Errors
///
/// [`CompactionError::Model`] when the model spec resolves to no provider (no
/// call made); [`CompactionError::Failed`] when neither request produced a
/// summary or `cancel` fired.
///
/// # Cancellation
///
/// `cancel` goes to every summary request. Cancelled during the warm one, it
/// skips the cold one and returns [`CompactionError::Failed`].
///
/// # Examples
///
/// Compact from the hook that runs before every model call:
///
/// ```
/// # #[cfg(feature = "testing")]
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// use haest_core::{
///     Agent, AgentDeps, AgentHooks, AgentOptions, BoxError, LoopEvent, Message, SharedDeps,
///     ShutWorkspace, Tools,
///     compaction::{CompactionOptions, compact, needs_compaction},
///     testing::{ScriptedProvider, text_answer},
/// };
/// use tokio_util::sync::CancellationToken;
///
/// struct Compacting(CompactionOptions);
///
/// #[async_trait::async_trait]
/// impl AgentHooks for Compacting {
///     async fn before_model_call(
///         &self,
///         agent: &mut Agent,
///         cancel: &CancellationToken,
///     ) -> Result<(), BoxError> {
///         if !needs_compaction(agent.history(), &self.0) {
///             return Ok(());
///         }
///         // A host also charges the usage to its ledger and saves the new history.
///         // An error ends the turn instead of retrying before every call.
///         compact(agent, cancel, &self.0).await?;
///         Ok(())
///     }
/// }
///
/// // The summary request is answered first, then the turn's own call.
/// let provider = ScriptedProvider::new([text_answer("Earlier: a and b."), text_answer("done")]);
/// let cfg = CompactionOptions { keep_recent: 2, ..CompactionOptions::new(1_000) };
/// let shared = SharedDeps::new(provider.resolver(), "/", ShutWorkspace);
/// let deps = AgentDeps::new(shared, Tools::new(), "").with_hooks(Compacting(cfg));
///
/// // Resume a conversation whose last request filled 90% of the window.
/// let mut full = text_answer("b done");
/// full.usage.input = 900;
/// let history = vec![
///     Message::user("a"),
///     Message::Assistant(text_answer("a done")),
///     Message::user("b"),
///     Message::Assistant(full),
/// ];
/// let mut agent = Agent::new(deps, AgentOptions { history, ..AgentOptions::new("scripted:m") });
/// agent.run("c", &mut Vec::<LoopEvent>::new()).await;
///
/// // [summary, "b done", "c", "done"]: the head became one message.
/// assert_eq!(agent.history().len(), 4);
/// assert!(matches!(&agent.history()[0], Message::User(u) if u.content.contains("Earlier: a and b.")));
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "testing"))]
/// # fn main() {}
/// ```
pub async fn compact(
    agent: &mut Agent,
    cancel: &CancellationToken,
    cfg: &CompactionOptions,
) -> Result<Option<CompactionResult>, CompactionError> {
    let cut = cut_index(agent.history(), cfg.keep_recent);
    if cut == 0 {
        return Ok(None);
    }
    let kept = agent.history().len() - cut;
    let (res, path) = summarize(agent, &agent.history()[..cut], kept, cancel).await?;
    agent.spent_outside_the_loop(&res.usage);
    let summary = match res.text {
        Ok(summary) => summary,
        Err(reason) => {
            return Err(CompactionError::Failed {
                reason,
                usage: res.usage,
            });
        }
    };
    agent
        .history_mut()
        .splice(..cut, [compaction_summary_message(&summary)]);
    Ok(Some(CompactionResult {
        summary,
        kept,
        path,
        usage: res.usage,
    }))
}

mod summary;

use summary::summarize;

#[cfg(test)]
mod tests;
