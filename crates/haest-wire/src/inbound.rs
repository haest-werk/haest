//! [`Inbound`]: what a peer sends, and [`decode`], which checks its version
//! before its shape.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{WIRE_V, words::word_enum};

/// One request from a peer: the wire version, an id the reply echoes, and
/// what it asks ([`Request`], flattened in).
///
/// ```
/// use haest_wire::{Control, Inbound, Request};
///
/// let stop = Inbound::new("r2", Request::Control(Control::Interrupt));
/// let line = serde_json::to_string(&stop)?;
/// assert_eq!(line, r#"{"v":1,"id":"r2","type":"control","verb":"interrupt"}"#);
/// # Ok::<(), serde_json::Error>(())
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

/// What a peer asks. Tagged by `type` (`snake_case`), fields `camelCase`.
///
/// `turn`, `control` and `approval_answer` need `command` access; the rest
/// only watch. A new request type is additive on the wire, so a `match` has a
/// `_` arm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum Request {
    /// Input for the conversation: a turn of its own when none runs, else it
    /// joins the running turn at its next step.
    Turn {
        /// The text.
        text: String,
    },
    /// Change or stop the conversation: `verb`, and that verb's own field.
    Control(Control),
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
    /// Whether it needs `command` access: `turn`, `control` and
    /// `approval_answer` do.
    pub fn needs_command(&self) -> bool {
        matches!(
            self,
            Request::Turn { .. } | Request::Control(_) | Request::ApprovalAnswer { .. }
        )
    }
}

/// What a [`Request::Control`] asks: tagged by `verb` (`snake_case`), with
/// the verb's own field beside it. A verb carries only the field it uses, so
/// `set_model` without a model is not a request (a [`DecodeError::Shape`]).
/// More verbs may come: a `match` has a `_` arm.
///
/// ```
/// use haest_wire::{Control, Request, decode};
///
/// let line = r#"{"v":1,"id":"r1","type":"control","verb":"set_model","model":"anthropic:claude-opus-5-5"}"#;
/// let asked = decode(line)?.request;
/// let model = "anthropic:claude-opus-5-5".to_string();
/// assert_eq!(asked, Request::Control(Control::SetModel { model }));
/// # Ok::<(), haest_wire::DecodeError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Control {
    /// Stop the running turn; does nothing between turns.
    Interrupt,
    /// The model from the next turn on: a running turn ends on the model it
    /// started with, and the reply comes once the change is made.
    SetModel {
        /// `provider:model`.
        model: String,
    },
    /// The effort from the next turn on: a running turn ends on the effort
    /// it started with, and the reply comes once the change is made.
    SetEffort {
        /// The effort's word; None (or `default`) = the model's own default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },
    /// A fresh session: new id, empty history. Made once the running turn
    /// ends; the reply comes once it is.
    Clear,
    /// End the conversation.
    Close,
}

word_enum! {
    /// How a peer answers an approval ask. More answers may come (for this
    /// session only, deny always): a `match` has a `_` arm, which denies.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[non_exhaustive]
    pub enum Answer ("answer") {
        /// Allow this call.
        Allow = "allow",
        /// Allow, and every later ask with the same tool and key.
        Always = "always",
        /// Refuse it.
        Deny = "deny",
    }
}

/// Why a peer's line is not a request: the [`Reply`](crate::Outbound::Reply)'s
/// error text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
            Request::Control(Control::Interrupt),
            Request::Control(Control::SetModel {
                model: "a:b".into(),
            }),
            Request::Control(Control::SetEffort {
                effort: Some("high".into()),
            }),
            Request::Control(Control::SetEffort { effort: None }),
            Request::Control(Control::Clear),
            Request::Control(Control::Close),
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
        let turn = Request::Turn {
            text: String::new(),
        };
        let answer = Request::ApprovalAnswer {
            ask: 1,
            answer: Answer::Deny,
        };
        let control = Request::Control(Control::Clear);
        assert!(turn.needs_command() && answer.needs_command() && control.needs_command());
        let events = Request::Events {
            after: 0,
            follow: true,
        };
        assert!(!Request::Snapshot.needs_command() && !events.needs_command());
    }

    /// A verb reads only its own field: one it does not use is ignored, as
    /// before; one it needs is required.
    #[test]
    fn a_control_reads_only_its_verbs_field() {
        let control = |line: &str| decode(line).map(|i| i.request);
        let interrupt =
            r#"{"v":1,"id":"x","type":"control","verb":"interrupt","model":"a:b","effort":"high"}"#;
        assert_eq!(
            control(interrupt).unwrap(),
            Request::Control(Control::Interrupt)
        );
        let set_model =
            r#"{"v":1,"id":"x","type":"control","verb":"set_model","model":"a:b","effort":"low"}"#;
        assert_eq!(
            control(set_model).unwrap(),
            Request::Control(Control::SetModel {
                model: "a:b".into()
            })
        );
        for refused in [
            r#"{"v":1,"id":"x","type":"control","verb":"set_model"}"#,
            r#"{"v":1,"id":"x","type":"control","verb":"nope"}"#,
            r#"{"v":1,"id":"x","type":"control"}"#,
        ] {
            assert!(
                matches!(control(refused), Err(DecodeError::Shape(_))),
                "{refused}"
            );
        }
    }
}
