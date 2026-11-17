//! Reading a tool call's arguments the way models actually send them: a
//! number as a string, `""` for "left out".

use serde_json::Value;

use super::{JsonMap, ToolError};

/// A tool's input from a JSON value, as a model sends it: for a test, an
/// example or a host that runs a tool itself.
///
/// # Errors
///
/// [`ToolError::Input`] when `value` is not a JSON object.
///
/// # Examples
///
/// ```
/// use haest_core::{req_str, tool};
/// use serde_json::json;
///
/// # fn main() -> Result<(), haest_core::BoxError> {
/// let input = tool::input(json!({"path": "a.txt"}))?;
/// assert_eq!(req_str(&input, "path")?, "a.txt");
/// assert!(tool::input(json!(["a.txt"])).is_err());
/// # Ok(())
/// # }
/// ```
pub fn input(value: Value) -> Result<JsonMap, ToolError> {
    let kind = match value {
        Value::Object(input) => return Ok(input),
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
    };
    Err(ToolError::Input(format!(
        "a tool's input must be a JSON object, not {kind}"
    )))
}

/// A required, non-empty string argument.
///
/// # Errors
///
/// [`ToolError::Input`] when `key` is missing, empty or not a string.
///
/// ```
/// use haest_core::{JsonMap, opt_bool, opt_num, opt_str, req_str, tool};
///
/// # fn main() -> Result<(), haest_core::BoxError> {
/// let input: JsonMap = tool::input(serde_json::json!({
///     "path": "a.txt", "limit": "20", "all": "true", "glob": "", "force": "yes"
/// }))?;
/// let input = &input;
/// assert_eq!(req_str(input, "path")?, "a.txt");
/// assert_eq!(opt_num(input, "limit")?, Some(20.0));
/// assert!(opt_bool(input, "all")?);
/// assert_eq!(opt_str(input, "glob")?, None); // "" counts as left out
/// assert!(req_str(input, "missing").is_err());
/// assert!(opt_bool(input, "force").is_err()); // "yes" is not a boolean
/// # Ok(())
/// # }
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

/// A flag: `true` and `false`, also as strings in any case (`"True"`).
/// Missing, `null` and `""` are false.
///
/// # Errors
///
/// [`ToolError::Input`] when `key` holds anything else (`"yes"`, `1`): the
/// model is told to send a boolean, instead of the flag being read as false.
pub fn opt_bool(input: &JsonMap, key: &str) -> Result<bool, ToolError> {
    let bad = || ToolError::Input(format!("\"{key}\" must be true or false"));
    match input.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::String(s)) => match s.trim() {
            "" => Ok(false),
            t if t.eq_ignore_ascii_case("true") => Ok(true),
            t if t.eq_ignore_ascii_case("false") => Ok(false),
            _ => Err(bad()),
        },
        Some(_) => Err(bad()),
    }
}

/// A value as a reader spells it: a string as itself, anything else as JSON (`16:9`, `80`, `true`).
pub fn spelled(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
