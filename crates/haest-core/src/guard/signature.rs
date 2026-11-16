//! A tool call and its result as one key: what the guard counts repeats by.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use serde_json::{Map, Value};

use crate::{messages::ToolCall, tool::ToolOutput};

/// Key-order-independent JSON, so `{a,b}` and `{b,a}` are the same call.
pub(super) fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(o) => canonical_object(o, out),
        other => out.push_str(&other.to_string()),
    }
}

fn canonical_object(o: &Map<String, Value>, out: &mut String) {
    let mut keys: Vec<&String> = o.keys().collect();
    keys.sort();
    out.push('{');
    for (i, k) in keys.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(k).unwrap_or_default());
        out.push(':');
        canonical(&o[k], out);
    }
    out.push('}');
}

/// One call + its result, as a key. A 64-bit hash: it only has to tell apart the
/// handful of distinct calls one turn makes, and it keeps no result text alive.
pub(super) fn signature(call: &ToolCall, out: &ToolOutput) -> u64 {
    let mut input = String::new();
    canonical_object(&call.input, &mut input);
    let mut h = DefaultHasher::new();
    (
        &call.name,
        '\0',
        input,
        '\0',
        out.is_error,
        '\0',
        &out.content,
    )
        .hash(&mut h);
    h.finish()
}
