//! The effort dialect of a direct OpenAI-compatible provider: a top-level
//! `reasoning_effort` that takes only the levels the provider lists and 400s on
//! anything else (Paper Instruments: low / high / max, no `medium`, no off).
//! OpenRouter has its own `reasoning` object (`compat::convert::flavor_extras`).

use kern_core::{catalog::Reasoning, effort::nearest_accepted_effort, provider::Effort};
use serde_json::{Map, Value};

/// Body fields for `effort` under `dialect`; empty when either is absent or nothing fits.
pub fn reasoning_effort_extras(
    dialect: Option<&Reasoning>,
    effort: Option<Effort>,
) -> Map<String, Value> {
    let mut out = Map::new();
    let (Some(dialect), Some(effort)) = (dialect, effort) else {
        return out;
    };
    if let Some(level) = nearest_accepted_effort(effort, dialect) {
        out.insert(
            "reasoning_effort".into(),
            serde_json::to_value(level).unwrap_or(Value::Null),
        );
    }
    out
}
