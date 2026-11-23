//! Every decision as JSON, and the one decoder of a hook's answer: what a
//! mod file's handler returns (haest-bridges hands it over as it was
//! returned) and what a shell or HTTP hook answers are read by these impls
//! and the event's own serde, so an answer means the same whatever carries it.
//!
//! A `tool.before` or loop-event answer that is not an object (nothing,
//! text, a number) passes, a key the event does not take is ignored, and a
//! key whose value is `null` counts as absent. Where a key takes text the
//! value is a string, and `input` an object; `prompt.submit` and
//! `system.prompt` answer text or nothing, `tool.after` an output or
//! nothing. Anything else is an answer haest does not take: the hook
//! failed. Nothing is coerced to text.

use haest_core::tool::JsonMap;
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, Error as _},
    ser::SerializeMap,
};
use serde_json::Value;

use super::{Compact, ModelAfterDecision, ModelBeforeDecision, TurnEndDecision};
use crate::api::Before;

impl Serialize for Before {
    /// `null`, `{ "deny": reason }` or `{ "input": {..} }`.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Before::Pass => s.serialize_unit(),
            Before::Deny(reason) => one_key(s, "deny", reason),
            Before::Input(input) => one_key(s, "input", input),
        }
    }
}

impl<'de> Deserialize<'de> for Before {
    /// `{ deny }` refuses (it wins over `input`), `{ input }` rewrites,
    /// anything else passes.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut answer = answer(d)?;
        if let Some(reason) = text(&mut answer, "deny")? {
            return Ok(Before::Deny(reason));
        }
        match answer.remove("input") {
            None | Some(Value::Null) => Ok(Before::Pass),
            Some(Value::Object(input)) => Ok(Before::Input(input)),
            Some(got) => Err(D::Error::custom(format_args!(
                "`input` must be an object, not {got}"
            ))),
        }
    }
}

impl<'de> Deserialize<'de> for ModelBeforeDecision {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut answer = answer(d)?;
        Ok(Self {
            compact: compact_key(&mut answer)?,
            steer: text(&mut answer, "steer")?,
            stop: text(&mut answer, "stop")?,
        })
    }
}

impl<'de> Deserialize<'de> for ModelAfterDecision {
    /// `compact` is not taken here: the answer's tool results are still to land.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut answer = answer(d)?;
        Ok(Self {
            steer: text(&mut answer, "steer")?,
            stop: text(&mut answer, "stop")?,
        })
    }
}

impl<'de> Deserialize<'de> for TurnEndDecision {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut answer = answer(d)?;
        Ok(Self {
            compact: compact_key(&mut answer)?,
            r#continue: text(&mut answer, "continue")?,
        })
    }
}

impl Serialize for Compact {
    /// `true`, or `{ "instructions": .. }`.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match &self.instructions {
            None => s.serialize_bool(true),
            Some(instructions) => one_key(s, "instructions", instructions),
        }
    }
}

impl<'de> Deserialize<'de> for Compact {
    /// `true`, or an object with optional `instructions`.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::Bool(true) => Ok(Compact::new()),
            Value::Object(mut asked) => Ok(Compact {
                instructions: text(&mut asked, "instructions")?,
            }),
            got => Err(D::Error::custom(format_args!(
                "`compact` is true or {{ instructions }}, not {got}"
            ))),
        }
    }
}

/// `{ key: value }`.
fn one_key<S: Serializer>(s: S, key: &str, value: &impl Serialize) -> Result<S::Ok, S::Error> {
    let mut map = s.serialize_map(Some(1))?;
    map.serialize_entry(key, value)?;
    map.end()
}

/// An answer's keys: an object's, or none for anything else (it passes).
fn answer<'de, D: Deserializer<'de>>(d: D) -> Result<JsonMap, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Object(answer) => answer,
        _ => JsonMap::new(),
    })
}

/// The text under `key`: absent or `null` is none, a string is the text.
fn text<E: de::Error>(answer: &mut JsonMap, key: &str) -> Result<Option<String>, E> {
    match answer.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(got) => Err(E::custom(format_args!("`{key}` must be text, not {got}"))),
    }
}

/// `compact`: absent, `null` or `false` asks for none.
fn compact_key<E: de::Error>(answer: &mut JsonMap) -> Result<Option<Compact>, E> {
    match answer.remove("compact") {
        None | Some(Value::Null | Value::Bool(false)) => Ok(None),
        Some(asked) => Compact::deserialize(asked).map(Some).map_err(E::custom),
    }
}
