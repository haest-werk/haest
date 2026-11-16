//! Reading a tool call's arguments the way models actually send them: a
//! number as a string, `""` for "left out".

use serde_json::Value;

use super::{JsonMap, ToolError};

/// A required, non-empty string argument.
///
/// # Errors
///
/// [`ToolError::Input`] when `key` is missing, empty or not a string.
///
/// ```
/// use haest_core::tool::{opt_bool, opt_num, opt_str, req_str};
///
/// let input = serde_json::json!({"path": "a.txt", "limit": "20", "all": "true", "glob": ""});
/// let input = input.as_object().unwrap();
/// assert_eq!(req_str(input, "path").unwrap(), "a.txt");
/// assert_eq!(opt_num(input, "limit").unwrap(), Some(20.0));
/// assert!(opt_bool(input, "all"));
/// assert_eq!(opt_str(input, "glob").unwrap(), None); // "" counts as left out
/// assert!(req_str(input, "missing").is_err());
/// ```
pub fn req_str<'a>(input: &'a JsonMap, key: &str) -> Result<&'a str, ToolError> {
    match input.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s),
        _ => Err(ToolError::Input(format!(
            "\"{key}\" is required and must be a string"
        ))),
    }
}

/// An optional string argument; missing, `null` and `""` are all None.
///
/// # Errors
///
/// [`ToolError::Input`] when `key` holds something other than a string.
pub fn opt_str<'a>(input: &'a JsonMap, key: &str) -> Result<Option<&'a str>, ToolError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(ToolError::Input(format!("\"{key}\" must be a string"))),
    }
}

/// A number, also as a numeric string (models send both).
/// Missing, `null`, `""` and whitespace are None.
///
/// # Errors
///
/// [`ToolError::Input`] when `key` holds anything else, or a number that is
/// not finite.
pub fn opt_num(input: &JsonMap, key: &str) -> Result<Option<f64>, ToolError> {
    let n = match input.get(key) {
        None | Some(Value::Null) => return Ok(None),
        // Models send "" for a number they mean to leave out.
        Some(Value::String(s)) if s.trim().is_empty() => return Ok(None),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(_) => None,
    };
    n.filter(|f| f.is_finite())
        .map(Some)
        .ok_or_else(|| ToolError::Input(format!("\"{key}\" must be a number")))
}

/// `true` or `"true"`; anything else is false.
pub fn opt_bool(input: &JsonMap, key: &str) -> bool {
    match input.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

/// A value as a reader spells it: a string as itself, anything else as JSON (`16:9`, `80`, `true`).
pub fn spelled(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
