//! The interactive conversation: a line editor with history, slash commands,
//! one turn per line. Ctrl+D (or /exit) quits; Ctrl+C at the prompt clears the line.

use rustyline::{DefaultEditor, error::ReadlineError};

use crate::{
    changes,
    commands::{Outcome, resolve},
    run::human,
    setup::Runtime,
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

pub async fn repl(rt: &mut Runtime, first: Option<String>) -> anyhow::Result<()> {
    let restored = rt.agent.messages.len();
    let workspace = rt.agent.deps().workspace.mode();
    rt.host.info(&format!(
        "kern {} -- {} -- {workspace} workspace -- session {}{}\n/help for commands, Ctrl+D to quit.",
        env!("CARGO_PKG_VERSION"),
        rt.agent.model_spec,
        rt.session_id,
        if restored > 0 {
            format!(" ({restored} messages restored)")
        } else {
            String::new()
        },
    ));
    let history = rt.host.cfg.data_dir.join("history");
    let mut editor = DefaultEditor::new()?;
    let _ = editor.load_history(&history);

    let mut go_on = match first {
        Some(p) => handle(rt, &p).await,
        None => true,
    };
    let mut warned = false;
    loop {
        // Quitting with sandbox edits nobody applied: say so once, quit on the next try.
        if !go_on {
            match changes::unapplied(rt).await {
                Some(lost) if !warned => {
                    rt.host
                        .warn(&format!("{lost}\n/apply them, or quit again to drop them."));
                    warned = true;
                }
                _ => break,
            }
        }
        let (ed, line) = tokio::task::spawn_blocking(move || {
            let line = editor.readline("> ");
            (editor, line)
        })
        .await?;
        editor = ed;
        go_on = match line {
            Ok(line) if line.trim().is_empty() => true,
            Ok(line) => {
                let _ = editor.add_history_entry(line.as_str());
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
