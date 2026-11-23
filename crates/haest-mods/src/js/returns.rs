//! What a mod file's handler may return, and why one was refused.

use haest_core::error::BoxError;
use serde_json::Value;

use crate::events::ModEvent;

/// A handler returned what its event does not take.
#[derive(Debug, thiserror::Error)]
pub(super) enum BadReturn {
    #[error("{event} returned {got}, not text")]
    NotText { event: &'static str, got: Value },
    #[error("tool.before returned an input that is not an object")]
    InputNotObject,
    #[error("tool.before returned {0}")]
    Before(Value),
    #[error("tool.after returned no tool output")]
    NotOutput(#[source] serde_json::Error),
    #[error("{event}'s answer did not come back whole", event = .event.as_str())]
    Loop {
        event: ModEvent,
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
