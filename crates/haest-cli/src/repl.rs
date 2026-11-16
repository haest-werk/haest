//! The interactive conversation: a line editor with history, slash commands,
//! one turn per line. Ctrl+D (or /exit) quits; Ctrl+C at the prompt clears the line.
//!
//! Turns the human did not start (`inbox.rs`) never run under an open prompt:
//! the prompt owns the terminal (an approval ask would fight it for stdin) and
//! output would land in the half-typed line. So one that arrives while the
//! prompt waits is announced ABOVE it (rustyline's external printer), and runs
//! as soon as the prompt returns: Enter on an empty line runs it at once; a
//! typed line runs after it, since it came first. Whatever arrives during a
//! turn is steered into it, and what is left runs right after.

use std::sync::mpsc::Sender;

use rustyline::{DefaultEditor, ExternalPrinter, error::ReadlineError};

use crate::{
    changes,
    commands::{Outcome, resolve},
    run::{agent_turns, human},
    term::Runtime,
};

async fn handle(rt: &mut Runtime, line: &str) -> bool {
    match resolve(rt, line).await {
        Outcome::Send(text) => {
            human(rt, &text).await;
            true
        }
        Outcome::Done => true,
        Outcome::Exit => false,
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
    let waiting = rt.inbox.waiting();
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

pub async fn repl(rt: &mut Runtime, first: Option<String>) -> anyhow::Result<()> {
    let restored = rt.agent.messages.len();
    let workspace = rt.agent.deps().workspace.mode();
    rt.host.info(&format!(
        "haest {} -- {} -- {workspace} workspace -- session {}{}\n/help for commands, Ctrl+D to quit.",
        env!("CARGO_PKG_VERSION"),
        rt.agent.model_spec,
        rt.session_id(),
        if restored > 0 {
            format!(" ({restored} messages restored)")
        } else {
            String::new()
        },
    ));
    let history = rt.host.settings.data_dir.join("history");
    let mut editor = DefaultEditor::new()?;
    let _ = editor.load_history(&history);
    let above = above_prompt(&mut editor);

    let mut go_on = match first {
        Some(p) => handle(rt, &p).await,
        None => true,
    };
    let mut guard = QuitGuard::default();
    loop {
        if go_on {
            guard.went_on();
            agent_turns(rt).await;
        } else {
            match guard.quit(before_quitting(rt).await) {
                Some(lost) => rt.host.warn(&lost),
                None => break,
            }
        }
        // The terminal is the prompt's: an approval ask waits for it to return.
        let ui = rt.ui.clone();
        let terminal = ui.stdin.lock().await;
        ui.term.lock().unwrap().set_above(above.clone());
        let (ed, line) = tokio::task::spawn_blocking(move || {
            let line = editor.readline("> ");
            (editor, line)
        })
        .await?;
        ui.term.lock().unwrap().set_above(None);
        drop(terminal);
        editor = ed;
        go_on = match line {
            Ok(line) if line.trim().is_empty() => true,
            Ok(line) => {
                let _ = editor.add_history_entry(line.as_str());
                agent_turns(rt).await;
                handle(rt, &line).await
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
