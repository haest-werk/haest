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

use crate::{ask, render::inert, term::TermUi};

pub(crate) struct TermPrompter {
    ui: Arc<TermUi>,
    interactive: bool,
}

impl TermPrompter {
    pub(crate) fn new(ui: Arc<TermUi>, interactive: bool) -> Self {
        Self { ui, interactive }
    }
}

/// A command longer than this many characters is counted where it is asked.
const LONG: usize = 500;

/// The command, then what is asked, last, right above the answer: the screen
/// always shows what is asked, and how much command is above it.
fn question(req: &ApprovalRequest) -> String {
    let asked = format!(
        "Allow {}? {}",
        shown(&req.tool_name),
        shown(&req.description)
    );
    let answers = "[y]es / [a]lways / [N]o: ";
    let preview = req.input_preview.trim();
    if preview.is_empty() || preview == req.description.trim() {
        return format!("{asked}\n{answers}");
    }
    let command = visible(&squeezed(&inert(preview)));
    let (lines, chars) = (command.lines().count(), command.chars().count());
    let above = match (lines, chars > LONG) {
        (1, false) => String::new(),
        (1, true) => format!(" (1 line, {chars} characters above)"),
        (n, false) => format!(" ({n} lines above)"),
        (n, true) => format!(" ({n} lines, {chars} characters above)"),
    };
    let command = command.replace('\n', "\n  ");
    format!("  {command}\n{asked}{above}\n{answers}")
}

/// `text` as the terminal shows it, each further line indented under the
/// first.
fn shown(text: &str) -> String {
    visible(&inert(text)).replace('\n', "\n  ")
}

/// How a tab is shown: the word break bash makes of it, in notation that is
/// not shell syntax (`\t` would read as the letter `t`).
const TAB: &str = "⟨tab⟩";

/// `text`, [`inert`] already, with nothing in it that passes for a space or
/// draws as nothing: a tab as [`TAB`], and any other whitespace but a space
/// or a newline, and every [`blank`] character, as its escape.
fn visible(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push_str(TAB),
            c if (c.is_whitespace() && !c.is_ascii()) || blank(c) => {
                out.extend(c.escape_unicode());
            }
            c => out.push(c),
        }
    }
    out
}

/// Not whitespace, yet drawn as blank cells or as nothing: the Hangul
/// fillers, the blank braille pattern, and the default-ignorable code points
/// [`hides`](crate::render::hides) leaves out.
fn blank(c: char) -> bool {
    matches!(c,
        '\u{00ad}' | '\u{034f}' | '\u{115f}' | '\u{1160}' | '\u{17b4}' | '\u{17b5}'
        | '\u{180b}'..='\u{180f}' | '\u{206a}'..='\u{206f}' | '\u{2800}' | '\u{3164}'
        | '\u{fe00}'..='\u{fe0f}' | '\u{ffa0}' | '\u{fff0}'..='\u{fff8}'
        | '\u{1bca0}'..='\u{1bca3}' | '\u{1d159}' | '\u{1d173}'..='\u{1d17a}'
        | '\u{e0000}'..='\u{e0fff}')
}

/// Spaces and tabs: what pads a command.
fn pads(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// `text` with its padding counted instead of drawn, so no padding can push
/// the rest of a command off the screen: more than 2 blank lines in a row as
/// one `⟨n blank lines⟩` line, and more than 8 spaces or tabs in a row as
/// `⟨n spaces⟩`. Bash does not care how much space separates its words, and
/// inside quotes the count says exactly what is there.
fn squeezed(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut blank: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        if line.chars().all(pads) {
            blank.push(line);
            continue;
        }
        blank_lines(&mut out, &mut blank);
        out.push(counted_padding(line));
    }
    blank_lines(&mut out, &mut blank);
    out.join("\n")
}

/// A run of blank lines into `out`, as it is or counted.
fn blank_lines(out: &mut Vec<String>, run: &mut Vec<&str>) {
    if run.len() > 2 {
        out.push(format!("⟨{} blank lines⟩", run.len()));
    } else {
        out.extend(run.iter().map(|l| counted_padding(l)));
    }
    run.clear();
}

/// A line's indentation up to this many columns is drawn as it is: a
/// script's nesting reads as written, and no more than a row of it can stand
/// before a line.
const INDENT: usize = 40;

/// `line` with its indentation past [`INDENT`] columns, and each run of more
/// than 8 spaces or tabs inside it, counted.
fn counted_padding(line: &str) -> String {
    let body = line.trim_start_matches(pads);
    let indent = &line[..line.len() - body.len()];
    let tab = TAB.chars().count();
    let width: usize = indent
        .chars()
        .map(|c| if c == '\t' { tab } else { 1 })
        .sum();
    let mut out = if width <= INDENT {
        indent.to_owned()
    } else {
        counted(indent)
    };
    let mut run = String::new();
    for c in body.chars() {
        if pads(c) {
            run.push(c);
        } else {
            out.push_str(&counted(&run));
            run.clear();
            out.push(c);
        }
    }
    out + &counted(&run)
}

/// A run of spaces and tabs: as it is up to 8, counted past that.
fn counted(run: &str) -> String {
    let n = run.chars().count();
    if n <= 8 {
        run.to_owned()
    } else if !run.contains('\t') {
        format!("⟨{n} spaces⟩")
    } else if !run.contains(' ') {
        format!("⟨{n} tabs⟩")
    } else {
        format!("⟨{n} spaces and tabs⟩")
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
        // Nothing else reaches the screen until it is answered, or given up
        // (the read dropped: the guard lets go then too).
        let answering = self.ui.answering();
        self.ui.ask(&question(req));
        let line = ask::read_line().await;
        answering.answered(line.as_deref());
        answer(line.as_deref())
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
            let raw: Vec<char> = q
                .chars()
                .filter(|&c| crate::render::hides(c) || (c.is_control() && c != '\n'))
                .collect();
            assert!(raw.is_empty(), "{raw:?} in {q:?}");
        }
        assert!(drawn_over.contains("touch PWNED"), "{drawn_over}");
        assert!(padded.contains("; touch PWNED"), "{padded}");
    }

    /// The rows `text` takes on a terminal `cols` wide.
    fn rows(text: &str, cols: usize) -> usize {
        text.split('\n')
            .map(|l| l.chars().count().div_ceil(cols).max(1))
            .sum()
    }

    /// Padding cannot push a command's first line, or what is asked, off the
    /// screen: the whole question fits in a few rows, and asks last.
    #[test]
    fn padding_cannot_push_the_command_or_the_question_off_the_screen() {
        let blank_lines = format!("touch PWNED3{}rm -rf ./build", "\n".repeat(41));
        let blank_spaced = format!("touch PWNED4{}rm -rf ./build", "\n \t".repeat(41));
        let spaces = format!("touch PWNED5;{}rm -rf ./build", " ".repeat(3000));
        let tabs = format!("touch PWNED6;{}rm -rf ./build", " \t".repeat(1500));
        for command in [blank_lines, blank_spaced, spaces, tabs] {
            let q = question(&ask(&command));
            assert!(rows(&q, 120) <= 6, "{} rows: {q}", rows(&q, 120));
            assert!(q.contains("touch PWNED"), "{q}");
            assert!(q.contains("rm -rf ./build"), "{q}");
            let last: Vec<&str> = q.rsplit('\n').take(2).collect();
            assert!(last[1].starts_with("Allow Bash? "), "asked last: {q}");
        }
        // A space that is not one shows as its escape, so it draws no blank
        // space: what is asked says how much is above it.
        let q = question(&ask(&format!(
            "touch PWNED7;{}rm -rf ./build",
            " \u{a0}".repeat(1500)
        )));
        let asked = q.rsplit('\n').nth(1).unwrap();
        assert!(
            asked.ends_with("(1 line, 10527 characters above)"),
            "{asked}"
        );
        assert!(q.contains(r"\u{a0}"), "{q}");
    }

    /// A command too long to read at a glance says so where it is asked.
    #[test]
    fn the_question_says_how_much_command_is_above_it() {
        let q = question(&ask("cd ./build\nrm -rf ./out\nls"));
        assert!(q.contains("(3 lines above)"), "{q}");
        let q = question(&ask(&format!("echo {}", "ab".repeat(400))));
        assert!(q.contains("(1 line, 805 characters above)"), "{q}");
        let q = question(&ask("rm -rf ./build"));
        assert!(!q.contains("above"), "{q}");
    }

    /// A character that draws blank but is not whitespace shows as its
    /// escape, so it cannot pad a command unseen.
    #[test]
    fn a_blank_drawing_character_shows_as_its_escape() {
        for pad in ['\u{2800}', '\u{3164}', '\u{ffa0}', '\u{115f}'] {
            let command = format!(
                "touch PWNED6 #{}\nrm -rf ./build",
                pad.to_string().repeat(3000)
            );
            let q = question(&ask(&command));
            assert!(!q.contains(pad), "{pad:?} raw in the question");
            assert!(q.contains(&format!("{}", pad.escape_unicode())), "{q}");
        }
    }

    /// A tab shows as the word break bash makes of it, never as `\t`, which
    /// bash would read as the letter `t`.
    #[test]
    fn a_tab_shows_as_the_word_break_it_is() {
        let q = question(&ask("rm -rf ./build\t~"));
        assert!(q.contains("./build⟨tab⟩~"), "{q}");
        assert!(!q.contains(r"\t"), "{q}");
    }

    /// A script's nesting reads as written; only indentation that could
    /// push the rest off the screen is counted.
    #[test]
    fn a_scripts_indentation_reads_as_written() {
        let script = "for d in */; do\n    if [ -d \"$d/build\" ]; then\n        if [ -f \"$d/Makefile\" ]; then\n            rm -rf \"$d/build\"\n        fi\n    fi\ndone";
        let q = question(&ask(script));
        assert!(q.contains("\n              rm -rf \"$d/build\"\n"), "{q}");
        let q = question(&ask(&format!(
            "touch PWNED\n{}rm -rf ./build",
            " ".repeat(3000)
        )));
        assert!(q.contains("⟨3000 spaces⟩rm -rf ./build"), "{q}");
    }
}
