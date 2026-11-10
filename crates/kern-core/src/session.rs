//! Append-only JSONL session log. One file per kern session; every line is one
//! record. Replaying the file rebuilds the exact provider-neutral history, which
//! is how a revived conversation resumes -- no round trip to a host, no
//! transcript re-parsing.
//!
//! Record kinds:
//!   header      once, first line
//!   message     one Message appended to history
//!   compaction  history was summarised: keep the last `kept` messages, prefix the summary
//!   config      model / effort changed
//!   ledger      the conversation's spend so far (absolute; the last one wins -- see ledger.rs)
//!   tasks       the whole task list (absolute; the last one wins -- see task_board.rs)
//!
//! The JSON shape is the TypeScript kern's, so either kern resumes the other's file.
//!
//! PLUG POINT: [`replay`] is separate from writing. A host that writes the file
//! itself (with record kinds of its own) replays it with [`replay`] alone:
//! a record of a kind kern does not know is skipped, not counted corrupt.
//! [`SessionSink`] is where records go; [`JsonlSessionFile`] writes them through an [`crate::Fs`].

mod file;
#[cfg(test)]
mod tests;
mod time;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub use file::{JsonlSessionFile, SessionSink};
pub use time::iso_utc;

use crate::{ledger::LedgerSnapshot, messages::Message, provider::Effort, task_board::KernTask};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SessionRecord {
    Header {
        /// Always 1.
        v: u32,
        session_id: String,
        conversation_id: String,
        cwd: String,
        /// ISO 8601, UTC (`2026-11-10T08:30:00.000Z`).
        created_at: String,
    },
    Message {
        #[serde(deserialize_with = "millis")]
        t: u64,
        message: Message,
    },
    Compaction {
        #[serde(deserialize_with = "millis")]
        t: u64,
        summary: String,
        kept: usize,
    },
    Config {
        #[serde(deserialize_with = "millis")]
        t: u64,
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
    Ledger {
        #[serde(deserialize_with = "millis")]
        t: u64,
        ledger: LedgerSnapshot,
    },
    Tasks {
        #[serde(deserialize_with = "millis")]
        t: u64,
        tasks: Vec<KernTask>,
    },
}

/// The record kinds kern knows; any other `type` belongs to whoever wrote it.
const KINDS: [&str; 6] = [
    "header",
    "message",
    "compaction",
    "config",
    "ledger",
    "tasks",
];

impl SessionRecord {
    /// The first line of a session file. `created_ms`: milliseconds since the epoch.
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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReplayedSession {
    pub messages: Vec<Message>,
    /// The last model spec a config record named.
    pub model: Option<String>,
    /// None = no config record set it; `Some(None)` = set back to the model's default.
    pub effort: Option<Option<Effort>>,
    pub ledger: Option<LedgerSnapshot>,
    pub tasks: Option<Vec<KernTask>>,
    /// Lines that are not a record (a torn final line from a crash mid-write).
    pub corrupt_lines: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid session id \"{0}\"")]
pub struct InvalidSessionId(pub String);

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
pub fn session_file(dir: &Path, id: &str) -> Result<PathBuf, InvalidSessionId> {
    match valid_id(id) {
        true => Ok(dir.join(format!("{id}.jsonl"))),
        false => Err(InvalidSessionId(id.into())),
    }
}

/// The session id a file name in a [`sessions_dir`] stands for; None for a
/// file that is not a session's.
pub fn session_id_of(file_name: &str) -> Option<&str> {
    file_name.strip_suffix(".jsonl").filter(|id| valid_id(id))
}

/// `<data_dir>/sessions/<id>.jsonl`.
pub fn session_path(data_dir: &Path, session_id: &str) -> Result<PathBuf, InvalidSessionId> {
    session_file(&sessions_dir(data_dir), session_id)
}

/// The user message that stands in for everything a compaction summarised.
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

fn apply(s: &mut ReplayedSession, r: SessionRecord) {
    match r {
        SessionRecord::Header { .. } => {}
        SessionRecord::Message { message, .. } => s.messages.push(message),
        SessionRecord::Compaction { summary, kept, .. } => {
            let from = s.messages.len() - kept.min(s.messages.len());
            let tail = s.messages.split_off(from);
            s.messages = std::iter::once(compaction_summary_message(&summary))
                .chain(tail)
                .collect();
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
    s
}
