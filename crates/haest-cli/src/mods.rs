//! Approval asks on the terminal. With no terminal to ask on, an ask is a deny
//! that says why. The terminal has one reader at a time: asks from main and
//! from subagents take turns, and one raised while the REPL prompt is open
//! waits for it (announced above the prompt). Driven over stdio, the parent
//! answers them instead ([`CliPrompter::Session`]).

use std::sync::Arc;

use async_trait::async_trait;
use haest::{
    mods::{ApprovalAnswer, ApprovalRequest, Prompter},
    runtime::Ui,
    session::SessionPrompter,
};

use crate::{ask, term::TermUi};

pub(crate) struct TermPrompter {
    ui: Arc<TermUi>,
    interactive: bool,
}

impl TermPrompter {
    pub(crate) fn new(ui: Arc<TermUi>, interactive: bool) -> Self {
        Self { ui, interactive }
    }
}

fn question(req: &ApprovalRequest) -> String {
    let mut q = format!(
        "Allow {}? {}",
        shown(&req.tool_name),
        shown(&req.description)
    );
    let preview = req.input_preview.trim();
    if !preview.is_empty() && preview != req.description.trim() {
        q.push_str(&format!("\n  {}", shown(preview)));
    }
    q + "\n[y]es / [a]lways / [N]o: "
}

/// `text` as the terminal shows it: every control character but a newline,
/// and every character that reorders or hides text (bidi controls, zero-width
/// ones), as its escape, so nothing in a command can draw over it; each
/// further line indented under the first.
fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let hides = matches!(c,
            '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}');
        if hides {
            out.extend(c.escape_unicode());
        } else if c.is_control() && c != '\n' {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out.replace('\n', "\n  ")
}

/// y / yes, a / always; anything else (an empty line included) is no.
fn answer(line: Option<&str>) -> ApprovalAnswer {
    match line.map(|l| l.trim().to_ascii_lowercase()).as_deref() {
        Some("y" | "yes") => ApprovalAnswer::Allow,
        Some("a" | "always") => ApprovalAnswer::AllowAlways,
        _ => ApprovalAnswer::Deny,
    }
}

#[async_trait]
impl Prompter for TermPrompter {
    async fn prompt(&self, req: &ApprovalRequest) -> ApprovalAnswer {
        if !self.interactive {
            self.ui.warn(&format!(
                "Denied {}: {} -- approval needed and there is no terminal to ask on (--approvals=false turns approvals off).",
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
        self.ui.ask(&question(req));
        answer(ask::read_line().await.as_deref())
    }

    fn auto_approved(&self, req: &ApprovalRequest) {
        self.ui.info(&format!(
            "Allowed {} (always): {}",
            req.tool_name, req.description
        ));
    }
}

/// Who answers approval asks: the human at the terminal, or the parent that
/// drives haest over stdio (each ask an `approval_ask` frame).
pub(crate) enum CliPrompter {
    Term(TermPrompter),
    Session(SessionPrompter),
}

#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl Prompter for CliPrompter {
    async fn prompt(&self, req: &ApprovalRequest) -> ApprovalAnswer {
        match self {
            Self::Term(p) => p.prompt(req).await,
            Self::Session(p) => p.prompt(req).await,
        }
    }

    fn auto_approved(&self, req: &ApprovalRequest) {
        match self {
            Self::Term(p) => p.auto_approved(req),
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

    fn ask(preview: &str) -> ApprovalRequest {
        ApprovalRequest {
            tool_name: "Bash".into(),
            description: "haest wants to run a command that deletes files".into(),
            input_preview: preview.into(),
            tool_use_id: None,
            key: Some(preview.into()),
        }
    }

    /// Nothing in a command can draw over it on the terminal: no carriage
    /// return, escape sequence or bidi override reaches it raw, and the end
    /// of a long command shows.
    #[test]
    fn the_question_shows_the_command_as_it_is() {
        let drawn_over = question(&ask(
            "rm -rf ./build; touch PWNED #\r\u{1b}[2K  rm -rf ./build",
        ));
        let reordered = question(&ask("ls \u{202e}dlihc\u{202c} \u{200b}x"));
        let padded = question(&ask(&format!(
            "rm -rf ./build{}; touch PWNED",
            " ".repeat(186)
        )));

        for q in [&drawn_over, &reordered] {
            let raw: Vec<char> = q.chars().filter(|c| *c != '\n' && (c.is_control() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))).collect();
            assert!(raw.is_empty(), "{raw:?} in {q:?}");
        }
        assert!(drawn_over.contains("touch PWNED"), "{drawn_over}");
        assert!(padded.contains("; touch PWNED"), "{padded}");
    }
}
