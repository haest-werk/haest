//! Compaction as the runtime does it: when ([`RuntimeHooks::compaction_for`])
//! and how ([`RuntimeHooks::compact_now`]), the spend of the summary calls
//! included.

use haest_core::{
    Agent,
    compaction::{CompactionError, CompactionOptions, CompactionResult, compact},
    error::report,
    now_ms,
    session::SessionRecord,
};
use tokio_util::sync::CancellationToken;

use super::RuntimeHooks;

/// Who asks for a compaction ([`RuntimeHooks::compact_now`]): it decides
/// whether a conversation with nothing to cut yet is told so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Compact {
    /// The human's /compact ([`Runtime::compact`](crate::Runtime::compact)):
    /// the human hears "Nothing to compact yet."
    Manual,
    /// The loop's own check before a model call: nothing to cut stays silent.
    Auto,
}

impl RuntimeHooks {
    /// When to compact a conversation on the model `spec`: at 80% of its
    /// [`window`](Self::window), or at the `compact_at` setting when that is
    /// lower and not 0. About the last 12 messages are kept as they are.
    pub(crate) fn compaction_for(&self, spec: &str) -> CompactionOptions {
        CompactionOptions {
            ceiling: Some(self.settings().caps.compact_at).filter(|n| *n > 0),
            ..CompactionOptions::new(self.window(spec))
        }
    }

    /// Compact `agent`'s conversation now: the older messages become one
    /// summary, the latest are kept, and the session file records it
    /// ([`saved`](Self::saved), through the agent's hooks). Each summary
    /// call's cost goes on the conversation's spend as it comes back (core's
    /// `compact` books it), summary or not.
    ///
    /// `by` says who asked: nothing to cut is said only to the human's
    /// [`Compact::Manual`]. `cancel` stops the summary call.
    ///
    /// # Errors
    ///
    /// When no summary came out: the history is as it was. The error is said
    /// to the UI already, unless `cancel` fired: then the human stopped it,
    /// and the turn ends as aborted.
    pub(crate) async fn compact_now(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
        by: Compact,
    ) -> Result<(), CompactionError> {
        let cfg = self.compaction_for(agent.model_spec());
        match compact(agent, cancel, &cfg).await {
            Ok(None) if by == Compact::Manual => self.info("Nothing to compact yet."),
            Ok(None | Some(_)) => {}
            Err(e) => {
                if !cancel.is_cancelled() {
                    self.error(&compaction_failed(&e));
                }
                return Err(e);
            }
        }
        Ok(())
    }

    /// A compaction of the conversation, whoever made it (the runtime's own,
    /// a mod's): the session file records it, so a resume rebuilds the
    /// history the agent has now, and the human is told.
    pub(super) fn saved(&self, res: &CompactionResult) {
        self.append(&SessionRecord::Compaction {
            t: now_ms(),
            summary: res.summary.clone(),
            kept: res.kept,
        });
        self.info(&format!(
            "Conversation compacted; kept the last {} messages.",
            res.kept
        ));
    }
}

/// What the human and a failed turn are told when a summary did not come out.
pub(super) fn compaction_failed(e: &CompactionError) -> String {
    format!("Compaction failed: {}", report(e))
}
