//! Streamed tool arguments (JSON text) -> an input object. Shared by both OpenAI
//! dialects (Chat Completions and Responses), which stream arguments the same way.

use serde_json::{Map, Value};

/// Never fails: unparseable text comes back as `{ _unparsed }`, which the loop answers as a model error.
pub fn parse_args(args: &str) -> Map<String, Value> {
    if args.trim().is_empty() {
        return Map::new();
    }
    match serde_json::from_str::<Value>(args) {
        Ok(Value::Object(m)) => m,
        Ok(other) => Map::from_iter([("value".to_string(), other)]),
        Err(_) => Map::from_iter([("_unparsed".to_string(), Value::String(args.to_string()))]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn objects_parse_blanks_are_empty_and_garbage_is_surfaced() {
        assert_eq!(Value::Object(parse_args(r#"{"a":1}"#)), json!({"a": 1}));
        assert!(parse_args("  ").is_empty());
        assert_eq!(Value::Object(parse_args("5")), json!({"value": 5}));
        assert_eq!(
            Value::Object(parse_args("{bad")),
            json!({"_unparsed": "{bad"})
        );
    }
}
