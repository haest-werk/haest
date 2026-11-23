//! What the shell and HTTP bridges share: how a hook in another process
//! reads an event and answers it, in two dialects, and the one way each
//! transport turns a payload into its input and an answer into the event's
//! decision. A transport only carries bytes (a command's stdin and stdout,
//! a POST and its body); what they mean is here.
//!
//! - [`Haest`] (the default): the input is `{ "event": "<name>", ...payload }`,
//!   the payload's own JSON (haest-mods' types, what a mod file's handler
//!   gets), and the answer is the decision's own JSON (`{ deny }`,
//!   `{ input }`, `{ stop }`, `{ steer }`, `{ continue }`, `{ compact }`),
//!   read by haest-mods' serde. Empty output passes. Every event.
//! - [`Claude`]: Claude Code's hook JSON, so its hook scripts run as they
//!   are (claude.rs has the mapping). The events Claude Code has, as
//!   haest-mods' `ModEvent::claude_code` names them.
//!
//! Neither holds anything: what a hook's input names of the conversation
//! (its directory, the session it is on) comes with each event. In both, a
//! command's exit 2 is the event's blocking answer with stderr as its text
//! ([`blocked`]).
//!
//! A transport is a [`Hook<E>`](haest_mods::Hook) of every event its
//! dialect is a [`Dialect<E>`] of, by one generic impl: so `ShellHook<Claude>`
//! and `HttpHook<Claude>` are hooks of exactly the events claude.rs maps,
//! and bound to another one they do not compile.

use std::path::{Path, PathBuf};

use haest_core::{ChildEnv, tool::TRUNCATE_CHARS, truncate_middle};
use haest_mods::{Event, ModEvent};
use serde_json::{Value, json};

mod claude;
mod haest;

pub use claude::Claude;
pub use haest::Haest;

/// How much of a hook's own words (its stderr, an error body) a failure
/// carries: what a human or the model reads of it, not the whole output.
const SHOWN_CHARS: usize = 2_000;

/// What a hook in another process answered, before its dialect reads it.
#[derive(Debug)]
pub(crate) enum Reply {
    /// Its answer: a command's stdout on exit 0, a 2xx response's body.
    Answer(String),
    /// A command's exit 2: the event's blocking answer, its stderr the text.
    #[cfg_attr(
        not(any(feature = "shell", test)),
        expect(dead_code, reason = "only a command exits 2")
    )]
    Blocked(String),
}

/// What a dialect hands a command beside the event, whatever the event.
pub(crate) trait Vars {
    /// The variables a command gets on top of the host's, in a
    /// conversation in `dir`.
    #[cfg_attr(
        not(any(feature = "shell", test)),
        expect(dead_code, reason = "only a command has variables")
    )]
    fn env(&self, dir: &Path) -> ChildEnv;
}

/// A dialect: what a hook in another process is handed for the event `E`,
/// and how its answer reads as `E`'s decision. Which events a dialect
/// speaks is which `Dialect<E>` it implements: a transport in it is a hook
/// of those and no other.
pub(crate) trait Dialect<E: Event>: Vars + Send + Sync + 'static {
    /// The hook's input for `payload`, whose own JSON is `json`, in a
    /// conversation in `dir`.
    fn input(&self, payload: &E, json: Value, dir: &Path) -> Result<Value, HookFailure>;

    /// `E`'s decision, from what the hook answered about `payload`.
    fn decide(&self, payload: E, reply: Reply) -> Result<E::Decision, HookFailure>;
}

/// One event, ready to carry: where the conversation runs, and the hook's
/// input as bytes.
#[derive(Debug)]
pub(crate) struct Outgoing {
    /// The directory a command runs in, and the dialect names.
    #[cfg_attr(
        not(any(feature = "shell", test)),
        expect(dead_code, reason = "a POST runs nowhere")
    )]
    pub(crate) dir: PathBuf,
    /// The input, JSON.
    pub(crate) body: Vec<u8>,
}

/// `payload` as `dialect` hands it over. `dir` is the conversation's
/// directory when the host set one; else a tool event's own (`ctx.cwd`),
/// else the host process's working directory.
pub(crate) fn outgoing<E: Event, D: Dialect<E>>(
    dialect: &D,
    payload: &E,
    dir: Option<&Path>,
) -> Result<Outgoing, HookFailure> {
    let event = E::NAME;
    let json =
        serde_json::to_value(payload).map_err(|source| HookFailure::Payload { event, source })?;
    let dir = match dir {
        Some(dir) => dir.to_path_buf(),
        None => match json["ctx"]["cwd"].as_str() {
            Some(cwd) => PathBuf::from(cwd),
            None => std::env::current_dir().map_err(HookFailure::NoDir)?,
        },
    };
    let input = dialect.input(payload, json, &dir)?;
    let body =
        serde_json::to_vec(&input).map_err(|source| HookFailure::Payload { event, source })?;
    Ok(Outgoing { dir, body })
}

/// `E`'s blocking answer, `text` its reason, in either dialect:
/// `tool.before` refuses the call, `model.before` and `model.after` stop
/// the turn, `turn.end` keeps it going with `text`. Read as the
/// protocol's own words (`{ deny }`, `{ stop }`, `{ continue }`), so it is
/// what a mod file answering them does. Any other event has no blocking
/// answer: a failure. The model reads `text` as it reads a tool's output:
/// its first and last [`TRUNCATE_CHARS`] at most.
pub(crate) fn blocked<E: Event>(text: &str) -> Result<E::Decision, HookFailure> {
    let event = E::NAME;
    let word = match event {
        ModEvent::ToolBefore => "deny",
        ModEvent::ModelBefore | ModEvent::ModelAfter => "stop",
        ModEvent::TurnEnd => "continue",
        _ => {
            return Err(HookFailure::Blocked {
                event,
                stderr: shown(text),
            });
        }
    };
    let text = match text.trim() {
        "" => NO_REASON.to_string(),
        said => truncate_middle(said, TRUNCATE_CHARS).0,
    };
    serde_json::from_value(json!({ word: text }))
        .map_err(|source| HookFailure::Answer { event, source })
}

/// What a blocking answer says when the hook gave no reason.
const NO_REASON: &str = "a hook blocked this and gave no reason";

/// The first and last [`SHOWN_CHARS`] of `text`, trimmed.
pub(crate) fn shown(text: &str) -> String {
    truncate_middle(text.trim(), SHOWN_CHARS).0
}

/// Why a hook in another process gave no decision: what its binding's
/// `on_error` then handles.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HookFailure {
    /// The payload could not be written as JSON.
    #[error("{event}'s payload cannot be written as JSON")]
    Payload {
        event: ModEvent,
        source: serde_json::Error,
    },
    /// No directory to run in or to name.
    #[error("no directory for the hook: the host's working directory cannot be read")]
    NoDir(#[source] std::io::Error),
    /// The answer is not the event's decision.
    #[error("{event}: the hook's answer is not a decision it takes")]
    Answer {
        event: ModEvent,
        source: serde_json::Error,
    },
    /// An answer of a shape the dialect does not take.
    #[error("{event}: {why}")]
    Unreadable { event: ModEvent, why: String },
    /// Exit 2 on an event that has no blocking answer.
    #[error("{event} has no blocking answer, so exit 2 is a failure: {stderr}")]
    Blocked { event: ModEvent, stderr: String },
    /// A Claude Code answer haest's event cannot take (a prompt refused).
    #[error("{event}: {why}")]
    Refused { event: ModEvent, why: String },
    /// The command exited neither 0 nor 2.
    #[cfg(feature = "shell")]
    #[error("the command exited {code}{}", said(stderr))]
    Exit { code: i32, stderr: String },
    /// The server answered a status that is not 2xx.
    #[cfg(feature = "http")]
    #[error("the server answered {status}{}", said(body))]
    Status { status: u16, body: String },
    /// The request got no answer.
    #[cfg(feature = "http")]
    #[error("the request got no answer")]
    Http(#[source] haest_http::HttpError),
    /// The call or the turn was interrupted while the hook ran.
    #[error("interrupted while the hook ran")]
    Interrupted,
}

/// `: <text>`, or nothing when the hook said nothing.
fn said(text: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!(": {text}")
    }
}

#[cfg(test)]
mod tests;
