//! Where a durable turn writes its records: [`Journal`], one run's sink.

use std::sync::Arc;

use super::{RunEnd, RunKey, TurnState};
use crate::{
    deps::now_ms,
    messages::Message,
    session::{SessionRecord, SessionSink},
};

/// One durable run's records, in the order the turn makes them. Cheap to
/// clone: the clones write to the same sink.
#[derive(Clone)]
pub(crate) struct Journal {
    key: RunKey,
    sink: Arc<dyn SessionSink>,
}

impl Journal {
    pub(crate) fn new(key: RunKey, sink: Arc<dyn SessionSink>) -> Self {
        Self { key, sink }
    }

    /// The run's key: every tool call's [`ToolContext::run_key`](crate::ToolContext::run_key).
    pub(crate) fn key(&self) -> &RunKey {
        &self.key
    }

    /// The `run` record: the run starts on `base` messages of history.
    pub(crate) fn started(&self, input_hash: &str, base: usize) {
        self.sink.append(&SessionRecord::Run {
            t: now_ms(),
            key: self.key.clone(),
            input_hash: input_hash.into(),
            base,
        });
    }

    /// A message the turn appended.
    pub(crate) fn message(&self, message: &Message) {
        self.sink.append(&SessionRecord::Message {
            t: now_ms(),
            message: message.clone(),
        });
    }

    /// What the turn caps count now.
    pub(crate) fn turn(&self, state: TurnState) {
        self.sink
            .append(&SessionRecord::Turn { t: now_ms(), state });
    }

    /// The `run_end` record: a second start returns `end`.
    pub(crate) fn ended(&self, end: RunEnd) {
        self.sink.append(&SessionRecord::RunEnd {
            t: now_ms(),
            key: self.key.clone(),
            end,
        });
    }

    /// Resolves once every record so far has landed: the intent before the effect.
    pub(crate) async fn flush(&self) {
        self.sink.flush().await;
    }
}
