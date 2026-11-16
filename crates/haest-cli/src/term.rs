//! The terminal as haest's [`Ui`]: lines on stderr (or `notice` events with
//! --json), subagents under their names, and the one reader of stdin at a time
//! (the REPL prompt, or one approval ask).

use std::{
    io::{Stderr, Stdout},
    sync::{Arc, Mutex},
};

use haest::{core::remedy::Limit, runtime::Ui, tools::agents::SubagentEvent};

use crate::{
    agents::AgentLog,
    render::{Level, Renderer},
};

pub type Term = Renderer<Stdout, Stderr>;

/// A conversation as the CLI runs it.
pub type Runtime = haest::runtime::Runtime<TermUi>;

pub struct TermUi {
    pub term: Arc<Mutex<Term>>,
    /// Who reads the terminal: the REPL prompt, or one approval ask at a time.
    pub stdin: tokio::sync::Mutex<()>,
    log: Mutex<AgentLog>,
}

impl TermUi {
    pub fn new(json: bool) -> Self {
        Self {
            term: Arc::new(Mutex::new(Renderer::terminal(json))),
            stdin: tokio::sync::Mutex::new(()),
            log: Mutex::default(),
        }
    }

    /// A question answered on the next line (see `ask.rs`).
    pub fn ask(&self, question: &str) {
        self.term.lock().unwrap().ask(question);
    }

    /// The REPL prompt is open (a line printed now lands above it): an
    /// arrival waits for Enter, an approval asks above the prompt.
    pub fn prompting(&self) -> bool {
        self.term.lock().unwrap().prompting()
    }

    fn notice(&self, level: Level, text: &str) {
        self.term.lock().unwrap().notice(level, text);
    }
}

impl Ui for TermUi {
    fn info(&self, text: &str) {
        self.notice(Level::Info, text);
    }

    fn warn(&self, text: &str) {
        self.notice(Level::Warning, text);
    }

    fn error(&self, text: &str) {
        self.notice(Level::Error, text);
    }

    fn remedy(&self, limit: Limit) -> Option<String> {
        crate::remedy::remedy(limit)
    }

    fn arrival(&self, headline: &str) {
        if self.prompting() {
            self.info(&format!("{headline}\nhaest takes it when you press Enter."));
        } else {
            self.info(headline);
        }
    }

    fn subagent(&self, id: &str, event: &SubagentEvent) {
        if let Some((level, text)) = self.log.lock().unwrap().event(id, event) {
            self.notice(level, &text);
        }
    }

    fn subagents_running(&self, ids: &[String]) {
        if let Some(text) = self.log.lock().unwrap().activity(ids.len()) {
            self.notice(Level::Info, &text);
        }
    }
}
