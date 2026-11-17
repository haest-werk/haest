//! The inbox: [`inbox`] makes its two ends, the [`InboxSender`] the jobs and
//! the subagents submit to and the [`InboxQueue`] the turns take from.

use std::{collections::VecDeque, fmt};

use crate::{
    RuntimeHooks,
    haest::{
        AgentControl,
        tools::{AgentTurnGate, Inbox},
    },
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// The sending end the jobs and the subagent hub hold: queues, and tells the human.
///
/// Its [`Inbox::submit`] queues the text for main and shows the human its
/// first line, cut short ([`Ui::arrival`](crate::Ui::arrival)). Made by
/// [`inbox`]; clones send to the same queue.
#[derive(Clone)]
pub(crate) struct InboxSender {
    tx: UnboundedSender<String>,
    hooks: RuntimeHooks,
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
        self.hooks.ui().arrival(&headline(&text));
        let _ = self.tx.send(text);
    }
}

/// The conversation's inbox, the end its turns take from, with the gate every
/// turn the human did not start passes ([`Runtime::inbox`](crate::Runtime::inbox)).
///
/// It holds the turns the human did not start: a background job that exited,
/// a subagent's message for main, a background subagent's answer. As the TS
/// haest's `host/turns.ts`, for a terminal:
///   - while a turn runs, what arrives is steered into it (read at its next step);
///   - what that turn left unread, and what arrived after it, run as the NEXT
///     turn -- one at a time, never two at once -- through the ping-pong gate
///     (the `max_agent_turns` setting); held items ride in with the human's
///     next message;
///   - nothing is dropped: an item waits here until a turn takes it.
///
/// A host with an input line never runs one under an open prompt: the
/// [`Ui`](crate::Ui) announces it and the host runs it when the prompt returns.
///
/// [`Runtime`](crate::Runtime)'s turn methods read it for the host. A host
/// that runs its own loop reads it two ways: while a turn runs,
/// [`arrival`](Self::arrival) gives each item as it comes, to steer into the
/// turn; between turns, [`take`](Self::take) gives everything waiting as one
/// input, to run through [`agent_turn`](Self::agent_turn). The human's own
/// text goes through [`human`](Self::human), which ends the gate's count.
pub struct InboxQueue {
    rx: UnboundedReceiver<String>,
    /// Received while no turn ran, not yet taken (print mode's wait).
    ready: VecDeque<String>,
    gate: AgentTurnGate,
}

/// How many wait, and the gate.
impl fmt::Debug for InboxQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InboxQueue")
            .field("waiting", &self.waiting())
            .field("gate", &self.gate)
            .finish_non_exhaustive()
    }
}

/// The conversation's inbox: the [`InboxSender`] the jobs and the subagents
/// submit to, and the [`InboxQueue`] the turns take from.
///
/// `max_agent_turns` is the gate's cap: turns in a row with no message from
/// the human before the rest are held (0 = no cap). The human hears through
/// `hooks` when the gate starts holding.
pub(crate) fn inbox(hooks: &RuntimeHooks, max_agent_turns: u32) -> (InboxSender, InboxQueue) {
    let (tx, rx) = unbounded_channel();
    let notice = hooks.clone();
    let gate = AgentTurnGate::new(max_agent_turns, move |text| notice.warn(text))
        .with_remedies(hooks.remedies());
    let sender = InboxSender {
        tx,
        hooks: hooks.clone(),
    };
    (sender, InboxQueue::new(rx, gate))
}

impl InboxQueue {
    /// A queue over `rx` with `gate`; [`inbox`] makes one with its sender.
    pub(crate) fn new(rx: UnboundedReceiver<String>, gate: AgentTurnGate) -> Self {
        Self {
            rx,
            ready: VecDeque::new(),
            gate,
        }
    }

    /// The human posted: the gate's count starts over, and anything it held
    /// rides in ahead of their text. Returns the text to run the turn with.
    pub fn human(&mut self, text: &str) -> String {
        self.gate.human(text)
    }

    /// A turn the human did not start: the text to run it with, or None
    /// (held until the human posts). From half the cap on, the text carries a
    /// note on how many such turns ran in a row.
    pub fn agent_turn(&mut self, text: &str) -> Option<String> {
        self.gate.agent_turn(text)
    }

    /// The next arrival (a turn running steers it in): what [`wait`](Self::wait)
    /// kept first, else the next item to come. Waits until there is one; None
    /// only once every sender is gone.
    ///
    /// Cancel-safe: in a `select!` beside the running turn, an item is never
    /// lost when the turn's branch wins.
    pub async fn arrival(&mut self) -> Option<String> {
        match self.ready.pop_front() {
            Some(text) => Some(text),
            None => self.rx.recv().await,
        }
    }

    /// Wait until an arrival waits for [`Self::take`]: how a host with no
    /// turn running (print mode, waiting on a background subagent) sleeps
    /// until there is work. Returns at once when one already waits.
    /// Cancel-safe, as [`arrival`](Self::arrival).
    ///
    /// Returns false once every sender is gone and nothing waits: nothing
    /// will ever arrive.
    pub async fn wait(&mut self) -> bool {
        if !self.ready.is_empty() {
            return true;
        }
        match self.rx.recv().await {
            Some(text) => {
                self.ready.push_back(text);
                true
            }
            None => false,
        }
    }

    /// Everything waiting, oldest first, as one turn's input: what the last
    /// turn left unread, then what arrived since. None when nothing waits.
    ///
    /// `control` is the main agent's ([`Agent::control`](haest_core::Agent::control)):
    /// its queue holds what was steered into the last turn and not read. Run
    /// the text through [`agent_turn`](Self::agent_turn) before the turn.
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
