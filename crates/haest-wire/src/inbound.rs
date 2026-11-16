//! [`Inbound`]: what a peer sends, and [`decode`], which checks its version
//! before its shape.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::WIRE_V;

/// One request from a peer: the wire version, an id the reply echoes, and
/// what it asks ([`Request`], flattened in).
///
/// ```
/// use haest_wire::{Inbound, Request, Verb};
///
/// let stop = Inbound::new("r2", Request::Control { verb: Verb::Interrupt, model_spec: None, effort: None });
/// let line = serde_json::to_string(&stop).unwrap();
/// assert_eq!(line, r#"{"v":1,"id":"r2","type":"control","verb":"interrupt"}"#);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inbound {
    /// The wire version the peer speaks; must be [`WIRE_V`].
    pub v: u32,
    /// The request's id: the [`Reply`](crate::Outbound::Reply) echoes it.
    pub id: String,
    /// What it asks.
    #[serde(flatten)]
    pub request: Request,
}

impl Inbound {
    /// A request of this wire version.
    pub fn new(id: impl Into<String>, request: Request) -> Self {
        Self {
            v: WIRE_V,
            id: id.into(),
            request,
        }
    }
}

/// What a peer asks. Tagged by `type` (snake_case), fields camelCase.
///
/// `turn`, `control` and `approval_answer` need `command` access; the rest
/// only watch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Request {
    /// Input for the conversation: a turn of its own when none runs, else it
    /// joins the running turn at its next step.
    Turn {
        /// The text.
        text: String,
    },
    /// Change or stop the conversation.
    Control {
        /// What to do.
        verb: Verb,
        /// The model for `set_model`, `provider:model` (the key `model`).
        #[serde(default, rename = "model", skip_serializing_if = "Option::is_none")]
        model_spec: Option<String>,
        /// The effort's word for `set_effort`; None = the model's own default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },
    /// The answer to an [`approval_ask`](crate::Outbound::ApprovalAsk).
    ApprovalAnswer {
        /// The ask's number.
        ask: u64,
        /// The answer.
        answer: Answer,
    },
    /// The conversation now: the reply's result is a [`Snapshot`](crate::Snapshot).
    Snapshot,
    /// The messages, and the seq they are current to: the reply's result is
    /// `{"seq": n, "messages": [...]}`. Follow from that seq.
    History,
    /// The stream frames after `after`, from the ring (a `gap` first if it
    /// lost some), then the reply; with `follow`, every new frame after it.
    Events {
        /// The last seq the peer has; 0 for all.
        #[serde(default)]
        after: u64,
        /// Keep sending new frames.
        #[serde(default)]
        follow: bool,
    },
}

impl Request {
    /// Whether it needs `command` access.
    pub fn commands(&self) -> bool {
        matches!(
            self,
            Request::Turn { .. } | Request::Control { .. } | Request::ApprovalAnswer { .. }
        )
    }
}

/// A [`Request::Control`] verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verb {
    /// Stop the running turn; does nothing between turns.
    Interrupt,
    /// The model from the next model call on (`model`); the word is
    /// `set_model`.
    #[serde(rename = "set_model")]
    SetModelSpec,
    /// The effort from the next model call on (`effort`).
    SetEffort,
    /// A fresh session: new id, empty history.
    Clear,
    /// End the conversation.
    Close,
}

/// How a peer answers an approval ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// Allow this call.
    Allow,
    /// Allow, and every later ask with the same tool and key.
    Always,
    /// Refuse it.
    Deny,
}

/// Why a peer's line is not a request: the [`Reply`](crate::Outbound::Reply)'s
/// error text.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// It is not JSON.
    #[error("the frame is not JSON")]
    Json(#[source] serde_json::Error),
    /// It is of another wire version: refused before its shape is read.
    #[error("the frame is wire version {got}; this end speaks version {want}")]
    Version {
        /// The frame's `v`.
        got: u64,
        /// [`WIRE_V`].
        want: u32,
    },
    /// It has no numeric `v`, so no version to check.
    #[error("the frame has no numeric v")]
    NoVersion,
    /// It is JSON of this version, but not a request (no `id`, an unknown
    /// `type`, a field missing or of the wrong kind); the parser's reason is
    /// its source.
    #[error("the frame is not a request")]
    Shape(#[source] serde_json::Error),
}

/// One line from a peer as a request: its version checked first, so a peer
/// on another version gets [`DecodeError::Version`], never a pile of shape
/// errors.
///
/// # Errors
///
/// A [`DecodeError`] that says what is wrong with the line.
pub fn decode(line: &str) -> Result<Inbound, DecodeError> {
    decode_value(serde_json::from_str(line).map_err(DecodeError::Json)?)
}

/// [`decode`] for a value already parsed (a WebSocket text message).
///
/// # Errors
///
/// [`DecodeError::NoVersion`], [`DecodeError::Version`] or
/// [`DecodeError::Shape`].
pub fn decode_value(value: Value) -> Result<Inbound, DecodeError> {
    let Some(v) = value.get("v").and_then(Value::as_u64) else {
        return Err(DecodeError::NoVersion);
    };
    if v != u64::from(WIRE_V) {
        return Err(DecodeError::Version {
            got: v,
            want: WIRE_V,
        });
    }
    serde_json::from_value(value).map_err(DecodeError::Shape)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Port of validate.test.ts: one sample per request type round-trips.
    #[test]
    fn every_request_type_reads_back_from_its_line() {
        let requests = [
            Request::Turn { text: "hi".into() },
            Request::Control {
                verb: Verb::Interrupt,
                model_spec: None,
                effort: None,
            },
            Request::Control {
                verb: Verb::SetModelSpec,
                model_spec: Some("a:b".into()),
                effort: None,
            },
            Request::Control {
                verb: Verb::SetEffort,
                model_spec: None,
                effort: Some("high".into()),
            },
            Request::Control {
                verb: Verb::Clear,
                model_spec: None,
                effort: None,
            },
            Request::Control {
                verb: Verb::Close,
                model_spec: None,
                effort: None,
            },
            Request::ApprovalAnswer {
                ask: 1,
                answer: Answer::Always,
            },
            Request::Snapshot,
            Request::History,
            Request::Events {
                after: 5,
                follow: true,
            },
        ];
        for (n, request) in requests.into_iter().enumerate() {
            let line =
                serde_json::to_string(&Inbound::new(n.to_string(), request.clone())).unwrap();
            let back = decode(&line).unwrap();
            assert_eq!((back.id, back.request), (n.to_string(), request), "{line}");
        }
    }

    #[test]
    fn a_line_that_is_not_json_is_a_json_error() {
        assert!(matches!(decode("{not json"), Err(DecodeError::Json(_))));
    }

    /// The version is checked before the shape: a frame of another version
    /// with a shape this end does not know is still a version error.
    #[test]
    fn another_version_is_refused_before_its_shape_is_read() {
        let r = decode_value(json!({"v": 99, "id": "x", "type": "nothing_known"}));
        assert!(matches!(
            r,
            Err(DecodeError::Version {
                got: 99,
                want: WIRE_V
            })
        ));
    }

    #[test]
    fn a_frame_without_a_numeric_version_is_refused_as_that() {
        let r = decode_value(json!({"id": "x", "type": "turn", "text": "hi"}));
        assert!(matches!(r, Err(DecodeError::NoVersion)));
        let r = decode_value(json!({"v": "1", "id": "x", "type": "snapshot"}));
        assert!(matches!(r, Err(DecodeError::NoVersion)));
        assert!(matches!(
            decode_value(Value::Null),
            Err(DecodeError::NoVersion)
        ));
    }

    #[test]
    fn the_right_version_with_a_wrong_shape_is_a_shape_error() {
        let missing_text = json!({"v": WIRE_V, "id": "x", "type": "turn"});
        assert!(matches!(
            decode_value(missing_text),
            Err(DecodeError::Shape { .. })
        ));
        let unknown = json!({"v": WIRE_V, "id": "x", "type": "nope"});
        assert!(matches!(
            decode_value(unknown),
            Err(DecodeError::Shape { .. })
        ));
        let no_id = json!({"v": WIRE_V, "type": "snapshot"});
        assert!(matches!(
            decode_value(no_id),
            Err(DecodeError::Shape { .. })
        ));
    }

    /// `events` without its fields asks for everything, once.
    #[test]
    fn events_defaults_to_everything_without_following() {
        let r = decode(r#"{"v":1,"id":"e","type":"events"}"#).unwrap();
        assert_eq!(
            r.request,
            Request::Events {
                after: 0,
                follow: false
            }
        );
    }

    #[test]
    fn only_turns_controls_and_answers_need_command_access() {
        assert!(
            Request::Turn {
                text: String::new()
            }
            .commands()
        );
        assert!(
            Request::ApprovalAnswer {
                ask: 1,
                answer: Answer::Deny
            }
            .commands()
        );
        assert!(!Request::Snapshot.commands());
        assert!(
            !Request::Events {
                after: 0,
                follow: true
            }
            .commands()
        );
    }
}
