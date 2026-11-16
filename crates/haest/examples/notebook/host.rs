//! The host's say in the loop: the notebook's run clock, where a pause does not count
//! toward the turn's time cap, and its session records, which the NODE writes
//! (haest writes no file).
//!
//! PLUG POINTS: `Clock`, `TurnCapsSource`, `AgentHooks::on_message`,
//! `SessionSink`.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use haest::{
    Agent, AgentHooks, Clock, Message, TurnCaps, TurnCapsSource,
    core::session::{SessionRecord, SessionSink},
    core::tool::ToolServices,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// How long the owner keeps the run paused: ten times notebook.toml's time cap.
pub const PAUSE_MS: u64 = 600_000;

/// The notebook's PauseGate, reduced to its clock: wall time without the pauses.
/// `leave_out_pauses: false` is a plain wall clock, the counter-proof.
pub struct RunClock {
    wall: AtomicU64,
    paused: AtomicU64,
    leave_out_pauses: bool,
}

impl RunClock {
    pub fn new(start_ms: u64, leave_out_pauses: bool) -> Self {
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

    pub fn now(&self) -> u64 {
        self.wall.load(Ordering::SeqCst) - self.paused.load(Ordering::SeqCst)
    }
}

/// The node's session store: haest hands it records in history order, the node
/// writes the file (with record kinds of its own).
#[derive(Default)]
pub struct NodeSessions {
    records: Mutex<Vec<SessionRecord>>,
}

impl SessionSink for NodeSessions {
    fn append(&self, record: &SessionRecord) {
        self.records.lock().unwrap().push(record.clone());
    }
}

impl NodeSessions {
    /// The file as the node writes it: one JSON line per record, plus a line
    /// of the node's own kind that haest's replay must skip.
    pub fn file_text(&self) -> String {
        let mut text = String::new();
        for (i, r) in self.records.lock().unwrap().iter().enumerate() {
            if i == 1 {
                text.push_str(&json!({"type": "pause", "t": 0, "state": "paused"}).to_string());
                text.push('\n');
            }
            text.push_str(&serde_json::to_string(r).expect("a record serialises"));
            text.push('\n');
        }
        text
    }

    /// When each message record was stamped.
    pub fn message_times(&self) -> Vec<u64> {
        let records = self.records.lock().unwrap();
        records
            .iter()
            .filter_map(|r| match r {
                SessionRecord::Message { t, .. } => Some(*t),
                _ => None,
            })
            .collect()
    }
}

pub struct NotebookHost {
    pub clock: RunClock,
    sessions: Arc<dyn SessionSink>,
    /// The owner pauses the run before this model call (1-based); 0 = never.
    pause_before_call: usize,
    calls: AtomicUsize,
    /// The run's caps, from its settings.
    caps: TurnCaps,
}

impl NotebookHost {
    pub fn new(
        clock: RunClock,
        sessions: Arc<dyn SessionSink>,
        pause_before_call: usize,
        caps: TurnCaps,
    ) -> Self {
        Self {
            clock,
            sessions,
            pause_before_call,
            calls: AtomicUsize::new(0),
            caps,
        }
    }
}

impl ToolServices for NotebookHost {}

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
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
