//! One setting's value against the model's spec: an enum's values, a range's
//! whole numbers, and the shape of `seed` and `size` when no spec says more.

use haest_core::tool::spelled;
use serde_json::Value;

use super::{ImageSettingsError, SettingSpec, values_of};

/// A whole number, as JSON carries one.
fn whole(v: &Value) -> Option<f64> {
    v.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0)
}

/// `value` refused for `name`, which takes what `expected` says.
fn bad<T>(
    model: &str,
    name: &str,
    value: &Value,
    expected: String,
) -> Result<T, ImageSettingsError> {
    Err(ImageSettingsError::BadValue {
        model: model.into(),
        name: name.into(),
        value: value.clone(),
        expected,
    })
}

/// A `{type: 'boolean'}` (or unknown) setting's value, as the API expects it.
pub(super) fn free_value(
    model: &str,
    name: &str,
    value: &Value,
) -> Result<Value, ImageSettingsError> {
    if name == "seed" && !whole(value).is_some_and(|n| n >= 0.0) {
        return bad(model, name, value, "a whole number 0 or above".into());
    }
    let is_size = |s: &str| {
        s.split_once('x').is_some_and(|(w, h)| {
            [w, h]
                .iter()
                .all(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        })
    };
    if name == "size" && !value.as_str().is_some_and(is_size) {
        let expected = "WIDTHxHEIGHT in pixels, e.g. \"1024x1536\"";
        return bad(model, name, value, expected.into());
    }
    Ok(value.clone())
}

/// Number.MAX_SAFE_INTEGER: a range with no bound on one side.
const SAFE: f64 = 9_007_199_254_740_991.0;

/// `value` for setting `name`, checked against the model's `spec`.
pub(super) fn check_one(
    model: &str,
    name: &str,
    spec: &SettingSpec,
    value: &Value,
) -> Result<Value, ImageSettingsError> {
    match spec.kind.as_deref() {
        Some("enum") => {
            let allowed = values_of(spec);
            let v = spelled(value);
            if !allowed.contains(&v) {
                return bad(
                    model,
                    name,
                    value,
                    format!("one of: {}", allowed.join(", ")),
                );
            }
            Ok(Value::String(v))
        }
        Some("range") => {
            let (min, max) = (spec.min.unwrap_or(-SAFE), spec.max.unwrap_or(SAFE));
            if !whole(value).is_some_and(|n| n >= min && n <= max) {
                let expected = format!("a whole number from {min} to {max}");
                return bad(model, name, value, expected);
            }
            Ok(value.clone())
        }
        _ => free_value(model, name, value),
    }
}
