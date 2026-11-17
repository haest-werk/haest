//! Rebuilding a session from its file: [`replay`].

use std::collections::HashSet;

use serde_json::Value;

use super::{EffortChange, KINDS, SessionRecord, compaction_summary_message};
use crate::{
    ledger::LedgerSnapshot,
    messages::{Message, ToolCall, ToolResultMessage},
    task_board::Task,
};

/// A session rebuilt from its file ([`replay`]): what a resumed conversation
/// starts from.
///
/// A host restores each part where it lives: the messages as the new agent's
/// history ([`AgentOptions`](crate::AgentOptions)), `ledger` into its
/// [`Ledger`](crate::ledger::Ledger), `tasks` into its
/// [`TaskBoard`](crate::task_board::TaskBoard) (`restore` on both), and the
/// model and effort as the conversation's settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReplayedSession {
    /// The history, compactions applied, every unanswered tool call answered
    /// as interrupted.
    pub messages: Vec<Message>,
    /// The last model spec a config record named.
    pub model_spec: Option<String>,
    /// What the config records did to the effort, the last that changed it
    /// winning: [`EffortChange::Unchanged`] when none did.
    pub effort: EffortChange,
    /// The last ledger record; None when there was none.
    pub ledger: Option<LedgerSnapshot>,
    /// The last task list; None when there was none.
    pub tasks: Option<Vec<Task>>,
    /// Lines that are not a record (a torn final line from a crash mid-write),
    /// and records of a kind haest knows that do not parse.
    pub corrupt_lines: usize,
}

/// One line: a record, a line that is someone else's (skipped), or garbage (counted).
pub(crate) enum Line {
    Record(Box<SessionRecord>),
    Foreign,
    Corrupt,
}

pub(crate) fn parse_line(line: &str) -> Line {
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(line) else {
        return Line::Corrupt;
    };
    let known = obj
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| KINDS.contains(&t));
    if !known {
        return Line::Foreign;
    }
    match serde_json::from_value(Value::Object(obj)) {
        Ok(r) => Line::Record(Box::new(r)),
        Err(_) => Line::Corrupt,
    }
}

/// The answer to a tool call that has no result because haest stopped mid-call.
pub(crate) const INTERRUPTED: &str =
    "Interrupted: haest stopped before this tool call returned, so it may or may not have run.";

/// `call` answered with the error saying it was interrupted ([`INTERRUPTED`]).
pub(crate) fn interrupted(call: ToolCall) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: call.id,
        tool_name: call.name,
        content: INTERRUPTED.into(),
        is_error: true,
        media: Vec::new(),
    }
}

/// A log that ends mid-tool (the process was killed) holds calls with no
/// result, and every provider refuses that history. Each such call is answered
/// after its siblings' results with an error saying so. Never written: the
/// file keeps what happened and every replay makes the same repair, so a
/// resumed conversation's history -- what a later compaction counted its tail
/// on -- is rebuilt exactly.
fn answer_interrupted(messages: Vec<Message>) -> Vec<Message> {
    let mut out = Vec::with_capacity(messages.len());
    let mut rest = messages.into_iter().peekable();
    while let Some(m) = rest.next() {
        let Message::Assistant(a) = &m else {
            out.push(m);
            continue;
        };
        let calls: Vec<ToolCall> = a.tool_calls().cloned().collect();
        out.push(m);
        let mut answered = HashSet::new();
        while let Some(Message::Tool(r)) = rest.peek() {
            answered.insert(r.tool_call_id.clone());
            out.extend(rest.next());
        }
        out.extend(
            calls
                .into_iter()
                .filter(|c| !answered.contains(&c.id))
                .map(|c| Message::Tool(interrupted(c))),
        );
    }
    out
}

/// `messages` with the calls of the last answer that have no result taken
/// out as pending, in call order, when only results follow that answer (a
/// turn that stopped mid-tool); every other call without a result is answered
/// as [`answer_interrupted`] answers it.
///
/// A durable run's replay resumes the pending calls; a turn that starts
/// answers them as interrupted, after the results already there.
pub(crate) fn split_pending(messages: Vec<Message>) -> (Vec<Message>, Vec<ToolCall>) {
    let last = messages
        .iter()
        .rposition(|m| matches!(m, Message::Assistant(_)));
    let Some(last) = last.filter(|&i| {
        messages[i + 1..]
            .iter()
            .all(|m| matches!(m, Message::Tool(_)))
    }) else {
        return (answer_interrupted(messages), Vec::new());
    };
    let mut head = messages;
    let tail = head.split_off(last);
    let mut settled = answer_interrupted(head);
    let answered: HashSet<&str> = tail
        .iter()
        .filter_map(|m| match m {
            Message::Tool(r) => Some(r.tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    let pending = match &tail[0] {
        Message::Assistant(a) => a
            .tool_calls()
            .filter(|c| !answered.contains(c.id.as_str()))
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    settled.extend(tail);
    (settled, pending)
}

fn apply(s: &mut ReplayedSession, r: SessionRecord) {
    match r {
        SessionRecord::Header { .. } => {}
        SessionRecord::Message { message, .. } => s.messages.push(message),
        SessionRecord::Compaction { summary, kept, .. } => {
            compact(&mut s.messages, &summary, kept);
        }
        SessionRecord::Config {
            model_spec: model,
            effort,
            ..
        } => {
            if let Some(m) = model.filter(|m| !m.is_empty()) {
                s.model_spec = Some(m);
            }
            if !effort.is_unchanged() {
                s.effort = effort;
            }
        }
        SessionRecord::Ledger { ledger, .. } => s.ledger = Some(ledger),
        SessionRecord::Tasks { tasks, .. } => s.tasks = Some(tasks),
        // A durable run's own records (crate::run): the conversation is in its messages.
        SessionRecord::Run { .. } | SessionRecord::Turn { .. } | SessionRecord::RunEnd { .. } => {}
    }
}

/// A compaction record: keep the `kept` latest messages behind the summary.
pub(crate) fn compact(messages: &mut Vec<Message>, summary: &str, kept: usize) {
    *messages = answer_interrupted(std::mem::take(messages));
    let from = messages.len().saturating_sub(kept);
    messages.splice(..from, [compaction_summary_message(summary)]);
}

/// Rebuild a session from a file's text. Split on '\n' only: U+2028/2029 are
/// legal inside JSON strings and must not end a line. Never fails.
///
/// Blank lines and records of a kind haest does not know are skipped; a line
/// that is not JSON, or a known record that does not parse, is counted in
/// [`ReplayedSession::corrupt_lines`]. A compaction keeps its `kept` latest
/// messages behind the summary. Config, ledger and tasks: the last one wins.
///
/// A tool call left without a result (the process was killed mid-tool) comes
/// back answered with an error saying it was interrupted and may or may not
/// have run: every provider refuses a call with no result. The repair is
/// never written, so every replay of the same file gives the same history.
pub fn replay(text: &str) -> ReplayedSession {
    let mut s = ReplayedSession::default();
    for line in text.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Line::Record(r) => apply(&mut s, *r),
            Line::Foreign => {}
            // A torn final line from a crash mid-write is expected; count it, keep going.
            Line::Corrupt => s.corrupt_lines += 1,
        }
    }
    s.messages = answer_interrupted(s.messages);
    s
}
