//! THE INBOX: turns the human did not start -- a background job that exited, a
//! subagent's message for main, a background subagent's answer. The TS kern's
//! `host/turns.ts`, for a terminal:
//!   - while a turn runs, what arrives is steered into it (read at its next step);
//!   - what that turn left unread, and what arrived after it, run as the NEXT
//!     turn -- one at a time, never two at once -- through the ping-pong gate
//!     (KERN_MAX_AGENT_TURNS); held items ride in with the human's next message;
//!   - nothing is dropped: an item waits here until a turn takes it.
//!
//! A host with an input line never runs one under an open prompt: the
//! [`Ui`](crate::Ui) announces it and the host runs it when the prompt returns.

use std::{collections::VecDeque, sync::Arc};

use crate::{
    Host,
    kern::{
        AgentControl,
        tools::{AgentTurnGate, Inbox},
    },
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// The sending end the jobs and the subagent hub hold: queues, and tells the human.
pub struct InboxSender {
    tx: UnboundedSender<String>,
    host: Arc<Host>,
}

/// What the human sees of an arrival: its first line, cut short.
fn headline(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut head: String = line.chars().take(120).collect();
    if head.len() < line.len() {
        head.push_str("...");
    }
    head
}

impl Inbox for InboxSender {
    fn submit(&self, text: String) {
        self.host.ui.arrival(&headline(&text));
        let _ = self.tx.send(text);
    }
}

/// The receiving end, with the gate every turn the human did not start passes.
pub struct InboxQueue {
    rx: UnboundedReceiver<String>,
    /// Received while no turn ran, not yet taken (print mode's wait).
    ready: VecDeque<String>,
    gate: AgentTurnGate,
}

/// The conversation's inbox: the end jobs and agents submit to, the end its turns take from.
pub fn inbox(host: &Arc<Host>, max_agent_turns: u32) -> (Arc<InboxSender>, InboxQueue) {
    let (tx, rx) = unbounded_channel();
    let notice = host.clone();
    let gate = AgentTurnGate::new(max_agent_turns, move |text| notice.warn(text));
    let sender = InboxSender {
        tx,
        host: host.clone(),
    };
    (Arc::new(sender), InboxQueue::new(rx, gate))
}

impl InboxQueue {
    pub fn new(rx: UnboundedReceiver<String>, gate: AgentTurnGate) -> Self {
        Self {
            rx,
            ready: VecDeque::new(),
            gate,
        }
    }

    /// The human posted: anything held rides in ahead of their text.
    pub fn human(&mut self, text: &str) -> String {
        self.gate.human(text)
    }

    /// A turn the human did not start: the text to run it with, or None (held).
    pub fn agent_turn(&mut self, text: &str) -> Option<String> {
        self.gate.agent_turn(text)
    }

    /// The next arrival (a turn running steers it in).
    pub async fn arrival(&mut self) -> Option<String> {
        match self.ready.pop_front() {
            Some(text) => Some(text),
            None => self.rx.recv().await,
        }
    }

    /// Wait for an arrival and keep it for [`Self::take`].
    pub async fn wait(&mut self) {
        if let Some(text) = self.rx.recv().await {
            self.ready.push_back(text);
        }
    }

    /// Everything waiting, oldest first, as one turn's input: what the last
    /// turn left unread, then what arrived since. None when nothing waits.
    pub fn take(&mut self, control: &AgentControl) -> Option<String> {
        let mut parts: Vec<String> = control.take_queued().into_iter().collect();
        parts.extend(self.ready.drain(..));
        while let Ok(text) = self.rx.try_recv() {
            parts.push(text);
        }
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    /// Items that arrived and wait for a turn (not counting a turn's unread steers).
    pub fn waiting(&self) -> usize {
        self.ready.len() + self.rx.len()
    }
}

#[cfg(test)]
mod tests;
