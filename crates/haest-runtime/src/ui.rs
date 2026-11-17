//! What a host shows the human. The runtime decides WHAT happened; the [`Ui`]
//! decides how it looks (a terminal, a web panel, a log).

use std::sync::Arc;

use haest_core::remedy::Limit;
use haest_tools::SubagentEvent;

/// PLUG POINT: how the human hears from a conversation.
///
/// The runtime calls it from whatever task it runs on, so it is `Send + Sync`
/// and should return quickly. A host implements the three levels; the rest
/// have defaults that fall back on them. The turn's own output (text, tool
/// calls, usage) does not come through here: it is the events
/// [`Agent::run`](haest_core::Agent::run) hands the host's sink.
///
/// ```
/// use haest_runtime::Ui;
///
/// /// Everything as a log line.
/// struct Log;
///
/// impl Ui for Log {
///     fn info(&self, text: &str) {
///         eprintln!("info: {text}");
///     }
///     fn warn(&self, text: &str) {
///         eprintln!("warn: {text}");
///     }
///     fn error(&self, text: &str) {
///         eprintln!("error: {text}");
///     }
/// }
/// ```
pub trait Ui: Send + Sync {
    /// A notice that needs nothing from the human: the conversation was
    /// compacted, a model was sent a lower effort than asked, a mod's notice.
    fn info(&self, text: &str);
    /// Something was skipped or held, and the run goes on: an MCP server did
    /// not start, attachments a model cannot read were withheld, the inbox
    /// holds turns until the human writes.
    fn warn(&self, text: &str);
    /// Something failed: a compaction, a write to the session file, a mod.
    fn error(&self, text: &str);

    /// What haest does under the hood; called only with the `debug` setting
    /// on. By default an [`info`](Self::info) line starting `[haest] `.
    fn debug(&self, text: &str) {
        self.info(&format!("[haest] {text}"));
    }

    /// A turn the human did not start arrived (a job ended, an agent wrote):
    /// its first line, cut short. By default an [`info`](Self::info) line.
    ///
    /// The text itself waits in the inbox
    /// ([`InboxQueue`](crate::InboxQueue)) for the host to run.
    fn arrival(&self, headline: &str) {
        self.info(headline);
    }

    /// A subagent's life, keyed by its id: started, a tool it ran, a guard
    /// notice, stopped. Ignored by default.
    fn subagent(&self, _id: &str, _event: &SubagentEvent) {}

    /// The subagents running now, each time that set changes: their ids (the
    /// `id` [`subagent`](Self::subagent) gets). Ignored by default.
    fn subagents_running(&self, _ids: &[String]) {}

    /// How the human gets past `limit` in this host (its command, its
    /// setting): added after haest's message about it, which names neither.
    /// None by default.
    fn remedy(&self, _limit: Limit) -> Option<String> {
        None
    }
}

/// A UI the host shares is one: a host that keeps its UI for itself as well
/// hands [`build`](crate::build) an `Arc` of it. Every call goes to the UI
/// inside, its own defaults included.
impl<T: Ui + ?Sized> Ui for Arc<T> {
    fn info(&self, text: &str) {
        (**self).info(text);
    }
    fn warn(&self, text: &str) {
        (**self).warn(text);
    }
    fn error(&self, text: &str) {
        (**self).error(text);
    }
    fn debug(&self, text: &str) {
        (**self).debug(text);
    }
    fn arrival(&self, headline: &str) {
        (**self).arrival(headline);
    }
    fn subagent(&self, id: &str, event: &SubagentEvent) {
        (**self).subagent(id, event);
    }
    fn subagents_running(&self, ids: &[String]) {
        (**self).subagents_running(ids);
    }
    fn remedy(&self, limit: Limit) -> Option<String> {
        (**self).remedy(limit)
    }
}
