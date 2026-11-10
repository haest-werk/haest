//! What a host shows the human. The runtime decides WHAT happened; the [`Ui`]
//! decides how it looks (a terminal, a web panel, a log).

use kern_tools::agents::SubagentEvent;

pub trait Ui: Send + Sync {
    fn info(&self, text: &str);
    fn warn(&self, text: &str);
    fn error(&self, text: &str);

    /// What kern does under the hood (the `debug` setting is on).
    fn debug(&self, text: &str) {
        self.info(&format!("[kern] {text}"));
    }

    /// The human's input line is open: what arrives now waits for it.
    fn prompting(&self) -> bool {
        false
    }

    /// A turn the human did not start arrived (a job ended, an agent wrote):
    /// its first line, cut short.
    fn arrival(&self, headline: &str) {
        self.info(headline);
    }

    /// A subagent's life: started, a tool it ran, a guard notice, stopped.
    fn subagent(&self, _id: &str, _event: &SubagentEvent) {}

    /// The subagents running now, by name.
    fn subagents_running(&self, _names: &[String]) {}
}
