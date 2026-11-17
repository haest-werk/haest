//! The terminal as haest's [`Ui`]: every line it is told goes into the
//! session's stream (a `notice`, an `ask`, a subagent's frames), and the
//! terminal shows that stream ([`show`]): lines on stderr, or every frame as
//! a JSON line with --json. It is also the one reader of stdin at a time (the
//! REPL prompt, or one approval ask).

use std::{
    io::{Stderr, Stdout},
    sync::{Arc, Mutex},
};

use haest::{
    core::remedy::Limit,
    runtime::{HubUi, Ui},
    session::{Hub, Subscription},
    tools::SubagentEvent,
    wire::{Outbound, Path},
};
use tokio::task::JoinHandle;

use crate::{agents::AgentLog, render::Renderer};

pub(crate) type Term = Renderer<Stdout, Stderr>;

/// A conversation as the CLI runs it: the CLI shares its UI with the
/// approval prompter, the control socket and the broker link.
pub(crate) type Runtime = haest::runtime::Runtime<Arc<TermUi>>;

pub(crate) struct TermUi {
    pub term: Arc<Mutex<Term>>,
    /// Who reads the terminal: the REPL prompt, or one approval ask at a time.
    pub stdin: tokio::sync::Mutex<()>,
    /// The runtime's lines into the stream the terminal shows: notices and
    /// subagent frames, as any session's.
    lines: HubUi,
    log: Mutex<AgentLog>,
    /// The human types slash commands here (not when a parent drives haest
    /// over stdio): a remedy may name one.
    slash: bool,
}

impl TermUi {
    /// A terminal over a stream of its own (a new id, the default ring).
    #[cfg(test)]
    pub(crate) fn new(json: bool) -> Self {
        let sid = uuid::Uuid::new_v4().to_string();
        Self::with_hub(json, Hub::new(sid))
    }

    /// A terminal over `hub`: the stream the session publishes into too.
    pub(crate) fn with_hub(json: bool, hub: Hub) -> Self {
        Self {
            term: Arc::new(Mutex::new(Renderer::terminal(json))),
            stdin: tokio::sync::Mutex::new(()),
            lines: HubUi::new(hub),
            log: Mutex::default(),
            slash: true,
        }
    }

    /// No terminal: a parent process drives haest over stdio, and reads
    /// every line as a frame.
    pub(crate) fn over_stdio(mut self) -> Self {
        self.slash = false;
        self
    }

    /// A question answered on the next line (see `ask.rs`).
    pub(crate) fn ask(&self, question: &str) {
        let text = question.to_string();
        self.lines
            .hub()
            .publish(Path::main(), Outbound::Ask { text });
    }

    /// The REPL prompt is open (a line printed now lands above it): an
    /// arrival waits for Enter, an approval asks above the prompt.
    pub(crate) fn prompting(&self) -> bool {
        self.term.lock().unwrap().prompting()
    }
}

/// The terminal shows `sub`, frame by frame, until the stream closes.
pub(crate) fn show(term: Arc<Mutex<Term>>, mut sub: Subscription) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(frame) = sub.recv().await {
            term.lock().unwrap().frame(&frame);
        }
    })
}

/// The runtime's lines as any session's ([`HubUi`]); the terminal adds the
/// remedies, where an arrival waits, and what the subagents are doing.
impl Ui for TermUi {
    fn info(&self, text: &str) {
        self.lines.info(text);
    }

    fn warn(&self, text: &str) {
        self.lines.warn(text);
    }

    fn error(&self, text: &str) {
        self.lines.error(text);
    }

    fn remedy(&self, limit: Limit) -> Option<String> {
        crate::remedy::remedy(limit, self.slash)
    }

    fn arrival(&self, headline: &str) {
        if self.prompting() {
            self.info(&format!("{headline}\nhaest takes it when you press Enter."));
        } else {
            self.info(headline);
        }
    }

    fn subagent(&self, id: &str, event: &SubagentEvent) {
        self.lines.subagent(id, event);
    }

    fn subagents_running(&self, ids: &[String]) {
        if let Some(text) = self.log.lock().unwrap().activity(ids.len()) {
            self.lines.info(&text);
        }
    }
}
