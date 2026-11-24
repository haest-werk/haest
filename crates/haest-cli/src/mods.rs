//! Approval asks on the terminal. With no terminal to ask on, an ask is a deny
//! that says why, on stderr: stdin must be a terminal, and so must stderr,
//! where the question shows (with --json it is a line on stdout, which must
//! not be a regular file). The terminal has one
//! reader at a time: asks from main and from subagents take turns, and one
//! raised while the REPL prompt is open waits for it (announced above the
//! prompt). Driven over stdio, the parent answers them instead
//! ([`CliPrompter::Session`]); at a terminal with peers that may answer (a
//! `command` control socket or broker link), both are asked and the first
//! answer wins ([`CliPrompter::Shared`]).

mod question;

use std::sync::Arc;

use async_trait::async_trait;
use haest::{
    mods::{ApprovalAnswer, ApprovalRequest, Prompter},
    runtime::Ui,
    session::{OpenAsk, SessionPrompter},
};

use self::question::question;
use crate::{
    ask::{self, NoTerminal},
    term::TermUi,
};

pub(crate) struct TermPrompter {
    ui: Arc<TermUi>,
    /// Can a human answer here ([`ask::interactive`]).
    interactive: Result<(), NoTerminal>,
}

impl TermPrompter {
    pub(crate) fn new(ui: Arc<TermUi>, interactive: Result<(), NoTerminal>) -> Self {
        Self { ui, interactive }
    }
}

/// y / yes, a / always; anything else (an empty line included) is no.
fn answer(line: Option<&str>) -> ApprovalAnswer {
    match line.map(|l| l.trim().to_ascii_lowercase()).as_deref() {
        Some("y" | "yes") => ApprovalAnswer::Allow,
        Some("a" | "always") => ApprovalAnswer::AllowAlways,
        _ => ApprovalAnswer::Deny,
    }
}

impl TermPrompter {
    /// Asks `req` on the terminal. `approval`: the number the same ask has
    /// on the session's stream, when peers are asked too (the question's
    /// frame names it, so a peer knows the two are one ask).
    async fn ask(&self, req: &ApprovalRequest, approval: Option<u64>) -> ApprovalAnswer {
        if let Err(why) = self.interactive {
            self.ui.refused(&format!(
                "Denied {}: {} -- approval needed and {why} (--approvals=false turns approvals off).",
                req.tool_name, req.description
            ));
            return ApprovalAnswer::Deny;
        }
        let _terminal = match self.ui.stdin.try_lock() {
            Ok(free) => free,
            Err(_) => {
                let when = if self.ui.prompting() {
                    "press Enter at the prompt to answer it"
                } else {
                    "it is asked after the question before it"
                };
                self.ui.info(&format!(
                    "{} waits for your approval: {} -- {when}.",
                    req.tool_name, req.description
                ));
                self.ui.stdin.lock().await
            }
        };
        // Nothing else reaches the screen until it is answered, or given up
        // (the read dropped: the guard lets go then too).
        let mut answering = self.ui.ask(&question(req), approval);
        // Only what is typed once the question is on the screen answers it.
        if !answering.drawn().await {
            return ApprovalAnswer::Deny;
        }
        let line = ask::read_answer().await;
        answering.answered(line.as_deref());
        answer(line.as_deref())
    }
}

#[async_trait]
impl Prompter for TermPrompter {
    async fn prompt(&self, req: &ApprovalRequest) -> ApprovalAnswer {
        self.ask(req, None).await
    }

    fn auto_approved(&self, req: &ApprovalRequest) {
        self.ui.info(&format!(
            "Allowed {} (always): {}",
            req.tool_name, req.description
        ));
    }
}

/// Who answers approval asks: the human at the terminal, the parent that
/// drives haest over stdio (each ask an `approval_ask` frame), or both.
pub(crate) enum CliPrompter {
    Term(TermPrompter),
    Session(SessionPrompter),
    /// At a terminal whose peers may answer too (a `command` control socket
    /// or broker link): each ask goes to both, and the first answer wins
    /// ([`first_answer`]).
    Shared(TermPrompter, SessionPrompter),
}

/// The first answer to `open`, a peer's or `terminal`'s, which asks the same
/// on the terminal. The other side's question is withdrawn: a peer's answer
/// drops the terminal's read (it stops waiting, and the line it showed
/// ends); the terminal's answer is said on the stream, so peers drop theirs.
async fn first_answer(
    mut open: OpenAsk,
    terminal: impl Future<Output = ApprovalAnswer>,
) -> ApprovalAnswer {
    tokio::select! {
        answer = open.peer_answer() => answer,
        answer = terminal => open.answered_at_terminal(answer),
    }
}

#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl Prompter for CliPrompter {
    async fn prompt(&self, req: &ApprovalRequest) -> ApprovalAnswer {
        match self {
            Self::Term(p) => p.prompt(req).await,
            Self::Session(p) => p.prompt(req).await,
            Self::Shared(term, peers) => {
                let open = peers.open(req);
                let approval = open.ask();
                first_answer(open, term.ask(req, Some(approval))).await
            }
        }
    }

    fn auto_approved(&self, req: &ApprovalRequest) {
        match self {
            // The terminal's line goes into the stream the peers read.
            Self::Term(p) | Self::Shared(p, _) => p.auto_approved(req),
            Self::Session(p) => p.auto_approved(req),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_yes_or_always_allow_and_silence_is_no() {
        assert_eq!(answer(Some("y\n")), ApprovalAnswer::Allow);
        assert_eq!(answer(Some(" YES ")), ApprovalAnswer::Allow);
        assert_eq!(answer(Some("a\n")), ApprovalAnswer::AllowAlways);
        assert_eq!(answer(Some("\n")), ApprovalAnswer::Deny);
        assert_eq!(answer(Some("sure")), ApprovalAnswer::Deny);
        assert_eq!(answer(None), ApprovalAnswer::Deny);
    }
}
