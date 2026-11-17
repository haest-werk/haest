//! The host's say in the loop: the notebook's run clock, where a pause does not count
//! toward the turn's time cap, and its session records, which the NODE writes
//! (haest writes no file).
//!
//! PLUG POINTS: `Clock`, `TurnCapsSource`, `AgentHooks::on_message`,
//! `SessionSink`.

use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use haest::{
    Agent, AgentHooks, BoxError, CancellationToken, Clock, Message, TurnCaps, TurnCapsSource,
    async_trait,
    core::session::{SessionRecord, SessionSink, SessionSinkRef},
    serde_json::{self, json},
};

/// How long the owner keeps the run paused: ten times notebook.toml's time cap.
pub(crate) const PAUSE_MS: u64 = 600_000;

/// The notebook's PauseGate, reduced to its clock: wall time without the pauses.
/// `leave_out_pauses: false` is a plain wall clock, the counter-proof.
pub(crate) struct RunClock {
    wall: AtomicU64,
    paused: AtomicU64,
    leave_out_pauses: bool,
}

impl RunClock {
    pub(crate) fn new(start_ms: u64, leave_out_pauses: bool) -> Self {
        Self {
            wall: AtomicU64::new(start_ms),
            paused: AtomicU64::new(0),
            leave_out_pauses,
        }
    }

    /// The owner paused the run for `ms`, then resumed it.
    fn pause_for(&self, ms: u64) {
        self.wall.fetch_add(ms, Ordering::SeqCst);
        if self.leave_out_pauses {
            self.paused.fetch_add(ms, Ordering::SeqCst);
        }
    }

    pub(crate) fn now(&self) -> u64 {
        self.wall.load(Ordering::SeqCst) - self.paused.load(Ordering::SeqCst)
    }
}

/// The node's session store: haest hands it records in history order, the node
/// writes the file (with record kinds of its own).
#[derive(Default)]
pub(crate) struct NodeSessions {
    records: Mutex<Vec<SessionRecord>>,
}

impl SessionSink for NodeSessions {
    fn append(&self, record: &SessionRecord) {
        self.records().push(record.clone());
    }
}

impl NodeSessions {
    /// Only this file's code runs under the lock, so a poisoned one is a bug here.
    fn records(&self) -> MutexGuard<'_, Vec<SessionRecord>> {
        self.records
            .lock()
            .expect("the node's session records lock poisoned")
    }

    /// The file as the node writes it: one JSON line per record, plus a line
    /// of the node's own kind that haest's replay must skip.
    pub(crate) fn file_text(&self) -> serde_json::Result<String> {
        let mut text = String::new();
        for (i, r) in self.records().iter().enumerate() {
            if i == 1 {
                text.push_str(&json!({"type": "pause", "t": 0, "state": "paused"}).to_string());
                text.push('\n');
            }
            text.push_str(&serde_json::to_string(r)?);
            text.push('\n');
        }
        Ok(text)
    }

    /// When each message record was stamped.
    pub(crate) fn message_times(&self) -> Vec<u64> {
        self.records()
            .iter()
            .filter_map(|r| match r {
                SessionRecord::Message { t, .. } => Some(*t),
                _ => None,
            })
            .collect()
    }
}

pub(crate) struct NotebookHost {
    pub clock: RunClock,
    sessions: SessionSinkRef,
    /// The owner pauses the run before this model call (1-based); 0 = never.
    pause_before_call: usize,
    calls: AtomicUsize,
    /// The run's caps, from its settings.
    caps: TurnCaps,
}

impl NotebookHost {
    pub(crate) fn new(
        clock: RunClock,
        sessions: impl Into<SessionSinkRef>,
        pause_before_call: usize,
        caps: TurnCaps,
    ) -> Self {
        Self {
            clock,
            sessions: sessions.into(),
            pause_before_call,
            calls: AtomicUsize::new(0),
            caps,
        }
    }
}

impl NotebookHost {
    /// The run's caps, as its settings set them.
    pub(crate) fn caps(&self) -> TurnCaps {
        self.caps.clone()
    }
}

#[async_trait]
impl AgentHooks for NotebookHost {
    fn on_message(&self, msg: &Message) {
        self.sessions.append(&SessionRecord::Message {
            t: self.clock.now(),
            message: msg.clone(),
        });
    }

    /// Where the notebook's run waits while paused: every tool call has its result.
    async fn before_model_call(
        &self,
        _agent: &mut Agent,
        _cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.pause_before_call {
            self.clock.pause_for(PAUSE_MS);
        }
        Ok(())
    }
}

impl TurnCapsSource for NotebookHost {
    fn turn_caps(&self, _spec: &str) -> Option<TurnCaps> {
        Some(self.caps.clone())
    }
}

/// The turn's time cap reads the run clock: a pause does not count.
impl Clock for NotebookHost {
    fn now_ms(&self) -> u64 {
        self.clock.now()
    }
}
