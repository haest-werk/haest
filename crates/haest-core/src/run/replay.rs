//! Rebuilding a durable run from its records: [`replay_run`].

use std::collections::HashSet;

use super::{RunEnd, RunKey, TurnState};
use crate::{
    messages::{Message, ToolCall},
    session::{Line, SessionRecord, answer_interrupted, compact, parse_line},
};

/// A durable run rebuilt from its records ([`replay_run`]): what
/// [`Agent::run_durable`](crate::Agent::run_durable) resumes from, or returns.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayedRun {
    /// The run's key, from its `run` record.
    pub key: RunKey,
    /// The hash of the input the run started with.
    pub input_hash: String,
    /// How many messages the history held when the run started: the run's
    /// own messages come after them.
    pub base: usize,
    /// The run's own messages. The last answer's calls that have no result
    /// stay unanswered (they are [`pending`](Self::pending)); any other call
    /// without a result is answered as interrupted, as [`replay`](crate::session::replay) does.
    pub messages: Vec<Message>,
    /// The calls of the last answer that have no result, in call order.
    pub pending: Vec<ToolCall>,
    /// The last `turn` record: what the turn caps counted. None before the
    /// first one landed.
    pub turn: Option<TurnState>,
    /// How the run ended; None for a run that never ended.
    pub end: Option<RunEnd>,
    /// Lines that are not a record, and known records that do not parse.
    pub corrupt_lines: usize,
}

impl ReplayedRun {
    fn started(key: RunKey, input_hash: String, base: usize) -> Self {
        Self {
            key,
            input_hash,
            base,
            messages: Vec::new(),
            pending: Vec::new(),
            turn: None,
            end: None,
            corrupt_lines: 0,
        }
    }

    fn apply(&mut self, record: SessionRecord) {
        match record {
            SessionRecord::Message { message, .. } => self.messages.push(message),
            SessionRecord::Compaction { summary, kept, .. } => {
                compact(&mut self.messages, &summary, kept)
            }
            SessionRecord::Turn { state, .. } => self.turn = Some(state),
            SessionRecord::RunEnd { end, .. } => self.end = Some(end),
            // Who the session is, its model, ledger and tasks: the host's, not the run's.
            SessionRecord::Header { .. }
            | SessionRecord::Config { .. }
            | SessionRecord::Ledger { .. }
            | SessionRecord::Tasks { .. }
            | SessionRecord::Run { .. } => {}
        }
    }

    /// The last answer's unanswered calls become `pending`; every earlier
    /// call without a result is answered as interrupted.
    fn settle(&mut self) {
        let messages = std::mem::take(&mut self.messages);
        let last = messages
            .iter()
            .rposition(|m| matches!(m, Message::Assistant(_)));
        let Some(last) = last.filter(|&i| {
            messages[i + 1..]
                .iter()
                .all(|m| matches!(m, Message::Tool(_)))
        }) else {
            self.messages = answer_interrupted(messages);
            return;
        };
        let mut head = messages;
        let tail = head.split_off(last);
        self.messages = answer_interrupted(head);
        let answered: HashSet<&str> = tail
            .iter()
            .filter_map(|m| match m {
                Message::Tool(r) => Some(r.tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        if let Message::Assistant(a) = &tail[0] {
            self.pending = a
                .tool_calls()
                .filter(|c| !answered.contains(c.id.as_str()))
                .cloned()
                .collect();
        }
        self.messages.extend(tail);
    }
}

/// Rebuild a durable run from its records' text, one JSON record a line (a
/// session file, or what a [`RunStore`](super::RunStore) keeps). Never fails
/// on a bad line; None when no `run` record is there.
///
/// The run starts at the last `run` record: one written again after a start
/// whose records never landed starts it over. A record of a kind haest does
/// not know is skipped. Session records that are not the run's (the header,
/// config, ledger, tasks) are skipped too: they stay the host's.
///
/// # Examples
///
/// ```
/// use haest_core::run::replay_run;
///
/// let log = [
///     r#"{"type":"run","t":1,"key":"job-1","inputHash":"0f","base":0}"#,
///     r#"{"type":"message","t":2,"message":{"role":"user","content":"hi"}}"#,
///     r#"{"type":"run_end","t":3,"key":"job-1","stopReason":"end_turn","text":"hello"}"#,
/// ]
/// .join("\n");
///
/// let run = replay_run(&log).ok_or("no run in the log")?;
/// assert_eq!(run.key.as_str(), "job-1");
/// assert_eq!(run.end.unwrap().text, "hello");
/// assert!(replay_run("").is_none());
/// # Ok::<(), &'static str>(())
/// ```
pub fn replay_run(text: &str) -> Option<ReplayedRun> {
    let mut run: Option<ReplayedRun> = None;
    let mut corrupt = 0;
    for line in text.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Line::Record(r) => match *r {
                SessionRecord::Run {
                    key,
                    input_hash,
                    base,
                    ..
                } => run = Some(ReplayedRun::started(key, input_hash, base)),
                other => {
                    if let Some(run) = run.as_mut() {
                        run.apply(other);
                    }
                }
            },
            Line::Foreign => {}
            Line::Corrupt => corrupt += 1,
        }
    }
    let mut run = run?;
    run.settle();
    run.corrupt_lines = corrupt;
    Some(run)
}
