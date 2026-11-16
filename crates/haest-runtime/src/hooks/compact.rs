//! Compaction as the runtime does it: when ([`RuntimeHooks::compaction_for`])
//! and how ([`RuntimeHooks::compact_now`]), the spend of the summary calls
//! included.

use tokio_util::sync::CancellationToken;

use super::RuntimeHooks;
use crate::haest::{
    Agent,
    core::compaction::{CompactionConfig, CompactionError, compact},
    core::error::report,
    core::messages::Usage,
    core::session::SessionRecord,
    now_ms,
};

/// Messages kept verbatim after a compaction summary (the TS haest's number).
const KEEP_RECENT: usize = 12;
const COMPACT_SHARE: f64 = 0.8;

impl RuntimeHooks {
    /// When to compact a conversation on the model `spec`: at 80% of its
    /// [`window`](Self::window), or at the `compact_at` setting when that is
    /// lower and not 0. About the last 12 messages are kept as they are.
    pub fn compaction_for(&self, spec: &str) -> CompactionConfig {
        CompactionConfig {
            context_window: self.window(spec),
            threshold: COMPACT_SHARE,
            ceiling: Some(self.settings.caps.compact_at).filter(|n| *n > 0),
            keep_recent: KEEP_RECENT,
        }
    }

    /// Compact `agent`'s conversation now: the older messages become one
    /// summary, the latest are kept, and the session file records it. What
    /// the summary calls cost goes on the conversation's spend either way.
    ///
    /// `auto`: the loop's own check, so nothing to cut stays silent; a host's
    /// /compact passes false and the human hears "Nothing to compact yet."
    /// `cancel` stops the summary call.
    ///
    /// # Errors
    ///
    /// When no summary came out: the history is as it was. The error is said
    /// to the UI already, unless `cancel` fired: then the human stopped it,
    /// and the turn ends as aborted.
    pub async fn compact_now(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
        auto: bool,
    ) -> Result<(), CompactionError> {
        let cfg = self.compaction_for(agent.model_spec());
        match compact(agent, cancel, &cfg).await {
            Ok(None) if !auto => self.info("Nothing to compact yet."),
            Ok(None) => {}
            Ok(Some(res)) => {
                charge(agent, &res.usage);
                self.append(SessionRecord::Compaction {
                    t: now_ms(),
                    summary: res.summary,
                    kept: res.kept,
                });
                self.info(&format!(
                    "Conversation compacted; kept the last {} messages.",
                    res.kept
                ));
            }
            Err(e) => {
                charge(agent, &e.usage());
                if !cancel.is_cancelled() {
                    self.error(&compaction_failed(&e));
                }
                return Err(e);
            }
        }
        Ok(())
    }
}

/// What the human and a failed turn are told when a summary did not come out.
pub(super) fn compaction_failed(e: &CompactionError) -> String {
    format!("Compaction failed: {}", report(e))
}

/// What the summary calls cost goes on the conversation's bill, summary or
/// not. The loop only counts it toward the turn's caps (`take_side_spend`).
fn charge(agent: &Agent, usage: &Usage) {
    if let Some(ledger) = &agent.deps().ledger {
        ledger.lock().unwrap().charge(usage);
    }
}
