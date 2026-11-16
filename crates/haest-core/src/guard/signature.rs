//! A tool call and its result as one key: what the guard counts repeats by.

use serde_json::{Map, Value};

use crate::{messages::ToolCall, stable_hash::hash64, tool::ToolOutput};

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
/// The same in every build, so a resumed durable run's repeat counts still match.
pub(super) fn signature(call: &ToolCall, out: &ToolOutput) -> u64 {
    let mut input = String::new();
    canonical_object(&call.input, &mut input);
    hash64(&[
        call.name.as_bytes(),
        input.as_bytes(),
        &[u8::from(out.is_error)],
        out.content.as_bytes(),
    ])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A resumed durable run restores the repeat counts by these keys, maybe
    /// in a build made with another toolchain: the key must not move.
    #[test]
    fn a_signature_is_the_same_in_every_build() {
        let call = ToolCall {
            id: "c1".into(),
            name: "Read".into(),
            input: json!({"path": "a"}).as_object().unwrap().clone(),
        };
        assert_eq!(
            signature(&call, &ToolOutput::text("x")),
            0xfaf8_31ec_4d64_fec1
        );
    }
}
