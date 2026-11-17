//! The interactive conversation: a line editor with history, slash commands,
//! one turn per line. Ctrl+D (or /exit) quits; Ctrl+C at the prompt clears the line.
//!
//! Turns the human did not start (`inbox.rs`) never run under an open prompt:
//! the prompt owns the terminal (an approval ask would fight it for stdin) and
//! output would land in the half-typed line. So the prompt holds the session
//! (no turn starts while it is open); one that arrives meanwhile is announced
//! ABOVE it (rustyline's external printer), and runs as soon as the prompt
//! returns: Enter on an empty line runs it at once; a typed line runs after
//! it, since it came first. Whatever arrives during a turn is steered into
//! it, and what is left runs right after.

use std::sync::mpsc::Sender;

use haest::runtime::Ui;
use rustyline::{DefaultEditor, ExternalPrinter, error::ReadlineError};
use tokio_util::sync::CancellationToken;

use crate::{
    changes,
    commands::{Outcome, resolve},
    run::{self, Session},
    term::{Runtime, TermUi},
};

/// One line the human typed: a slash command runs on the conversation; text
/// goes as the human's turn, after the turns that waited. False = quit.
/// Ctrl+C gives up a command that does not end (a mod's that never answers).
async fn handle(session: &Session, ui: &TermUi, line: &str) -> anyhow::Result<bool> {
    // Held, so the line is queued before a turn could start that it would
    // be steered into.
    let hold = session.hold().await?;
    let line = line.to_string();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let resolved = session.with(move |rt| Box::pin(async move { resolve(rt, &line, &stop).await }));
    let Some(outcome) = run::unless_interrupted(&cancel, resolved).await else {
        ui.warn("Stopped the command.");
        return Ok(true);
    };
    let outcome = outcome?;
    match outcome {
        Outcome::Send(text) => {
            let turn = session.send_input(haest::session::Input::human(text))?;
            drop(hold);
            // Its end, or the gate's refusal: either way the prompt is back.
            let _ = turn.await;
            Ok(true)
        }
        Outcome::Done => Ok(true),
        Outcome::Exit => Ok(false),
    }
}

/// Where a line goes while the prompt waits: rustyline prints it above the
/// prompt and redraws the half-typed line below. A thread of its own, since a
/// print may wait on the editor. None when the terminal cannot (lines then go
/// to stderr as ever).
fn above_prompt(editor: &mut DefaultEditor) -> Option<Sender<String>> {
    let mut printer = editor.create_external_printer().ok()?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in rx {
            if printer.print(line).is_err() {
                break;
            }
        }
    });
    Some(tx)
}

/// What quitting now would lose ([`QuitGuard`] says when it is said).
async fn before_quitting(rt: &Runtime) -> Option<String> {
    let mut lost = Vec::new();
    if let Some(changes) = changes::unapplied(rt).await {
        lost.push(format!(
            "{changes}\n/apply them, or quit again to drop them."
        ));
    }
    let waiting = rt.inbox().waiting();
    if waiting > 0 {
        lost.push(format!(
            "{waiting} message(s) for haest have not run yet: press Enter to run them, or quit again to drop them."
        ));
    }
    (!lost.is_empty()).then(|| lost.join("\n"))
}

/// Whether a quit goes through: one that would lose something is held back
/// with the warning, and the next quit goes through -- unless what it would
/// lose changed since (more sandbox edits, a message arrived) or the human
/// went on meanwhile, which hold it back again.
#[derive(Default)]
struct QuitGuard {
    /// The warning the human saw, while it still stands.
    warned: Option<String>,
}

impl QuitGuard {
    /// The human quits, and `lost` is what that would lose: Some(warning) =
    /// stay, and say it; None = quit.
    fn quit(&mut self, lost: Option<String>) -> Option<String> {
        let lost = lost.filter(|l| self.warned.as_ref() != Some(l))?;
        self.warned = Some(lost.clone());
        Some(lost)
    }

    /// The human did something else: the warning no longer stands.
    fn went_on(&mut self) {
        self.warned = None;
    }
}

/// The first line on the terminal, and where the line history is kept.
fn welcome(rt: &Runtime) -> std::path::PathBuf {
    let restored = rt.agent().history().len();
    let workspace = rt.agent().deps().shared().workspace().mode();
    rt.hooks().info(&format!(
        "haest {} -- {} -- {workspace} workspace -- session {}{}\n/help for commands, Ctrl+D to quit.",
        env!("CARGO_PKG_VERSION"),
        rt.agent().model_spec(),
        rt.session_id(),
        if restored > 0 {
            format!(" ({restored} messages restored)")
        } else {
            String::new()
        },
    ));
    rt.hooks().settings().data_dir.join("history")
}

pub(crate) async fn repl(
    session: &Session,
    ui: &TermUi,
    first: Option<String>,
) -> anyhow::Result<()> {
    let history = session
        .with(|rt| Box::pin(async move { welcome(rt) }))
        .await?;
    let mut editor = DefaultEditor::new()?;
    let _ = editor.load_history(&history);
    let above = above_prompt(&mut editor);

    let mut go_on = match first {
        Some(p) => handle(session, ui, &p).await?,
        None => true,
    };
    let mut guard = QuitGuard::default();
    loop {
        if go_on {
            guard.went_on();
            // Every turn that waits runs before the prompt opens.
            session.idle().await;
        } else {
            let lost = session
                .with(|rt| Box::pin(async move { before_quitting(rt).await }))
                .await?;
            match guard.quit(lost) {
                Some(lost) => ui.warn(&lost),
                None => break,
            }
        }
        // No turn starts while the prompt is open; the terminal is the
        // prompt's (an approval ask waits for it to return).
        let hold = session.hold().await?;
        let terminal = ui.stdin.lock().await;
        ui.term.lock().unwrap().set_above(above.clone());
        let (ed, line) = tokio::task::spawn_blocking(move || {
            let line = editor.readline("> ");
            (editor, line)
        })
        .await?;
        ui.term.lock().unwrap().set_above(None);
        drop(terminal);
        drop(hold);
        editor = ed;
        go_on = match line {
            Ok(line) if line.trim().is_empty() => true,
            Ok(line) => {
                let _ = editor.add_history_entry(line.as_str());
                // What arrived while the prompt was open came first.
                session.idle().await;
                handle(session, ui, &line).await?
            }
            Err(ReadlineError::Interrupted) => true,
            Err(ReadlineError::Eof) => false,
            Err(e) => return Err(e.into()),
        };
    }
    if let Some(dir) = history.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = editor.save_history(&history);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quit_that_would_lose_what_the_human_was_not_told_of_is_held_back() {
        let one = || Some("1 change(s) only in the sandbox: ~ a.rs".to_string());
        let two = || Some("2 change(s) only in the sandbox: ~ a.rs, + b.rs".to_string());
        let mut q = QuitGuard::default();
        assert_eq!(q.quit(None), None, "nothing to lose: it quits");
        assert_eq!(q.quit(one()), one(), "held back, with the warning");
        assert_eq!(
            q.quit(two()),
            two(),
            "more would be lost since: held back again"
        );
        assert_eq!(q.quit(two()), None, "quit again: it goes");

        let mut q = QuitGuard::default();
        assert_eq!(q.quit(one()), one());
        q.went_on();
        assert_eq!(q.quit(one()), one(), "the human went on after the warning");
    }
}
