//! The interactive conversation: a line editor with history, slash commands,
//! one turn per line. Ctrl+D (or /exit) quits, once haest has said what that
//! drops (edits only in the sandbox, turns that wait), and Ctrl+D runs none
//! of those turns; Ctrl+C at the prompt clears the line.
//!
//! Turns the human did not start (`inbox.rs`) never run under an open prompt:
//! the prompt owns the terminal (an approval ask would fight it for stdin) and
//! output would land in the half-typed line. So the prompt holds the session
//! (no turn starts while it is open); one that arrives meanwhile is announced
//! ABOVE it (rustyline's external printer), and runs as soon as the prompt
//! returns: Enter on an empty line runs it at once; a typed line runs after
//! it, since it came first. Whatever arrives during a turn is steered into
//! it, and what is left runs right after.
//!
//! The conversation may end under the prompt: a peer with command access
//! closes it, or it fails. The prompt says so at once, with what leaving
//! would drop. The sandbox is haest's until it exits, so /changes and /apply
//! still work then; every other line, and Ctrl+D, leaves, guarded as a quit.

use std::sync::{Arc, Mutex, mpsc::Sender};

use haest::{
    runtime::Ui,
    session::{Hold, SessionError},
    wire::Level,
};
use rustyline::{DefaultEditor, ExternalPrinter, error::ReadlineError};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{
    changes::{self, Sandbox},
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

/// The prompt's line editor, and where a line goes while it waits for one.
struct Prompt {
    editor: Arc<Mutex<DefaultEditor>>,
    above: Option<Sender<String>>,
}

impl Prompt {
    /// The next line the human types. The terminal is the prompt's
    /// meanwhile (an approval ask waits for it to return), and a line printed
    /// now lands above it.
    async fn read(&self, ui: &TermUi) -> anyhow::Result<rustyline::Result<String>> {
        let terminal = ui.stdin.lock().await;
        ui.term.lock().unwrap().set_above(self.above.clone());
        let editor = self.editor.clone();
        let line = tokio::task::spawn_blocking(move || editor.lock().unwrap().readline("> ")).await;
        ui.term.lock().unwrap().set_above(None);
        drop(terminal);
        Ok(line?)
    }

    /// `line` in the history, for the arrow keys.
    fn remember(&self, line: &str) {
        let _ = self.editor.lock().unwrap().add_history_entry(line);
    }
}

/// The quit warning for changes only in the sandbox ([`Sandbox::unapplied`]).
fn drops(changes: &str) -> String {
    format!("{changes}\n/apply them, or quit again to drop them.")
}

/// What quitting now would lose ([`QuitGuard`] says when it is said).
async fn before_quitting(rt: &Runtime) -> Option<String> {
    let mut lost = Vec::new();
    if let Some(changes) = changes::unapplied(rt).await {
        lost.push(drops(&changes));
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

/// How the conversation at the prompt stopped.
enum Stopped {
    /// The human quit; the session is still held.
    Quit(Hold),
    /// The conversation ended under the prompt, with the line the human
    /// typed as it did, which it never took: Enter on an empty line and
    /// Ctrl+D come as an empty one, a wish to leave.
    Ended(Option<String>),
}

/// Whether `e` is the conversation's end.
fn ended(e: &anyhow::Error) -> bool {
    matches!(e.downcast_ref::<SessionError>(), Some(SessionError::Closed))
}

/// The conversation at the prompt, until the human quits or it ends.
async fn converse(
    session: &Session,
    ui: &TermUi,
    prompt: &Prompt,
    first: Option<String>,
) -> anyhow::Result<Stopped> {
    let mut go_on = match first {
        Some(p) => match handle(session, ui, &p).await {
            Err(e) if ended(&e) => return Ok(Stopped::Ended(None)),
            go_on => go_on?,
        },
        None => true,
    };
    let mut guard = QuitGuard::default();
    // Kept from a Ctrl+D to the quit it asks for: what waits starts only if
    // the human goes on.
    let mut held: Option<Hold> = None;
    // The human's last line until the conversation has taken it ([`Stopped::Ended`]).
    let mut untaken: Option<String> = None;
    loop {
        if go_on {
            guard.went_on();
            // Every turn that waits runs before the prompt opens.
            session.idle().await;
        }
        // No turn starts while the prompt is open, or while a quit is
        // decided; the terminal is the prompt's (an approval ask waits for
        // it to return).
        let hold = match held.take() {
            Some(hold) => hold,
            None => match session.hold().await {
                Ok(hold) => hold,
                Err(SessionError::Closed) => return Ok(Stopped::Ended(untaken)),
                Err(e) => return Err(e.into()),
            },
        };
        if !go_on {
            let lost = session
                .with(|rt| Box::pin(async move { before_quitting(rt).await }))
                .await;
            let lost = match lost {
                Ok(lost) => lost,
                Err(SessionError::Closed) => return Ok(Stopped::Ended(untaken)),
                Err(e) => return Err(e.into()),
            };
            match guard.quit(lost) {
                Some(lost) => ui.warn(&lost),
                None => return Ok(Stopped::Quit(hold)),
            }
        }
        let line = prompt.read(ui).await?;
        untaken = match &line {
            Ok(line) => Some(line.clone()),
            Err(ReadlineError::Eof) => Some(String::new()),
            Err(_) => None,
        };
        // Ctrl+D asks to quit: still held, so whether it may is decided
        // before anything that waits can start.
        if matches!(line, Err(ReadlineError::Eof)) {
            held = Some(hold);
        } else {
            drop(hold);
        }
        go_on = match line {
            Ok(line) if line.trim().is_empty() => true,
            Ok(line) => {
                prompt.remember(&line);
                // What arrived while the prompt was open came first.
                session.idle().await;
                match handle(session, ui, &line).await {
                    Err(e) if ended(&e) => return Ok(Stopped::Ended(untaken)),
                    go_on => {
                        untaken = None;
                        go_on?
                    }
                }
            }
            Err(ReadlineError::Interrupted) => true,
            Err(ReadlineError::Eof) => false,
            Err(e) => return Err(e.into()),
        };
    }
}

/// What the prompt says the moment the conversation ends under it, without
/// waiting for Enter: that it ended, and what leaving would drop.
async fn say_the_end(ui: Arc<TermUi>, sandbox: Option<Sandbox>) {
    const ENDED: &str = "The conversation has ended: haest takes no more turns.";
    ui.shown().await;
    let lost = match &sandbox {
        Some(sandbox) => sandbox.unapplied().await,
        None => None,
    };
    let text = match lost {
        Some(lost) => format!("{ENDED}\n{lost}\n/apply them before you leave."),
        None => format!("{ENDED} Press Enter to leave."),
    };
    ui.tell_after_the_end(Level::Warning, &text).await;
}

/// The conversation has ended under the prompt: haest takes no more turns,
/// but the sandbox is haest's until it exits, so /changes and /apply still
/// work. Every other line, and Ctrl+D, leaves, held back once while that
/// would drop changes ([`QuitGuard`]). `line` was typed as it ended.
async fn after_the_end(
    ui: &TermUi,
    prompt: &Prompt,
    sandbox: Option<&Sandbox>,
    mut line: Option<String>,
) -> anyhow::Result<()> {
    let mut guard = QuitGuard::default();
    loop {
        let read = match line.take() {
            Some(line) => Ok(line),
            None => prompt.read(ui).await?,
        };
        let typed = match read {
            Ok(typed) => typed,
            Err(ReadlineError::Interrupted) => {
                guard.went_on();
                continue;
            }
            Err(ReadlineError::Eof) => String::new(),
            Err(e) => return Err(e.into()),
        };
        let command = typed.split_whitespace().next().unwrap_or_default();
        let said = match (command, sandbox) {
            ("/changes", Some(sandbox)) => Some((Level::Info, sandbox.changes().await)),
            ("/apply", Some(sandbox)) => match sandbox.apply().await {
                (text, true) => Some((Level::Warning, text)),
                (text, false) => Some((Level::Info, text)),
            },
            ("/changes" | "/apply", None) => Some((Level::Info, changes::REAL_FS.to_string())),
            _ => None,
        };
        if let Some((level, text)) = said {
            prompt.remember(&typed);
            guard.went_on();
            ui.tell_after_the_end(level, &text).await;
            continue;
        }
        let lost = match sandbox {
            Some(sandbox) => sandbox.unapplied().await,
            None => None,
        };
        match guard.quit(lost.as_deref().map(drops)) {
            Some(lost) => ui.tell_after_the_end(Level::Warning, &lost).await,
            None => return Ok(()),
        }
    }
}

/// A task stopped when this drops.
struct Stops(JoinHandle<()>);

impl Drop for Stops {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The conversation at the prompt, until the human quits. The session is
/// still held then: the caller lets go of the [`Hold`] only once the session
/// is closed, so nothing that waited starts on the way out. When the
/// conversation ends under the prompt, the human has the sandbox until they
/// leave ([`after_the_end`]), and then it is [`SessionError::Closed`].
pub(crate) async fn repl(
    session: &Session,
    ui: &Arc<TermUi>,
    first: Option<String>,
    sandbox: Option<Sandbox>,
) -> anyhow::Result<Hold> {
    // Said only while the human is here: stopped when this returns, before
    // the human's quit closes the conversation.
    let _end = Stops(tokio::spawn(say_the_end(ui.clone(), sandbox.clone())));
    // None: the conversation ended before the prompt first opened.
    let history = match session
        .with(|rt| Box::pin(async move { welcome(rt) }))
        .await
    {
        Ok(history) => Some(history),
        Err(SessionError::Closed) => None,
        Err(e) => return Err(e.into()),
    };
    let mut editor = DefaultEditor::new()?;
    if let Some(history) = &history {
        let _ = editor.load_history(history);
    }
    let above = above_prompt(&mut editor);
    let prompt = Prompt {
        editor: Arc::new(Mutex::new(editor)),
        above,
    };
    let stopped = match history {
        Some(_) => converse(session, ui, &prompt, first).await,
        None => Ok(Stopped::Ended(None)),
    };
    let quit = match stopped {
        Ok(Stopped::Quit(hold)) => Ok(hold),
        Ok(Stopped::Ended(line)) => after_the_end(ui, &prompt, sandbox.as_ref(), line)
            .await
            .and(Err(SessionError::Closed.into())),
        Err(e) => Err(e),
    };
    if let Some(history) = &history {
        if let Some(dir) = history.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = prompt.editor.lock().unwrap().save_history(history);
    }
    quit
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
