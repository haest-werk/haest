//! Append-only JSONL session log. One file per haest session; every line is one
//! record. Replaying the file rebuilds the exact provider-neutral history, which
//! is how a revived conversation resumes -- no round trip to a host, no
//! transcript re-parsing.
//!
//! Record kinds ([`SessionRecord`]):
//!
//! | `type` | What it says |
//! |---|---|
//! | `header` | once, first line: the ids, the cwd, when it was created |
//! | `message` | one [`Message`] appended to history |
//! | `compaction` | history was summarised: keep the last `kept` messages, prefix the summary |
//! | `config` | model / effort changed |
//! | `ledger` | the conversation's spend so far (absolute; the last one wins -- [`crate::ledger`]) |
//! | `tasks` | the whole task list (absolute; the last one wins -- [`crate::task_board`]) |
//!
//! The JSON shape is the TypeScript haest's, so either haest resumes the other's file.
//!
//! PLUG POINT: [`replay`] is separate from writing. A host that writes the file
//! itself (with record kinds of its own) replays it with [`replay`] alone:
//! a record of a kind haest does not know is skipped, not counted corrupt.
//! [`SessionSink`] is where records go; [`JsonlSessionFile`] writes them through an [`crate::Fs`].
//!
//! Where the file lives: `<data_dir>/sessions/<id>.jsonl` ([`session_path`]).
//! A resumed conversation starts from a [`ReplayedSession`]: its messages,
//! model, effort, ledger and task list.
//!
//! # Examples
//!
//! A host that keeps the file itself replays it alone:
//!
//! ```
//! use haest_core::{Message, session::replay};
//!
//! let file = [
//!     r#"{"type":"header","v":1,"sessionId":"s1","conversationId":"c1","cwd":"/p","createdAt":"2026-11-10T08:30:00.000Z"}"#,
//!     r#"{"type":"message","t":1,"message":{"role":"user","content":"hi"}}"#,
//!     r#"{"type":"config","t":2,"model":"openrouter:example/model-1","effort":"high"}"#,
//!     r#"{"type":"host_note","t":3,"note":"a record of the host's own: skipped"}"#,
//!     r#"{"type":"message","t":4,"mess"#, // torn by a crash mid-write
//! ]
//! .join("\n");
//!
//! let s = replay(&file);
//! assert_eq!(s.messages, [Message::user("hi")]);
//! assert_eq!(s.model.as_deref(), Some("openrouter:example/model-1"));
//! assert_eq!(s.corrupt_lines, 1);
//! ```

mod file;
#[cfg(test)]
mod tests;
mod time;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub use file::{JsonlSessionFile, SessionSink};
pub use time::iso_utc;

use crate::{
    ledger::LedgerSnapshot,
    messages::{Message, ToolCall, ToolResultMessage},
    provider::Effort,
    task_board::Task,
};

/// One line of a session file. Serialized with a `type` tag (`header`,
/// `message`, ...) and camelCase fields, the TypeScript haest's shape.
///
/// Every record but the header carries `t`: milliseconds since the epoch when
/// it was written (any JSON number reads, kept as whole milliseconds).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SessionRecord {
    /// The first line: who the session is. Build it with [`SessionRecord::header`].
    Header {
        /// Always 1.
        v: u32,
        /// The session's id, also its file name.
        session_id: String,
        /// The host's conversation the session belongs to.
        conversation_id: String,
        /// The agent's working directory when the session began.
        cwd: String,
        /// ISO 8601, UTC (`2026-11-10T08:30:00.000Z`).
        created_at: String,
    },
    /// One message appended to history.
    Message {
        /// When it was written, in ms since the epoch.
        #[serde(deserialize_with = "millis")]
        t: u64,
        /// The message.
        message: Message,
    },
    /// History was summarised: replay keeps the last `kept` messages and puts
    /// the summary before them ([`compaction_summary_message`]).
    Compaction {
        /// When it was written, in ms since the epoch.
        #[serde(deserialize_with = "millis")]
        t: u64,
        /// What the summarised messages said.
        summary: String,
        /// How many of the latest messages were kept as they were.
        kept: usize,
    },
    /// The model or the effort changed.
    Config {
        /// When it was written, in ms since the epoch.
        #[serde(deserialize_with = "millis")]
        t: u64,
        /// The new model spec; absent (or empty) = unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Absent = unchanged; `null` = back to the model's default.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        effort: Option<Option<Effort>>,
    },
    /// What the conversation has spent so far; the last one wins.
    Ledger {
        /// When it was written, in ms since the epoch.
        #[serde(deserialize_with = "millis")]
        t: u64,
        /// The spend and caps, absolute.
        ledger: LedgerSnapshot,
    },
    /// The whole task list; the last one wins.
    Tasks {
        /// When it was written, in ms since the epoch.
        #[serde(deserialize_with = "millis")]
        t: u64,
        /// Every task, in list order.
        tasks: Vec<Task>,
    },
}

/// The record kinds haest knows; any other `type` belongs to whoever wrote it.
const KINDS: [&str; 6] = [
    "header",
    "message",
    "compaction",
    "config",
    "ledger",
    "tasks",
];

impl SessionRecord {
    /// The first line of a session file, version 1. `created_ms`:
    /// milliseconds since the epoch, written as an ISO 8601 date ([`iso_utc`]).
    pub fn header(session_id: &str, conversation_id: &str, cwd: &str, created_ms: u64) -> Self {
        SessionRecord::Header {
            v: 1,
            session_id: session_id.into(),
            conversation_id: conversation_id.into(),
            cwd: cwd.into(),
            created_at: iso_utc(created_ms),
        }
    }
}

/// A timestamp written by a JS clock: any JSON number, kept as whole milliseconds.
fn millis<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let n = serde_json::Number::deserialize(d)?;
    Ok(n.as_u64()
        .or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
        .unwrap_or(0))
}

/// A present key, even `null`, is `Some`; only an absent key stays `None` (via `default`).
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<Effort>>, D::Error> {
    Option::<Effort>::deserialize(d).map(Some)
}

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
    pub model: Option<String>,
    /// None = no config record set it; `Some(None)` = set back to the model's default.
    pub effort: Option<Option<Effort>>,
    /// The last ledger record; None when there was none.
    pub ledger: Option<LedgerSnapshot>,
    /// The last task list; None when there was none.
    pub tasks: Option<Vec<Task>>,
    /// Lines that are not a record (a torn final line from a crash mid-write),
    /// and records of a kind haest knows that do not parse.
    pub corrupt_lines: usize,
}

/// A session id that is not `[a-zA-Z0-9_-]+`, refused so its file cannot
/// land outside the sessions dir.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid session id \"{id}\"")]
pub struct InvalidSessionId {
    /// The id, as given.
    pub id: String,
}

/// Where a data dir's sessions are: `<data_dir>/sessions`.
pub fn sessions_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("sessions")
}

/// A session id is `[a-zA-Z0-9_-]+`, so its file cannot escape the dir.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Session `id`'s file in a [`sessions_dir`]: `<dir>/<id>.jsonl`.
///
/// # Errors
///
/// [`InvalidSessionId`] unless `id` is `[a-zA-Z0-9_-]+`.
pub fn session_file(dir: &Path, id: &str) -> Result<PathBuf, InvalidSessionId> {
    if !valid_id(id) {
        return Err(InvalidSessionId { id: id.into() });
    }
    Ok(dir.join(format!("{id}.jsonl")))
}

/// The session id a file name in a [`sessions_dir`] stands for; None for a
/// file that is not a session's (`a.jsonl` -> `a`, `notes.txt` -> None).
pub fn session_id_of(file_name: &str) -> Option<&str> {
    file_name.strip_suffix(".jsonl").filter(|id| valid_id(id))
}

/// The session file for `session_id` under a data dir:
/// `<data_dir>/sessions/<session_id>.jsonl`.
///
/// # Errors
///
/// [`InvalidSessionId`] unless `session_id` is `[a-zA-Z0-9_-]+`.
pub fn session_path(data_dir: &Path, session_id: &str) -> Result<PathBuf, InvalidSessionId> {
    session_file(&sessions_dir(data_dir), session_id)
}

/// The user message that stands in for everything a compaction summarised:
/// a line saying the earlier conversation was compacted, then `summary`.
/// Live compaction and [`replay`] both use it, so a resumed history matches
/// the one the agent had.
pub fn compaction_summary_message(summary: &str) -> Message {
    Message::user(format!(
        "[Earlier conversation was compacted. Summary of what happened so far:]\n\n{summary}"
    ))
}

/// One line: a record, a line that is someone else's (skipped), or garbage (counted).
enum Line {
    Record(Box<SessionRecord>),
    Foreign,
    Corrupt,
}

fn parse_line(line: &str) -> Line {
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

const INTERRUPTED: &str =
    "Interrupted: haest stopped before this tool call returned, so it may or may not have run.";

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
                .map(|c| {
                    Message::Tool(ToolResultMessage {
                        tool_call_id: c.id,
                        tool_name: c.name,
                        content: INTERRUPTED.into(),
                        is_error: true,
                        media: Vec::new(),
                    })
                }),
        );
    }
    out
}

fn apply(s: &mut ReplayedSession, r: SessionRecord) {
    match r {
        SessionRecord::Header { .. } => {}
        SessionRecord::Message { message, .. } => s.messages.push(message),
        SessionRecord::Compaction { summary, kept, .. } => {
            s.messages = answer_interrupted(std::mem::take(&mut s.messages));
            let from = s.messages.len().saturating_sub(kept);
            s.messages
                .splice(..from, [compaction_summary_message(&summary)]);
        }
        SessionRecord::Config { model, effort, .. } => {
            if let Some(m) = model.filter(|m| !m.is_empty()) {
                s.model = Some(m);
            }
            if effort.is_some() {
                s.effort = effort;
            }
        }
        SessionRecord::Ledger { ledger, .. } => s.ledger = Some(ledger),
        SessionRecord::Tasks { tasks, .. } => s.tasks = Some(tasks),
    }
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
