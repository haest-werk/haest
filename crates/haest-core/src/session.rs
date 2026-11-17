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
//! | `run`, `turn`, `run_end` | a durable run's start, turn state and result ([`crate::run`]); [`replay`] skips them |
//!
//! The JSON shape is the TypeScript haest's, so either haest resumes the other's file.
//!
//! PLUG POINT: [`replay`] is separate from writing. A host that writes the file
//! itself (with record kinds of its own) replays it with [`replay`] alone:
//! a record of a kind haest does not know is skipped, not counted corrupt.
//! [`SessionSink`] is where records go; [`JsonlSessionFile`] writes them through an [`crate::Fs`].
//!
//! Where the file lives: `<data_dir>/sessions/<id>.jsonl` ([`session_file`] in
//! the [`sessions_dir`]).
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
//! assert_eq!(s.model_spec.as_deref(), Some("openrouter:example/model-1"));
//! assert_eq!(s.corrupt_lines, 1);
//! ```

mod file;
mod replay;
#[cfg(test)]
mod tests;
mod time;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

pub use file::{JsonlSessionFile, SessionSink, SessionSinkRef};
pub(crate) use replay::{INTERRUPTED, Line, answer_interrupted, compact, parse_line};
pub use replay::{ReplayedSession, replay};
pub use time::iso_utc;

use crate::{
    ledger::LedgerSnapshot,
    messages::Message,
    provider::Effort,
    run::{RunEnd, RunKey, TurnState},
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
#[non_exhaustive]
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
        #[serde(rename = "model", default, skip_serializing_if = "Option::is_none")]
        model_spec: Option<String>,
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
    /// A durable run started ([`crate::run`]). A later `run` record in the
    /// same log starts the run over.
    Run {
        /// When it was written, in ms since the epoch.
        #[serde(deserialize_with = "millis")]
        t: u64,
        /// The run's key.
        key: RunKey,
        /// The input's hash ([`input_hash`](crate::run::input_hash)).
        input_hash: String,
        /// How many messages the history held when the run started.
        #[serde(default)]
        base: usize,
    },
    /// What a durable run's turn caps count now; the last one wins.
    Turn {
        /// When it was written, in ms since the epoch.
        #[serde(deserialize_with = "millis")]
        t: u64,
        /// The turn's spend, model calls, time and repeat counts.
        #[serde(flatten)]
        state: TurnState,
    },
    /// A durable run ended on its own: a second start returns this.
    RunEnd {
        /// When it was written, in ms since the epoch.
        #[serde(deserialize_with = "millis")]
        t: u64,
        /// The run's key.
        key: RunKey,
        /// How it ended.
        #[serde(flatten)]
        end: RunEnd,
    },
}

/// The record kinds haest knows; any other `type` belongs to whoever wrote it.
const KINDS: [&str; 9] = [
    "header",
    "message",
    "compaction",
    "config",
    "ledger",
    "tasks",
    "run",
    "turn",
    "run_end",
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
#[expect(
    clippy::option_option,
    reason = "serde's shape for a key that may be absent, null, or set"
)]
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<Effort>>, D::Error> {
    Option::<Effort>::deserialize(d).map(Some)
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

/// The user message that stands in for everything a compaction summarised:
/// a line saying the earlier conversation was compacted, then `summary`.
/// Live compaction and [`replay`] both use it, so a resumed history matches
/// the one the agent had.
pub fn compaction_summary_message(summary: &str) -> Message {
    Message::user(format!(
        "[Earlier conversation was compacted. Summary of what happened so far:]\n\n{summary}"
    ))
}
