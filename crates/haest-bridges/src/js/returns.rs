//! Why what crossed into or out of a mod file was refused.

use haest_core::error::BoxError;
use haest_mods::ModEvent;
use serde_json::Value;

/// A payload that could not be written as JSON, or an answer its event does
/// not take.
#[derive(Debug, thiserror::Error)]
pub(super) enum BadReturn {
    #[error("{event} returned {got}, not text")]
    NotText { event: &'static str, got: Value },
    #[error("{event}'s payload cannot be written as JSON")]
    Payload {
        event: ModEvent,
        source: serde_json::Error,
    },
    #[error("handler {nth} on {event} returned an answer it does not take")]
    Answer {
        event: ModEvent,
        /// Which of the event's handlers, from 1, in the order added.
        nth: usize,
        source: serde_json::Error,
    },
}

/// A text answer: the text, nothing, or a refusal of anything else.
pub(super) fn text_of(v: Value, event: &'static str) -> Result<Option<String>, BoxError> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s)),
        got => Err(BadReturn::NotText { event, got }.into()),
    }
}
