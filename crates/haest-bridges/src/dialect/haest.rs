//! The haest dialect: the protocol as haest-mods types it, on the wire.

use std::path::Path;

use haest_core::ChildEnv;
use haest_mods::Event;
use serde_json::Value;

use super::{Dialect, HookFailure, Reply, Vars, blocked};

/// haest's own dialect, the default: a hook reads the event's name and its
/// payload as a mod file's handler gets it, and answers what a handler
/// returns.
///
/// | | |
/// |---|---|
/// | input | `{ "event": "tool.before", ...payload }`: the payload's own JSON ([`Event`]) with the event's name added |
/// | answer | the decision's own JSON: `{ deny }`, `{ input }`, a tool output, text, `{ stop }`, `{ steer }`, `{ continue }`, `{ compact }`; empty passes |
/// | events | every one, `model.before` and `model.after` included |
///
/// An answer that is not JSON, or JSON the event does not take (`input`
/// that is not an object), is a failure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Haest;

/// The event and its payload are all on stdin.
impl Vars for Haest {
    fn env(&self, _dir: &Path) -> ChildEnv {
        ChildEnv::default()
    }
}

impl<E: Event> Dialect<E> for Haest {
    fn input(&self, _payload: &E, json: Value, _dir: &Path) -> Result<Value, HookFailure> {
        let mut input = serde_json::Map::new();
        input.insert("event".into(), E::NAME.as_str().into());
        if let Value::Object(fields) = json {
            input.extend(fields);
        }
        Ok(Value::Object(input))
    }

    fn decide(&self, _payload: E, reply: Reply) -> Result<E::Decision, HookFailure> {
        match reply {
            Reply::Blocked(stderr) => blocked::<E>(&stderr),
            Reply::Answer(text) if text.trim().is_empty() => Ok(E::Decision::default()),
            Reply::Answer(text) => {
                serde_json::from_str(&text).map_err(|source| HookFailure::Answer {
                    event: E::NAME,
                    source,
                })
            }
        }
    }
}
